//! Comptes d'imputation et saisie des heures (8 h = 1 jour) par semaine.
//!
//! - un compte = un fichier comptes/<alias>.md (nom, alias, catégorie, jours attribués,
//!   description) ; la catégorie (`projet`, `interne` ou `conges`) classe les comptes ;
//! - un projet est rattaché à un compte par `compte: <alias>` dans son frontmatter ;
//! - les heures d'une semaine sont dans imputations/AAAA/AAAA-Sxx.md (frontmatter `hours`,
//!   tableau régénéré dans le corps) ;
//! - l'IA peut proposer la répartition d'une semaine à partir du rapport hebdomadaire ;
//! - les dates de dernière saisie (dans l'outil de gestion des temps) sont dans
//!   imputations/saisies.md ; elles découpent le temps en périodes, chacune avec un bilan par
//!   compte et la liste des semaines incomplètes.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, LazyLock};

use chrono::{Datelike, Duration, NaiveDate};
use indexmap::IndexMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::Settings;
use crate::de;
use crate::error::{not_found, vault_err, Result};
use crate::index::{Index, NoteRow};
use crate::llm::{chat_json, Llm, Msg};
use crate::prompts::IMPUTATION_SYSTEM;
use crate::reports::ReportService;
use crate::tasks::rank_changes;
use crate::util::{iso_date, parse_date, week_bounds, week_label};
use crate::vault::{join_frontmatter, meta_f64, meta_str, number, slugify, split_frontmatter, value_str, Meta, Vault};

pub const HOURS_PER_DAY: f64 = 8.0;
/// granularité des heures saisies
const STEP: f64 = 0.5;
const DAY_NAMES: [&str; 7] = ["Lun", "Mar", "Mer", "Jeu", "Ven", "Sam", "Dim"];
/// Catégories de compte, dans l'ordre d'affichage ; `projet` si absente ou inconnue.
pub const CATEGORIES: [&str; 3] = ["projet", "interne", "conges"];
static ALIAS_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9][A-Za-z0-9_.-]{0,39}$").unwrap());

/// Heures par jour puis par compte : {date: {alias: heures}}.
pub type Hours = IndexMap<String, IndexMap<String, f64>>;

pub fn workdays(start: NaiveDate) -> Vec<String> {
    (0..5).map(|i| (start + Duration::days(i)).to_string()).collect()
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round_ties_even() / 100.0
}

fn round3(value: f64) -> f64 {
    (value * 1000.0).round_ties_even() / 1000.0
}

/// Arrondi au pas de saisie (demi-heure).
fn round_step(hours: f64) -> f64 {
    round2((hours / STEP).round_ties_even() * STEP)
}

/// Catégorie normalisée (minuscules, sans accents) ; erreur si elle n'est pas dans `CATEGORIES`.
fn check_category(value: &str) -> Result<&'static str> {
    let key = value.trim().to_lowercase().replace('è', "e");
    CATEGORIES
        .into_iter()
        .find(|c| *c == key)
        .ok_or_else(|| vault_err(format!("Catégorie invalide : {value} (projet, interne ou conges)")))
}

/// Rang d'affichage d'une catégorie (inconnue → `projet`).
fn category_rank(category: &str) -> usize {
    CATEGORIES.iter().position(|c| *c == category).unwrap_or(0)
}

fn check_number(value: f64, field: &str) -> Result<f64> {
    if !value.is_finite() {
        return Err(vault_err(format!("Valeur invalide pour {field} : {value}")));
    }
    if value < 0.0 {
        return Err(vault_err(format!("{field} ne peut pas être négatif")));
    }
    Ok(value)
}

