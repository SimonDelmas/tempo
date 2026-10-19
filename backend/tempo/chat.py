"""Chat avec l'agent : boucle d'appels d'outils en lecture seule sur les notes et les tâches.

Le protocole passe par le mode json (déjà éprouvé avec Vibe) plutôt que par le tool calling
natif : à chaque étape le modèle demande un outil ou donne sa réponse finale."""

import datetime as dt
import json
from collections.abc import Callable
from dataclasses import dataclass
from typing import Any, Literal

from pydantic import BaseModel

from .config import Settings
from .index import Index, extract_links
from .llm import LLM
from .prompts import CHAT_SYSTEM
from .tasks import TaskService
from .vault import NotFoundError, Vault, split_frontmatter

MAX_STEPS = 8
MAX_NOTE_CHARS = 8000
MAX_RESULT_CHARS = 12000
MAX_HISTORY = 20
WEEKDAYS = ["lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi", "dimanche"]


class ChatMessage(BaseModel):
    role: Literal["user", "assistant"]
    content: str


class AgentStep(BaseModel):
    action: Literal["tool", "answer"]
    tool: str | None = None
    args: dict[str, Any] = {}
    answer: str = ""


@dataclass
class Tool:
    name: str
    description: str
    run: Callable[..., Any]


def _clip(text: str, limit: int) -> str:
    return text if len(text) <= limit else text[:limit] + "\n[…tronqué]"


