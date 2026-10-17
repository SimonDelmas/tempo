"""Index SQLite du vault : métadonnées, liens et recherche plein texte.

L'index est un cache : il est reconstruit au démarrage et tenu à jour par le watcher.
"""

import json
import re
import sqlite3
import threading
from pathlib import Path

from watchdog.events import FileSystemEventHandler
from watchdog.observers import Observer

from .vault import Vault, content_hash, jsonable, split_frontmatter

WIKILINK_RE = re.compile(r"\[\[([^\]\|#\n]+)(?:#[^\]\|\n]*)?(?:\|[^\]\n]*)?\]\]")
DATE_PREFIX_RE = re.compile(r"^(\d{4}-\d{2}-\d{2})")

FOLDER_TYPES = {
    "00-inbox": "inbox",
    "journal": "journal",
    "reunions": "reunion",
    "echanges": "echange",
    "taches": "tache",
    "projets": "projet",
    "sujets": "sujet",
    "personnes": "personne",
    "rapports": "rapport",
    "_templates": "modele",
}

SCHEMA = """
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
"""


def extract_links(text: str) -> list[str]:
    return sorted({m.group(1).strip() for m in WIKILINK_RE.finditer(text)})


def note_title(meta: dict, body: str, stem: str) -> str:
    if meta.get("title"):
        return str(meta["title"])
    if meta.get("subject"):
        return str(meta["subject"])
    match = re.search(r"^#\s+(.+)$", body, re.M)
    return match.group(1).strip() if match else stem


def infer_type(rel: str) -> str:
    first = rel.split("/", 1)[0]
    return FOLDER_TYPES.get(first, "note") if "/" in rel else "note"


