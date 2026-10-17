"""Tâches : un fichier par tâche dans taches/, avec une section Historique datée."""

from __future__ import annotations

import datetime as dt
import re

from .index import Index
from .vault import (NotFoundError, Vault, VaultError, append_to_section, join_frontmatter,
                    jsonable, section_lines, slugify, split_frontmatter)

STATUSES = ["a_faire", "en_cours", "attente", "bloquee", "terminee"]
PRIORITIES = ["basse", "normale", "haute"]
OPEN_STATUSES = [s for s in STATUSES if s != "terminee"]
HISTORY_HEADING = "## Historique"
HISTORY_LINE_RE = re.compile(r"^-\s*(\d{4}-\d{2}-\d{2})\s*:\s*(.+)$")
TASK_ID_RE = re.compile(r"^T-(\d+)", re.I)
EDITABLE_FIELDS = {"title", "status", "priority", "project", "due"}


def _today() -> str:
    return dt.date.today().isoformat()


def _check(field: str, value, allowed: list[str]) -> None:
    if value is not None and value not in allowed:
        raise VaultError(f"Valeur invalide pour {field} : {value} (attendu : {', '.join(allowed)})")


def _parse_date(value) -> dt.date:
    try:
        return dt.date.fromisoformat(str(value)[:10])
    except ValueError as exc:
        raise VaultError(f"Date invalide : {value} (format AAAA-MM-JJ)") from exc


def parse_history(text: str) -> list[dict]:
    events = []
    for line in section_lines(text, HISTORY_HEADING):
        match = HISTORY_LINE_RE.match(line.strip())
        if match:
            events.append({"date": match.group(1), "text": match.group(2).strip()})
    return events


class TaskService:
    folder = "taches"

    def __init__(self, vault: Vault, index: Index):
        self.vault = vault
        self.index = index

    @staticmethod
    def _summary(note: dict) -> dict:
        meta = note["meta"]
        return {
            "id": note["alias"] or note["stem"],
            "title": note["title"],
            "status": meta.get("status", "a_faire"),
            "priority": meta.get("priority", "normale"),
            "project": meta.get("project"),
            "due": str(meta["due"]) if meta.get("due") else None,
            "created": meta.get("created"),
            "path": note["path"],
        }

    def list(self, status: str | None = None, project: str | None = None) -> list[dict]:
        tasks = [self._summary(n) for n in self.index.list_notes(type="tache")]
        if status:
            tasks = [t for t in tasks if t["status"] == status]
        if project:
            tasks = [t for t in tasks if t["project"] == project]
        return sorted(tasks, key=lambda t: t["id"])

    def open_tasks(self) -> list[dict]:
        return [t for t in self.list() if t["status"] in OPEN_STATUSES]

    def path_of(self, task_id: str) -> str:
        path = self.index.resolve_link(task_id)
        if not path:
            raise NotFoundError(f"Tâche introuvable : {task_id}")
        return path

    def get(self, task_id: str) -> dict:
        path = self.path_of(task_id)
        note = self.index.get(path)
        assert note
        text = self.vault.read_text(path)
        return {**self._summary(note), "content": text, "history": parse_history(text)}

    def next_id(self) -> str:
        numbers = [0]
        for note in self.index.list_notes(type="tache"):
            for candidate in (note["alias"], note["stem"]):
                match = TASK_ID_RE.match(candidate or "")
                if match:
                    numbers.append(int(match.group(1)))
        folder = self.vault.root / self.folder
        if folder.is_dir():
            for f in folder.glob("T-*.md"):
                match = TASK_ID_RE.match(f.stem)
                if match:
                    numbers.append(int(match.group(1)))
        return f"T-{max(numbers) + 1:03d}"

    def create(self, title: str, project: str | None = None, priority: str = "normale",
               status: str = "a_faire", due: str | None = None, description: str = "",
               origin: str | None = None, today: str | None = None) -> dict:
        title = title.strip()
        if not title:
            raise VaultError("Le titre de la tâche est obligatoire")
        _check("status", status, STATUSES)
        _check("priority", priority, PRIORITIES)
        today = today or _today()
        task_id = self.next_id()
        meta = {"id": task_id, "type": "tache", "title": title, "status": status,
                "priority": priority, "project": project or None,
                "due": _parse_date(due) if due else None,
                "created": today}
        meta = {k: v for k, v in meta.items() if v is not None}
        created = "créée" + (f" depuis [[{origin}]]" if origin else "")
        body = f"\n{description.strip()}\n\n" if description.strip() else "\n"
        body += f"{HISTORY_HEADING}\n- {today} : {created}\n"
        rel = self.vault.unique_path(self.folder, f"{task_id}-{slugify(title, 40)}")
        self.vault.write(rel, join_frontmatter(meta, body), create_only=True)
        self.index.upsert(rel)
        return self.get(task_id)

    def update(self, task_id: str, changes: dict | None = None, note: str | None = None,
               today: str | None = None) -> dict:
        changes = {k: v for k, v in (changes or {}).items() if k in EDITABLE_FIELDS}
        _check("status", changes.get("status"), STATUSES)
        _check("priority", changes.get("priority"), PRIORITIES)
        today = today or _today()
        path = self.path_of(task_id)
        text = self.vault.read_text(path)
        meta, body = split_frontmatter(text)
        history = []
        old_status = jsonable(meta.get("status", "a_faire"))
        for key, value in changes.items():
            if value in ("", None):
                meta.pop(key, None)
            else:
                meta[key] = _parse_date(value) if key == "due" else value
        new_status = meta.get("status", "a_faire")
        if "status" in changes and new_status != old_status:
            history.append(f"statut {old_status} → {new_status}")
        if note and note.strip():
            history.append(note.strip())
        new_text = join_frontmatter(meta, body)
        if history:
            new_text = append_to_section(new_text, HISTORY_HEADING, f"- {today} : {' — '.join(history)}")
        if new_text != text:
            self.vault.write(path, new_text)
            self.index.upsert(path)
        return self.get(task_id)

    def events(self, start: str, end: str) -> list[dict]:
        """Événements d'historique de toutes les tâches entre deux dates (incluses)."""
        result = []
        for task in self.list():
            text = self.vault.read_text(task["path"])
            for event in parse_history(text):
                if start <= event["date"] <= end:
                    result.append({**event, "task_id": task["id"], "title": task["title"],
                                   "status": task["status"], "project": task["project"]})
        return sorted(result, key=lambda e: (e["date"], e["task_id"]))
