//! Rapports : hebdomadaire (manager) et avancement d'une tâche.

use std::collections::HashSet;
use std::sync::Arc;

use chrono::{Duration, Local, NaiveDate, TimeZone};
use serde_json::{json, Value};

use crate::config::Settings;
use crate::error::Result;
use crate::index::{Index, NoteRow};
use crate::llm::{strip_fences, Llm, Msg};
use crate::prompts::{TASK_REPORT_SYSTEM, WEEKLY_SYSTEM};
use crate::tasks::{is_open, TaskService};
use crate::util::{clip, today, week_bounds, week_label};
use crate::vault::{join_frontmatter, split_frontmatter, strip_section, Meta, Vault};

pub const MAX_NOTE_CHARS: usize = 6000;
pub const MAX_CONTEXT_CHARS: usize = 150_000;
const OTHER_TYPES: [&str; 6] = ["sujet", "projet", "note", "personne", "ressource", "formation"];

fn now_minutes() -> String {
    Local::now().format("%Y-%m-%dT%H:%M").to_string()
}

fn local_timestamp(day: NaiveDate, end_of_day: bool) -> f64 {
    let time = if end_of_day { day.and_hms_micro_opt(23, 59, 59, 999_999) } else { day.and_hms_opt(0, 0, 0) };
    Local.from_local_datetime(&time.unwrap()).earliest().map(|t| t.timestamp_micros() as f64 / 1e6).unwrap_or(0.0)
}

pub struct ReportService {
    vault: Arc<Vault>,
    index: Arc<Index>,
    tasks: Arc<TaskService>,
    llm: Arc<dyn Llm>,
    settings: Arc<Settings>,
}

impl ReportService {
    pub fn new(vault: Arc<Vault>, index: Arc<Index>, tasks: Arc<TaskService>, llm: Arc<dyn Llm>, settings: Arc<Settings>) -> Self {
        ReportService { vault, index, tasks, llm, settings }
    }

    fn note_block(&self, note: &NoteRow) -> Result<String> {
        let text = self.vault.read_text(&note.path)?;
        let (_, body) = split_frontmatter(&text);
        let body = strip_section(body, "## Texte original");
        let date = if note.date.is_empty() { "sans date" } else { &note.date };
        Ok(format!("### [[{}]] — {} ({date})\n{}", note.link_name(), note.title, clip(body.trim(), MAX_NOTE_CHARS)))
    }

    fn blocks(&self, sections: &mut Vec<String>, title: &str, notes: &[NoteRow]) -> Result<()> {
        if !notes.is_empty() {
            let blocks = notes.iter().map(|n| self.note_block(n)).collect::<Result<Vec<_>>>()?;
            sections.push(format!("## {title}\n\n{}", blocks.join("\n\n")));
        }
        Ok(())
    }

    /// Données de la semaine, choisies par requêtes (dates, statuts) : le LLM ne fait que rédiger.
    pub fn weekly_context(&self, start: NaiveDate, end: NaiveDate, extra: &str) -> Result<String> {
        let (s, e) = (start.to_string(), end.to_string());
        let mut sections = vec![format!("Semaine du {s} au {e}.")];
        let list = |kind: &str| self.index.list_notes(Some(kind), Some(&s), Some(&e));
        self.blocks(&mut sections, "Réunions", &list("reunion")?)?;
        self.blocks(&mut sections, "Échanges (mails / Teams)", &list("echange")?)?;
        self.blocks(&mut sections, "Journal", &list("journal")?)?;
        let others: Vec<NoteRow> = self
            .index
            .modified_between(local_timestamp(start, false), local_timestamp(end, true))?
            .into_iter()
            .filter(|n| OTHER_TYPES.contains(&n.kind.as_str()))
            .collect();
        self.blocks(&mut sections, "Autres notes modifiées cette semaine", &others)?;

        let events = self.tasks.events(&s, &e)?;
        if !events.is_empty() {
            let lines: Vec<String> =
                events.iter().map(|ev| format!("- {} [[{}]] {} : {}", ev.date, ev.task_id, ev.title, ev.text)).collect();
            sections.push(format!("## Historique des tâches sur la semaine\n{}", lines.join("\n")));
        }

        let all_tasks = self.tasks.all()?;
        let open_tasks: Vec<_> = all_tasks.iter().filter(|t| is_open(&t.status)).collect();
        if !open_tasks.is_empty() {
            let lines: Vec<String> = open_tasks
                .iter()
                .map(|t| {
                    format!(
                        "- [[{}]] {} | projet: {} | statut: {} | priorité: {} | échéance: {}",
                        t.id,
                        t.title,
                        t.project_or_dash(),
                        t.status,
                        t.priority,
                        t.due_or_dash()
                    )
                })
                .collect();
            sections.push(format!("## Tâches ouvertes (état actuel)\n{}", lines.join("\n")));
        }
        let done_ids: HashSet<&str> = events
            .iter()
            .filter(|ev| ev.text.starts_with("statut") && ev.text.contains("→ terminee"))
            .map(|ev| ev.task_id.as_str())
            .collect();
        let done: Vec<String> = all_tasks
            .iter()
            .filter(|t| done_ids.contains(t.id.as_str()))
            .map(|t| format!("- [[{}]] {}", t.id, t.title))
            .collect();
        if !done.is_empty() {
            sections.push(format!("## Tâches terminées cette semaine\n{}", done.join("\n")));
        }
        let next_end = (end + Duration::days(7)).to_string();
        let upcoming: Vec<String> = open_tasks
            .iter()
            .filter(|t| t.due.as_deref().is_some_and(|d| d <= next_end.as_str()))
            .map(|t| format!("- [[{}]] {} : {}", t.id, t.title, t.due_or_dash()))
            .collect();
        if !upcoming.is_empty() {
            sections.push(format!("## Échéances jusqu'à la fin de la semaine prochaine\n{}", upcoming.join("\n")));
        }
        if !extra.trim().is_empty() {
            sections.push(format!("## Éléments ajoutés par l'utilisateur (à intégrer)\n{}", extra.trim()));
        }
        Ok(clip(sections.join("\n\n").trim(), MAX_CONTEXT_CHARS))
    }

