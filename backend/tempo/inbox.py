"""Inbox : résumés de mails / Teams → propositions du LLM → notes et tâches."""

from __future__ import annotations

import datetime as dt
import re
from typing import Literal

from pydantic import BaseModel, Field, field_validator

from .config import Settings
from .index import Index
from .journal import JournalService
from .llm import LLM
from .prompts import INBOX_SYSTEM
from .tasks import PRIORITIES, STATUSES, TaskService
from .vault import Vault, VaultError, content_hash, join_frontmatter, slugify

INBOX_FOLDER = "00-inbox"
SEPARATOR_RE = re.compile(r"^\s*-{3,}\s*$", re.M)


def _valid_date(value: str | None) -> str | None:
    if not value:
        return None
    try:
        return dt.date.fromisoformat(str(value)[:10]).isoformat()
    except ValueError:
        return None


class EchangeProposal(BaseModel):
    source: Literal["mail", "teams", "autre"] = "autre"
    date: str = Field(default_factory=lambda: dt.date.today().isoformat())
    subject: str = "Échange"
    participants: list[str] = []
    projects: list[str] = []
    thread: str = ""
    summary: str = ""

    @field_validator("source", mode="before")
    @classmethod
    def _source(cls, v):
        v = str(v or "").lower()
        return v if v in ("mail", "teams") else "autre"

    @field_validator("date", mode="before")
    @classmethod
    def _date(cls, v):
        return _valid_date(v) or dt.date.today().isoformat()


class NewTaskProposal(BaseModel):
    accepted: bool = True
    title: str
    description: str = ""
    project: str | None = None
    priority: str = "normale"
    status: str = "a_faire"
    due: str | None = None

    @field_validator("priority", mode="before")
    @classmethod
    def _priority(cls, v):
        return v if v in PRIORITIES else "normale"

    @field_validator("status", mode="before")
    @classmethod
    def _status(cls, v):
        return v if v in STATUSES else "a_faire"

    @field_validator("due", mode="before")
    @classmethod
    def _due(cls, v):
        return _valid_date(v)


class TaskUpdateProposal(BaseModel):
    accepted: bool = True
    task_id: str
    status: str | None = None
    note: str = ""

    @field_validator("status", mode="before")
    @classmethod
    def _status(cls, v):
        return v if v in STATUSES else None


class InboxProposal(BaseModel):
    echange: EchangeProposal
    new_tasks: list[NewTaskProposal] = []
    task_updates: list[TaskUpdateProposal] = []
    journal_line: str = ""
    duplicate_of: str | None = None


def split_summaries(text: str) -> list[str]:
    return [part.strip() for part in SEPARATOR_RE.split(text) if part.strip()]