/// Nombre affiché sans zéros inutiles (`8`, `1.75`), comme le format `g` de Python.
pub fn fmt(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        return format!("{}", value as i64);
    }
    let magnitude = value.abs().log10().floor() as i32;
    let decimals = (5 - magnitude).max(0) as usize;
    let text = format!("{value:.decimals$}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Frontmatter `hours` → {date: {alias: heures}} sans les valeurs nulles.
fn clean_hours(raw: Option<&Value>) -> Hours {
    let mut result = Hours::new();
    let Some(Value::Object(days)) = raw else { return result };
    for (day, entries) in days {
        let Value::Object(entries) = entries else { continue };
        let mut kept = IndexMap::new();
        for (alias, hours) in entries {
            let value = match hours {
                Value::Number(n) => n.as_f64(),
                Value::String(s) => s.trim().parse().ok(),
                _ => None,
            };
            if let Some(value) = value.filter(|v| *v > 0.0) {
                kept.insert(alias.clone(), value);
            }
        }
        if !kept.is_empty() {
            result.insert(day.chars().take(10).collect(), kept);
        }
    }
    result
}

/// Arrondit au pas de saisie et ramène le total de la journée à 8 h.
pub fn balance_day(entries: &IndexMap<String, f64>) -> IndexMap<String, f64> {
    let entries: IndexMap<&String, f64> = entries.iter().filter(|(_, h)| **h > 0.0).map(|(a, h)| (a, *h)).collect();
    let total: f64 = entries.values().sum();
    if total == 0.0 {
        return IndexMap::new();
    }
    let mut scaled: IndexMap<String, f64> =
        entries.iter().map(|(a, h)| ((*a).clone(), round_step(h * HOURS_PER_DAY / total))).collect();
    let gap = round2(HOURS_PER_DAY - scaled.values().sum::<f64>());
    if gap != 0.0 {
        // première valeur maximale, comme max() en Python
        let mut biggest = 0;
        for (i, value) in scaled.values().enumerate() {
            if *value > scaled[biggest] {
                biggest = i;
            }
        }
        scaled[biggest] = round2(scaled[biggest] + gap);
    }
    scaled.retain(|_, h| *h > 0.0);
    scaled
}

fn hours_to_value(hours: &Hours) -> Value {
    Value::Object(
        hours
            .iter()
            .map(|(day, entries)| {
                (day.clone(), Value::Object(entries.iter().map(|(a, h)| (a.clone(), number(*h))).collect()))
            })
            .collect(),
    )
}

#[derive(Debug, Clone, Serialize)]
pub struct Account {
    pub alias: String,
    pub title: String,
    /// `projet`, `interne` ou `conges`
    pub categorie: String,
    pub description: String,
    pub days: f64,
    pub consumed_days: f64,
    pub remaining_days: f64,
    pub projects: Vec<String>,
    pub path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Project {
    pub id: String,
    pub title: String,
    pub path: String,
    pub compte: Option<String>,
    /// position de la feature dans le backlog, `None` si jamais ordonnée
    pub rank: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Week {
    pub week: String,
    pub start: String,
    pub end: String,
    pub days: Vec<String>,
    pub hours: Hours,
    pub totals: IndexMap<String, f64>,
    pub comment: String,
    pub path: Option<String>,
}

/// Heures d'un compte sur une période.
#[derive(Debug, Clone, Serialize)]
pub struct PeriodAccount {
    pub alias: String,
    pub title: String,
    pub hours: f64,
    pub days: f64,
    /// part des heures de la période (0 à 1)
    pub share: f64,
}

/// Semaine contenant des jours ouvrés dont le total n'est pas 8 h.
#[derive(Debug, Clone, Serialize)]
pub struct WeekGap {
    pub week: String,
    pub start: String,
    pub days: Vec<String>,
    /// heures à ajouter pour atteindre 8 h par jour
    pub missing_hours: f64,
    /// heures au-delà de 8 h par jour
    pub extra_hours: f64,
}

/// Période entre deux saisies ; `open` : du lendemain de la dernière saisie à aujourd'hui.
#[derive(Debug, Clone, Serialize)]
pub struct Period {
    pub start: String,
    pub end: String,
    pub open: bool,
    pub workdays: usize,
    pub complete_days: usize,
    pub complete: bool,
    pub total_hours: f64,
    pub expected_hours: f64,
    pub accounts: Vec<PeriodAccount>,
    pub incomplete_weeks: Vec<WeekGap>,
}

/// Dates de saisie et périodes qu'elles définissent (les plus récentes en premier).
#[derive(Debug, Clone, Serialize)]
pub struct Tracking {
    pub saisies: Vec<String>,
    pub last_saisie: Option<String>,
    pub periods: Vec<Period>,
    pub path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProposalEntry {
    #[serde(default, deserialize_with = "de::string")]
    compte: String,
    #[serde(default, deserialize_with = "de::number")]
    hours: f64,
}

#[derive(Debug, Deserialize)]
struct ProposalDay {
    #[serde(default, deserialize_with = "de::string")]
    date: String,
    #[serde(default)]
    entries: Vec<ProposalEntry>,
}

#[derive(Debug, Deserialize)]
struct ImputationProposal {
    #[serde(default)]
    days: Vec<ProposalDay>,
    #[serde(default, deserialize_with = "de::string")]
    comment: String,
}

pub struct ImputationService {
    vault: Arc<Vault>,
    index: Arc<Index>,
    reports: Arc<ReportService>,
    llm: Arc<dyn Llm>,
    settings: Arc<Settings>,
}

const ACCOUNTS_FOLDER: &str = "comptes";
const PROJECTS_FOLDER: &str = "projets";
const FOLDER: &str = "imputations";
const SAISIES_PATH: &str = "imputations/saisies.md";

impl ImputationService {
    pub fn new(vault: Arc<Vault>, index: Arc<Index>, reports: Arc<ReportService>, llm: Arc<dyn Llm>, settings: Arc<Settings>) -> Self {
        ImputationService { vault, index, reports, llm, settings }
    }

    // --- comptes ------------------------------------------------------------

    fn account_notes(&self) -> Result<Vec<NoteRow>> {
        Ok(self.index.by_type("compte")?.into_iter().filter(|n| !n.alias.is_empty()).collect())
    }

    fn account_note(&self, alias: &str) -> Result<NoteRow> {
        let key = alias.trim().to_lowercase();
        self.account_notes()?
            .into_iter()
            .find(|n| n.alias.to_lowercase() == key)
            .ok_or_else(|| not_found(format!("Compte d'imputation introuvable : {alias}")))
    }

    /// Heures saisies par compte (alias en minuscules), toutes semaines confondues.
    pub fn consumed_hours(&self, exclude_week: Option<&str>) -> Result<HashMap<String, f64>> {
        let mut totals = HashMap::new();
        for note in self.index.by_type("imputation")? {
            if exclude_week.is_some() && meta_str(&note.meta, "week").as_deref() == exclude_week {
                continue;
            }
            for entries in clean_hours(note.meta.get("hours")).values() {
                for (alias, hours) in entries {
                    *totals.entry(alias.to_lowercase()).or_insert(0.0) += hours;
                }
            }
        }
        Ok(totals)
    }

    pub fn accounts(&self) -> Result<Vec<Account>> {
        let consumed = self.consumed_hours(None)?;
        let projects = self.projects()?;
        let mut result: Vec<Account> = self
            .account_notes()?
            .into_iter()
            .map(|note| {
                let alias = note.alias.clone();
                let days = meta_f64(&note.meta, "days");
                let used = consumed.get(&alias.to_lowercase()).copied().unwrap_or(0.0) / HOURS_PER_DAY;
                Account {
                    projects: projects
                        .iter()
                        .filter(|p| p.compte.as_deref().unwrap_or("").to_lowercase() == alias.to_lowercase())
                        .map(|p| p.id.clone())
                        .collect(),
                    title: note.title,
                    categorie: meta_str(&note.meta, "categorie")
                        .and_then(|c| check_category(&c).ok())
                        .unwrap_or(CATEGORIES[0])
                        .to_string(),
                    description: meta_str(&note.meta, "description").unwrap_or_default(),
                    days,
                    consumed_days: round3(used),
                    remaining_days: round3(days - used),
                    path: note.path,
                    alias,
                }
            })
            .collect();
        result.sort_by_key(|a| (category_rank(&a.categorie), a.alias.to_lowercase()));
        Ok(result)
    }

    fn account(&self, alias: &str) -> Result<Account> {
        self.accounts()?
            .into_iter()
            .find(|a| a.alias == alias)
            .ok_or_else(|| not_found(format!("Compte d'imputation introuvable : {alias}")))
    }

    /// Catégorie vide → `projet`.
    pub fn create_account(&self, title: &str, alias: &str, categorie: &str, days: f64, description: &str) -> Result<Account> {
        let (title, alias) = (title.trim(), alias.trim());
        if title.is_empty() {
            return Err(vault_err("Le nom du compte est obligatoire"));
        }
        if !ALIAS_RE.is_match(alias) {
            return Err(vault_err("Alias invalide : lettres, chiffres, « - », « _ » ou « . », sans espace"));
        }
        if self.index.resolve_link(alias)?.is_some() {
            return Err(vault_err(format!("L'alias « {alias} » est déjà utilisé")));
        }
        let categorie = if categorie.trim().is_empty() { CATEGORIES[0] } else { check_category(categorie)? };
        let mut meta: Meta = serde_json::from_value(json!({
            "type": "compte",
            "title": title,
            "alias": alias,
            "categorie": categorie,
            "days": number(check_number(days, "days")?),
        }))
        .unwrap();
        if !description.trim().is_empty() {
            meta.insert("description".into(), description.trim().into());
        }
        let body = format!("\n# {title}\n\nCompte d'imputation `{alias}`.\n");
        let rel = self.vault.unique_path(ACCOUNTS_FOLDER, &slugify(alias, 50))?;
        self.vault.write(&rel, &join_frontmatter(&meta, &body), None, true)?;
        self.index.upsert(&rel)?;
        self.account(alias)
    }

    /// Champs à `None` inchangés ; une description vide est retirée.
    pub fn update_account(
        &self,
        alias: &str,
        title: Option<&str>,
        categorie: Option<&str>,
        days: Option<f64>,
        description: Option<&str>,
    ) -> Result<Account> {
        let note = self.account_note(alias)?;
        let text = self.vault.read_text(&note.path)?;
        let (mut meta, body) = split_frontmatter(&text);
        if let Some(title) = title {
            if title.trim().is_empty() {
                return Err(vault_err("Le nom du compte est obligatoire"));
            }
            meta.insert("title".into(), title.trim().into());
        }
        if let Some(categorie) = categorie {
            meta.insert("categorie".into(), check_category(categorie)?.into());
        }
        if let Some(days) = days {
            meta.insert("days".into(), number(check_number(days, "days")?));
        }
        match description.map(str::trim) {
            Some("") => {
                meta.shift_remove("description");
            }
            Some(description) => {
                meta.insert("description".into(), description.into());
            }
            None => {}
        }
        self.vault.write(&note.path, &join_frontmatter(&meta, body), None, false)?;
        self.index.upsert(&note.path)?;
        self.account(&note.alias)
    }

    // --- projets ------------------------------------------------------------

    pub fn projects(&self) -> Result<Vec<Project>> {
        let mut projects: Vec<Project> = self
            .index
            .by_type("projet")?
            .into_iter()
            .map(|n| Project {
                compte: meta_str(&n.meta, "compte"),
                rank: n.meta.get("rank").and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64))),
                id: n.stem,
                title: n.title,
                path: n.path,
            })
            .collect();
        projects.sort_by_key(|p| p.title.to_lowercase());
        Ok(projects)
    }

    /// Projets dans l'ordre du backlog : rang croissant, projets jamais ordonnés à la fin par
    /// titre.
    pub fn projects_by_rank(&self) -> Result<Vec<Project>> {
        let mut projects = self.projects()?;
        projects.sort_by_key(|p| (p.rank.is_none(), p.rank));
        Ok(projects)
    }

    /// Place le projet avant le projet `before` dans le backlog (à la fin sans `before`).
    pub fn move_project(&self, project_id: &str, before: Option<&str>) -> Result<Vec<Project>> {
        self.project(project_id)?;
        let group: Vec<Project> = self.projects_by_rank()?.into_iter().filter(|p| p.id != project_id).collect();
        let at = match before.filter(|b| !b.is_empty()) {
            Some(b) => group.iter().position(|p| p.id == b).ok_or_else(|| not_found(format!("Projet introuvable : {b}")))?,
            None => group.len(),
        };
        let ranks: Vec<(String, Option<i64>)> = group.iter().map(|p| (p.id.clone(), p.rank)).collect();
        for (id, rank) in rank_changes(&ranks, project_id, at) {
            let path = self.project(&id)?.path;
            let text = self.vault.read_text(&path)?;
            let (mut meta, body) = split_frontmatter(&text);
            meta.insert("rank".into(), Value::from(rank));
            self.vault.write(&path, &join_frontmatter(&meta, body), None, false)?;
            self.index.upsert(&path)?;
        }
        self.projects_by_rank()
    }

    /// Crée la note `projets/<slug>.md` ; l'identifiant du projet est le nom du fichier.
    pub fn create_project(&self, title: &str) -> Result<Project> {
        let title = title.trim();
        if title.is_empty() {
            return Err(vault_err("Le nom du projet est obligatoire"));
        }
        if self.projects()?.iter().any(|p| p.title.to_lowercase() == title.to_lowercase()) {
            return Err(vault_err(format!("Le projet « {title} » existe déjà")));
        }
        let meta: Meta = serde_json::from_value(json!({"type": "projet", "title": title})).unwrap();
        let body = format!("\n# {title}\n");
        let rel = self.vault.unique_path(PROJECTS_FOLDER, &slugify(title, 40))?;
        self.vault.write(&rel, &join_frontmatter(&meta, &body), None, true)?;
        self.index.upsert(&rel)?;
        self.projects()?
            .into_iter()
            .find(|p| p.path == rel)
            .ok_or_else(|| not_found(format!("Projet introuvable : {rel}")))
    }

    fn project(&self, project_id: &str) -> Result<Project> {
        self.projects()?
            .into_iter()
            .find(|p| p.id == project_id)
            .ok_or_else(|| not_found(format!("Projet introuvable : {project_id}")))
    }

    pub fn set_project_account(&self, project_id: &str, alias: Option<&str>) -> Result<Project> {
        let project = self.project(project_id)?;
        let text = self.vault.read_text(&project.path)?;
        let (mut meta, body) = split_frontmatter(&text);
        match alias.filter(|a| !a.is_empty()) {
            Some(alias) => {
                meta.insert("compte".into(), self.account_note(alias)?.alias.into());
            }
            None => {
                meta.shift_remove("compte");
            }
        }
        self.vault.write(&project.path, &join_frontmatter(&meta, body), None, false)?;
        self.index.upsert(&project.path)?;
        self.project(project_id)
    }

    // --- semaines -----------------------------------------------------------

    pub fn week_path(&self, day: NaiveDate) -> String {
        let (start, _) = week_bounds(day);
        format!("{FOLDER}/{}/{}.md", start.year(), week_label(start))
    }

    pub fn week(&self, day: NaiveDate) -> Result<Week> {
        let (start, end) = week_bounds(day);
        let rel = self.week_path(day);
        let exists = self.vault.exists(&rel);
        let (meta, comment) = if exists {
            let (meta, _) = split_frontmatter(&self.vault.read_text(&rel)?);
            let comment = meta_str(&meta, "comment").unwrap_or_default();
            (meta, comment)
        } else {
            (Meta::new(), String::new())
        };
        let hours = clean_hours(meta.get("hours"));
        let days = workdays(start);
        let totals = days
            .iter()
            .map(|d| (d.clone(), round2(hours.get(d).map(|e| e.values().sum()).unwrap_or(0.0))))
            .collect();
        Ok(Week {
            week: week_label(start),
            start: start.to_string(),
            end: end.to_string(),
            days,
            hours,
            totals,
            comment,
            path: exists.then_some(rel),
        })
    }

    /// Jours ouvrés de la semaine, jusqu'à `day` inclus, dont le total n'est pas 8 h.
    pub fn incomplete_days(&self, day: NaiveDate) -> Result<Vec<String>> {
        let week = self.week(day)?;
        let limit = day.to_string();
        Ok(week.days.iter().filter(|d| **d <= limit && week.totals[*d] != HOURS_PER_DAY).cloned().collect())
    }

    pub fn save_week(&self, day: NaiveDate, hours: &Hours, comment: &str) -> Result<Week> {
        let (start, end) = week_bounds(day);
        let known: HashMap<String, String> =
            self.accounts()?.into_iter().map(|a| (a.alias.to_lowercase(), a.alias)).collect();
        let mut clean = Hours::new();
        for (date, entries) in hours {
            let date_obj = parse_date(date)?;
            if date_obj < start || date_obj > end {
                return Err(vault_err(format!("{date} n'appartient pas à la semaine {}", week_label(start))));
            }
            let mut kept = IndexMap::new();
            for (alias, value) in entries {
                let value = check_number(*value, &format!("heures {alias} le {date}"))?;
                if value == 0.0 {
                    continue;
                }
                let Some(canonical) = known.get(&alias.to_lowercase()) else {
                    return Err(not_found(format!("Compte d'imputation introuvable : {alias}")));
                };
                kept.insert(canonical.clone(), round_step(value));
            }
            if kept.values().sum::<f64>() > 24.0 {
                return Err(vault_err(format!("Plus de 24 h saisies le {date}")));
            }
            if !kept.is_empty() {
                clean.insert(date_obj.to_string(), kept);
            }
        }
        clean.sort_keys();
        let rel = self.week_path(day);
        let mut meta: Meta = serde_json::from_value(json!({
            "type": "imputation", "week": week_label(start), "period_start": start.to_string(),
            "period_end": end.to_string(), "hours": hours_to_value(&clean),
        }))
        .unwrap();
        if !comment.trim().is_empty() {
            meta.insert("comment".into(), comment.trim().into());
        }
        self.vault.write(&rel, &join_frontmatter(&meta, &render(start, &clean, comment)), None, false)?;
        self.index.upsert(&rel)?;
        self.week(day)
    }

    // --- saisies et périodes -----------------------------------------------

    /// Dates de dernière saisie, triées et sans doublon.
    pub fn saisies(&self) -> Result<Vec<NaiveDate>> {
        if !self.vault.exists(SAISIES_PATH) {
            return Ok(Vec::new());
        }
        let (meta, _) = split_frontmatter(&self.vault.read_text(SAISIES_PATH)?);
        let dates: BTreeSet<NaiveDate> = match meta.get("dates") {
            Some(Value::Array(items)) => items.iter().filter_map(|v| iso_date(&value_str(v))).collect(),
            Some(value) => iso_date(&value_str(value)).into_iter().collect(),
            None => BTreeSet::new(),
        };
        Ok(dates.into_iter().collect())
    }

    fn save_saisies(&self, dates: &BTreeSet<NaiveDate>) -> Result<()> {
        let list: Vec<String> = dates.iter().map(|d| d.to_string()).collect();
        let meta: Meta = serde_json::from_value(json!({"type": "saisies-imputation", "dates": list})).unwrap();
        let mut body = String::from(
            "\n# Saisies des imputations\n\nDates de dernière saisie dans l'outil de gestion des temps : \
             chaque date clôt la période commencée le lendemain de la précédente.\n\n",
        );
        for date in dates.iter().rev() {
            body.push_str(&format!("- {date}\n"));
        }
        self.vault.write(SAISIES_PATH, &join_frontmatter(&meta, &body), None, false)?;
        self.index.upsert(SAISIES_PATH)?;
        Ok(())
    }

    pub fn add_saisie(&self, date: NaiveDate, today: NaiveDate) -> Result<Tracking> {
        let mut dates: BTreeSet<NaiveDate> = self.saisies()?.into_iter().collect();
        dates.insert(date);
        self.save_saisies(&dates)?;
        self.tracking(today)
    }

    pub fn remove_saisie(&self, date: NaiveDate, today: NaiveDate) -> Result<Tracking> {
        let mut dates: BTreeSet<NaiveDate> = self.saisies()?.into_iter().collect();
        if !dates.remove(&date) {
            return Err(not_found(format!("Pas de saisie le {date}")));
        }
        self.save_saisies(&dates)?;
        self.tracking(today)
    }

    /// Heures de toutes les semaines : {date: {alias: heures}}.
    fn all_hours(&self) -> Result<Hours> {
        let mut result = Hours::new();
        for note in self.index.by_type("imputation")? {
            for (day, entries) in clean_hours(note.meta.get("hours")) {
                let target = result.entry(day).or_default();
                for (alias, hours) in entries {
                    *target.entry(alias).or_insert(0.0) += hours;
                }
            }
        }
        Ok(result)
    }

    /// Périodes délimitées par les dates de saisie : la première date marque le début du
    /// suivi, chaque date suivante clôt une période ; la période ouverte court du lendemain
    /// de la dernière saisie jusqu'à `today`.
    pub fn tracking(&self, today: NaiveDate) -> Result<Tracking> {
        let saisies = self.saisies()?;
        let hours = self.all_hours()?;
        let accounts: HashMap<String, (String, String)> =
            self.accounts()?.into_iter().map(|a| (a.alias.to_lowercase(), (a.alias, a.title))).collect();
        let mut periods: Vec<Period> =
            saisies.windows(2).map(|w| period(w[0] + Duration::days(1), w[1], false, &hours, &accounts)).collect();
        if let Some(last) = saisies.last()
            && *last < today
        {
            periods.push(period(*last + Duration::days(1), today, true, &hours, &accounts));
        }
        periods.reverse();
        Ok(Tracking {
            last_saisie: saisies.last().map(|d| d.to_string()),
            saisies: saisies.iter().rev().map(|d| d.to_string()).collect(),
            periods,
            path: self.vault.exists(SAISIES_PATH).then(|| SAISIES_PATH.to_string()),
        })
    }

    /// Jours ouvrés incomplets depuis la dernière saisie (à défaut : dans la semaine en cours).
    pub fn missing_days(&self, today: NaiveDate) -> Result<usize> {
        if self.saisies()?.is_empty() {
            return Ok(self.incomplete_days(today)?.len());
        }
        let tracking = self.tracking(today)?;
        Ok(tracking.periods.iter().filter(|p| p.open).map(|p| p.workdays - p.complete_days).sum())
    }

    // --- proposition IA -----------------------------------------------------

    fn weekly_report_text(&self, start: NaiveDate) -> Result<Option<String>> {
        let label = week_label(start);
        let reports: Vec<NoteRow> = self
            .index
            .by_type("rapport")?
            .into_iter()
            .filter(|n| {
                meta_str(&n.meta, "kind").as_deref() == Some("hebdo") && meta_str(&n.meta, "week").as_deref() == Some(&label)
            })
            .collect();
        let Some(latest) = reports.iter().reduce(|best, n| {
            let generated = |n: &NoteRow| meta_str(&n.meta, "generated").unwrap_or_default();
            if generated(n) > generated(best) { n } else { best }
        }) else {
            return Ok(None);
        };
        let text = self.vault.read_text(&latest.path)?;
        Ok(Some(split_frontmatter(&text).1.trim().to_string()))
    }

    pub fn proposal_context(&self, day: NaiveDate) -> Result<String> {
        let (start, end) = week_bounds(day);
        let label = week_label(start);
        let consumed = self.consumed_hours(Some(&label))?;
        let accounts = self.accounts()?;
        let days: Vec<String> = workdays(start).iter().enumerate().map(|(i, d)| format!("{} {d}", DAY_NAMES[i])).collect();
        let mut lines = vec![format!("Semaine {label} du {start} au {end}. Jours à répartir : {}.", days.join(", "))];
        let account_lines: Vec<String> = accounts
            .iter()
            .map(|a| {
                let used = consumed.get(&a.alias.to_lowercase()).copied().unwrap_or(0.0) / HOURS_PER_DAY;
                let projects = if a.projects.is_empty() { "-".to_string() } else { a.projects.join(", ") };
                let description =
                    if a.description.is_empty() { String::new() } else { format!(" | description : {}", a.description) };
                format!(
                    "- {} — {}{description} | catégorie : {} | jours attribués : {} | consommés hors cette semaine : {} | restants avant cette semaine : {} | projets : {projects}",
                    a.alias,
                    a.title,
                    a.categorie,
                    fmt(a.days),
                    fmt(round2(used)),
                    fmt(round2(a.days - used)),
                )
            })
            .collect();
        lines.push(format!("\n## Comptes d'imputation\n{}", account_lines.join("\n")));
        let linked: Vec<String> = self
            .projects()?
            .into_iter()
            .filter_map(|p| p.compte.as_ref().map(|c| format!("- {} ({}) → {c}", p.id, p.title)))
            .collect();
        if !linked.is_empty() {
            lines.push(format!("\n## Projets rattachés à un compte\n{}", linked.join("\n")));
        }
        let current = self.week(day)?;
        if !current.hours.is_empty() {
            let mut entries: Vec<(&String, &IndexMap<String, f64>)> = current.hours.iter().collect();
            entries.sort_by_key(|(d, _)| *d);
            let saisies: Vec<String> = entries
                .iter()
                .map(|(d, e)| {
                    format!("- {d} : {}", e.iter().map(|(a, h)| format!("{a} {} h", fmt(*h))).collect::<Vec<_>>().join(", "))
                })
                .collect();
            lines.push(format!("\n## Heures déjà saisies cette semaine (à respecter)\n{}", saisies.join("\n")));
        }
        match self.weekly_report_text(start)? {
            Some(report) => lines.push(format!("\n## Rapport hebdomadaire de la semaine\n{report}")),
            None => lines.push(format!(
                "\n## Activité de la semaine (pas de rapport hebdomadaire généré)\n{}",
                self.reports.weekly_context(start, end, "")?
            )),
        }
        Ok(lines.join("\n"))
    }

    /// Répartition proposée par le LLM, équilibrée à 8 h par jour ; rien n'est écrit.
    pub fn propose(&self, day: NaiveDate) -> Result<Value> {
        let (start, _) = week_bounds(day);
        let accounts = self.accounts()?;
        if accounts.is_empty() {
            return Err(vault_err("Aucun compte d'imputation : créez-en au moins un"));
        }
        let known: HashMap<String, String> = accounts.into_iter().map(|a| (a.alias.to_lowercase(), a.alias)).collect();
        let proposal: ImputationProposal = chat_json(
            self.llm.as_ref(),
            &[Msg::system(IMPUTATION_SYSTEM), Msg::user(self.proposal_context(day)?)],
            &self.settings.model_smart,
        )?;
        let days = workdays(start);
        let mut hours = Hours::new();
        for item in proposal.days {
            let date: String = item.date.chars().take(10).collect();
            if !days.contains(&date) {
                continue;
            }
            let mut entries: IndexMap<String, f64> = IndexMap::new();
            for entry in item.entries {
                if let Some(alias) = known.get(&entry.compte.trim().to_lowercase())
                    && entry.hours > 0.0 {
                        *entries.entry(alias.clone()).or_insert(0.0) += entry.hours;
                    }
            }
            let balanced = balance_day(&entries);
            if !balanced.is_empty() {
                hours.insert(date, balanced);
            }
        }
        Ok(json!({"hours": hours_to_value(&hours), "comment": proposal.comment.trim()}))
    }
}

/// Bilan par compte et complétude (8 h par jour ouvré) du `start` au `end` inclus.
fn period(start: NaiveDate, end: NaiveDate, open: bool, hours: &Hours, accounts: &HashMap<String, (String, String)>) -> Period {
    let mut by_account: IndexMap<String, f64> = IndexMap::new();
    let mut gaps: IndexMap<NaiveDate, WeekGap> = IndexMap::new();
    let (mut workdays, mut complete_days) = (0, 0);
    let mut day = start;
    while day <= end {
        let entries = hours.get(&day.to_string());
        for (alias, h) in entries.into_iter().flatten() {
            let key = accounts.get(&alias.to_lowercase()).map(|(a, _)| a.clone()).unwrap_or_else(|| alias.clone());
            *by_account.entry(key).or_insert(0.0) += h;
        }
        if day.weekday().num_days_from_monday() < 5 {
            workdays += 1;
            let total = round2(entries.map(|e| e.values().sum()).unwrap_or(0.0));
            if total == HOURS_PER_DAY {
                complete_days += 1;
            } else {
                let (monday, _) = week_bounds(day);
                let gap = gaps.entry(monday).or_insert_with(|| WeekGap {
                    week: week_label(monday),
                    start: monday.to_string(),
                    days: Vec::new(),
                    missing_hours: 0.0,
                    extra_hours: 0.0,
                });
                gap.days.push(day.to_string());
                gap.missing_hours = round2(gap.missing_hours + (HOURS_PER_DAY - total).max(0.0));
                gap.extra_hours = round2(gap.extra_hours + (total - HOURS_PER_DAY).max(0.0));
            }
        }
        day += Duration::days(1);
    }
    let total_hours = round2(by_account.values().sum());
    let mut rows: Vec<PeriodAccount> = by_account
        .into_iter()
        .map(|(alias, h)| PeriodAccount {
            title: accounts.get(&alias.to_lowercase()).map(|(_, t)| t.clone()).unwrap_or_default(),
            hours: round2(h),
            days: round3(h / HOURS_PER_DAY),
            share: if total_hours > 0.0 { round3(h / total_hours) } else { 0.0 },
            alias,
        })
        .collect();
    rows.sort_by(|a, b| b.hours.total_cmp(&a.hours).then_with(|| a.alias.to_lowercase().cmp(&b.alias.to_lowercase())));
    Period {
        start: start.to_string(),
        end: end.to_string(),
        open,
        workdays,
        complete_days,
        complete: complete_days == workdays,
        total_hours,
        expected_hours: workdays as f64 * HOURS_PER_DAY,
        accounts: rows,
        incomplete_weeks: gaps.into_values().collect(),
    }
}

/// Tableau Markdown de la semaine (régénéré à chaque enregistrement).
fn render(start: NaiveDate, hours: &Hours, comment: &str) -> String {
    let days: BTreeSet<String> = workdays(start).into_iter().chain(hours.keys().cloned()).collect();
    let mut aliases: Vec<&String> = hours.values().flat_map(|e| e.keys()).collect::<BTreeSet<_>>().into_iter().collect();
    aliases.sort_by_key(|a| a.to_lowercase());
    let header: Vec<String> = days
        .iter()
        .map(|d| {
            let weekday = iso_date(d).map(|d| d.weekday().num_days_from_monday() as usize).unwrap_or(0);
            format!("{} {}/{}", DAY_NAMES[weekday], &d[8..10], &d[5..7])
        })
        .collect();
    let mut lines = vec![
        format!("\n# Imputations {}\n", week_label(start)),
        format!("| Compte | {} | Total |", header.join(" | ")),
        format!("|---|{}", "---:|".repeat(days.len() + 1)),
    ];
    let cell = |d: &String, alias: &str| hours.get(d).and_then(|e| e.get(alias)).copied().unwrap_or(0.0);
    for alias in aliases {
        let values: Vec<f64> = days.iter().map(|d| cell(d, alias)).collect();
        let cells: Vec<String> = values.iter().map(|v| if *v != 0.0 { fmt(*v) } else { String::new() }).collect();
        lines.push(format!("| [[{alias}]] | {} | {} |", cells.join(" | "), fmt(values.iter().sum())));
    }
    let totals: Vec<f64> = days.iter().map(|d| hours.get(d).map(|e| e.values().sum()).unwrap_or(0.0)).collect();
    lines.push(format!(
        "| **Total** | {} | {} |",
        totals.iter().map(|t| fmt(*t)).collect::<Vec<_>>().join(" | "),
        fmt(totals.iter().sum())
    ));
    let mut text = lines.join("\n") + "\n";
    if !comment.trim().is_empty() {
        text.push_str(&format!("\n## Justification\n\n{}\n", comment.trim()));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(items: &[(&str, f64)]) -> IndexMap<String, f64> {
        items.iter().map(|(a, h)| (a.to_string(), *h)).collect()
    }

    #[test]
    fn balance() {
        assert_eq!(balance_day(&map(&[("A", 5.0), ("B", 5.0)])), map(&[("A", 4.0), ("B", 4.0)]));
        assert_eq!(balance_day(&map(&[("A", 3.0), ("B", 3.0), ("C", 3.0)])), map(&[("A", 3.0), ("B", 2.5), ("C", 2.5)]));
        assert!(balance_day(&IndexMap::new()).is_empty());
    }

    #[test]
    fn fmt_like_python_g() {
        assert_eq!(fmt(14.0), "14");
        assert_eq!(fmt(1.75), "1.75");
        assert_eq!(fmt(38.25), "38.25");
        assert_eq!(fmt(0.1 + 0.2), "0.3");
    }
}