class ChatService:
    def __init__(self, vault: Vault, index: Index, tasks: TaskService, llm: LLM,
                 settings: Settings):
        self.vault = vault
        self.index = index
        self.tasks = tasks
        self.llm = llm
        self.settings = settings
        self.tools = {t.name: t for t in [
            Tool(name="search_notes", run=self.search_notes,
                 description='{"query": "mots-clés"} — recherche plein texte dans toutes les notes'),
            Tool(name="read_note", run=self.read_note,
                 description='{"name": "lien ou chemin"} — contenu d\'une note (ou tâche), '
                             "avec les notes qui la mentionnent"),
            Tool(name="list_notes", run=self.list_notes,
                 description='{"type": "reunion|echange|journal|sujet|projet|personne|ressource|'
                             'formation|rapport" (optionnel), "start": "AAAA-MM-JJ" (optionnel), '
                             '"end": "AAAA-MM-JJ" (optionnel)} — notes datées sur une période'),
            Tool(name="list_tasks", run=self.list_tasks,
                 description='{"status": "a_faire|en_cours|attente|bloquee|terminee" (optionnel), '
                             '"project": "identifiant" (optionnel)} — liste des tâches'),
            Tool(name="get_task", run=self.get_task,
                 description='{"id": "T-001"} — fiche complète d\'une tâche et son historique'),
            Tool(name="task_events", run=self.task_events,
                 description='{"start": "AAAA-MM-JJ", "end": "AAAA-MM-JJ"} — événements '
                             "d'historique des tâches sur une période (créations, statuts, notes)"),
        ]}

    # --- outils -------------------------------------------------------------

    @staticmethod
    def _ref(note: dict) -> dict:
        return {"lien": f"[[{note['alias'] or note['stem']}]]", "titre": note["title"],
                "type": note["type"], "date": note.get("date") or None, "chemin": note["path"]}

    def search_notes(self, query: str) -> list[dict]:
        results = []
        for hit in self.index.search(query, limit=12):
            note = self.index.get(hit["path"])
            if note:
                results.append({**self._ref(note), "extrait": hit["snippet"]})
        return results

    def _resolve(self, name: str) -> str:
        name = name.strip().removeprefix("[[").removesuffix("]]").split("|")[0]
        if self.index.get(name):
            return name
        path = self.index.resolve_link(name) or self.index.resolve_link(name.removesuffix(".md"))
        if not path:
            raise NotFoundError(f"Aucune note ne correspond à « {name} »")
        return path

    def read_note(self, name: str) -> dict:
        path = self._resolve(name)
        note = self.index.get(path)
        assert note
        _, body = split_frontmatter(self.vault.read_text(path))
        return {**self._ref(note), "meta": note["meta"], "contenu": _clip(body.strip(), MAX_NOTE_CHARS),
                "mentionnee_dans": [self._ref(n) for n in self.index.backlinks(path)[:15]]}

    def list_notes(self, type: str | None = None, start: str | None = None,
                   end: str | None = None) -> list[dict]:
        notes = self.index.list_notes(type=type or None, start=start or None, end=end or None)
        notes = [n for n in notes if n["type"] not in ("modele", "inbox", "tache")]
        return [self._ref(n) for n in notes[-60:]]

    def list_tasks(self, status: str | None = None, project: str | None = None) -> list[dict]:
        return [{**{k: t[k] for k in ("id", "title", "status", "priority", "project", "due")},
                 "lien": f"[[{t['id']}]]"} for t in self.tasks.list(status or None, project or None)]

    def get_task(self, id: str) -> dict:
        task = self.tasks.get(id)
        _, body = split_frontmatter(task.pop("content"))
        task.pop("path")
        return {**task, "lien": f"[[{task['id']}]]", "contenu": _clip(body.strip(), MAX_NOTE_CHARS)}

    def task_events(self, start: str, end: str) -> list[dict]:
        return [{**e, "lien": f"[[{e['task_id']}]]"} for e in self.tasks.events(start, end)]

    def run_tool(self, name: str | None, args: dict) -> str:
        tool = self.tools.get(name or "")
        if not tool:
            return f"Erreur : outil inconnu « {name} ». Outils : {', '.join(self.tools)}."
        try:
            result = tool.run(**args)
        except TypeError as exc:
            return f"Erreur : arguments invalides pour {name} ({exc})."
        except NotFoundError as exc:
            return f"Erreur : {exc}"
        if not result:
            return "Aucun résultat."
        return _clip(json.dumps(result, ensure_ascii=False, default=str), MAX_RESULT_CHARS)

    # --- conversation -------------------------------------------------------

    def system_prompt(self, today: dt.date) -> str:
        tools = "\n".join(f"- {t.name} {t.description}" for t in self.tools.values())
        return CHAT_SYSTEM.format(today=today.isoformat(), weekday=WEEKDAYS[today.weekday()],
                                  tools=tools)

    def links_of(self, text: str) -> dict[str, str]:
        """Liens [[…]] de la réponse résolus en chemins (les liens inconnus sont omis)."""
        links = {}
        for name in extract_links(text):
            path = self.index.resolve_link(name)
            if path:
                links[name] = path
        return links

    def ask(self, history: list[ChatMessage], today: dt.date | None = None) -> dict:
        messages = [{"role": "system", "content": self.system_prompt(today or dt.date.today())}]
        messages += [m.model_dump() for m in history[-MAX_HISTORY:]]
        steps = []
        for _ in range(MAX_STEPS):
            step = self.llm.chat_json(messages, model=self.settings.model_smart, schema=AgentStep)
            if step.action == "answer":
                break
            result = self.run_tool(step.tool, step.args)
            steps.append({"tool": step.tool, "args": step.args,
                          "summary": result if result.startswith(("Erreur", "Aucun")) else None})
            messages += [
                {"role": "assistant", "content": step.model_dump_json(exclude={"answer"})},
                {"role": "user", "content": f"Résultat de {step.tool} :\n{result}"},
            ]
        else:
            messages.append({"role": "user", "content": "Nombre maximal d'outils atteint : donne "
                             "maintenant ta réponse finale avec ce que tu as trouvé."})
            step = self.llm.chat_json(messages, model=self.settings.model_smart, schema=AgentStep)
        answer = step.answer.strip() or "Je n'ai pas de réponse à proposer."
        return {"answer": answer, "steps": steps, "links": self.links_of(answer)}
