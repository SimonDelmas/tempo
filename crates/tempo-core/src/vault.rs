//! Accès au vault : lecture/écriture des fichiers Markdown et du frontmatter.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;
use yaml_rust2::{Yaml, YamlLoader};

use crate::error::{not_found, vault_err, Error, Result};

pub const IGNORED_DIRS: [&str; 5] = [".git", ".tempo", ".obsidian", ".trash", "node_modules"];

/// Frontmatter YAML d'une note, dans l'ordre des clés du fichier.
pub type Meta = Map<String, Value>;

static FRONTMATTER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)\A---\r?\n(.*?)\r?\n---[ \t]*(?:\r?\n|\z)").unwrap());
static HEADING_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(#{1,6})\s").unwrap());
static NON_ALNUM_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^a-zA-Z0-9]+").unwrap());

pub fn content_hash(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    hex::encode(digest)[..16].to_string()
}

pub fn slugify(text: &str, max_len: usize) -> String {
    let ascii: String = text.nfkd().filter(char::is_ascii).collect();
    let slug = NON_ALNUM_RE.replace_all(&ascii, "-").trim_matches('-').to_lowercase();
    let cut: String = slug.chars().take(max_len).collect();
    let cut = cut.trim_matches('-');
    if cut.is_empty() { "note".into() } else { cut.into() }
}

// --- valeurs du frontmatter ------------------------------------------------------

/// Valeur « vraie » au sens de Python (`x or y`) : ni nulle, ni vide, ni zéro.
pub fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// Texte d'une valeur scalaire (`str(x)` en Python, sans les guillemets du JSON).
pub fn value_str(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        other => other.to_string(),
    }
}

/// Texte d'une clé si elle est renseignée (`str(meta[k]) if meta.get(k) else None`).
pub fn meta_str(meta: &Meta, key: &str) -> Option<String> {
    let value = meta.get(key);
    truthy(value).then(|| value_str(value.unwrap()))
}

pub fn meta_f64(meta: &Meta, key: &str) -> f64 {
    match meta.get(key) {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// Nombre JSON, entier quand c'est possible (`8` plutôt que `8.0`).
pub fn number(value: f64) -> Value {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        Value::from(value as i64)
    } else {
        serde_json::Number::from_f64(value).map(Value::Number).unwrap_or(Value::Null)
    }
}

fn yaml_to_json(yaml: Yaml) -> Value {
    match yaml {
        Yaml::Real(s) => s
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .unwrap_or(Value::String(s)),
        Yaml::Integer(i) => Value::from(i),
        Yaml::String(s) => Value::String(s),
        Yaml::Boolean(b) => Value::Bool(b),
        Yaml::Array(items) => Value::Array(items.into_iter().map(yaml_to_json).collect()),
        Yaml::Hash(hash) => {
            let mut map = Map::new();
            for (k, v) in hash {
                let key = match yaml_to_json(k) {
                    Value::String(s) => s,
                    other => value_str(&other),
                };
                map.insert(key, yaml_to_json(v));
            }
            Value::Object(map)
        }
        Yaml::Alias(_) | Yaml::Null | Yaml::BadValue => Value::Null,
    }
}

/// Sépare le frontmatter du corps ; un frontmatter illisible est traité comme du texte.
pub fn split_frontmatter(text: &str) -> (Meta, &str) {
    let Some(caps) = FRONTMATTER_RE.captures(text) else {
        return (Meta::new(), text);
    };
    let end = caps.get(0).unwrap().end();
    match YamlLoader::load_from_str(&caps[1]) {
        Ok(docs) => match docs.into_iter().next().map(yaml_to_json) {
            Some(Value::Object(map)) => (map, &text[end..]),
            None | Some(Value::Null) => (Meta::new(), &text[end..]),
            Some(_) => (Meta::new(), text),
        },
        Err(_) => (Meta::new(), text),
    }
}

// --- écriture YAML (style PyYAML : clés en bloc, listes simples en ligne) -----------

static YAML_NUMBER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?:[-+]?(?:0b[01_]+|0o?[0-7_]+|0x[0-9a-fA-F_]+|[0-9][0-9_]*(?:\.[0-9_]*)?(?:[eE][-+]?[0-9]+)?|\.[0-9_]+(?:[eE][-+]?[0-9]+)?|[0-9][0-9_]*(?::[0-5]?[0-9])+(?:\.[0-9_]*)?|\.(?:inf|Inf|INF))|\.(?:nan|NaN|NAN))$",
    )
    .unwrap()
});
const YAML_KEYWORDS: [&str; 22] = [
    "yes", "Yes", "YES", "no", "No", "NO", "true", "True", "TRUE", "false",
    "False", "FALSE", "on", "On", "ON", "off", "Off", "OFF", "null", "Null", "NULL", "~",
];

