//! Bibliothèque de documents : dépôt, conversion en Markdown pivot, file « À classer »,
//! thèmes et index de recherche.
//!
//! Le dossier de la bibliothèque (`TEMPO_LIBRARY`) contient un pivot `<nom>.md` par document
//! (frontmatter : classement et source ; corps : texte converti) et les fichiers d'origine dans
//! `sources/`. Les pivots sont la source de vérité : l'index (`library.db`, rangé hors du vault
//! synchronisé) n'est qu'un cache reconstruit à partir d'eux. Le classement proposé par le LLM
//! n'est écrit qu'après validation par l'utilisateur.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use regex::Regex;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::config::Settings;
use crate::convert::{self, format_of, Tools, FORMATS};
use crate::de;
use crate::error::{not_found, vault_err, Error, Result};
use crate::index::{fold, Index};
use crate::journal::file_name;
use crate::llm::{chat_json, Llm, Msg};
use crate::prompts::LIBRARY_CLASSIFY_SYSTEM;
use crate::util::{head, today};
use crate::vault::{content_hash, join_frontmatter, meta_str, slugify, split_frontmatter, value_str, Meta, Vault};

pub const SOURCES_DIR: &str = "sources";
pub const TRASH_DIR: &str = ".trash";
pub const THEMES_FOLDER: &str = "themes";
/// statut d'un document dont le classement attend la validation
pub const PENDING: &str = "a_classer";
/// taille maximale d'un fichier déposé
pub const MAX_UPLOAD_BYTES: usize = 100 * 1024 * 1024;
/// taille visée d'un passage indexé (en caractères)
pub const CHUNK_CHARS: usize = 1500;
/// début du texte transmis au modèle pour proposer le classement
const CLASSIFY_CHARS: usize = 12_000;
const MAX_TAGS: usize = 8;
const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS documents(id TEXT PRIMARY KEY, doc TEXT, hash TEXT, pending INTEGER);
CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
    doc UNINDEXED, ord UNINDEXED, page UNINDEXED, title, section, text,
    tokenize='unicode61 remove_diacritics 2'
);
";

static PAGE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^<!--\s*page\s+(\d+)\s*-->$").unwrap());
static HEADING_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(#{1,6})\s+(.+?)\s*#*\s*$").unwrap());
static COPY_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(.+) \((\d+)\)$").unwrap());
static WORD_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\w+").unwrap());

// --- modèle ----------------------------------------------------------------------

/// Document tel que décrit par le frontmatter de son pivot.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Document {
    /// nom du pivot sans `.md` : identifiant dans l'API et dans les liens `[[…]]`
    pub id: String,
    pub title: String,
    pub format: String,
    pub themes: Vec<String>,
    pub projects: Vec<String>,
    pub tags: Vec<String>,
    pub summary: String,
    /// fichier d'origine, relatif au dossier de la bibliothèque (`sources/<nom>.<ext>`)
    pub source: String,
    pub source_hash: String,
    pub size: u64,
    /// pages d'un PDF, diapositives d'une présentation
    pub pages: Option<u64>,
    pub added: String,
    pub updated: Option<String>,
    /// lien vers le mail dont le document est une pièce jointe
    pub attached_to: Option<String>,
    /// classement à valider (file « À classer »)
    pub pending: bool,
}

fn string_list(meta: &Meta, key: &str) -> Vec<String> {
    let items: Vec<String> = match meta.get(key) {
        Some(Value::Array(items)) => items.iter().map(value_str).collect(),
        Some(Value::String(s)) => s.split(',').map(String::from).collect(),
        _ => Vec::new(),
    };
    items.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

fn meta_u64(meta: &Meta, key: &str) -> Option<u64> {
    meta.get(key).and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())))
}

/// Document décrit par un frontmatter, `None` si ce n'est pas un pivot (`type: document`).
pub fn document_from(id: &str, meta: &Meta) -> Option<Document> {
    if meta_str(meta, "type").as_deref() != Some("document") {
        return None;
    }
    Some(Document {
        id: id.to_string(),
        title: meta_str(meta, "title").unwrap_or_else(|| id.to_string()),
        format: meta_str(meta, "format").unwrap_or_default(),
        themes: string_list(meta, "themes"),
        projects: string_list(meta, "projects"),
        tags: string_list(meta, "tags").into_iter().map(|t| t.trim_start_matches('#').to_string()).collect(),
        summary: meta_str(meta, "summary").unwrap_or_default(),
        source: meta_str(meta, "source").unwrap_or_default(),
        source_hash: meta_str(meta, "source_hash").unwrap_or_default(),
        size: meta_u64(meta, "size").unwrap_or(0),
        pages: meta_u64(meta, "pages"),
        added: meta_str(meta, "added").unwrap_or_default(),
        updated: meta_str(meta, "updated"),
        attached_to: meta_str(meta, "attached_to"),
        pending: meta_str(meta, "status").as_deref() == Some(PENDING),
    })
}

/// Tag normalisé : minuscules, sans `#`, espaces remplacés par des tirets.
pub fn normalize_tag(tag: &str) -> String {
    let tag = tag.trim().trim_start_matches('#').to_lowercase();
    tag.split_whitespace().collect::<Vec<_>>().join("-").trim_matches(['-', ',', ';']).to_string()
}

fn normalize_tags(tags: &[String]) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();
    for tag in tags.iter().map(|t| normalize_tag(t)).filter(|t| !t.is_empty()) {
        if !result.contains(&tag) {
            result.push(tag);
        }
    }
    result
}

fn dedup(values: &[String]) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();
    for value in values.iter().map(|v| v.trim()).filter(|v| !v.is_empty()) {
        if !result.iter().any(|r| r == value) {
            result.push(value.to_string());
        }
    }
    result
}

fn bytes_hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))[..16].to_string()
}

/// Identifiant de document valide : un nom de fichier, sans dossier.
fn check_id(id: &str) -> Result<()> {
    if id.trim().is_empty() || id.starts_with('.') || id.contains(['/', '\\', ':', '\0']) {
        return Err(vault_err(format!("Document invalide : {id}")));
    }
    Ok(())
}

