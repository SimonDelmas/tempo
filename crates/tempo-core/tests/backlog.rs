mod common;

use axum::http::{Method, StatusCode};
use common::TestApp;
use serde_json::json;
use tempo_core::vault::split_frontmatter;

fn plan(c: &common::Ctx, id: &str, week: Option<&str>, today: &str) {
    let changes = json!({ "iteration": week });
    c.tasks.update(id, changes.as_object().unwrap(), None, Some(today)).unwrap();
}

fn ids(tasks: &[tempo_core::tasks::Task]) -> Vec<&str> {
    tasks.iter().map(|t| t.id.as_str()).collect()
}

#[test]
fn plan_and_board_by_week() {
    let c = common::ctx();
    plan(&c, "T-001", Some("2026-S43"), "2026-10-19");
    plan(&c, "T-003", Some("2026-S44"), "2026-10-19");
    let text = c.vault.read_text(&c.tasks.path_of("T-001").unwrap()).unwrap();
    assert!(text.contains("\niterations: [2026-S43]\n"), "liste en ligne : {text}");
    assert!(text.trim_end().ends_with("- 2026-10-19 : planifiée en 2026-S43"));

    let board = c.tasks.board(None, Some("2026-10-21")).unwrap();
    assert_eq!((board.week.as_str(), board.start.as_str(), board.end.as_str()), ("2026-S43", "2026-10-19", "2026-10-25"));
    assert_eq!((board.previous.as_str(), board.next.as_str()), ("2026-S42", "2026-S44"));
    assert_eq!(ids(&board.tasks), ["T-001"]);
    assert_eq!(board.weeks, ["2026-S43", "2026-S44"]);
    assert_eq!(ids(&c.tasks.board(Some("2026-S44"), Some("2026-10-21")).unwrap().tasks), ["T-003"]);
    assert!(c.tasks.board(Some("semaine"), None).is_err());

    // retrait de la planification
    plan(&c, "T-003", None, "2026-10-19");
    assert!(c.tasks.get("T-003").unwrap().task.iterations.is_empty());
}

#[test]
fn roll_over_moves_open_tasks_and_keeps_history() {
    let c = common::ctx();
    plan(&c, "T-001", Some("2026-S42"), "2026-10-12");
    plan(&c, "T-002", Some("2026-S42"), "2026-10-12"); // terminée : reste en S42
    assert_eq!(c.tasks.roll_over(Some("2026-10-19")).unwrap(), ["T-001"]);
    assert!(c.tasks.roll_over(Some("2026-10-19")).unwrap().is_empty(), "idempotent");

    let t1 = c.tasks.get("T-001").unwrap();
    assert_eq!(t1.task.iterations, ["2026-S42", "2026-S43"]);
    assert_eq!(t1.history.last().unwrap().text, "reportée de 2026-S42 en 2026-S43");
    assert_eq!(ids(&c.tasks.board(Some("2026-S42"), Some("2026-10-19")).unwrap().tasks), ["T-001", "T-002"]);
    assert_eq!(ids(&c.tasks.board(None, Some("2026-10-19")).unwrap().tasks), ["T-001"]);

    // replanifier sur une semaine future conserve la semaine passée
    plan(&c, "T-001", Some("2026-S45"), "2026-10-19");
    assert_eq!(c.tasks.get("T-001").unwrap().task.iterations, ["2026-S42", "2026-S45"]);
}

