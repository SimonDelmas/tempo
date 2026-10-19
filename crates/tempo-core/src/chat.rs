//! Chat avec l'agent : boucle d'appels d'outils sur les notes et les tâches.
//!
//! Le protocole passe par le mode json (déjà éprouvé avec Vibe) plutôt que par le tool calling
//! natif : à chaque étape le modèle demande un outil ou donne sa réponse finale. Les outils de
//! consultation sont en lecture seule ; les outils de proposition n'écrivent rien, ils préparent
//! des modifications que l'utilisateur valide une à une (`ChatService::apply`).
//!
//! La réponse finale est diffusée au fil de l'eau : le texte du champ "answer" est extrait du
//! json partiel reçu du modèle.

use std::sync::{Arc, LazyLock};

use chrono::{Datelike, NaiveDate};
use regex::Regex;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::config::Settings;
use crate::error::{not_found, vault_err, Error, Result};
use crate::index::{extract_links, Index, NoteRow};
use crate::journal::JournalService;
use crate::llm::{chat_json, extract_json, Llm, Msg};
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

static ACTION_ANSWER_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""action"\s*:\s*"answer""#).unwrap());
static ANSWER_START_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""answer"\s*:\s*""#).unwrap());

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum StepAction {
    Tool,
    Answer,
}

#[derive(Debug, Clone, Deserialize)]
struct AgentStep {
    action: StepAction,
    #[serde(default)]
    tool: Option<String>,
    #[serde(default)]
    args: Map<String, Value>,
    #[serde(default)]
    answer: String,
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

/// Décode le début d'une chaîne json (sans le guillemet ouvrant), même incomplète.
pub fn decode_partial_string(raw: &str) -> String {
    let chars: Vec<char> = raw.chars().collect();
    let hex_at = |i: usize| -> Option<u32> {
        if i + 4 > chars.len() {
            return None;
        }
        u32::from_str_radix(&chars[i..i + 4].iter().collect::<String>(), 16).ok()
    };
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' {
            break;
        }
        if c != '\\' {
            out.push(c);
            i += 1;
            continue;
        }
        let Some(&escape) = chars.get(i + 1) else { break };
        if escape == 'u' {
            if i + 6 > chars.len() {
                break;
            }
            match hex_at(i + 2) {
                // paire de substitution UTF-16
                Some(high @ 0xD800..=0xDBFF) => {
                    if i + 12 > chars.len() {
                        break;
                    }
                    if chars[i + 6] == '\\' && chars[i + 7] == 'u'
                        && let Some(low @ 0xDC00..=0xDFFF) = hex_at(i + 8) {
                            if let Some(c) = char::from_u32(0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00)) {
                                out.push(c);
                            }
                            i += 12;
                            continue;
                        }
                    i += 6;
                }
                Some(code) => {
                    if let Some(c) = char::from_u32(code) {
                        out.push(c);
                    }
                    i += 6;
                }
                None => i += 6,
            }
            continue;
        }
        out.push(match escape {
            'n' => '\n',
            't' => '\t',
            'r' => '\r',
            'b' => '\u{8}',
            'f' => '\u{c}',
            other => other,
        });
        i += 2;
    }
    out
}

