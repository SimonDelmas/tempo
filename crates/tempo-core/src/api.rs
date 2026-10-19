//! API HTTP de Tempo (`/api/*`) et service du frontend compilé.

use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post, put};
use axum::{Json, Router};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::chat::{ChatMessage, ChatProposal, Role};
use crate::error::{vault_err, Error, Result};
use crate::imputations::Hours;
use crate::inbox::InboxProposal;
use crate::services::Services;
use crate::tasks::{NewTask, PRIORITIES, STATUSES};
use crate::util::{date_or_today, parse_date, today};

/// Contenu d'un fichier du frontend : (octets, type MIME).
pub type AssetFn = Arc<dyn Fn(&str) -> Option<(Vec<u8>, String)> + Send + Sync>;

/// Origine des fichiers de l'interface servis hors de `/api`.
#[derive(Clone, Default)]
pub enum Frontend {
    /// pas d'interface (tests, développement avec Vite)
    #[default]
    None,
    /// dossier compilé (`frontend/dist`)
    Dir(PathBuf),
    /// fichiers embarqués dans l'exécutable (application de bureau)
    Embedded(AssetFn),
}

#[derive(Clone, Default)]
pub struct ApiOptions {
    pub frontend: Frontend,
    /// Jeton exigé sur toutes les requêtes (application de bureau) : transmis une fois dans
    /// l'URL d'ouverture (`/?token=…`), puis conservé dans un cookie HttpOnly.
    pub token: Option<String>,
}

type AppState = Arc<Services>;
type ApiResult = Result<Json<Value>>;

/// Exécute un traitement bloquant (disque, SQLite, appels HTTP au LLM) hors du runtime async.
async fn blocking<T: Serialize + Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> ApiResult {
    let value = tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Error::Internal(format!("Traitement interrompu : {e}")))??;
    Ok(Json(serde_json::to_value(value).map_err(|e| Error::Internal(e.to_string()))?))
}

/// Corps json ; une requête mal formée renvoie une erreur 422 au format `{"detail": …}`.
struct JsonBody<T>(T);

impl<T: DeserializeOwned, S: Send + Sync> axum::extract::FromRequest<S> for JsonBody<T> {
    type Rejection = Response;

    async fn from_request(req: Request, state: &S) -> std::result::Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(value)) => Ok(JsonBody(value)),
            Err(rejection) => Err((StatusCode::UNPROCESSABLE_ENTITY, Json(json!({"detail": rejection.body_text()}))).into_response()),
        }
    }
}

// --- corps et paramètres des requêtes ------------------------------------------

#[derive(Deserialize)]
struct PathParam {
    path: String,
}

#[derive(Deserialize)]
struct DateParam {
    date: Option<String>,
}

#[derive(Deserialize)]
struct SearchParam {
    q: String,
}

#[derive(Deserialize)]
struct NameParam {
    name: String,
}

#[derive(Deserialize)]
struct TaskFilter {
    status: Option<String>,
    project: Option<String>,
}

#[derive(Deserialize)]
struct NoteWrite {
    path: String,
    content: String,
    expected_hash: Option<String>,
}

#[derive(Deserialize)]
struct NoteCreate {
    #[serde(default = "note_type", rename = "type")]
    kind: String,
    #[serde(default)]
    title: String,
    date: Option<String>,
}

fn note_type() -> String {
    "note".into()
}

#[derive(Deserialize)]
struct TaskCreate {
    title: String,
    project: Option<String>,
    priority: Option<String>,
    status: Option<String>,
    due: Option<String>,
    #[serde(default)]
    description: String,
    /// note d'origine, tracée dans l'historique de la tâche
    origin: Option<String>,
}

#[derive(Deserialize)]
struct TaskPatch {
    #[serde(default)]
    changes: Map<String, Value>,
    note: Option<String>,
}

#[derive(Deserialize)]
struct InboxAdd {
    text: String,
}

#[derive(Deserialize)]
struct InboxApply {
    path: String,
    proposal: InboxProposal,
}

#[derive(Deserialize)]
struct WeeklyRequest {
    date: Option<String>,
    #[serde(default)]
    extra: String,
}

