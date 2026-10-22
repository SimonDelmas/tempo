//! Recherche sémantique : embeddings des notes dans sqlite-vec (<vault>/.tempo/vectors.db).
//!
//! Comme l'index plein texte, c'est un cache reconstructible : chaque note est découpée en
//! passages, vectorisés par une API d'embeddings compatible OpenAI ; une note n'est revectorisée
//! que si son contenu a changé (hash de l'index). Sans modèle d'embeddings configuré
//! (TEMPO_EMBED_MODEL), la fonctionnalité est simplement désactivée.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, Once};
use std::thread::JoinHandle;
use std::time::Duration;

use indexmap::IndexMap;
use regex::Regex;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{json, Value};

use crate::config::Settings;
use crate::error::{Error, Result};
use crate::index::{Index, NoteRow};
use crate::llm::{http_agent, post_json};
use crate::util::head;
use crate::vault::{split_frontmatter, strip_section, Vault};

pub const CHUNK_CHARS: usize = 1200;
const MAX_CHUNKS_PER_NOTE: usize = 20;
const BATCH_SIZE: usize = 32;
const NOTES_PER_BATCH: usize = 8;
const SKIPPED_TYPES: [&str; 2] = ["modele", "inbox"];

static PARAGRAPH_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n\s*\n").unwrap());
static WORD3_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\w{3,}").unwrap());

/// Rôle des textes à vectoriser : certains modèles (e5) les préfixent différemment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbedKind {
    /// question ou recherche de l'utilisateur
    Query,
    /// passage d'une note ou d'un document
    Passage,
}

/// Vectorise une liste de textes (un vecteur par texte, dans l'ordre).
pub type Embedder = Arc<dyn Fn(&[String], EmbedKind) -> Result<Vec<Vec<f32>>> + Send + Sync>;

pub fn openai_embedder(settings: &Settings) -> Result<Embedder> {
    let agent = http_agent(settings.llm_ca_bundle.as_deref(), Duration::from_secs(120))?;
    let base = settings.embed_base_url.clone().unwrap_or_default().trim_end_matches('/').to_string();
    let key = settings.embed_api_key.clone().unwrap_or_default();
    let model = settings.embed_model.clone().unwrap_or_default();
    Ok(Arc::new(move |texts: &[String], _: EmbedKind| {
        let body = json!({"model": model, "input": texts});
        let response = post_json(&agent, &format!("{base}/embeddings"), &key, &body, "d'embeddings")?;
        let text = response
            .into_body()
            .read_to_string()
            .map_err(|e| Error::Llm(format!("Erreur de l'API d'embeddings : {e}")))?;
        let data: Value = serde_json::from_str(&text)
            .map_err(|e| Error::Llm(format!("Erreur de l'API d'embeddings : réponse illisible ({e})")))?;
        let mut items: Vec<&Value> = data["data"].as_array().map(|a| a.iter().collect()).unwrap_or_default();
        items.sort_by_key(|d| d["index"].as_u64().unwrap_or(0));
        let vectors: Vec<Vec<f32>> = items
            .iter()
            .map(|d| d["embedding"].as_array().map(|v| v.iter().filter_map(|x| x.as_f64()).map(|x| x as f32).collect()).unwrap_or_default())
            .collect();
        if vectors.len() != texts.len() {
            return Err(Error::Llm("Erreur de l'API d'embeddings : nombre de vecteurs inattendu".into()));
        }
        Ok(vectors)
    }))
}

/// Embeddings factices (sac de mots haché), pour les tests.
pub fn hashing_embedder(dim: usize) -> Embedder {
    Arc::new(move |texts: &[String], _: EmbedKind| {
        Ok(texts
            .iter()
            .map(|text| {
                let mut v = vec![0f32; dim];
                for word in WORD3_RE.find_iter(&text.to_lowercase()) {
                    let digest = md5::compute(word.as_str().as_bytes());
                    let slot = digest.0.iter().fold(0usize, |acc, b| (acc * 256 + *b as usize) % dim);
                    v[slot] += 1.0;
                }
                let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
                let norm = if norm == 0.0 { 1.0 } else { norm };
                v.iter().map(|x| x / norm).collect()
            })
            .collect())
    })
}

/// Passages d'environ CHUNK_CHARS caractères, coupés entre paragraphes, titre en tête.
pub fn chunk_text(title: &str, body: &str) -> Vec<String> {
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    for paragraph in PARAGRAPH_RE.split(body).map(str::trim).filter(|p| !p.is_empty()) {
        let mut paragraph = paragraph.to_string();
        while paragraph.chars().count() > CHUNK_CHARS {
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
            }
            chunks.push(head(&paragraph, CHUNK_CHARS));
            paragraph = paragraph.chars().skip(CHUNK_CHARS).collect();
        }
        if !current.is_empty() && current.chars().count() + paragraph.chars().count() > CHUNK_CHARS {
            chunks.push(std::mem::take(&mut current));
        }
        current = if current.is_empty() { paragraph } else { format!("{current}\n\n{paragraph}") };
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    if chunks.is_empty() {
        return vec![title.to_string()];
    }
    chunks.into_iter().take(MAX_CHUNKS_PER_NOTE).map(|c| format!("{title}\n\n{c}")).collect()
}

