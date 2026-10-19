//! Tâches : un fichier par tâche dans taches/, avec une section Historique datée.
//!
//! Organisation façon backlog : `rank` ordonne les tâches dans le backlog (regroupées par
//! projet, qui tient lieu de feature) et `iterations` liste les semaines (`2026-S43`) où la
//! tâche a été planifiée — la dernière est la semaine en cours de planification, les
//! précédentes gardent l'historique des boards.

use std::sync::{Arc, LazyLock};

use chrono::{Duration, NaiveDate, Weekday};
use regex::Regex;
use serde::Serialize;
use serde_json::{Map, Value};

use crate::error::{not_found, vault_err, Result};
use crate::index::{Index, NoteRow};
use crate::util::{parse_date, today, week_bounds, week_label};
use crate::vault::{
    append_to_section, join_frontmatter, meta_str, section_lines, slugify, split_frontmatter, value_str, Meta, Vault,
};

pub const STATUSES: [&str; 5] = ["a_faire", "en_cours", "attente", "bloquee", "terminee"];
pub const PRIORITIES: [&str; 3] = ["basse", "normale", "haute"];
pub const HISTORY_HEADING: &str = "## Historique";
const EDITABLE_FIELDS: [&str; 6] = ["title", "status", "priority", "project", "due", "iteration"];
const FOLDER: &str = "taches";
/// écart entre deux rangs consécutifs lors d'une renumérotation
const RANK_STEP: i64 = 1000;

static HISTORY_LINE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^-\s*(\d{4}-\d{2}-\d{2})\s*:\s*(.+)$").unwrap());
static WEEK_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\d{4})-S(\d{2})$").unwrap());
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

/// Lundi d'une semaine ISO notée `2026-S43`.
pub fn week_start(label: &str) -> Result<NaiveDate> {
    WEEK_RE
        .captures(label)
        .and_then(|c| NaiveDate::from_isoywd_opt(c[1].parse().ok()?, c[2].parse().ok()?, Weekday::Mon))
        .ok_or_else(|| vault_err(format!("Semaine invalide : {label} (format AAAA-Snn)")))
}

/// Semaine décalée de `offset` semaines.
pub fn week_offset(label: &str, offset: i64) -> Result<String> {
    Ok(week_label(week_start(label)? + Duration::weeks(offset)))
}

/// Semaines planifiées dans le frontmatter (`iterations`, liste ou valeur seule).
fn meta_iterations(meta: &Meta) -> Vec<String> {
    match meta.get("iterations") {
        Some(Value::Array(items)) => items.iter().map(value_str).filter(|s| !s.is_empty()).collect(),
        Some(Value::String(s)) if !s.is_empty() => vec![s.clone()],
        _ => Vec::new(),
    }
}

fn set_iterations(meta: &mut Meta, iterations: Vec<String>) {
    if iterations.is_empty() {
        meta.shift_remove("iterations");
    } else {
        meta.insert("iterations".into(), Value::Array(iterations.into_iter().map(Value::String).collect()));
    }
}

/// Planifie la tâche sur `target` (ou la retire de sa planification avec `None`) : les
/// semaines passées sont conservées comme historique, les semaines courantes ou futures sont
/// remplacées. Renvoie la ligne d'historique, `None` si rien ne change.
fn plan(meta: &mut Meta, target: Option<&str>, current: &str) -> Result<Option<String>> {
    let before = meta_iterations(meta);
    let mut kept: Vec<String> = before.iter().filter(|w| w.as_str() < current).cloned().collect();
    if let Some(target) = target {
        week_start(target)?;
        kept.retain(|w| w.as_str() < target);
        kept.push(target.to_string());
    }
    if kept == before {
        return Ok(None);
    }
    let removed = before.last().filter(|w| !kept.contains(w)).cloned();
    set_iterations(meta, kept);
    Ok(match (target, removed) {
        (Some(target), _) => Some(format!("planifiée en {target}")),
        (None, Some(week)) => Some(format!("retirée de l'itération {week}")),
        (None, None) => None,
    })
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
    /// position dans le backlog (croissante), `None` si jamais ordonnée
    pub rank: Option<i64>,
    /// semaines planifiées, de la plus ancienne à la plus récente
    pub iterations: Vec<String>,
    /// dernière semaine planifiée
    pub iteration: Option<String>,
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
            rank: meta.get("rank").and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64))),
            iteration: meta_iterations(meta).last().cloned(),
            iterations: meta_iterations(meta),
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