#[derive(Deserialize)]
struct AccountCreate {
    title: String,
    alias: String,
    #[serde(default)]
    days: f64,
    #[serde(default)]
    description: String,
}

#[derive(Deserialize)]
struct AccountPatch {
    title: Option<String>,
    days: Option<f64>,
    description: Option<String>,
}

#[derive(Deserialize)]
struct SaisieDate {
    date: String,
}

#[derive(Deserialize)]
struct ProjectCreate {
    title: String,
}

#[derive(Deserialize)]
struct ProjectAccount {
    compte: Option<String>,
}

#[derive(Deserialize)]
struct TextAction {
    action: String,
    text: String,
}

#[derive(Deserialize)]
struct ChatRequest {
    messages: Vec<ChatMessage>,
}

#[derive(Deserialize)]
struct WeekHours {
    date: String,
    #[serde(default)]
    hours: Hours,
    #[serde(default)]
    comment: String,
}

fn check_conversation(messages: &[ChatMessage]) -> Result<()> {
    match messages.last() {
        Some(m) if m.role == Role::User => Ok(()),
        _ => Err(vault_err("La conversation doit se terminer par un message de l'utilisateur")),
    }
}

// --- routes --------------------------------------------------------------------

pub fn router(services: Arc<Services>, options: ApiOptions) -> Router {
    let api = Router::new()
        // général
        .route("/api/config", get(get_config))
        .route("/api/state", get(get_state))
        .route("/api/reindex", post(reindex))
        // notes
        .route("/api/files", get(list_files))
        .route("/api/notes", get(read_note).put(write_note).post(create_note).delete(delete_note))
        .route("/api/journal/today", post(journal_today))
        .route("/api/search", get(search))
        .route("/api/resolve", get(resolve))
        .route("/api/links", get(links))
        // tâches
        .route("/api/tasks", get(list_tasks).post(create_task))
        .route("/api/tasks/{task_id}", get(get_task).patch(update_task))
        // inbox
        .route("/api/inbox", get(inbox_list).post(inbox_add).delete(inbox_discard))
        .route("/api/inbox/analyze", post(inbox_analyze))
        .route("/api/inbox/apply", post(inbox_apply))
        // rapports
        .route("/api/reports/weekly", post(weekly_report))
        .route("/api/reports/task/{task_id}", post(task_report))
        // comptes d'imputation
        .route("/api/accounts", get(list_accounts).post(create_account))
        .route("/api/accounts/{alias}", patch(update_account))
        .route("/api/projects", get(list_projects).post(create_project))
        .route("/api/projects/{project_id}/account", put(set_project_account))
        .route("/api/imputations", get(get_week).put(save_week))
        .route("/api/imputations/propose", post(propose_week))
        .route("/api/imputations/saisies", get(get_tracking).post(add_saisie).delete(remove_saisie))
        // recherche sémantique
        .route("/api/semantic/status", get(semantic_status))
        .route("/api/semantic/sync", post(semantic_sync))
        .route("/api/semantic/search", get(semantic_search))
        .route("/api/semantic/related", get(semantic_related))
        // actions IA de l'éditeur
        .route("/api/assist/extract-tasks", post(extract_tasks))
        .route("/api/assist/text", post(transform_text))
        // chat
        .route("/api/chat", post(chat))
        .route("/api/chat/stream", post(chat_stream))
        .route("/api/chat/apply", post(chat_apply))
        .with_state(services);

    let app = match options.frontend {
        Frontend::None => api,
        Frontend::Dir(dir) => api.fallback_service(tower_http::services::ServeDir::new(dir)),
        Frontend::Embedded(assets) => api.fallback(move |req: Request| serve_asset(assets.clone(), req)),
    };
    match options.token {
        Some(token) => app.layer(middleware::from_fn(move |req: Request, next: Next| check_token(token.clone(), req, next))),
        None => app,
    }
}

async fn serve_asset(assets: AssetFn, req: Request) -> Response {
    let path = req.uri().path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    match assets(path) {
        Some((bytes, mime)) => ([(header::CONTENT_TYPE, mime)], bytes).into_response(),
        None => (StatusCode::NOT_FOUND, Json(json!({"detail": "Not Found"}))).into_response(),
    }
}