class InboxService:
    def __init__(self, vault: Vault, index: Index, tasks: TaskService, journal: JournalService,
                 llm: LLM, settings: Settings):
        self.vault = vault
        self.index = index
        self.tasks = tasks
        self.journal = journal
        self.llm = llm
        self.settings = settings

    def list(self) -> list[dict]:
        folder = self.vault.root / INBOX_FOLDER
        if not folder.is_dir():
            return []
        items = []
        for f in sorted(folder.glob("*.md")):
            text = f.read_text(encoding="utf-8")
            items.append({"path": self.vault.rel(f), "content": text,
                          "preview": " ".join(text.split())[:160]})
        return items

    def add(self, text: str) -> list[str]:
        parts = split_summaries(text)
        if not parts:
            raise VaultError("Aucun résumé à importer")
        stamp = dt.datetime.now().strftime("%Y%m%d-%H%M%S")
        paths = []
        for i, part in enumerate(parts, 1):
            rel = self.vault.unique_path(INBOX_FOLDER, f"{stamp}-{i}")
            self.vault.write(rel, part + "\n", create_only=True)
            self.index.upsert(rel)
            paths.append(rel)
        return paths

    def discard(self, rel: str) -> None:
        self._check_inbox_path(rel)
        self.vault.delete(rel)
        self.index.remove(rel)

    def _check_inbox_path(self, rel: str) -> None:
        if not rel.startswith(INBOX_FOLDER + "/"):
            raise VaultError("Ce fichier n'est pas dans l'inbox")

    def _context(self) -> str:
        today = dt.date.today()
        open_tasks = self.tasks.open_tasks()
        task_lines = [
            f"- {t['id']} | {t['title']} | projet: {t['project'] or '-'} | statut: {t['status']}"
            f" | échéance: {t['due'] or '-'}" for t in open_tasks
        ] or ["(aucune)"]
        projects = sorted({n["stem"] for n in self.index.list_notes(type="projet")}
                          | {t["project"] for t in self.tasks.list() if t["project"]})
        since = (today - dt.timedelta(days=60)).isoformat()
        threads = {}
        for note in self.index.list_notes(type="echange", start=since):
            thread = note["meta"].get("thread")
            if thread:
                threads[thread] = f"- {thread} : {note['title']} (dernier échange {note['date']})"
        return (
            f"Date du jour : {today.isoformat()}\n\n"
            f"Tâches ouvertes :\n" + "\n".join(task_lines) + "\n\n"
            f"Projets connus : {', '.join(projects) or '(aucun)'}\n\n"
            f"Fils d'échanges récents :\n" + ("\n".join(threads.values()) or "(aucun)")
        )

    def analyze(self, rel: str) -> InboxProposal:
        self._check_inbox_path(rel)
        raw = self.vault.read_text(rel)
        messages = [
            {"role": "system", "content": INBOX_SYSTEM},
            {"role": "user", "content": f"{self._context()}\n\n=== Résumé à traiter ===\n{raw}"},
        ]
        proposal = self.llm.chat_json(messages, model=self.settings.model_smart,
                                      schema=InboxProposal)
        known = {t["id"].upper() for t in self.tasks.list()}
        proposal.task_updates = [u for u in proposal.task_updates if u.task_id.upper() in known]
        duplicates = self.index.find_by_meta("source_hash", content_hash(raw.strip()))
        proposal.duplicate_of = duplicates[0]["path"] if duplicates else None
        return proposal

    def apply(self, rel: str, proposal: InboxProposal) -> dict:
        self._check_inbox_path(rel)
        raw = self.vault.read_text(rel).strip()
        ech = proposal.echange
        stem = f"{ech.date}-{ech.source}-{slugify(ech.subject, 40)}"
        echange_path = self.vault.unique_path(f"echanges/{ech.date[:4]}/{ech.date[5:7]}", stem)
        echange_link = echange_path.rsplit("/", 1)[-1][:-3]

        created = [
            self.tasks.create(title=t.title, project=t.project, priority=t.priority,
                              status=t.status, due=t.due, description=t.description,
                              origin=echange_link, today=ech.date)
            for t in proposal.new_tasks if t.accepted and t.title.strip()
        ]
        updated = []
        for u in proposal.task_updates:
            if not u.accepted:
                continue
            note = f"{u.note.strip()} ([[{echange_link}]])" if u.note.strip() else f"[[{echange_link}]]"
            changes = {"status": u.status} if u.status else {}
            updated.append(self.tasks.update(u.task_id, changes, note=note, today=ech.date))

        task_ids = [t["id"] for t in created + updated]
        meta = {
            "type": "echange", "source": ech.source, "date": dt.date.fromisoformat(ech.date),
            "subject": ech.subject, "participants": ech.participants, "projects": ech.projects,
            "thread": ech.thread or slugify(ech.subject, 40),
            "tasks": [f"[[{tid}]]" for tid in task_ids], "source_hash": content_hash(raw),
        }
        body = f"\n# {ech.subject}\n\n{ech.summary.strip()}\n\n## Texte original\n\n{raw}\n"
        self.vault.write(echange_path, join_frontmatter(meta, body), create_only=True)
        self.index.upsert(echange_path)

        journal_path = None
        if proposal.journal_line.strip():
            links = " ".join(f"[[{tid}]]" for tid in task_ids)
            line = f"- [[{echange_link}]] : {proposal.journal_line.strip()}"
            journal_path = self.journal.append(
                ech.date, "## Échanges", line + (f" → {links}" if links else ""))

        self.vault.delete(rel)
        self.index.remove(rel)
        return {"echange": echange_path, "created_tasks": [t["id"] for t in created],
                "updated_tasks": [t["id"] for t in updated], "journal": journal_path}