/// Document dont `id` est une copie numérotée (`Spec (2)`) : celui qu'il remplacerait.
fn base_of_copy(id: &str) -> Option<String> {
    COPY_RE.captures(id).map(|c| c[1].to_string())
}

// --- passages --------------------------------------------------------------------

/// Passage indexé : section (titres imbriqués), page de début, texte.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Chunk {
    pub section: String,
    pub page: Option<u32>,
    pub text: String,
}

#[derive(Default)]
struct Chunker {
    chunks: Vec<Chunk>,
    current: Chunk,
    headings: Vec<(usize, String)>,
    page: Option<u32>,
}

impl Chunker {
    fn section(&self) -> String {
        self.headings.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join(" › ")
    }

    fn flush(&mut self) {
        let done = std::mem::take(&mut self.current);
        if !done.text.trim().is_empty() {
            self.chunks.push(done);
        }
    }

    fn add_paragraph(&mut self, paragraph: &str) {
        let paragraph = paragraph.trim();
        if paragraph.is_empty() {
            return;
        }
        let size = paragraph.chars().count();
        if !self.current.text.is_empty() && self.current.text.chars().count() + size > CHUNK_CHARS {
            self.flush();
        }
        if self.current.text.is_empty() {
            self.current.section = self.section();
            self.current.page = self.page;
        }
        if size <= CHUNK_CHARS {
            if !self.current.text.is_empty() {
                self.current.text.push_str("\n\n");
            }
            self.current.text.push_str(paragraph);
            return;
        }
        // paragraphe trop long (tableau, texte sans ligne vide) : coupé en morceaux
        let chars: Vec<char> = paragraph.chars().collect();
        for piece in chars.chunks(CHUNK_CHARS) {
            self.flush();
            self.current = Chunk { section: self.section(), page: self.page, text: piece.iter().collect() };
        }
        self.flush();
    }
}

/// Découpe le corps d'un pivot en passages d'environ [`CHUNK_CHARS`] caractères, coupés aux
/// titres et aux changements de page (`<!-- page N -->`), avec leur section et leur page.
pub fn chunk_document(body: &str) -> Vec<Chunk> {
    let mut chunker = Chunker::default();
    let mut paragraph: Vec<&str> = Vec::new();
    let mut in_code = false;
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_code = !in_code;
            paragraph.push(line);
            continue;
        }
        if in_code {
            paragraph.push(line);
            continue;
        }
        if let Some(caps) = PAGE_RE.captures(trimmed) {
            chunker.add_paragraph(&paragraph.join("\n"));
            paragraph.clear();
            chunker.flush();
            chunker.page = caps[1].parse().ok();
        } else if let Some(caps) = HEADING_RE.captures(trimmed) {
            chunker.add_paragraph(&paragraph.join("\n"));
            paragraph.clear();
            chunker.flush();
            let level = caps[1].len();
            chunker.headings.retain(|(l, _)| *l < level);
            chunker.headings.push((level, caps[2].to_string()));
        } else if trimmed.is_empty() {
            chunker.add_paragraph(&paragraph.join("\n"));
            paragraph.clear();
        } else {
            paragraph.push(line);
        }
    }
    chunker.add_paragraph(&paragraph.join("\n"));
    chunker.flush();
    chunker.chunks
}

/// Titre de la table des matières d'un document.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OutlineEntry {
    pub level: usize,
    pub title: String,
}

/// Titres du corps d'un pivot (hors blocs de code).
pub fn outline(body: &str) -> Vec<OutlineEntry> {
    let mut entries = Vec::new();
    let mut in_code = false;
    for line in body.lines().map(str::trim) {
        if line.starts_with("```") {
            in_code = !in_code;
        } else if !in_code && let Some(caps) = HEADING_RE.captures(line) {
            entries.push(OutlineEntry { level: caps[1].len(), title: caps[2].to_string() });
        }
    }
    entries
}

// --- réponses de l'API -----------------------------------------------------------

/// Document trouvé : passage le plus pertinent (vide sans recherche).
#[derive(Debug, Clone, Serialize)]
pub struct LibraryHit {
    #[serde(flatten)]
    pub document: Document,
    pub snippet: String,
    pub section: String,
    pub page: Option<u32>,
}

