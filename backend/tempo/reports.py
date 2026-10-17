"""Rapports : hebdomadaire (manager) et avancement d'une tâche."""

import datetime as dt

from .config import Settings
from .index import Index
from .llm import LLM, strip_fences
from .prompts import TASK_REPORT_SYSTEM, WEEKLY_SYSTEM
from .tasks import OPEN_STATUSES, TaskService
from .vault import Vault, join_frontmatter, split_frontmatter, strip_section

MAX_NOTE_CHARS = 6000
MAX_CONTEXT_CHARS = 150_000


def week_bounds(day: dt.date) -> tuple[dt.date, dt.date]:
    monday = day - dt.timedelta(days=day.weekday())
    return monday, monday + dt.timedelta(days=6)


def _clip(text: str, limit: int = MAX_NOTE_CHARS) -> str:
    text = text.strip()
    return text if len(text) <= limit else text[:limit] + "\n[…tronqué]"


class ReportService:
    def __init__(self, vault: Vault, index: Index, tasks: TaskService, llm: LLM,
                 settings: Settings):
        self.vault = vault
        self.index = index
        self.tasks = tasks
        self.llm = llm
        self.settings = settings

    def _note_block(self, note: dict) -> str:
        _, body = split_frontmatter(self.vault.read_text(note["path"]))
        body = strip_section(body, "## Texte original")
        link = note["alias"] or note["stem"]
        return f"### [[{link}]] — {note['title']} ({note['date'] or 'sans date'})\n{_clip(body)}"

    def weekly_context(self, start: dt.date, end: dt.date, extra: str = "") -> str:
        s, e = start.isoformat(), end.isoformat()
        sections = [f"Semaine du {s} au {e}."]

        def block(title: str, notes: list[dict]) -> None:
            if notes:
                sections.append(f"## {title}\n\n" + "\n\n".join(self._note_block(n) for n in notes))

        block("Réunions", self.index.list_notes(type="reunion", start=s, end=e))
        block("Échanges (mails / Teams)", self.index.list_notes(type="echange", start=s, end=e))
        block("Journal", self.index.list_notes(type="journal", start=s, end=e))

        start_ts = dt.datetime.combine(start, dt.time.min).timestamp()
        end_ts = dt.datetime.combine(end, dt.time.max).timestamp()
        others = [n for n in self.index.modified_between(start_ts, end_ts)
                  if n["type"] in ("sujet", "projet", "note", "personne")]
        block("Autres notes modifiées cette semaine", others)

        events = self.tasks.events(s, e)
        if events:
            sections.append("## Historique des tâches sur la semaine\n" + "\n".join(
                f"- {ev['date']} [[{ev['task_id']}]] {ev['title']} : {ev['text']}" for ev in events))

        all_tasks = self.tasks.list()
        open_tasks = [t for t in all_tasks if t["status"] in OPEN_STATUSES]
        if open_tasks:
            sections.append("## Tâches ouvertes (état actuel)\n" + "\n".join(
                f"- [[{t['id']}]] {t['title']} | projet: {t['project'] or '-'} | statut: "
                f"{t['status']} | priorité: {t['priority']} | échéance: {t['due'] or '-'}"
                for t in open_tasks))
        done_ids = {ev["task_id"] for ev in events
                    if ev["text"].startswith("statut") and "→ terminee" in ev["text"]}
        done = [t for t in all_tasks if t["id"] in done_ids]
        if done:
            sections.append("## Tâches terminées cette semaine\n" + "\n".join(
                f"- [[{t['id']}]] {t['title']}" for t in done))
        next_end = (end + dt.timedelta(days=7)).isoformat()
        upcoming = [t for t in open_tasks if t["due"] and t["due"] <= next_end]
        if upcoming:
            sections.append("## Échéances jusqu'à la fin de la semaine prochaine\n" + "\n".join(
                f"- [[{t['id']}]] {t['title']} : {t['due']}" for t in upcoming))
        if extra.strip():
            sections.append("## Éléments ajoutés par l'utilisateur (à intégrer)\n" + extra.strip())

        return _clip("\n\n".join(sections), MAX_CONTEXT_CHARS)

    def weekly(self, day: dt.date | None = None, extra: str = "") -> dict:
        start, end = week_bounds(day or dt.date.today())
        context = self.weekly_context(start, end, extra)
        content = strip_fences(self.llm.chat(
            [{"role": "system", "content": WEEKLY_SYSTEM}, {"role": "user", "content": context}],
            model=self.settings.model_smart))
        year, week, _ = start.isocalendar()
        label = f"{year}-S{week:02d}"
        meta = {"type": "rapport", "kind": "hebdo", "week": label, "period_start": start,
                "period_end": end, "generated": dt.datetime.now().isoformat(timespec="minutes")}
        body = f"\n# Rapport hebdomadaire {label} ({start:%d/%m} – {end:%d/%m})\n\n{content}\n"
        rel = self.vault.unique_path("rapports/hebdo", label)
        self.vault.write(rel, join_frontmatter(meta, body), create_only=True)
        self.index.upsert(rel)
        return {"path": rel}

    def task_context(self, task_id: str) -> str:
        task = self.tasks.get(task_id)
        parts = [f"# Fiche de la tâche [[{task['id']}]]\n{task['content']}"]
        notes = [n for n in self.index.backlinks(task["path"]) if n["type"] != "rapport"]
        notes.sort(key=lambda n: n["date"] or "")
        if notes:
            parts.append("# Notes qui mentionnent la tâche\n\n" +
                         "\n\n".join(self._note_block(n) for n in notes))
        return _clip("\n\n".join(parts), MAX_CONTEXT_CHARS)

    def task_report(self, task_id: str) -> dict:
        task = self.tasks.get(task_id)
        context = self.task_context(task_id)
        content = strip_fences(self.llm.chat(
            [{"role": "system", "content": TASK_REPORT_SYSTEM},
             {"role": "user", "content": context}],
            model=self.settings.model_smart))
        today = dt.date.today()
        meta = {"type": "rapport", "kind": "tache", "date": today, "tasks": [f"[[{task['id']}]]"],
                "generated": dt.datetime.now().isoformat(timespec="minutes")}
        body = f"\n# Avancement {task['id']} — {task['title']} ({today:%d/%m/%Y})\n\n{content}\n"
        rel = self.vault.unique_path("rapports/taches", f"{task['id']}-{today.isoformat()}")
        self.vault.write(rel, join_frontmatter(meta, body), create_only=True)
        self.index.upsert(rel)
        return {"path": rel}
