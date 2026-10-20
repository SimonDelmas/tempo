mod common;

use std::sync::Arc;

use chrono::NaiveDate;
use common::{Ctx, TestApp};
use serde_json::{json, Map, Value};
use tempo_core::chat::{ChatMessage, ChatProposal, ChatService, MAX_STEPS};
use tempo_core::error::Result;
use tempo_core::llm::{fake_tool_calls, FakeLlm, Llm, Msg, ToolCall, ToolDef, ToolTurn};

fn tool(name: &str, args: Value) -> String {
    fake_tool_calls(&[(name, args)])
}

fn answer(text: &str) -> String {
    text.to_string()
}

fn make_chat(c: &Ctx, llm: Arc<FakeLlm>) -> ChatService {
    ChatService::new(common::chat_deps(c, llm.clone()), llm, c.settings.clone(), None)
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
    let hits = chat.search_notes("recette", None).unwrap();
    assert!(hits.as_array().unwrap().iter().any(|h| h["lien"] == "[[T-004]]"));
    let note = chat.read_note("[[2026-10-15-copil-sap]]").unwrap();
    assert!(note["type"] == "reunion" && note["contenu"].as_str().unwrap().contains("Décisions"));
    assert!(note["liens"].as_array().unwrap().iter().any(|n| n["lien"] == "[[T-001]]"));
    assert_eq!(chat.read_note("reunions/2026/2026-10-15-copil-sap.md").unwrap()["titre"], note["titre"]);
    let en_cours = chat.list_tasks(Some("en_cours"), None).unwrap();
    let ids: Vec<&str> = en_cours.as_array().unwrap().iter().map(|t| t["id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec!["T-001"]);
    assert!(!chat.get_task("T-001").unwrap()["history"].as_array().unwrap().is_empty());
    let reunions = chat.list_notes(Some("reunion"), None, None).unwrap();
    assert!(reunions.as_array().unwrap().iter().all(|n| n["type"] == "reunion"));
    // le modèle de réunion (type: reunion) n'est pas une réunion
    assert!(reunions.as_array().unwrap().iter().all(|n| !n["chemin"].as_str().unwrap().starts_with("_templates/")));
    let hits = chat.search_notes("Participants Décisions Actions", None).unwrap();
    assert!(hits.as_array().unwrap().iter().all(|n| !n["chemin"].as_str().unwrap().starts_with("_templates/")));

    // recherche exacte, par regex, par nom et par frontmatter
    let grep = chat.grep_notes("recette maintenue au 28", false, None, None).unwrap();
    assert_eq!(grep["notes"][0]["lien"], "[[2026-10-15-copil-sap]]");
    assert_eq!(grep["notes"][0]["lignes"][0]["texte"], "- Recette maintenue au 28 octobre.");
    let grep = chat.grep_notes(r"\[\[T-00[34]\]\]", true, Some("reunion"), None).unwrap();
    assert_eq!(grep["notes"].as_array().unwrap().len(), 1);
    let found = chat.find_notes(None, Some("reunion"), json!({"participants": "Julie Bernard"}).as_object().unwrap()).unwrap();
    assert_eq!(found[0]["lien"], "[[2026-10-15-copil-sap]]");
    assert!(!chat.search_notes("procédure de recette", Some("phrase")).unwrap().as_array().unwrap().is_empty());
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
    assert!(run("search_notes", json!({"query": "x", "mode": "regex"})).starts_with("Erreur : mode invalide"));
    assert!(run("grep_notes", json!({"pattern": "(", "regex": true})).starts_with("Erreur : Expression régulière invalide"));
    // texte cherché tel quel : les caractères spéciaux ne sont pas interprétés
    assert!(run("grep_notes", json!({"pattern": "([[T-004]])"})).contains("procédure de recette"));
    assert!(run("find_notes", json!({"meta": {"projects": "sap"}})).contains("copil-sap"));
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
    let create: ChatProposal = serde_json::from_value(
        json!({"kind": "create_task", "title": "Préparer la recette", "project": "sap", "due": "2026-10-27"}),
    )
    .unwrap();
    let created = chat.apply(&create, day()).unwrap();
    let id = created["message"].as_str().unwrap().split_whitespace().nth(1).unwrap().to_string();
    let task = c.tasks.get(&id).unwrap().task;
    assert_eq!((task.project.as_deref(), task.due.as_deref()), (Some("sap"), Some("2026-10-27")));

    let update: ChatProposal = serde_json::from_value(json!({
        "kind": "update_task", "task_id": "T-003", "status": "en_cours", "note": "accès obtenus",
        "new_title": "Obtenir les accès production", "iteration": "2026-S44",
    }))
    .unwrap();
    chat.apply(&update, day()).unwrap();
    let t3 = c.tasks.get("T-003").unwrap();
    assert_eq!((t3.task.status.as_str(), t3.task.title.as_str()), ("en_cours", "Obtenir les accès production"));
    assert!(t3.content.contains("accès obtenus") && t3.content.contains("2026-S44"));

    let journal = ChatProposal::Journal { line: "Point recette".into() };
    let path = chat.apply(&journal, day()).unwrap()["path"].as_str().unwrap().to_string();
    assert_eq!(path, "journal/2026/2026-10-19.md");
    assert!(c.vault.read_text(&path).unwrap().contains("- Point recette"));
}

#[test]
fn proposals_stored_by_the_previous_version_still_apply() {
    // ancien format à plat, conservé dans les conversations du navigateur
    let old = json!({"kind": "journal", "title": "", "description": "", "project": null, "priority": null,
                     "status": null, "due": null, "task_id": null, "note": "", "line": "Ancienne proposition"});
    assert_eq!(serde_json::from_value::<ChatProposal>(old).unwrap(), ChatProposal::Journal { line: "Ancienne proposition".into() });
}

const COPIL: &str = "reunions/2026/2026-10-15-copil-sap.md";

/// Propositions de l'agent pour une suite d'appels d'outils (un par tour).
fn propose(c: &Ctx, calls: Vec<(&str, Value)>) -> Value {
    let mut responses: Vec<String> = calls.into_iter().map(|(name, a)| tool(name, a)).collect();
    responses.push(answer("Voilà."));
    let chat = make_chat(c, FakeLlm::new(responses));
    let files = c.vault.list_files();
    let copil = c.vault.read_text(COPIL).unwrap();
    let result = ask(&chat, "?");
    assert_eq!((c.vault.list_files(), c.vault.read_text(COPIL).unwrap()), (files, copil)); // rien n'est écrit
    result
}

#[test]
fn note_proposals_are_checked_then_applied() {
    let c = common::ctx();
    let result = propose(&c, vec![
        ("propose_note_edit", json!({"name": "[[2026-10-15-copil-sap]]", "edits": [
            {"find": "Recette maintenue au 28 octobre.", "replace": "Recette décalée au 4 novembre."}]})),
        ("propose_note_append", json!({"name": COPIL, "section": "Décisions", "text": "- Go pour la reprise."})),
        ("propose_note_meta", json!({"name": "2026-10-15-copil-sap", "set": {"participants": ["Claire Martin"], "statut": "valide"}})),
        ("propose_link", json!({"name": "2026-10-15-copil-sap", "target": "mapping-unites-de-mesure"})),
        ("propose_link", json!({"name": "2026-10-15-copil-sap", "target": "T-003", "field": "tasks"})),
        ("propose_unlink", json!({"name": "2026-10-15-copil-sap", "target": "T-004"})),
    ]);
    let steps = result["steps"].as_array().unwrap();
    assert!(steps.iter().all(|s| s["summary"].is_null()), "{steps:?}");
    let proposals = result["proposals"].as_array().unwrap();
    let kinds: Vec<&str> = proposals.iter().map(|p| p["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, vec!["edit_note", "append_note", "update_meta", "link", "link", "unlink"]);
    assert!(proposals.iter().all(|p| p["path"] == COPIL));
    assert_eq!(proposals[1]["section"], "## Décisions");
    assert_eq!(proposals[2]["before"], json!({"participants": ["Claire Martin", "Marc Leroy", "Julie Bernard"], "statut": null}));
    assert_eq!(proposals[3]["link"], "[[mapping-unites-de-mesure]]");

    // validées l'une après l'autre sur la même note : chacune est rejouée sur le contenu du moment
    let chat = make_chat(&c, FakeLlm::new(Vec::<String>::new()));
    for proposal in proposals {
        let proposal: ChatProposal = serde_json::from_value(proposal.clone()).unwrap();
        assert_eq!(chat.apply(&proposal, day()).unwrap()["path"], COPIL);
    }
    let text = c.vault.read_text(COPIL).unwrap();
    assert!(text.contains("participants: [Claire Martin]\ntasks: ['[[T-001]]', '[[T-002]]', '[[T-003]]']\nstatut: valide\n---"));
    assert!(text.contains("- Recette décalée au 4 novembre.\n"));
    assert!(text.contains("- Go pour la reprise.\n\n## Actions"));
    assert!(text.contains("procédure de recette (T-004)") && !text.contains("[[T-004]]"));
    assert!(text.trim_end().ends_with("## Liens\n- [[mapping-unites-de-mesure]]"));
    let backlinks = c.index.backlinks("sujets/mapping-unites-de-mesure.md").unwrap();
    assert!(backlinks.iter().any(|n| n.path == COPIL)); // index à jour
}

#[test]
fn note_proposals_errors_are_returned_to_the_model() {
    let c = common::ctx();
    let result = propose(&c, vec![
        ("propose_note_edit", json!({"name": COPIL, "edits": [{"find": "texte absent", "replace": "x"}]})),
        ("propose_note_edit", json!({"name": COPIL, "edits": [{"find": "## ", "replace": "x"}]})),
        ("propose_note_meta", json!({"name": "T-003", "set": {"status": "terminee"}})),
        ("propose_link", json!({"name": COPIL, "target": "T-001", "field": "tasks"})),
        ("propose_link", json!({"name": COPIL, "target": "note-inexistante"})),
        ("propose_unlink", json!({"name": COPIL, "target": "mapping-unites-de-mesure"})),
        ("propose_note_create", json!({"type": "journal", "title": "x"})),
    ]);
    let summaries: Vec<&str> = result["steps"].as_array().unwrap().iter().map(|s| s["summary"].as_str().unwrap()).collect();
    assert!(summaries[0].contains("introuvable"));
    assert!(summaries[1].contains("cite un passage plus long"));
    assert!(summaries[2].contains("propose_task_update"));
    assert!(summaries[3].contains("contient déjà ce lien"));
    assert!(summaries[4].contains("Aucune note"));
    assert!(summaries[5].contains("aucun lien"));
    assert!(summaries[6].contains("type invalide"));
    assert!(result["proposals"].as_array().unwrap().is_empty());
}

#[test]
fn create_and_delete_notes() {
    let c = common::ctx();
    let result = propose(&c, vec![
        ("propose_note_create", json!({"type": "personne", "title": "Marc Leroy", "content": "Fournit les jeux de test ([[2026-10-15-copil-sap]])."})),
        ("propose_note_delete", json!({"name": "mapping-unites-de-mesure"})),
    ]);
    let proposals = result["proposals"].as_array().unwrap();
    assert_eq!(proposals[1]["backlinks"], json!([]));
    let chat = make_chat(&c, FakeLlm::new(Vec::<String>::new()));
    let create: ChatProposal = serde_json::from_value(proposals[0].clone()).unwrap();
    let path = chat.apply(&create, day()).unwrap()["path"].as_str().unwrap().to_string();
    assert_eq!(path, "personnes/Marc Leroy.md");
    let text = c.vault.read_text(&path).unwrap();
    assert!(text.contains("type: personne") && text.contains("# Marc Leroy") && text.contains("Fournit les jeux de test"));
    assert!(c.index.backlinks(COPIL).unwrap().iter().any(|n| n.path == path));

    let delete: ChatProposal = serde_json::from_value(proposals[1].clone()).unwrap();
    chat.apply(&delete, day()).unwrap();
    assert!(!c.vault.exists("sujets/mapping-unites-de-mesure.md"));
    assert!(c.index.get("sujets/mapping-unites-de-mesure.md").unwrap().is_none());
}

#[test]
fn delete_lists_backlinks() {
    let c = common::ctx();
    let result = propose(&c, vec![("propose_note_delete", json!({"name": "T-004"}))]);
    let backlinks = result["proposals"][0]["backlinks"].as_array().unwrap();
    assert!(backlinks.contains(&json!("[[2026-10-15-copil-sap]]")));
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
    let llm: Arc<dyn Llm> = Arc::new(Preamble);
    let chat = ChatService::new(common::chat_deps(&c, llm.clone()), llm, c.settings.clone(), None);
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
