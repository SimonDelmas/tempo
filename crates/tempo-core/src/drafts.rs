//! Brouillons : textes libres rangés en onglets, que l'IA propose de transformer en note
//! ou en tâches.
//!
//! Chaque brouillon est un fichier `<vault>/.brouillons/<id>.md` (frontmatter `title`,
//! `created`, `updated`). Le dossier commence par un point : il est ignoré par l'index, les
//! brouillons ne sont donc pas des notes, mais ils vivent dans le vault et survivent aux
//! redémarrages comme aux mises à jour de l'application.

use std::collections::BTreeSet;
use std::sync::Arc;

use chrono::{Local, NaiveDate};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Value};

use crate::config::Settings;
use crate::de;
use crate::error::{not_found, vault_err, Result};
use crate::index::Index;
use crate::journal::{JournalService, NOTE_KINDS};
use crate::llm::{chat_json, Llm, Msg};
use crate::prompts::DRAFT_SYSTEM;
use crate::tasks::{NewTask, TaskService, PRIORITIES, STATUSES};
use crate::util::{head, today};
use crate::vault::{append_to_section, join_frontmatter, meta_str, split_frontmatter, Meta, Vault};

pub const DRAFTS_FOLDER: &str = ".brouillons";
pub const MAX_DRAFT_CHARS: usize = 20_000;

#[derive(Debug, Clone, Serialize)]
pub struct Draft {
    pub id: String,
    pub title: String,
    pub content: String,
    pub created: String,
    pub updated: String,
}

fn yes() -> bool {
    true
}

fn de_kind<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<String, D::Error> {
    let value = de::string(d)?;
    Ok(if NOTE_KINDS.contains(&value.as_str()) { value } else { "note".into() })
}

fn de_priority<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<String, D::Error> {
    let value = de::string(d)?;
    Ok(if PRIORITIES.contains(&value.as_str()) { value } else { "normale".into() })
}

fn de_status<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<String, D::Error> {
    let value = de::string(d)?;
    Ok(if STATUSES.contains(&value.as_str()) { value } else { "a_faire".into() })
}

fn note_kind() -> String {
    "note".into()
}

fn normale() -> String {
    "normale".into()
}

fn a_faire() -> String {
    "a_faire".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftNote {
    #[serde(default = "yes", deserialize_with = "de::bool_true")]
    pub accepted: bool,
    #[serde(default = "note_kind", rename = "type", deserialize_with = "de_kind")]
    pub kind: String,
    #[serde(default, deserialize_with = "de::string")]
    pub title: String,
    #[serde(default, deserialize_with = "de::string")]
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftTask {
    #[serde(default = "yes", deserialize_with = "de::bool_true")]
    pub accepted: bool,
    #[serde(default, deserialize_with = "de::string")]
    pub title: String,
    #[serde(default, deserialize_with = "de::string")]
    pub description: String,
    #[serde(default, deserialize_with = "de::opt_string")]
    pub project: Option<String>,
    #[serde(default = "normale", deserialize_with = "de_priority")]
    pub priority: String,
    #[serde(default = "a_faire", deserialize_with = "de_status")]
    pub status: String,
    #[serde(default, deserialize_with = "de::opt_date")]
    pub due: Option<String>,
    #[serde(default, deserialize_with = "de::opt_string")]
    pub duplicate_of: Option<String>,
}

/// Proposition de l'IA pour un brouillon : une note, des tâches, ou les deux.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftProposal {
    #[serde(default)]
    pub note: Option<DraftNote>,
    #[serde(default)]
    pub tasks: Vec<DraftTask>,
    #[serde(default, deserialize_with = "de::string")]
    pub comment: String,
}

pub struct DraftService {
    vault: Arc<Vault>,
    index: Arc<Index>,
    tasks: Arc<TaskService>,
    journal: Arc<JournalService>,
    llm: Arc<dyn Llm>,
    settings: Arc<Settings>,
}

fn now() -> String {
    Local::now().format("%Y-%m-%dT%H:%M:%S").to_string()
}

/// Identifiant de brouillon : chiffres, lettres et tirets uniquement (nom de fichier sûr).
fn check_id(id: &str) -> Result<()> {
    if !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        Ok(())
    } else {
        Err(vault_err(format!("Brouillon invalide : {id}")))
    }
}

