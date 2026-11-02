mod common;

use common::{q, TestApp};
use serde_json::Value;
use tempo_core::semantic::hashing_embedder;

const SPEC: &str = "# Spécification\n\nLe flux zeppelinage transite par la passerelle.\n";

fn write_note(app: &TestApp, path: &str, text: &str) {
    app.services.vault.write(path, text, None, false).unwrap();
    app.services.index.upsert(path).unwrap();
}

async fn search(app: &TestApp, query: &str) -> Vec<Value> {
    app.get(&format!("/api/search/global?q={}", q(query))).await.json().as_array().unwrap().clone()
}

fn targets<'a>(hits: &'a [Value], group: &str) -> Vec<&'a str> {
    hits.iter().filter(|h| h["group"] == group).map(|h| h["target"].as_str().unwrap()).collect()
}

#[tokio::test]
async fn covers_notes_tasks_documents_and_drafts() {
    let app = TestApp::new(None, None);
    write_note(&app, "sujets/dirigeable.md", "---\ntype: sujet\n---\n\n# Dirigeable\n\nLe zeppelinage reprend.\n");
    write_note(&app, "taches/T-900-vol.md", "---\ntype: tache\nid: T-900\nstatus: a_faire\ntitle: Vol\n---\n\nPréparer le zeppelinage.\n");
    app.upload("Spec.md", SPEC.as_bytes()).await;
    let draft = app.services.drafts.create("Idées").unwrap();
    app.services.drafts.save(&draft.id, None, Some("Penser au Zéppelinage de mardi")).unwrap();

    let hits = search(&app, "zeppelinage").await;
    assert_eq!(targets(&hits, "note"), ["sujets/dirigeable.md"]);
    assert_eq!(targets(&hits, "tache"), ["taches/T-900-vol.md"]);
    assert_eq!(targets(&hits, "document"), ["Spec"]);
    assert_eq!(targets(&hits, "brouillon"), [draft.id.as_str()]);

    // extrait du brouillon : le terme d'origine, accent compris, entre « »
    let found = hits.iter().find(|h| h["group"] == "brouillon").unwrap();
    assert_eq!(found["title"], "Idées");
    assert!(found["snippet"].as_str().unwrap().contains("«Zéppelinage»"), "{found}");
}

#[tokio::test]
async fn excludes_templates_and_inbox() {
    let app = TestApp::new(None, None);
    write_note(&app, "_templates/gabarit.md", "# Gabarit\n\nmotintrouvable\n");
    write_note(&app, "00-inbox/mail.md", "# Mail\n\nmotintrouvable\n");
    write_note(&app, "sujets/garde.md", "# Garde\n\nmotintrouvable\n");
    assert_eq!(targets(&search(&app, "motintrouvable").await, "note"), ["sujets/garde.md"]);
}

#[tokio::test]
async fn titles_come_first_and_groups_are_limited() {
    let app = TestApp::new(None, None);
    for i in 0..10 {
        write_note(&app, &format!("sujets/corps-{i}.md"), &format!("# Corps {i}\n\nvolumetrie volumetrie volumetrie\n"));
    }
    write_note(&app, "sujets/titre.md", "# Volumetrie\n\nUne fois un long texte sans autre rapport avec le sujet de la recherche.\n");
    let hits = search(&app, "volumetrie").await;
    let notes = targets(&hits, "note");
    assert_eq!(notes[0], "sujets/titre.md");
    assert_eq!(notes.len(), 8);
}

#[tokio::test]
async fn empty_query_finds_nothing() {
    let app = TestApp::new(None, None);
    assert!(search(&app, "  ").await.is_empty());
    assert!(search(&app, "?!").await.is_empty());
}

#[tokio::test]
async fn meaning_finds_close_notes() {
    let app = TestApp::new(None, Some(hashing_embedder(256)));
    write_note(&app, "sujets/zeppelin.md", "# Zeppelin\n\nDirigeable gonflé à l'hélium, nacelle et hangar d'amarrage.\n");
    app.services.semantic.sync();
    let reply = app.get(&format!("/api/search/global/meaning?q={}", q("dirigeable hélium nacelle"))).await.json();
    let hits = reply.as_array().unwrap();
    assert_eq!(hits[0]["target"], "sujets/zeppelin.md");
    assert_eq!(hits[0]["group"], "note");
}

#[tokio::test]
async fn meaning_without_model_finds_nothing() {
    let app = TestApp::new(None, None);
    assert!(app.get("/api/search/global/meaning?q=dirigeable").await.json().as_array().unwrap().is_empty());
}
