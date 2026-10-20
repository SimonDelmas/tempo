mod common;

use std::sync::Arc;

use chrono::NaiveDate;
use common::{Ctx, TestApp};
use serde_json::{json, Map, Value};
use tempo_core::chat::{ChatMessage, ChatProposal, ChatService, ProposalKind, MAX_STEPS};
use tempo_core::error::Result;
use tempo_core::llm::{fake_tool_calls, FakeLlm, Llm, Msg, ToolCall, ToolDef, ToolTurn};

fn tool(name: &str, args: Value) -> String {
    fake_tool_calls(&[(name, args)])
}

fn answer(text: &str) -> String {
    text.to_string()
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
    assert!(calls[0].messages[0].content.contains("2026-10-19 (lundi)"));
    assert!(calls.iter().all(|c| c.tools.contains(&"task_events") && c.tools.contains(&"propose_task") && c.allow_calls));
    // l'appel du modèle puis le résultat de l'outil, reliés par l'identifiant de l'appel
    let messages = &calls.last().unwrap().messages;
    let (call, result) = (&messages[messages.len() - 2], messages.last().unwrap());
    assert_eq!((call.role.as_str(), call.tool_calls[0].function.name.as_str()), ("assistant", "read_note"));
    assert_eq!((result.role.as_str(), result.tool_call_id.as_ref()), ("tool", Some(&call.tool_calls[0].id)));
    assert!(result.content.contains("Décisions"));
}

#[test]
fn agent_loop_runs_parallel_tool_calls() {
    let c = common::ctx();
    let llm = FakeLlm::new([
        fake_tool_calls(&[("list_tasks", json!({"project": "sap"})), ("get_task", json!({"id": "T-003"}))]),
        answer("Voici le point."),
    ]);
    let chat = make_chat(&c, llm.clone());
    let result = ask(&chat, "Point sur sap et T-003 ?");
    let tools: Vec<&str> = result["steps"].as_array().unwrap().iter().map(|s| s["tool"].as_str().unwrap()).collect();
    assert_eq!(tools, vec!["list_tasks", "get_task"]);
    let messages = &llm.calls()[1].messages;
    let results: Vec<_> = messages.iter().filter(|m| m.role == "tool").map(|m| m.tool_call_id.clone().unwrap()).collect();
    assert_eq!(results, vec!["call_0", "call_1"]);
}

#[test]
fn invalid_arguments_are_returned_to_the_model() {
    let c = common::ctx();
    let llm = FakeLlm::new([tool("read_note", json!("{\"name\": ")), answer("Rattrapé")]);
    let chat = make_chat(&c, llm.clone());
    let result = ask(&chat, "?");
    assert_eq!(result["answer"], "Rattrapé");
    assert!(result["steps"][0]["summary"].as_str().unwrap().starts_with("Erreur : arguments invalides pour read_note"));
    assert_eq!(result["steps"][0]["args"], "{\"name\": ");
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
    let last = llm.calls().last().unwrap().clone();
    assert!(last.messages.last().unwrap().content.contains("Nombre maximal"));
    assert!(!last.allow_calls); // tool_choice: none au dernier tour
}

#[test]
fn proposals_are_validated_and_returned() {
    let c = common::ctx();
    let llm = FakeLlm::new([
        tool("propose_task", json!({"title": "Préparer la recette", "project": "sap", "due": "2026-10-27"})),
        tool("propose_task_update", json!({"task_id": "T-003", "status": "inconnu"})),
        tool("propose_task_update", json!({"task_id": "T-003", "status": "en_cours", "priority": "haute", "note": "accès obtenus"})),
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
    // priorité déjà « haute » : pas une modification
    assert_eq!((&result["proposals"][1]["status"], &result["proposals"][1]["priority"]), (&json!("en_cours"), &Value::Null));
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
fn stream_resets_text_before_tool_calls() {
    // texte puis appels d'outils dans le même tour : le texte diffusé est effacé
    struct Preamble;
    impl Llm for Preamble {
        fn chat(&self, _: &[Msg], _: &str, _: bool) -> Result<String> {
            unreachable!()
        }
        fn chat_tools(
            &self,
            messages: &[Msg],
            _: &str,
            _: &[ToolDef],
            _: bool,
            on_text: &mut dyn FnMut(&str),
        ) -> Result<ToolTurn> {
            let first = !messages.iter().any(|m| m.role == "tool");
            let text = if first { "Je regarde…" } else { "Fini." };
            on_text(text);
            let tool_calls =
                if first { vec![ToolCall::new("c1", "list_tasks", "{}")] } else { Vec::new() };
            Ok(ToolTurn { text: text.into(), tool_calls })
        }
    }
    let c = common::ctx();
    let chat = ChatService::new(c.vault.clone(), c.index.clone(), c.tasks.clone(), c.journal.clone(), Arc::new(Preamble), c.settings.clone(), None);
    let mut events = Vec::new();
    chat.stream(&[ChatMessage::user("?")], None, &mut |e| events.push(e)).unwrap();
    let types: Vec<&str> = events.iter().map(|e| e["type"].as_str().unwrap()).collect();
    assert_eq!(types, vec!["delta", "reset", "step", "delta", "done"]);
    assert_eq!(events.last().unwrap()["answer"], "Fini.");
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
