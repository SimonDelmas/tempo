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

    // la dernière tâche passe en tête : elle seule reçoit un rang, les autres restent sans
    // rang (donc après) et leurs fichiers ne changent pas
    let before_text = c.vault.read_text(&c.tasks.path_of("T-003").unwrap()).unwrap();
    let backlog = c.tasks.move_task("T-004", Some("T-001"), None).unwrap();
    assert_eq!(ids(&backlog), ["T-004", "T-001", "T-002", "T-003"]);
    let ranks: Vec<Option<i64>> = backlog.iter().map(|t| t.rank).collect();
    assert_eq!(ranks, [Some(1000), None, None, None]);
    assert_eq!(c.vault.read_text(&c.tasks.path_of("T-003").unwrap()).unwrap(), before_text);
    let text = c.vault.read_text(&c.tasks.path_of("T-004").unwrap()).unwrap();
    let (meta, _) = split_frontmatter(&text);
    assert_eq!(meta["project"], "sap", "le projet ne change pas");
    assert!(!text.contains("projet sap"), "pas d'historique pour un simple déplacement");

    // insertion parmi des tâches sans rang : les voisines prennent un rang pour garder l'ordre
    let backlog = c.tasks.move_task("T-003", None, Some("T-001")).unwrap();
    assert_eq!(ids(&backlog), ["T-004", "T-001", "T-003", "T-002"]);
    assert!(backlog.iter().all(|t| t.rank.is_some()));

    // une place libre entre deux rangs : seul le fichier déplacé change
    let before_text = c.vault.read_text(&c.tasks.path_of("T-003").unwrap()).unwrap();
    assert_eq!(ids(&c.tasks.move_task("T-002", Some("T-003"), None).unwrap()), ["T-004", "T-001", "T-002", "T-003"]);
    assert_eq!(c.vault.read_text(&c.tasks.path_of("T-003").unwrap()).unwrap(), before_text);

    // sans repère : fin du backlog ; repère inconnu : 404
    assert_eq!(ids(&c.tasks.move_task("T-004", None, None).unwrap()), ["T-001", "T-002", "T-003", "T-004"]);
    assert!(c.tasks.move_task("T-004", Some("T-999"), None).is_err());

    // le board d'une semaine reprend le même ordre
    for id in ["T-001", "T-003", "T-004"] {
        plan(&c, id, Some("2026-S43"), "2026-10-19");
    }
    c.tasks.move_task("T-004", Some("T-001"), None).unwrap();
    assert_eq!(ids(&c.tasks.board(None, Some("2026-10-21")).unwrap().tasks), ["T-004", "T-001", "T-003"]);
}

#[test]
fn created_task_gets_rank_once_backlog_is_ranked() {
    let c = common::ctx();
    let new = |title: &str| tempo_core::tasks::NewTask { title: title.into(), ..Default::default() };
    // backlog sans rang : la nouvelle tâche suit les autres par identifiant
    let t5 = c.tasks.create(new("Cinq"), Some("2026-10-19")).unwrap();
    assert_eq!(t5.task.rank, None);
    // une fois tout le backlog ordonné, elle prend un rang après le dernier
    for id in ["T-001", "T-002", "T-003", "T-004", "T-005"] {
        c.tasks.move_task(id, None, None).unwrap();
    }
    let last = c.tasks.backlog(None).unwrap().tasks.iter().filter_map(|t| t.rank).max().unwrap();
    let t6 = c.tasks.create(new("Six"), Some("2026-10-19")).unwrap();
    assert_eq!(t6.task.rank, Some(last + 1000));
    assert_eq!(ids(&c.tasks.backlog(None).unwrap().tasks).last(), Some(&"T-006"));
}

#[test]
fn index_filters_tasks_by_columns() {
    let c = common::ctx();
    use tempo_core::index::TaskQuery;
    let q = |query: TaskQuery| -> Vec<String> { c.tasks.query(&query).unwrap().into_iter().map(|t| t.id).collect() };
    assert_eq!(q(TaskQuery { status: Some("terminee"), ..Default::default() }), ["T-002"]);
    assert_eq!(q(TaskQuery { open: true, ..Default::default() }), ["T-001", "T-003", "T-004"]);
    plan(&c, "T-003", Some("2026-S44"), "2026-10-19");
    assert_eq!(q(TaskQuery { week: Some("2026-S44"), ..Default::default() }), ["T-003"]);
    assert_eq!(c.index.task_weeks().unwrap(), ["2026-S44"]);
    // l'index suit les modifications et se reconstruit à l'identique
    c.tasks.update("T-003", json!({"status": "terminee"}).as_object().unwrap(), None, Some("2026-10-19")).unwrap();
    assert_eq!(q(TaskQuery { status: Some("terminee"), ..Default::default() }), ["T-002", "T-003"]);
    c.index.rebuild().unwrap();
    assert_eq!(q(TaskQuery { status: Some("terminee"), ..Default::default() }), ["T-002", "T-003"]);
    assert_eq!(c.index.task_weeks().unwrap(), ["2026-S44"]);
}

#[test]
fn archive_done_tasks_and_reopen() {
    let c = common::ctx();
    plan(&c, "T-002", Some("2026-S42"), "2026-10-12");
    assert_eq!(c.tasks.archive_done(Some("2026-10-19")).unwrap(), ["T-002"]);
    assert!(c.tasks.archive_done(None).unwrap().is_empty(), "idempotent");

    let t2 = c.tasks.get("T-002").unwrap();
    assert!(t2.task.archived && t2.task.path.starts_with("taches/archives/T-002-"), "{}", t2.task.path);
    assert_eq!(t2.history.last().unwrap().text, "archivée");
    assert!(!c.vault.exists("taches/T-002-preparer-copil-sap.md"));
    // hors du backlog, mais toujours dans les listes, les boards passés et les liens
    assert!(!ids(&c.tasks.backlog(Some("2026-10-19")).unwrap().tasks).contains(&"T-002"));
    assert!(ids(&c.tasks.all().unwrap()).contains(&"T-002"));
    assert_eq!(ids(&c.tasks.board(Some("2026-S42"), Some("2026-10-19")).unwrap().tasks), ["T-002"]);
    assert_eq!(c.index.resolve_link("T-002").unwrap().as_deref(), Some(t2.task.path.as_str()));
    assert_eq!(c.tasks.next_id().unwrap(), "T-005");

    // reprise : la tâche revient dans taches/
    let changes = json!({"status": "en_cours"});
    let t2 = c.tasks.update("T-002", changes.as_object().unwrap(), None, Some("2026-10-20")).unwrap();
    assert!(!t2.task.archived && t2.task.path.starts_with("taches/T-002-"));
    assert_eq!(t2.history.last().unwrap().text, "statut terminee → en_cours — sortie des archives");
    assert!(ids(&c.tasks.backlog(None).unwrap().tasks).contains(&"T-002"));
}

#[tokio::test]
async fn archive_route() {
    let app = TestApp::new(None, None);
    let archived = app.post("/api/tasks/archive", None).await;
    assert_eq!(archived.status, StatusCode::OK, "{}", archived.text);
    assert_eq!(archived.json()["archived"], json!(["T-002"]));
    let task = app.get("/api/tasks/T-002").await.json();
    assert_eq!(task["archived"], true);
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
