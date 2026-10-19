//! Actions IA dans l'éditeur : extraire des tâches d'une note, transformer une sélection.
//!
//! Rien n'est écrit ici : les tâches extraites sont des propositions que l'utilisateur coche
//! avant création, et le texte transformé est inséré par l'éditeur à sa demande.

use std::collections::BTreeSet;
use std::sync::Arc;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::Settings;
use crate::de;
use crate::error::{vault_err, Result};
use crate::index::Index;
use crate::llm::{chat_json, strip_fences, Llm, Msg};
use crate::prompts::{EXTRACT_TASKS_SYSTEM, TEXT_ACTIONS, TEXT_SYSTEM};
use crate::tasks::{TaskService, PRIORITIES};
use crate::util::{head, iso_date, today};
use crate::vault::{split_frontmatter, strip_section, Vault};

pub const MAX_TEXT_CHARS: usize = 20_000;

fn normale() -> String {
    "normale".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDraft {
    #[serde(default, deserialize_with = "de::string")]
    pub title: String,
    #[serde(default, deserialize_with = "de::string")]
    pub description: String,
    #[serde(default, deserialize_with = "de::opt_string")]
    pub project: Option<String>,
    #[serde(default = "normale", deserialize_with = "de::string")]
    pub priority: String,
    #[serde(default, deserialize_with = "de::opt_string")]
    pub due: Option<String>,
    #[serde(default, deserialize_with = "de::opt_string")]
    pub duplicate_of: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TaskDrafts {
    #[serde(default)]
    tasks: Vec<TaskDraft>,
}

pub struct AssistService {
    vault: Arc<Vault>,
    index: Arc<Index>,
    tasks: Arc<TaskService>,
    llm: Arc<dyn Llm>,
    settings: Arc<Settings>,
}

impl AssistService {
    pub fn new(vault: Arc<Vault>, index: Arc<Index>, tasks: Arc<TaskService>, llm: Arc<dyn Llm>, settings: Arc<Settings>) -> Self {
        AssistService { vault, index, tasks, llm, settings }
    }

    pub fn extract_tasks(&self, path: &str, today_date: Option<NaiveDate>) -> Result<Value> {
        let text = self.vault.read_text(path)?;
        let (_, body) = split_frontmatter(&text);
        let body = strip_section(body, "## Texte original").trim().to_string();
        if body.is_empty() {
            return Ok(json!({"tasks": []}));
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
        lines.push(format!("Note « {path} » :"));
        lines.push(head(&body, MAX_TEXT_CHARS));
        let system = EXTRACT_TASKS_SYSTEM.replace("{today}", &today_date.unwrap_or_else(today).to_string());
        let drafts: TaskDrafts =
            chat_json(self.llm.as_ref(), &[Msg::system(system), Msg::user(lines.join("\n"))], &self.settings.model_smart)?;
        let known: Vec<String> = open_tasks.iter().map(|t| t.id.to_uppercase()).collect();
        let result: Vec<TaskDraft> = drafts
            .tasks
            .into_iter()
            .filter(|d| !d.title.trim().is_empty())
            .map(|mut draft| {
                if !PRIORITIES.contains(&draft.priority.as_str()) {
                    draft.priority = "normale".into();
                }
                draft.due = draft.due.as_deref().and_then(iso_date).map(|d| d.to_string());
                if draft.duplicate_of.as_ref().is_some_and(|id| !known.contains(&id.to_uppercase())) {
                    draft.duplicate_of = None;
                }
                draft
            })
            .collect();
        Ok(json!({"tasks": result}))
    }

    pub fn transform(&self, action: &str, text: &str) -> Result<Value> {
        let Some((_, instruction)) = TEXT_ACTIONS.iter().find(|(name, _)| *name == action) else {
            return Err(vault_err(format!("Action inconnue : {action}")));
        };
        if text.trim().is_empty() {
            return Err(vault_err("Aucun texte à traiter"));
        }
        let content = self.llm.chat(
            &[Msg::system(TEXT_SYSTEM), Msg::user(format!("{instruction}\n\n{}", head(text, MAX_TEXT_CHARS)))],
            &self.settings.model_fast,
            false,
        )?;
        Ok(json!({"text": strip_fences(&content)}))
    }
}
