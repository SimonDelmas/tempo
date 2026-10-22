//! Index SQLite du vault : métadonnées, liens et recherche plein texte.
//!
//! L'index est un cache : il est reconstruit au démarrage et tenu à jour par le watcher.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use regex::Regex;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row};
use serde::Serialize;
use serde_json::Value;
use unicode_normalization::UnicodeNormalization;

use crate::error::Result;
use crate::vault::{content_hash, split_frontmatter, truthy, value_str, Meta, Vault};

static WIKILINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[\[([^\]\|#\n]+)(?:#[^\]\|\n]*)?(?:\|[^\]\n]*)?\]\]").unwrap());
static DATE_PREFIX_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\d{4}-\d{2}-\d{2})").unwrap());
static TITLE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^#[ \t]+(.+?)\s*$").unwrap());
static WORD_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\w+").unwrap());

pub const FOLDER_TYPES: [(&str, &str); 16] = [
    ("00-inbox", "inbox"),
    ("journal", "journal"),
    ("reunions", "reunion"),
    ("echanges", "echange"),
    ("taches", "tache"),
    ("projets", "projet"),
    ("sujets", "sujet"),
    ("themes", "theme"),
    ("personnes", "personne"),
    ("ressources", "ressource"),
    ("formations", "formation"),
    ("syntheses", "synthese"),
    ("rapports", "rapport"),
    ("comptes", "compte"),
    ("imputations", "imputation"),
    ("_templates", "modele"),
];

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS notes(
    path TEXT PRIMARY KEY, stem TEXT, alias TEXT, title TEXT, type TEXT,
    date TEXT, meta TEXT, hash TEXT, mtime REAL
);
CREATE TABLE IF NOT EXISTS links(src TEXT, target TEXT);
CREATE INDEX IF NOT EXISTS links_target ON links(target);
CREATE INDEX IF NOT EXISTS links_src ON links(src);
CREATE VIRTUAL TABLE IF NOT EXISTS notes_fts USING fts5(
    path UNINDEXED, title, body, tokenize='unicode61 remove_diacritics 2'
);
";

/// Cibles des liens `[[…]]` d'un texte, sans doublon, triées.
pub fn extract_links(text: &str) -> Vec<String> {
    let mut links: Vec<String> = WIKILINK_RE.captures_iter(text).map(|c| c[1].trim().to_string()).collect();
    links.sort();
    links.dedup();
    links
}

pub fn note_title(meta: &Meta, body: &str, stem: &str) -> String {
    for key in ["title", "subject"] {
        if truthy(meta.get(key)) {
            return value_str(&meta[key]);
        }
    }
    TITLE_RE.captures(body).map(|c| c[1].trim().to_string()).unwrap_or_else(|| stem.to_string())
}

pub fn infer_type(rel: &str) -> &'static str {
    match rel.split_once('/') {
        Some((first, _)) => FOLDER_TYPES.iter().find(|(f, _)| *f == first).map(|(_, t)| *t).unwrap_or("note"),
        None => "note",
    }
}

/// Note telle qu'indexée.
#[derive(Debug, Clone, Serialize)]
pub struct NoteRow {
    pub path: String,
    pub stem: String,
    pub alias: String,
    pub title: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub date: String,
    pub meta: Meta,
    pub hash: String,
    pub mtime: f64,
}

impl NoteRow {
    fn from_row(row: &Row) -> rusqlite::Result<Self> {
        let meta: String = row.get("meta")?;
        Ok(NoteRow {
            path: row.get("path")?,
            stem: row.get("stem")?,
            alias: row.get("alias")?,
            title: row.get("title")?,
            kind: row.get("type")?,
            date: row.get("date")?,
            meta: serde_json::from_str(&meta).unwrap_or_default(),
            hash: row.get("hash")?,
            mtime: row.get("mtime")?,
        })
    }

    /// Modèle de note (`_templates/`), quel que soit le `type` de son frontmatter.
    pub fn is_template(&self) -> bool {
        self.kind == "modele" || infer_type(&self.path) == "modele"
    }

    /// Nom utilisé dans les liens : l'alias s'il existe, sinon le nom du fichier.
    pub fn link_name(&self) -> &str {
        if self.alias.is_empty() { &self.stem } else { &self.alias }
    }

    /// Résumé renvoyé par l'API (liste des fichiers, liens).
    pub fn summary(&self) -> Value {
        serde_json::json!({"path": self.path, "stem": self.stem, "alias": self.alias,
                           "title": self.title, "type": self.kind, "date": self.date})
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub path: String,
    pub title: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub date: String,
    pub snippet: String,
}

/// Ligne d'une note trouvée par [`Index::grep`] (numérotée à partir de 1).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GrepHit {
    pub path: String,
    pub line: usize,
    pub text: String,
}

/// Combinaison des mots d'une recherche plein texte.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SearchMode {
    /// tous les mots, en préfixe
    Words,
    /// les mots dans l'ordre, côte à côte
    Phrase,
    /// l'un des mots au moins
    Any,
}

