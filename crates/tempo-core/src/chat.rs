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
use crate::drafts::DraftService;
use crate::error::{not_found, vault_err, Error, Result};
use crate::imputations::{Account, Hours, ImputationService, CATEGORIES};
use crate::inbox::{split_summaries, InboxItem, InboxProposal, InboxService};
use crate::index::{extract_links, infer_type, Index, NoteRow, SearchMode};
use crate::journal::{JournalService, NOTE_KINDS};
use crate::library::LibraryService;
use crate::library_agent::{self, ReadLog};
use crate::llm::{Llm, Msg, ToolDef};
use crate::prompts::CHAT_SYSTEM;
use crate::reports::ReportService;
use crate::semantic::SemanticIndex;
use crate::notes::{add_link, append_text, apply_edits, heading, remove_link, split_raw, update_meta, with_meta, TextEdit};
use crate::tasks::{week_start, NewTask, Task, TaskService, EDITABLE_FIELDS, PRIORITIES, STATUSES};
use crate::util::{clip, date_or_today, iso_date, parse_date, today, week_bounds, week_label};
use crate::vault::{content_hash, join_frontmatter, split_frontmatter, value_str, Vault};

pub const MAX_STEPS: usize = 12;
const MAX_NOTE_CHARS: usize = 8000;
const MAX_RESULT_CHARS: usize = 12000;
const MAX_HISTORY: usize = 20;
const WEEKDAYS: [&str; 7] = ["lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi", "dimanche"];
/// modes de `search_notes`
const SEARCH_MODES: [&str; 3] = ["mots", "phrase", "ou"];
const MAX_GREP_HITS: usize = 60;
/// types proposés à `list_notes` (les tâches ont leur propre outil)
const LISTED_KINDS: [&str; 10] = ["reunion", "echange", "journal", "sujet", "projet", "personne", "ressource", "formation", "rapport", "synthese"];

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
        /// identifiants des projets auxquels la note se rattache
        #[serde(default)]
        projects: Vec<String>,
        /// documents de la bibliothèque cités dans le contenu (clé `sources` du frontmatter)
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        sources: Vec<String>,
        /// citations de documents du contenu, vérifiées en fin de réponse (affichage seulement)
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        citations: Vec<Value>,
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
    /// place la tâche dans l'ordre global du backlog (partagé par le board), avant la tâche
    /// `before` (à la fin sans `before`)
    MoveTask {
        task_id: String,
        #[serde(default)]
        title: String,
        before: Option<String>,
    },
    CreateProject { title: String },
    /// ordre des projets (features) dans le backlog
    MoveProject {
        project_id: String,
        #[serde(default)]
        title: String,
        before: Option<String>,
    },
    /// compte d'imputation du projet (`None` : détaché)
    SetProjectAccount {
        project_id: String,
        #[serde(default)]
        title: String,
        compte: Option<String>,
    },
    CreateAccount {
        title: String,
        alias: String,
        categorie: String,
        #[serde(default)]
        days: f64,
        #[serde(default)]
        description: String,
    },
    /// champs à `None` inchangés ; `before` : valeurs d'origine pour l'affichage
    UpdateAccount {
        alias: String,
        title: Option<String>,
        categorie: Option<String>,
        days: Option<f64>,
        description: Option<String>,
        #[serde(default)]
        before: Map<String, Value>,
    },
    /// heures de toute la semaine contenant `date` (remplacées), par jour puis par compte
    SaveImputations {
        date: String,
        #[serde(default)]
        week: String,
        hours: Hours,
        #[serde(default)]
        comment: String,
        #[serde(default)]
        before: Hours,
    },
    /// date de saisie des imputations dans l'outil officiel, ajoutée ou retirée
    Saisie { date: String, done: bool },
    /// résumé(s) de mails ou de messages ajoutés à l'inbox
    InboxAdd { text: String },
    InboxDiscard {
        path: String,
        #[serde(default)]
        preview: String,
    },
    /// élément de l'inbox traité selon l'analyse habituelle (échange, tâches, journal)
    InboxApply { path: String, proposal: InboxProposal },
    /// rapport hebdomadaire de la semaine contenant `date` (généré à la validation)
    WeeklyReport {
        date: Option<String>,
        #[serde(default)]
        week: String,
        #[serde(default)]
        extra: String,
    },
    /// rapport d'avancement d'une tâche (généré à la validation)
    TaskReport {
        task_id: String,
        #[serde(default)]
        title: String,
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

fn check_days(value: Option<f64>) -> Result<Option<f64>> {
    match value {
        Some(days) if !days.is_finite() || days < 0.0 => Err(vault_err(format!("Nombre de jours invalide : {days}"))),
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
struct SearchArgs {
    query: String,
    #[serde(default)]
    mode: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GrepArgs {
    pattern: String,
    #[serde(default)]
    regex: bool,
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    folder: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FindArgs {
    #[serde(default)]
    name: Option<String>,
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    meta: Map<String, Value>,
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
    #[serde(default)]
    projects: Option<Vec<String>>,
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WeekArgs {
    #[serde(default)]
    week: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DateArgs {
    #[serde(default)]
    date: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskMoveArgs {
    task_id: String,
    #[serde(default)]
    before: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TitleArgs {
    title: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectMoveArgs {
    project_id: String,
    #[serde(default)]
    before: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectAccountArgs {
    project_id: String,
    #[serde(default)]
    compte: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountCreateArgs {
    title: String,
    alias: String,
    #[serde(default)]
    categorie: Option<String>,
    #[serde(default)]
    days: Option<f64>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountUpdateArgs {
    alias: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    categorie: Option<String>,
    #[serde(default)]
    days: Option<f64>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImputationsArgs {
    date: String,
    hours: Hours,
    #[serde(default)]
    comment: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SaisieArgs {
    date: String,
    done: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TextArgs {
    text: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathArgs {
    path: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WeeklyReportArgs {
    #[serde(default)]
    date: Option<String>,
    #[serde(default)]
    extra: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskIdArgs {
    task_id: String,
}

/// Résultat d'un outil : des données pour le modèle, ou une proposition à valider.
enum ToolOutput {
    Data(Value),
    Proposal(Box<ChatProposal>),
}

impl ToolOutput {
    fn proposal(proposal: ChatProposal) -> Self {
        ToolOutput::Proposal(Box::new(proposal))
    }
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

/// Filtre facultatif sur le type de note (les modèles ont tendance à le remplir sinon).
fn any_kind() -> Value {
    json!({"type": "string", "enum": LISTED_KINDS,
           "description": "facultatif : limite à un type de note ; à omettre pour chercher dans toutes les notes"})
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
        tool("search_notes", "Recherche plein texte dans le titre et le corps des notes (porte sur des mots), \
                              classée par pertinence.",
             object(&["query"], json!({
                 "query": text("mots-clés"),
                 "mode": {"type": "string", "enum": SEARCH_MODES,
                          "description": "mots : tous les mots (par défaut) ; phrase : les mots côte à côte ; ou : l'un des mots"},
             }))),
        tool("grep_notes",
             "Recherche exacte (ou par expression régulière) ligne par ligne dans le texte complet des notes, \
              frontmatter compris, sans tenir compte de la casse. Renvoie les lignes trouvées : utile pour une \
              référence, une date, une adresse, ou pour retrouver un passage exact avant propose_note_edit. \
              Sans type ni dossier, un seul appel parcourt tout le vault (tâches comprises).",
             object(&["pattern"], json!({
                 "pattern": text("texte exact cherché (ou expression régulière si regex est vrai)"),
                 "regex": {"type": "boolean", "description": "pattern est une expression régulière (syntaxe Rust)"},
                 "type": any_kind(), "folder": text("facultatif : dossier du vault, ex. reunions/2026 (tout le vault sinon)"),
             }))),
        tool("find_notes",
             "Notes par nom de fichier ou titre (morceau, sans tenir compte de la casse ni des accents), par type, \
              et par valeurs du frontmatter (pour une liste, l'un de ses éléments ; [[x]] vaut x). Ex. réunions où \
              Marc Leroy était présent : {\"type\": \"reunion\", \"meta\": {\"participants\": \"Marc Leroy\"}}.",
             object(&[], json!({
                 "name": text("morceau du nom ou du titre"), "type": any_kind(),
                 "meta": {"type": "object", "description": "clé du frontmatter : valeur attendue",
                          "additionalProperties": {"type": ["string", "number", "boolean"]}},
             }))),
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
        tool("task_board", "Board d'une semaine : tâches planifiées cette semaine-là, avec leur statut.",
             object(&[], json!({"week": text("semaine AAAA-Snn, la semaine courante par défaut")}))),
        tool("task_backlog", "Backlog : toutes les tâches dans l'ordre du backlog (un seul ordre, repris par le board).",
             object(&[], json!({}))),
        tool("list_projects", "Projets (features du backlog), dans l'ordre du backlog, avec leur compte d'imputation.",
             object(&[], json!({}))),
        tool("list_accounts", "Comptes d'imputation : alias, catégorie, jours prévus, consommés et restants.",
             object(&[], json!({}))),
        tool("get_imputations", "Heures imputées sur la semaine contenant une date, par jour et par compte.",
             object(&[], json!({"date": date("un jour de la semaine, aujourd'hui par défaut")}))),
        tool("suggest_imputations",
             "Répartition des heures de la semaine proposée à partir de l'activité (tâches, réunions, journal). \
              Ne modifie rien : sert à préparer propose_imputations.",
             object(&[], json!({"date": date("un jour de la semaine, aujourd'hui par défaut")}))),
        tool("imputation_tracking", "Dates de saisie des imputations dans l'outil officiel et périodes à compléter.",
             object(&[], json!({}))),
        tool("list_inbox", "Éléments en attente dans l'inbox (résumés de mails ou de messages Teams).",
             object(&[], json!({}))),
        tool("list_drafts", "Brouillons de l'espace Draft (titre et début du contenu).", object(&[], json!({}))),
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
                 "projects": {"type": "array", "items": {"type": "string"},
                              "description": "identifiants des projets dont traite la note (list_projects)"},
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
        tool("propose_task_move",
             &format!("{PROPOSAL}place une tâche dans l'ordre du backlog (un seul ordre pour toutes les \
                       tâches, repris par le board), avant une autre tâche (à la fin sans before). Pour \
                       changer son projet, utilise propose_task_update."),
             object(&["task_id"], json!({
                 "task_id": text("tâche à déplacer"), "before": text("tâche devant laquelle se placer"),
             }))),
        tool("propose_project_create", &format!("{PROPOSAL}nouveau projet (feature du backlog)."),
             object(&["title"], json!({"title": text("nom du projet")}))),
        tool("propose_project_move", &format!("{PROPOSAL}ordre d'un projet dans le backlog."),
             object(&["project_id"], json!({
                 "project_id": text("identifiant du projet"), "before": text("projet devant lequel se placer (fin sinon)"),
             }))),
        tool("propose_project_account", &format!("{PROPOSAL}compte d'imputation d'un projet (absent : détacher)."),
             object(&["project_id"], json!({"project_id": text("identifiant du projet"), "compte": text("alias du compte")}))),
        tool("propose_account_create", &format!("{PROPOSAL}nouveau compte d'imputation."),
             object(&["title", "alias"], json!({
                 "title": text("nom du compte"), "alias": text("alias court, sans espace (ex. SAP-MIG)"),
                 "categorie": choice(&CATEGORIES), "days": {"type": "number", "description": "jours prévus"},
                 "description": text("description"),
             }))),
        tool("propose_account_update", &format!("{PROPOSAL}modification d'un compte d'imputation."),
             object(&["alias"], json!({
                 "alias": text("alias du compte"), "title": text("nouveau nom"), "categorie": choice(&CATEGORIES),
                 "days": {"type": "number", "description": "jours prévus"},
                 "description": text("description (vide pour la retirer)"),
             }))),
        tool("propose_imputations",
             &format!("{PROPOSAL}heures de la semaine contenant date. Toute la semaine est remplacée : reprends \
                       les heures existantes (get_imputations) à conserver. 8 h par jour ouvré."),
             object(&["date", "hours"], json!({
                 "date": date("un jour de la semaine"),
                 "hours": {"type": "object", "description": "par jour (AAAA-MM-JJ) puis par alias de compte : heures, \
                           ex. {\"2026-10-19\": {\"SAP-MIG\": 6, \"TRANSVERSE\": 2}}",
                           "additionalProperties": {"type": "object", "additionalProperties": {"type": "number"}}},
                 "comment": text("commentaire de la semaine"),
             }))),
        tool("propose_saisie",
             &format!("{PROPOSAL}date de saisie des imputations dans l'outil officiel, ajoutée (done: true) ou retirée."),
             object(&["date", "done"], json!({"date": date("date de saisie"), "done": {"type": "boolean"}}))),
        tool("propose_inbox_add",
             &format!("{PROPOSAL}résumé(s) de mails ou de messages à ajouter à l'inbox (plusieurs résumés \
                       séparés par une ligne ---)."),
             object(&["text"], json!({"text": text("résumé(s)")}))),
        tool("propose_inbox_discard", &format!("{PROPOSAL}élément de l'inbox écarté (corbeille)."),
             object(&["path"], json!({"path": text("chemin donné par list_inbox")}))),
        tool("propose_inbox_process",
             &format!("{PROPOSAL}traitement d'un élément de l'inbox : il est analysé comme dans l'espace Inbox \
                       (note d'échange, tâches à créer ou mettre à jour, ligne de journal)."),
             object(&["path"], json!({"path": text("chemin donné par list_inbox")}))),
        tool("propose_weekly_report",
             &format!("{PROPOSAL}rapport hebdomadaire de la semaine contenant date, généré à la validation."),
             object(&[], json!({
                 "date": date("un jour de la semaine, aujourd'hui par défaut"),
                 "extra": text("éléments à intégrer au rapport"),
             }))),
        tool("propose_task_report", &format!("{PROPOSAL}rapport d'avancement d'une tâche, généré à la validation."),
             object(&["task_id"], json!({"task_id": text("identifiant, ex. T-001")}))),
    ]
}

/// Services sur lesquels l'agent s'appuie, pour consulter et pour appliquer les propositions.
#[derive(Clone)]
pub struct ChatDeps {
    pub vault: Arc<Vault>,
    pub index: Arc<Index>,
    pub tasks: Arc<TaskService>,
    pub journal: Arc<JournalService>,
    pub inbox: Arc<InboxService>,
    pub reports: Arc<ReportService>,
    pub imputations: Arc<ImputationService>,
    pub drafts: Arc<DraftService>,
    /// bibliothèque de documents (outils search_documents, read_document, list_documents)
    pub library: Option<Arc<LibraryService>>,
}

pub struct ChatService {
    vault: Arc<Vault>,
    index: Arc<Index>,
    tasks: Arc<TaskService>,
    journal: Arc<JournalService>,
    inbox: Arc<InboxService>,
    reports: Arc<ReportService>,
    imputations: Arc<ImputationService>,
    drafts: Arc<DraftService>,
    library: Option<Arc<LibraryService>>,
    llm: Arc<dyn Llm>,
    settings: Arc<Settings>,
    semantic: Option<Arc<SemanticIndex>>,
}

impl ChatService {
    pub fn new(deps: ChatDeps, llm: Arc<dyn Llm>, settings: Arc<Settings>, semantic: Option<Arc<SemanticIndex>>) -> Self {
        let ChatDeps { vault, index, tasks, journal, inbox, reports, imputations, drafts, library } = deps;
        ChatService {
            vault,
            index,
            tasks,
            journal,
            inbox,
            reports,
            imputations,
            drafts,
            library,
            llm,
            settings,
            semantic: semantic.filter(|s| s.enabled()),
        }
    }

    /// Outils proposés au modèle (avec la recherche sémantique si elle est active).
    pub fn tools(&self) -> Vec<ToolDef> {
        let mut tools = read_tools();
        if self.semantic.is_some() {
            tools.push(semantic_tool());
        }
        if self.library.is_some() {
            tools.extend(library_agent::tool_defs());
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

    /// `mode` : `mots` (par défaut), `phrase` ou `ou`.
    pub fn search_notes(&self, query: &str, mode: Option<&str>) -> Result<Value> {
        let mode = match check_choice("mode", mode.map(String::from), &SEARCH_MODES)?.as_deref() {
            Some("phrase") => SearchMode::Phrase,
            Some("ou") => SearchMode::Any,
            _ => SearchMode::Words,
        };
        let mut results = Vec::new();
        for hit in self.index.search_with(query, mode, 12)? {
            if let Some(note) = self.index.get(&hit.path)?.filter(|n| !n.is_template()) {
                let mut item = Self::note_ref(&note);
                item.insert("extrait".into(), hit.snippet.into());
                results.push(Value::Object(item));
            }
        }
        Ok(Value::Array(results))
    }

    /// Lignes trouvées, regroupées par note. Le texte est cherché tel quel, sauf avec `regex`.
    pub fn grep_notes(&self, pattern: &str, regex: bool, kind: Option<&str>, folder: Option<&str>) -> Result<Value> {
        if pattern.is_empty() {
            return Err(vault_err("Le texte cherché est vide"));
        }
        let source = if regex { pattern.to_string() } else { regex::escape(pattern) };
        let pattern = regex::RegexBuilder::new(&source)
            .case_insensitive(true)
            .size_limit(1 << 20)
            .build()
            .map_err(|e| vault_err(format!("Expression régulière invalide : {e}")))?;
        let hits = self.index.grep(&pattern, kind.filter(|k| !k.is_empty()), folder, MAX_GREP_HITS)?;
        let mut notes: Vec<Map<String, Value>> = Vec::new();
        for hit in &hits {
            if notes.last().is_none_or(|n| n["chemin"] != hit.path.as_str()) {
                let Some(note) = self.index.get(&hit.path)? else { continue };
                let mut item = Self::note_ref(&note);
                item.insert("lignes".into(), json!([]));
                notes.push(item);
            }
            let text: String = hit.text.chars().take(200).collect();
            notes.last_mut().unwrap()["lignes"].as_array_mut().unwrap().push(json!({"n": hit.line, "texte": text}));
        }
        let mut result = json!({"notes": notes});
        if hits.len() >= MAX_GREP_HITS {
            result["tronque"] = format!("{MAX_GREP_HITS} lignes au plus : précise la recherche").into();
        }
        Ok(result)
    }

    pub fn find_notes(&self, name: Option<&str>, kind: Option<&str>, meta: &Map<String, Value>) -> Result<Value> {
        let meta: Vec<(String, String)> = meta.iter().map(|(k, v)| (k.clone(), value_str(v))).collect();
        let notes = self.index.find_notes(name, kind.filter(|k| !k.is_empty()), &meta)?;
        let skip = notes.len().saturating_sub(60);
        Ok(Value::Array(notes[skip..].iter().map(|n| Value::Object(Self::note_ref(n))).collect()))
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
            .filter(|n| !n.is_template() && !["inbox", "tache"].contains(&n.kind.as_str()))
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

    fn task_ref(t: &Task) -> Value {
        json!({"id": t.id, "title": t.title, "status": t.status, "priority": t.priority, "project": t.project,
               "due": t.due, "iteration": t.iteration, "lien": format!("[[{}]]", t.id)})
    }

    pub fn task_board(&self, week: Option<&str>) -> Result<Value> {
        let board = self.tasks.board(week, None)?;
        Ok(json!({"week": board.week, "current": board.current, "start": board.start, "end": board.end,
                  "tasks": board.tasks.iter().map(Self::task_ref).collect::<Vec<_>>()}))
    }

    pub fn task_backlog(&self) -> Result<Value> {
        let backlog = self.tasks.backlog(None)?;
        Ok(json!({"current": backlog.current,
                  "weeks": backlog.weeks.iter().map(|w| w.label.clone()).collect::<Vec<_>>(),
                  "tasks": backlog.tasks.iter().map(Self::task_ref).collect::<Vec<_>>()}))
    }

    pub fn list_projects(&self) -> Result<Value> {
        Ok(Value::Array(
            self.imputations
                .projects_by_rank()?
                .into_iter()
                .map(|p| json!({"id": p.id, "title": p.title, "compte": p.compte, "lien": format!("[[{}]]", p.id)}))
                .collect(),
        ))
    }

    pub fn list_accounts(&self) -> Result<Value> {
        Ok(Value::Array(
            self.imputations
                .accounts()?
                .into_iter()
                .map(|a| {
                    json!({"alias": a.alias, "title": a.title, "categorie": a.categorie, "description": a.description,
                           "days": a.days, "consumed_days": a.consumed_days, "remaining_days": a.remaining_days,
                           "projects": a.projects})
                })
                .collect(),
        ))
    }

    pub fn get_imputations(&self, date: Option<&str>) -> Result<Value> {
        let week = self.imputations.week(date_or_today(date)?)?;
        Ok(json!({"week": week.week, "days": week.days, "hours": week.hours, "totals": week.totals, "comment": week.comment}))
    }

    pub fn suggest_imputations(&self, date: Option<&str>) -> Result<Value> {
        self.imputations.propose(date_or_today(date)?)
    }

    pub fn imputation_tracking(&self) -> Result<Value> {
        Ok(serde_json::to_value(self.imputations.tracking(today())?).unwrap())
    }

    pub fn list_inbox(&self) -> Result<Value> {
        Ok(Value::Array(
            self.inbox
                .list()?
                .into_iter()
                .map(|item| json!({"path": item.path, "contenu": clip(item.content.trim(), 3000)}))
                .collect(),
        ))
    }

    pub fn list_drafts(&self) -> Result<Value> {
        Ok(Value::Array(
            self.drafts
                .list()?
                .into_iter()
                .map(|d| json!({"id": d.id, "title": d.title, "updated": d.updated, "debut": clip(d.content.trim(), 500)}))
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
        let NoteCreateArgs { note_type, title, date, content, projects } = args;
        check_choice("type", Some(note_type.clone()), &NOTE_KINDS)?;
        if title.trim().is_empty() {
            return Err(vault_err("Le titre de la note est obligatoire"));
        }
        let date = check_date(date.as_deref())?;
        let projects: Vec<String> =
            projects.unwrap_or_default().into_iter().map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect();
        for project in &projects {
            self.project_title(project)?;
        }
        Ok(ChatProposal::CreateNote {
            note_type,
            title: title.trim().into(),
            date,
            content: content.unwrap_or_default(),
            projects,
            sources: Vec::new(),
            citations: Vec::new(),
        })
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

    fn project_title(&self, project_id: &str) -> Result<String> {
        self.imputations
            .projects()?
            .into_iter()
            .find(|p| p.id == project_id)
            .map(|p| p.title)
            .ok_or_else(|| not_found(format!("Projet introuvable : {project_id} (voir list_projects)")))
    }

    fn account(&self, alias: &str) -> Result<Account> {
        self.imputations
            .accounts()?
            .into_iter()
            .find(|a| a.alias.eq_ignore_ascii_case(alias.trim()))
            .ok_or_else(|| not_found(format!("Compte d'imputation introuvable : {alias} (voir list_accounts)")))
    }

    fn propose_task_move(&self, args: TaskMoveArgs) -> Result<ChatProposal> {
        let task = self.tasks.get(&args.task_id)?.task;
        let before = args.before.map(|b| b.trim().to_string()).filter(|b| !b.is_empty());
        if let Some(before) = &before {
            if self.tasks.get(before)?.task.id == task.id {
                return Err(vault_err("Une tâche ne peut pas se placer devant elle-même"));
            }
        }
        Ok(ChatProposal::MoveTask { task_id: task.id, title: task.title, before })
    }

    fn propose_project_create(&self, title: &str) -> Result<ChatProposal> {
        let title = title.trim();
        if title.is_empty() {
            return Err(vault_err("Le nom du projet est obligatoire"));
        }
        if self.imputations.projects()?.iter().any(|p| p.title.to_lowercase() == title.to_lowercase()) {
            return Err(vault_err(format!("Le projet « {title} » existe déjà")));
        }
        Ok(ChatProposal::CreateProject { title: title.into() })
    }

    fn propose_project_move(&self, args: ProjectMoveArgs) -> Result<ChatProposal> {
        let title = self.project_title(&args.project_id)?;
        let before = args.before.map(|b| b.trim().to_string()).filter(|b| !b.is_empty());
        if let Some(before) = &before {
            if *before == args.project_id {
                return Err(vault_err("Un projet ne peut pas se placer devant lui-même"));
            }
            self.project_title(before)?;
        }
        Ok(ChatProposal::MoveProject { project_id: args.project_id, title, before })
    }

    fn propose_project_account(&self, args: ProjectAccountArgs) -> Result<ChatProposal> {
        let project = self.imputations.projects()?.into_iter().find(|p| p.id == args.project_id);
        let project = project.ok_or_else(|| not_found(format!("Projet introuvable : {} (voir list_projects)", args.project_id)))?;
        let compte = match args.compte.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
            Some(alias) => Some(self.account(alias)?.alias),
            None => None,
        };
        if compte == project.compte {
            return Err(vault_err("Le projet est déjà dans cet état"));
        }
        Ok(ChatProposal::SetProjectAccount { project_id: project.id, title: project.title, compte })
    }

    fn propose_account_create(&self, args: AccountCreateArgs) -> Result<ChatProposal> {
        let (title, alias) = (args.title.trim().to_string(), args.alias.trim().to_string());
        if title.is_empty() {
            return Err(vault_err("Le nom du compte est obligatoire"));
        }
        if alias.is_empty() || alias.chars().any(char::is_whitespace) {
            return Err(vault_err("Alias invalide : sans espace (ex. SAP-MIG)"));
        }
        if self.account(&alias).is_ok() || self.index.resolve_link(&alias)?.is_some() {
            return Err(vault_err(format!("L'alias « {alias} » est déjà utilisé")));
        }
        let categorie = check_choice("categorie", args.categorie, &CATEGORIES)?.unwrap_or_else(|| CATEGORIES[0].into());
        let days = check_days(args.days)?.unwrap_or(0.0);
        Ok(ChatProposal::CreateAccount { title, alias, categorie, days, description: args.description.unwrap_or_default() })
    }

    fn propose_account_update(&self, args: AccountUpdateArgs) -> Result<ChatProposal> {
        let account = self.account(&args.alias)?;
        let title = args.title.map(|t| t.trim().to_string()).filter(|t| !t.is_empty() && *t != account.title);
        let categorie = check_choice("categorie", args.categorie, &CATEGORIES)?.filter(|c| *c != account.categorie);
        let days = check_days(args.days)?.filter(|d| *d != account.days);
        let description = args.description.map(|d| d.trim().to_string()).filter(|d| *d != account.description);
        let mut before = Map::new();
        for (key, changed, current) in [
            ("title", title.is_some(), json!(account.title)),
            ("categorie", categorie.is_some(), json!(account.categorie)),
            ("days", days.is_some(), json!(account.days)),
            ("description", description.is_some(), json!(account.description)),
        ] {
            if changed {
                before.insert(key.into(), current);
            }
        }
        if before.is_empty() {
            return Err(vault_err("Aucune modification proposée"));
        }
        Ok(ChatProposal::UpdateAccount { alias: account.alias, title, categorie, days, description, before })
    }

    fn propose_imputations(&self, args: ImputationsArgs) -> Result<ChatProposal> {
        let day = parse_date(&args.date)?;
        let (start, end) = week_bounds(day);
        let current = self.imputations.week(day)?;
        let mut hours = Hours::new();
        for (date, entries) in &args.hours {
            let date = parse_date(date)?;
            if date < start || date > end {
                return Err(vault_err(format!("{date} n'appartient pas à la semaine {}", current.week)));
            }
            let mut kept = indexmap::IndexMap::new();
            for (alias, value) in entries {
                if !value.is_finite() || *value < 0.0 || *value > 24.0 {
                    return Err(vault_err(format!("Heures invalides pour {alias} le {date} : {value}")));
                }
                if *value > 0.0 {
                    *kept.entry(self.account(alias)?.alias).or_insert(0.0) += value;
                }
            }
            if kept.values().sum::<f64>() > 24.0 {
                return Err(vault_err(format!("Plus de 24 h le {date}")));
            }
            if !kept.is_empty() {
                hours.insert(date.to_string(), kept);
            }
        }
        hours.sort_keys();
        let comment = args.comment.unwrap_or_else(|| current.comment.clone());
        if hours == current.hours && comment.trim() == current.comment.trim() {
            return Err(vault_err("Aucune modification proposée"));
        }
        Ok(ChatProposal::SaveImputations { date: day.to_string(), week: current.week, hours, comment, before: current.hours })
    }

    fn propose_saisie(&self, args: SaisieArgs) -> Result<ChatProposal> {
        let date = parse_date(&args.date)?;
        let known = self.imputations.saisies()?.contains(&date);
        match (args.done, known) {
            (true, true) => Err(vault_err(format!("La saisie du {date} est déjà enregistrée"))),
            (false, false) => Err(vault_err(format!("Aucune saisie enregistrée le {date}"))),
            _ => Ok(ChatProposal::Saisie { date: date.to_string(), done: args.done }),
        }
    }

    fn propose_inbox_add(&self, text: &str) -> Result<ChatProposal> {
        if split_summaries(text).is_empty() {
            return Err(vault_err("Aucun résumé à ajouter"));
        }
        Ok(ChatProposal::InboxAdd { text: text.trim().into() })
    }

    /// Élément de l'inbox désigné par son chemin.
    fn inbox_item(&self, path: &str) -> Result<InboxItem> {
        let path = path.trim().replace('\\', "/");
        self.inbox
            .list()?
            .into_iter()
            .find(|item| item.path == path)
            .ok_or_else(|| not_found(format!("Élément d'inbox introuvable : {path} (voir list_inbox)")))
    }

    fn propose_weekly_report(&self, args: WeeklyReportArgs) -> Result<ChatProposal> {
        let date = check_date(args.date.as_deref())?;
        let week = week_label(date_or_today(date.as_deref())?);
        Ok(ChatProposal::WeeklyReport { date, week, extra: args.extra.unwrap_or_default().trim().into() })
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
            ChatProposal::CreateNote { note_type, title, date, content, projects, sources, .. } => {
                let date = date.as_deref().unwrap_or(&today_str);
                let path = self.journal.create_note_with_body(note_type, title, Some(date), Some(content), projects)?;
                // traçabilité : les documents cités sont listés dans le frontmatter
                if !sources.is_empty() {
                    let text = self.vault.read_text(&path)?;
                    let (mut meta, body) = split_frontmatter(&text);
                    let links: Vec<String> = sources.iter().map(|id| format!("[[{id}]]")).collect();
                    meta.insert("sources".into(), json!(links));
                    self.vault.write(&path, &join_frontmatter(&meta, body), None, false)?;
                    self.index.upsert(&path)?;
                }
                Ok(json!({"path": path, "message": "Note créée"}))
            }
            ChatProposal::DeleteNote { path, .. } => {
                self.vault.delete(path)?;
                self.index.remove(path)?;
                Ok(json!({"path": Value::Null, "message": "Note supprimée (récupérable dans .trash)"}))
            }
            ChatProposal::MoveTask { task_id, before, .. } => {
                self.tasks.move_task(task_id, before.as_deref(), None)?;
                Ok(json!({"path": self.tasks.path_of(task_id)?, "message": format!("Tâche {task_id} déplacée dans le backlog")}))
            }
            ChatProposal::CreateProject { title } => {
                let project = self.imputations.create_project(title)?;
                Ok(json!({"path": project.path, "message": format!("Projet {} créé", project.id)}))
            }
            ChatProposal::MoveProject { project_id, before, .. } => {
                self.imputations.move_project(project_id, before.as_deref())?;
                Ok(json!({"path": Value::Null, "message": format!("Projet {project_id} déplacé dans le backlog")}))
            }
            ChatProposal::SetProjectAccount { project_id, compte, .. } => {
                let project = self.imputations.set_project_account(project_id, compte.as_deref())?;
                Ok(json!({"path": project.path, "message": format!("Compte du projet {project_id} mis à jour")}))
            }
            ChatProposal::CreateAccount { title, alias, categorie, days, description } => {
                let account = self.imputations.create_account(title, alias, categorie, *days, description)?;
                Ok(json!({"path": account.path, "message": format!("Compte {} créé", account.alias)}))
            }
            ChatProposal::UpdateAccount { alias, title, categorie, days, description, .. } => {
                let account =
                    self.imputations.update_account(alias, title.as_deref(), categorie.as_deref(), *days, description.as_deref())?;
                Ok(json!({"path": account.path, "message": format!("Compte {} mis à jour", account.alias)}))
            }
            ChatProposal::SaveImputations { date, hours, comment, .. } => {
                let week = self.imputations.save_week(parse_date(date)?, hours, comment)?;
                Ok(json!({"path": week.path, "message": format!("Imputations {} enregistrées", week.week)}))
            }
            ChatProposal::Saisie { date, done } => {
                let date = parse_date(date)?;
                let tracking = if *done {
                    self.imputations.add_saisie(date, today())?
                } else {
                    self.imputations.remove_saisie(date, today())?
                };
                let message = if *done { format!("Saisie du {date} enregistrée") } else { format!("Saisie du {date} retirée") };
                Ok(json!({"path": tracking.path, "message": message}))
            }
            ChatProposal::InboxAdd { text } => {
                let paths = self.inbox.add(text)?;
                Ok(json!({"path": paths.first(), "message": format!("{} élément(s) ajouté(s) à l'inbox", paths.len())}))
            }
            ChatProposal::InboxDiscard { path, .. } => {
                self.inbox.discard(path)?;
                Ok(json!({"path": Value::Null, "message": "Élément de l'inbox écarté"}))
            }
            ChatProposal::InboxApply { path, proposal } => {
                let result = self.inbox.apply(path, proposal)?;
                Ok(json!({"path": result.get("echange").cloned().unwrap_or(Value::Null), "message": "Élément de l'inbox traité"}))
            }
            ChatProposal::WeeklyReport { date, extra, .. } => {
                let day = date.as_deref().map(parse_date).transpose()?;
                let result = self.reports.weekly(day, extra)?;
                Ok(json!({"path": result["path"], "message": "Rapport hebdomadaire généré"}))
            }
            ChatProposal::TaskReport { task_id, .. } => {
                let result = self.reports.task_report(task_id)?;
                Ok(json!({"path": result["path"], "message": format!("Rapport de la tâche {task_id} généré")}))
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
            "search_notes" => {
                let a = args!(SearchArgs);
                ToolOutput::Data(self.search_notes(&a.query, a.mode.as_deref())?)
            }
            "grep_notes" => {
                let a = args!(GrepArgs);
                ToolOutput::Data(self.grep_notes(&a.pattern, a.regex, a.kind.as_deref(), a.folder.as_deref())?)
            }
            "find_notes" => {
                let a = args!(FindArgs);
                ToolOutput::Data(self.find_notes(a.name.as_deref(), a.kind.as_deref(), &a.meta)?)
            }
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
            "task_board" => ToolOutput::Data(self.task_board(args!(WeekArgs).week.as_deref())?),
            "task_backlog" => {
                args!(NoArgs);
                ToolOutput::Data(self.task_backlog()?)
            }
            "list_projects" => {
                args!(NoArgs);
                ToolOutput::Data(self.list_projects()?)
            }
            "list_accounts" => {
                args!(NoArgs);
                ToolOutput::Data(self.list_accounts()?)
            }
            "get_imputations" => ToolOutput::Data(self.get_imputations(args!(DateArgs).date.as_deref())?),
            "suggest_imputations" => ToolOutput::Data(self.suggest_imputations(args!(DateArgs).date.as_deref())?),
            "imputation_tracking" => {
                args!(NoArgs);
                ToolOutput::Data(self.imputation_tracking()?)
            }
            "list_inbox" => {
                args!(NoArgs);
                ToolOutput::Data(self.list_inbox()?)
            }
            "list_drafts" => {
                args!(NoArgs);
                ToolOutput::Data(self.list_drafts()?)
            }
            "propose_task_move" => ToolOutput::proposal(self.propose_task_move(args!(TaskMoveArgs))?),
            "propose_project_create" => ToolOutput::proposal(self.propose_project_create(&args!(TitleArgs).title)?),
            "propose_project_move" => ToolOutput::proposal(self.propose_project_move(args!(ProjectMoveArgs))?),
            "propose_project_account" => ToolOutput::proposal(self.propose_project_account(args!(ProjectAccountArgs))?),
            "propose_account_create" => ToolOutput::proposal(self.propose_account_create(args!(AccountCreateArgs))?),
            "propose_account_update" => ToolOutput::proposal(self.propose_account_update(args!(AccountUpdateArgs))?),
            "propose_imputations" => ToolOutput::proposal(self.propose_imputations(args!(ImputationsArgs))?),
            "propose_saisie" => ToolOutput::proposal(self.propose_saisie(args!(SaisieArgs))?),
            "propose_inbox_add" => ToolOutput::proposal(self.propose_inbox_add(&args!(TextArgs).text)?),
            "propose_inbox_discard" => {
                let item = self.inbox_item(&args!(PathArgs).path)?;
                ToolOutput::proposal(ChatProposal::InboxDiscard { path: item.path, preview: item.preview })
            }
            "propose_inbox_process" => {
                let item = self.inbox_item(&args!(PathArgs).path)?;
                let proposal = self.inbox.analyze(&item.path)?;
                ToolOutput::proposal(ChatProposal::InboxApply { path: item.path, proposal })
            }
            "propose_weekly_report" => ToolOutput::proposal(self.propose_weekly_report(args!(WeeklyReportArgs))?),
            "propose_task_report" => {
                let task = self.tasks.get(&args!(TaskIdArgs).task_id)?.task;
                ToolOutput::proposal(ChatProposal::TaskReport { task_id: task.id, title: task.title })
            }
            "propose_task" => ToolOutput::proposal(self.propose_task(args!(ProposeTaskArgs))?),
            "propose_task_update" => ToolOutput::proposal(self.propose_task_update(args!(ProposeUpdateArgs))?),
            "propose_journal_line" => ToolOutput::proposal(self.propose_journal_line(args!(LineArgs))?),
            "propose_note_create" => ToolOutput::proposal(self.propose_note_create(args!(NoteCreateArgs))?),
            "propose_note_edit" => {
                let a = args!(NoteEditArgs);
                let (path, title) = self.target(&a.name)?;
                ToolOutput::proposal(self.checked(ChatProposal::EditNote { path, title, edits: a.edits })?)
            }
            "propose_note_append" => {
                let a = args!(NoteAppendArgs);
                let (path, title) = self.target(&a.name)?;
                let section = a.section.filter(|s| !s.trim().is_empty()).map(|s| heading(&s));
                ToolOutput::proposal(self.checked(ChatProposal::AppendNote { path, title, section, text: a.text })?)
            }
            "propose_note_meta" => ToolOutput::proposal(self.propose_note_meta(args!(NoteMetaArgs))?),
            "propose_note_delete" => ToolOutput::proposal(self.propose_note_delete(&args!(NameArgs).name)?),
            "propose_link" => {
                let a = args!(LinkArgs);
                let (path, title) = self.target(&a.name)?;
                let (link, target_path) = self.link_to(&a.target)?;
                let field = a.field.filter(|f| !f.trim().is_empty());
                let section = a.section.filter(|s| !s.trim().is_empty()).map(|s| heading(&s));
                ToolOutput::proposal(self.checked(ChatProposal::Link { path, title, link, target_path, field, section })?)
            }
            "propose_unlink" => {
                let a = args!(UnlinkArgs);
                let (path, title) = self.target(&a.name)?;
                let (link, target_path) = self.link_to(&a.target)?;
                ToolOutput::proposal(self.checked(ChatProposal::Unlink { path, title, link, target_path })?)
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
        self.run_tool_logged(name, args, proposals, &mut ReadLog::default())
    }

    /// Comme [`Self::run_tool`] ; les passages de documents lus sont consignés dans `reads`
    /// (vérification des citations).
    pub fn run_tool_logged(
        &self,
        name: Option<&str>,
        args: &Map<String, Value>,
        proposals: &mut Vec<ChatProposal>,
        reads: &mut ReadLog,
    ) -> Result<String> {
        let name = name.unwrap_or("");
        let mut limit = MAX_RESULT_CHARS;
        let library_call = self.library.as_ref().and_then(|library| library_agent::call(library, name, args, reads));
        let output = match library_call {
            Some(result) => {
                limit = library_agent::MAX_RESULT_CHARS;
                result.map(|r| r.map(ToolOutput::Data))
            }
            None => self.call_tool(name, args),
        };
        let output = match output {
            Ok(Ok(output)) => output,
            Ok(Err(message)) => return Ok(message),
            Err(err @ (Error::Vault(_) | Error::NotFound(_) | Error::Conflict { .. })) => return Ok(format!("Erreur : {err}")),
            Err(err) => return Err(err),
        };
        match output {
            ToolOutput::Proposal(proposal) => {
                proposals.push(*proposal);
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
                    Ok(clip(&value.to_string(), limit))
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
        let mut reads = ReadLog::default();
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
                        let result = self.run_tool_logged(Some(&name), &args, &mut proposals, &mut reads)?;
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
        // notes proposées (synthèses) : leurs citations de documents sont vérifiées comme celles de
        // la réponse, et les documents cités iront dans la clé `sources` du frontmatter
        if let Some(library) = &self.library {
            for proposal in proposals.iter_mut() {
                if let ChatProposal::CreateNote { content, sources, citations, .. } = proposal {
                    *citations = library_agent::citations(library, content, &reads, &|name| {
                        self.index.resolve_link(name).ok().flatten().is_some()
                    });
                    sources.clear();
                    for id in citations.iter().filter_map(|c| c["document"].as_str()) {
                        if !sources.iter().any(|s| s == id) {
                            sources.push(id.to_string());
                        }
                    }
                }
            }
        }
        let citations = match &self.library {
            Some(library) => library_agent::citations(library, &answer, &reads, &|name| {
                self.index.resolve_link(name).ok().flatten().is_some()
            }),
            None => Vec::new(),
        };
        emit(json!({"type": "done", "links": self.links_of(&answer)?, "answer": answer, "steps": steps,
                    "proposals": proposals, "citations": citations}));
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
