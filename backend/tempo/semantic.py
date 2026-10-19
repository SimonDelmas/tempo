"""Recherche sémantique : embeddings des notes dans sqlite-vec (<vault>/.tempo/vectors.db).

Comme l'index plein texte, c'est un cache reconstructible : chaque note est découpée en
passages, vectorisés par une API d'embeddings compatible OpenAI ; une note n'est revectorisée
que si son contenu a changé (hash de l'index). Sans modèle d'embeddings configuré
(TEMPO_EMBED_MODEL), la fonctionnalité est simplement désactivée."""

import hashlib
import re
import sqlite3
import struct
import threading
from collections.abc import Callable
from pathlib import Path

from .config import Settings
from .index import Index
from .llm import LLMError
from .vault import Vault, split_frontmatter, strip_section

CHUNK_CHARS = 1200
MAX_CHUNKS_PER_NOTE = 20
BATCH_SIZE = 32
SKIPPED_TYPES = ("modele", "inbox")

Embedder = Callable[[list[str]], list[list[float]]]


def openai_embedder(settings: Settings) -> Embedder:
    import httpx
    from openai import OpenAI, OpenAIError

    http_client = httpx.Client(verify=settings.llm_ca_bundle) if settings.llm_ca_bundle else None
    client = OpenAI(base_url=settings.embed_base_url, api_key=settings.embed_api_key, timeout=120,
                    http_client=http_client)

    def embed(texts: list[str]) -> list[list[float]]:
        try:
            response = client.embeddings.create(model=settings.embed_model, input=texts)
        except OpenAIError as exc:
            raise LLMError(f"Erreur de l'API d'embeddings : {exc}") from exc
        return [d.embedding for d in sorted(response.data, key=lambda d: d.index)]

    return embed


def hashing_embedder(dim: int = 256) -> Embedder:
    """Embeddings factices (sac de mots haché), pour les tests."""
    def embed(texts: list[str]) -> list[list[float]]:
        vectors = []
        for text in texts:
            v = [0.0] * dim
            for word in re.findall(r"\w{3,}", text.lower()):
                v[int(hashlib.md5(word.encode()).hexdigest(), 16) % dim] += 1.0
            norm = sum(x * x for x in v) ** 0.5 or 1.0
            vectors.append([x / norm for x in v])
        return vectors
    return embed


def chunk_text(title: str, body: str) -> list[str]:
    """Passages d'environ CHUNK_CHARS caractères, coupés entre paragraphes, titre en tête."""
    paragraphs = [p.strip() for p in re.split(r"\n\s*\n", body) if p.strip()]
    chunks, current = [], ""
    for paragraph in paragraphs:
        while len(paragraph) > CHUNK_CHARS:
            if current:
                chunks.append(current)
                current = ""
            chunks.append(paragraph[:CHUNK_CHARS])
            paragraph = paragraph[CHUNK_CHARS:]
        if current and len(current) + len(paragraph) > CHUNK_CHARS:
            chunks.append(current)
            current = ""
        current = f"{current}\n\n{paragraph}" if current else paragraph
    if current:
        chunks.append(current)
    return [f"{title}\n\n{c}" for c in chunks[:MAX_CHUNKS_PER_NOTE]] or [title]