fn plain_allowed(s: &str, flow: bool) -> bool {
    let Some(first) = s.chars().next() else { return false };
    if s.trim() != s || s.chars().any(|c| c.is_control()) {
        return false;
    }
    if ",[]{}#&*!|>'\"%@`".contains(first) {
        return false;
    }
    let second = s.chars().nth(1);
    if "-?:".contains(first) && second.is_none_or(|c| c == ' ') {
        return false;
    }
    if s.contains(": ") || s.contains(" #") || s.ends_with(':') {
        return false;
    }
    if flow && s.chars().any(|c| ",[]{}".contains(c)) {
        return false;
    }
    // les dates restent sans guillemets : relues, elles redeviennent des dates
    !(YAML_KEYWORDS.contains(&s) || YAML_NUMBER_RE.is_match(s))
}

fn yaml_scalar(value: &Value, flow: bool) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => i.to_string(),
            (None, Some(f)) if f.fract() == 0.0 && f.abs() < 1e15 => (f as i64).to_string(),
            _ => n.to_string(),
        },
        Value::String(s) if plain_allowed(s, flow) => s.clone(),
        Value::String(s) if !s.chars().any(|c| c.is_control()) => format!("'{}'", s.replace('\'', "''")),
        Value::String(s) => serde_json::to_string(s).unwrap(),
        Value::Array(items) if items.is_empty() => "[]".into(),
        Value::Object(map) if map.is_empty() => "{}".into(),
        Value::Array(items) => {
            format!("[{}]", items.iter().map(|v| yaml_scalar(v, true)).collect::<Vec<_>>().join(", "))
        }
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter()
                .map(|(k, v)| format!("{}: {}", yaml_scalar(&Value::String(k.clone()), true), yaml_scalar(v, true)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn is_simple(value: &Value) -> bool {
    !matches!(value, Value::Array(a) if !a.is_empty()) && !matches!(value, Value::Object(o) if !o.is_empty())
}

fn emit_mapping(out: &mut String, map: &Meta, indent: usize) {
    for (key, value) in map {
        let pad = " ".repeat(indent);
        let key = yaml_scalar(&Value::String(key.clone()), false);
        match value {
            Value::Object(inner) if !inner.is_empty() => {
                out.push_str(&format!("{pad}{key}:\n"));
                emit_mapping(out, inner, indent + 2);
            }
            Value::Array(items) if !items.iter().all(is_simple) => {
                out.push_str(&format!("{pad}{key}:\n"));
                emit_sequence(out, items, indent);
            }
            other => out.push_str(&format!("{pad}{key}: {}\n", yaml_scalar(other, false))),
        }
    }
}

fn emit_sequence(out: &mut String, items: &[Value], indent: usize) {
    let pad = " ".repeat(indent);
    for item in items {
        match item {
            Value::Object(inner) if !inner.is_empty() => {
                let mut nested = String::new();
                emit_mapping(&mut nested, inner, indent + 2);
                out.push_str(&format!("{pad}- {}", &nested[indent + 2..]));
            }
            other => out.push_str(&format!("{pad}- {}\n", yaml_scalar(other, true))),
        }
    }
}

pub fn dump_yaml(meta: &Meta) -> String {
    let mut out = String::new();
    emit_mapping(&mut out, meta, 0);
    out
}

pub fn join_frontmatter(meta: &Meta, body: &str) -> String {
    if meta.is_empty() {
        return body.to_string();
    }
    format!("---\n{}\n---\n{body}", dump_yaml(meta).trim())
}

// --- sections Markdown -------------------------------------------------------------

fn heading_level(heading: &str) -> usize {
    heading.len() - heading.trim_start_matches('#').len()
}

fn ends_section(line: &str, level: usize) -> bool {
    HEADING_RE.captures(line).is_some_and(|m| m[1].len() <= level)
}

/// Ajoute `line` à la fin de la section `heading` (créée si absente).
pub fn append_to_section(text: &str, heading: &str, line: &str) -> String {
    let mut lines: Vec<String> = text.trim_end_matches('\n').split('\n').map(String::from).collect();
    let Some(start) = lines.iter().position(|l| l.trim() == heading) else {
        return format!("{}\n\n{heading}\n{line}\n", text.trim_end_matches('\n'));
    };
    let level = heading_level(heading);
    let end = (start + 1..lines.len()).find(|&i| ends_section(&lines[i], level)).unwrap_or(lines.len());
    let mut insert_at = end;
    while insert_at - 1 > start && lines[insert_at - 1].trim().is_empty() {
        insert_at -= 1;
    }
    lines.insert(insert_at, line.to_string());
    if insert_at + 1 < lines.len() && !lines[insert_at + 1].trim().is_empty() {
        lines.insert(insert_at + 1, String::new());
    }
    lines.join("\n") + "\n"
}

pub fn section_lines<'a>(text: &'a str, heading: &str) -> Vec<&'a str> {
    let lines: Vec<&str> = text.split('\n').collect();
    let Some(start) = lines.iter().position(|l| l.trim() == heading) else {
        return Vec::new();
    };
    let level = heading_level(heading);
    lines[start + 1..].iter().take_while(|l| !ends_section(l, level)).copied().collect()
}