const TOKEN_COOKIE: &str = "tempo_token";

async fn check_token(token: String, req: Request, next: Next) -> Response {
    let from_query = req
        .uri()
        .query()
        .and_then(|q| q.split('&').find_map(|pair| pair.strip_prefix("token=")))
        .is_some_and(|t| t == token);
    if from_query {
        let cookie = format!("{TOKEN_COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/");
        let location = req.uri().path().to_string();
        return Response::builder()
            .status(StatusCode::SEE_OTHER)
            .header(header::LOCATION, location)
            .header(header::SET_COOKIE, cookie)
            .body(Body::empty())
            .unwrap();
    }
    let expected = format!("{TOKEN_COOKIE}={token}");
    let authorized = req
        .headers()
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .any(|c| c.trim() == expected);
    if authorized {
        next.run(req).await
    } else {
        (StatusCode::FORBIDDEN, Json(json!({"detail": "Accès refusé"}))).into_response()
    }
}

// --- général -------------------------------------------------------------------

async fn get_config(State(s): State<AppState>) -> ApiResult {
    Ok(Json(json!({
        "vault": s.vault.root.to_string_lossy(), "llm_enabled": s.settings.llm_enabled(),
        "model_fast": s.settings.model_fast, "model_smart": s.settings.model_smart,
        "statuses": STATUSES, "priorities": PRIORITIES, "semantic_enabled": s.semantic.enabled(),
        "shortcuts": {"back": s.settings.shortcut_back, "forward": s.settings.shortcut_forward},
    })))
}

async fn get_state(State(s): State<AppState>) -> ApiResult {
    blocking(move || {
        Ok(json!({"revision": s.index.revision(), "inbox": s.inbox.list()?.len(),
                  "imputation_missing": s.imputations.missing_days(today())?}))
    })
    .await
}

async fn reindex(State(s): State<AppState>) -> ApiResult {
    blocking(move || Ok(json!({"count": s.index.rebuild()?}))).await
}

// --- notes ---------------------------------------------------------------------

async fn list_files(State(s): State<AppState>) -> ApiResult {
    blocking(move || Ok(s.index.all_notes()?.iter().map(|n| n.summary()).collect::<Vec<_>>())).await
}

async fn read_note(State(s): State<AppState>, Query(q): Query<PathParam>) -> ApiResult {
    blocking(move || s.vault.read(&q.path)).await
}

async fn write_note(State(s): State<AppState>, JsonBody(body): JsonBody<NoteWrite>) -> ApiResult {
    blocking(move || {
        let hash = s.vault.write(&body.path, &body.content, body.expected_hash.as_deref(), false)?;
        s.index.upsert(&body.path)?;
        Ok(json!({"path": body.path, "hash": hash}))
    })
    .await
}

async fn create_note(State(s): State<AppState>, JsonBody(body): JsonBody<NoteCreate>) -> ApiResult {
    blocking(move || Ok(json!({"path": s.journal.create_note(&body.kind, &body.title, body.date.as_deref())?}))).await
}

async fn delete_note(State(s): State<AppState>, Query(q): Query<PathParam>) -> ApiResult {
    blocking(move || {
        s.vault.delete(&q.path)?;
        s.index.remove(&q.path)?;
        Ok(json!({"ok": true}))
    })
    .await
}

async fn journal_today(State(s): State<AppState>, Query(q): Query<DateParam>) -> ApiResult {
    blocking(move || Ok(json!({"path": s.journal.ensure(q.date.as_deref())?}))).await
}

async fn search(State(s): State<AppState>, Query(q): Query<SearchParam>) -> ApiResult {
    blocking(move || s.index.search(&q.q, 30)).await
}

async fn resolve(State(s): State<AppState>, Query(q): Query<NameParam>) -> ApiResult {
    blocking(move || match s.index.resolve_link(&q.name)? {
        Some(path) => Ok(json!({"path": path})),
        None => Err(Error::NotFound(format!("Aucune note ne correspond à « {} »", q.name))),
    })
    .await
}

