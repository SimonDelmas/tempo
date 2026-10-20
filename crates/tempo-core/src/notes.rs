//! Modifications ciblées d'une note : remplacement de passages, ajout dans une section,
//! frontmatter clé par clé, ajout et retrait de liens.
//!
//! Fonctions pures sur le texte d'une note : elles servent à vérifier une proposition de
//! l'agent, puis à l'appliquer sur le contenu du moment (qui a pu changer entre-temps). Le
//! frontmatter n'est réécrit que s'il change ; sinon il est conservé tel qu'écrit.

use std::sync::LazyLock;

use regex::{Captures, Regex};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::{vault_err, Result};
use crate::index::extract_links;
use crate::vault::{append_to_section, join_frontmatter, split_frontmatter, Meta};

/// Section où sont ajoutés les liens qui ne vont pas dans le frontmatter.
pub const LINKS_HEADING: &str = "## Liens";

static LINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[\[([^\]\|#\n]+)(?:#[^\]\|\n]*)?(?:\|([^\]\n]*))?\]\]").unwrap());

/// Remplacement d'un passage : `find` doit figurer une seule fois dans la note.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextEdit {
    pub find: String,
    #[serde(default)]
    pub replace: String,
}

/// Frontmatter tel qu'écrit (délimiteurs compris) et corps de la note.
pub fn split_raw(text: &str) -> (&str, &str) {
    let (_, body) = split_frontmatter(text);
    text.split_at(text.len() - body.len())
}

fn excerpt(text: &str) -> String {
    let short: String = text.chars().take(60).collect();
    if short.len() < text.len() { format!("{short}…") } else { short }
}

/// Applique les remplacements dans l'ordre ; chaque passage cherché doit être unique.
pub fn apply_edits(body: &str, edits: &[TextEdit]) -> Result<String> {
    if edits.is_empty() {
        return Err(vault_err("Aucune modification proposée"));
    }
    // les fins de ligne du modèle suivent celles du fichier
    let eol = if body.contains("\r\n") { "\r\n" } else { "\n" };
    let mut text = body.to_string();
    for (i, edit) in edits.iter().enumerate() {
        let find = edit.find.replace("\r\n", "\n").replace('\n', eol);
        let replace = edit.replace.replace("\r\n", "\n").replace('\n', eol);
        if find.trim().is_empty() {
            return Err(vault_err(format!("Modification {} : passage à remplacer vide", i + 1)));
        }
        match text.matches(&find).count() {
            0 => return Err(vault_err(format!("Modification {} : passage introuvable « {} »", i + 1, excerpt(&edit.find)))),
            1 => text = text.replacen(&find, &replace, 1),
            n => {
                return Err(vault_err(format!(
                    "Modification {} : « {} » figure {n} fois, cite un passage plus long",
                    i + 1,
                    excerpt(&edit.find)
                )))
            }
        }
    }
    Ok(text)
}

/// Titre de section normalisé (`Décisions` → `## Décisions`).
pub fn heading(section: &str) -> String {
    let section = section.trim();
    if section.starts_with('#') { section.to_string() } else { format!("## {section}") }
}

/// Ajoute `text` à la fin d'une section (créée si absente) ou, sans section, de la note.
pub fn append_text(body: &str, section: Option<&str>, text: &str) -> Result<String> {
    let text = text.trim_end();
    if text.trim().is_empty() {
        return Err(vault_err("Le texte à ajouter est vide"));
    }
    Ok(match section.filter(|s| !s.trim().is_empty()) {
        Some(section) => append_to_section(body, &heading(section), text),
        None => format!("{}\n\n{text}\n", body.trim_end()),
    })
}

/// Modifie le frontmatter : les clés existantes gardent leur place, les nouvelles vont à la fin.
pub fn update_meta(meta: &mut Meta, set: &Map<String, Value>, remove: &[String]) -> Result<()> {
    if set.is_empty() && remove.is_empty() {
        return Err(vault_err("Aucune modification proposée"));
    }
    if set.keys().chain(remove).any(|k| k.trim().is_empty()) {
        return Err(vault_err("Clé de frontmatter vide"));
    }
    for (key, value) in set {
        meta.insert(key.clone(), value.clone());
    }
    for key in remove {
        meta.shift_remove(key);
    }
    Ok(())
}

/// Texte de la note avec un nouveau frontmatter et le corps inchangé.
pub fn with_meta(meta: &Meta, body: &str) -> String {
    join_frontmatter(meta, body)
}

/// Ajoute le lien `link` (`[[nom]]`) à la liste `field` du frontmatter ou, sans champ, à une
/// section du corps (`## Liens` par défaut). `is_target` reconnaît une cible déjà liée.
pub fn add_link(
    text: &str,
    link: &str,
    field: Option<&str>,
    section: Option<&str>,
    is_target: impl Fn(&str) -> bool,
) -> Result<String> {
    let (raw_meta, body) = split_raw(text);
    let Some(field) = field.map(str::trim).filter(|f| !f.is_empty()) else {
        if extract_links(text).iter().any(|name| is_target(name)) {
            return Err(vault_err("La note contient déjà ce lien"));
        }
        let heading = heading(section.filter(|s| !s.trim().is_empty()).unwrap_or(LINKS_HEADING));
        return Ok(format!("{raw_meta}{}", append_to_section(body, &heading, &format!("- {link}"))));
    };
    let (mut meta, _) = split_frontmatter(text);
    let linked = |value: &Value| value.as_str().is_some_and(|s| extract_links(s).iter().any(|name| is_target(name)));
    let entry = Value::String(link.to_string());
    let updated = match meta.get(field) {
        None | Some(Value::Null) => Value::Array(vec![entry]),
        Some(Value::String(s)) if s.trim().is_empty() => Value::Array(vec![entry]),
        Some(Value::Array(items)) if items.iter().any(linked) => {
            return Err(vault_err(format!("Le champ {field} contient déjà ce lien")));
        }
        Some(Value::Array(items)) => Value::Array(items.iter().cloned().chain([entry]).collect()),
        Some(value) if linked(value) => return Err(vault_err(format!("Le champ {field} contient déjà ce lien"))),
        Some(value @ Value::String(_)) => Value::Array(vec![value.clone(), entry]),
        Some(_) => return Err(vault_err(format!("Le champ {field} n'est pas une liste"))),
    };
    meta.insert(field.to_string(), updated);
    Ok(with_meta(&meta, body))
}

/// Retire les liens vers une cible : entrées du frontmatter supprimées, liens du corps
/// remplacés par leur texte. Renvoie le nouveau texte et le nombre de liens retirés.
pub fn remove_link(text: &str, is_target: impl Fn(&str) -> bool) -> Result<(String, usize)> {
    let (raw_meta, body) = split_raw(text);
    let (mut meta, _) = split_frontmatter(text);
    let linked = |value: &Value| value.as_str().is_some_and(|s| extract_links(s).iter().any(|name| is_target(name)));
    let mut removed = 0;
    let keys: Vec<String> = meta.keys().cloned().collect();
    for key in keys {
        match meta.get_mut(&key) {
            Some(Value::Array(items)) => {
                let before = items.len();
                items.retain(|item| !linked(item));
                removed += before - items.len();
            }
            Some(value) if linked(value) => {
                meta.shift_remove(&key);
                removed += 1;
            }
            _ => {}
        }
    }
    let meta_changed = removed > 0;
    let new_body = LINK_RE.replace_all(body, |caps: &Captures| {
        let name = caps[1].trim();
        if !is_target(name) {
            return caps[0].to_string();
        }
        removed += 1;
        caps.get(2).map(|alias| alias.as_str()).unwrap_or(name).to_string()
    });
    if removed == 0 {
        return Err(vault_err("La note ne contient aucun lien vers cette cible"));
    }
    let text = if meta_changed { with_meta(&meta, &new_body) } else { format!("{raw_meta}{new_body}") };
    Ok((text, removed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOTE: &str = "---\ntype: reunion\ntasks: ['[[T-001]]']\n---\n\n# COPIL\n\n## Décisions\n- Recette au 28.\n\n## Actions\n- Moi : procédure ([[T-004]]).\n";

    fn edit(find: &str, replace: &str) -> TextEdit {
        TextEdit { find: find.into(), replace: replace.into() }
    }

    #[test]
    fn edits_require_unique_passages() {
        let (_, body) = split_raw(NOTE);
        let result = apply_edits(body, &[edit("Recette au 28.", "Recette au 4 novembre.")]).unwrap();
        assert!(result.contains("Recette au 4 novembre.") && !result.contains("au 28"));
        assert!(apply_edits(body, &[edit("absent", "x")]).unwrap_err().to_string().contains("introuvable"));
        assert!(apply_edits(body, &[edit("##", "x")]).unwrap_err().to_string().contains("2 fois"));
        assert!(apply_edits(body, &[]).is_err());
        // fins de ligne Windows
        let crlf = body.replace('\n', "\r\n");
        let result = apply_edits(&crlf, &[edit("## Décisions\n- Recette", "## Décisions\n- La recette")]).unwrap();
        assert!(result.contains("## Décisions\r\n- La recette"));
    }

    #[test]
    fn append_to_section_or_end() {
        let (_, body) = split_raw(NOTE);
        let result = append_text(body, Some("Décisions"), "- Go pour la reprise.").unwrap();
        assert!(result.contains("- Recette au 28.\n- Go pour la reprise.\n\n## Actions"));
        assert!(append_text(body, None, "Fin.").unwrap().ends_with("([[T-004]]).\n\nFin.\n"));
        assert!(append_text(body, Some("Risques"), "- Accès").unwrap().ends_with("## Risques\n- Accès\n"));
        assert!(append_text(body, None, "  ").is_err());
    }

    #[test]
    fn meta_keeps_key_order() {
        let (mut meta, _) = split_frontmatter(NOTE);
        let set = json!({"type": "sujet", "projects": ["sap"]});
        update_meta(&mut meta, set.as_object().unwrap(), &["tasks".into()]).unwrap();
        assert_eq!(meta.keys().collect::<Vec<_>>(), vec!["type", "projects"]);
        assert!(update_meta(&mut meta, &Map::new(), &[]).is_err());
    }

    #[test]
    fn links_in_frontmatter_or_section() {
        let is = |target: &'static str| move |name: &str| name.eq_ignore_ascii_case(target);
        let result = add_link(NOTE, "[[T-003]]", Some("tasks"), None, is("T-003")).unwrap();
        assert!(result.starts_with("---\ntype: reunion\ntasks: ['[[T-001]]', '[[T-003]]']\n---\n"));
        assert!(add_link(NOTE, "[[T-001]]", Some("tasks"), None, is("t-001")).is_err());
        let result = add_link(NOTE, "[[Claire Martin]]", Some("participants"), None, is("Claire Martin")).unwrap();
        assert!(result.contains("participants: ['[[Claire Martin]]']"));
        // dans le corps : frontmatter conservé tel quel
        let result = add_link(NOTE, "[[sap]]", None, None, is("sap")).unwrap();
        assert!(result.starts_with(&NOTE[..44]) && result.ends_with("## Liens\n- [[sap]]\n"));
        assert!(add_link(NOTE, "[[T-004]]", None, None, is("T-004")).is_err());
    }

    #[test]
    fn unlink_everywhere() {
        let text = format!("{NOTE}Voir [[T-001|la reprise]] et [[T-001]].\n");
        let (result, count) = remove_link(&text, |name| name == "T-001").unwrap();
        assert_eq!(count, 3);
        assert!(result.contains("tasks: []") && result.contains("Voir la reprise et T-001."));
        let (result, count) = remove_link(NOTE, |name| name == "T-004").unwrap();
        assert_eq!(count, 1);
        assert!(result.starts_with(&NOTE[..44]) && result.contains("procédure (T-004)"));
        assert!(remove_link(NOTE, |name| name == "zzz").is_err());
    }
}