#[test]
fn move_orders_all_tasks_shared_with_board() {
    let c = common::ctx();
    // sans rang : ordre des identifiants
    assert_eq!(ids(&c.tasks.backlog(Some("2026-10-19")).unwrap().tasks), ["T-001", "T-002", "T-003", "T-004"]);

    // la dernière tâche passe en tête : tout le backlog est renuméroté
    let backlog = c.tasks.move_task("T-004", Some("T-001"), None).unwrap();
    assert_eq!(ids(&backlog), ["T-004", "T-001", "T-002", "T-003"]);
    assert!(backlog.iter().all(|t| t.rank.is_some()));
    let text = c.vault.read_text(&c.tasks.path_of("T-004").unwrap()).unwrap();
    let (meta, _) = split_frontmatter(&text);
    assert_eq!(meta["project"], "sap", "le projet ne change pas");
    assert!(!text.contains("projet sap"), "pas d'historique pour un simple déplacement");

    // une place libre entre deux rangs : seul le fichier déplacé change
    let before_text = c.vault.read_text(&c.tasks.path_of("T-002").unwrap()).unwrap();
    assert_eq!(ids(&c.tasks.move_task("T-003", None, Some("T-001")).unwrap()), ["T-004", "T-001", "T-003", "T-002"]);
    assert_eq!(c.vault.read_text(&c.tasks.path_of("T-002").unwrap()).unwrap(), before_text);

    // sans repère : fin du backlog ; repère inconnu : 404
    assert_eq!(ids(&c.tasks.move_task("T-004", None, None).unwrap()), ["T-001", "T-003", "T-002", "T-004"]);
    assert!(c.tasks.move_task("T-004", Some("T-999"), None).is_err());

    // le board d'une semaine reprend le même ordre
    for id in ["T-001", "T-003", "T-004"] {
        plan(&c, id, Some("2026-S43"), "2026-10-19");
    }
    c.tasks.move_task("T-004", Some("T-001"), None).unwrap();
    assert_eq!(ids(&c.tasks.board(None, Some("2026-10-21")).unwrap().tasks), ["T-004", "T-001", "T-003"]);
}

#[tokio::test]
async fn project_order_routes() {
    let app = TestApp::new(None, None);
    for title in ["Bascule", "Archivage"] {
        assert_eq!(app.post("/api/projects", Some(json!({"title": title}))).await.status, StatusCode::OK);
    }
    // la liste des projets reste alphabétique, le rang sert au backlog
    let ids = |v: serde_json::Value| -> Vec<String> {
        v.as_array().unwrap().iter().map(|p| p["id"].as_str().unwrap().to_string()).collect()
    };
    assert_eq!(ids(app.get("/api/projects").await.json()), ["archivage", "bascule", "sap"]);

    let moved = app.post("/api/projects/sap/move", Some(json!({"before": "archivage"}))).await;
    assert_eq!(moved.status, StatusCode::OK, "{}", moved.text);
    assert_eq!(ids(moved.json()), ["sap", "archivage", "bascule"]);
    let moved = app.post("/api/projects/archivage/move", Some(json!({}))).await.json();
    assert_eq!(ids(moved.clone()), ["sap", "bascule", "archivage"]);
    assert!(moved.as_array().unwrap().iter().all(|p| p["rank"].is_i64()));

    let note = app.get("/api/notes?path=projets/sap.md").await.json();
    assert!(note["content"].as_str().unwrap().contains("\nrank: "), "rang dans la note du projet");
    assert_eq!(app.post("/api/projects/inconnu/move", Some(json!({}))).await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn backlog_routes() {
    let app = TestApp::new(None, None);
    let created = app.post("/api/tasks", Some(json!({"title": "Préparer la démo", "project": "sap", "iteration": "2026-S50"}))).await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.text);
    let id = created.json()["id"].as_str().unwrap().to_string();
    assert_eq!(created.json()["iteration"], "2026-S50");

    let board = app.get("/api/tasks/board?week=2026-S50").await.json();
    assert_eq!(board["tasks"][0]["id"], id.as_str());
    assert_eq!(app.get("/api/tasks/board?week=x").await.status, StatusCode::BAD_REQUEST);

    let backlog = app.get("/api/tasks/backlog").await.json();
    assert_eq!(backlog["weeks"].as_array().unwrap().len(), 8);
    assert_eq!(backlog["weeks"][0]["label"], backlog["current"]);

    let order = |v: serde_json::Value| -> Vec<String> {
        v.as_array().unwrap().iter().map(|t| t["id"].as_str().unwrap().to_string()).collect()
    };
    let moved = app.post(&format!("/api/tasks/{id}/move"), Some(json!({"before": "T-001"}))).await;
    assert_eq!(moved.status, StatusCode::OK, "{}", moved.text);
    assert_eq!(order(moved.json())[0], id);
    let moved = app.post(&format!("/api/tasks/{id}/move"), Some(json!({"after": "T-002"}))).await.json();
    assert_eq!(order(moved)[..3], ["T-001", "T-002", id.as_str()]);
    let missing = app.post(&format!("/api/tasks/{id}/move"), Some(json!({"after": "T-999"}))).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);

    let unplanned = app.call(Method::PATCH, &format!("/api/tasks/{id}"), Some(json!({"changes": {"iteration": null}}))).await;
    assert!(unplanned.json()["iteration"].is_null());
}
