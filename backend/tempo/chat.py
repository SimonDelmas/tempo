"""Chat avec l'agent : boucle d'appels d'outils sur les notes et les tâches.

Le protocole passe par le mode json (déjà éprouvé avec Vibe) plutôt que par le tool calling
natif : à chaque étape le modèle demande un outil ou donne sa réponse finale. Les outils de
consultation sont en lecture seule ; les outils de proposition n'écrivent rien, ils préparent
des modifications que l'utilisateur valide une à une (`ChatService.apply`).

La réponse finale est diffusée au fil de l'eau : le texte du champ "answer" est extrait du json
partiel reçu du modèle."""

import datetime as dt
import json
import re
from collections.abc import Callable, Iterator
from dataclasses import dataclass
from typing import Any, Literal

from pydantic import BaseModel

from .config import Settings
from .index import Index, extract_links
from .journal import JournalService
from .llm import LLM, extract_json
from .prompts import CHAT_SYSTEM
from .tasks import PRIORITIES, STATUSES, TaskService
from .vault import NotFoundError, Vault, VaultError, split_frontmatter

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


class ChatProposal(BaseModel):
    """Modification proposée par l'agent, appliquée seulement après validation."""
    kind: Literal["create_task", "update_task", "journal"]
    title: str = ""
    description: str = ""
    project: str | None = None
    priority: str | None = None
    status: str | None = None
    due: str | None = None
    task_id: str | None = None
    note: str = ""
    line: str = ""


@dataclass
class Tool:
    name: str
    description: str
    run: Callable[..., Any]


def _clip(text: str, limit: int) -> str:
    return text if len(text) <= limit else text[:limit] + "\n[…tronqué]"


def _check_date(value: str | None) -> str | None:
    if not value:
        return None
    try:
        return dt.date.fromisoformat(str(value)[:10]).isoformat()
    except ValueError as exc:
        raise VaultError(f"Date invalide : {value} (format AAAA-MM-JJ)") from exc


def _check(field: str, value: str | None, allowed: list[str]) -> str | None:
    if value and value not in allowed:
        raise VaultError(f"{field} invalide : {value} (valeurs : {', '.join(allowed)})")
    return value or None


_JSON_ESCAPES = {"n": "\n", "t": "\t", "r": "\r", "b": "\b", "f": "\f"}


def decode_partial_string(raw: str) -> str:
    """Décode le début d'une chaîne json (sans le guillemet ouvrant), même incomplète."""
    out, i = [], 0
    while i < len(raw):
        c = raw[i]
        if c == '"':
            break
        if c == "\\":
            if i + 1 >= len(raw):
                break
            escape = raw[i + 1]
            if escape == "u":
                if i + 6 > len(raw):
                    break
                try:
                    out.append(chr(int(raw[i + 2:i + 6], 16)))
                except ValueError:
                    pass
                i += 6
                continue
            out.append(_JSON_ESCAPES.get(escape, escape))
            i += 2
            continue
        out.append(c)
        i += 1
    return "".join(out)


def partial_answer(raw: str) -> str | None:
    """Texte de la réponse finale contenu dans un json encore incomplet, ou None si ce n'est
    pas (encore) une réponse finale."""
    if not re.search(r'"action"\s*:\s*"answer"', raw):
        return None
    match = re.search(r'"answer"\s*:\s*"', raw)
    return decode_partial_string(raw[match.end():]) if match else None


