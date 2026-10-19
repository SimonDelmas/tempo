//! Inbox : résumés de mails / Teams → propositions du LLM → notes et tâches.

use std::sync::{Arc, LazyLock};

use indexmap::{IndexMap, IndexSet};
use regex::Regex;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Map, Value};

use crate::config::Settings;
use crate::de;
use crate::error::{vault_err, Result};
use crate::index::Index;
use crate::journal::JournalService;
use crate::llm::{chat_json, Llm, Msg};
use crate::prompts::INBOX_SYSTEM;
use crate::tasks::{NewTask, TaskService, PRIORITIES, STATUSES};
use crate::util::{head, today};
use crate::vault::{content_hash, join_frontmatter, slugify, Meta, Vault};

pub const INBOX_FOLDER: &str = "00-inbox";
static SEPARATOR_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^\s*-{3,}\s*$").unwrap());

fn today_str() -> String {
    today().to_string()
}

fn default_subject() -> String {
    "Échange".into()
}

fn default_source() -> String {
    "autre".into()
}

fn de_source<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<String, D::Error> {
    let source = de::string(d)?.to_lowercase();
    Ok(if source == "mail" || source == "teams" { source } else { "autre".into() })
}

fn de_date_or_today<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<String, D::Error> {
    Ok(de::opt_date(d)?.unwrap_or_else(today_str))
}

fn de_priority<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<String, D::Error> {
    let value = de::string(d)?;
    Ok(if PRIORITIES.contains(&value.as_str()) { value } else { "normale".into() })
}

fn de_status<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<String, D::Error> {
    let value = de::string(d)?;
    Ok(if STATUSES.contains(&value.as_str()) { value } else { "a_faire".into() })
}

