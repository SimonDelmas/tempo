//! Chat avec l'agent : boucle d'appels d'outils sur les notes et les tâches.
//!
//! Le protocole est le tool calling natif de l'API OpenAI : à chaque tour le modèle appelle un
//! ou plusieurs outils, ou répond en texte. Les outils de consultation sont en lecture seule ;
//! les outils de proposition n'écrivent rien, ils préparent des modifications que l'utilisateur
//! valide une à une (`ChatService::apply`).
//!
//! Le texte du modèle est diffusé au fil de l'eau ; s'il précède des appels d'outils, un
//! événement `reset` l'efface de la réponse en cours.

use std::sync::Arc;

use chrono::{Datelike, NaiveDate};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::config::Settings;
use crate::error::{not_found, vault_err, Error, Result};
use crate::index::{extract_links, Index, NoteRow};
use crate::journal::JournalService;
use crate::llm::{Llm, Msg, ToolDef};
use crate::prompts::CHAT_SYSTEM;
use crate::semantic::SemanticIndex;
use crate::tasks::{NewTask, TaskService, PRIORITIES, STATUSES};
use crate::util::{clip, iso_date, today};
use crate::vault::{split_frontmatter, Vault};

pub const MAX_STEPS: usize = 8;
const MAX_NOTE_CHARS: usize = 8000;
const MAX_RESULT_CHARS: usize = 12000;
const MAX_HISTORY: usize = 20;
const WEEKDAYS: [&str; 7] = ["lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi", "dimanche"];
/// types proposés à `list_notes` (les tâches ont leur propre outil)
const LISTED_KINDS: [&str; 9] = ["reunion", "echange", "journal", "sujet", "projet", "personne", "ressource", "formation", "rapport"];

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

