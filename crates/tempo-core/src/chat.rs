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
use crate::index::{extract_links, infer_type, Index, NoteRow};
use crate::journal::{JournalService, NOTE_KINDS};
use crate::llm::{Llm, Msg, ToolDef};
use crate::prompts::CHAT_SYSTEM;
use crate::semantic::SemanticIndex;
use crate::notes::{add_link, append_text, apply_edits, heading, remove_link, split_raw, update_meta, with_meta, TextEdit};
use crate::tasks::{week_start, NewTask, TaskService, EDITABLE_FIELDS, PRIORITIES, STATUSES};
use crate::util::{clip, iso_date, today};
use crate::vault::{content_hash, split_frontmatter, Vault};

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

/// Modification proposée par l'agent, appliquée seulement après validation : une variante
/// par opération (`kind` en json). Les opérations sur une note sont vérifiées à la
/// proposition, puis rejouées à la validation sur le contenu du moment. Les champs `title`
/// des notes ne servent qu'à l'affichage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChatProposal {
    CreateTask {
        title: String,
        #[serde(default)]
        description: String,
        project: Option<String>,
        priority: Option<String>,
        status: Option<String>,
        due: Option<String>,
    },
    /// `title` : titre actuel (affichage) ; `new_title` : renommage
    UpdateTask {
        task_id: String,
        #[serde(default)]
        title: String,
        status: Option<String>,
        priority: Option<String>,
        due: Option<String>,
        #[serde(default)]
        note: String,
        #[serde(default)]
        new_title: Option<String>,
        #[serde(default)]
        project: Option<String>,
        /// semaine où planifier la tâche (`2026-S43`)
        #[serde(default)]
        iteration: Option<String>,
    },
    /// ligne ajoutée au journal du jour
    Journal { line: String },
    CreateNote {
        #[serde(rename = "type")]
        note_type: String,
        title: String,
        date: Option<String>,
        #[serde(default)]
        content: String,
    },
    EditNote {
        path: String,
        #[serde(default)]
        title: String,
        edits: Vec<TextEdit>,
    },
    AppendNote {
        path: String,
        #[serde(default)]
        title: String,
        section: Option<String>,
        text: String,
    },
    /// frontmatter : clés modifiées ou ajoutées, clés retirées ; `before` garde les valeurs
    /// d'origine pour l'affichage
    UpdateMeta {
        path: String,
        #[serde(default)]
        title: String,
        #[serde(default)]
        set: Map<String, Value>,
        #[serde(default)]
        remove: Vec<String>,
        #[serde(default)]
        before: Map<String, Value>,
    },
    /// suppression (la note part dans `.trash/`) ; `backlinks` : notes qui la mentionnent
    DeleteNote {
        path: String,
        #[serde(default)]
        title: String,
        #[serde(default)]
        backlinks: Vec<String>,
    },
    /// lien `[[cible]]` ajouté à un champ du frontmatter ou à une section du corps
    Link {
        path: String,
        #[serde(default)]
        title: String,
        link: String,
        target_path: String,
        field: Option<String>,
        section: Option<String>,
    },
    /// liens vers la cible retirés (frontmatter et corps)
    Unlink {
        path: String,
        #[serde(default)]
        title: String,
        link: String,
        target_path: String,
    },
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