pub(crate) fn pack(vector: &[f32]) -> Vec<u8> {
    vector.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn unpack(blob: &[u8]) -> Vec<f32> {
    blob.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect()
}

pub(crate) fn register_sqlite_vec() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        #[allow(clippy::missing_transmute_annotations)]
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute(sqlite_vec::sqlite3_vec_init as *const ())));
    });
}

#[derive(Debug, Clone, Serialize)]
pub struct SemanticHit {
    pub path: String,
    pub title: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub date: String,
    pub alias: String,
    pub stem: String,
    pub snippet: String,
    pub score: f64,
}

pub struct SemanticIndex {
    index: Arc<Index>,
    vault: Arc<Vault>,
    embedder: Option<Embedder>,
    model: Option<String>,
    error: Mutex<Option<String>>,
    conn: Option<Mutex<Connection>>,
    /// une seule synchronisation à la fois
    sync_lock: Mutex<()>,
    stop: AtomicBool,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl SemanticIndex {
    pub fn new(vault: Arc<Vault>, index: Arc<Index>, embedder: Option<Embedder>, db_path: &Path, model: Option<&str>) -> Result<Self> {
        let mut semantic = SemanticIndex {
            index,
            vault,
            embedder: None,
            model: model.map(String::from),
            error: Mutex::new(None),
            conn: None,
            sync_lock: Mutex::new(()),
            stop: AtomicBool::new(false),
            thread: Mutex::new(None),
        };
        let Some(embedder) = embedder else { return Ok(semantic) };
        register_sqlite_vec();
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(db_path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT);
             CREATE TABLE IF NOT EXISTS chunks(id INTEGER PRIMARY KEY, path TEXT, hash TEXT, text TEXT);
             CREATE INDEX IF NOT EXISTS chunks_path ON chunks(path);",
        )?;
        // changement de modèle : les vecteurs ne sont plus comparables
        let model = model.unwrap_or("");
        if Self::get_meta(&conn, "model")?.is_some_and(|m| m != model) {
            Self::reset(&conn)?;
        }
        Self::set_meta(&conn, "model", model)?;
        semantic.embedder = Some(embedder);
        semantic.conn = Some(Mutex::new(conn));
        Ok(semantic)
    }

    pub fn enabled(&self) -> bool {
        self.conn.is_some()
    }

    // --- stockage -----------------------------------------------------------

    fn get_meta(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
        conn.query_row("SELECT value FROM meta WHERE key = ?", [key], |r| r.get(0)).optional()
    }

    fn set_meta(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
        conn.execute("INSERT OR REPLACE INTO meta VALUES (?, ?)", [key, value]).map(|_| ())
    }

    fn reset(conn: &Connection) -> rusqlite::Result<()> {
        conn.execute_batch("DROP TABLE IF EXISTS vec_chunks; DELETE FROM chunks; DELETE FROM meta WHERE key = 'dim';")
    }

    fn ensure_vec_table(conn: &Connection, dim: usize) -> rusqlite::Result<()> {
        let mut current = Self::get_meta(conn, "dim")?;
        if current.as_deref().is_some_and(|d| d != dim.to_string()) {
            Self::reset(conn)?;
            current = None;
        }
        if current.is_none() {
            conn.execute_batch(&format!(
                "CREATE VIRTUAL TABLE IF NOT EXISTS vec_chunks USING vec0(embedding float[{dim}] distance_metric=cosine)"
            ))?;
            Self::set_meta(conn, "dim", &dim.to_string())?;
        }
        Ok(())
    }