/// Filtres de la liste des documents ; `-` vaut « sans thème » / « sans projet ».
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LibraryFilter {
    pub q: Option<String>,
    pub theme: Option<String>,
    pub project: Option<String>,
    pub tag: Option<String>,
    pub format: Option<String>,
    pub pending: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocumentDetail {
    #[serde(flatten)]
    pub document: Document,
    /// texte converti (corps du pivot)
    pub content: String,
    pub outline: Vec<OutlineEntry>,
    /// document que celui-ci remplacerait (même nom de fichier)
    pub replaces: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct QueueItem {
    #[serde(flatten)]
    pub document: Document,
    pub replaces: Option<String>,
}

/// Fichier présent dans le dossier sans pivot qui le désigne (copié à la main).
#[derive(Debug, Clone, Serialize)]
pub struct Untracked {
    /// chemin relatif au dossier de la bibliothèque
    pub path: String,
    pub name: String,
    pub size: u64,
    /// format pris en charge, `None` sinon
    pub format: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Queue {
    pub pending: Vec<QueueItem>,
    pub untracked: Vec<Untracked>,
}

/// Résultat d'un dépôt : le document et les pièces jointes ajoutées avec lui (mail).
#[derive(Debug, Clone, Serialize)]
pub struct Ingested {
    pub document: Document,
    pub attachments: Vec<Document>,
    /// pièces jointes écartées (doublon, format non pris en charge, conversion impossible)
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Theme {
    pub id: String,
    pub title: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TagCount {
    pub tag: String,
    pub count: usize,
}

/// Classement proposé par le modèle, sans écriture.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Classification {
    #[serde(default, deserialize_with = "de::string")]
    pub title: String,
    #[serde(default, deserialize_with = "de::string")]
    pub summary: String,
    #[serde(default, deserialize_with = "de::string_list")]
    pub themes: Vec<String>,
    /// thèmes à créer suggérés quand aucun thème existant ne convient
    #[serde(default, deserialize_with = "de::string_list")]
    pub new_themes: Vec<String>,
    #[serde(default, deserialize_with = "de::string_list")]
    pub projects: Vec<String>,
    #[serde(default, deserialize_with = "de::string_list")]
    pub tags: Vec<String>,
    /// document que celui-ci remplacerait (déterminé par le nom, pas par le modèle)
    #[serde(default, skip_deserializing)]
    pub replaces: Option<String>,
}

/// Classement validé par l'utilisateur.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DocumentMeta {
    pub title: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub themes: Vec<String>,
    #[serde(default)]
    pub projects: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// document à remplacer par celui-ci (file « À classer »)
    #[serde(default)]
    pub replaces: Option<String>,
}

// --- service ---------------------------------------------------------------------

pub struct LibraryService {
    /// dossier de la bibliothèque
    pub root: PathBuf,
    vault: Arc<Vault>,
    index: Arc<Index>,
    db: Mutex<Connection>,
    tools: Tools,
    llm: Arc<dyn Llm>,
    settings: Arc<Settings>,
    /// sérialise les écritures (choix des noms, remplacements)
    write_lock: Mutex<()>,
    /// fichiers non ingérés, recalculés avec la file
    untracked: AtomicUsize,
    watcher: Mutex<Option<RecommendedWatcher>>,
}

impl LibraryService {
    pub fn new(settings: Arc<Settings>, vault: Arc<Vault>, index: Arc<Index>, llm: Arc<dyn Llm>) -> Result<Self> {
        std::fs::create_dir_all(settings.library.join(SOURCES_DIR))?;
        let root = dunce::canonicalize(&settings.library)?;
        std::fs::create_dir_all(&settings.library_cache)?;
        let conn = Connection::open(settings.library_cache.join("library.db"))?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version != SCHEMA_VERSION {
            conn.execute_batch("DROP TABLE IF EXISTS documents; DROP TABLE IF EXISTS chunks_fts;")?;
            conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))?;
        }
        conn.execute_batch(SCHEMA)?;
        let tools = Tools::discover(settings.pandoc.as_deref(), settings.pdfium.as_deref());
        Ok(LibraryService {
            root,
            vault,
            index,
            db: Mutex::new(conn),
            tools,
            llm,
            settings,
            write_lock: Mutex::new(()),
            untracked: AtomicUsize::new(0),
            watcher: Mutex::new(None),
        })
    }

    /// Outils de conversion trouvés (pandoc, pdfium).
    pub fn tools(&self) -> &Tools {
        &self.tools
    }

    fn pivot_path(&self, id: &str) -> PathBuf {
        self.root.join(format!("{id}.md"))
    }

    fn read_pivot(&self, id: &str) -> Result<(Document, Meta, String)> {
        check_id(id)?;
        let path = self.pivot_path(id);
        let text = std::fs::read_to_string(&path).map_err(|_| not_found(format!("Document introuvable : {id}")))?;
        let (meta, body) = split_frontmatter(&text);
        let document = document_from(id, &meta).ok_or_else(|| not_found(format!("Document introuvable : {id}")))?;
        Ok((document, meta, body.to_string()))
    }

    fn write_pivot(&self, id: &str, meta: &Meta, body: &str) -> Result<()> {
        let body = if body.starts_with('\n') { body.to_string() } else { format!("\n{body}") };
        std::fs::write(self.pivot_path(id), join_frontmatter(meta, &body))?;
        Ok(())
    }

    /// Chemin absolu du fichier d'origine, refusé s'il sort du dossier.
    fn source_path(&self, document: &Document) -> Result<PathBuf> {
        let rel = document.source.replace('\\', "/");
        if rel.is_empty() || rel.split('/').any(|p| p == ".." || p.contains(':')) {
            return Err(vault_err(format!("Source invalide pour {} : {}", document.id, document.source)));
        }
        Ok(self.root.join(rel))
    }

    fn rel(&self, path: &Path) -> Option<String> {
        let relative = path.strip_prefix(&self.root).ok()?;
        Some(relative.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/"))
    }

    /// Déplace un fichier de la bibliothèque dans `.trash/<horodatage>/` (récupérable).
    fn trash(&self, path: &Path, stamp: &str) -> Result<()> {
        if !path.is_file() {
            return Ok(());
        }
        let relative = path.strip_prefix(&self.root).map_err(|_| vault_err("Chemin hors de la bibliothèque"))?;
        let target = self.root.join(TRASH_DIR).join(stamp).join(relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if std::fs::rename(path, &target).is_err() {
            std::fs::copy(path, &target)?;
            std::fs::remove_file(path)?;
        }
        Ok(())
    }

    // --- index -----------------------------------------------------------------

    /// Indexe le pivot `id` s'il a changé (ou le retire de l'index s'il n'existe plus) ; vrai
    /// s'il a été réindexé.
    pub fn reindex(&self, id: &str) -> Result<bool> {
        let text = std::fs::read_to_string(self.pivot_path(id)).ok();
        let parsed = text.as_deref().and_then(|t| {
            let (meta, body) = split_frontmatter(t);
            document_from(id, &meta).map(|d| (d, body.to_string()))
        });
        let mut conn = self.db.lock().unwrap();
        let Some((document, body)) = parsed else {
            let tx = conn.transaction()?;
            tx.execute("DELETE FROM documents WHERE id = ?", [id])?;
            tx.execute("DELETE FROM chunks_fts WHERE doc = ?", [id])?;
            tx.commit()?;
            return Ok(false);
        };
        let hash = content_hash(text.as_deref().unwrap_or_default());
        let current: Option<String> =
            conn.query_row("SELECT hash FROM documents WHERE id = ?", [id], |r| r.get(0)).optional()?;
        if current.as_deref() == Some(hash.as_str()) {
            return Ok(false);
        }
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM chunks_fts WHERE doc = ?", [id])?;
        tx.execute(
            "INSERT OR REPLACE INTO documents VALUES (?,?,?,?)",
            params![id, serde_json::to_string(&document).unwrap(), hash, document.pending],
        )?;
        // fiche du document : titre, résumé, tags, nom du fichier d'origine
        let card = [document.summary.as_str(), &document.tags.join(" "), &document.source].join("\n");
        tx.execute(
            "INSERT INTO chunks_fts VALUES (?,?,?,?,?,?)",
            params![id, -1, Option::<u32>::None, document.title, "Fiche", card],
        )?;
        for (ord, chunk) in chunk_document(&body).iter().enumerate() {
            tx.execute(
                "INSERT INTO chunks_fts VALUES (?,?,?,?,?,?)",
                params![id, ord as i64, chunk.page, document.title, chunk.section, chunk.text],
            )?;
        }
        tx.commit()?;
        Ok(true)
    }

    /// Identifiants des pivots du dossier (fichiers `.md` de premier niveau).
    fn pivot_ids(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(&self.root) else { return Vec::new() };
        let mut ids: Vec<String> = entries
            .flatten()
            .filter(|e| e.path().is_file())
            .filter_map(|e| e.file_name().to_string_lossy().strip_suffix(".md").map(String::from))
            .filter(|id| check_id(id).is_ok())
            .collect();
        ids.sort();
        ids
    }

    /// Met l'index en accord avec les pivots : pivots modifiés réindexés, disparus retirés ;
    /// renvoie le nombre de documents réindexés.
    pub fn sync(&self) -> Result<usize> {
        let ids = self.pivot_ids();
        let mut count = 0;
        for id in &ids {
            // un pivot illisible n'empêche pas l'indexation des autres
            if self.reindex(id).unwrap_or(false) {
                count += 1;
            }
        }
        let known: Vec<String> = {
            let conn = self.db.lock().unwrap();
            let mut stmt = conn.prepare("SELECT id FROM documents")?;
            stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
        };
        let present: HashSet<&String> = ids.iter().collect();
        for id in known.iter().filter(|id| !present.contains(id)) {
            self.reindex(id)?;
            count += 1;
        }
        self.refresh_untracked();
        self.index.touch();
        Ok(count)
    }

    /// Indexe la bibliothèque puis, si demandé, la surveille ; avec surveillance, l'indexation
    /// tourne en arrière-plan (première indexation d'un gros dossier).
    pub fn start(self: &Arc<Self>, watch: bool) -> Result<()> {
        if !watch {
            self.sync()?;
            return Ok(());
        }
        let library = self.clone();
        std::thread::spawn(move || {
            if let Err(err) = library.sync() {
                eprintln!("Indexation de la bibliothèque impossible : {err}");
            }
        });
        let weak = Arc::downgrade(self);
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            let (Ok(event), Some(library)) = (event, weak.upgrade()) else { return };
            library.on_change(&event.paths);
        })
        .map_err(|e| Error::Internal(format!("Surveillance de la bibliothèque impossible : {e}")))?;
        watcher
            .watch(&self.root, RecursiveMode::Recursive)
            .map_err(|e| Error::Internal(format!("Surveillance de la bibliothèque impossible : {e}")))?;
        *self.watcher.lock().unwrap() = Some(watcher);
        Ok(())
    }

    pub fn stop(&self) {
        self.watcher.lock().unwrap().take();
    }

    /// Fichiers modifiés dans le dossier (watcher) : pivots réindexés, file recalculée.
    fn on_change(&self, paths: &[PathBuf]) {
        let mut changed = false;
        for path in paths {
            let Some(rel) = self.rel(path) else { continue };
            if rel.split('/').any(|part| part.starts_with('.')) {
                continue;
            }
            changed = true;
            if let Some(id) = rel.strip_suffix(".md").filter(|id| !id.contains('/')) {
                let _ = self.reindex(id);
            }
        }
        if changed {
            self.refresh_untracked();
            self.index.touch();
        }
    }

    // --- consultation ----------------------------------------------------------

    /// Tous les documents indexés, par titre.
    pub fn documents(&self) -> Result<Vec<Document>> {
        let conn = self.db.lock().unwrap();
        let mut stmt = conn.prepare("SELECT doc FROM documents")?;
        let mut documents: Vec<Document> = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .filter_map(|json| serde_json::from_str(&json).ok())
            .collect();
        documents.sort_by_key(|d| (fold(&d.title), d.id.clone()));
        Ok(documents)
    }

    /// Nombre de documents à classer et de fichiers à ingérer (pastille de l'espace).
    pub fn pending_count(&self) -> Result<usize> {
        let conn = self.db.lock().unwrap();
        let pending: i64 = conn.query_row("SELECT count(*) FROM documents WHERE pending", [], |r| r.get(0))?;
        Ok(pending as usize + self.untracked.load(Ordering::SeqCst))
    }

    /// Documents filtrés ; avec une recherche (titre, mots-clés, tags), triés par pertinence
    /// avec leur meilleur passage.
    pub fn list(&self, filter: &LibraryFilter) -> Result<Vec<LibraryHit>> {
        let matches = |d: &Document| {
            let in_list = |values: &[String], wanted: &Option<String>| match wanted.as_deref().map(str::trim) {
                None | Some("") => true,
                Some("-") => values.is_empty(),
                Some(w) => values.iter().any(|v| v == w),
            };
            in_list(&d.themes, &filter.theme)
                && in_list(&d.projects, &filter.project)
                && filter.tag.as_deref().is_none_or(|t| t.is_empty() || d.tags.iter().any(|x| x == &normalize_tag(t)))
                && filter.format.as_deref().is_none_or(|f| f.is_empty() || d.format == f)
                && filter.pending.is_none_or(|p| d.pending == p)
        };
        let documents: HashMap<String, Document> =
            self.documents()?.into_iter().filter(|d| matches(d)).map(|d| (d.id.clone(), d)).collect();
        let words: Vec<String> = filter
            .q
            .as_deref()
            .map(|q| WORD_RE.find_iter(q).map(|m| m.as_str().to_string()).collect())
            .unwrap_or_default();
        if words.is_empty() {
            let mut hits: Vec<LibraryHit> = documents
                .into_values()
                .map(|document| LibraryHit { document, snippet: String::new(), section: String::new(), page: None })
                .collect();
            hits.sort_by_key(|h| (fold(&h.document.title), h.document.id.clone()));
            return Ok(hits);
        }
        let fts = words.iter().map(|w| format!("\"{w}\"*")).collect::<Vec<_>>().join(" ");
        let conn = self.db.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT doc, section, page, snippet(chunks_fts, 5, '«', '»', '…', 14) \
             FROM chunks_fts WHERE chunks_fts MATCH ? ORDER BY bm25(chunks_fts, 0, 0, 0, 8.0, 2.0, 1.0) LIMIT 2000",
        )?;
        let rows = stmt.query_map([&fts], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<u32>>(2)?, r.get::<_, String>(3)?))
        })?;
        let mut seen = HashSet::new();
        let mut hits = Vec::new();
        for row in rows.flatten() {
            let (doc, section, page, snippet) = row;
            if !seen.insert(doc.clone()) {
                continue;
            }
            if let Some(document) = documents.get(&doc) {
                let section = if section == "Fiche" { String::new() } else { section };
                hits.push(LibraryHit { document: document.clone(), snippet, section, page });
            }
        }
        Ok(hits)
    }

    pub fn get(&self, id: &str) -> Result<Document> {
        Ok(self.read_pivot(id)?.0)
    }

    /// Document désigné par un lien `[[nom]]` : son identifiant, sinon son titre (sans casse
    /// ni accents).
    pub fn resolve(&self, name: &str) -> Result<Option<String>> {
        let key = fold(name);
        let documents = self.documents()?;
        Ok(documents
            .iter()
            .find(|d| fold(&d.id) == key)
            .or_else(|| documents.iter().find(|d| fold(&d.title) == key))
            .map(|d| d.id.clone()))
    }

    /// Fiche complète : classement, texte converti, table des matières.
    pub fn detail(&self, id: &str) -> Result<DocumentDetail> {
        let (document, _, body) = self.read_pivot(id)?;
        let replaces = self.replaces_for(&document);
        Ok(DocumentDetail { outline: outline(&body), content: body.trim_start_matches('\n').to_string(), document, replaces })
    }

    /// Fichier d'origine : chemin et format.
    pub fn source(&self, id: &str) -> Result<(PathBuf, Document)> {
        let document = self.get(id)?;
        let path = self.source_path(&document)?;
        if !path.is_file() {
            return Err(not_found(format!("Fichier d'origine introuvable : {}", document.source)));
        }
        Ok((path, document))
    }

    /// Ouvre le fichier d'origine avec l'application par défaut du poste, ou le montre dans
    /// l'explorateur (`reveal`).
    pub fn open(&self, id: &str, reveal: bool) -> Result<()> {
        let (path, _) = self.source(id)?;
        let mut command = Command::new("explorer.exe");
        if reveal {
            command.arg(format!("/select,{}", path.display()));
        } else {
            command.arg(&path);
        }
        command.spawn().map_err(|e| Error::Internal(format!("Ouverture impossible : {e}")))?;
        Ok(())
    }

    /// Document que `document` remplacerait : une copie numérotée (`Spec (2)`) en attente de
    /// classement remplace `Spec` s'il existe.
    fn replaces_for(&self, document: &Document) -> Option<String> {
        if !document.pending {
            return None;
        }
        base_of_copy(&document.id).filter(|base| self.read_pivot(base).is_ok())
    }

    /// File « À classer » : documents en attente de validation et fichiers non ingérés.
    pub fn queue(&self) -> Result<Queue> {
        let pending = self
            .documents()?
            .into_iter()
            .filter(|d| d.pending)
            .map(|document| QueueItem { replaces: self.replaces_for(&document), document })
            .collect();
        let untracked = self.untracked_files();
        self.untracked.store(untracked.len(), Ordering::SeqCst);
        Ok(Queue { pending, untracked })
    }

    fn refresh_untracked(&self) {
        self.untracked.store(self.untracked_files().len(), Ordering::SeqCst);
    }

    /// Fichiers du dossier qu'aucun pivot ne désigne (hors `.trash` et dossiers cachés).
    fn untracked_files(&self) -> Vec<Untracked> {
        let mut pivots: HashSet<String> = HashSet::new();
        let mut sources: HashSet<String> = HashSet::new();
        for id in self.pivot_ids() {
            let Ok(text) = read_head(&self.pivot_path(&id), 8192) else { continue };
            let (meta, _) = split_frontmatter(&text);
            if let Some(document) = document_from(&id, &meta) {
                sources.insert(document.source.replace('\\', "/").to_lowercase());
                pivots.insert(format!("{id}.md").to_lowercase());
            }
        }
        let mut files: Vec<Untracked> = walkdir::WalkDir::new(&self.root)
            .into_iter()
            .filter_entry(|e| e.depth() == 0 || !e.file_name().to_string_lossy().starts_with('.'))
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter_map(|e| {
                let rel = self.rel(e.path())?;
                let key = rel.to_lowercase();
                if pivots.contains(&key) || sources.contains(&key) || rel.starts_with('~') || rel.contains("/~$") {
                    return None;
                }
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with("~$") || name.eq_ignore_ascii_case("desktop.ini") || name.eq_ignore_ascii_case("thumbs.db") {
                    return None;
                }
                let size = e.metadata().map(|m| m.len()).unwrap_or(0);
                Some(Untracked { format: format_of(&name).map(String::from), path: rel, name, size })
            })
            .collect();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        files
    }

    // --- dépôt -----------------------------------------------------------------

    /// Identifiant libre pour un nom de fichier : son nom sans extension, numéroté (`(2)`) s'il
    /// est déjà pris par un pivot ou une source.
    fn free_id(&self, name: &str, ext: &str) -> String {
        let stem = Path::new(name).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let base = file_name(&stem);
        let taken = |id: &str| self.pivot_path(id).exists() || self.root.join(SOURCES_DIR).join(format!("{id}.{ext}")).exists();
        if !taken(&base) {
            return base;
        }
        (2..).map(|n| format!("{base} ({n})")).find(|id| !taken(id)).unwrap()
    }

    /// Ajoute un fichier : source rangée dans `sources/`, conversion, pivot `status: a_classer`.
    /// Un fichier identique à une source déjà présente est refusé ; les pièces jointes d'un
    /// mail sont ajoutées à leur tour, reliées au mail.
    pub fn ingest(&self, name: &str, bytes: &[u8]) -> Result<Ingested> {
        self.ingest_with(name, bytes, None)
    }

    fn ingest_with(&self, name: &str, bytes: &[u8], attached_to: Option<&str>) -> Result<Ingested> {
        let name = Path::new(name.trim()).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let format = format_of(&name).ok_or_else(|| {
            vault_err(format!("Format non pris en charge : {name} (formats acceptés : {})", FORMATS.join(", ")))
        })?;
        if bytes.len() > MAX_UPLOAD_BYTES {
            return Err(vault_err(format!("Fichier trop volumineux : {name} ({} Mo au plus)", MAX_UPLOAD_BYTES / 1024 / 1024)));
        }
        let hash = bytes_hash(bytes);
        if let Some(existing) = self.documents()?.into_iter().find(|d| d.source_hash == hash) {
            return Err(vault_err(format!("Ce fichier est déjà dans la bibliothèque : « {} »", existing.title)));
        }
        let converted = convert::convert(bytes, format, &self.tools)?;
        let ext = Path::new(&name).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let document = {
            let _guard = self.write_lock.lock().unwrap();
            let id = self.free_id(&name, &ext);
            let source = format!("{SOURCES_DIR}/{id}.{ext}");
            std::fs::write(self.root.join(&source), bytes)?;
            let title = converted.title.clone().filter(|t| !t.trim().is_empty()).unwrap_or_else(|| id.clone());
            let mut meta = Meta::new();
            meta.insert("type".into(), "document".into());
            meta.insert("title".into(), title.trim().into());
            meta.insert("themes".into(), json!([]));
            meta.insert("projects".into(), json!([]));
            meta.insert("tags".into(), json!([]));
            meta.insert("summary".into(), "".into());
            meta.insert("source".into(), source.into());
            meta.insert("format".into(), format.into());
            meta.insert("source_hash".into(), hash.into());
            meta.insert("size".into(), (bytes.len() as u64).into());
            if let Some(pages) = converted.pages {
                meta.insert("pages".into(), (pages as u64).into());
            }
            meta.insert("added".into(), today().to_string().into());
            if let Some(parent) = attached_to {
                meta.insert("attached_to".into(), format!("[[{parent}]]").into());
            }
            meta.insert("status".into(), PENDING.into());
            self.write_pivot(&id, &meta, &converted.markdown)?;
            self.reindex(&id)?;
            self.get(&id)?
        };
        let mut attachments = Vec::new();
        let mut warnings = Vec::new();
        // une pièce jointe d'une pièce jointe n'est pas ajoutée (mails transférés en cascade)
        if attached_to.is_none() {
            for (file, data) in &converted.attachments {
                if format_of(file).is_none() {
                    warnings.push(format!("{file} : format non pris en charge"));
                    continue;
                }
                match self.ingest_with(file, data, Some(&document.id)) {
                    Ok(done) => attachments.push(done.document),
                    Err(err) => warnings.push(format!("{file} : {err}")),
                }
            }
        }
        self.index.touch();
        Ok(Ingested { document, attachments, warnings })
    }

    /// Ingère un fichier copié à la main dans le dossier : il est converti, rangé dans
    /// `sources/` sous son nom de document, et l'original retiré.
    pub fn ingest_path(&self, rel: &str) -> Result<Ingested> {
        let rel = rel.replace('\\', "/");
        if rel.is_empty() || rel.split('/').any(|p| p == ".." || p.starts_with('.') || p.contains(':')) {
            return Err(vault_err(format!("Chemin invalide : {rel}")));
        }
        let path = self.root.join(&rel);
        if !path.is_file() {
            return Err(not_found(format!("Fichier introuvable : {rel}")));
        }
        let bytes = std::fs::read(&path)?;
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let ingested = self.ingest(&name, &bytes)?;
        if self.root.join(&ingested.document.source) != path {
            std::fs::remove_file(&path)?;
        }
        self.refresh_untracked();
        self.index.touch();
        Ok(ingested)
    }

    // --- classement ------------------------------------------------------------

    pub fn themes(&self) -> Result<Vec<Theme>> {
        let mut themes: Vec<Theme> = self
            .index
            .by_type("theme")?
            .into_iter()
            .filter(|n| !n.is_template())
            .map(|n| Theme { id: n.stem, title: n.title, path: n.path })
            .collect();
        themes.sort_by_key(|t| fold(&t.title));
        Ok(themes)
    }

    /// Crée la note `themes/<slug>.md` ; l'identifiant du thème est le nom du fichier.
    pub fn create_theme(&self, title: &str) -> Result<Theme> {
        let title = title.trim();
        if title.is_empty() {
            return Err(vault_err("Le nom du thème est obligatoire"));
        }
        if self.themes()?.iter().any(|t| fold(&t.title) == fold(title)) {
            return Err(vault_err(format!("Le thème « {title} » existe déjà")));
        }
        let meta: Meta = serde_json::from_value(json!({"type": "theme", "title": title})).unwrap();
        let rel = self.vault.unique_path(THEMES_FOLDER, &slugify(title, 40))?;
        self.vault.write(&rel, &join_frontmatter(&meta, &format!("\n# {title}\n")), None, true)?;
        self.index.upsert(&rel)?;
        self.themes()?.into_iter().find(|t| t.path == rel).ok_or_else(|| not_found(format!("Thème introuvable : {rel}")))
    }

    /// Projets du vault : (identifiant, titre).
    fn projects(&self) -> Result<Vec<(String, String)>> {
        Ok(self.index.by_type("projet")?.into_iter().filter(|n| !n.is_template()).map(|n| (n.stem, n.title)).collect())
    }

    /// Tags des notes et des documents, des plus fréquents aux plus rares.
    pub fn tags(&self) -> Result<Vec<TagCount>> {
        let mut counts: HashMap<String, usize> = HashMap::new();
        for (tag, n) in self.index.tag_counts()? {
            *counts.entry(normalize_tag(&tag)).or_default() += n;
        }
        for document in self.documents()? {
            for tag in &document.tags {
                *counts.entry(normalize_tag(tag)).or_default() += 1;
            }
        }
        let mut tags: Vec<TagCount> = counts.into_iter().filter(|(t, _)| !t.is_empty()).map(|(tag, count)| TagCount { tag, count }).collect();
        tags.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.tag.cmp(&b.tag)));
        Ok(tags)
    }

    /// Classement proposé par le modèle pour un document, sans écriture : thèmes et projets
    /// limités à ceux du vault (un thème inconnu devient une suggestion de thème à créer), tags
    /// normalisés.
    pub fn classify(&self, id: &str) -> Result<Classification> {
        let (document, _, body) = self.read_pivot(id)?;
        let themes = self.themes()?;
        let projects = self.projects()?;
        let tags = self.tags()?;
        let list = |items: Vec<String>| if items.is_empty() { "aucun".to_string() } else { items.join("\n") };
        let headings: Vec<String> = outline(&body).iter().take(60).map(|e| format!("{} {}", "#".repeat(e.level), e.title)).collect();
        let original = Path::new(&document.source).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let lines = [
            format!("Thèmes existants (identifiant : nom) :\n{}", list(themes.iter().map(|t| format!("- {} : {}", t.id, t.title)).collect())),
            format!("Projets existants (identifiant : nom) :\n{}", list(projects.iter().map(|(id, title)| format!("- {id} : {title}")).collect())),
            format!("Tags déjà utilisés : {}", if tags.is_empty() { "aucun".into() } else { tags.iter().take(150).map(|t| t.tag.as_str()).collect::<Vec<_>>().join(", ") }),
            String::new(),
            format!("Fichier : {original} ({}{})", document.format, document.pages.map(|p| format!(", {p} pages")).unwrap_or_default()),
            format!("Titre actuel : {}", document.title),
            format!("Titres du document :\n{}", list(headings)),
            "=== Début du document ===".into(),
            head(body.trim(), CLASSIFY_CHARS),
            "=== Fin de l'extrait ===".into(),
            "Réponds avec l'objet json demandé.".into(),
        ];
        let mut proposal: Classification = chat_json(
            self.llm.as_ref(),
            &[Msg::system(LIBRARY_CLASSIFY_SYSTEM), Msg::user(lines.join("\n"))],
            &self.settings.model_classify,
        )?;
        // thèmes : identifiant ou nom d'un thème existant, sinon suggestion de création
        let mut kept = Vec::new();
        let mut suggested = proposal.new_themes.clone();
        for value in &proposal.themes {
            match themes.iter().find(|t| t.id == value.trim() || fold(&t.title) == fold(value)) {
                Some(theme) => kept.push(theme.id.clone()),
                None => suggested.push(value.clone()),
            }
        }
        proposal.themes = dedup(&kept);
        proposal.new_themes = dedup(&suggested)
            .into_iter()
            .filter(|s| !themes.iter().any(|t| fold(&t.title) == fold(s) || t.id == *s))
            .take(3)
            .collect();
        proposal.projects = dedup(
            &proposal
                .projects
                .iter()
                .filter_map(|p| projects.iter().find(|(id, title)| id == p.trim() || fold(title) == fold(p)).map(|(id, _)| id.clone()))
                .collect::<Vec<_>>(),
        );
        proposal.tags = normalize_tags(&proposal.tags).into_iter().take(MAX_TAGS).collect();
        if proposal.title.trim().is_empty() {
            proposal.title = document.title.clone();
        }
        proposal.title = proposal.title.trim().to_string();
        proposal.summary = proposal.summary.trim().to_string();
        proposal.replaces = self.replaces_for(&document);
        Ok(proposal)
    }

    /// Valide ou modifie le classement d'un document : frontmatter écrit, `status` retiré.
    /// Avec `replaces`, le document prend la place de celui qu'il remplace (même nom, liens
    /// valides) ; l'ancienne source et l'ancien pivot partent dans `.trash/`.
    pub fn update(&self, id: &str, changes: &DocumentMeta) -> Result<Document> {
        let title = changes.title.trim();
        if title.is_empty() {
            return Err(vault_err("Le titre du document est obligatoire"));
        }
        let known_themes = self.themes()?;
        let themes = dedup(&changes.themes);
        if let Some(unknown) = themes.iter().find(|t| !known_themes.iter().any(|k| &k.id == *t)) {
            return Err(vault_err(format!("Thème inconnu : {unknown} (à créer d'abord dans themes/)")));
        }
        let known_projects = self.projects()?;
        let projects = dedup(&changes.projects);
        if let Some(unknown) = projects.iter().find(|p| !known_projects.iter().any(|(id, _)| id == *p)) {
            return Err(vault_err(format!("Projet inconnu : {unknown}")));
        }
        let _guard = self.write_lock.lock().unwrap();
        let (document, mut meta, body) = self.read_pivot(id)?;
        meta.insert("title".into(), title.into());
        meta.insert("themes".into(), json!(themes));
        meta.insert("projects".into(), json!(projects));
        meta.insert("tags".into(), json!(normalize_tags(&changes.tags)));
        meta.insert("summary".into(), changes.summary.trim().into());
        meta.shift_remove("status");
        let target = changes.replaces.as_deref().map(str::trim).filter(|t| !t.is_empty() && *t != id);
        let Some(target) = target else {
            self.write_pivot(id, &meta, &body)?;
            self.reindex(id)?;
            self.index.touch();
            return self.get(id);
        };
        // remplacement : le nouveau document reprend le nom (et la date d'ajout) de l'ancien
        let (old, old_meta, _) = self.read_pivot(target)?;
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
        self.trash(&self.source_path(&old)?, &stamp)?;
        self.trash(&self.pivot_path(target), &stamp)?;
        let current_source = self.source_path(&document)?;
        let ext = current_source.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
        let mut source = format!("{SOURCES_DIR}/{target}.{ext}");
        let mut n = 2;
        while self.root.join(&source).exists() && self.root.join(&source) != current_source {
            source = format!("{SOURCES_DIR}/{target} ({n}).{ext}");
            n += 1;
        }
        std::fs::rename(&current_source, self.root.join(&source))?;
        meta.insert("source".into(), source.into());
        if let Some(added) = old_meta.get("added") {
            meta.insert("added".into(), added.clone());
        }
        meta.insert("updated".into(), today().to_string().into());
        self.write_pivot(target, &meta, &body)?;
        std::fs::remove_file(self.pivot_path(id))?;
        self.reindex(id)?;
        self.reindex(target)?;
        self.index.touch();
        self.get(target)
    }

    /// Supprime un document : pivot et source dans `.trash/` (récupérables).
    pub fn delete(&self, id: &str) -> Result<()> {
        let _guard = self.write_lock.lock().unwrap();
        let (document, _, _) = self.read_pivot(id)?;
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
        self.trash(&self.source_path(&document)?, &stamp)?;
        self.trash(&self.pivot_path(id), &stamp)?;
        self.reindex(id)?;
        self.refresh_untracked();
        self.index.touch();
        Ok(())
    }

    /// Régénère le texte converti à partir du fichier d'origine, sans toucher au classement.
    pub fn reconvert(&self, id: &str) -> Result<Document> {
        let (document, mut meta, _) = self.read_pivot(id)?;
        let bytes = std::fs::read(self.source_path(&document)?)
            .map_err(|_| not_found(format!("Fichier d'origine introuvable : {}", document.source)))?;
        let format = format_of(&document.source).unwrap_or(document.format.as_str()).to_string();
        let converted = convert::convert(&bytes, &format, &self.tools)?;
        match converted.pages {
            Some(pages) => meta.insert("pages".into(), (pages as u64).into()),
            None => meta.shift_remove("pages"),
        };
        let _guard = self.write_lock.lock().unwrap();
        self.write_pivot(id, &meta, &converted.markdown)?;
        self.reindex(id)?;
        self.index.touch();
        self.get(id)
    }
}

