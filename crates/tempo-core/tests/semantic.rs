mod common;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use common::{Ctx, TestApp};
use tempo_core::chat::ChatService;
use tempo_core::llm::FakeLlm;
use tempo_core::semantic::{hashing_embedder, Embedder, SemanticIndex};
use tempo_core::Error;

const ZEPPELIN: &str = "sujets/zeppelin.md";

/// note aux mots introuvables ailleurs, pour un classement déterministe
fn add_zeppelin(c: &Ctx) {
    c.vault
        .write(
            ZEPPELIN,
            "---\ntype: sujet\n---\n\n# Zeppelin\n\nDirigeable gonflé à l'hélium, nacelle et hangar d'amarrage.\n",
            None,
            false,
        )
        .unwrap();
    c.index.upsert(ZEPPELIN).unwrap();
}

fn make(c: &Ctx, embedder: Option<Embedder>, model: &str) -> Arc<SemanticIndex> {
    Arc::new(
        SemanticIndex::new(
            c.vault.clone(),
            c.index.clone(),
            Some(embedder.unwrap_or_else(|| hashing_embedder(256))),
            &c.vault.root.join(".tempo").join("vectors.db"),
            Some(model),
        )
        .unwrap(),
    )
}

#[test]
fn disabled_without_embedder() {
    let c = common::ctx();
    let semantic = SemanticIndex::new(c.vault.clone(), c.index.clone(), None, &c.vault.root.join(".tempo/vectors.db"), None).unwrap();
    assert!(!semantic.enabled());
    assert_eq!(semantic.sync(), 0);
    assert!(semantic.search("x", 10).unwrap().is_empty() && semantic.related("x", 8).unwrap().is_empty());
    assert_eq!(semantic.status().unwrap()["enabled"], false);
}

#[test]
fn sync_is_incremental() {
    let c = common::ctx();
    let calls = Arc::new(Mutex::new(0usize));
    let embed = hashing_embedder(256);
    let counter = calls.clone();
    let counting: Embedder = Arc::new(move |texts: &[String]| {
        *counter.lock().unwrap() += texts.len();
        embed(texts)
    });
    let semantic = make(&c, Some(counting), "test");
    let total = semantic.sync();
    let status = semantic.status().unwrap();
    assert!(total > 5 && status["total"] == total && status["indexed"] == total);
    assert_eq!(semantic.sync(), 0); // rien n'a changé
    let path = "sujets/mapping-unites-de-mesure.md";
    let text = c.vault.read_text(path).unwrap();
    c.vault.write(path, &format!("{text}\nNouvelle conversion des palettes.\n"), None, false).unwrap();
    c.index.upsert(path).unwrap();
    assert_eq!(semantic.sync(), 1);
    c.vault.delete(path).unwrap();
    c.index.remove(path).unwrap();
    semantic.sync();
    assert!(semantic.search("unités de mesure palettes", 10).unwrap().iter().all(|r| r.path != path));
    assert!(*calls.lock().unwrap() > 0);
}

#[test]
fn search_and_related() {
    let c = common::ctx();
    add_zeppelin(&c);
    let semantic = make(&c, None, "test");
    semantic.sync();
    let results = semantic.search("hangar du dirigeable à hélium", 10).unwrap();
    assert_eq!(results[0].path, ZEPPELIN);
    assert!(results[0].score > 0.0 && results[0].score <= 1.0 && results[0].snippet.starts_with("Dirigeable"));
    assert!(results.windows(2).all(|w| w[0].score >= w[1].score));
    let task = "taches/T-003-obtenir-acces-prod-sap.md";
    let related: HashSet<String> = semantic.related(task, 8).unwrap().into_iter().map(|r| r.path).collect();
    assert!(!related.contains(task));
    let linked: HashSet<String> = c.index.backlinks(task).unwrap().into_iter().map(|n| n.path).collect();
    assert!(related.is_disjoint(&linked)); // déjà liées : pas suggérées
}

#[test]
fn model_change_resets_vectors() {
    let c = common::ctx();
    make(&c, None, "a").sync();
    let semantic = make(&c, Some(hashing_embedder(64)), "b");
    assert_eq!(semantic.status().unwrap()["indexed"], 0);
    assert!(semantic.sync() > 0);
    assert!(!semantic.search("recette", 10).unwrap().is_empty());
}

#[test]
fn embedding_errors_are_reported() {
    let c = common::ctx();
    let failing: Embedder = Arc::new(|_: &[String]| Err(Error::Llm("API indisponible".into())));
    let semantic = make(&c, Some(failing), "test");
    assert_eq!(semantic.sync(), 0);
    assert_eq!(semantic.status().unwrap()["error"], "API indisponible");
}

#[test]
fn chat_semantic_tool() {
    let c = common::ctx();
    add_zeppelin(&c);
    let semantic = make(&c, None, "test");
    semantic.sync();
    let llm = FakeLlm::new(Vec::<String>::new());
    let chat = ChatService::new(common::chat_deps(&c, llm.clone()), llm, c.settings.clone(), Some(semantic));
    assert!(chat.tool_names().contains(&"semantic_search"));
    let hits = chat.semantic_search("nacelle de dirigeable").unwrap();
    assert_eq!(hits[0]["lien"], "[[zeppelin]]");
    assert!(hits[0].get("proximite").is_some());
    let llm = FakeLlm::new(Vec::<String>::new());
    let without = ChatService::new(common::chat_deps(&c, llm.clone()), llm, c.settings.clone(), None);
    assert!(!without.tool_names().contains(&"semantic_search"));
}

#[tokio::test]
async fn semantic_api() {
    let app = TestApp::new(Some(FakeLlm::new(Vec::<String>::new())), Some(hashing_embedder(256)));
    assert_eq!(app.get("/api/config").await.json()["semantic_enabled"], true);
    let synced = app.post("/api/semantic/sync", None).await.json();
    assert!(synced["synced"] == synced["total"] && synced["total"] == synced["indexed"]);
    let hits = app.get(&format!("/api/semantic/search?q={}", common::q("procédure de recette"))).await.json();
    let top: Vec<&str> = hits.as_array().unwrap().iter().take(3).map(|h| h["path"].as_str().unwrap()).collect();
    assert!(top.contains(&"taches/T-004-rediger-procedure-recette.md"), "{top:?}");
    assert!(app.get("/api/semantic/related?path=projets/sap.md").await.json().is_array());

    let disabled = TestApp::new(Some(FakeLlm::new(Vec::<String>::new())), None);
    assert_eq!(disabled.get("/api/config").await.json()["semantic_enabled"], false);
}
