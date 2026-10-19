//! Petites fonctions partagées : dates ISO, troncature de texte.

use std::sync::LazyLock;

use chrono::{Datelike, Duration, Local, NaiveDate};
use regex::Regex;

use crate::error::{vault_err, Result};

static ISO_DATE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{4}-\d{2}-\d{2}$").unwrap());

pub fn today() -> NaiveDate {
    Local::now().date_naive()
}

/// Date au format AAAA-MM-JJ (les caractères suivants, une heure par exemple, sont ignorés).
pub fn iso_date(value: &str) -> Option<NaiveDate> {
    let head: String = value.chars().take(10).collect();
    if !ISO_DATE_RE.is_match(&head) {
        return None;
    }
    NaiveDate::parse_from_str(&head, "%Y-%m-%d").ok()
}

/// Comme [`iso_date`], avec l'erreur affichée à l'utilisateur.
pub fn parse_date(value: &str) -> Result<NaiveDate> {
    iso_date(value).ok_or_else(|| vault_err(format!("Date invalide : {value} (format AAAA-MM-JJ)")))
}

/// Date facultative : `None` si absente ou vide, aujourd'hui par défaut.
pub fn date_or_today(value: Option<&str>) -> Result<NaiveDate> {
    match value.filter(|v| !v.is_empty()) {
        Some(v) => parse_date(v),
        None => Ok(today()),
    }
}

/// Lundi et dimanche de la semaine d'un jour.
pub fn week_bounds(day: NaiveDate) -> (NaiveDate, NaiveDate) {
    let monday = day - Duration::days(day.weekday().num_days_from_monday() as i64);
    (monday, monday + Duration::days(6))
}

/// Libellé ISO d'une semaine : `2026-S42`.
pub fn week_label(day: NaiveDate) -> String {
    let week = day.iso_week();
    format!("{}-S{:02}", week.year(), week.week())
}

/// Coupe un texte trop long (en caractères) en le signalant.
pub fn clip(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        text.to_string()
    } else {
        let cut: String = text.chars().take(limit).collect();
        format!("{cut}\n[…tronqué]")
    }
}

/// Les `n` premiers caractères d'un texte.
pub fn head(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn week_bounds_and_label() {
        let day = NaiveDate::from_ymd_opt(2026, 10, 17).unwrap();
        let (start, end) = week_bounds(day);
        assert_eq!((start.to_string(), end.to_string()), ("2026-10-12".into(), "2026-10-18".into()));
        assert_eq!(week_label(day), "2026-S42");
    }

    #[test]
    fn iso_dates() {
        assert!(iso_date("2026-10-20T10:00").is_some());
        assert!(iso_date("vendredi").is_none());
        assert!(iso_date("2026-13-01").is_none());
    }
}