/// Board d'une semaine : les tâches qui y ont été planifiées, avec leur statut actuel.
#[derive(Debug, Clone, Serialize)]
pub struct Board {
    pub week: String,
    pub current: String,
    pub start: String,
    pub end: String,
    pub previous: String,
    pub next: String,
    /// semaines ayant des tâches, plus la semaine courante et la semaine affichée, triées
    pub weeks: Vec<String>,
    pub tasks: Vec<Task>,
}

/// Semaine proposée dans le sélecteur d'itération du backlog.
#[derive(Debug, Clone, Serialize)]
pub struct WeekOption {
    pub label: String,
    pub start: String,
}

/// Backlog : toutes les tâches dans l'ordre du backlog, et les semaines planifiables.
#[derive(Debug, Clone, Serialize)]
pub struct Backlog {
    pub current: String,
    pub weeks: Vec<WeekOption>,
    pub tasks: Vec<Task>,
}

/// Ordre du backlog : rang croissant, tâches jamais ordonnées à la fin par identifiant.
fn backlog_order(a: &Task, b: &Task) -> std::cmp::Ordering {
    (a.rank.is_none(), a.rank, &a.id).cmp(&(b.rank.is_none(), b.rank, &b.id))
}

/// Rangs à écrire pour insérer `moved` à la position `at` d'un groupe ordonné (qui ne le
/// contient pas) : le rang médian entre les voisins s'il y a de la place, sinon une
/// renumérotation du groupe par pas de [`RANK_STEP`] (seuls les rangs modifiés sont renvoyés,
/// toujours celui de `moved`).
pub fn rank_changes(group: &[(String, Option<i64>)], moved: &str, at: usize) -> Vec<(String, i64)> {
    let prev = at.checked_sub(1).map(|i| group[i].1);
    let next = group.get(at).map(|(_, rank)| *rank);
    let free = match (prev, next) {
        (None, None) => Some(RANK_STEP),
        (Some(Some(p)), None) => Some(p + RANK_STEP),
        (None, Some(Some(n))) => Some(n - RANK_STEP),
        (Some(Some(p)), Some(Some(n))) if n - p > 1 => Some(p + (n - p) / 2),
        _ => None,
    };
    if let Some(rank) = free {
        return vec![(moved.to_string(), rank)];
    }
    let mut order: Vec<(&str, Option<i64>)> = group.iter().map(|(id, rank)| (id.as_str(), *rank)).collect();
    order.insert(at, (moved, None));
    order
        .into_iter()
        .enumerate()
        .map(|(i, (id, old))| (id, old, (i as i64 + 1) * RANK_STEP))
        .filter(|(id, old, rank)| *id == moved || *old != Some(*rank))
        .map(|(id, _, rank)| (id.to_string(), rank))
        .collect()
}