fn de_opt_status<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Option<String>, D::Error> {
    Ok(de::opt_string(d)?.filter(|s| STATUSES.contains(&s.as_str())))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EchangeProposal {
    #[serde(default = "default_source", deserialize_with = "de_source")]
    pub source: String,
    #[serde(default = "today_str", deserialize_with = "de_date_or_today")]
    pub date: String,
    #[serde(default = "default_subject", deserialize_with = "de::string")]
    pub subject: String,
    #[serde(default, deserialize_with = "de::string_list")]
    pub participants: Vec<String>,
    #[serde(default, deserialize_with = "de::string_list")]
    pub projects: Vec<String>,
    #[serde(default, deserialize_with = "de::string")]
    pub thread: String,
    #[serde(default, deserialize_with = "de::string")]
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewTaskProposal {
    #[serde(default = "de::yes", deserialize_with = "de::bool_true")]
    pub accepted: bool,
    #[serde(deserialize_with = "de::string")]
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
}

fn normale() -> String {
    "normale".into()
}

fn a_faire() -> String {
    "a_faire".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskUpdateProposal {
    #[serde(default = "de::yes", deserialize_with = "de::bool_true")]
    pub accepted: bool,
    #[serde(deserialize_with = "de::string")]
    pub task_id: String,
    #[serde(default, deserialize_with = "de_opt_status")]
    pub status: Option<String>,
    #[serde(default, deserialize_with = "de::string")]
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxProposal {
    pub echange: EchangeProposal,
    #[serde(default)]
    pub new_tasks: Vec<NewTaskProposal>,
    #[serde(default)]
    pub task_updates: Vec<TaskUpdateProposal>,
    #[serde(default, deserialize_with = "de::string")]
    pub journal_line: String,
    #[serde(default, deserialize_with = "de::opt_string")]
    pub duplicate_of: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct InboxItem {
    pub path: String,
    pub content: String,
    pub preview: String,
}

/// Découpe un collage de plusieurs résumés séparés par des lignes `---`.
pub fn split_summaries(text: &str) -> Vec<String> {
    SEPARATOR_RE.split(text).map(str::trim).filter(|p| !p.is_empty()).map(String::from).collect()
}

pub struct InboxService {
    vault: Arc<Vault>,
    index: Arc<Index>,
    tasks: Arc<TaskService>,
    journal: Arc<JournalService>,
    llm: Arc<dyn Llm>,
    settings: Arc<Settings>,
}

impl InboxService {
    pub fn new(
        vault: Arc<Vault>,
        index: Arc<Index>,
        tasks: Arc<TaskService>,
        journal: Arc<JournalService>,
        llm: Arc<dyn Llm>,
        settings: Arc<Settings>,
    ) -> Self {
        InboxService { vault, index, tasks, journal, llm, settings }
    }

    pub fn list(&self) -> Result<Vec<InboxItem>> {
        let folder = self.vault.root.join(INBOX_FOLDER);
        let Ok(entries) = std::fs::read_dir(&folder) else { return Ok(Vec::new()) };
        let mut names: Vec<String> = entries
            .flatten()
            .filter(|e| e.path().is_file())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.to_lowercase().ends_with(".md"))
            .collect();
        names.sort();
        names
            .into_iter()
            .map(|name| {
                let path = format!("{INBOX_FOLDER}/{name}");
                let content = self.vault.read_text(&path)?;
                let preview = head(&content.split_whitespace().collect::<Vec<_>>().join(" "), 160);
                Ok(InboxItem { path, content, preview })
            })
            .collect()
    }

    pub fn add(&self, text: &str) -> Result<Vec<String>> {
        let parts = split_summaries(text);
        if parts.is_empty() {
            return Err(vault_err("Aucun résumé à importer"));
        }
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
        let mut paths = Vec::new();
        for (i, part) in parts.iter().enumerate() {
            let rel = self.vault.unique_path(INBOX_FOLDER, &format!("{stamp}-{}", i + 1))?;
            self.vault.write(&rel, &format!("{part}\n"), None, true)?;
            self.index.upsert(&rel)?;
            paths.push(rel);
        }
        Ok(paths)
    }

    pub fn discard(&self, rel: &str) -> Result<()> {
        check_inbox_path(rel)?;
        self.vault.delete(rel)?;
        self.index.remove(rel)
    }

    fn context(&self) -> Result<String> {
        let today = today();
        let task_lines: Vec<String> = self
            .tasks
            .open_tasks()?
            .iter()
            .map(|t| {
                format!(
                    "- {} | {} | projet: {} | statut: {} | échéance: {}",
                    t.id,
                    t.title,
                    t.project_or_dash(),
                    t.status,
                    t.due_or_dash()
                )
            })
            .collect();
        let mut projects: Vec<String> = self.index.by_type("projet")?.into_iter().map(|n| n.stem).collect();
        projects.extend(self.tasks.all()?.into_iter().filter_map(|t| t.project));
        let projects: IndexSet<String> = {
            projects.sort();
            projects.into_iter().collect()
        };
        let since = (today - chrono::Duration::days(60)).to_string();
        let mut threads: IndexMap<String, String> = IndexMap::new();
        for note in self.index.list_notes(Some("echange"), Some(&since), None)? {
            if let Some(thread) = crate::vault::meta_str(&note.meta, "thread") {
                let line = format!("- {thread} : {} (dernier échange {})", note.title, note.date);
                threads.insert(thread, line);
            }
        }
        let join_or = |items: Vec<String>, sep: &str| {
            if items.is_empty() { "(aucun)".to_string() } else { items.join(sep) }
        };
        Ok(format!(
            "Date du jour : {today}\n\nTâches ouvertes :\n{}\n\nProjets connus : {}\n\nFils d'échanges récents :\n{}",
            if task_lines.is_empty() { "(aucune)".to_string() } else { task_lines.join("\n") },
            join_or(projects.into_iter().collect(), ", "),
            join_or(threads.into_values().collect(), "\n"),
        ))
    }

    pub fn analyze(&self, rel: &str) -> Result<InboxProposal> {
        check_inbox_path(rel)?;
        let raw = self.vault.read_text(rel)?;
        let messages = [
            Msg::system(INBOX_SYSTEM),
            Msg::user(format!("{}\n\n=== Résumé à traiter ===\n{raw}", self.context()?)),
        ];
        let mut proposal: InboxProposal = chat_json(self.llm.as_ref(), &messages, &self.settings.model_smart)?;
        let known: Vec<String> = self.tasks.all()?.into_iter().map(|t| t.id.to_uppercase()).collect();
        proposal.task_updates.retain(|u| known.contains(&u.task_id.to_uppercase()));
        let duplicates = self.index.find_by_meta("source_hash", &content_hash(raw.trim()))?;
        proposal.duplicate_of = duplicates.into_iter().next().map(|n| n.path);
        Ok(proposal)
    }

    /// Écrit l'échange, crée et met à jour les tâches validées, complète le journal, puis
    /// retire le résumé de l'inbox.
    pub fn apply(&self, rel: &str, proposal: &InboxProposal) -> Result<Value> {
        check_inbox_path(rel)?;
        let raw = self.vault.read_text(rel)?.trim().to_string();
        let ech = &proposal.echange;
        let stem = format!("{}-{}-{}", ech.date, ech.source, slugify(&ech.subject, 40));
        let echange_path = self.vault.unique_path(&format!("echanges/{}/{}", &ech.date[..4], &ech.date[5..7]), &stem)?;
        let echange_link = echange_path.rsplit('/').next().unwrap().trim_end_matches(".md").to_string();

        let mut created = Vec::new();
        for t in proposal.new_tasks.iter().filter(|t| t.accepted && !t.title.trim().is_empty()) {
            let new = NewTask {
                title: t.title.clone(),
                project: t.project.clone(),
                priority: Some(t.priority.clone()),
                status: Some(t.status.clone()),
                due: t.due.clone(),
                description: t.description.clone(),
                origin: Some(echange_link.clone()),
            };
            created.push(self.tasks.create(new, Some(&ech.date))?.task.id);
        }
        let mut updated = Vec::new();
        for u in proposal.task_updates.iter().filter(|u| u.accepted) {
            let note = match u.note.trim() {
                "" => format!("[[{echange_link}]]"),
                text => format!("{text} ([[{echange_link}]])"),
            };
            let mut changes = Map::new();
            if let Some(status) = &u.status {
                changes.insert("status".into(), status.clone().into());
            }
            updated.push(self.tasks.update(&u.task_id, &changes, Some(&note), Some(&ech.date))?.task.id);
        }

        let task_ids: Vec<&String> = created.iter().chain(updated.iter()).collect();
        let thread = if ech.thread.is_empty() { slugify(&ech.subject, 40) } else { ech.thread.clone() };
        let meta: Meta = serde_json::from_value(json!({
            "type": "echange", "source": ech.source, "date": ech.date, "subject": ech.subject,
            "participants": ech.participants, "projects": ech.projects, "thread": thread,
            "tasks": task_ids.iter().map(|id| format!("[[{id}]]")).collect::<Vec<_>>(),
            "source_hash": content_hash(&raw),
        }))
        .unwrap();
        let body = format!("\n# {}\n\n{}\n\n## Texte original\n\n{raw}\n", ech.subject, ech.summary.trim());
        self.vault.write(&echange_path, &join_frontmatter(&meta, &body), None, true)?;
        self.index.upsert(&echange_path)?;

        let mut journal_path = None;
        let journal_line = proposal.journal_line.trim();
        if !journal_line.is_empty() {
            let links = task_ids.iter().map(|id| format!("[[{id}]]")).collect::<Vec<_>>().join(" ");
            let mut line = format!("- [[{echange_link}]] : {journal_line}");
            if !links.is_empty() {
                line.push_str(&format!(" → {links}"));
            }
            journal_path = Some(self.journal.append(&ech.date, "## Échanges", &line)?);
        }

        self.vault.delete(rel)?;
        self.index.remove(rel)?;
        Ok(json!({"echange": echange_path, "created_tasks": created, "updated_tasks": updated, "journal": journal_path}))
    }
}

fn check_inbox_path(rel: &str) -> Result<()> {
    if rel.starts_with(&format!("{INBOX_FOLDER}/")) {
        Ok(())
    } else {
        Err(vault_err("Ce fichier n'est pas dans l'inbox"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split() {
        assert_eq!(split_summaries("a\n---\n\nb\n-----\n"), vec!["a", "b"]);
    }
}