    fn write_report(&self, folder: &str, stem: &str, meta: Value, body: &str) -> Result<Value> {
        let meta: Meta = serde_json::from_value(meta).unwrap();
        let rel = self.vault.unique_path(folder, stem)?;
        self.vault.write(&rel, &join_frontmatter(&meta, body), None, true)?;
        self.index.upsert(&rel)?;
        Ok(json!({"path": rel}))
    }

    pub fn weekly(&self, day: Option<NaiveDate>, extra: &str) -> Result<Value> {
        let (start, end) = week_bounds(day.unwrap_or_else(today));
        let context = self.weekly_context(start, end, extra)?;
        let content = strip_fences(&self.llm.chat(&[Msg::system(WEEKLY_SYSTEM), Msg::user(context)], &self.settings.model_smart, false)?);
        let label = week_label(start);
        let meta = json!({"type": "rapport", "kind": "hebdo", "week": label, "period_start": start.to_string(),
                          "period_end": end.to_string(), "generated": now_minutes()});
        let body = format!(
            "\n# Rapport hebdomadaire {label} ({} – {})\n\n{content}\n",
            start.format("%d/%m"),
            end.format("%d/%m")
        );
        self.write_report("rapports/hebdo", &label, meta, &body)
    }

    pub fn task_context(&self, task_id: &str) -> Result<String> {
        let task = self.tasks.get(task_id)?;
        let mut parts = vec![format!("# Fiche de la tâche [[{}]]\n{}", task.task.id, task.content)];
        let mut notes: Vec<NoteRow> =
            self.index.backlinks(&task.task.path)?.into_iter().filter(|n| n.kind != "rapport").collect();
        notes.sort_by(|a, b| a.date.cmp(&b.date));
        if !notes.is_empty() {
            let blocks = notes.iter().map(|n| self.note_block(n)).collect::<Result<Vec<_>>>()?;
            parts.push(format!("# Notes qui mentionnent la tâche\n\n{}", blocks.join("\n\n")));
        }
        Ok(clip(parts.join("\n\n").trim(), MAX_CONTEXT_CHARS))
    }

    pub fn task_report(&self, task_id: &str) -> Result<Value> {
        let task = self.tasks.get(task_id)?.task;
        let context = self.task_context(task_id)?;
        let content =
            strip_fences(&self.llm.chat(&[Msg::system(TASK_REPORT_SYSTEM), Msg::user(context)], &self.settings.model_smart, false)?);
        let today = today();
        let meta = json!({"type": "rapport", "kind": "tache", "date": today.to_string(),
                          "tasks": [format!("[[{}]]", task.id)], "generated": now_minutes()});
        let body = format!("\n# Avancement {} — {} ({})\n\n{content}\n", task.id, task.title, today.format("%d/%m/%Y"));
        self.write_report("rapports/taches", &format!("{}-{today}", task.id), meta, &body)
    }
}
