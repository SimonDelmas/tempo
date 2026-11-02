//! Recherche globale (Ctrl+P) : notes, tâches, documents de la bibliothèque et brouillons.
//!
//! Lecture seule, sans index propre : on interroge les index existants et on garde les groupes
//! séparés, car leurs scores ne sont pas comparables. La recherche par mots ne charge jamais le
//! modèle d'embeddings ; celle par le sens est une route à part, appelée après la frappe.

use serde::Serialize;

use crate::drafts::Draft;
use crate::error::Result;
use crate::index::{fold, infer_type};
use crate::library::LibraryHit;
use crate::services::Services;
use crate::util::head;

/// Résultats demandés à l'index des notes, avant le filtrage et la répartition par groupe.
const NOTES_FETCHED: usize = 200;
const SNIPPET_BEFORE: usize = 60;
const SNIPPET_AFTER: usize = 100;

#[derive(Debug, Clone, Serialize)]
pub struct GlobalHit {
    /// `tache`, `note`, `document` ou `brouillon`
    pub group: &'static str,
    /// chemin de la note, identifiant du document ou du brouillon
    pub target: String,
    pub title: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub date: String,
    /// extrait, les termes trouvés entre « »
    pub snippet: String,
    pub section: String,
    pub page: Option<u32>,
}

/// Les modèles et l'inbox en attente restent hors de la recherche globale.
fn searchable(path: &str, kind: &str) -> bool {
    !["modele", "inbox"].contains(&kind) && !["modele", "inbox"].contains(&infer_type(path))
}

fn note_group(path: &str, kind: &str) -> &'static str {
    if kind == "tache" || infer_type(path) == "tache" { "tache" } else { "note" }
}

fn query_words(query: &str) -> Vec<String> {
    fold(query).split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(String::from).collect()
}

fn contains_all(text: &str, words: &[String]) -> bool {
    let text = fold(text);
    words.iter().all(|w| text.contains(w.as_str()))
}

fn document_hit(hit: LibraryHit) -> GlobalHit {
    GlobalHit {
        group: "document",
        target: hit.document.id,
        title: hit.document.title,
        kind: hit.document.format,
        date: hit.document.added,
        snippet: hit.snippet,
        section: hit.section,
        page: hit.page,
    }
}

/// Extrait autour du premier terme trouvé (termes entre « »), sinon le début du texte.
fn draft_snippet(content: &str, words: &[String]) -> String {
    let chars: Vec<char> = content.chars().collect();
    // texte replié caractère par caractère, pour retrouver la position dans l'original
    let mut folded = String::new();
    let mut origin = Vec::new();
    for (i, c) in chars.iter().enumerate() {
        let one = fold(&c.to_string());
        let one = if one.is_empty() { " ".to_string() } else { one };
        origin.extend(std::iter::repeat_n(i, one.len()));
        folded.push_str(&one);
    }
    let found = words.iter().filter_map(|w| folded.find(w.as_str()).map(|at| (at, at + w.len()))).min();
    let flat = |part: &[char]| part.iter().collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ");
    let Some((start, end)) = found else {
        return head(&flat(&chars), SNIPPET_AFTER);
    };
    let (start, end) = (origin[start], origin[end - 1] + 1);
    let from = start.saturating_sub(SNIPPET_BEFORE);
    let to = (end + SNIPPET_AFTER).min(chars.len());
    format!(
        "{}{} «{}» {}{}",
        if from > 0 { "…" } else { "" },
        flat(&chars[from..start]),
        flat(&chars[start..end]),
        flat(&chars[end..to]),
        if to < chars.len() { "…" } else { "" },
    )
    .trim()
    .to_string()
}

fn draft_hit(draft: Draft, words: &[String]) -> GlobalHit {
    let first_line = draft.content.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let title = if !draft.title.trim().is_empty() {
        draft.title.clone()
    } else if !first_line.is_empty() {
        head(first_line, 60)
    } else {
        "Brouillon sans titre".to_string()
    };
    GlobalHit {
        group: "brouillon",
        snippet: draft_snippet(&draft.content, words),
        target: draft.id,
        title,
        kind: "brouillon".into(),
        date: draft.updated,
        section: String::new(),
        page: None,
    }
}

/// Recherche par mots : au plus `limit` résultats par groupe, les titres d'abord pour les notes
/// et les tâches, les plus récents d'abord pour les brouillons.
pub fn by_words(services: &Services, query: &str, limit: usize) -> Result<Vec<GlobalHit>> {
    let words = query_words(query);
    if words.is_empty() {
        return Ok(Vec::new());
    }
    let mut notes: Vec<_> =
        services.index.search(query, NOTES_FETCHED)?.into_iter().filter(|h| searchable(&h.path, &h.kind)).collect();
    // tri stable : à pertinence égale, l'ordre de l'index est conservé
    notes.sort_by_key(|h| !contains_all(&h.title, &words));
    let mut hits = Vec::new();
    for group in ["tache", "note"] {
        hits.extend(notes.iter().filter(|h| note_group(&h.path, &h.kind) == group).take(limit).map(|h| GlobalHit {
            group,
            target: h.path.clone(),
            title: h.title.clone(),
            kind: h.kind.clone(),
            date: h.date.clone(),
            snippet: h.snippet.clone(),
            section: String::new(),
            page: None,
        }));
    }
    hits.extend(services.library.search_words(query, limit)?.into_iter().map(document_hit));
    let mut drafts: Vec<Draft> = services
        .drafts
        .list()?
        .into_iter()
        .filter(|d| contains_all(&format!("{} {}", d.title, d.content), &words))
        .collect();
    drafts.sort_by(|a, b| b.updated.cmp(&a.updated));
    hits.extend(drafts.into_iter().take(limit).map(|d| draft_hit(d, &words)));
    Ok(hits)
}

/// Recherche par le sens, notes puis documents ; charge le modèle d'embeddings au besoin. Les
/// notes sont filtrées au seuil de similarité de la bibliothèque, sinon l'index renverrait
/// toujours ses `limit` voisins, même sans aucun rapport.
pub fn by_meaning(services: &Services, query: &str, limit: usize) -> Result<Vec<GlobalHit>> {
    let threshold = f64::from(services.library.min_similarity().unwrap_or(0.0));
    let mut hits: Vec<GlobalHit> = services
        .semantic
        .search(query, limit)?
        .into_iter()
        .filter(|h| h.score >= threshold && searchable(&h.path, &h.kind))
        .map(|h| GlobalHit {
            group: note_group(&h.path, &h.kind),
            target: h.path,
            title: h.title,
            kind: h.kind,
            date: h.date,
            snippet: h.snippet,
            section: String::new(),
            page: None,
        })
        .collect();
    hits.extend(services.library.search_meaning(query, limit)?.into_iter().map(document_hit));
    Ok(hits)
}
