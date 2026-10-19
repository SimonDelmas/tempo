//! Client HTTP compatible OpenAI, testé contre un petit serveur local (aucune vraie API).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;

use serde::Deserialize;
use serde_json::{json, Value};
use tempo_core::llm::{chat_json, Llm, Msg, OpenAiCompatibleLlm};

/// Serveur d'une seule requête : renvoie (corps reçu, chemin) et répond `response`.
fn serve_once(content_type: &'static str, response: String) -> (String, mpsc::Receiver<(String, Value)>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
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
        tx.send((path, serde_json::from_slice(&body).unwrap())).unwrap();
        let mut stream = stream;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
            response.len()
        )
        .unwrap();
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
