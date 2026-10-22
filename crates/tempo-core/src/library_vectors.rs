//! Vecteurs des passages de la bibliothèque (sqlite-vec, dans `library.db`) : recherche par le
//! sens, fusionnée avec la recherche plein texte.
//!
//! Comme l'index plein texte, c'est un cache : chaque passage de `chunks_fts` reçoit un vecteur
//! en arrière-plan (table `chunk_vec`, même rowid ; `embedded` garde le document de chaque
//! passage vectorisé). Un document réindexé perd ses vecteurs, recalculés ensuite ; un
//! changement de modèle repart de zéro.

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::error::Result;
use crate::semantic::{pack, register_sqlite_vec, EmbedKind, Embedder};
use crate::util::head;

/// passages vectorisés par appel au modèle
pub const BATCH: usize = 16;
/// texte d'un passage transmis au modèle (le modèle tronque de toute façon à 512 jetons)
const EMBED_CHARS: usize = 2000;
/// écart toléré avec le meilleur score : au-delà, un passage est jugé hors sujet
const RELATIVE_MARGIN: f32 = 0.025;

/// Réglages de la recherche par le sens de la bibliothèque.
#[derive(Clone)]
pub struct VectorConfig {
    pub embedder: Embedder,
    /// nom du modèle : un changement efface les vecteurs
    pub model: String,
    /// similarité cosinus minimale d'un passage retenu (dépend du modèle)
    pub min_similarity: f32,
}

pub struct VectorStore {
    conn: Mutex<Connection>,
    config: VectorConfig,
    error: Mutex<Option<String>>,
}

