mod common;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use common::{q, TestApp};
use serde_json::{json, Value};
use tempo_core::llm::{FakeLlm, Llm};

const SPEC: &str = "---\ntitle: Interface articles\n---\n\n# Interface articles\n\nFlux des articles entre le PIM et SAP.\n\n\
                    ## Mapping\n\nLes unités PCE deviennent PC.\n";

fn app(responses: Vec<Value>) -> (TestApp, Arc<FakeLlm>) {
    let llm = FakeLlm::new(responses.iter().map(|r| r.to_string()).collect::<Vec<_>>());
    (TestApp::new(Some(llm.clone() as Arc<dyn Llm>), None), llm)
}

fn library_dir(app: &TestApp) -> std::path::PathBuf {
    app.services.library.root.clone()
}

#[tokio::test]
async fn depot_classement_propose_puis_valide() {
    let proposal = json!({
        "title": "Spécification de l'interface articles",
        "summary": "Décrit le flux des articles du PIM vers SAP.",
        "themes": ["interfaces", "Données de référence", "inconnu"],
        "new_themes": ["Interfaces"],
        "projects": ["sap", "Migration SAP", "crm"],
        "tags": ["Spécification", "#SAP", "flux articles", "sap"]
    });
    let (app, llm) = app(vec![proposal]);
    assert_eq!(app.post("/api/themes", Some(json!({"title": "Interfaces"}))).await.json()["id"], "interfaces");
    let duplicate = app.post("/api/themes", Some(json!({"title": "interfaces"}))).await;
    assert_eq!(duplicate.status, StatusCode::BAD_REQUEST);

    let reply = app.upload("Interface articles.md", SPEC.as_bytes()).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    let document = &reply.json()["document"];
    assert_eq!(document["id"], "Interface articles");
    assert_eq!(document["title"], "Interface articles");
    assert_eq!(document["pending"], true);
    assert_eq!(document["source"], "sources/Interface articles.md");
    // le pivot est rangé dans le dossier, la source conservée à côté
    let root = library_dir(&app);
    let pivot = std::fs::read_to_string(root.join("Interface articles.md")).unwrap();
    assert!(pivot.starts_with("---\ntype: document\ntitle: Interface articles\nthemes: []\n"), "{pivot}");
    assert!(pivot.contains("status: a_classer\n---\n\n# Interface articles\n"), "{pivot}");
    assert!(root.join("sources").join("Interface articles.md").is_file());

    let queue = app.get("/api/library/queue").await.json();
    assert_eq!(queue["pending"].as_array().unwrap().len(), 1);
    assert_eq!(app.get("/api/state").await.json()["library"], 1);

    // proposition : thèmes et projets limités à ceux du vault, tags normalisés
    let classification = app.post(&format!("/api/library/docs/{}/classify", q("Interface articles")), None).await.json();
    assert_eq!(classification["themes"], json!(["interfaces"]));
    assert_eq!(classification["new_themes"], json!(["Données de référence", "inconnu"]));
    assert_eq!(classification["projects"], json!(["sap"]));
    assert_eq!(classification["tags"], json!(["spécification", "sap", "flux-articles"]));
    let prompt = &llm.calls()[0].messages[1].content;
    assert!(prompt.contains("- interfaces : Interfaces") && prompt.contains("- sap : Migration SAP"), "{prompt}");
    assert!(prompt.contains("Les unités PCE deviennent PC.") && prompt.contains("## Mapping"), "{prompt}");
    // rien n'est écrit avant validation
    assert_eq!(std::fs::read_to_string(root.join("Interface articles.md")).unwrap(), pivot);

    let unknown = app
        .call(Method::PUT, &format!("/api/library/docs/{}", q("Interface articles")),
              Some(json!({"title": "Spec", "themes": ["inconnu"]})))
        .await;
    assert_eq!(unknown.status, StatusCode::BAD_REQUEST);
    let validated = app
        .call(
            Method::PUT,
            &format!("/api/library/docs/{}", q("Interface articles")),
            Some(json!({"title": classification["title"], "summary": classification["summary"],
                        "themes": ["interfaces"], "projects": ["sap"], "tags": ["SAP", "Flux articles"]})),
        )
        .await;
    assert_eq!(validated.status, StatusCode::OK, "{}", validated.text);
    assert_eq!(validated.json()["pending"], false);
    let pivot = std::fs::read_to_string(root.join("Interface articles.md")).unwrap();
    assert!(pivot.contains("title: Spécification de l'interface articles\nthemes: [interfaces]\nprojects: [sap]\ntags: [sap, flux-articles]\n"), "{pivot}");
    assert!(!pivot.contains("status:"), "{pivot}");
    assert_eq!(app.get("/api/state").await.json()["library"], 0);

    // vue par thème, par projet, recherche par mot-clé, titre ou tag
    let by_theme = app.get("/api/library?theme=interfaces").await.json();
    assert_eq!(by_theme.as_array().unwrap().len(), 1);
    assert_eq!(app.get("/api/library?theme=-").await.json().as_array().unwrap().len(), 0);
    assert_eq!(app.get("/api/library?project=sap").await.json().as_array().unwrap().len(), 1);
    let found = app.get(&format!("/api/library?q={}", q("unites PC"))).await.json();
    assert_eq!(found[0]["id"], "Interface articles");
    assert_eq!(found[0]["section"], "Interface articles › Mapping");
    assert!(found[0]["snippet"].as_str().unwrap().contains("«unités»"), "{found}");
    assert_eq!(app.get(&format!("/api/library?q={}", q("spécification"))).await.json().as_array().unwrap().len(), 1);
    assert_eq!(app.get("/api/library?q=flux-articles").await.json().as_array().unwrap().len(), 1);
    assert_eq!(app.get("/api/library?tag=SAP").await.json().as_array().unwrap().len(), 1);
    assert_eq!(app.get("/api/library?q=introuvable").await.json().as_array().unwrap().len(), 0);

    // tags partagés avec les notes (vault-exemple : sujets/mapping-unites-de-mesure.md)
    let tags = app.get("/api/tags").await.json();
    let sap = tags.as_array().unwrap().iter().find(|t| t["tag"] == "sap").unwrap();
    assert_eq!(sap["count"], 2);
}

