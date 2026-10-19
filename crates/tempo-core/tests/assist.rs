mod common;

use chrono::NaiveDate;
use common::TestApp;
use serde_json::json;
use tempo_core::assist::AssistService;
use tempo_core::llm::FakeLlm;
use tempo_core::Error;

const NOTE: &str = "reunions/2026/2026-10-15-copil-sap.md";

#[test]
fn extract_tasks_cleans_drafts() {
    let c = common::ctx();
    let drafts = json!({"tasks": [
        {"title": "Rédiger la procédure de recette", "project": "sap", "duplicate_of": "T-004"},
        {"title": "Relancer Marc pour les jeux de test", "priority": "urgente", "due": "vendredi", "duplicate_of": "T-999"},
        {"title": "  "},
    ]});
    let llm = FakeLlm::new([drafts.to_string()]);
    let assist = AssistService::new(c.vault.clone(), c.index.clone(), c.tasks.clone(), llm.clone(), c.settings.clone());
    let result = assist.extract_tasks(NOTE, NaiveDate::from_ymd_opt(2026, 10, 19)).unwrap();
    let tasks = result["tasks"].as_array().unwrap();
    let titles: Vec<&str> = tasks.iter().map(|t| t["title"].as_str().unwrap()).collect();
    assert_eq!(titles, vec!["Rédiger la procédure de recette", "Relancer Marc pour les jeux de test"]);
    assert_eq!(tasks[0]["duplicate_of"], "T-004");
    assert_eq!(tasks[1]["priority"], "normale");
    assert!(tasks[1]["due"].is_null() && tasks[1]["duplicate_of"].is_null());
    let prompt = &llm.calls()[0].messages;
    assert!(prompt[0].content.contains("2026-10-19"));
    assert!(prompt[1].content.contains("T-004 : Rédiger la procédure de recette"));
    assert!(prompt[1].content.contains("Recette maintenue au 28 octobre"));
    assert!(prompt[1].content.lines().next().unwrap().contains("sap"));
}

#[test]
fn transform() {
    let c = common::ctx();
    let llm = FakeLlm::new(["```markdown\n- point 1\n```"]);
    let assist = AssistService::new(c.vault.clone(), c.index.clone(), c.tasks.clone(), llm.clone(), c.settings.clone());
    assert_eq!(assist.transform("resumer", "un long texte").unwrap(), json!({"text": "- point 1"}));
    assert_eq!(llm.calls()[0].model, c.settings.model_fast);
    assert!(matches!(assist.transform("traduire", "x"), Err(Error::Vault(_))));
    assert!(matches!(assist.transform("resumer", "  "), Err(Error::Vault(_))));
}

#[tokio::test]
async fn assist_api_and_task_origin() {
    let llm = FakeLlm::new([json!({"tasks": []}).to_string(), "Texte reformulé".to_string()]);
    let app = TestApp::new(Some(llm), None);
    assert_eq!(app.post(&format!("/api/assist/extract-tasks?path={NOTE}"), None).await.json(), json!({"tasks": []}));
    let text = app.post("/api/assist/text", Some(json!({"action": "reformuler", "text": "x"}))).await.json();
    assert_eq!(text, json!({"text": "Texte reformulé"}));
    let task = app.post("/api/tasks", Some(json!({"title": "Relancer Marc", "origin": "2026-10-15-copil-sap"}))).await.json();
    assert!(task["content"].as_str().unwrap().contains("créée depuis [[2026-10-15-copil-sap]]"));
}
