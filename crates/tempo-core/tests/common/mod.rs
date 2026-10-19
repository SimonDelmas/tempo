//! Fixtures des tests : copie de `vault-exemple/` dans un dossier temporaire (ne jamais écrire
//! dans `vault-exemple/` depuis un test) et client HTTP de test sur le routeur.
#![allow(dead_code)]

use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use serde_json::Value;
use tempfile::TempDir;
use tower::ServiceExt;

use tempo_core::index::Index;
use tempo_core::journal::JournalService;
use tempo_core::llm::{DisabledLlm, Llm};
use tempo_core::semantic::Embedder;
use tempo_core::tasks::TaskService;
use tempo_core::vault::Vault;
use tempo_core::{router, ApiOptions, Services, Settings};

pub struct Ctx {
    _dir: TempDir,
    pub settings: Arc<Settings>,
    pub vault: Arc<Vault>,
    pub index: Arc<Index>,
    pub tasks: Arc<TaskService>,
    pub journal: Arc<JournalService>,
}

fn copy_dir(from: &Path, to: &Path) {
    for entry in walkdir::WalkDir::new(from) {
        let entry = entry.unwrap();
        let target = to.join(entry.path().strip_prefix(from).unwrap());
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&target).unwrap();
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

pub fn example_vault() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vault-exemple");
    copy_dir(&source, &dir.path().join("vault"));
    dir
}

pub fn ctx() -> Ctx {
    let dir = example_vault();
    let settings = Arc::new(Settings::for_vault(dir.path().join("vault")));
    let vault = Arc::new(Vault::new(&settings.vault).unwrap());
    let index = Arc::new(Index::new(vault.clone(), &vault.root.join(".tempo").join("index.db")).unwrap());
    index.rebuild().unwrap();
    let tasks = Arc::new(TaskService::new(vault.clone(), index.clone()));
    let journal = Arc::new(JournalService::new(vault.clone(), index.clone()));
    Ctx { _dir: dir, settings, vault, index, tasks, journal }
}

/// Application complète (index reconstruit) avec un client HTTP de test.
pub struct TestApp {
    _dir: TempDir,
    pub services: Arc<Services>,
    app: axum::Router,
}

pub struct Reply {
    pub status: StatusCode,
    pub text: String,
}

impl Reply {
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.text).unwrap_or_else(|_| panic!("réponse non json : {}", self.text))
    }
}

impl TestApp {
    pub fn new(llm: Option<Arc<dyn Llm>>, embedder: Option<Embedder>) -> Self {
        let dir = example_vault();
        let settings = Settings::for_vault(dir.path().join("vault"));
        let services = Services::new(settings, llm.unwrap_or_else(|| Arc::new(DisabledLlm)), embedder).unwrap();
        services.start().unwrap();
        let app = router(services.clone(), ApiOptions::default());
        TestApp { _dir: dir, services, app }
    }

    pub async fn call(&self, method: Method, uri: &str, body: Option<Value>) -> Reply {
        let mut request = Request::builder().method(method).uri(uri);
        let body = match body {
            Some(value) => {
                request = request.header("content-type", "application/json");
                Body::from(value.to_string())
            }
            None => Body::empty(),
        };
        let response = self.app.clone().oneshot(request.body(body).unwrap()).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        Reply { status, text: String::from_utf8_lossy(&bytes).into_owned() }
    }

    pub async fn get(&self, uri: &str) -> Reply {
        self.call(Method::GET, uri, None).await
    }

    pub async fn post(&self, uri: &str, body: Option<Value>) -> Reply {
        self.call(Method::POST, uri, body).await
    }
}

/// Encode un paramètre d'URL.
pub fn q(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}
