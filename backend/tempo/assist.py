"""Actions IA dans l'éditeur : extraire des tâches d'une note, transformer une sélection.

Rien n'est écrit ici : les tâches extraites sont des propositions que l'utilisateur coche avant
création, et le texte transformé est inséré par l'éditeur à sa demande."""

import datetime as dt

from pydantic import BaseModel

from .config import Settings
from .index import Index
from .llm import LLM, strip_fences
from .prompts import EXTRACT_TASKS_SYSTEM, TEXT_ACTIONS, TEXT_SYSTEM
from .tasks import PRIORITIES, TaskService
from .vault import Vault, VaultError, split_frontmatter, strip_section

MAX_TEXT_CHARS = 20_000


class TaskDraft(BaseModel):
    title: str
    description: str = ""
    project: str | None = None
    priority: str = "normale"
    due: str | None = None
    duplicate_of: str | None = None


class TaskDrafts(BaseModel):
    tasks: list[TaskDraft] = []


class AssistService:
    def __init__(self, vault: Vault, index: Index, tasks: TaskService, llm: LLM,
                 settings: Settings):
        self.vault = vault
        self.index = index
        self.tasks = tasks
        self.llm = llm
        self.settings = settings

    def extract_tasks(self, path: str, today: dt.date | None = None) -> dict:
        _, body = split_frontmatter(self.vault.read_text(path))
        body = strip_section(body, "## Texte original").strip()
        if not body:
            return {"tasks": []}
        open_tasks = self.tasks.open_tasks()
        projects = sorted({n["alias"] or n["stem"] for n in self.index.list_notes(type="projet")}
                          | {t["project"] for t in open_tasks if t["project"]})
        context = "\n".join([
            "Projets connus : " + (", ".join(projects) or "aucun"),
            "Tâches ouvertes :",
            *[f"- {t['id']} : {t['title']} ({t['project'] or 'sans projet'})" for t in open_tasks],
            "",
            f"Note « {path} » :",
            body[:MAX_TEXT_CHARS],
        ])
        system = EXTRACT_TASKS_SYSTEM.format(today=(today or dt.date.today()).isoformat())
        drafts = self.llm.chat_json(
            [{"role": "system", "content": system}, {"role": "user", "content": context}],
            model=self.settings.model_smart, schema=TaskDrafts)
        known = {t["id"].upper() for t in open_tasks}
        result = []
        for draft in drafts.tasks:
            if not draft.title.strip():
                continue
            if draft.priority not in PRIORITIES:
                draft.priority = "normale"
            if draft.due:
                try:
                    draft.due = dt.date.fromisoformat(draft.due[:10]).isoformat()
                except ValueError:
                    draft.due = None
            if draft.duplicate_of and draft.duplicate_of.upper() not in known:
                draft.duplicate_of = None
            result.append(draft.model_dump())
        return {"tasks": result}

    def transform(self, action: str, text: str) -> dict:
        if action not in TEXT_ACTIONS:
            raise VaultError(f"Action inconnue : {action}")
        if not text.strip():
            raise VaultError("Aucun texte à traiter")
        content = strip_fences(self.llm.chat(
            [{"role": "system", "content": TEXT_SYSTEM},
             {"role": "user", "content": f"{TEXT_ACTIONS[action]}\n\n{text[:MAX_TEXT_CHARS]}"}],
            model=self.settings.model_fast))
        return {"text": content}
