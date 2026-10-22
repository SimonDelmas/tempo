//! Outils de l'agent sur la bibliothèque de documents, et vérification des citations.
//!
//! `search_documents` renvoie des passages (document, section ou page, texte) ; `read_document`
//! lit un document entier quand il est court, sinon donne son résumé et sa table des matières
//! puis lit une section ou des pages ; `list_documents` parcourt le corpus. Chaque passage lu est
//! consigné dans un [`ReadLog`] : une citation `[[document#section]]` ou `[[document#p. N]]` de
//! la réponse n'est vérifiée que si l'agent a effectivement lu ce passage.

use std::sync::LazyLock;

use regex::Regex;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::error::{vault_err, Result};
use crate::index::{fold, SearchMode};
use crate::library::{outline_sizes, pages_text, section_text, Document, LibraryFilter, LibraryService};
use crate::llm::ToolDef;

/// Document lu en entier en dessous de cette taille (caractères, ~10 000 tokens).
pub const FULL_READ_CHARS: usize = 40_000;
/// Taille d'une lecture par section ou par pages ; la suite se demande avec `offset`.
pub const PART_CHARS: usize = 30_000;
/// Taille maximale du résultat d'un outil de la bibliothèque transmis au modèle.
pub const MAX_RESULT_CHARS: usize = 44_000;
const SEARCH_LIMIT: usize = 10;
const EXCERPT_CHARS: usize = 320;

