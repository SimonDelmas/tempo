mod common;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use common::{q, TestApp};
use serde_json::json;
use tempo_core::api::{router, ApiOptions, AssetFn, Frontend};
use tempo_core::llm::FakeLlm;
use tower::ServiceExt;

#[tokio::test]
async fn api_flow() {
    let app = TestApp::new(None, None);
    let files = app.get("/api/files").await.json();
    assert!(files.as_array().unwrap().iter().any(|f| f["path"] == "projets/sap.md"));

    let note = app.get("/api/notes?path=projets/sap.md").await.json();
    let ok = app
        .call(
            Method::PUT,
            "/api/notes",
            Some(json!({"path": "projets/sap.md", "content": format!("{}\nAjout\n", note["content"].as_str().unwrap()),
                        "expected_hash": note["hash"]})),
        )
        .await;
    assert_eq!(ok.status, StatusCode::OK);
    let conflict = app
        .call(Method::PUT, "/api/notes", Some(json!({"path": "projets/sap.md", "content": "x", "expected_hash": note["hash"]})))
        .await;
    assert_eq!(conflict.status, StatusCode::CONFLICT);
    assert!(conflict.json()["current_content"].as_str().unwrap().ends_with("Ajout\n"));

    let created = app.post("/api/notes", Some(json!({"type": "reunion", "title": "Point hebdo", "date": "2026-10-19"}))).await.json();
    assert_eq!(created["path"], "reunions/2026/2026-10-19-point-hebdo.md");
    for (kind, folder) in [("ressource", "ressources"), ("formation", "formations")] {
        let created = app.post("/api/notes", Some(json!({"type": kind, "title": "Guide ABAP"}))).await.json();
        assert_eq!(created["path"], format!("{folder}/guide-abap.md"));
        let read = app.get(&format!("/api/notes?path={}", q(created["path"].as_str().unwrap()))).await.json();
        assert_eq!(read["meta"]["type"], kind);
    }

    let today = app.post("/api/journal/today?date=2026-10-17", None).await.json();
    assert_eq!(today["path"], "journal/2026/2026-10-17.md");

    let task = app.post("/api/tasks", Some(json!({"title": "Nouvelle tâche", "project": "sap"}))).await.json();
    let patched =
        app.call(Method::PATCH, &format!("/api/tasks/{}", task["id"].as_str().unwrap()), Some(json!({"changes": {"status": "en_cours"}}))).await;
    assert_eq!(patched.json()["status"], "en_cours");
    let en_cours = app.get("/api/tasks?status=en_cours").await.json();
    assert_eq!(en_cours.as_array().unwrap().last().unwrap()["id"], task["id"]);

    let links = app.get("/api/links?path=taches/T-004-rediger-procedure-recette.md").await.json();
    assert!(links["backlinks"].as_array().unwrap().iter().any(|n| n["stem"] == "2026-10-15-copil-sap"));
    assert_eq!(app.get("/api/resolve?name=T-004").await.status, StatusCode::OK);
    assert_eq!(app.get("/api/resolve?name=inconnu").await.status, StatusCode::NOT_FOUND);

    assert_eq!(app.get("/api/notes?path=../x.md").await.status, StatusCode::BAD_REQUEST);
    // sans configuration, les fonctions IA renvoient une erreur explicite
    let response = app.post("/api/inbox/analyze?path=00-inbox/exemple-teams.md", None).await;
    assert_eq!(response.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(response.json()["detail"].as_str().unwrap().contains("VIBE_BASE_URL"));

    let search = app.get(&format!("/api/search?q={}", q("procédure recette"))).await.json();
    assert!(!search.as_array().unwrap().is_empty());
    let state = app.get("/api/state").await.json();
    assert!(state["revision"].as_u64().unwrap() > 0 && state["inbox"] == 1);
    let config = app.get("/api/config").await.json();
    assert_eq!(config["llm_enabled"], false);
    assert_eq!(config["statuses"][0], "a_faire");

    let invalid = app.post("/api/tasks", Some(json!({"project": "sap"}))).await;
    assert_eq!(invalid.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(invalid.json()["detail"].is_string());
}

#[tokio::test]
async fn delete_note_moves_it_to_trash() {
    let app = TestApp::new(None, None);
    let deleted = app.call(Method::DELETE, "/api/notes?path=sujets/mapping-unites-de-mesure.md", None).await;
    assert_eq!(deleted.status, StatusCode::OK);
    let files = app.get("/api/files").await.json();
    assert!(!files.as_array().unwrap().iter().any(|f| f["path"] == "sujets/mapping-unites-de-mesure.md"));
    assert!(app.services.vault.root.join(".trash").is_dir());
}

#[tokio::test]
async fn token_protects_the_api_and_embedded_assets_are_served() {
    let app = TestApp::new(Some(FakeLlm::new(Vec::<String>::new())), None);
    let assets: AssetFn = Arc::new(|path: &str| (path == "index.html").then(|| (b"<html>".to_vec(), "text/html".to_string())));
    let router = router(app.services.clone(), ApiOptions { frontend: Frontend::Embedded(assets), token: Some("secret".into()) });
    let send = |uri: &str, cookie: Option<&str>| {
        let mut request = axum::http::Request::builder().uri(uri);
        if let Some(cookie) = cookie {
            request = request.header("cookie", cookie);
        }
        router.clone().oneshot(request.body(axum::body::Body::empty()).unwrap())
    };
    assert_eq!(send("/api/config", None).await.unwrap().status(), StatusCode::FORBIDDEN);
    assert_eq!(send("/api/config", Some("tempo_token=autre")).await.unwrap().status(), StatusCode::FORBIDDEN);
    let opened = send("/?token=secret", None).await.unwrap();
    assert_eq!(opened.status(), StatusCode::SEE_OTHER);
    let cookie = opened.headers()["set-cookie"].to_str().unwrap();
    assert!(cookie.starts_with("tempo_token=secret;") && cookie.contains("HttpOnly"));
    assert_eq!(send("/api/config", Some("a=b; tempo_token=secret")).await.unwrap().status(), StatusCode::OK);
    let page = send("/", Some("tempo_token=secret")).await.unwrap();
    assert_eq!(page.status(), StatusCode::OK);
    assert_eq!(page.headers()["content-type"], "text/html");
    assert_eq!(send("/absent.js", Some("tempo_token=secret")).await.unwrap().status(), StatusCode::NOT_FOUND);
}