fn rel_path(id: &str) -> String {
    format!("{DRAFTS_FOLDER}/{id}.md")
}

impl DraftService {
    pub fn new(
        vault: Arc<Vault>,
        index: Arc<Index>,
        tasks: Arc<TaskService>,
        journal: Arc<JournalService>,
        llm: Arc<dyn Llm>,
        settings: Arc<Settings>,
    ) -> Self {
        DraftService { vault, index, tasks, journal, llm, settings }
    }

    /// Brouillons dans l'ordre de création (ordre des onglets).
    pub fn list(&self) -> Result<Vec<Draft>> {
        let Ok(entries) = std::fs::read_dir(self.vault.root.join(DRAFTS_FOLDER)) else { return Ok(Vec::new()) };
        let mut ids: Vec<String> = entries
            .flatten()
            .filter(|e| e.path().is_file())
            .filter_map(|e| e.file_name().to_string_lossy().strip_suffix(".md").map(String::from))
            .filter(|id| check_id(id).is_ok())
            .collect();
        ids.sort();
        ids.iter().map(|id| self.get(id)).collect()
    }

    pub fn get(&self, id: &str) -> Result<Draft> {
        check_id(id)?;
        let rel = rel_path(id);
        if !self.vault.exists(&rel) {
            return Err(not_found(format!("Brouillon introuvable : {id}")));
        }
        let text = self.vault.read_text(&rel)?;
        let (meta, body) = split_frontmatter(&text);
        Ok(Draft {
            id: id.to_string(),
            title: meta_str(&meta, "title").unwrap_or_default(),
            content: body.strip_prefix('\n').unwrap_or(body).to_string(),
            created: meta_str(&meta, "created").unwrap_or_default(),
            updated: meta_str(&meta, "updated").unwrap_or_default(),
        })
    }

    pub fn create(&self, title: &str) -> Result<Draft> {
        let stamp = Local::now().format("%Y%m%d-%H%M%S").to_string();
        let mut id = stamp.clone();
        let mut n = 2;
        while self.vault.exists(&rel_path(&id)) {
            id = format!("{stamp}-{n}");
            n += 1;
        }
        let created = now();
        self.write(&id, title, "", &created)?;
        self.get(&id)
    }

    /// Enregistre le titre et le texte (sauvegarde automatique de l'onglet).
    pub fn save(&self, id: &str, title: Option<&str>, content: Option<&str>) -> Result<Draft> {
        let current = self.get(id)?;
        let title = title.unwrap_or(&current.title);
        let content = content.unwrap_or(&current.content);
        self.write(id, title, content, &current.created)?;
        self.get(id)
    }

    fn write(&self, id: &str, title: &str, content: &str, created: &str) -> Result<()> {
        let mut meta = Meta::new();
        meta.insert("title".into(), title.trim().into());
        meta.insert("created".into(), created.into());
        meta.insert("updated".into(), now().into());
        self.vault.write(&rel_path(id), &join_frontmatter(&meta, &format!("\n{content}")), None, false)?;
        Ok(())
    }

    /// Ferme l'onglet : le fichier part dans `.trash/` (récupérable).
    pub fn delete(&self, id: &str) -> Result<()> {
        check_id(id)?;
        self.vault.delete(&rel_path(id))
    }

