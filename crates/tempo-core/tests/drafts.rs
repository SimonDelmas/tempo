mod common;

use axum::http::{Method, StatusCode};
use chrono::NaiveDate;
use common::TestApp;
use serde_json::json;
use tempo_core::drafts::DraftService;
use tempo_core::llm::FakeLlm;
use tempo_core::Error;

fn service(c: &common::Ctx, llm: std::sync::Arc<FakeLlm>) -> DraftService {
    DraftService::new(c.vault.clone(), c.index.clone(), c.tasks.clone(), c.journal.clone(), llm, c.settings.clone())
}

#[test]
fn drafts_are_files_outside_the_index() {
    let c = common::ctx();
    let drafts = service(&c, FakeLlm::new(Vec::<String>::new()));
    let first = drafts.create("").unwrap();
    let second = drafts.create("Idées").unwrap();
    assert_ne!(first.id, second.id);
    drafts.save(&first.id, Some("Réunion"), Some("- point 1\n- point 2\n")).unwrap();

    let list = drafts.list().unwrap();
    assert_eq!(list.iter().map(|d| d.title.as_str()).collect::<Vec<_>>(), vec!["Réunion", "Idées"]);
    assert_eq!(list[0].content, "- point 1\n- point 2\n");
    assert!(c.vault.root.join(".brouillons").join(format!("{}.md", first.id)).is_file());
    // pas des notes : ni dans la liste des fichiers du vault, ni dans l'index
    c.index.rebuild().unwrap();
    assert!(c.vault.list_files().iter().all(|f| !f.starts_with(".brouillons")));
    assert!(c.index.all_notes().unwrap().iter().all(|n| !n.path.starts_with(".brouillons")));

    // un service recréé (redémarrage) retrouve les brouillons
    let reopened = service(&c, FakeLlm::new(Vec::<String>::new()));
    assert_eq!(reopened.get(&first.id).unwrap().content, "- point 1\n- point 2\n");

    drafts.delete(&second.id).unwrap();
    assert_eq!(drafts.list().unwrap().len(), 1);
    assert!(matches!(drafts.get(&second.id), Err(Error::NotFound(_))));
    assert!(matches!(drafts.get("../projets/sap"), Err(Error::Vault(_))));
}

#[test]
fn analyze_cleans_the_proposal() {
    let c = common::ctx();
    let response = json!({
        "note": {"type": "compte-rendu", "title": "", "content": "## Décisions\n- recette le 28"},
        "tasks": [
            {"title": "Rédiger la procédure de recette", "project": "sap", "duplicate_of": "T-004"},
            {"title": "Relancer Marc", "priority": "urgente", "status": "fini", "due": "vendredi", "duplicate_of": "T-999"},
            {"title": " "}
        ],
        "comment": "Compte-rendu et deux actions."
    });
    let llm = FakeLlm::new([response.to_string()]);
    let drafts = service(&c, llm.clone());
    let draft = drafts.create("Copil SAP").unwrap();
    assert!(matches!(drafts.analyze(&draft.id, None), Err(Error::Vault(_))));
    drafts.save(&draft.id, None, Some("Copil : recette le 28. Je dois relancer Marc vendredi.")).unwrap();

    let proposal = drafts.analyze(&draft.id, NaiveDate::from_ymd_opt(2026, 10, 19)).unwrap();
    let note = proposal.note.unwrap();
    assert_eq!((note.kind.as_str(), note.title.as_str()), ("note", "Copil SAP"));
    assert_eq!(proposal.tasks.len(), 2);
    assert!(!proposal.tasks[0].accepted && proposal.tasks[0].duplicate_of.as_deref() == Some("T-004"));
    let relance = &proposal.tasks[1];
    assert!(relance.accepted && relance.duplicate_of.is_none() && relance.due.is_none());
    assert_eq!((relance.priority.as_str(), relance.status.as_str()), ("normale", "a_faire"));

    let messages = &llm.calls()[0].messages;
    assert!(messages[0].content.contains("2026-10-19"));
    assert!(messages[1].content.contains("T-004 : Rédiger la procédure de recette"));
    assert!(messages[1].content.contains("Je dois relancer Marc vendredi."));
}

#[tokio::test]
async fn drafts_api_flow() {
    let proposal = json!({
        "note": {"type": "reunion", "title": "Point SAP", "content": "## Décisions\n- recette le 28"},
        "tasks": [{"title": "Relancer Marc", "project": "sap", "due": "2026-10-23"}],
        "comment": ""
    });
    let llm = FakeLlm::new([proposal.to_string()]);
    let app = TestApp::new(Some(llm), None);

    let draft = app.post("/api/drafts", Some(json!({}))).await.json();
    let id = draft["id"].as_str().unwrap().to_string();
    let saved = app
        .call(Method::PUT, &format!("/api/drafts/{id}"), Some(json!({"title": "Point", "content": "Recette le 28, relancer Marc"})))
        .await
        .json();
    assert_eq!((saved["title"].as_str(), saved["content"].as_str()), (Some("Point"), Some("Recette le 28, relancer Marc")));
    assert_eq!(app.get("/api/drafts").await.json().as_array().unwrap().len(), 1);

    let analyzed = app.post(&format!("/api/drafts/{id}/analyze"), None).await.json();
    assert_eq!(analyzed["note"]["type"], "reunion");
    assert_eq!(analyzed["tasks"][0]["accepted"], true);

    let empty = json!({"note": null, "tasks": [], "comment": ""});
    let refused = app.post(&format!("/api/drafts/{id}/apply"), Some(json!({"proposal": empty}))).await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST);

    let result = app.post(&format!("/api/drafts/{id}/apply"), Some(json!({"proposal": analyzed, "close": true}))).await.json();
    let note_path = result["note"].as_str().unwrap();
    assert!(note_path.starts_with("reunions/") && note_path.ends_with("Point SAP.md"));
    let task_id = result["created_tasks"][0].as_str().unwrap();
    let note = app.get(&format!("/api/notes?path={}", common::q(note_path))).await.json();
    let content = note["content"].as_str().unwrap();
    assert!(content.contains("# Point SAP\n\n## Décisions\n- recette le 28\n"));
    assert!(content.contains(&format!("## Tâches\n- [[{task_id}]]")));
    assert!(!content.contains("## Participants"));
    assert_eq!(note["meta"]["tasks"], json!([format!("[[{task_id}]]")]));
    let task = app.get(&format!("/api/tasks/{task_id}")).await.json();
    let stem = note_path.rsplit('/').next().unwrap().trim_end_matches(".md");
    assert!(task["content"].as_str().unwrap().contains(&format!("créée depuis [[{stem}]]")));
    assert_eq!(task["due"], "2026-10-23");

    // le brouillon est fermé
    assert_eq!(app.get("/api/drafts").await.json(), json!([]));
    assert_eq!(app.call(Method::DELETE, &format!("/api/drafts/{id}"), None).await.status, StatusCode::NOT_FOUND);
}