/// Début d'un fichier (frontmatter d'un pivot), sans lire les gros corps en entier.
fn read_head(path: &Path, limit: usize) -> std::io::Result<String> {
    let mut buffer = vec![0u8; limit];
    let mut file = std::fs::File::open(path)?;
    let mut read = 0;
    while read < limit {
        let n = file.read(&mut buffer[read..])?;
        if n == 0 {
            break;
        }
        read += n;
    }
    buffer.truncate(read);
    Ok(String::from_utf8_lossy(&buffer).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passages_coupes_aux_titres_et_aux_pages() {
        let body = "\n# Spécification\n\nIntroduction courte.\n\n## Flux\n\nLe flux A.\n\n```\n# pas un titre\n```\n\n\
                    <!-- page 2 -->\n\nSuite du flux.\n\n# Annexes\n\nTableau.\n";
        let chunks = chunk_document(body);
        let summary: Vec<(&str, Option<u32>, &str)> =
            chunks.iter().map(|c| (c.section.as_str(), c.page, c.text.as_str())).collect();
        assert_eq!(
            summary,
            vec![
                ("Spécification", None, "Introduction courte."),
                ("Spécification › Flux", None, "Le flux A.\n\n```\n# pas un titre\n```"),
                ("Spécification › Flux", Some(2), "Suite du flux."),
                ("Annexes", Some(2), "Tableau."),
            ]
        );
    }

    #[test]
    fn longs_paragraphes_decoupes() {
        let long = "mot ".repeat(1000);
        let chunks = chunk_document(&format!("Court.\n\n{long}\n\nFin."));
        assert!(chunks.len() >= 4, "{}", chunks.len());
        assert!(chunks.iter().all(|c| c.text.chars().count() <= CHUNK_CHARS));
        assert_eq!(chunks.first().unwrap().text, "Court.");
        assert_eq!(chunks.last().unwrap().text, "Fin.");
    }

    #[test]
    fn table_des_matieres() {
        let entries = outline("# A\n\ntexte\n\n## B\n```\n# code\n```\n### C ###\n");
        assert_eq!(
            entries,
            vec![
                OutlineEntry { level: 1, title: "A".into() },
                OutlineEntry { level: 2, title: "B".into() },
                OutlineEntry { level: 3, title: "C".into() },
            ]
        );
    }

    #[test]
    fn tags_et_copies() {
        assert_eq!(normalize_tag(" #Compte Rendu "), "compte-rendu");
        assert_eq!(normalize_tags(&["SAP".into(), "sap".into(), " ".into()]), vec!["sap".to_string()]);
        assert_eq!(base_of_copy("Spec interface (2)").as_deref(), Some("Spec interface"));
        assert_eq!(base_of_copy("Spec interface"), None);
        assert!(check_id("../x").is_err() && check_id(".cache").is_err() && check_id("Spec (2)").is_ok());
    }

    #[test]
    fn frontmatter_d_un_pivot() {
        let (meta, _) = split_frontmatter(
            "---\ntype: document\ntitle: Spec\nthemes: [interfaces]\ntags: ['#SAP']\nsize: 12\npages: 3\nstatus: a_classer\n---\n",
        );
        let document = document_from("Spec", &meta).unwrap();
        assert_eq!(document.themes, vec!["interfaces"]);
        assert_eq!(document.tags, vec!["SAP"]);
        assert_eq!((document.size, document.pages, document.pending), (12, Some(3), true));
        let (note, _) = split_frontmatter("---\ntype: note\n---\n");
        assert!(document_from("x", &note).is_none());
    }
}