/// Texte comparable sans casse ni accents.
pub fn fold(text: &str) -> String {
    text.trim().nfkd().filter(|c| !('\u{300}'..='\u{36f}').contains(c)).collect::<String>().to_lowercase()
}

pub struct Index {
    pub vault: Arc<Vault>,
    conn: Mutex<Connection>,
    revision: AtomicU64,
    watcher: Mutex<Option<RecommendedWatcher>>,
}

impl Index {
    pub fn new(vault: Arc<Vault>, db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(db_path)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Index { vault, conn: Mutex::new(conn), revision: AtomicU64::new(0), watcher: Mutex::new(None) })
    }

    /// Compteur incrémenté à chaque modification (le frontend s'en sert pour se rafraîchir).
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::SeqCst)
    }

    fn bump(&self) {
        self.revision.fetch_add(1, Ordering::SeqCst);
    }

    /// Signale au frontend un changement hors des notes (bibliothèque de documents).
    pub fn touch(&self) {
        self.bump();
    }

    // --- écriture -----------------------------------------------------------

    pub fn rebuild(&self) -> Result<usize> {
        let files = self.vault.list_files();
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute_batch("DELETE FROM notes; DELETE FROM links; DELETE FROM notes_fts;")?;
        for rel in &files {
            // un fichier illisible (encodage…) n'empêche pas l'indexation des autres
            let _ = self.upsert_in(&tx, rel);
        }
        tx.commit()?;
        drop(conn);
        self.bump();
        Ok(files.len())
    }

    pub fn upsert(&self, rel: &str) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        self.upsert_in(&tx, rel)?;
        tx.commit()?;
        drop(conn);
        self.bump();
        Ok(())
    }

    pub fn remove(&self, rel: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        Self::delete_in(&conn, rel)?;
        drop(conn);
        self.bump();
        Ok(())
    }

    fn delete_in(conn: &Connection, rel: &str) -> rusqlite::Result<()> {
        conn.execute("DELETE FROM notes WHERE path = ?", [rel])?;
        conn.execute("DELETE FROM links WHERE src = ?", [rel])?;
        conn.execute("DELETE FROM notes_fts WHERE path = ?", [rel])?;
        Ok(())
    }

    fn upsert_in(&self, conn: &Connection, rel: &str) -> Result<()> {
        let path = self.vault.resolve(rel)?;
        let text = std::fs::read_to_string(&path)?;
        let (meta, body) = split_frontmatter(&text);
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let date_source = if truthy(meta.get("date")) {
            value_str(&meta["date"])
        } else {
            DATE_PREFIX_RE.captures(&stem).map(|c| c[1].to_string()).unwrap_or_default()
        };
        let date: String = date_source.chars().take(10).collect();
        let alias = ["id", "alias"].iter().find(|k| truthy(meta.get(**k))).map(|k| value_str(&meta[*k])).unwrap_or_default();
        let kind = if truthy(meta.get("type")) { value_str(&meta["type"]) } else { infer_type(rel).to_string() };
        let title = note_title(&meta, body, &stem);
        let mtime = std::fs::metadata(&path)?
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);

        Self::delete_in(conn, rel)?;
        conn.execute(
            "INSERT INTO notes VALUES (?,?,?,?,?,?,?,?,?)",
            params![rel, stem, alias, title, kind, date, serde_json::to_string(&meta).unwrap(), content_hash(&text), mtime],
        )?;
        for target in extract_links(&text) {
            conn.execute("INSERT INTO links VALUES (?,?)", params![rel, target.to_lowercase()])?;
        }
        conn.execute("INSERT INTO notes_fts VALUES (?,?,?)", params![rel, title, body])?;
        Ok(())
    }

    /// Appelé par le watcher pour un fichier créé / modifié / supprimé.
    pub fn sync_path(&self, path: &Path) {
        if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("md")) || self.vault.is_ignored(path) {
            return;
        }
        let Some(rel) = self.vault.rel(path) else { return };
        let _ = if path.is_file() { self.upsert(&rel) } else { self.remove(&rel) };
    }

    // --- lecture ------------------------------------------------------------

    fn query(&self, sql: &str, params: impl rusqlite::Params) -> Result<Vec<NoteRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(params, NoteRow::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn get(&self, rel: &str) -> Result<Option<NoteRow>> {
        Ok(self.query("SELECT * FROM notes WHERE path = ?", [rel])?.into_iter().next())
    }

    pub fn list_notes(&self, kind: Option<&str>, start: Option<&str>, end: Option<&str>) -> Result<Vec<NoteRow>> {
        let mut sql = String::from("SELECT * FROM notes WHERE 1=1");
        let mut values: Vec<&str> = Vec::new();
        if let Some(kind) = kind.filter(|k| !k.is_empty()) {
            sql.push_str(" AND type = ?");
            values.push(kind);
        }
        if let Some(start) = start.filter(|s| !s.is_empty()) {
            sql.push_str(" AND date >= ? AND date != ''");
            values.push(start);
        }
        if let Some(end) = end.filter(|s| !s.is_empty()) {
            sql.push_str(" AND date <= ? AND date != ''");
            values.push(end);
        }
        sql.push_str(" ORDER BY date, path");
        self.query(&sql, params_from_iter(values))
    }

    pub fn all_notes(&self) -> Result<Vec<NoteRow>> {
        self.list_notes(None, None, None)
    }

    pub fn by_type(&self, kind: &str) -> Result<Vec<NoteRow>> {
        self.list_notes(Some(kind), None, None)
    }

    pub fn modified_between(&self, start_ts: f64, end_ts: f64) -> Result<Vec<NoteRow>> {
        self.query("SELECT * FROM notes WHERE mtime >= ? AND mtime <= ? ORDER BY mtime", params![start_ts, end_ts])
    }

    /// Chemin de la note désignée par un lien `[[nom]]` (alias, nom de fichier ou titre).
    pub fn resolve_link(&self, name: &str) -> Result<Option<String>> {
        let key = name.trim().to_lowercase();
        let conn = self.conn.lock().unwrap();
        for column in ["alias", "stem", "title"] {
            let path: Option<String> = conn
                .query_row(&format!("SELECT path FROM notes WHERE lower({column}) = ? LIMIT 1"), [&key], |r| r.get(0))
                .optional()?;
            if path.is_some() {
                return Ok(path);
            }
        }
        Ok(None)
    }

    pub fn backlinks(&self, rel: &str) -> Result<Vec<NoteRow>> {
        let Some(note) = self.get(rel)? else { return Ok(Vec::new()) };
        let keys: Vec<String> =
            [&note.stem, &note.alias, &note.title].iter().filter(|k| !k.is_empty()).map(|k| k.to_lowercase()).collect();
        let marks = vec!["?"; keys.len()].join(",");
        let mut values = keys;
        values.push(rel.to_string());
        self.query(
            &format!(
                "SELECT DISTINCT n.* FROM links l JOIN notes n ON n.path = l.src \
                 WHERE l.target IN ({marks}) AND l.src != ? ORDER BY n.date DESC, n.path"
            ),
            params_from_iter(values),
        )
    }

    /// Notes qui contiennent un lien `[[…]]` vers l'un de ces noms (sans casse), les plus
    /// récentes d'abord : par exemple les synthèses qui citent un document de la bibliothèque.
    pub fn linking_notes(&self, names: &[&str]) -> Result<Vec<NoteRow>> {
        let keys: Vec<String> = names.iter().map(|n| n.trim().to_lowercase()).filter(|n| !n.is_empty()).collect();
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let marks = vec!["?"; keys.len()].join(",");
        self.query(
            &format!("SELECT DISTINCT n.* FROM links l JOIN notes n ON n.path = l.src WHERE l.target IN ({marks}) ORDER BY n.date DESC, n.path"),
            params_from_iter(keys),
        )
    }

    pub fn outlinks(&self, rel: &str) -> Result<Vec<NoteRow>> {
        let targets: Vec<String> = {
            let conn = self.conn.lock().unwrap();
            let mut stmt = conn.prepare("SELECT DISTINCT target FROM links WHERE src = ?")?;
            stmt.query_map([rel], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
        };
        let mut result: Vec<NoteRow> = Vec::new();
        for target in targets {
            if let Some(path) = self.resolve_link(&target)?
                && !result.iter().any(|n| n.path == path)
                    && let Some(note) = self.get(&path)? {
                        result.push(note);
                    }
        }
        Ok(result)
    }

    pub fn find_by_meta(&self, key: &str, value: &str) -> Result<Vec<NoteRow>> {
        self.query("SELECT * FROM notes WHERE json_extract(meta, ?) = ?", params![format!("$.{key}"), value])
    }

    /// Notes dont le nom de fichier, l'alias ou le titre contient `name`, d'un type donné, et
    /// dont le frontmatter a les valeurs demandées (pour une liste : l'un de ses éléments). Les
    /// comparaisons ignorent la casse et les accents ; un lien `[[x]]` vaut `x`.
    pub fn find_notes(&self, name: Option<&str>, kind: Option<&str>, meta: &[(String, String)]) -> Result<Vec<NoteRow>> {
        let name = name.map(fold).filter(|n| !n.is_empty());
        let wanted: Vec<(&str, String)> = meta.iter().map(|(k, v)| (k.as_str(), fold(v))).collect();
        let matches = |value: &Value, expected: &str| -> bool {
            let same = |v: &Value| {
                let text = value_str(v);
                fold(&text) == expected || extract_links(&text).iter().any(|link| fold(link) == expected)
            };
            match value {
                Value::Array(items) => items.iter().any(same),
                other => same(other),
            }
        };
        Ok(self
            .list_notes(kind, None, None)?
            .into_iter()
            .filter(|n| !n.is_template())
            .filter(|n| name.as_ref().is_none_or(|name| [&n.stem, &n.alias, &n.title].iter().any(|s| fold(s).contains(name))))
            .filter(|n| wanted.iter().all(|(key, expected)| n.meta.get(*key).is_some_and(|v| matches(v, expected))))
            .collect())
    }

    /// Tags des notes (liste `tags` du frontmatter, `#` retiré) avec leur nombre d'occurrences.
    pub fn tag_counts(&self) -> Result<Vec<(String, usize)>> {
        let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for note in self.all_notes()?.iter().filter(|n| !n.is_template()) {
            let tags = match note.meta.get("tags") {
                Some(Value::Array(items)) => items.iter().map(value_str).collect(),
                Some(Value::String(s)) => s.split([',', ' ']).map(String::from).collect(),
                _ => Vec::new(),
            };
            for tag in tags {
                let tag = tag.trim().trim_start_matches('#').to_string();
                if !tag.is_empty() {
                    *counts.entry(tag).or_default() += 1;
                }
            }
        }
        Ok(counts.into_iter().collect())
    }

    /// Lignes des notes (frontmatter compris) qui correspondent à `pattern`, dans l'ordre des
    /// notes ; `folder` restreint aux chemins qui commencent par ce dossier.
    pub fn grep(&self, pattern: &Regex, kind: Option<&str>, folder: Option<&str>, limit: usize) -> Result<Vec<GrepHit>> {
        let folder = folder.map(|f| format!("{}/", f.trim().trim_matches('/'))).filter(|f| f != "/");
        let mut hits = Vec::new();
        for note in self.list_notes(kind, None, None)? {
            if note.is_template() || folder.as_ref().is_some_and(|f| !note.path.starts_with(f.as_str())) {
                continue;
            }
            let Ok(text) = self.vault.read_text(&note.path) else { continue };
            for (i, line) in text.lines().enumerate() {
                if pattern.is_match(line) {
                    hits.push(GrepHit { path: note.path.clone(), line: i + 1, text: line.trim_end().to_string() });
                    if hits.len() >= limit {
                        return Ok(hits);
                    }
                }
            }
        }
        Ok(hits)
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        self.search_with(query, SearchMode::Words, limit)
    }

    /// Recherche plein texte (titre et corps) : tous les mots (en préfixe), la phrase exacte,
    /// ou l'un des mots.
    pub fn search_with(&self, query: &str, mode: SearchMode, limit: usize) -> Result<Vec<SearchHit>> {
        let words: Vec<&str> = WORD_RE.find_iter(query).map(|m| m.as_str()).collect();
        if words.is_empty() {
            return Ok(Vec::new());
        }
        let fts = match mode {
            SearchMode::Words => words.iter().map(|w| format!("\"{w}\"*")).collect::<Vec<_>>().join(" "),
            SearchMode::Phrase => format!("\"{}\"", words.join(" ")),
            SearchMode::Any => words.iter().map(|w| format!("\"{w}\"*")).collect::<Vec<_>>().join(" OR "),
        };
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT n.path, n.title, n.type, n.date, \
             snippet(notes_fts, 2, '«', '»', '…', 12) AS snippet \
             FROM notes_fts JOIN notes n ON n.path = notes_fts.path \
             WHERE notes_fts MATCH ? ORDER BY rank LIMIT ?",
        )?;
        let hits = stmt
            .query_map(params![fts, limit as i64], |r| {
                Ok(SearchHit { path: r.get(0)?, title: r.get(1)?, kind: r.get(2)?, date: r.get(3)?, snippet: r.get(4)? })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(hits)
    }

    // --- surveillance du dossier --------------------------------------------

    pub fn start_watching(self: &Arc<Self>) -> Result<()> {
        let index = Arc::downgrade(self);
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            let (Ok(event), Some(index)) = (event, index.upgrade()) else { return };
            for path in &event.paths {
                if !path.is_dir() {
                    index.sync_path(path);
                }
            }
        })
        .map_err(|e| crate::error::Error::Internal(format!("Surveillance du vault impossible : {e}")))?;
        watcher
            .watch(&self.vault.root, RecursiveMode::Recursive)
            .map_err(|e| crate::error::Error::Internal(format!("Surveillance du vault impossible : {e}")))?;
        *self.watcher.lock().unwrap() = Some(watcher);
        Ok(())
    }

    pub fn stop_watching(&self) {
        self.watcher.lock().unwrap().take();
    }
}
