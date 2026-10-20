//! Client HTTP compatible OpenAI, testé contre un petit serveur local (aucune vraie API).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;

use serde::Deserialize;
use serde_json::{json, Value};
use tempo_core::llm::{chat_json, Llm, Msg, OpenAiCompatibleLlm, ToolCall, ToolDef};

/// Serveur d'une seule requête : renvoie (corps reçu, chemin) et répond `response`.
fn serve_once(content_type: &'static str, response: String) -> (String, mpsc::Receiver<(String, Value)>) {
    serve(content_type, vec![response])
}

/// Serveur qui répond `responses` dans l'ordre, une par requête.
fn serve(content_type: &'static str, responses: Vec<String>) -> (String, mpsc::Receiver<(String, Value)>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for response in responses {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line.trim().is_empty() {
                    break;
                }
                if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let path = request_line.split_whitespace().nth(1).unwrap().to_string();
            let _ = tx.send((path, serde_json::from_slice(&body).unwrap()));
            let mut stream = stream;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                response.len()
            )
            .unwrap();
        }
    });
    (url, rx)
}

#[derive(Deserialize)]
struct Answer {
    ok: bool,
}

#[test]
fn json_mode_request_contains_json_word() {
    let completion = json!({"id": "x", "object": "chat.completion", "created": 0, "model": "m",
        "choices": [{"index": 0, "finish_reason": "stop", "message": {"role": "assistant", "content": "{\"ok\": true}"}}]});
    let (url, rx) = serve_once("application/json", completion.to_string());
    let llm = OpenAiCompatibleLlm::new(&url, "key", None).unwrap();
    let result: Answer = chat_json(&llm, &[Msg::system("Réponds en objet json"), Msg::user("Résumé")], "m").unwrap();
    assert!(result.ok);
    let (path, body) = rx.recv().unwrap();
    assert_eq!(path, "/v1/chat/completions");
    assert_eq!(body["response_format"], json!({"type": "json_object"}));
    assert!(body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["role"] == "user" && m["content"].as_str().unwrap().contains("json")));
}

#[test]
fn streaming_reads_server_sent_events() {
    let events = ["Bon", "jour", " !"]
        .iter()
        .map(|t| format!("data: {}\n\n", json!({"choices": [{"index": 0, "delta": {"content": t}}]})))
        .collect::<String>()
        + "data: [DONE]\n\n";
    let (url, rx) = serve_once("text/event-stream", events);
    let llm = OpenAiCompatibleLlm::new(&url, "key", None).unwrap();
    let mut chunks = Vec::new();
    llm.chat_stream(&[Msg::user("Salut")], "m", false, &mut |c| chunks.push(c.to_string())).unwrap();
    assert_eq!(chunks, vec!["Bon", "jour", " !"]);
    let (_, body) = rx.recv().unwrap();
    assert_eq!(body["stream"], true);
    assert!(body.get("response_format").is_none());
}

#[test]
fn errors_inside_the_stream_are_reported() {
    // Vibe signale par exemple un dépassement de quota dans le flux, avec un statut 200
    let events = format!(
        "data: {}\n\ndata: {}\n\n",
        json!({"choices": [{"index": 0, "delta": {"content": "Bon"}}]}),
        json!({"error": {"message": "exceeded token rate limit", "code": "429"}})
    );
    let (url, _rx) = serve_once("text/event-stream", events);
    let llm = OpenAiCompatibleLlm::new(&url, "key", None).unwrap();
    let error = llm.chat_tools(&[Msg::user("Salut")], "m", &[], true, &mut |_| {}).unwrap_err();
    assert!(error.to_string().contains("exceeded token rate limit"), "{error}");
}

#[test]
fn rate_limit_in_stream_is_retried_before_any_text() {
    let limited = format!(
        "data: {}\n\n",
        json!({"error": {"message": "exceeded token rate limit. Please retry after 1 second.", "code": "429"}})
    );
    let ok = format!("data: {}\n\ndata: [DONE]\n\n", json!({"choices": [{"index": 0, "delta": {"content": "Bonjour"}}]}));
    let (url, rx) = serve("text/event-stream", vec![limited, ok]);
    let llm = OpenAiCompatibleLlm::new(&url, "key", None).unwrap();
    let mut text = String::new();
    let turn = llm.chat_tools(&[Msg::user("Salut")], "m", &[], true, &mut |t| text.push_str(t)).unwrap();
    assert_eq!((text.as_str(), turn.text.as_str()), ("Bonjour", "Bonjour"));
    assert_eq!(rx.iter().take(2).count(), 2);
}

#[test]
fn tool_calls_are_rebuilt_from_stream() {
    // forme des événements renvoyés par Vibe : id et nom d'abord, arguments ensuite, par index
    let chunks = [
        json!({"delta": {"role": "assistant"}}),
        json!({"delta": {"content": "Je regarde."}}),
        json!({"delta": {"tool_calls": [{"index": 0, "id": "call_a", "type": "function", "function": {"name": "list_tasks", "arguments": ""}}]}}),
        json!({"delta": {"tool_calls": [{"index": 0, "function": {"arguments": "{\"project\":"}}]}}),
        json!({"delta": {"tool_calls": [{"index": 0, "function": {"arguments": "\"sap\"}"}}]}}),
        json!({"delta": {"tool_calls": [{"index": 1, "id": "call_b", "type": "function", "function": {"name": "get_task", "arguments": ""}}]}}),
        json!({"delta": {"tool_calls": [{"index": 1, "function": {"arguments": "{\"id\":\"T-003\"}"}}]}}),
        json!({"delta": {}, "finish_reason": "tool_calls"}),
    ];
    let mut events: String = chunks
        .iter()
        .map(|c| format!("data: {}\n\n", json!({"choices": [{"index": 0, "delta": c["delta"], "finish_reason": c["finish_reason"]}]})))
        .collect();
    events += &format!("data: {}\n\ndata: [DONE]\n\n", json!({"choices": [], "usage": {"total_tokens": 144}}));
    let (url, rx) = serve_once("text/event-stream", events);
    let llm = OpenAiCompatibleLlm::new(&url, "key", None).unwrap();
    let tools = [ToolDef { name: "list_tasks", description: "Tâches".into(), parameters: json!({"type": "object"}) }];
    let history = [Msg::user("Point ?"), Msg::assistant_tools("", vec![ToolCall::new("call_0", "get_task", "{}")]), Msg::tool("call_0", "ok")];
    let mut text = String::new();
    let turn = llm.chat_tools(&history, "m", &tools, true, &mut |t| text.push_str(t)).unwrap();
    assert_eq!((text.as_str(), turn.text.as_str()), ("Je regarde.", "Je regarde."));
    assert_eq!(turn.tool_calls, vec![
        ToolCall::new("call_a", "list_tasks", "{\"project\":\"sap\"}"),
        ToolCall::new("call_b", "get_task", "{\"id\":\"T-003\"}"),
    ]);
    let (_, body) = rx.recv().unwrap();
    assert_eq!(body["tools"][0]["function"]["name"], "list_tasks");
    assert_eq!(body["tool_choice"], "auto");
    // tour d'appels sans texte : contenu nul ; résultat relié à son appel
    assert_eq!(body["messages"][1]["content"], Value::Null);
    assert_eq!(body["messages"][1]["tool_calls"][0]["function"]["name"], "get_task");
    assert_eq!(body["messages"][2], json!({"role": "tool", "content": "ok", "tool_call_id": "call_0"}));
    assert!(body.get("response_format").is_none());
}