impl VectorStore {
    pub fn open(db_path: &Path, config: VectorConfig) -> Result<Self> {
        register_sqlite_vec();
        let conn = Connection::open(db_path)?;
        conn.busy_timeout(Duration::from_secs(10))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS vec_meta(key TEXT PRIMARY KEY, value TEXT);
             CREATE TABLE IF NOT EXISTS embedded(rowid INTEGER PRIMARY KEY, doc TEXT);
             CREATE INDEX IF NOT EXISTS embedded_doc ON embedded(doc);",
        )?;
        let model: Option<String> = conn.query_row("SELECT value FROM vec_meta WHERE key = 'model'", [], |r| r.get(0)).optional()?;
        if model.is_some_and(|m| m != config.model) {
            Self::reset(&conn)?;
        }
        conn.execute("INSERT OR REPLACE INTO vec_meta VALUES ('model', ?)", [&config.model])?;
        Ok(VectorStore { conn: Mutex::new(conn), config, error: Mutex::new(None) })
    }

    fn reset(conn: &Connection) -> rusqlite::Result<()> {
        conn.execute_batch("DROP TABLE IF EXISTS chunk_vec; DELETE FROM embedded; DELETE FROM vec_meta WHERE key = 'dim';")
    }

    fn dim(conn: &Connection) -> rusqlite::Result<Option<usize>> {
        let dim: Option<String> = conn.query_row("SELECT value FROM vec_meta WHERE key = 'dim'", [], |r| r.get(0)).optional()?;
        Ok(dim.and_then(|d| d.parse().ok()))
    }

    fn ensure_table(conn: &Connection, dim: usize) -> rusqlite::Result<()> {
        match Self::dim(conn)? {
            Some(current) if current == dim => return Ok(()),
            Some(_) => Self::reset(conn)?,
            None => {}
        }
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE IF NOT EXISTS chunk_vec USING vec0(embedding float[{dim}] distance_metric=cosine)"
        ))?;
        conn.execute("INSERT OR REPLACE INTO vec_meta VALUES ('dim', ?)", [dim.to_string()])?;
        Ok(())
    }

    /// Oublie les vecteurs d'un document (réindexé ou supprimé).
    pub fn forget(&self, doc: &str) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        if Self::dim(&tx)?.is_some() {
            tx.execute("DELETE FROM chunk_vec WHERE rowid IN (SELECT rowid FROM embedded WHERE doc = ?)", [doc])?;
        }
        tx.execute("DELETE FROM embedded WHERE doc = ?", [doc])?;
        tx.commit()?;
        Ok(())
    }

    /// Vectorise au plus `max` passages qui n'ont pas encore de vecteur ; renvoie leur nombre.
    pub fn embed_pending(&self, max: usize) -> Result<usize> {
        let rows: Vec<(i64, String, String, String, String)> = {
            let conn = self.conn.lock().unwrap();
            let mut stmt = conn.prepare(
                "SELECT rowid, doc, title, section, text FROM chunks_fts \
                 WHERE ord >= 0 AND rowid NOT IN (SELECT rowid FROM embedded) LIMIT ?",
            )?;
            stmt.query_map([max as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        if rows.is_empty() {
            return Ok(0);
        }
        let texts: Vec<String> = rows
            .iter()
            .map(|(_, _, title, section, text)| {
                let heading = [title.as_str(), section.as_str()].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>();
                head(&format!("{}\n\n{text}", heading.join(" › ")), EMBED_CHARS)
            })
            .collect();
        // appel au modèle hors verrou : la recherche reste disponible pendant la vectorisation
        let outcome = (self.config.embedder)(&texts, EmbedKind::Passage);
        let vectors = match outcome {
            Ok(vectors) => vectors,
            Err(err) => {
                *self.error.lock().unwrap() = Some(err.to_string());
                return Err(err);
            }
        };
        *self.error.lock().unwrap() = None;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        if let Some(first) = vectors.first() {
            Self::ensure_table(&tx, first.len())?;
        }
        for ((rowid, doc, _, _, text), vector) in rows.iter().zip(&vectors) {
            // le passage a pu être réindexé entre-temps (rowid repris par un autre texte)
            let current: Option<String> =
                tx.query_row("SELECT text FROM chunks_fts WHERE rowid = ? AND doc = ?", params![rowid, doc], |r| r.get(0)).optional()?;
            if current.as_deref() != Some(text.as_str()) {
                continue;
            }
            tx.execute("DELETE FROM chunk_vec WHERE rowid = ?", [rowid])?;
            tx.execute("INSERT INTO chunk_vec(rowid, embedding) VALUES (?, ?)", params![rowid, pack(vector)])?;
            tx.execute("INSERT OR REPLACE INTO embedded VALUES (?, ?)", params![rowid, doc])?;
        }
        tx.commit()?;
        Ok(rows.len())
    }

    /// Vectorise tous les passages en attente (sans surveillance : tests, réindexation).
    pub fn embed_all(&self) -> Result<usize> {
        let mut total = 0;
        loop {
            let done = self.embed_pending(BATCH)?;
            if done == 0 {
                return Ok(total);
            }
            total += done;
        }
    }

    /// Passages les plus proches d'une recherche : (rowid, similarité), du plus proche au plus
    /// lointain, sans ceux trop éloignés du meilleur ou sous le seuil du modèle.
    pub fn nearest(&self, query: &str, k: usize) -> Result<Vec<(i64, f32)>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        if Self::dim(&self.conn.lock().unwrap())?.is_none() {
            return Ok(Vec::new());
        }
        let vector = (self.config.embedder)(&[query.to_string()], EmbedKind::Query)?.into_iter().next().unwrap_or_default();
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT rowid, distance FROM chunk_vec WHERE embedding MATCH ? AND k = ? ORDER BY distance")?;
        let hits: Vec<(i64, f32)> = stmt
            .query_map(params![pack(&vector), k as i64], |r| Ok((r.get::<_, i64>(0)?, 1.0 - r.get::<_, f64>(1)? as f32)))?
            .collect::<rusqlite::Result<_>>()?;
        let best = hits.first().map(|(_, s)| *s).unwrap_or(0.0);
        Ok(hits
            .into_iter()
            .filter(|(_, s)| *s >= self.config.min_similarity && *s >= best - RELATIVE_MARGIN)
            .collect())
    }

    /// Avancement : passages vectorisés sur le total, dernière erreur du modèle.
    pub fn status(&self) -> Result<Value> {
        let conn = self.conn.lock().unwrap();
        let total: i64 = conn.query_row("SELECT count(*) FROM chunks_fts WHERE ord >= 0", [], |r| r.get(0))?;
        let embedded: i64 = conn.query_row("SELECT count(*) FROM embedded", [], |r| r.get(0))?;
        Ok(json!({"enabled": true, "model": self.config.model, "embedded": embedded, "total": total,
                  "error": *self.error.lock().unwrap()}))
    }
}