class ChatService:
    def __init__(self, vault: Vault, index: Index, tasks: TaskService, journal: JournalService,
                 llm: LLM, settings: Settings):
        self.vault = vault
        self.index = index
        self.tasks = tasks
        self.journal = journal
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
        self.proposal_tools = {t.name: t for t in [
            Tool(name="propose_task", run=self.propose_task,
                 description='{"title": "...", "description": "..." (optionnel), "project": '
                             '"identifiant" (optionnel), "priority": "basse|normale|haute" '
                             '(optionnel), "due": "AAAA-MM-JJ" (optionnel)} — nouvelle tâche'),
            Tool(name="propose_task_update", run=self.propose_task_update,
                 description='{"task_id": "T-001", "status": "a_faire|en_cours|attente|bloquee|'
                             'terminee" (optionnel), "priority": "..." (optionnel), "due": '
                             '"AAAA-MM-JJ" (optionnel), "note": "ce qui a changé" (optionnel)} '
                             "— mise à jour d'une tâche existante, tracée dans son historique"),
            Tool(name="propose_journal_line", run=self.propose_journal_line,
                 description='{"line": "texte"} — ligne à ajouter au journal du jour'),
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

    # --- propositions (validées par l'utilisateur, voir apply) ---------------

    def propose_task(self, title: str, description: str = "", project: str | None = None,
                     priority: str | None = None, due: str | None = None) -> ChatProposal:
        if not title.strip():
            raise VaultError("Le titre de la tâche est obligatoire")
        return ChatProposal(kind="create_task", title=title.strip(), description=description,
                            project=project or None,
                            priority=_check("priority", priority, PRIORITIES), due=_check_date(due))

    def propose_task_update(self, task_id: str, status: str | None = None,
                            priority: str | None = None, due: str | None = None,
                            note: str = "") -> ChatProposal:
        task = self.tasks.get(task_id)
        proposal = ChatProposal(kind="update_task", task_id=task["id"], title=task["title"],
                                status=_check("status", status, STATUSES),
                                priority=_check("priority", priority, PRIORITIES),
                                due=_check_date(due), note=(note or "").strip())
        if not (proposal.status or proposal.priority or proposal.due or proposal.note):
            raise VaultError("Aucune modification proposée")
        return proposal

    def propose_journal_line(self, line: str) -> ChatProposal:
        if not line.strip():
            raise VaultError("La ligne de journal est vide")
        return ChatProposal(kind="journal", line=line.strip())

    def apply(self, proposal: ChatProposal, today: dt.date | None = None) -> dict:
        today_str = (today or dt.date.today()).isoformat()
        if proposal.kind == "create_task":
            task = self.tasks.create(proposal.title, proposal.project,
                                     proposal.priority or "normale", proposal.status or "a_faire",
                                     proposal.due, proposal.description, today=today_str)
            return {"path": task["path"], "message": f"Tâche {task['id']} créée"}
        if proposal.kind == "update_task":
            if not proposal.task_id:
                raise VaultError("Tâche non précisée")
            changes = {k: getattr(proposal, k) for k in ("status", "priority", "due")
                       if getattr(proposal, k)}
            task = self.tasks.update(proposal.task_id, changes, proposal.note, today=today_str)
            return {"path": task["path"], "message": f"Tâche {task['id']} mise à jour"}
        if not proposal.line.strip():
            raise VaultError("La ligne de journal est vide")
        path = self.journal.append(today_str, "## Notes", f"- {proposal.line.strip()}")
        return {"path": path, "message": "Ligne ajoutée au journal"}

    def run_tool(self, name: str | None, args: dict, proposals: list[ChatProposal]) -> str:
        tool = self.tools.get(name or "") or self.proposal_tools.get(name or "")
        if not tool:
            known = ", ".join([*self.tools, *self.proposal_tools])
            return f"Erreur : outil inconnu « {name} ». Outils : {known}."
        try:
            result = tool.run(**args)
        except TypeError as exc:
            return f"Erreur : arguments invalides pour {name} ({exc})."
        except VaultError as exc:
            return f"Erreur : {exc}"
        if isinstance(result, ChatProposal):
            proposals.append(result)
            return (f"Proposition n°{len(proposals)} enregistrée : elle sera soumise à "
                    "l'utilisateur, qui la validera ou non.")
        if not result:
            return "Aucun résultat."
        return _clip(json.dumps(result, ensure_ascii=False, default=str), MAX_RESULT_CHARS)

    # --- conversation -------------------------------------------------------

    def system_prompt(self, today: dt.date) -> str:
        def describe(tools: dict[str, Tool]) -> str:
            return "\n".join(f"- {t.name} {t.description}" for t in tools.values())
        return CHAT_SYSTEM.format(today=today.isoformat(), weekday=WEEKDAYS[today.weekday()],
                                  tools=describe(self.tools),
                                  proposals=describe(self.proposal_tools))

    def links_of(self, text: str) -> dict[str, str]:
        """Liens [[…]] de la réponse résolus en chemins (les liens inconnus sont omis)."""
        links = {}
        for name in extract_links(text):
            path = self.index.resolve_link(name)
            if path:
                links[name] = path
        return links

    def _next_step(self, messages: list[dict]) -> Iterator[tuple[str, Any]]:
        """Étape suivante du modèle : ("delta", texte) au fil de la réponse finale, puis
        ("step", AgentStep)."""
        raw, sent = "", 0
        for chunk in self.llm.chat_stream(messages, model=self.settings.model_smart, json_mode=True):
            raw += chunk
            text = partial_answer(raw)
            if text is not None and len(text) > sent:
                yield "delta", text[sent:]
                sent = len(text)
        try:
            step = AgentStep.model_validate_json(extract_json(raw))
        except ValueError:
            # json invalide : nouvel essai non diffusé (avec demande de correction)
            step = self.llm.chat_json(messages, model=self.settings.model_smart, schema=AgentStep)
        yield "step", step

    def stream(self, history: list[ChatMessage], today: dt.date | None = None) -> Iterator[dict]:
        """Événements de la réponse : step (outil appelé), delta (texte de la réponse), done."""
        messages = [{"role": "system", "content": self.system_prompt(today or dt.date.today())}]
        messages += [m.model_dump() for m in history[-MAX_HISTORY:]]
        steps: list[dict] = []
        proposals: list[ChatProposal] = []
        step: AgentStep | None = None
        for attempt in range(MAX_STEPS + 1):
            if attempt == MAX_STEPS:
                messages.append({"role": "user", "content": "Nombre maximal d'outils atteint : "
                                 "donne maintenant ta réponse finale avec ce que tu as trouvé."})
            for kind, value in self._next_step(messages):
                if kind == "delta":
                    yield {"type": "delta", "text": value}
                else:
                    step = value
            assert step
            if step.action == "answer" or attempt == MAX_STEPS:
                break
            result = self.run_tool(step.tool, step.args, proposals)
            summary = result if result.startswith(("Erreur", "Aucun")) else None
            steps.append({"tool": step.tool, "args": step.args, "summary": summary})
            yield {"type": "step", **steps[-1]}
            messages += [
                {"role": "assistant", "content": step.model_dump_json(exclude={"answer"})},
                {"role": "user", "content": f"Résultat de {step.tool} :\n{result}"},
            ]
        answer = (step.answer if step else "").strip() or "Je n'ai pas de réponse à proposer."
        yield {"type": "done", "answer": answer, "steps": steps, "links": self.links_of(answer),
               "proposals": [p.model_dump() for p in proposals]}

    def ask(self, history: list[ChatMessage], today: dt.date | None = None) -> dict:
        """Réponse complète, sans diffusion."""
        events = list(self.stream(history, today))
        return {k: v for k, v in events[-1].items() if k != "type"}