/// Chemin de la note visée par une proposition (vide pour les tâches, le journal, une création).
fn proposal_path(proposal: &ChatProposal) -> &str {
    match proposal {
        ChatProposal::EditNote { path, .. }
        | ChatProposal::AppendNote { path, .. }
        | ChatProposal::UpdateMeta { path, .. }
        | ChatProposal::DeleteNote { path, .. }
        | ChatProposal::Link { path, .. }
        | ChatProposal::Unlink { path, .. } => path,
        _ => "",
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
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    iteration: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LineArgs {
    line: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoteCreateArgs {
    #[serde(rename = "type")]
    note_type: String,
    title: String,
    #[serde(default)]
    date: Option<String>,
    #[serde(default)]
    content: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoteEditArgs {
    name: String,
    edits: Vec<TextEdit>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoteAppendArgs {
    name: String,
    text: String,
    #[serde(default)]
    section: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoteMetaArgs {
    name: String,
    #[serde(default)]
    set: Map<String, Value>,
    #[serde(default)]
    remove: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LinkArgs {
    name: String,
    target: String,
    #[serde(default)]
    field: Option<String>,
    #[serde(default)]
    section: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UnlinkArgs {
    name: String,
    target: String,
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
                 "due": date("échéance"), "note": text("ce qui a changé"), "title": text("nouveau titre"),
                 "project": text("identifiant du projet de rattachement"),
                 "iteration": text("semaine où planifier la tâche, format AAAA-Snn (ex. 2026-S43)"),
             }))),
        tool("propose_journal_line", &format!("{PROPOSAL}ligne à ajouter au journal du jour."),
             object(&["line"], json!({"line": text("texte de la ligne")}))),
        tool("propose_note_create", &format!("{PROPOSAL}nouvelle note, rangée selon son type."),
             object(&["type", "title"], json!({
                 "type": choice(&NOTE_KINDS), "title": text("titre (devient le nom du fichier)"),
                 "date": date("date de la note, aujourd'hui par défaut"),
                 "content": text("corps en Markdown, sous le titre (sections ##, puces, [[liens]])"),
             }))),
        tool("propose_note_edit",
             &format!("{PROPOSAL}remplacement de passages du corps d'une note (ou tâche). Chaque passage cité \
                       doit figurer exactement une fois dans la note lue avec read_note ; cite juste assez de \
                       texte pour qu'il soit unique. Pour le frontmatter, utilise propose_note_meta."),
             object(&["name", "edits"], json!({
                 "name": text("lien [[…]], nom ou chemin de la note"),
                 "edits": {"type": "array", "minItems": 1, "items": object(&["find", "replace"], json!({
                     "find": text("passage exact à remplacer"), "replace": text("nouveau texte (vide pour supprimer)"),
                 }))},
             }))),
        tool("propose_note_append",
             &format!("{PROPOSAL}texte ajouté à la fin d'une section (créée si absente) ou de la note."),
             object(&["name", "text"], json!({
                 "name": text("lien [[…]], nom ou chemin de la note"), "text": text("texte Markdown à ajouter"),
                 "section": text("titre de section, ex. « ## Décisions » (fin de la note si absent)"),
             }))),
        tool("propose_note_meta",
             &format!("{PROPOSAL}modification du frontmatter clé par clé. Pour le statut, la priorité, \
                       l'échéance, le projet ou l'itération d'une tâche, utilise propose_task_update."),
             object(&["name"], json!({
                 "name": text("lien [[…]], nom ou chemin de la note"),
                 "set": {"type": "object", "description": "clés à créer ou modifier, avec leur valeur (texte, nombre, liste)"},
                 "remove": {"type": "array", "items": {"type": "string"}, "description": "clés à retirer"},
             }))),
        tool("propose_note_delete", &format!("{PROPOSAL}suppression d'une note (déplacée dans la corbeille du vault)."),
             object(&["name"], json!({"name": text("lien [[…]], nom ou chemin de la note")}))),
        tool("propose_link",
             &format!("{PROPOSAL}lien [[cible]] ajouté à une note : dans une liste du frontmatter (champ, ex. \
                       tasks, participants) ou à la fin d'une section du corps (« ## Liens » par défaut)."),
             object(&["name", "target"], json!({
                 "name": text("note à modifier (lien, nom ou chemin)"), "target": text("note vers laquelle pointer"),
                 "field": text("liste du frontmatter où ajouter le lien"), "section": text("section du corps"),
             }))),
        tool("propose_unlink",
             &format!("{PROPOSAL}retrait des liens d'une note vers une cible (entrées du frontmatter \
                       supprimées, liens du corps remplacés par leur texte)."),
             object(&["name", "target"], json!({
                 "name": text("note à modifier (lien, nom ou chemin)"), "target": text("note dont retirer les liens"),
             }))),
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
        let refs = |notes: Vec<NoteRow>| Value::Array(notes.iter().take(15).map(|n| Value::Object(Self::note_ref(n))).collect());
        result.insert("liens".into(), refs(self.index.outlinks(&path)?));
        result.insert("mentionnee_dans".into(), refs(self.index.backlinks(&path)?));
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
        Ok(ChatProposal::CreateTask {
            title: args.title.trim().to_string(),
            description: args.description.unwrap_or_default(),
            project: args.project.filter(|p| !p.is_empty()),
            priority: check_choice("priority", args.priority, &PRIORITIES)?,
            status: None,
            due: check_date(args.due.as_deref())?,
        })
    }

    fn propose_task_update(&self, args: ProposeUpdateArgs) -> Result<ChatProposal> {
        let task = self.tasks.get(&args.task_id)?.task;
        // le modèle renvoie souvent les valeurs inchangées : seules les vraies modifications restent
        let status = check_choice("status", args.status, &STATUSES)?.filter(|s| *s != task.status);
        let priority = check_choice("priority", args.priority, &PRIORITIES)?.filter(|p| *p != task.priority);
        let due = check_date(args.due.as_deref())?.filter(|d| Some(d) != task.due.as_ref());
        let note = args.note.unwrap_or_default().trim().to_string();
        let new_title = args.title.map(|t| t.trim().to_string()).filter(|t| !t.is_empty() && *t != task.title);
        let project = args.project.map(|p| p.trim().to_string()).filter(|p| !p.is_empty() && Some(p) != task.project.as_ref());
        let iteration = args.iteration.map(|w| w.trim().to_string()).filter(|w| !w.is_empty());
        if let Some(week) = &iteration {
            week_start(week)?;
        }
        if [&status, &priority, &due, &new_title, &project, &iteration].iter().all(|v| v.is_none()) && note.is_empty() {
            return Err(vault_err("Aucune modification proposée"));
        }
        Ok(ChatProposal::UpdateTask { task_id: task.id, title: task.title, status, priority, due, note, new_title, project, iteration })
    }

    fn propose_journal_line(&self, args: LineArgs) -> Result<ChatProposal> {
        if args.line.trim().is_empty() {
            return Err(vault_err("La ligne de journal est vide"));
        }
        Ok(ChatProposal::Journal { line: args.line.trim().to_string() })
    }

    fn propose_note_create(&self, args: NoteCreateArgs) -> Result<ChatProposal> {
        let NoteCreateArgs { note_type, title, date, content } = args;
        check_choice("type", Some(note_type.clone()), &NOTE_KINDS)?;
        if title.trim().is_empty() {
            return Err(vault_err("Le titre de la note est obligatoire"));
        }
        let date = check_date(date.as_deref())?;
        Ok(ChatProposal::CreateNote { note_type, title: title.trim().into(), date, content: content.unwrap_or_default() })
    }

    /// Note visée par une proposition : chemin et titre.
    fn target(&self, name: &str) -> Result<(String, String)> {
        let path = self.resolve(name)?;
        let note = self.index.get(&path)?.ok_or_else(|| not_found(format!("Note introuvable : {path}")))?;
        Ok((path, note.title))
    }

    /// Lien `[[…]]` vers une note, avec son chemin.
    fn link_to(&self, name: &str) -> Result<(String, String)> {
        let path = self.resolve(name)?;
        let note = self.index.get(&path)?.ok_or_else(|| not_found(format!("Note introuvable : {path}")))?;
        Ok((format!("[[{}]]", note.link_name()), path))
    }

    /// Vérifie qu'une opération sur une note s'applique à son contenu actuel.
    fn checked(&self, proposal: ChatProposal) -> Result<ChatProposal> {
        let text = self.vault.read_text(proposal_path(&proposal))?;
        self.transform(&proposal, &text)?;
        Ok(proposal)
    }

    fn propose_note_meta(&self, args: NoteMetaArgs) -> Result<ChatProposal> {
        let (path, title) = self.target(&args.name)?;
        let (meta, _) = split_frontmatter(&self.vault.read_text(&path)?);
        let before = args.set.keys().chain(&args.remove).map(|k| (k.clone(), meta.get(k).cloned().unwrap_or(Value::Null))).collect();
        self.checked(ChatProposal::UpdateMeta { path, title, set: args.set, remove: args.remove, before })
    }

    fn propose_note_delete(&self, name: &str) -> Result<ChatProposal> {
        let (path, title) = self.target(name)?;
        let backlinks = self.index.backlinks(&path)?.iter().map(|n| format!("[[{}]]", n.link_name())).collect();
        Ok(ChatProposal::DeleteNote { path, title, backlinks })
    }

    /// La cible d'un lien `[[nom]]` est-elle la note `target_path` ?
    fn points_to(&self, name: &str, target_path: &str) -> bool {
        name == target_path || self.index.resolve_link(name).ok().flatten().as_deref() == Some(target_path)
    }

    /// Nouveau texte d'une note après une opération (erreur si elle ne s'applique pas).
    fn transform(&self, proposal: &ChatProposal, text: &str) -> Result<String> {
        let (raw_meta, body) = split_raw(text);
        match proposal {
            ChatProposal::EditNote { edits, .. } => Ok(format!("{raw_meta}{}", apply_edits(body, edits)?)),
            ChatProposal::AppendNote { section, text: added, .. } => {
                Ok(format!("{raw_meta}{}", append_text(body, section.as_deref(), added)?))
            }
            ChatProposal::UpdateMeta { path, set, remove, .. } => {
                let (mut meta, _) = split_frontmatter(text);
                let is_task = infer_type(path) == "tache" || meta.get("type").and_then(Value::as_str) == Some("tache");
                if let Some(key) = set.keys().chain(remove).find(|k| is_task && EDITABLE_FIELDS.contains(&k.as_str())) {
                    return Err(vault_err(format!("Le champ {key} d'une tâche se modifie avec propose_task_update")));
                }
                update_meta(&mut meta, set, remove)?;
                Ok(with_meta(&meta, body))
            }
            ChatProposal::Link { link, target_path, field, section, path, .. } => {
                if target_path == path {
                    return Err(vault_err("Une note ne peut pas pointer vers elle-même"));
                }
                add_link(text, link, field.as_deref(), section.as_deref(), |name| self.points_to(name, target_path))
            }
            ChatProposal::Unlink { target_path, .. } => {
                Ok(remove_link(text, |name| self.points_to(name, target_path))?.0)
            }
            _ => Err(vault_err("Cette proposition ne modifie pas une note existante")),
        }
    }

    /// Applique une proposition validée par l'utilisateur.
    pub fn apply(&self, proposal: &ChatProposal, today_date: Option<NaiveDate>) -> Result<Value> {
        let today_str = today_date.unwrap_or_else(today).to_string();
        match proposal {
            ChatProposal::CreateTask { title, description, project, priority, status, due } => {
                let new = NewTask {
                    title: title.clone(),
                    project: project.clone(),
                    priority: priority.clone().filter(|p| !p.is_empty()),
                    status: status.clone().filter(|s| !s.is_empty()),
                    due: due.clone(),
                    description: description.clone(),
                    origin: None,
                };
                let task = self.tasks.create(new, Some(&today_str))?.task;
                Ok(json!({"path": task.path, "message": format!("Tâche {} créée", task.id)}))
            }
            ChatProposal::UpdateTask { task_id, status, priority, due, note, new_title, project, iteration, .. } => {
                let mut changes = Map::new();
                let fields = [("status", status), ("priority", priority), ("due", due), ("title", new_title), ("project", project), ("iteration", iteration)];
                for (key, value) in fields {
                    if let Some(value) = value.as_ref().filter(|v| !v.is_empty()) {
                        changes.insert(key.into(), value.clone().into());
                    }
                }
                let task = self.tasks.update(task_id, &changes, Some(note), Some(&today_str))?.task;
                Ok(json!({"path": task.path, "message": format!("Tâche {} mise à jour", task.id)}))
            }
            ChatProposal::Journal { line } => {
                let line = line.trim();
                if line.is_empty() {
                    return Err(vault_err("La ligne de journal est vide"));
                }
                let path = self.journal.append(&today_str, "## Notes", &format!("- {line}"))?;
                Ok(json!({"path": path, "message": "Ligne ajoutée au journal"}))
            }
            ChatProposal::CreateNote { note_type, title, date, content } => {
                let date = date.as_deref().unwrap_or(&today_str);
                let path = self.journal.create_note_with_body(note_type, title, Some(date), Some(content))?;
                Ok(json!({"path": path, "message": "Note créée"}))
            }
            ChatProposal::DeleteNote { path, .. } => {
                self.vault.delete(path)?;
                self.index.remove(path)?;
                Ok(json!({"path": Value::Null, "message": "Note supprimée (récupérable dans .trash)"}))
            }
            _ => {
                // rejouée sur le contenu actuel ; le hash protège d'une écriture concurrente
                let path = proposal_path(proposal);
                let current = self.vault.read_text(path)?;
                let updated = self.transform(proposal, &current)?;
                self.vault.write(path, &updated, Some(&content_hash(&current)), false)?;
                self.index.upsert(path)?;
                Ok(json!({"path": path, "message": "Note modifiée"}))
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
            "propose_note_create" => ToolOutput::Proposal(self.propose_note_create(args!(NoteCreateArgs))?),
            "propose_note_edit" => {
                let a = args!(NoteEditArgs);
                let (path, title) = self.target(&a.name)?;
                ToolOutput::Proposal(self.checked(ChatProposal::EditNote { path, title, edits: a.edits })?)
            }
            "propose_note_append" => {
                let a = args!(NoteAppendArgs);
                let (path, title) = self.target(&a.name)?;
                let section = a.section.filter(|s| !s.trim().is_empty()).map(|s| heading(&s));
                ToolOutput::Proposal(self.checked(ChatProposal::AppendNote { path, title, section, text: a.text })?)
            }
            "propose_note_meta" => ToolOutput::Proposal(self.propose_note_meta(args!(NoteMetaArgs))?),
            "propose_note_delete" => ToolOutput::Proposal(self.propose_note_delete(&args!(NameArgs).name)?),
            "propose_link" => {
                let a = args!(LinkArgs);
                let (path, title) = self.target(&a.name)?;
                let (link, target_path) = self.link_to(&a.target)?;
                let field = a.field.filter(|f| !f.trim().is_empty());
                let section = a.section.filter(|s| !s.trim().is_empty()).map(|s| heading(&s));
                ToolOutput::Proposal(self.checked(ChatProposal::Link { path, title, link, target_path, field, section })?)
            }
            "propose_unlink" => {
                let a = args!(UnlinkArgs);
                let (path, title) = self.target(&a.name)?;
                let (link, target_path) = self.link_to(&a.target)?;
                ToolOutput::Proposal(self.checked(ChatProposal::Unlink { path, title, link, target_path })?)
            }
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