    pub fn analyze(&self, id: &str, today_date: Option<NaiveDate>) -> Result<DraftProposal> {
        let draft = self.get(id)?;
        let text = draft.content.trim();
        if text.is_empty() {
            return Err(vault_err("Le brouillon est vide"));
        }
        let open_tasks = self.tasks.open_tasks()?;
        let projects: BTreeSet<String> = self
            .index
            .by_type("projet")?
            .iter()
            .map(|n| n.link_name().to_string())
            .chain(open_tasks.iter().filter_map(|t| t.project.clone()))
            .collect();
        let mut lines = vec![
            format!(
                "Projets connus : {}",
                if projects.is_empty() { "aucun".to_string() } else { projects.into_iter().collect::<Vec<_>>().join(", ") }
            ),
            "Tâches ouvertes :".to_string(),
        ];
        lines.extend(open_tasks.iter().map(|t| format!("- {} : {} ({})", t.id, t.title, t.project.as_deref().unwrap_or("sans projet"))));
        lines.push(String::new());
        if !draft.title.trim().is_empty() {
            lines.push(format!("Titre du brouillon : {}", draft.title.trim()));
        }
        lines.push("=== Brouillon à traiter ===".to_string());
        lines.push(head(text, MAX_DRAFT_CHARS));
        let system = DRAFT_SYSTEM
            .replace("{today}", &today_date.unwrap_or_else(today).to_string())
            .replace("{kinds}", &NOTE_KINDS.join(" | "));
        let mut proposal: DraftProposal =
            chat_json(self.llm.as_ref(), &[Msg::system(system), Msg::user(lines.join("\n"))], &self.settings.model_smart)?;
        proposal.note = proposal.note.filter(|n| !n.title.trim().is_empty() || !n.content.trim().is_empty());
        if let Some(note) = proposal.note.as_mut() {
            if note.title.trim().is_empty() {
                note.title = draft.title.trim().to_string();
            }
        }
        let known: Vec<String> = open_tasks.iter().map(|t| t.id.to_uppercase()).collect();
        proposal.tasks.retain(|t| !t.title.trim().is_empty());
        for task in &mut proposal.tasks {
            if task.duplicate_of.as_ref().is_some_and(|id| !known.contains(&id.to_uppercase())) {
                task.duplicate_of = None;
            }
            // une tâche déjà couverte par une tâche ouverte n'est pas cochée par défaut
            task.accepted = task.duplicate_of.is_none();
        }
        Ok(proposal)
    }

    /// Crée la note et les tâches validées ; les tâches citent la note créée, qui liste les
    /// tâches. `close` retire ensuite le brouillon (dans `.trash/`).
    pub fn apply(&self, id: &str, proposal: &DraftProposal, close: bool) -> Result<Value> {
        self.get(id)?;
        let note = proposal.note.as_ref().filter(|n| n.accepted);
        let tasks: Vec<&DraftTask> = proposal.tasks.iter().filter(|t| t.accepted && !t.title.trim().is_empty()).collect();
        if note.is_none() && tasks.is_empty() {
            return Err(vault_err("Rien à créer : cochez la note ou au moins une tâche"));
        }
        let note_path = match note {
            Some(n) => Some(self.journal.create_note_with_body(&n.kind, &n.title, None, Some(&n.content))?),
            None => None,
        };
        let link = note_path.as_deref().map(|p| p.rsplit('/').next().unwrap_or(p).trim_end_matches(".md").to_string());
        let mut created = Vec::new();
        for t in tasks {
            let new = NewTask {
                title: t.title.clone(),
                project: t.project.clone(),
                priority: Some(t.priority.clone()),
                status: Some(t.status.clone()),
                due: t.due.clone(),
                description: t.description.clone(),
                origin: link.clone(),
            };
            created.push(self.tasks.create(new, None)?.task.id);
        }
        if let (Some(path), false) = (&note_path, created.is_empty()) {
            let text = self.vault.read_text(path)?;
            let links: Vec<String> = created.iter().map(|id| format!("[[{id}]]")).collect();
            let text = links.iter().fold(text, |text, link| append_to_section(&text, "## Tâches", &format!("- {link}")));
            // le modèle de réunion prévoit une liste `tasks` dans le frontmatter
            let (mut meta, body) = split_frontmatter(&text);
            let text = match meta.get_mut("tasks") {
                Some(tasks) => {
                    *tasks = json!(links);
                    join_frontmatter(&meta, body)
                }
                None => text,
            };
            self.vault.write(path, &text, None, false)?;
            self.index.upsert(path)?;
        }
        if close {
            self.delete(id)?;
        }
        Ok(json!({"note": note_path, "created_tasks": created, "closed": close}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids() {
        assert!(check_id("20261019-101500").is_ok());
        assert!(check_id("20261019-101500-2").is_ok());
        assert!(check_id("../x").is_err());
        assert!(check_id("").is_err());
    }
}