class SemanticIndex:
    def __init__(self, vault: Vault, index: Index, embedder: Embedder | None, db_path: Path,
                 model: str | None = None):
        self.vault = vault
        self.index = index
        self.embedder = embedder
        self.model = model
        self.error: str | None = None
        self.lock = threading.RLock()
        self._sync_lock = threading.Lock()  # une seule synchronisation à la fois
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None
        self.conn: sqlite3.Connection | None = None
        if embedder is None:
            return
        import sqlite_vec

        db_path.parent.mkdir(parents=True, exist_ok=True)
        self.conn = sqlite3.connect(str(db_path), check_same_thread=False)
        self.conn.enable_load_extension(True)
        sqlite_vec.load(self.conn)
        self.conn.enable_load_extension(False)
        self.conn.executescript("""
            CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT);
            CREATE TABLE IF NOT EXISTS chunks(id INTEGER PRIMARY KEY, path TEXT, hash TEXT, text TEXT);
            CREATE INDEX IF NOT EXISTS chunks_path ON chunks(path);
        """)
        # changement de modèle : les vecteurs ne sont plus comparables
        if self._meta("model") not in (None, model or ""):
            self._reset()
        self._set_meta("model", model or "")

    @property
    def enabled(self) -> bool:
        return self.conn is not None

    # --- stockage -----------------------------------------------------------

    def _meta(self, key: str) -> str | None:
        assert self.conn
        row = self.conn.execute("SELECT value FROM meta WHERE key = ?", (key,)).fetchone()
        return row[0] if row else None

    def _set_meta(self, key: str, value: str) -> None:
        assert self.conn
        self.conn.execute("INSERT OR REPLACE INTO meta VALUES (?, ?)", (key, value))
        self.conn.commit()

    def _reset(self) -> None:
        assert self.conn
        self.conn.execute("DROP TABLE IF EXISTS vec_chunks")
        self.conn.execute("DELETE FROM chunks")
        self.conn.execute("DELETE FROM meta WHERE key = 'dim'")
        self.conn.commit()

    def _ensure_vec_table(self, dim: int) -> None:
        assert self.conn
        current = self._meta("dim")
        if current is not None and int(current) != dim:
            self._reset()
            current = None
        if current is None:
            self.conn.execute(f"CREATE VIRTUAL TABLE IF NOT EXISTS vec_chunks USING vec0("
                              f"embedding float[{dim}] distance_metric=cosine)")
            self._set_meta("dim", str(dim))

    @staticmethod
    def _pack(vector: list[float]) -> bytes:
        return struct.pack(f"{len(vector)}f", *vector)

    def _delete_note(self, path: str) -> None:
        assert self.conn
        ids = [r[0] for r in self.conn.execute("SELECT id FROM chunks WHERE path = ?", (path,))]
        if ids and self._meta("dim"):
            marks = ",".join("?" * len(ids))
            self.conn.execute(f"DELETE FROM vec_chunks WHERE rowid IN ({marks})", ids)
        self.conn.execute("DELETE FROM chunks WHERE path = ?", (path,))

    # --- synchronisation ----------------------------------------------------

    def _notes(self) -> list[dict]:
        return [n for n in self.index.list_notes() if n["type"] not in SKIPPED_TYPES]

    def _note_chunks(self, note: dict) -> list[str]:
        _, body = split_frontmatter(self.vault.read_text(note["path"]))
        return chunk_text(note["title"], strip_section(body, "## Texte original").strip())

    def sync(self) -> int:
        """Vectorise les notes nouvelles ou modifiées ; renvoie le nombre de notes traitées."""
        if not self.enabled or not self.embedder:
            return 0
        with self._sync_lock:
            return self._sync()

    def _sync(self) -> int:
        assert self.embedder
        assert self.conn
        with self.lock:
            notes = {n["path"]: n for n in self._notes()}
            stored = dict(self.conn.execute("SELECT path, hash FROM chunks GROUP BY path").fetchall())
            for path in stored:
                if path not in notes:
                    self._delete_note(path)
            self.conn.commit()
        todo = [n for p, n in notes.items() if stored.get(p) != n["hash"]]
        done = 0
        try:
            for start in range(0, len(todo), 8):
                # appels à l'API hors verrou : la recherche reste disponible pendant la synchro
                batch = todo[start:start + 8]
                pieces = [(note, text) for note in batch for text in self._note_chunks(note)]
                vectors: list[list[float]] = []
                for i in range(0, len(pieces), BATCH_SIZE):
                    vectors += self.embedder([t for _, t in pieces[i:i + BATCH_SIZE]])
                with self.lock:
                    if vectors:
                        self._ensure_vec_table(len(vectors[0]))
                    for note in batch:
                        self._delete_note(note["path"])
                    for (note, text), vector in zip(pieces, vectors):
                        cursor = self.conn.execute("INSERT INTO chunks(path, hash, text) VALUES (?,?,?)",
                                                   (note["path"], note["hash"], text))
                        self.conn.execute("INSERT INTO vec_chunks(rowid, embedding) VALUES (?, ?)",
                                          (cursor.lastrowid, self._pack(vector)))
                    self.conn.commit()
                done += len(batch)
            self.error = None
        except (LLMError, OSError, ValueError) as exc:
            with self.lock:
                self.conn.rollback()
            self.error = str(exc)
        return done

    def start(self, interval: float = 15) -> None:
        """Synchronisation en arrière-plan, à chaque changement de l'index."""
        if not self.enabled or self._thread:
            return

        def loop() -> None:
            last = -1
            while not self._stop.is_set():
                if self.index.revision != last:
                    last = self.index.revision
                    self.sync()
                self._stop.wait(interval)

        self._thread = threading.Thread(target=loop, name="tempo-embeddings", daemon=True)
        self._thread.start()

    def stop(self) -> None:
        self._stop.set()
        if self._thread:
            self._thread.join(timeout=2)
            self._thread = None

    def status(self) -> dict:
        if not self.enabled:
            return {"enabled": False, "model": None, "indexed": 0, "total": 0, "error": None}
        assert self.conn
        with self.lock:
            notes = self._notes()
            stored = dict(self.conn.execute("SELECT path, hash FROM chunks GROUP BY path").fetchall())
        indexed = sum(1 for n in notes if stored.get(n["path"]) == n["hash"])
        return {"enabled": True, "model": self.model, "indexed": indexed, "total": len(notes),
                "error": self.error}

    # --- recherche ----------------------------------------------------------

    def _nearest(self, vector: list[float], k: int, exclude: set[str]) -> list[dict]:
        assert self.conn
        if not self._meta("dim"):
            return []
        with self.lock:
            rows = self.conn.execute(
                "SELECT c.path, c.text, v.distance FROM "
                "(SELECT rowid, distance FROM vec_chunks WHERE embedding MATCH ? AND k = ?) v "
                "JOIN chunks c ON c.id = v.rowid ORDER BY v.distance",
                (self._pack(vector), k * 4 + len(exclude) * 2),
            ).fetchall()
        best: dict[str, tuple[float, str]] = {}
        for path, text, distance in rows:
            if path not in exclude and (path not in best or distance < best[path][0]):
                best[path] = (distance, text)
        results = []
        for path, (distance, text) in sorted(best.items(), key=lambda item: item[1][0])[:k]:
            note = self.index.get(path)
            if not note:
                continue
            # extrait sans le titre ajouté au passage ni les lignes de titre Markdown
            body = text.split("\n\n", 1)[-1]
            snippet = " ".join(" ".join(line for line in body.splitlines()
                                        if not line.lstrip().startswith("#")).split())
            results.append({"path": path, "title": note["title"], "type": note["type"],
                            "date": note["date"], "alias": note["alias"], "stem": note["stem"],
                            "snippet": snippet[:220] + ("…" if len(snippet) > 220 else ""),
                            "score": round(1 - distance, 3)})
        return results

    def search(self, query: str, k: int = 10) -> list[dict]:
        if not self.enabled or not self.embedder or not query.strip():
            return []
        return self._nearest(self.embedder([query])[0], k, set())

    def related(self, path: str, k: int = 8) -> list[dict]:
        """Notes proches d'une note et pas encore liées à elle (suggestions de liens)."""
        if not self.enabled:
            return []
        assert self.conn
        with self.lock:
            dim = self._meta("dim")
            blobs = [r[0] for r in self.conn.execute(
                "SELECT v.embedding FROM chunks c JOIN vec_chunks v ON v.rowid = c.id "
                "WHERE c.path = ?", (path,))] if dim else []
        if not blobs or not dim:
            return []
        n = int(dim)
        vectors = [struct.unpack(f"{n}f", blob) for blob in blobs]
        mean = [sum(col) / len(vectors) for col in zip(*vectors)]
        linked = {path} | {x["path"] for x in self.index.outlinks(path)} \
            | {x["path"] for x in self.index.backlinks(path)}
        return self._nearest(mean, k, linked)
