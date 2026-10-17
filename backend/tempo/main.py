"""API HTTP de Tempo et service du frontend compilé."""

import datetime as dt
from contextlib import asynccontextmanager

from fastapi import FastAPI, Request
from fastapi.responses import JSONResponse
from fastapi.staticfiles import StaticFiles
from pydantic import BaseModel

from .config import PROJECT_ROOT, Settings, load_settings
from .inbox import InboxProposal, InboxService
from .index import Index
from .journal import JournalService
from .llm import LLM, LLMError, LLMNotConfigured, build_llm
from .reports import ReportService
from .tasks import PRIORITIES, STATUSES, TaskService
from .vault import ConflictError, NotFoundError, Vault, VaultError

FRONTEND_DIST = PROJECT_ROOT / "frontend" / "dist"


class NoteWrite(BaseModel):
    path: str
    content: str
    expected_hash: str | None = None


class NoteCreate(BaseModel):
    type: str = "note"
    title: str = ""
    date: str | None = None


class TaskCreate(BaseModel):
    title: str
    project: str | None = None
    priority: str = "normale"
    status: str = "a_faire"
    due: str | None = None
    description: str = ""


class TaskPatch(BaseModel):
    changes: dict = {}
    note: str | None = None


class InboxAdd(BaseModel):
    text: str


class InboxApply(BaseModel):
    path: str
    proposal: InboxProposal


class WeeklyRequest(BaseModel):
    date: str | None = None
    extra: str = ""


class Services:
    def __init__(self, settings: Settings, llm: LLM):
        self.settings = settings
        self.vault = Vault(settings.vault)
        self.index = Index(self.vault, self.vault.root / ".tempo" / "index.db")
        self.tasks = TaskService(self.vault, self.index)
        self.journal = JournalService(self.vault, self.index)
        self.inbox = InboxService(self.vault, self.index, self.tasks, self.journal, llm, settings)
        self.reports = ReportService(self.vault, self.index, self.tasks, llm, settings)


def _note_summary(note: dict) -> dict:
    return {k: note[k] for k in ("path", "stem", "alias", "title", "type", "date")}


def create_app(settings: Settings | None = None, llm: LLM | None = None) -> FastAPI:
    settings = settings or load_settings()
    s = Services(settings, llm or build_llm(settings))

    @asynccontextmanager
    async def lifespan(app: FastAPI):
        s.index.rebuild()
        if settings.watch:
            s.index.start_watching()
        yield
        s.index.stop_watching()

    app = FastAPI(title="Tempo", lifespan=lifespan)
    app.state.services = s

    @app.exception_handler(ConflictError)
    async def _conflict(_: Request, exc: ConflictError):
        return JSONResponse(status_code=409, content={"detail": str(exc),
                                                      "current_content": exc.current_content})

    @app.exception_handler(NotFoundError)
    async def _not_found(_: Request, exc: NotFoundError):
        return JSONResponse(status_code=404, content={"detail": str(exc)})

    @app.exception_handler(VaultError)
    async def _vault_error(_: Request, exc: VaultError):
        return JSONResponse(status_code=400, content={"detail": str(exc)})

    @app.exception_handler(LLMNotConfigured)
    async def _llm_off(_: Request, exc: LLMNotConfigured):
        return JSONResponse(status_code=503, content={"detail": str(exc)})

    @app.exception_handler(LLMError)
    async def _llm_error(_: Request, exc: LLMError):
        return JSONResponse(status_code=502, content={"detail": str(exc)})

    # --- général ---------------------------------------------------------

    @app.get("/api/config")
    def get_config():
        return {"vault": str(s.vault.root), "llm_enabled": settings.llm_enabled,
                "model_fast": settings.model_fast, "model_smart": settings.model_smart,
                "statuses": STATUSES, "priorities": PRIORITIES}

    @app.get("/api/state")
    def get_state():
        return {"revision": s.index.revision, "inbox": len(s.inbox.list())}

    @app.post("/api/reindex")
    def reindex():
        return {"count": s.index.rebuild()}

    # --- notes -----------------------------------------------------------

    @app.get("/api/files")
    def list_files():
        return [_note_summary(n) for n in s.index.list_notes()]

    @app.get("/api/notes")
    def read_note(path: str):
        return s.vault.read(path)

    @app.put("/api/notes")
    def write_note(body: NoteWrite):
        new_hash = s.vault.write(body.path, body.content, expected_hash=body.expected_hash)
        s.index.upsert(body.path)
        return {"path": body.path, "hash": new_hash}

    @app.post("/api/notes")
    def create_note(body: NoteCreate):
        return {"path": s.journal.create_note(body.type, body.title, body.date)}

    @app.delete("/api/notes")
    def delete_note(path: str):
        s.vault.delete(path)
        s.index.remove(path)
        return {"ok": True}

    @app.post("/api/journal/today")
    def journal_today(date: str | None = None):
        return {"path": s.journal.ensure(date)}

    @app.get("/api/search")
    def search(q: str):
        return s.index.search(q)

    @app.get("/api/resolve")
    def resolve(name: str):
        path = s.index.resolve_link(name)
        if not path:
            raise NotFoundError(f"Aucune note ne correspond à « {name} »")
        return {"path": path}

    @app.get("/api/links")
    def links(path: str):
        return {"backlinks": [_note_summary(n) for n in s.index.backlinks(path)],
                "outlinks": [_note_summary(n) for n in s.index.outlinks(path)]}

    # --- tâches ----------------------------------------------------------

    @app.get("/api/tasks")
    def list_tasks(status: str | None = None, project: str | None = None):
        return s.tasks.list(status, project)

    @app.get("/api/tasks/{task_id}")
    def get_task(task_id: str):
        return s.tasks.get(task_id)

    @app.post("/api/tasks")
    def create_task(body: TaskCreate):
        return s.tasks.create(**body.model_dump())

    @app.patch("/api/tasks/{task_id}")
    def update_task(task_id: str, body: TaskPatch):
        return s.tasks.update(task_id, body.changes, note=body.note)

    # --- inbox -----------------------------------------------------------

    @app.get("/api/inbox")
    def inbox_list():
        return s.inbox.list()

    @app.post("/api/inbox")
    def inbox_add(body: InboxAdd):
        return {"paths": s.inbox.add(body.text)}

    @app.post("/api/inbox/analyze")
    def inbox_analyze(path: str):
        return s.inbox.analyze(path)

    @app.post("/api/inbox/apply")
    def inbox_apply(body: InboxApply):
        return s.inbox.apply(body.path, body.proposal)

    @app.delete("/api/inbox")
    def inbox_discard(path: str):
        s.inbox.discard(path)
        return {"ok": True}

    # --- rapports --------------------------------------------------------

    @app.post("/api/reports/weekly")
    def weekly_report(body: WeeklyRequest):
        day = dt.date.fromisoformat(body.date) if body.date else None
        return s.reports.weekly(day, body.extra)

    @app.post("/api/reports/task/{task_id}")
    def task_report(task_id: str):
        return s.reports.task_report(task_id)

    if FRONTEND_DIST.is_dir():
        app.mount("/", StaticFiles(directory=FRONTEND_DIST, html=True), name="frontend")

    return app


def run() -> None:
    import uvicorn

    settings = load_settings()
    print(f"Tempo : http://{settings.host}:{settings.port}  (vault : {settings.vault})")
    if not settings.llm_enabled:
        print("Attention : API LLM non configurée (.env), les fonctions IA sont désactivées.")
    uvicorn.run(create_app(settings), host=settings.host, port=settings.port)


if __name__ == "__main__":
    run()
