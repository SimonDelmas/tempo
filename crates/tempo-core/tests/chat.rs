mod common;

use std::sync::Arc;

use chrono::NaiveDate;
use common::{Ctx, TestApp};
use serde_json::{json, Map, Value};
use tempo_core::chat::{ChatMessage, ChatProposal, ChatService, ProposalKind, MAX_STEPS};
use tempo_core::llm::FakeLlm;

fn tool(name: &str, args: Value) -> String {
    json!({"action": "tool", "tool": name, "args": args}).to_string()
}

fn answer(text: &str) -> String {
    json!({"action": "answer", "answer": text}).to_string()
}

fn make_chat(c: &Ctx, llm: Arc<FakeLlm>) -> ChatService {
    ChatService::new(c.vault.clone(), c.index.clone(), c.tasks.clone(), c.journal.clone(), llm, c.settings.clone(), None)
}

fn day() -> Option<NaiveDate> {
    NaiveDate::from_ymd_opt(2026, 10, 19)
}

fn ask(chat: &ChatService, question: &str) -> Value {
    chat.ask(&[ChatMessage::user(question)], day()).unwrap()
}

fn args(value: Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

#[test]
fn tools_read_vault() {
    let c = common::ctx();
    let chat = make_chat(&c, FakeLlm::new(Vec::<String>::new()));
    let hits = chat.search_notes("recette").unwrap();
    assert!(hits.as_array().unwrap().iter().any(|h| h["lien"] == "[[T-004]]"));
    let note = chat.read_note("[[2026-10-15-copil-sap]]").unwrap();
    assert!(note["type"] == "reunion" && note["contenu"].as_str().unwrap().contains("Décisions"));
    assert_eq!(chat.read_note("reunions/2026/2026-10-15-copil-sap.md").unwrap()["titre"], note["titre"]);
    let en_cours = chat.list_tasks(Some("en_cours"), None).unwrap();
    let ids: Vec<&str> = en_cours.as_array().unwrap().iter().map(|t| t["id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec!["T-001"]);
    assert!(!chat.get_task("T-001").unwrap()["history"].as_array().unwrap().is_empty());
    assert!(chat.list_notes(Some("reunion"), None, None).unwrap().as_array().unwrap().iter().all(|n| n["type"] == "reunion"));
    assert!(!chat.task_events("2026-10-13", "2026-10-15").unwrap().as_array().unwrap().is_empty());
}

#[test]
fn tool_errors_are_returned_to_the_model() {
    let c = common::ctx();
    let chat = make_chat(&c, FakeLlm::new(Vec::<String>::new()));
    let run = |name: &str, a: Value| chat.run_tool(Some(name), &args(a), &mut Vec::new()).unwrap();
    assert!(run("delete_note", json!({})).starts_with("Erreur : outil inconnu"));
    assert!(run("read_note", json!({"nom": "x"})).starts_with("Erreur : arguments invalides"));
    assert!(run("read_note", json!({"name": "inexistante"})).starts_with("Erreur : Aucune note"));
    assert_eq!(run("search_notes", json!({"query": "zzzzz"})), "Aucun résultat.");
}

#[test]
fn agent_loop_calls_tools_then_answers() {
    let c = common::ctx();
    let llm = FakeLlm::new([
        tool("search_notes", json!({"query": "COPIL"})),
        tool("read_note", json!({"name": "2026-10-15-copil-sap"})),
        answer("La recette est maintenue au 28 octobre [[2026-10-15-copil-sap]] ([[T-404]])."),
    ]);
    let chat = make_chat(&c, llm.clone());
    let result = ask(&chat, "Quelle est la date de recette ?");
    let tools: Vec<&str> = result["steps"].as_array().unwrap().iter().map(|s| s["tool"].as_str().unwrap()).collect();
    assert_eq!(tools, vec!["search_notes", "read_note"]);
    assert_eq!(result["links"], json!({"2026-10-15-copil-sap": "reunions/2026/2026-10-15-copil-sap.md"}));
    let calls = llm.calls();
    let system = &calls[0].messages[0].content;
    assert!(system.contains("2026-10-19 (lundi)") && system.contains("task_events"));
    assert!(calls.last().unwrap().messages.last().unwrap().content.starts_with("Résultat de read_note"));
    assert!(calls.iter().all(|c| c.json_mode));
}

#[test]
fn agent_loop_stops_after_max_steps() {
    let c = common::ctx();
    let mut responses = vec![tool("list_tasks", json!({})); MAX_STEPS];
    responses.push(answer("Voici les tâches."));
    let llm = FakeLlm::new(responses);
    let chat = make_chat(&c, llm.clone());
    let result = ask(&chat, "Mes tâches ?");
    assert_eq!(result["steps"].as_array().unwrap().len(), MAX_STEPS);
    assert_eq!(result["answer"], "Voici les tâches.");
    assert!(llm.calls().last().unwrap().messages.last().unwrap().content.contains("Nombre maximal"));
}

#[test]
fn proposals_are_validated_and_returned() {
    let c = common::ctx();
    let llm = FakeLlm::new([
        tool("propose_task", json!({"title": "Préparer la recette", "project": "sap", "due": "2026-10-27"})),
        tool("propose_task_update", json!({"task_id": "T-003", "status": "inconnu"})),
        tool("propose_task_update", json!({"task_id": "T-003", "status": "en_cours", "note": "accès obtenus"})),
        tool("propose_journal_line", json!({"line": "Point recette avec Marc"})),
        answer("Je propose une tâche, une mise à jour de [[T-003]] et une ligne de journal."),
    ]);
    let chat = make_chat(&c, llm);
    let files_before = c.vault.list_files();
    let result = ask(&chat, "Crée une tâche pour la recette et passe T-003 en cours");
    assert_eq!(c.vault.list_files(), files_before); // rien n'est écrit avant validation
    let kinds: Vec<&str> = result["proposals"].as_array().unwrap().iter().map(|p| p["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, vec!["create_task", "update_task", "journal"]);
    assert_eq!(result["proposals"][1]["title"], "Obtenir les accès production SAP");
    assert!(result["steps"][1]["summary"].as_str().unwrap().starts_with("Erreur : status invalide"));
}

#[test]
fn apply_proposals() {
    let c = common::ctx();
    let chat = make_chat(&c, FakeLlm::new(Vec::<String>::new()));
    let mut create = ChatProposal::new(ProposalKind::CreateTask);
    create.title = "Préparer la recette".into();
    create.project = Some("sap".into());
    create.due = Some("2026-10-27".into());
    let created = chat.apply(&create, day()).unwrap();
    let id = created["message"].as_str().unwrap().split_whitespace().nth(1).unwrap().to_string();
    let task = c.tasks.get(&id).unwrap().task;
    assert_eq!((task.project.as_deref(), task.due.as_deref()), (Some("sap"), Some("2026-10-27")));

    let mut update = ChatProposal::new(ProposalKind::UpdateTask);
    update.task_id = Some("T-003".into());
    update.status = Some("en_cours".into());
    update.note = "accès obtenus".into();
    chat.apply(&update, day()).unwrap();
    let t3 = c.tasks.get("T-003").unwrap();
    assert_eq!(t3.task.status, "en_cours");
    assert!(t3.content.contains("accès obtenus"));

    let mut journal = ChatProposal::new(ProposalKind::Journal);
    journal.line = "Point recette".into();
    let path = chat.apply(&journal, day()).unwrap()["path"].as_str().unwrap().to_string();
    assert_eq!(path, "journal/2026/2026-10-19.md");
    assert!(c.vault.read_text(&path).unwrap().contains("- Point recette"));
}

#[test]
fn stream_emits_steps_deltas_and_done() {
    let c = common::ctx();
    let llm = FakeLlm::chunked([tool("list_tasks", json!({"status": "bloquee"})), answer("La tâche [[T-003]] est bloquée.")], 7);
    let chat = make_chat(&c, llm);
    let mut events = Vec::new();
    chat.stream(&[ChatMessage::user("Bloquées ?")], None, &mut |e| events.push(e)).unwrap();
    assert_eq!(events[0], json!({"type": "step", "tool": "list_tasks", "args": {"status": "bloquee"}, "summary": null}));
    let deltas: Vec<&str> = events.iter().filter(|e| e["type"] == "delta").map(|e| e["text"].as_str().unwrap()).collect();
    assert!(deltas.len() > 1);
    assert_eq!(deltas.concat(), "La tâche [[T-003]] est bloquée.");
    let done = events.last().unwrap();
    assert_eq!(done["type"], "done");
    assert_eq!(done["links"], json!({"T-003": "taches/T-003-obtenir-acces-prod-sap.md"}));
}

#[test]
fn stream_recovers_from_invalid_json() {
    let c = common::ctx();
    let chat = make_chat(&c, FakeLlm::new(["pas du json".to_string(), answer("Rattrapé")]));
    assert_eq!(ask(&chat, "?")["answer"], "Rattrapé");
}

#[tokio::test]
async fn chat_api() {
    let app = TestApp::new(Some(FakeLlm::new([answer("Bonjour")])), None);
    let ok = app.post("/api/chat", Some(json!({"messages": [{"role": "user", "content": "Salut"}]}))).await;
    assert_eq!(ok.json()["answer"], "Bonjour");
    let bad = app.post("/api/chat", Some(json!({"messages": [{"role": "assistant", "content": "x"}]}))).await;
    assert_eq!(bad.status, axum::http::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn chat_stream_and_apply_api() {
    let llm = FakeLlm::new([tool("propose_journal_line", json!({"line": "Test"})), answer("C'est proposé.")]);
    let app = TestApp::new(Some(llm), None);
    let response = app.post("/api/chat/stream", Some(json!({"messages": [{"role": "user", "content": "x"}]}))).await;
    let events: Vec<Value> =
        response.text.lines().filter_map(|l| l.strip_prefix("data: ")).map(|l| serde_json::from_str(l).unwrap()).collect();
    let done = events.last().unwrap();
    assert_eq!(done["type"], "done");
    let applied = app.post("/api/chat/apply", Some(done["proposals"][0].clone())).await.json();
    assert!(applied["path"].as_str().unwrap().starts_with("journal/"));

    let unconfigured = TestApp::new(None, None); // API LLM non configurée
    let failing = unconfigured.post("/api/chat/stream", Some(json!({"messages": [{"role": "user", "content": "x"}]}))).await;
    let event: Value = serde_json::from_str(failing.text.trim().strip_prefix("data: ").unwrap()).unwrap();
    assert_eq!(event["type"], "error");
}