async fn links(State(s): State<AppState>, Query(q): Query<PathParam>) -> ApiResult {
    blocking(move || {
        Ok(json!({
            "backlinks": s.index.backlinks(&q.path)?.iter().map(|n| n.summary()).collect::<Vec<_>>(),
            "outlinks": s.index.outlinks(&q.path)?.iter().map(|n| n.summary()).collect::<Vec<_>>(),
        }))
    })
    .await
}

// --- tâches --------------------------------------------------------------------

async fn list_tasks(State(s): State<AppState>, Query(q): Query<TaskFilter>) -> ApiResult {
    blocking(move || s.tasks.list(q.status.as_deref(), q.project.as_deref())).await
}

async fn get_task(State(s): State<AppState>, Path(task_id): Path<String>) -> ApiResult {
    blocking(move || s.tasks.get(&task_id)).await
}

async fn create_task(State(s): State<AppState>, JsonBody(body): JsonBody<TaskCreate>) -> ApiResult {
    blocking(move || {
        let new = NewTask {
            title: body.title,
            project: body.project,
            priority: body.priority,
            status: body.status,
            due: body.due,
            description: body.description,
            origin: body.origin,
        };
        s.tasks.create(new, None)
    })
    .await
}

async fn update_task(State(s): State<AppState>, Path(task_id): Path<String>, JsonBody(body): JsonBody<TaskPatch>) -> ApiResult {
    blocking(move || s.tasks.update(&task_id, &body.changes, body.note.as_deref(), None)).await
}

// --- inbox ---------------------------------------------------------------------

async fn inbox_list(State(s): State<AppState>) -> ApiResult {
    blocking(move || s.inbox.list()).await
}

async fn inbox_add(State(s): State<AppState>, JsonBody(body): JsonBody<InboxAdd>) -> ApiResult {
    blocking(move || Ok(json!({"paths": s.inbox.add(&body.text)?}))).await
}

async fn inbox_analyze(State(s): State<AppState>, Query(q): Query<PathParam>) -> ApiResult {
    blocking(move || s.inbox.analyze(&q.path)).await
}

async fn inbox_apply(State(s): State<AppState>, JsonBody(body): JsonBody<InboxApply>) -> ApiResult {
    blocking(move || s.inbox.apply(&body.path, &body.proposal)).await
}

async fn inbox_discard(State(s): State<AppState>, Query(q): Query<PathParam>) -> ApiResult {
    blocking(move || {
        s.inbox.discard(&q.path)?;
        Ok(json!({"ok": true}))
    })
    .await
}

// --- rapports ------------------------------------------------------------------

async fn weekly_report(State(s): State<AppState>, JsonBody(body): JsonBody<WeeklyRequest>) -> ApiResult {
    blocking(move || {
        let day = body.date.as_deref().filter(|d| !d.is_empty()).map(parse_date).transpose()?;
        s.reports.weekly(day, &body.extra)
    })
    .await
}

async fn task_report(State(s): State<AppState>, Path(task_id): Path<String>) -> ApiResult {
    blocking(move || s.reports.task_report(&task_id)).await
}

// --- comptes d'imputation ------------------------------------------------------

async fn list_accounts(State(s): State<AppState>) -> ApiResult {
    blocking(move || s.imputations.accounts()).await
}

async fn create_account(State(s): State<AppState>, JsonBody(body): JsonBody<AccountCreate>) -> ApiResult {
    blocking(move || s.imputations.create_account(&body.title, &body.alias, body.days, &body.description)).await
}

async fn update_account(State(s): State<AppState>, Path(alias): Path<String>, JsonBody(body): JsonBody<AccountPatch>) -> ApiResult {
    blocking(move || s.imputations.update_account(&alias, body.title.as_deref(), body.days, body.description.as_deref()))
        .await
}

async fn list_projects(State(s): State<AppState>) -> ApiResult {
    blocking(move || s.imputations.projects()).await
}

async fn create_project(State(s): State<AppState>, JsonBody(body): JsonBody<ProjectCreate>) -> ApiResult {
    blocking(move || s.imputations.create_project(&body.title)).await
}