class Index:
    def __init__(self, vault: Vault, db_path: Path):
        self.vault = vault
        db_path.parent.mkdir(parents=True, exist_ok=True)
        self.conn = sqlite3.connect(str(db_path), check_same_thread=False)
        self.conn.row_factory = sqlite3.Row
        self.lock = threading.RLock()
        self.revision = 0
        self._observer = None
        with self.lock:
            self.conn.executescript(SCHEMA)

    # --- écriture -----------------------------------------------------------

    def rebuild(self) -> int:
        files = self.vault.list_files()
        with self.lock:
            self.conn.execute("DELETE FROM notes")
            self.conn.execute("DELETE FROM links")
            self.conn.execute("DELETE FROM notes_fts")
            for rel in files:
                self._upsert(rel)
            self.conn.commit()
            self.revision += 1
        return len(files)

    def upsert(self, rel: str) -> None:
        with self.lock:
            self._upsert(rel)
            self.conn.commit()
            self.revision += 1

    def remove(self, rel: str) -> None:
        with self.lock:
            self._delete(rel)
            self.conn.commit()
            self.revision += 1

    def _delete(self, rel: str) -> None:
        self.conn.execute("DELETE FROM notes WHERE path = ?", (rel,))
        self.conn.execute("DELETE FROM links WHERE src = ?", (rel,))
        self.conn.execute("DELETE FROM notes_fts WHERE path = ?", (rel,))

    def _upsert(self, rel: str) -> None:
        path = self.vault.resolve(rel)
        text = path.read_text(encoding="utf-8")
        meta, body = split_frontmatter(text)
        meta: dict = jsonable(meta)  # type: ignore[assignment]
        stem = path.stem
        date_match = DATE_PREFIX_RE.match(stem)
        date = str(meta.get("date") or (date_match.group(1) if date_match else ""))[:10]

        self._delete(rel)
        self.conn.execute(
            "INSERT INTO notes VALUES (?,?,?,?,?,?,?,?,?)",
            (
                rel, stem, str(meta.get("id") or ""), note_title(meta, body, stem),
                str(meta.get("type") or infer_type(rel)), date,
                json.dumps(meta, ensure_ascii=False), content_hash(text), path.stat().st_mtime,
            ),
        )
        self.conn.executemany(
            "INSERT INTO links VALUES (?,?)", [(rel, t.lower()) for t in extract_links(text)]
        )
        self.conn.execute(
            "INSERT INTO notes_fts VALUES (?,?,?)", (rel, note_title(meta, body, stem), body)
        )

    def sync_path(self, path: Path) -> None:
        """Appelé par le watcher pour un fichier créé / modifié / supprimé."""
        if Path(path).suffix.lower() != ".md" or self.vault.is_ignored(path):
            return
        try:
            rel = self.vault.rel(path)
            if Path(path).is_file():
                self.upsert(rel)
            else:
                self.remove(rel)
        except (OSError, ValueError, UnicodeDecodeError):
            pass

    # --- lecture ------------------------------------------------------------

    @staticmethod
    def _row(row: sqlite3.Row) -> dict:
        data = dict(row)
        if "meta" in data:
            data["meta"] = json.loads(data["meta"] or "{}")
        return data

    def _query(self, sql: str, params=()) -> list[dict]:
        with self.lock:
            return [self._row(r) for r in self.conn.execute(sql, params).fetchall()]

    def get(self, rel: str) -> dict | None:
        rows = self._query("SELECT * FROM notes WHERE path = ?", (rel,))
        return rows[0] if rows else None

    def list_notes(self, type: str | None = None, start: str | None = None,
                   end: str | None = None) -> list[dict]:
        sql, params = "SELECT * FROM notes WHERE 1=1", []
        if type:
            sql += " AND type = ?"
            params.append(type)
        if start:
            sql += " AND date >= ? AND date != ''"
            params.append(start)
        if end:
            sql += " AND date <= ? AND date != ''"
            params.append(end)
        return self._query(sql + " ORDER BY date, path", params)

    def modified_between(self, start_ts: float, end_ts: float) -> list[dict]:
        return self._query(
            "SELECT * FROM notes WHERE mtime >= ? AND mtime <= ? ORDER BY mtime", (start_ts, end_ts)
        )

    def resolve_link(self, name: str) -> str | None:
        key = name.strip().lower()
        for column in ("alias", "stem", "title"):
            rows = self._query(f"SELECT path FROM notes WHERE lower({column}) = ? LIMIT 1", (key,))
            if rows:
                return rows[0]["path"]
        return None

    def backlinks(self, rel: str) -> list[dict]:
        note = self.get(rel)
        if not note:
            return []
        keys = [k.lower() for k in (note["stem"], note["alias"], note["title"]) if k]
        marks = ",".join("?" * len(keys))
        return self._query(
            f"SELECT DISTINCT n.* FROM links l JOIN notes n ON n.path = l.src "
            f"WHERE l.target IN ({marks}) AND l.src != ? ORDER BY n.date DESC, n.path",
            (*keys, rel),
        )

    def outlinks(self, rel: str) -> list[dict]:
        with self.lock:
            targets = [r[0] for r in self.conn.execute(
                "SELECT DISTINCT target FROM links WHERE src = ?", (rel,))]
        result, seen = [], set()
        for target in targets:
            path = self.resolve_link(target)
            if path and path not in seen:
                seen.add(path)
                result.append(self.get(path))
        return [r for r in result if r]

    def find_by_meta(self, key: str, value: str) -> list[dict]:
        return self._query(
            "SELECT * FROM notes WHERE json_extract(meta, ?) = ?", (f"$.{key}", value)
        )

    def search(self, query: str, limit: int = 30) -> list[dict]:
        words = re.findall(r"\w+", query)
        if not words:
            return []
        fts = " ".join(f'"{w}"*' for w in words)
        with self.lock:
            rows = self.conn.execute(
                "SELECT n.path, n.title, n.type, n.date, "
                "snippet(notes_fts, 2, '«', '»', '…', 12) AS snippet "
                "FROM notes_fts JOIN notes n ON n.path = notes_fts.path "
                "WHERE notes_fts MATCH ? ORDER BY rank LIMIT ?",
                (fts, limit),
            ).fetchall()
        return [dict(r) for r in rows]

    # --- surveillance du dossier --------------------------------------------

    def start_watching(self) -> None:
        index = self

        class Handler(FileSystemEventHandler):
            def on_any_event(self, event):
                if event.is_directory:
                    return
                for attr in ("src_path", "dest_path"):
                    path = getattr(event, attr, None)
                    if path:
                        index.sync_path(Path(path))

        self._observer = Observer()
        self._observer.schedule(Handler(), str(self.vault.root), recursive=True)
        self._observer.daemon = True
        self._observer.start()

    def stop_watching(self) -> None:
        if self._observer:
            self._observer.stop()
            self._observer.join(timeout=2)
            self._observer = None