    fn delete_note(conn: &Connection, path: &str) -> rusqlite::Result<()> {
        if Self::get_meta(conn, "dim")?.is_some() {
            let ids: Vec<i64> = {
                let mut stmt = conn.prepare("SELECT id FROM chunks WHERE path = ?")?;
                stmt.query_map([path], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
            };
            for id in ids {
                conn.execute("DELETE FROM vec_chunks WHERE rowid = ?", [id])?;
            }
        }
        conn.execute("DELETE FROM chunks WHERE path = ?", [path])?;
        Ok(())
    }

    fn stored_hashes(conn: &Connection) -> rusqlite::Result<HashMap<String, String>> {
        let mut stmt = conn.prepare("SELECT path, hash FROM chunks GROUP BY path")?;
        
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect()
    }

    // --- synchronisation ----------------------------------------------------

    fn notes(&self) -> Result<Vec<NoteRow>> {
        Ok(self.index.all_notes()?.into_iter().filter(|n| !SKIPPED_TYPES.contains(&n.kind.as_str())).collect())
    }

    fn note_chunks(&self, note: &NoteRow) -> Result<Vec<String>> {
        let text = self.vault.read_text(&note.path)?;
        let (_, body) = split_frontmatter(&text);
        Ok(chunk_text(&note.title, strip_section(body, "## Texte original").trim()))
    }

    /// Vectorise les notes nouvelles ou modifiées ; renvoie le nombre de notes traitées.
    pub fn sync(&self) -> usize {
        let (Some(conn), Some(embedder)) = (&self.conn, &self.embedder) else { return 0 };
        let _guard = self.sync_lock.lock().unwrap();
        let mut done = 0;
        let outcome = (|| -> Result<()> {
            let notes: IndexMap<String, NoteRow> = self.notes()?.into_iter().map(|n| (n.path.clone(), n)).collect();
            let stored = {
                let conn = conn.lock().unwrap();
                let stored = Self::stored_hashes(&conn)?;
                for path in stored.keys().filter(|p| !notes.contains_key(*p)) {
                    Self::delete_note(&conn, path)?;
                }
                stored
            };
            let todo: Vec<&NoteRow> = notes.values().filter(|n| stored.get(&n.path) != Some(&n.hash)).collect();
            for batch in todo.chunks(NOTES_PER_BATCH) {
                // appels à l'API hors verrou : la recherche reste disponible pendant la synchro
                let mut pieces: Vec<(&NoteRow, String)> = Vec::new();
                for note in batch {
                    pieces.extend(self.note_chunks(note)?.into_iter().map(|text| (*note, text)));
                }
                let mut vectors: Vec<Vec<f32>> = Vec::new();
                for group in pieces.chunks(BATCH_SIZE) {
                    let texts: Vec<String> = group.iter().map(|(_, t)| t.clone()).collect();
                    vectors.extend(embedder(&texts, EmbedKind::Passage)?);
                }
                let mut conn = conn.lock().unwrap();
                let tx = conn.transaction()?;
                if let Some(first) = vectors.first() {
                    Self::ensure_vec_table(&tx, first.len())?;
                }
                for note in batch {
                    Self::delete_note(&tx, &note.path)?;
                }
                for ((note, text), vector) in pieces.iter().zip(&vectors) {
                    tx.execute("INSERT INTO chunks(path, hash, text) VALUES (?,?,?)", params![note.path, note.hash, text])?;
                    let id = tx.last_insert_rowid();
                    tx.execute("INSERT INTO vec_chunks(rowid, embedding) VALUES (?, ?)", params![id, pack(vector)])?;
                }
                tx.commit()?;
                done += batch.len();
            }
            Ok(())
        })();
        *self.error.lock().unwrap() = outcome.err().map(|e| e.to_string());
        done
    }

    /// Synchronisation en arrière-plan, à chaque changement de l'index.
    pub fn start(self: &Arc<Self>, interval: Duration) {
        if !self.enabled() || self.thread.lock().unwrap().is_some() {
            return;
        }
        let weak = Arc::downgrade(self);
        let handle = std::thread::Builder::new()
            .name("tempo-embeddings".into())
            .spawn(move || {
                let mut last = u64::MAX;
                loop {
                    let Some(semantic) = weak.upgrade() else { return };
                    if semantic.stop.load(Ordering::SeqCst) {
                        return;
                    }
                    let revision = semantic.index.revision();
                    if revision != last {
                        last = revision;
                        semantic.sync();
                    }
                    drop(semantic);
                    let steps = (interval.as_millis() / 200).max(1);
                    for _ in 0..steps {
                        std::thread::sleep(Duration::from_millis(200));
                        if weak.upgrade().is_none_or(|s| s.stop.load(Ordering::SeqCst)) {
                            return;
                        }
                    }
                }
            })
            .expect("thread de vectorisation");
        *self.thread.lock().unwrap() = Some(handle);
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.thread.lock().unwrap().take() {
            let _ = handle.join();
        }
    }

    pub fn status(&self) -> Result<Value> {
        let Some(conn) = &self.conn else {
            return Ok(json!({"enabled": false, "model": null, "indexed": 0, "total": 0, "error": null}));
        };
        let notes = self.notes()?;
        let stored = Self::stored_hashes(&conn.lock().unwrap())?;
        let indexed = notes.iter().filter(|n| stored.get(&n.path) == Some(&n.hash)).count();
        Ok(json!({"enabled": true, "model": self.model, "indexed": indexed, "total": notes.len(),
                  "error": *self.error.lock().unwrap()}))
    }

    // --- recherche ----------------------------------------------------------

    fn nearest(&self, vector: &[f32], k: usize, exclude: &HashSet<String>) -> Result<Vec<SemanticHit>> {
        let Some(conn) = &self.conn else { return Ok(Vec::new()) };
        let rows: Vec<(String, String, f64)> = {
            let conn = conn.lock().unwrap();
            if Self::get_meta(&conn, "dim")?.is_none() {
                return Ok(Vec::new());
            }
            let mut stmt = conn.prepare(
                "SELECT c.path, c.text, v.distance FROM \
                 (SELECT rowid, distance FROM vec_chunks WHERE embedding MATCH ? AND k = ?) v \
                 JOIN chunks c ON c.id = v.rowid ORDER BY v.distance",
            )?;
            
            stmt
                .query_map(params![pack(vector), (k * 4 + exclude.len() * 2) as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        let mut best: IndexMap<String, (f64, String)> = IndexMap::new();
        for (path, text, distance) in rows {
            if exclude.contains(&path) || best.get(&path).is_some_and(|(d, _)| *d <= distance) {
                continue;
            }
            best.insert(path, (distance, text));
        }
        let mut ranked: Vec<(String, (f64, String))> = best.into_iter().collect();
        ranked.sort_by(|a, b| a.1 .0.total_cmp(&b.1 .0));
        let mut results = Vec::new();
        for (path, (distance, text)) in ranked.into_iter().take(k) {
            let Some(note) = self.index.get(&path)? else { continue };
            // extrait sans le titre ajouté au passage ni les lignes de titre Markdown
            let body = text.split_once("\n\n").map(|(_, b)| b).unwrap_or(&text);
            let kept: Vec<&str> = body.lines().filter(|l| !l.trim_start().starts_with('#')).collect();
            let snippet = kept.join(" ").split_whitespace().collect::<Vec<_>>().join(" ");
            let ellipsis = if snippet.chars().count() > 220 { "…" } else { "" };
            results.push(SemanticHit {
                path,
                title: note.title,
                kind: note.kind,
                date: note.date,
                alias: note.alias,
                stem: note.stem,
                snippet: format!("{}{ellipsis}", head(&snippet, 220)),
                score: ((1.0 - distance) * 1000.0).round() / 1000.0,
            });
        }
        Ok(results)
    }

    pub fn search(&self, query: &str, k: usize) -> Result<Vec<SemanticHit>> {
        let Some(embedder) = self.embedder.as_ref().filter(|_| self.enabled() && !query.trim().is_empty()) else {
            return Ok(Vec::new());
        };
        let vector = embedder(&[query.to_string()], EmbedKind::Query)?.into_iter().next().unwrap_or_default();
        self.nearest(&vector, k, &HashSet::new())
    }

    /// Notes proches d'une note et pas encore liées à elle (suggestions de liens).
    pub fn related(&self, path: &str, k: usize) -> Result<Vec<SemanticHit>> {
        let Some(conn) = &self.conn else { return Ok(Vec::new()) };
        let vectors: Vec<Vec<f32>> = {
            let conn = conn.lock().unwrap();
            if Self::get_meta(&conn, "dim")?.is_none() {
                return Ok(Vec::new());
            }
            let mut stmt = conn
                .prepare("SELECT v.embedding FROM chunks c JOIN vec_chunks v ON v.rowid = c.id WHERE c.path = ?")?;
            let blobs: Vec<Vec<u8>> = stmt.query_map([path], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
            blobs.iter().map(|b| unpack(b)).collect()
        };
        let Some(first) = vectors.first() else { return Ok(Vec::new()) };
        let mut mean = vec![0f32; first.len()];
        for vector in &vectors {
            for (m, x) in mean.iter_mut().zip(vector) {
                *m += x / vectors.len() as f32;
            }
        }
        let mut linked: HashSet<String> = HashSet::from([path.to_string()]);
        linked.extend(self.index.outlinks(path)?.into_iter().map(|n| n.path));
        linked.extend(self.index.backlinks(path)?.into_iter().map(|n| n.path));
        self.nearest(&mean, k, &linked)
    }
}

impl Drop for SemanticIndex {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks() {
        let body: Vec<String> = (0..10).map(|i| format!("Paragraphe {i} {}", "x".repeat(300))).collect();
        let chunks = chunk_text("Titre", &body.join("\n\n"));
        assert!(chunks.len() > 1 && chunks.iter().all(|c| c.starts_with("Titre\n\n")));
        assert!(chunks.iter().all(|c| c.chars().count() <= CHUNK_CHARS + 10));
        assert_eq!(chunk_text("Vide", ""), vec!["Vide"]);
        assert_eq!(chunk_text("Long", &"y".repeat(CHUNK_CHARS * 2 + 5)).len(), 3);
    }
}