async fn set_project_account(
    State(s): State<AppState>,
    Path(project_id): Path<String>,
    JsonBody(body): JsonBody<ProjectAccount>,
) -> ApiResult {
    blocking(move || s.imputations.set_project_account(&project_id, body.compte.as_deref())).await
}

async fn get_week(State(s): State<AppState>, Query(q): Query<DateParam>) -> ApiResult {
    blocking(move || s.imputations.week(date_or_today(q.date.as_deref())?)).await
}

async fn save_week(State(s): State<AppState>, JsonBody(body): JsonBody<WeekHours>) -> ApiResult {
    blocking(move || s.imputations.save_week(date_or_today(Some(&body.date))?, &body.hours, &body.comment)).await
}

async fn propose_week(State(s): State<AppState>, Query(q): Query<DateParam>) -> ApiResult {
    blocking(move || s.imputations.propose(date_or_today(q.date.as_deref())?)).await
}

async fn get_tracking(State(s): State<AppState>) -> ApiResult {
    blocking(move || s.imputations.tracking(today())).await
}

async fn add_saisie(State(s): State<AppState>, JsonBody(body): JsonBody<SaisieDate>) -> ApiResult {
    blocking(move || s.imputations.add_saisie(parse_date(&body.date)?, today())).await
}

async fn remove_saisie(State(s): State<AppState>, Query(q): Query<SaisieDate>) -> ApiResult {
    blocking(move || s.imputations.remove_saisie(parse_date(&q.date)?, today())).await
}

// --- recherche sémantique ------------------------------------------------------

async fn semantic_status(State(s): State<AppState>) -> ApiResult {
    blocking(move || s.semantic.status()).await
}

async fn semantic_sync(State(s): State<AppState>) -> ApiResult {
    blocking(move || {
        let synced = s.semantic.sync();
        let mut result = s.semantic.status()?;
        result["synced"] = synced.into();
        Ok(result)
    })
    .await
}

async fn semantic_search(State(s): State<AppState>, Query(q): Query<SearchParam>) -> ApiResult {
    blocking(move || s.semantic.search(&q.q, 10)).await
}

async fn semantic_related(State(s): State<AppState>, Query(q): Query<PathParam>) -> ApiResult {
    blocking(move || s.semantic.related(&q.path, 8)).await
}

// --- actions IA de l'éditeur -----------------------------------------------------

async fn extract_tasks(State(s): State<AppState>, Query(q): Query<PathParam>) -> ApiResult {
    blocking(move || s.assist.extract_tasks(&q.path, None)).await
}

async fn transform_text(State(s): State<AppState>, JsonBody(body): JsonBody<TextAction>) -> ApiResult {
    blocking(move || s.assist.transform(&body.action, &body.text)).await
}

// --- chat ----------------------------------------------------------------------

async fn chat(State(s): State<AppState>, JsonBody(body): JsonBody<ChatRequest>) -> ApiResult {
    check_conversation(&body.messages)?;
    blocking(move || s.chat.ask(&body.messages, None)).await
}

/// Réponse diffusée en Server-Sent Events (step, delta, done ou error).
async fn chat_stream(State(s): State<AppState>, JsonBody(body): JsonBody<ChatRequest>) -> Result<Response> {
    check_conversation(&body.messages)?;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    tokio::task::spawn_blocking(move || {
        let mut emit = |event: Value| {
            let _ = tx.send(format!("data: {event}\n\n"));
        };
        if let Err(err) = s.chat.stream(&body.messages, None, &mut emit) {
            emit(json!({"type": "error", "detail": err.to_string()}));
        }
    });
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|chunk| (Ok::<_, Infallible>(bytes::Bytes::from(chunk)), rx))
    });
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header("X-Accel-Buffering", HeaderValue::from_static("no"))
        .body(Body::from_stream(stream))
        .unwrap())
}

async fn chat_apply(State(s): State<AppState>, JsonBody(proposal): JsonBody<ChatProposal>) -> ApiResult {
    blocking(move || s.chat.apply(&proposal, None)).await
}