static CITATION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[\[([^\]\|#\n]+)(?:#([^\]\|\n]*))?(?:\|[^\]\n]*)?\]\]").unwrap());
static PAGE_ANCHOR_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(?:p\.?|page)\s*(\d+)$").unwrap());
static PAGES_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*(\d+)\s*(?:-\s*(\d+))?\s*$").unwrap());

pub const LIBRARY_TOOLS: [&str; 3] = ["search_documents", "read_document", "list_documents"];

fn text(description: &str) -> Value {
    json!({"type": "string", "description": description})
}

fn object(required: &[&str], properties: Value) -> Value {
    json!({"type": "object", "properties": properties, "required": required, "additionalProperties": false})
}

fn tool(name: &'static str, description: &str, parameters: Value) -> ToolDef {
    ToolDef { name, description: description.into(), parameters }
}

fn filters() -> Map<String, Value> {
    let Value::Object(map) = json!({
        "theme": text("facultatif : identifiant d'un thème (list_documents les donne)"),
        "project": text("facultatif : identifiant d'un projet"),
        "tag": text("facultatif : tag"),
        "format": text("facultatif : format du fichier d'origine (pdf, docx, pptx, xlsx, eml…)"),
    }) else {
        unreachable!()
    };
    map
}

/// Schémas des outils de la bibliothèque.
pub fn tool_defs() -> Vec<ToolDef> {
    let mut search = filters();
    search.insert("query".into(), text("mots-clés"));
    search.insert(
        "mode".into(),
        json!({"type": "string", "enum": ["mots", "phrase", "ou"],
               "description": "mots : tous les mots (par défaut) ; phrase : les mots côte à côte ; ou : l'un des mots"}),
    );
    let mut list = filters();
    list.insert("query".into(), text("facultatif : mots du titre, des tags ou du texte"));
    list.insert("added_after".into(), text("facultatif : ajoutés à partir de cette date (AAAA-MM-JJ)"));
    list.insert("added_before".into(), text("facultatif : ajoutés jusqu'à cette date (AAAA-MM-JJ)"));
    vec![
        tool(
            "search_documents",
            "Recherche plein texte dans les documents de la bibliothèque (PDF, Word, présentations, mails…). \
             Renvoie les passages les plus pertinents avec leur document, leur section ou leur page, et la \
             citation à reprendre dans la réponse. Porte sur des mots : essaie plusieurs formulations, en \
             français et en anglais.",
            object(&["query"], Value::Object(search)),
        ),
        tool(
            "read_document",
            "Lit un document de la bibliothèque. Sans section ni pages : le texte entier s'il est court, sinon son \
             résumé et sa table des matières (taille de chaque section) pour choisir quoi lire. Avec section (titre) \
             ou pages (\"3\" ou \"3-5\", documents PDF) : cette partie. Ne lis un document entier que si la question \
             porte sur lui tout entier (résumé, inventaire, comparaison) ; sinon, préfère les passages de \
             search_documents ou une section.",
            object(&["document"], json!({
                "document": text("identifiant du document (champ \"document\" des autres outils) ou lien [[…]]"),
                "section": text("facultatif : titre de la section à lire (avec ses sous-sections)"),
                "pages": text("facultatif : page ou plage de pages, ex. \"12\" ou \"12-14\""),
                "offset": {"type": "integer", "description": "facultatif : reprise d'une lecture tronquée (champ \"suite\")"},
            })),
        ),
        tool(
            "list_documents",
            "Liste les documents de la bibliothèque (titre, format, thèmes, projets, tags, date d'ajout, résumé), \
             filtrés par thème, projet, tag, format, date ou mots ; donne aussi les thèmes existants.",
            object(&[], Value::Object(list)),
        ),
    ]
}

/// Passage lu par l'agent pendant une réponse.
#[derive(Debug, Clone)]
pub struct ReadEntry {
    pub document: String,
    /// section lue (chemin des titres pour un passage), `None` pour le document entier ou des pages
    pub section: Option<String>,
    /// pages lues (première, dernière)
    pub pages: Option<(u32, u32)>,
    /// document lu en entier
    pub whole: bool,
    pub text: String,
}

/// Passages lus pendant une réponse.
#[derive(Debug, Clone, Default)]
pub struct ReadLog(pub Vec<ReadEntry>);

#[derive(Deserialize)]
struct SearchArgs {
    query: String,
    mode: Option<String>,
    #[serde(flatten)]
    filter: FilterArgs,
}

#[derive(Deserialize, Default)]
struct FilterArgs {
    theme: Option<String>,
    project: Option<String>,
    tag: Option<String>,
    format: Option<String>,
}

impl FilterArgs {
    fn filter(&self, q: Option<String>) -> LibraryFilter {
        LibraryFilter {
            q,
            theme: self.theme.clone(),
            project: self.project.clone(),
            tag: self.tag.clone(),
            format: self.format.clone(),
            pending: None,
        }
    }
}

#[derive(Deserialize)]
struct ReadArgs {
    document: String,
    section: Option<String>,
    pages: Option<String>,
    #[serde(default)]
    offset: usize,
}

#[derive(Deserialize)]
struct ListArgs {
    query: Option<String>,
    added_after: Option<String>,
    added_before: Option<String>,
    #[serde(flatten)]
    filter: FilterArgs,
}

/// Citation à reprendre pour un passage : la page s'il en a une, sinon le dernier titre de sa section.
pub fn citation_for(document: &str, section: &str, page: Option<u32>) -> String {
    match (page, section.rsplit(" › ").next().filter(|s| !s.is_empty())) {
        (Some(page), _) => format!("[[{document}#p. {page}]]"),
        (None, Some(title)) => format!("[[{document}#{title}]]"),
        (None, None) => format!("[[{document}]]"),
    }
}

fn clip_chars(text: &str, offset: usize, limit: usize) -> (String, Option<usize>) {
    let chars: Vec<char> = text.chars().collect();
    let start = offset.min(chars.len());
    let end = (start + limit).min(chars.len());
    (chars[start..end].iter().collect(), (end < chars.len()).then_some(end))
}

fn excerpt(text: &str) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().starts_with("<!--")).collect();
    let flat = lines.join(" ").split_whitespace().collect::<Vec<_>>().join(" ");
    let flat = flat.trim_start_matches(['#', ' ']).to_string();
    let (cut, more) = clip_chars(&flat, 0, EXCERPT_CHARS);
    if more.is_some() { format!("{cut}…") } else { cut }
}

fn document_card(d: &Document) -> Value {
    json!({
        "document": d.id, "titre": d.title, "format": d.format, "pages": d.pages, "themes": d.themes,
        "projets": d.projects, "tags": d.tags, "ajoute": d.added, "resume": d.summary,
        "a_classer": d.pending, "lien": format!("[[{}]]", d.id),
    })
}

/// Identifiant d'un document désigné par son identifiant, son titre ou un lien `[[…]]`.
fn document_id(library: &LibraryService, name: &str) -> Result<String> {
    let name = name.trim().trim_start_matches("[[").trim_end_matches("]]");
    let name = name.split(['#', '|']).next().unwrap_or(name).trim();
    library.resolve(name)?.ok_or_else(|| vault_err(format!("Aucun document ne correspond à « {name} »")))
}

/// Exécute un outil de la bibliothèque ; `None` si `name` n'en est pas un. Les erreurs de
/// l'agent (arguments, document inconnu) sont renvoyées en texte.
pub fn call(library: &LibraryService, name: &str, args: &Map<String, Value>, log: &mut ReadLog) -> Option<Result<std::result::Result<Value, String>>> {
    if !LIBRARY_TOOLS.contains(&name) {
        return None;
    }
    fn parse<T: serde::de::DeserializeOwned>(name: &str, args: &Map<String, Value>) -> std::result::Result<T, String> {
        serde_json::from_value(Value::Object(args.clone())).map_err(|e| format!("Erreur : arguments invalides pour {name} ({e})."))
    }
    let result = match name {
        "search_documents" => parse::<SearchArgs>(name, args).map(|a| search(library, a, log)),
        "read_document" => parse::<ReadArgs>(name, args).map(|a| read(library, a, log)),
        _ => parse::<ListArgs>(name, args).map(|a| list(library, a)),
    };
    Some(match result {
        Ok(Ok(value)) => Ok(Ok(value)),
        Ok(Err(err)) => Err(err),
        Err(message) => Ok(Err(message)),
    })
}

fn search(library: &LibraryService, args: SearchArgs, log: &mut ReadLog) -> Result<Value> {
    let mode = match args.mode.as_deref() {
        Some("phrase") => SearchMode::Phrase,
        Some("ou") => SearchMode::Any,
        _ => SearchMode::Words,
    };
    let filter = args.filter.filter(None);
    let mut passages = library.search_passages(&args.query, mode, &filter, SEARCH_LIMIT)?;
    // aucun passage avec tous les mots : on élargit à l'un des mots plutôt que de rendre la main vide
    let widened = passages.is_empty() && mode == SearchMode::Words && args.query.split_whitespace().count() > 1;
    if widened {
        passages = library.search_passages(&args.query, SearchMode::Any, &filter, SEARCH_LIMIT)?;
    }
    let mut results = Vec::new();
    for p in passages {
        log.0.push(ReadEntry {
            document: p.document.clone(),
            section: Some(p.section.clone()).filter(|s| !s.is_empty()),
            pages: p.page.map(|n| (n, n)),
            whole: false,
            text: p.text.clone(),
        });
        results.push(json!({
            "document": p.document, "titre": p.title, "section": p.section, "page": p.page,
            "citation": citation_for(&p.document, &p.section, p.page), "texte": p.text,
        }));
    }
    if results.is_empty() {
        return Ok(Value::Array(results));
    }
    let mut output = json!({"passages": results});
    if widened {
        output["remarque"] = "Aucun passage ne contient tous les mots : passages contenant l'un d'eux, à vérifier".into();
    }
    Ok(output)
}

fn parse_pages(pages: &str) -> Result<(u32, u32)> {
    let caps = PAGES_RE.captures(pages).ok_or_else(|| vault_err(format!("Pages invalides : {pages} (ex. \"3\" ou \"3-5\")")))?;
    let from: u32 = caps[1].parse().map_err(|_| vault_err("Page invalide"))?;
    let to: u32 = caps.get(2).and_then(|m| m.as_str().parse().ok()).unwrap_or(from);
    Ok((from.min(to), from.max(to)))
}

fn read(library: &LibraryService, args: ReadArgs, log: &mut ReadLog) -> Result<Value> {
    let id = document_id(library, &args.document)?;
    let (document, body) = library.content(&id)?;
    let mut result = document_card(&document);
    let section = args.section.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let pages = args.pages.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let (part, entry) = if let Some(wanted) = section {
        let (title, part) = section_text(&body, wanted).ok_or_else(|| {
            let titles: Vec<String> = outline_sizes(&body).into_iter().take(40).map(|s| s.title).collect();
            vault_err(format!("Section introuvable : « {wanted} ». Sections : {}", titles.join(" ; ")))
        })?;
        result["section"] = title.clone().into();
        result["citation"] = format!("[[{id}#{title}]]").into();
        (part, ReadEntry { document: id.clone(), section: Some(title), pages: None, whole: false, text: String::new() })
    } else if let Some(pages) = pages {
        let (from, to) = parse_pages(pages)?;
        let part = pages_text(&body, from, to)
            .ok_or_else(|| vault_err("Ce document n'est pas découpé en pages : lis-le par section"))?;
        result["pages"] = format!("{from}-{to}").into();
        result["citation"] = format!("[[{id}#p. {from}]]").into();
        (part, ReadEntry { document: id.clone(), section: None, pages: Some((from, to)), whole: false, text: String::new() })
    } else if body.chars().count() <= FULL_READ_CHARS {
        result["citation"] = "lien [[document#section]] ou [[document#p. N]] de la partie utilisée".into();
        (body.clone(), ReadEntry { document: id.clone(), section: None, pages: None, whole: true, text: String::new() })
    } else {
        // document long : résumé et table des matières, sans texte
        result["taille"] = body.chars().count().into();
        result["table_des_matieres"] = json!(outline_sizes(&body)
            .into_iter()
            .map(|s| json!({"niveau": s.level, "titre": s.title, "caracteres": s.chars, "page": s.page}))
            .collect::<Vec<_>>());
        result["consigne"] = "Document long : lis les sections (section) ou les pages (pages) utiles à la question, \
                              au besoin plusieurs, ou cherche un passage avec search_documents."
            .into();
        return Ok(result);
    };
    let (text, next) = clip_chars(&part, args.offset, PART_CHARS);
    if let Some(next) = next {
        result["suite"] = next.into();
        result["tronque"] = format!("Partie tronquée : rappelle read_document avec offset={next} pour la suite").into();
    }
    log.0.push(ReadEntry { text: text.clone(), ..entry });
    result["texte"] = text.into();
    Ok(result)
}

fn list(library: &LibraryService, args: ListArgs) -> Result<Value> {
    let query = args.query.clone().filter(|q| !q.trim().is_empty());
    let hits = library.list(&args.filter.filter(query))?;
    let documents: Vec<Value> = hits
        .iter()
        .map(|h| &h.document)
        .filter(|d| args.added_after.as_deref().is_none_or(|a| d.added.as_str() >= a))
        .filter(|d| args.added_before.as_deref().is_none_or(|b| d.added.as_str() <= b))
        .take(80)
        .map(document_card)
        .collect();
    let themes: Vec<Value> = library.themes()?.into_iter().map(|t| json!({"id": t.id, "titre": t.title})).collect();
    Ok(json!({"documents": documents, "total": hits.len(), "themes": themes}))
}

/// Vérifie les citations de documents d'une réponse : le document existe, la section ou la page
/// citée aussi, et l'agent a lu ce passage. `is_note` écarte les liens qui désignent une note.
pub fn citations(library: &LibraryService, answer: &str, log: &ReadLog, is_note: &dyn Fn(&str) -> bool) -> Vec<Value> {
    let mut seen = Vec::new();
    let mut citations = Vec::new();
    for caps in CITATION_RE.captures_iter(answer) {
        let target = caps[1].trim().to_string();
        let anchor = caps.get(2).map(|m| m.as_str().trim().to_string()).filter(|a| !a.is_empty());
        let key = match &anchor {
            Some(a) => format!("{target}#{a}"),
            None => target.clone(),
        };
        if seen.contains(&key) || (anchor.is_none() && is_note(&target)) {
            continue;
        }
        seen.push(key.clone());
        let Ok(Some(id)) = library.resolve(&target) else { continue };
        let Ok((document, body)) = library.content(&id) else { continue };
        let reads: Vec<&ReadEntry> = log.0.iter().filter(|e| e.document == id).collect();
        let page = anchor.as_deref().and_then(|a| PAGE_ANCHOR_RE.captures(a)).and_then(|c| c[1].parse::<u32>().ok());
        let (section, found, passage) = match (&anchor, page) {
            (None, _) => (None, true, document.summary.clone()),
            (Some(_), Some(n)) => {
                let text = pages_text(&body, n, n);
                (None, text.as_ref().is_some_and(|t| t.lines().count() > 1), text.unwrap_or_default())
            }
            (Some(a), None) => match section_text(&body, a) {
                Some((title, text)) => (Some(title), true, text),
                None => (None, false, String::new()),
            },
        };
        let read = reads.iter().any(|e| {
            e.whole
                || match (page, &section) {
                    (Some(n), _) => e.pages.is_some_and(|(from, to)| n >= from && n <= to),
                    (None, Some(title)) => {
                        let key = fold(title);
                        e.section.as_deref().is_some_and(|s| s.split(" › ").any(|part| fold(part) == key))
                            || e.text.lines().any(|l| l.trim_start().starts_with('#') && fold(l.trim_start_matches(['#', ' '])) == key)
                    }
                    (None, None) => true,
                }
        });
        let reason = if !found {
            Some(if page.is_some() { "page absente du document" } else { "section introuvable dans le document" })
        } else if !read {
            Some("passage non consulté par l'agent")
        } else {
            None
        };
        citations.push(json!({
            "link": key, "document": id, "title": document.title, "section": section, "page": page,
            "verified": reason.is_none(), "reason": reason, "excerpt": excerpt(&passage),
        }));
    }
    citations
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn citations_des_passages() {
        assert_eq!(citation_for("Spec", "Contexte › Flux", None), "[[Spec#Flux]]");
        assert_eq!(citation_for("Rapport", "", Some(4)), "[[Rapport#p. 4]]");
        assert_eq!(citation_for("Note", "", None), "[[Note]]");
        assert_eq!(parse_pages(" 5 - 3 ").unwrap(), (3, 5));
        assert!(parse_pages("trois").is_err());
        assert_eq!(excerpt("a\n\n b"), "a b");
    }
}
