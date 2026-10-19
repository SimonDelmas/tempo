mod common;

use std::collections::{HashMap, HashSet};

use serde_json::json;
use tempo_core::tasks::NewTask;
use tempo_core::vault::{content_hash, split_frontmatter};
use tempo_core::Error;

#[test]
fn path_outside_vault_is_rejected() {
    let c = common::ctx();
    assert!(matches!(c.vault.resolve("../secret.md"), Err(Error::Vault(_))));
    assert!(matches!(c.vault.resolve("notes/script.py"), Err(Error::Vault(_))));
    assert!(matches!(c.vault.resolve("C:/Windows/x.md"), Err(Error::Vault(_))));
    assert!(c.vault.resolve("sujets/../projets/sap.md").is_ok());
}

#[test]
fn write_detects_external_change() {
    let c = common::ctx();
    let rel = "sujets/mapping-unites-de-mesure.md";
    let stale = content_hash("ancienne version");
    assert!(matches!(c.vault.write(rel, "nouveau", Some(&stale), false), Err(Error::Conflict { .. })));
    let current = c.vault.read(rel).unwrap();
    assert_eq!(c.vault.write(rel, "nouveau", Some(&current.hash), false).unwrap(), content_hash("nouveau"));
}

#[test]
fn index_types_links_and_search() {
    let c = common::ctx();
    let notes: HashMap<String, _> = c.index.all_notes().unwrap().into_iter().map(|n| (n.path.clone(), n)).collect();
    assert_eq!(notes["journal/2026/2026-10-15.md"].kind, "journal");
    assert_eq!(notes["taches/T-001-migration-referentiel-articles.md"].alias, "T-001");
    assert_eq!(c.index.resolve_link("T-001").unwrap().as_deref(), Some("taches/T-001-migration-referentiel-articles.md"));
    assert_eq!(
        c.index.resolve_link("2026-10-15-copil-sap").unwrap().as_deref(),
        Some("reunions/2026/2026-10-15-copil-sap.md")
    );

    let backlinks: HashSet<String> =
        c.index.backlinks("taches/T-001-migration-referentiel-articles.md").unwrap().into_iter().map(|n| n.path).collect();
    assert!(backlinks.contains("reunions/2026/2026-10-15-copil-sap.md"));
    assert!(backlinks.contains("sujets/mapping-unites-de-mesure.md"));

    let results = c.index.search("rejets", 30).unwrap();
    assert!(results.iter().any(|r| r.path == "journal/2026/2026-10-16.md"));
    // recherche insensible aux accents
    assert!(!c.index.search("echanges", 30).unwrap().is_empty() || !c.index.search("reunion", 30).unwrap().is_empty());
}

#[test]
fn index_week_filter() {
    let c = common::ctx();
    let paths: Vec<String> = c
        .index
        .list_notes(Some("journal"), Some("2026-10-15"), Some("2026-10-16"))
        .unwrap()
        .into_iter()
        .map(|n| n.path)
        .collect();
    assert_eq!(paths, vec!["journal/2026/2026-10-15.md", "journal/2026/2026-10-16.md"]);
}

#[test]
fn delete_moves_to_trash_and_watcher_ignores_it() {
    let c = common::ctx();
    let rel = "sujets/mapping-unites-de-mesure.md";
    c.vault.delete(rel).unwrap();
    assert!(!c.vault.exists(rel));
    assert!(!c.vault.list_files().iter().any(|f| f.contains(".trash")));
    assert!(c.vault.is_ignored(&c.vault.root.join(".trash").join("x").join("a.md")));
}

#[test]
fn list_and_get_tasks() {
    let c = common::ctx();
    let ids: Vec<String> = c.tasks.all().unwrap().into_iter().map(|t| t.id).collect();
    assert_eq!(ids, vec!["T-001", "T-002", "T-003", "T-004"]);
    let t3 = c.tasks.get("T-003").unwrap();
    assert_eq!(t3.task.status, "bloquee");
    assert_eq!(t3.task.due.as_deref(), Some("2026-10-20"));
    assert_eq!(t3.history.len(), 3);
}

#[test]
fn create_assigns_next_id() {
    let c = common::ctx();
    let new = NewTask {
        title: "Tester les comptes techniques".into(),
        project: Some("sap".into()),
        due: Some("2026-10-19".into()),
        origin: Some("2026-10-17-teams-acces".into()),
        ..Default::default()
    };
    let task = c.tasks.create(new, Some("2026-10-17")).unwrap();
    assert_eq!(task.task.id, "T-005");
    assert!(task.task.path.starts_with("taches/T-005-tester-les-comptes"));
    let text = c.vault.read_text(&task.task.path).unwrap();
    let (meta, body) = split_frontmatter(&text);
    assert_eq!(meta["status"], "a_faire");
    assert_eq!(meta["due"], "2026-10-19");
    assert!(text.contains("\ndue: 2026-10-19\n"), "date sans guillemets : {text}");
    assert!(body.contains("- 2026-10-17 : créée depuis [[2026-10-17-teams-acces]]"));
    assert_eq!(c.tasks.next_id().unwrap(), "T-006");
}

#[test]
fn update_status_appends_history() {
    let c = common::ctx();
    let changes = json!({"status": "en_cours"});
    let task = c.tasks.update("T-003", changes.as_object().unwrap(), Some("accès validés"), Some("2026-10-17")).unwrap();
    assert_eq!(task.task.status, "en_cours");
    let last = task.history.last().unwrap();
    assert_eq!((last.date.as_str(), last.text.as_str()), ("2026-10-17", "statut bloquee → en_cours — accès validés"));
    // le frontmatter reste lisible et l'historique reste en fin de fichier
    let text = c.vault.read_text(&task.task.path).unwrap();
    assert!(text.starts_with("---\nid: T-003\n"));
    assert!(text.trim_end().ends_with("accès validés"));
    assert!(text.contains("\ndue: 2026-10-20\ncreated: 2026-10-09\n"));
}

#[test]
fn update_rejects_invalid_values() {
    let c = common::ctx();
    let bad_status = json!({"status": "fini"});
    assert!(matches!(c.tasks.update("T-001", bad_status.as_object().unwrap(), None, None), Err(Error::Vault(_))));
    let bad_due = json!({"due": "demain"});
    assert!(matches!(c.tasks.update("T-001", bad_due.as_object().unwrap(), None, None), Err(Error::Vault(_))));
}

#[test]
fn events_between_dates() {
    let c = common::ctx();
    let events = c.tasks.events("2026-10-14", "2026-10-16").unwrap();
    assert!(events.iter().any(|e| e.date == "2026-10-15"
        && e.task_id == "T-002"
        && e.text == "statut en_cours → terminee ([[2026-10-15-copil-sap]])"));
    assert!(events.iter().all(|e| ("2026-10-14".."2026-10-17").contains(&e.date.as_str())));
}
