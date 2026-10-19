//! Tâches : un fichier par tâche dans taches/, avec une section Historique datée.

use std::sync::{Arc, LazyLock};

use regex::Regex;
use serde::Serialize;
use serde_json::{Map, Value};

use crate::error::{not_found, vault_err, Result};
use crate::index::{Index, NoteRow};
use crate::util::{parse_date, today};
use crate::vault::{
    append_to_section, join_frontmatter, meta_str, section_lines, slugify, split_frontmatter, value_str, Meta, Vault,
};

pub const STATUSES: [&str; 5] = ["a_faire", "en_cours", "attente", "bloquee", "terminee"];
pub const PRIORITIES: [&str; 3] = ["basse", "normale", "haute"];
pub const HISTORY_HEADING: &str = "## Historique";
const EDITABLE_FIELDS: [&str; 5] = ["title", "status", "priority", "project", "due"];
const FOLDER: &str = "taches";

static HISTORY_LINE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^-\s*(\d{4}-\d{2}-\d{2})\s*:\s*(.+)$").unwrap());
static TASK_ID_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^T-(\d+)").unwrap());

pub fn is_open(status: &str) -> bool {
    status != "terminee"
}

/// Refuse une valeur hors de la liste autorisée (`None` et `null` sont acceptés).
pub fn check_value(field: &str, value: Option<&Value>, allowed: &[&str]) -> Result<()> {
    match value {
        None | Some(Value::Null) => Ok(()),
        Some(Value::String(s)) if allowed.contains(&s.as_str()) => Ok(()),
        Some(other) => Err(vault_err(format!(
            "Valeur invalide pour {field} : {} (attendu : {})",
            value_str(other),
            allowed.join(", ")
        ))),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HistoryEvent {
    pub date: String,
    pub text: String,
}

pub fn parse_history(text: &str) -> Vec<HistoryEvent> {
    section_lines(text, HISTORY_HEADING)
        .iter()
        .filter_map(|line| HISTORY_LINE_RE.captures(line.trim()))
        .map(|c| HistoryEvent { date: c[1].to_string(), text: c[2].trim().to_string() })
        .collect()
}

/// Résumé d'une tâche (liste, tableau de bord).
#[derive(Debug, Clone, Serialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub status: String,
    pub priority: String,
    pub project: Option<String>,
    pub due: Option<String>,
    pub created: Option<String>,
    pub path: String,
}

impl Task {
    fn from_note(note: &NoteRow) -> Self {
        let meta = &note.meta;
        Task {
            id: note.link_name().to_string(),
            title: note.title.clone(),
            status: meta_str(meta, "status").unwrap_or_else(|| "a_faire".into()),
            priority: meta_str(meta, "priority").unwrap_or_else(|| "normale".into()),
            project: meta_str(meta, "project"),
            due: meta_str(meta, "due"),
            created: meta_str(meta, "created"),
            path: note.path.clone(),
        }
    }

    pub fn project_or_dash(&self) -> &str {
        self.project.as_deref().unwrap_or("-")
    }

    pub fn due_or_dash(&self) -> &str {
        self.due.as_deref().unwrap_or("-")
    }
}

/// Fiche complète : résumé, contenu du fichier et historique.
#[derive(Debug, Clone, Serialize)]
pub struct TaskDetail {
    #[serde(flatten)]
    pub task: Task,
    pub content: String,
    pub history: Vec<HistoryEvent>,
}

/// Événement d'historique d'une tâche, pour les rapports.
#[derive(Debug, Clone, Serialize)]
pub struct TaskEvent {
    pub date: String,
    pub text: String,
    pub task_id: String,
    pub title: String,
    pub status: String,
    pub project: Option<String>,
}

/// Paramètres de création d'une tâche.
#[derive(Debug, Clone, Default)]
pub struct NewTask {
    pub title: String,
    pub project: Option<String>,
    pub priority: Option<String>,
    pub status: Option<String>,
    pub due: Option<String>,
    pub description: String,
    /// note d'origine, tracée dans l'historique de la tâche
    pub origin: Option<String>,
}

pub struct TaskService {
    vault: Arc<Vault>,
    index: Arc<Index>,
}

impl TaskService {
    pub fn new(vault: Arc<Vault>, index: Arc<Index>) -> Self {
        TaskService { vault, index }
    }

    pub fn list(&self, status: Option<&str>, project: Option<&str>) -> Result<Vec<Task>> {
        let mut tasks: Vec<Task> = self.index.by_type("tache")?.iter().map(Task::from_note).collect();
        if let Some(status) = status.filter(|s| !s.is_empty()) {
            tasks.retain(|t| t.status == status);
        }
        if let Some(project) = project.filter(|p| !p.is_empty()) {
            tasks.retain(|t| t.project.as_deref() == Some(project));
        }
        tasks.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(tasks)
    }

    pub fn all(&self) -> Result<Vec<Task>> {
        self.list(None, None)
    }

    pub fn open_tasks(&self) -> Result<Vec<Task>> {
        Ok(self.all()?.into_iter().filter(|t| is_open(&t.status)).collect())
    }

    pub fn path_of(&self, task_id: &str) -> Result<String> {
        self.index.resolve_link(task_id)?.ok_or_else(|| not_found(format!("Tâche introuvable : {task_id}")))
    }

    pub fn get(&self, task_id: &str) -> Result<TaskDetail> {
        let path = self.path_of(task_id)?;
        let note = self.index.get(&path)?.ok_or_else(|| not_found(format!("Tâche introuvable : {task_id}")))?;
        let content = self.vault.read_text(&path)?;
        Ok(TaskDetail { task: Task::from_note(&note), history: parse_history(&content), content })
    }

    pub fn next_id(&self) -> Result<String> {
        let mut numbers = vec![0u64];
        let mut collect = |candidate: &str| {
            if let Some(c) = TASK_ID_RE.captures(candidate) {
                numbers.push(c[1].parse().unwrap_or(0));
            }
        };
        for note in self.index.by_type("tache")? {
            collect(&note.alias);
            collect(&note.stem);
        }
        if let Ok(entries) = std::fs::read_dir(self.vault.root.join(FOLDER)) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with("T-") && name.ends_with(".md") {
                    collect(&name[..name.len() - 3]);
                }
            }
        }
        Ok(format!("T-{:03}", numbers.into_iter().max().unwrap_or(0) + 1))
    }

    pub fn create(&self, new: NewTask, today_str: Option<&str>) -> Result<TaskDetail> {
        let title = new.title.trim();
        if title.is_empty() {
            return Err(vault_err("Le titre de la tâche est obligatoire"));
        }
        let status = new.status.unwrap_or_else(|| "a_faire".into());
        let priority = new.priority.unwrap_or_else(|| "normale".into());
        check_value("status", Some(&Value::String(status.clone())), &STATUSES)?;
        check_value("priority", Some(&Value::String(priority.clone())), &PRIORITIES)?;
        let today_str = today_str.map(String::from).unwrap_or_else(|| today().to_string());
        let task_id = self.next_id()?;
        let mut meta = Meta::new();
        meta.insert("id".into(), task_id.clone().into());
        meta.insert("type".into(), "tache".into());
        meta.insert("title".into(), title.into());
        meta.insert("status".into(), status.into());
        meta.insert("priority".into(), priority.into());
        if let Some(project) = new.project.filter(|p| !p.is_empty()) {
            meta.insert("project".into(), project.into());
        }
        if let Some(due) = new.due.filter(|d| !d.is_empty()) {
            meta.insert("due".into(), parse_date(&due)?.to_string().into());
        }
        meta.insert("created".into(), today_str.clone().into());
        let created = match new.origin.filter(|o| !o.is_empty()) {
            Some(origin) => format!("créée depuis [[{origin}]]"),
            None => "créée".into(),
        };
        let description = new.description.trim();
        let mut body = if description.is_empty() { "\n".to_string() } else { format!("\n{description}\n\n") };
        body.push_str(&format!("{HISTORY_HEADING}\n- {today_str} : {created}\n"));
        let rel = self.vault.unique_path(FOLDER, &format!("{task_id}-{}", slugify(title, 40)))?;
        self.vault.write(&rel, &join_frontmatter(&meta, &body), None, true)?;
        self.index.upsert(&rel)?;
        self.get(&task_id)
    }

    /// Modifie des champs de la tâche ; les changements de statut et la note éventuelle sont
    /// tracés dans l'historique.
    pub fn update(&self, task_id: &str, changes: &Map<String, Value>, note: Option<&str>, today_str: Option<&str>) -> Result<TaskDetail> {
        let changes: Vec<(&String, &Value)> = changes.iter().filter(|(k, _)| EDITABLE_FIELDS.contains(&k.as_str())).collect();
        let lookup = |key: &str| changes.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
        check_value("status", lookup("status"), &STATUSES)?;
        check_value("priority", lookup("priority"), &PRIORITIES)?;
        let today_str = today_str.map(String::from).unwrap_or_else(|| today().to_string());
        let path = self.path_of(task_id)?;
        let text = self.vault.read_text(&path)?;
        let (mut meta, body) = split_frontmatter(&text);
        let old_status = meta_str(&meta, "status").unwrap_or_else(|| "a_faire".into());
        for (key, value) in &changes {
            let empty = matches!(value, Value::Null) || matches!(value, Value::String(s) if s.is_empty());
            if empty {
                meta.shift_remove(key.as_str());
            } else if key.as_str() == "due" {
                meta.insert((*key).clone(), parse_date(&value_str(value))?.to_string().into());
            } else {
                meta.insert((*key).clone(), (*value).clone());
            }
        }
        let new_status = meta_str(&meta, "status").unwrap_or_else(|| "a_faire".into());
        let mut history = Vec::new();
        if lookup("status").is_some() && new_status != old_status {
            history.push(format!("statut {old_status} → {new_status}"));
        }
        if let Some(note) = note.map(str::trim).filter(|n| !n.is_empty()) {
            history.push(note.to_string());
        }
        let mut new_text = join_frontmatter(&meta, body);
        if !history.is_empty() {
            new_text = append_to_section(&new_text, HISTORY_HEADING, &format!("- {today_str} : {}", history.join(" — ")));
        }
        if new_text != text {
            self.vault.write(&path, &new_text, None, false)?;
            self.index.upsert(&path)?;
        }
        self.get(task_id)
    }

    /// Événements d'historique de toutes les tâches entre deux dates (incluses).
    pub fn events(&self, start: &str, end: &str) -> Result<Vec<TaskEvent>> {
        let mut result = Vec::new();
        for task in self.all()? {
            let text = self.vault.read_text(&task.path)?;
            for event in parse_history(&text) {
                if start <= event.date.as_str() && event.date.as_str() <= end {
                    result.push(TaskEvent {
                        date: event.date,
                        text: event.text,
                        task_id: task.id.clone(),
                        title: task.title.clone(),
                        status: task.status.clone(),
                        project: task.project.clone(),
                    });
                }
            }
        }
        result.sort_by(|a, b| (&a.date, &a.task_id).cmp(&(&b.date, &b.task_id)));
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_history_ignores_other_sections() {
        let text = "## Notes\n- 2026-01-01 : pas un événement\n\n## Historique\n- 2026-02-02 : créée\n";
        assert_eq!(parse_history(text), vec![HistoryEvent { date: "2026-02-02".into(), text: "créée".into() }]);
    }
}