impl ChatMessage {
    pub fn user(content: &str) -> Self {
        ChatMessage { role: Role::User, content: content.into() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalKind {
    CreateTask,
    UpdateTask,
    Journal,
}

/// Modification proposée par l'agent, appliquée seulement après validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatProposal {
    pub kind: ProposalKind,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub due: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub line: String,
}

impl ChatProposal {
    pub fn new(kind: ProposalKind) -> Self {
        ChatProposal {
            kind,
            title: String::new(),
            description: String::new(),
            project: None,
            priority: None,
            status: None,
            due: None,
            task_id: None,
            note: String::new(),
            line: String::new(),
        }
    }
}

fn check_date(value: Option<&str>) -> Result<Option<String>> {
    match value.filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(v) => iso_date(v)
            .map(|d| Some(d.to_string()))
            .ok_or_else(|| vault_err(format!("Date invalide : {v} (format AAAA-MM-JJ)"))),
    }
}

fn check_choice(field: &str, value: Option<String>, allowed: &[&str]) -> Result<Option<String>> {
    match value.filter(|v| !v.is_empty()) {
        Some(v) if !allowed.contains(&v.as_str()) => {
            Err(vault_err(format!("{field} invalide : {v} (valeurs : {})", allowed.join(", "))))
        }
        other => Ok(other),
    }
}

/// Arguments d'un appel d'outil, reçus en json (une chaîne vide vaut un objet vide).
fn parse_arguments(raw: &str) -> std::result::Result<Map<String, Value>, String> {
    if raw.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str::<Value>(raw) {
        Ok(Value::Object(args)) => Ok(args),
        Ok(_) => Err("un objet json est attendu".into()),
        Err(exc) => Err(format!("json illisible : {exc}")),
    }
}

// --- arguments des outils (un argument inconnu ou manquant est une erreur) ---------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QueryArgs {
    query: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NameArgs {
    name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListNotesArgs {
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    start: Option<String>,
    #[serde(default)]
    end: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListTasksArgs {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    project: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdArgs {
    id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PeriodArgs {
    start: String,
    end: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProposeTaskArgs {
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    due: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProposeUpdateArgs {
    task_id: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    due: Option<String>,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LineArgs {
    line: String,
}

/// Résultat d'un outil : des données pour le modèle, ou une proposition à valider.
enum ToolOutput {
    Data(Value),
    Proposal(ChatProposal),
}

// --- définition des outils (schémas json des arguments) ---------------------------

fn text(description: &str) -> Value {
    json!({"type": "string", "description": description})
}

fn date(description: &str) -> Value {
    json!({"type": "string", "format": "date", "description": format!("{description} (AAAA-MM-JJ)")})
}

fn choice(values: &[&str]) -> Value {
    json!({"type": "string", "enum": values})
}

/// Schéma d'un objet d'arguments : `required` liste les arguments obligatoires.
fn object(required: &[&str], properties: Value) -> Value {
    json!({"type": "object", "properties": properties, "required": required, "additionalProperties": false})
}

fn tool(name: &'static str, description: &str, parameters: Value) -> ToolDef {
    ToolDef { name, description: description.into(), parameters }
}

/// Outils de consultation, en lecture seule.
fn read_tools() -> Vec<ToolDef> {
    vec![
        tool("search_notes", "Recherche plein texte dans toutes les notes (porte sur des mots).",
             object(&["query"], json!({"query": text("mots-clés")}))),
        tool("read_note", "Contenu d'une note (ou d'une tâche), avec les notes qui la mentionnent.",
             object(&["name"], json!({"name": text("lien [[…]], nom ou chemin de la note")}))),
        tool("list_notes", "Notes datées sur une période, éventuellement d'un type donné.",
             object(&[], json!({"type": choice(&LISTED_KINDS), "start": date("début"), "end": date("fin")}))),
        tool("list_tasks", "Liste des tâches, filtrée par statut ou projet.",
             object(&[], json!({"status": choice(&STATUSES), "project": text("identifiant du projet")}))),
        tool("get_task", "Fiche complète d'une tâche et son historique.",
             object(&["id"], json!({"id": text("identifiant, ex. T-001")}))),
        tool("task_events", "Événements d'historique des tâches sur une période (créations, statuts, notes).",
             object(&["start", "end"], json!({"start": date("début"), "end": date("fin")}))),
    ]
}

fn semantic_tool() -> ToolDef {
    tool("semantic_search", "Recherche par le sens : notes proches d'une question ou d'une idée, même sans les mêmes mots.",
         object(&["query"], json!({"query": text("question ou idée")})))
}

/// Outils de proposition : rien n'est écrit, l'utilisateur valide chaque proposition.
fn proposal_tools() -> Vec<ToolDef> {
    const PROPOSAL: &str = "Proposition soumise à la validation de l'utilisateur (rien n'est modifié) : ";
    vec![
        tool("propose_task", &format!("{PROPOSAL}nouvelle tâche."),
             object(&["title"], json!({
                 "title": text("verbe à l'infinitif + objet, court"), "description": text("contexte utile"),
                 "project": text("identifiant du projet"), "priority": choice(&PRIORITIES), "due": date("échéance"),
             }))),
        tool("propose_task_update", &format!("{PROPOSAL}mise à jour d'une tâche existante, tracée dans son historique."),
             object(&["task_id"], json!({
                 "task_id": text("identifiant, ex. T-001"), "status": choice(&STATUSES), "priority": choice(&PRIORITIES),
                 "due": date("échéance"), "note": text("ce qui a changé"),
             }))),
        tool("propose_journal_line", &format!("{PROPOSAL}ligne à ajouter au journal du jour."),
             object(&["line"], json!({"line": text("texte de la ligne")}))),
    ]
}

pub struct ChatService {
    vault: Arc<Vault>,
    index: Arc<Index>,
    tasks: Arc<TaskService>,
    journal: Arc<JournalService>,
    llm: Arc<dyn Llm>,
    settings: Arc<Settings>,
    semantic: Option<Arc<SemanticIndex>>,
}

impl ChatService {
    pub fn new(
        vault: Arc<Vault>,
        index: Arc<Index>,
        tasks: Arc<TaskService>,
        journal: Arc<JournalService>,
        llm: Arc<dyn Llm>,
        settings: Arc<Settings>,
        semantic: Option<Arc<SemanticIndex>>,
    ) -> Self {
        ChatService { vault, index, tasks, journal, llm, settings, semantic: semantic.filter(|s| s.enabled()) }
    }

    /// Outils proposés au modèle (avec la recherche sémantique si elle est active).
    pub fn tools(&self) -> Vec<ToolDef> {
        let mut tools = read_tools();
        if self.semantic.is_some() {
            tools.push(semantic_tool());
        }
        tools.extend(proposal_tools());
        tools
    }

    pub fn tool_names(&self) -> Vec<&'static str> {
        self.tools().into_iter().map(|t| t.name).collect()
    }

    // --- outils -------------------------------------------------------------

    fn note_ref(note: &NoteRow) -> Map<String, Value> {
        let Value::Object(map) = json!({
            "lien": format!("[[{}]]", note.link_name()), "titre": note.title, "type": note.kind,
            "date": if note.date.is_empty() { Value::Null } else { Value::from(note.date.clone()) },
            "chemin": note.path,
        }) else {
            unreachable!()
        };
        map
    }

    pub fn search_notes(&self, query: &str) -> Result<Value> {
        let mut results = Vec::new();
        for hit in self.index.search(query, 12)? {
            if let Some(note) = self.index.get(&hit.path)? {
                let mut item = Self::note_ref(&note);
                item.insert("extrait".into(), hit.snippet.into());
                results.push(Value::Object(item));
            }
        }
        Ok(Value::Array(results))
    }

    pub fn semantic_search(&self, query: &str) -> Result<Value> {
        let Some(semantic) = &self.semantic else { return Ok(json!([])) };
        let hits = semantic.search(query, 8)?;
        Ok(Value::Array(
            hits.into_iter()
                .map(|n| {
                    let link = if n.alias.is_empty() { &n.stem } else { &n.alias };
                    json!({"lien": format!("[[{link}]]"), "titre": n.title, "type": n.kind,
                           "date": if n.date.is_empty() { Value::Null } else { Value::from(n.date.clone()) },
                           "chemin": n.path, "extrait": n.snippet, "proximite": n.score})
                })
                .collect(),
        ))
    }

    fn resolve(&self, name: &str) -> Result<String> {
        let name = name.trim();
        let name = name.strip_prefix("[[").unwrap_or(name);
        let name = name.strip_suffix("]]").unwrap_or(name);
        let name = name.split('|').next().unwrap_or(name);
        if self.index.get(name)?.is_some() {
            return Ok(name.to_string());
        }
        if let Some(path) = self.index.resolve_link(name)? {
            return Ok(path);
        }
        if let Some(path) = self.index.resolve_link(name.strip_suffix(".md").unwrap_or(name))? {
            return Ok(path);
        }
        Err(not_found(format!("Aucune note ne correspond à « {name} »")))
    }

    pub fn read_note(&self, name: &str) -> Result<Value> {
        let path = self.resolve(name)?;
        let note = self.index.get(&path)?.ok_or_else(|| not_found(format!("Note introuvable : {path}")))?;
        let text = self.vault.read_text(&path)?;
        let (_, body) = split_frontmatter(&text);
        let mut result = Self::note_ref(&note);
        result.insert("meta".into(), Value::Object(note.meta.clone()));
        result.insert("contenu".into(), clip(body.trim(), MAX_NOTE_CHARS).into());
        let backlinks: Vec<Value> = self.index.backlinks(&path)?.iter().take(15).map(|n| Value::Object(Self::note_ref(n))).collect();
        result.insert("mentionnee_dans".into(), Value::Array(backlinks));
        Ok(Value::Object(result))
    }

    pub fn list_notes(&self, kind: Option<&str>, start: Option<&str>, end: Option<&str>) -> Result<Value> {
        let notes: Vec<NoteRow> = self
            .index
            .list_notes(kind, start, end)?
            .into_iter()
            .filter(|n| !["modele", "inbox", "tache"].contains(&n.kind.as_str()))
            .collect();
        let skip = notes.len().saturating_sub(60);
        Ok(Value::Array(notes[skip..].iter().map(|n| Value::Object(Self::note_ref(n))).collect()))
    }

    pub fn list_tasks(&self, status: Option<&str>, project: Option<&str>) -> Result<Value> {
        Ok(Value::Array(
            self.tasks
                .list(status, project)?
                .into_iter()
                .map(|t| {
                    json!({"id": t.id, "title": t.title, "status": t.status, "priority": t.priority,
                           "project": t.project, "due": t.due, "lien": format!("[[{}]]", t.id)})
                })
                .collect(),
        ))
    }

    pub fn get_task(&self, id: &str) -> Result<Value> {
        let detail = self.tasks.get(id)?;
        let (_, body) = split_frontmatter(&detail.content);
        let mut task = serde_json::to_value(&detail.task).unwrap();
        let map = task.as_object_mut().unwrap();
        map.shift_remove("path");
        map.insert("history".into(), serde_json::to_value(&detail.history).unwrap());
        map.insert("lien".into(), format!("[[{}]]", detail.task.id).into());
        map.insert("contenu".into(), clip(body.trim(), MAX_NOTE_CHARS).into());
        Ok(task)
    }

    pub fn task_events(&self, start: &str, end: &str) -> Result<Value> {
        Ok(Value::Array(
            self.tasks
                .events(start, end)?
                .into_iter()
                .map(|e| {
                    let mut item = serde_json::to_value(&e).unwrap();
                    item["lien"] = format!("[[{}]]", e.task_id).into();
                    item
                })
                .collect(),
        ))
    }

    // --- propositions (validées par l'utilisateur, voir apply) ---------------

    fn propose_task(&self, args: ProposeTaskArgs) -> Result<ChatProposal> {
        if args.title.trim().is_empty() {
            return Err(vault_err("Le titre de la tâche est obligatoire"));
        }
        let mut proposal = ChatProposal::new(ProposalKind::CreateTask);
        proposal.title = args.title.trim().to_string();
        proposal.description = args.description.unwrap_or_default();
        proposal.project = args.project.filter(|p| !p.is_empty());
        proposal.priority = check_choice("priority", args.priority, &PRIORITIES)?;
        proposal.due = check_date(args.due.as_deref())?;
        Ok(proposal)
    }

    fn propose_task_update(&self, args: ProposeUpdateArgs) -> Result<ChatProposal> {
        let task = self.tasks.get(&args.task_id)?.task;
        let mut proposal = ChatProposal::new(ProposalKind::UpdateTask);
        // le modèle renvoie souvent les valeurs inchangées : seules les vraies modifications restent
        proposal.status = check_choice("status", args.status, &STATUSES)?.filter(|s| *s != task.status);
        proposal.priority = check_choice("priority", args.priority, &PRIORITIES)?.filter(|p| *p != task.priority);
        proposal.due = check_date(args.due.as_deref())?.filter(|d| Some(d) != task.due.as_ref());
        proposal.task_id = Some(task.id);
        proposal.title = task.title;
        proposal.note = args.note.unwrap_or_default().trim().to_string();
        if proposal.status.is_none() && proposal.priority.is_none() && proposal.due.is_none() && proposal.note.is_empty() {
            return Err(vault_err("Aucune modification proposée"));
        }
        Ok(proposal)
    }

    fn propose_journal_line(&self, args: LineArgs) -> Result<ChatProposal> {
        if args.line.trim().is_empty() {
            return Err(vault_err("La ligne de journal est vide"));
        }
        let mut proposal = ChatProposal::new(ProposalKind::Journal);
        proposal.line = args.line.trim().to_string();
        Ok(proposal)
    }

    /// Applique une proposition validée par l'utilisateur.
    pub fn apply(&self, proposal: &ChatProposal, today_date: Option<NaiveDate>) -> Result<Value> {
        let today_str = today_date.unwrap_or_else(today).to_string();
        match proposal.kind {
            ProposalKind::CreateTask => {
                let new = NewTask {
                    title: proposal.title.clone(),
                    project: proposal.project.clone(),
                    priority: proposal.priority.clone().filter(|p| !p.is_empty()),
                    status: proposal.status.clone().filter(|s| !s.is_empty()),
                    due: proposal.due.clone(),
                    description: proposal.description.clone(),
                    origin: None,
                };
                let task = self.tasks.create(new, Some(&today_str))?.task;
                Ok(json!({"path": task.path, "message": format!("Tâche {} créée", task.id)}))
            }
            ProposalKind::UpdateTask => {
                let task_id = proposal.task_id.as_deref().filter(|t| !t.is_empty()).ok_or_else(|| vault_err("Tâche non précisée"))?;
                let mut changes = Map::new();
                for (key, value) in [("status", &proposal.status), ("priority", &proposal.priority), ("due", &proposal.due)] {
                    if let Some(value) = value.as_ref().filter(|v| !v.is_empty()) {
                        changes.insert(key.into(), value.clone().into());
                    }
                }
                let task = self.tasks.update(task_id, &changes, Some(&proposal.note), Some(&today_str))?.task;
                Ok(json!({"path": task.path, "message": format!("Tâche {} mise à jour", task.id)}))
            }
            ProposalKind::Journal => {
                let line = proposal.line.trim();
                if line.is_empty() {
                    return Err(vault_err("La ligne de journal est vide"));
                }
                let path = self.journal.append(&today_str, "## Notes", &format!("- {line}"))?;
                Ok(json!({"path": path, "message": "Ligne ajoutée au journal"}))
            }
        }
    }

    fn call_tool(&self, name: &str, args: &Map<String, Value>) -> Result<std::result::Result<ToolOutput, String>> {
        fn parse<T: DeserializeOwned>(name: &str, args: &Map<String, Value>) -> std::result::Result<T, String> {
            serde_json::from_value(Value::Object(args.clone()))
                .map_err(|exc| format!("Erreur : arguments invalides pour {name} ({exc})."))
        }
        macro_rules! args {
            ($t:ty) => {
                match parse::<$t>(name, args) {
                    Ok(a) => a,
                    Err(message) => return Ok(Err(message)),
                }
            };
        }
        let output = match name {
            "search_notes" => ToolOutput::Data(self.search_notes(&args!(QueryArgs).query)?),
            "read_note" => ToolOutput::Data(self.read_note(&args!(NameArgs).name)?),
            "list_notes" => {
                let a = args!(ListNotesArgs);
                ToolOutput::Data(self.list_notes(a.kind.as_deref(), a.start.as_deref(), a.end.as_deref())?)
            }
            "list_tasks" => {
                let a = args!(ListTasksArgs);
                ToolOutput::Data(self.list_tasks(a.status.as_deref(), a.project.as_deref())?)
            }
            "get_task" => ToolOutput::Data(self.get_task(&args!(IdArgs).id)?),
            "task_events" => {
                let a = args!(PeriodArgs);
                ToolOutput::Data(self.task_events(&a.start, &a.end)?)
            }
            "semantic_search" if self.semantic.is_some() => ToolOutput::Data(self.semantic_search(&args!(QueryArgs).query)?),
            "propose_task" => ToolOutput::Proposal(self.propose_task(args!(ProposeTaskArgs))?),
            "propose_task_update" => ToolOutput::Proposal(self.propose_task_update(args!(ProposeUpdateArgs))?),
            "propose_journal_line" => ToolOutput::Proposal(self.propose_journal_line(args!(LineArgs))?),
            _ => {
                return Ok(Err(format!("Erreur : outil inconnu « {name} ». Outils : {}.", self.tool_names().join(", "))));
            }
        };
        Ok(Ok(output))
    }

    /// Exécute un outil ; les erreurs de l'utilisateur ou du modèle sont renvoyées au modèle
    /// sous forme de texte, les propositions sont mises de côté.
    pub fn run_tool(&self, name: Option<&str>, args: &Map<String, Value>, proposals: &mut Vec<ChatProposal>) -> Result<String> {
        let name = name.unwrap_or("");
        let output = match self.call_tool(name, args) {
            Ok(Ok(output)) => output,
            Ok(Err(message)) => return Ok(message),
            Err(err @ (Error::Vault(_) | Error::NotFound(_) | Error::Conflict { .. })) => return Ok(format!("Erreur : {err}")),
            Err(err) => return Err(err),
        };
        match output {
            ToolOutput::Proposal(proposal) => {
                proposals.push(proposal);
                Ok(format!(
                    "Proposition n°{} enregistrée : elle sera soumise à l'utilisateur, qui la validera ou non.",
                    proposals.len()
                ))
            }
            ToolOutput::Data(value) => {
                let empty = match &value {
                    Value::Array(a) => a.is_empty(),
                    Value::Object(o) => o.is_empty(),
                    Value::Null => true,
                    _ => false,
                };
                if empty {
                    Ok("Aucun résultat.".into())
                } else {
                    Ok(clip(&value.to_string(), MAX_RESULT_CHARS))
                }
            }
        }
    }

    // --- conversation -------------------------------------------------------

    pub fn system_prompt(&self, today_date: NaiveDate) -> String {
        CHAT_SYSTEM
            .replace("{today}", &today_date.to_string())
            .replace("{weekday}", WEEKDAYS[today_date.weekday().num_days_from_monday() as usize])
    }

    /// Liens [[…]] de la réponse résolus en chemins (les liens inconnus sont omis).
    pub fn links_of(&self, text: &str) -> Result<Map<String, Value>> {
        let mut links = Map::new();
        for name in extract_links(text) {
            if let Some(path) = self.index.resolve_link(&name)? {
                links.insert(name, path.into());
            }
        }
        Ok(links)
    }

    /// Événements de la réponse : step (outil appelé), delta (texte de la réponse), reset (le
    /// texte diffusé précédait des appels d'outils : il est effacé), done.
    pub fn stream(&self, history: &[ChatMessage], today_date: Option<NaiveDate>, emit: &mut dyn FnMut(Value)) -> Result<()> {
        let mut messages = vec![Msg::system(self.system_prompt(today_date.unwrap_or_else(today)))];
        let skip = history.len().saturating_sub(MAX_HISTORY);
        messages.extend(history[skip..].iter().map(|m| {
            Msg::new(if m.role == Role::User { "user" } else { "assistant" }, m.content.clone())
        }));
        let tools = self.tools();
        let mut steps: Vec<Value> = Vec::new();
        let mut proposals: Vec<ChatProposal> = Vec::new();
        let mut answer = String::new();
        for turn in 0..=MAX_STEPS {
            // dernier tour : les outils restent décrits mais le modèle doit répondre
            let last = turn == MAX_STEPS;
            if last {
                messages.push(Msg::user(
                    "Nombre maximal d'outils atteint : donne maintenant ta réponse finale avec ce que tu as trouvé.",
                ));
            }
            let reply = self.llm.chat_tools(&messages, &self.settings.model_smart, &tools, !last, &mut |text| {
                emit(json!({"type": "delta", "text": text}))
            })?;
            if reply.tool_calls.is_empty() || last {
                answer = reply.text;
                break;
            }
            if !reply.text.is_empty() {
                emit(json!({"type": "reset"}));
            }
            messages.push(Msg::assistant_tools(reply.text, reply.tool_calls.clone()));
            for call in reply.tool_calls {
                let name = call.function.name;
                let (args, result) = match parse_arguments(&call.function.arguments) {
                    Ok(args) => {
                        let result = self.run_tool(Some(&name), &args, &mut proposals)?;
                        (Value::Object(args), result)
                    }
                    Err(message) => (Value::String(call.function.arguments), format!("Erreur : arguments invalides pour {name} ({message}).")),
                };
                let summary = (result.starts_with("Erreur") || result.starts_with("Aucun")).then(|| result.clone());
                let record = json!({"tool": name, "args": args, "summary": summary});
                let mut event = json!({"type": "step"});
                event.as_object_mut().unwrap().extend(record.as_object().unwrap().clone());
                emit(event);
                steps.push(record);
                messages.push(Msg::tool(&call.id, result));
            }
        }
        let answer = Some(answer.trim().to_string()).filter(|a| !a.is_empty());
        let answer = answer.unwrap_or_else(|| "Je n'ai pas de réponse à proposer.".into());
        emit(json!({"type": "done", "links": self.links_of(&answer)?, "answer": answer, "steps": steps,
                    "proposals": proposals}));
        Ok(())
    }

    /// Réponse complète, sans diffusion.
    pub fn ask(&self, history: &[ChatMessage], today_date: Option<NaiveDate>) -> Result<Value> {
        let mut done = Value::Null;
        self.stream(history, today_date, &mut |event| {
            if event["type"] == "done" {
                done = event;
            }
        })?;
        done.as_object_mut().map(|o| o.shift_remove("type"));
        Ok(done)
    }
}