/// Texte de la réponse finale contenu dans un json encore incomplet, ou None si ce n'est pas
/// (encore) une réponse finale.
pub fn partial_answer(raw: &str) -> Option<String> {
    if !ACTION_ANSWER_RE.is_match(raw) {
        return None;
    }
    ANSWER_START_RE.find(raw).map(|m| decode_partial_string(&raw[m.end()..]))
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

const TOOLS: [(&str, &str); 6] = [
    ("search_notes", r#"{"query": "mots-clés"} — recherche plein texte dans toutes les notes"#),
    ("read_note", r#"{"name": "lien ou chemin"} — contenu d'une note (ou tâche), avec les notes qui la mentionnent"#),
    (
        "list_notes",
        r#"{"type": "reunion|echange|journal|sujet|projet|personne|ressource|formation|rapport" (optionnel), "start": "AAAA-MM-JJ" (optionnel), "end": "AAAA-MM-JJ" (optionnel)} — notes datées sur une période"#,
    ),
    (
        "list_tasks",
        r#"{"status": "a_faire|en_cours|attente|bloquee|terminee" (optionnel), "project": "identifiant" (optionnel)} — liste des tâches"#,
    ),
    ("get_task", r#"{"id": "T-001"} — fiche complète d'une tâche et son historique"#),
    (
        "task_events",
        r#"{"start": "AAAA-MM-JJ", "end": "AAAA-MM-JJ"} — événements d'historique des tâches sur une période (créations, statuts, notes)"#,
    ),
];
const SEMANTIC_TOOL: (&str, &str) =
    ("semantic_search", r#"{"query": "question ou idée"} — recherche par le sens (notes proches même sans les mêmes mots)"#);
const PROPOSAL_TOOLS: [(&str, &str); 3] = [
    (
        "propose_task",
        r#"{"title": "...", "description": "..." (optionnel), "project": "identifiant" (optionnel), "priority": "basse|normale|haute" (optionnel), "due": "AAAA-MM-JJ" (optionnel)} — nouvelle tâche"#,
    ),
    (
        "propose_task_update",
        r#"{"task_id": "T-001", "status": "a_faire|en_cours|attente|bloquee|terminee" (optionnel), "priority": "..." (optionnel), "due": "AAAA-MM-JJ" (optionnel), "note": "ce qui a changé" (optionnel)} — mise à jour d'une tâche existante, tracée dans son historique"#,
    ),
    ("propose_journal_line", r#"{"line": "texte"} — ligne à ajouter au journal du jour"#),
];

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

    /// Outils de consultation disponibles (avec la recherche sémantique si elle est active).
    fn tools(&self) -> Vec<(&'static str, &'static str)> {
        let mut tools = TOOLS.to_vec();
        if self.semantic.is_some() {
            tools.push(SEMANTIC_TOOL);
        }
        tools
    }

    pub fn tool_names(&self) -> Vec<&'static str> {
        self.tools().into_iter().chain(PROPOSAL_TOOLS).map(|(name, _)| name).collect()
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
        proposal.task_id = Some(task.id);
        proposal.title = task.title;
        proposal.status = check_choice("status", args.status, &STATUSES)?;
        proposal.priority = check_choice("priority", args.priority, &PRIORITIES)?;
        proposal.due = check_date(args.due.as_deref())?;
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
        let describe = |tools: Vec<(&str, &str)>| tools.iter().map(|(n, d)| format!("- {n} {d}")).collect::<Vec<_>>().join("\n");
        CHAT_SYSTEM
            .replace("{today}", &today_date.to_string())
            .replace("{weekday}", WEEKDAYS[today_date.weekday().num_days_from_monday() as usize])
            .replace("{tools}", &describe(self.tools()))
            .replace("{proposals}", &describe(PROPOSAL_TOOLS.to_vec()))
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

    /// Étape suivante du modèle ; le texte de la réponse finale est transmis au fil de l'eau.
    fn next_step(&self, messages: &[Msg], on_delta: &mut dyn FnMut(String)) -> Result<AgentStep> {
        let mut raw = String::new();
        let mut sent = 0;
        self.llm.chat_stream(messages, &self.settings.model_smart, true, &mut |chunk| {
            raw.push_str(chunk);
            if let Some(text) = partial_answer(&raw) {
                let count = text.chars().count();
                if count > sent {
                    on_delta(text.chars().skip(sent).collect());
                    sent = count;
                }
            }
        })?;
        match serde_json::from_str::<AgentStep>(&extract_json(&raw)) {
            Ok(step) => Ok(step),
            // json invalide : nouvel essai non diffusé (avec demande de correction)
            Err(_) => chat_json(self.llm.as_ref(), messages, &self.settings.model_smart),
        }
    }

    /// Événements de la réponse : step (outil appelé), delta (texte de la réponse), done.
    pub fn stream(&self, history: &[ChatMessage], today_date: Option<NaiveDate>, emit: &mut dyn FnMut(Value)) -> Result<()> {
        let mut messages = vec![Msg::system(self.system_prompt(today_date.unwrap_or_else(today)))];
        let skip = history.len().saturating_sub(MAX_HISTORY);
        messages.extend(history[skip..].iter().map(|m| {
            Msg::new(if m.role == Role::User { "user" } else { "assistant" }, m.content.clone())
        }));
        let mut steps: Vec<Value> = Vec::new();
        let mut proposals: Vec<ChatProposal> = Vec::new();
        let mut last: Option<AgentStep> = None;
        for attempt in 0..=MAX_STEPS {
            if attempt == MAX_STEPS {
                messages.push(Msg::user(
                    "Nombre maximal d'outils atteint : donne maintenant ta réponse finale avec ce que tu as trouvé.",
                ));
            }
            let step = self.next_step(&messages, &mut |text| emit(json!({"type": "delta", "text": text})))?;
            if step.action == StepAction::Answer || attempt == MAX_STEPS {
                last = Some(step);
                break;
            }
            let result = self.run_tool(step.tool.as_deref(), &step.args, &mut proposals)?;
            let summary = (result.starts_with("Erreur") || result.starts_with("Aucun")).then(|| result.clone());
            let record = json!({"tool": step.tool, "args": step.args, "summary": summary});
            let mut event = json!({"type": "step"});
            event.as_object_mut().unwrap().extend(record.as_object().unwrap().clone());
            emit(event);
            steps.push(record);
            messages.push(Msg::assistant(json!({"action": "tool", "tool": step.tool, "args": step.args}).to_string()));
            messages.push(Msg::user(format!("Résultat de {} :\n{result}", step.tool.as_deref().unwrap_or(""))));
            last = Some(step);
        }
        let answer = last.map(|s| s.answer.trim().to_string()).filter(|a| !a.is_empty());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial() {
        assert_eq!(partial_answer(r#"{"action": "tool", "tool": "x""#), None);
        assert_eq!(partial_answer(r#"{"action": "answer", "answer": "Bon"#).as_deref(), Some("Bon"));
        assert_eq!(
            partial_answer(r#"{"action": "answer", "answer": "a\nb \"c\" é"#).as_deref(),
            Some("a\nb \"c\" é")
        );
        assert_eq!(partial_answer(r#"{"action": "answer", "answer": "fin", "x": 1}"#).as_deref(), Some("fin"));
        assert_eq!(partial_answer(r#"{"action": "answer", "answer": "coupé \"#).as_deref(), Some("coupé "));
        assert_eq!(partial_answer(r#"{"action": "answer", "answer": "😀 \ud83d"#).as_deref(), Some("😀 "));
    }
}