/// Retire une section (ex. le texte original d'un échange) pour alléger un contexte.
pub fn strip_section(text: &str, heading: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let Some(start) = lines.iter().position(|l| l.trim() == heading) else {
        return text.to_string();
    };
    let kept = section_lines(text, heading).len();
    let mut result: Vec<&str> = lines[..start].to_vec();
    result.extend_from_slice(&lines[start + 1 + kept..]);
    result.join("\n")
}

// --- vault ---------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct NoteContent {
    pub path: String,
    pub content: String,
    pub hash: String,
    pub meta: Meta,
}

pub struct Vault {
    pub root: PathBuf,
}

impl Vault {
    pub fn new(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref();
        fs::create_dir_all(root)?;
        Ok(Vault { root: dunce::canonicalize(root)? })
    }

    /// Chemin absolu d'une note, refusé s'il sort du vault ou n'est pas un `.md`.
    pub fn resolve(&self, rel: &str) -> Result<PathBuf> {
        let rel = rel.replace('\\', "/");
        let mut parts: Vec<&str> = Vec::new();
        for part in rel.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    parts.pop().ok_or_else(|| vault_err("Chemin hors du vault"))?;
                }
                p if p.contains(':') => return Err(vault_err("Chemin hors du vault")),
                p => parts.push(p),
            }
        }
        let is_md = parts.last().is_some_and(|name| {
            Path::new(name).extension().is_some_and(|e| e.eq_ignore_ascii_case("md"))
        });
        if !is_md {
            return Err(vault_err("Seuls les fichiers .md sont gérés"));
        }
        Ok(parts.iter().fold(self.root.clone(), |path, part| path.join(part)))
    }

    /// Chemin relatif (séparateurs `/`) d'un fichier du vault.
    pub fn rel(&self, path: &Path) -> Option<String> {
        let relative = path.strip_prefix(&self.root).ok()?;
        let parts: Vec<String> = relative.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
        Some(parts.join("/"))
    }

    pub fn is_ignored(&self, path: &Path) -> bool {
        let Ok(relative) = path.strip_prefix(&self.root) else { return true };
        let parts: Vec<_> = relative.components().collect();
        parts.iter().take(parts.len().saturating_sub(1)).any(|p| {
            let name = p.as_os_str().to_string_lossy();
            IGNORED_DIRS.contains(&name.as_ref()) || name.starts_with('.')
        })
    }

    pub fn list_files(&self) -> Vec<String> {
        let mut files: Vec<String> = walkdir::WalkDir::new(&self.root)
            .into_iter()
            .filter_entry(|e| {
                e.depth() == 0 || !e.file_type().is_dir() || {
                    let name = e.file_name().to_string_lossy();
                    !(IGNORED_DIRS.contains(&name.as_ref()) || name.starts_with('.'))
                }
            })
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("md")))
            .filter_map(|e| self.rel(e.path()))
            .collect();
        files.sort();
        files
    }

    pub fn exists(&self, rel: &str) -> bool {
        self.resolve(rel).is_ok_and(|p| p.is_file())
    }

    pub fn read_text(&self, rel: &str) -> Result<String> {
        let path = self.resolve(rel)?;
        if !path.is_file() {
            return Err(not_found(format!("Note introuvable : {rel}")));
        }
        fs::read_to_string(&path)
            .map_err(|e| Error::Internal(format!("Lecture impossible de {rel} : {e}")))
    }

    pub fn read(&self, rel: &str) -> Result<NoteContent> {
        let content = self.read_text(rel)?;
        let (meta, _) = split_frontmatter(&content);
        Ok(NoteContent { path: rel.to_string(), hash: content_hash(&content), meta, content })
    }

    /// Écrit une note ; `expected_hash` détecte une modification faite ailleurs entre-temps.
    pub fn write(&self, rel: &str, content: &str, expected_hash: Option<&str>, create_only: bool) -> Result<String> {
        let path = self.resolve(rel)?;
        if path.exists() {
            if create_only {
                return Err(vault_err(format!("Le fichier existe déjà : {rel}")));
            }
            if let Some(expected) = expected_hash {
                let current = fs::read_to_string(&path)?;
                if content_hash(&current) != expected {
                    return Err(Error::Conflict { current_content: current });
                }
            }
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, content)?;
        Ok(content_hash(content))
    }

    /// Déplace le fichier dans .trash/ (récupérable).
    pub fn delete(&self, rel: &str) -> Result<()> {
        let path = self.resolve(rel)?;
        if !path.is_file() {
            return Err(not_found(format!("Note introuvable : {rel}")));
        }
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
        let relative = path.strip_prefix(&self.root).map_err(|_| vault_err("Chemin hors du vault"))?;
        let target = self.root.join(".trash").join(stamp).join(relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        if fs::rename(&path, &target).is_err() {
            fs::copy(&path, &target)?;
            fs::remove_file(&path)?;
        }
        Ok(())
    }

    pub fn unique_path(&self, folder: &str, stem: &str) -> Result<String> {
        let folder = folder.trim_matches('/');
        let make = |suffix: String| {
            if folder.is_empty() { format!("{stem}{suffix}.md") } else { format!("{folder}/{stem}{suffix}.md") }
        };
        let mut candidate = make(String::new());
        let mut n = 2;
        while self.resolve(&candidate)?.exists() {
            candidate = make(format!("-{n}"));
            n += 1;
        }
        Ok(candidate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn frontmatter_roundtrip() {
        let text = "---\ntype: tache\nstatus: en_cours\n---\n\nCorps\n";
        let (meta, body) = split_frontmatter(text);
        assert_eq!(Value::Object(meta.clone()), json!({"type": "tache", "status": "en_cours"}));
        assert_eq!(join_frontmatter(&meta, body), text);
    }

    #[test]
    fn yaml_style_matches_pyyaml() {
        let meta = json!({
            "date": "2026-10-17", "tags": ["a", "b"], "tasks": ["[[T-005]]"], "empty": [],
            "subject": "Accès : prod", "n": 8, "half": 2.5, "whole": 40.0, "num": "123",
            "hours": {"2026-10-12": {"SAP-MIG": 6}}, "quote": "l'été", "multi": "a\nb",
            "list": [{"a": 1, "b": "x"}]
        });
        let Value::Object(meta) = meta else { unreachable!() };
        assert_eq!(
            dump_yaml(&meta),
            "date: 2026-10-17\ntags: [a, b]\ntasks: ['[[T-005]]']\nempty: []\nsubject: 'Accès : prod'\n\
             n: 8\nhalf: 2.5\nwhole: 40\nnum: '123'\nhours:\n  2026-10-12:\n    SAP-MIG: 6\n\
             quote: l'été\nmulti: \"a\\nb\"\nlist:\n- a: 1\n  b: x\n"
        );
        let (parsed, _) = split_frontmatter(&join_frontmatter(&meta, ""));
        assert_eq!(parsed, meta.into_iter().map(|(k, v)| (k, if v == json!(40.0) { json!(40) } else { v })).collect::<Meta>());
    }

    #[test]
    fn append_to_section_inserts_before_next_heading() {
        let text = "# T\n\n## Échanges\n- a\n\n## Fait aujourd'hui\n";
        assert_eq!(
            append_to_section(text, "## Échanges", "- b"),
            "# T\n\n## Échanges\n- a\n- b\n\n## Fait aujourd'hui\n"
        );
    }

    #[test]
    fn append_to_section_creates_missing_section() {
        assert_eq!(append_to_section("Corps\n", "## Historique", "- x"), "Corps\n\n## Historique\n- x\n");
    }

    #[test]
    fn slugify_strips_accents() {
        assert_eq!(slugify("Accès production SAP", 40), "acces-production-sap");
        assert_eq!(slugify("  ", 40), "note");
    }
}