#[tokio::test]
async fn bibliotheque_hors_des_notes() {
    let app = TestApp::new(None, None);
    app.upload("Interface articles.md", SPEC.as_bytes()).await;
    let files = app.get("/api/files").await.json();
    assert!(files.as_array().unwrap().iter().all(|f| !f["path"].as_str().unwrap().starts_with("bibliotheque")));
    let search = app.get(&format!("/api/search?q={}", q("unités PCE"))).await.json();
    assert!(search.as_array().unwrap().iter().all(|h| !h["path"].as_str().unwrap().starts_with("bibliotheque")), "{search}");
    // un lien [[…]] d'une note peut désigner un document
    let resolved = app.get(&format!("/api/resolve?name={}", q("interface articles"))).await.json();
    assert_eq!(resolved, json!({"path": null, "document": "Interface articles"}));
    // sans API configurée, pas de proposition de classement
    let reply = app.post(&format!("/api/library/docs/{}/classify", q("Interface articles")), None).await;
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE, "{}", reply.text);
}

#[tokio::test]
async fn doublon_refuse_et_nouvelle_version() {
    let (app, _) = app(vec![]);
    let root = library_dir(&app);
    app.upload("Planning.txt", "Version 1 du planning".as_bytes()).await;
    app.call(Method::PUT, "/api/library/docs/Planning", Some(json!({"title": "Planning", "tags": ["planning"]}))).await;

    let again = app.upload("Copie du planning.txt", "Version 1 du planning".as_bytes()).await;
    assert_eq!(again.status, StatusCode::BAD_REQUEST);
    assert!(again.json()["detail"].as_str().unwrap().contains("déjà dans la bibliothèque"));

    // même nom, autre contenu : nouvelle version proposée en remplacement
    let v2 = app.upload("Planning.txt", "Version 2 du planning".as_bytes()).await.json();
    assert_eq!(v2["document"]["id"], "Planning (2)");
    let queue = app.get("/api/library/queue").await.json();
    assert_eq!(queue["pending"][0]["replaces"], "Planning");
    assert_eq!(app.get(&format!("/api/library/docs/{}", q("Planning (2)"))).await.json()["replaces"], "Planning");

    let replaced = app
        .call(Method::PUT, &format!("/api/library/docs/{}", q("Planning (2)")),
              Some(json!({"title": "Planning", "tags": ["planning"], "replaces": "Planning"})))
        .await;
    assert_eq!(replaced.status, StatusCode::OK, "{}", replaced.text);
    let replaced = replaced.json();
    assert_eq!(replaced["id"], "Planning");
    assert_eq!(replaced["source"], "sources/Planning.txt");
    assert!(replaced["updated"].is_string());
    let detail = app.get("/api/library/docs/Planning").await.json();
    assert_eq!(detail["content"], "Version 2 du planning\n");
    assert!(!root.join("Planning (2).md").exists() && !root.join("sources").join("Planning (2).txt").exists());
    assert_eq!(std::fs::read_to_string(root.join("sources").join("Planning.txt")).unwrap(), "Version 2 du planning");
    // l'ancienne version est dans la corbeille de la bibliothèque
    let trashed: Vec<String> = walkdir::WalkDir::new(root.join(".trash"))
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(trashed.contains(&"Planning.md".to_string()) && trashed.contains(&"Planning.txt".to_string()), "{trashed:?}");
    assert_eq!(app.get("/api/library").await.json().as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn fichier_copie_a_la_main() {
    let (app, _) = app(vec![]);
    let root = library_dir(&app);
    std::fs::create_dir_all(root.join("import")).unwrap();
    std::fs::write(root.join("import").join("Procédure.txt"), "Étapes de la recette").unwrap();
    std::fs::write(root.join("archive.zip"), b"PK").unwrap();
    std::fs::write(root.join("~$brouillon.docx"), b"verrou Office").unwrap();

    let queue = app.get("/api/library/queue").await.json();
    let untracked = queue["untracked"].as_array().unwrap();
    assert_eq!(untracked.len(), 2, "{queue}");
    assert_eq!(untracked[0]["path"], "archive.zip");
    assert_eq!(untracked[0]["format"], Value::Null);
    assert_eq!(untracked[1]["path"], "import/Procédure.txt");
    assert_eq!(untracked[1]["format"], "txt");

    let ingested = app.post("/api/library/ingest", Some(json!({"path": "import/Procédure.txt"}))).await;
    assert_eq!(ingested.status, StatusCode::OK, "{}", ingested.text);
    assert!(!root.join("import").join("Procédure.txt").exists());
    assert!(root.join("sources").join("Procédure.txt").is_file());
    let queue = app.get("/api/library/queue").await.json();
    assert_eq!(queue["untracked"].as_array().unwrap().len(), 1);
    assert_eq!(queue["pending"][0]["id"], "Procédure");
    let refused = app.post("/api/library/ingest", Some(json!({"path": "archive.zip"}))).await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST);
    let outside = app.post("/api/library/ingest", Some(json!({"path": "../vault/projets/sap.md"}))).await;
    assert_eq!(outside.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn source_reconversion_et_suppression() {
    let (app, _) = app(vec![]);
    let root = library_dir(&app);
    app.upload("Notes de cadrage.md", SPEC.as_bytes()).await;
    let id = q("Notes de cadrage");
    app.call(Method::PUT, &format!("/api/library/docs/{id}"), Some(json!({"title": "Cadrage", "projects": ["sap"]}))).await;

    let (source, headers) = app.call_raw(Method::GET, &format!("/api/library/docs/{id}/source"), Vec::new()).await;
    assert_eq!(source.text, SPEC);
    assert_eq!(headers["content-type"], "text/plain; charset=utf-8");
    assert!(headers["content-disposition"].to_str().unwrap().starts_with("inline; filename*=UTF-8''Notes%20de%20cadrage.md"));

    // la reconversion régénère le texte sans toucher au classement
    let pivot = root.join("Notes de cadrage.md");
    let text = std::fs::read_to_string(&pivot).unwrap();
    std::fs::write(&pivot, text.replace("Flux des articles", "Texte abîmé")).unwrap();
    app.services.library.sync().unwrap();
    assert_eq!(app.get("/api/library?q=abime").await.json().as_array().unwrap().len(), 1);
    let reconverted = app.post(&format!("/api/library/docs/{id}/reconvert"), None).await.json();
    assert_eq!(reconverted["title"], "Cadrage");
    assert_eq!(reconverted["projects"], json!(["sap"]));
    assert!(std::fs::read_to_string(&pivot).unwrap().contains("Flux des articles"));
    assert_eq!(app.get("/api/library?q=abime").await.json().as_array().unwrap().len(), 0);

    let detail = app.get(&format!("/api/library/docs/{id}")).await.json();
    assert_eq!(detail["outline"], json!([{"level": 1, "title": "Interface articles"}, {"level": 2, "title": "Mapping"}]));

    assert_eq!(app.call(Method::DELETE, &format!("/api/library/docs/{id}"), None).await.status, StatusCode::OK);
    assert_eq!(app.get(&format!("/api/library/docs/{id}")).await.status, StatusCode::NOT_FOUND);
    assert!(!pivot.exists() && !root.join("sources").join("Notes de cadrage.md").exists());
    assert_eq!(app.get("/api/library").await.json().as_array().unwrap().len(), 0);
    assert_eq!(app.get(&format!("/api/library/docs/{}", q("../projets/sap"))).await.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn formats_refuses() {
    let (app, _) = app(vec![]);
    let zip = app.upload("archive.zip", b"PK").await;
    assert_eq!(zip.status, StatusCode::BAD_REQUEST);
    assert!(zip.json()["detail"].as_str().unwrap().contains("Format non pris en charge"));
    let empty = app.upload("vide.txt", b"   \n").await;
    assert_eq!(empty.status, StatusCode::BAD_REQUEST);
    // rien n'est conservé d'un dépôt refusé
    assert!(std::fs::read_dir(library_dir(&app).join("sources")).unwrap().next().is_none());
    let config = app.get("/api/config").await.json();
    assert!(config["library"]["formats"].as_array().unwrap().contains(&json!("pdf")));
}

#[tokio::test]
async fn index_reconstruit_au_demarrage() {
    let (app, _) = app(vec![]);
    app.upload("Interface articles.md", SPEC.as_bytes()).await;
    // un pivot modifié hors de l'application (autre éditeur) est réindexé
    let root = library_dir(&app);
    let pivot = root.join("Interface articles.md");
    let text = std::fs::read_to_string(&pivot).unwrap();
    std::fs::write(&pivot, text.replace("tags: []", "tags: [edite-ailleurs]")).unwrap();
    std::fs::write(root.join("Orphelin.md"), "---\ntype: document\ntitle: Orphelin\nsource: sources/absent.pdf\n---\n\nTexte orphelin.\n").unwrap();
    assert_eq!(app.services.library.sync().unwrap(), 2);
    assert_eq!(app.get("/api/library?tag=edite-ailleurs").await.json().as_array().unwrap().len(), 1);
    assert_eq!(app.get("/api/library?q=orphelin").await.json()[0]["id"], "Orphelin");
    std::fs::remove_file(root.join("Orphelin.md")).unwrap();
    app.services.library.sync().unwrap();
    assert_eq!(app.get("/api/library?q=orphelin").await.json().as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn mail_et_piece_jointe() {
    let (app, _) = app(vec![]);
    let eml = "From: Jean Dupont <jean@exemple.fr>\r\nTo: Marie <marie@exemple.fr>\r\nSubject: Planning de recette\r\n\
               Date: Mon, 12 Oct 2026 14:03:00 +0200\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"b\"\r\n\r\n\
               --b\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nVoici le planning de la recette.\r\n\
               --b\r\nContent-Type: text/plain; name=\"planning.txt\"\r\nContent-Disposition: attachment; filename=\"planning.txt\"\r\n\r\n\
               Lot 1 en mars\r\n--b--\r\n";
    let reply = app.upload("Planning de recette.eml", eml.as_bytes()).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    let ingested = reply.json();
    assert_eq!(ingested["document"]["title"], "Planning de recette");
    assert_eq!(ingested["attachments"][0]["id"], "planning");
    assert_eq!(ingested["attachments"][0]["attached_to"], "[[Planning de recette]]");
    let detail = app.get(&format!("/api/library/docs/{}", q("Planning de recette"))).await.json();
    let content = detail["content"].as_str().unwrap();
    assert!(content.starts_with("**De** : Jean Dupont <jean@exemple.fr>"), "{content}");
    assert!(content.contains("**Pièces jointes** : planning.txt") && content.contains("Voici le planning"), "{content}");
    assert_eq!(app.get("/api/library/queue").await.json()["pending"].as_array().unwrap().len(), 2);
}