fn day_or_today(today_str: Option<&str>) -> Result<NaiveDate> {
    match today_str {
        Some(d) => parse_date(d),
        None => Ok(today()),
    }
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

    /// Modifie des champs de la tâche ; les changements de statut, de planification et la note
    /// éventuelle sont tracés dans l'historique. `iteration` (semaine `2026-S43`, vide pour
    /// retirer la tâche de sa planification) passe par [`plan`].
    pub fn update(&self, task_id: &str, changes: &Map<String, Value>, note: Option<&str>, today_str: Option<&str>) -> Result<TaskDetail> {
        let changes: Vec<(&String, &Value)> = changes.iter().filter(|(k, _)| EDITABLE_FIELDS.contains(&k.as_str())).collect();
        let lookup = |key: &str| changes.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
        check_value("status", lookup("status"), &STATUSES)?;
        check_value("priority", lookup("priority"), &PRIORITIES)?;
        let today_str = today_str.map(String::from).unwrap_or_else(|| today().to_string());
        let current = week_label(parse_date(&today_str)?);
        self.edit(task_id, &today_str, |meta| {
            let old_status = meta_str(meta, "status").unwrap_or_else(|| "a_faire".into());
            let mut history = Vec::new();
            for (key, value) in &changes {
                let empty = matches!(value, Value::Null) || matches!(value, Value::String(s) if s.is_empty());
                if key.as_str() == "iteration" {
                    let target = (!empty).then(|| value_str(value));
                    history.extend(plan(meta, target.as_deref(), &current)?);
                } else if empty {
                    meta.shift_remove(key.as_str());
                } else if key.as_str() == "due" {
                    meta.insert((*key).clone(), parse_date(&value_str(value))?.to_string().into());
                } else {
                    meta.insert((*key).clone(), (*value).clone());
                }
            }
            let new_status = meta_str(meta, "status").unwrap_or_else(|| "a_faire".into());
            if lookup("status").is_some() && new_status != old_status {
                history.insert(0, format!("statut {old_status} → {new_status}"));
            }
            if let Some(note) = note.map(str::trim).filter(|n| !n.is_empty()) {
                history.push(note.to_string());
            }
            Ok(history)
        })?;
        self.get(task_id)
    }

    /// Modifie le frontmatter d'une tâche ; `change` renvoie les lignes d'historique à tracer.
    fn edit(&self, task_id: &str, today_str: &str, change: impl FnOnce(&mut Meta) -> Result<Vec<String>>) -> Result<()> {
        let path = self.path_of(task_id)?;
        let text = self.vault.read_text(&path)?;
        let (mut meta, body) = split_frontmatter(&text);
        let history = change(&mut meta)?;
        let mut new_text = join_frontmatter(&meta, body);
        if !history.is_empty() {
            new_text = append_to_section(&new_text, HISTORY_HEADING, &format!("- {today_str} : {}", history.join(" — ")));
        }
        if new_text != text {
            self.vault.write(&path, &new_text, None, false)?;
            self.index.upsert(&path)?;
        }
        Ok(())
    }

    /// Backlog ordonné, avec la semaine courante et les huit semaines planifiables.
    pub fn backlog(&self, today_str: Option<&str>) -> Result<Backlog> {
        let day = day_or_today(today_str)?;
        let mut tasks = self.all()?;
        tasks.sort_by(backlog_order);
        let weeks = (0..8)
            .map(|i| {
                let start = week_bounds(day).0 + Duration::weeks(i);
                WeekOption { label: week_label(start), start: start.to_string() }
            })
            .collect();
        Ok(Backlog { current: week_label(day), weeks, tasks })
    }

    /// Board d'une semaine (la semaine courante par défaut), dans l'ordre du backlog.
    pub fn board(&self, week: Option<&str>, today_str: Option<&str>) -> Result<Board> {
        let current = week_label(day_or_today(today_str)?);
        let week = week.filter(|w| !w.is_empty()).unwrap_or(&current).to_string();
        let start = week_start(&week)?;
        let all = self.all()?;
        let mut weeks: Vec<String> = all.iter().flat_map(|t| t.iterations.iter().cloned()).collect();
        weeks.extend([current.clone(), week.clone()]);
        weeks.sort();
        weeks.dedup();
        let mut tasks: Vec<Task> = all.into_iter().filter(|t| t.iterations.contains(&week)).collect();
        tasks.sort_by(backlog_order);
        Ok(Board {
            previous: week_offset(&week, -1)?,
            next: week_offset(&week, 1)?,
            start: start.to_string(),
            end: (start + Duration::days(6)).to_string(),
            week,
            current,
            weeks,
            tasks,
        })
    }

    /// Reporte sur la semaine courante les tâches non terminées dont la dernière semaine
    /// planifiée est passée ; la semaine quittée reste dans `iterations`. Renvoie les
    /// identifiants reportés.
    pub fn roll_over(&self, today_str: Option<&str>) -> Result<Vec<String>> {
        let today_str = today_str.map(String::from).unwrap_or_else(|| today().to_string());
        let current = week_label(parse_date(&today_str)?);
        let mut moved = Vec::new();
        for task in self.open_tasks()? {
            let Some(from) = task.iteration.clone().filter(|w| *w < current) else { continue };
            self.edit(&task.id, &today_str, |meta| {
                let mut iterations = meta_iterations(meta);
                iterations.push(current.clone());
                set_iterations(meta, iterations);
                Ok(vec![format!("reportée de {from} en {current}")])
            })?;
            moved.push(task.id);
        }
        Ok(moved)
    }

    /// Déplace une tâche dans le backlog : elle rejoint le projet `project` (`None` : sans
    /// projet) et se place avant la tâche `before` (à la fin du projet sans `before`). Les rangs
    /// du projet sont renumérotés quand il n'y a plus de place entre les voisins.
    pub fn move_task(&self, task_id: &str, project: Option<&str>, before: Option<&str>) -> Result<Vec<Task>> {
        let project = project.filter(|p| !p.is_empty());
        let task = self.get(task_id)?.task;
        let mut group: Vec<Task> =
            self.all()?.into_iter().filter(|t| t.id != task.id && t.project.as_deref() == project).collect();        group.sort_by(backlog_order);
        let at = match before.filter(|b| !b.is_empty()) {
            Some(b) => group.iter().position(|t| t.id == b).ok_or_else(|| not_found(format!("Tâche introuvable : {b}")))?,
            None => group.len(),
        };
        let ranks: Vec<(String, Option<i64>)> = group.into_iter().map(|t| (t.id, t.rank)).collect();
        let today_str = today().to_string();
        for (id, rank) in rank_changes(&ranks, &task.id, at) {
            let moved = id == task.id;
            self.edit(&id, &today_str, |meta| {
                meta.insert("rank".into(), Value::from(rank));
                let old = meta_str(meta, "project");
                if !moved || old.as_deref() == project {
                    return Ok(Vec::new());
                }
                match project {
                    Some(p) => meta.insert("project".into(), p.into()),
                    None => meta.shift_remove("project"),
                };
                Ok(vec![format!("projet {} → {}", old.as_deref().unwrap_or("aucun"), project.unwrap_or("aucun"))])
            })?;
        }
        Ok(self.backlog(None)?.tasks)
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
    fn weeks() {
        assert_eq!(week_start("2026-S43").unwrap().to_string(), "2026-10-19");
        assert_eq!(week_offset("2026-S53", 1).unwrap(), "2027-S01");
        assert_eq!(week_offset("2027-S01", -1).unwrap(), "2026-S53");
        assert!(week_start("2026-43").is_err());
    }

    #[test]
    fn rank_changes_uses_gaps_then_renumbers() {
        let group = |ranks: &[Option<i64>]| -> Vec<(String, Option<i64>)> {
            ranks.iter().enumerate().map(|(i, r)| (format!("t{i}"), *r)).collect()
        };
        let ranked = group(&[Some(1000), Some(2000)]);
        assert_eq!(rank_changes(&ranked, "x", 0), [("x".to_string(), 0)]);
        assert_eq!(rank_changes(&ranked, "x", 1), [("x".to_string(), 1500)]);
        assert_eq!(rank_changes(&ranked, "x", 2), [("x".to_string(), 3000)]);
        assert_eq!(rank_changes(&[], "x", 0), [("x".to_string(), 1000)]);
        // plus de place, ou voisins sans rang : renumérotation, rangs inchangés omis
        let tight = group(&[Some(1000), Some(1001)]);
        assert_eq!(rank_changes(&tight, "x", 1), [("x".to_string(), 2000), ("t1".to_string(), 3000)]);
        let unranked = group(&[None, None]);
        assert_eq!(
            rank_changes(&unranked, "x", 1),
            [("t0".to_string(), 1000), ("x".to_string(), 2000), ("t1".to_string(), 3000)]
        );
    }

    #[test]
    fn plan_keeps_past_weeks() {
        let mut meta = Meta::new();
        set_iterations(&mut meta, vec!["2026-S41".into(), "2026-S43".into()]);
        assert_eq!(plan(&mut meta, Some("2026-S44"), "2026-S43").unwrap().as_deref(), Some("planifiée en 2026-S44"));
        assert_eq!(meta_iterations(&meta), ["2026-S41", "2026-S44"]);
        assert_eq!(plan(&mut meta, Some("2026-S44"), "2026-S43").unwrap(), None);
        assert_eq!(plan(&mut meta, None, "2026-S43").unwrap().as_deref(), Some("retirée de l'itération 2026-S44"));
        assert_eq!(meta_iterations(&meta), ["2026-S41"]);
    }

    #[test]
    fn parse_history_ignores_other_sections() {
        let text = "## Notes\n- 2026-01-01 : pas un événement\n\n## Historique\n- 2026-02-02 : créée\n";
        assert_eq!(parse_history(text), vec![HistoryEvent { date: "2026-02-02".into(), text: "créée".into() }]);
    }
}
