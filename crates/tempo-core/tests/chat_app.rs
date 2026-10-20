//! Outils du chat sur le reste de l'application : backlog, projets, comptes et imputations,
//! inbox, rapports.

mod common;

use std::sync::Arc;

use chrono::NaiveDate;
use common::Ctx;
use serde_json::{json, Map, Value};
use tempo_core::chat::{ChatMessage, ChatProposal, ChatService};
use tempo_core::llm::{fake_tool_calls, FakeLlm};

fn make_chat(c: &Ctx, llm: Arc<FakeLlm>) -> ChatService {
    ChatService::new(common::chat_deps(c, llm.clone()), llm, c.settings.clone(), None)
}

fn day() -> Option<NaiveDate> {
    NaiveDate::from_ymd_opt(2026, 10, 19)
}

/// Réponses du modèle : un appel d'outil par tour (ou une réponse brute, ex. l'analyse de
/// l'inbox demandée par un outil), puis la réponse finale.
fn propose(c: &Ctx, responses: Vec<String>) -> Value {
    let mut responses = responses;
    responses.push("Voilà.".into());
    let chat = make_chat(c, FakeLlm::new(responses));
    let files = c.vault.list_files();
    let result = chat.ask(&[ChatMessage::user("?")], day()).unwrap();
    assert_eq!(c.vault.list_files(), files); // rien n'est écrit avant validation
    result
}

fn tool(name: &str, args: Value) -> String {
    fake_tool_calls(&[(name, args)])
}

fn summaries(result: &Value) -> Vec<Option<&str>> {
    result["steps"].as_array().unwrap().iter().map(|s| s["summary"].as_str()).collect()
}

/// Applique les propositions dans l'ordre ; `llm` répond aux générations (rapports).
fn apply_all(c: &Ctx, result: &Value, llm: Arc<FakeLlm>) -> Vec<Value> {
    let chat = make_chat(c, llm);
    result["proposals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| chat.apply(&serde_json::from_value::<ChatProposal>(p.clone()).unwrap(), day()).unwrap())
        .collect()
}

#[test]
fn read_tools() {
    let c = common::ctx();
    let chat = make_chat(&c, FakeLlm::new(Vec::<String>::new()));
    let backlog = chat.task_backlog().unwrap();
    assert_eq!(backlog["tasks"].as_array().unwrap().len(), 4);
    assert_eq!(backlog["tasks"][0]["lien"], "[[T-001]]");
    assert!(chat.task_board(Some("2026-S43")).unwrap()["tasks"].is_array());
    assert_eq!(chat.list_projects().unwrap(), json!([{"id": "sap", "title": "Migration SAP", "compte": "SAP-MIG", "lien": "[[sap]]"}]));
    let accounts = chat.list_accounts().unwrap();
    let aliases: Vec<&str> = accounts.as_array().unwrap().iter().map(|a| a["alias"].as_str().unwrap()).collect();
    assert!(aliases.contains(&"SAP-MIG") && aliases.contains(&"TRANSVERSE"));
    assert_eq!(chat.get_imputations(Some("2026-10-21")).unwrap()["week"], "2026-S43");
    assert!(chat.imputation_tracking().unwrap()["periods"].is_array());
    let inbox = chat.list_inbox().unwrap();
    assert_eq!(inbox[0]["path"], "00-inbox/exemple-teams.md");
    assert_eq!(chat.list_drafts().unwrap(), json!([]));
    // outil sans argument : un argument inconnu est refusé
    let args = json!({"x": 1}).as_object().unwrap().clone();
    assert!(chat.run_tool(Some("task_backlog"), &args, &mut Vec::new()).unwrap().starts_with("Erreur : arguments invalides"));
    assert!(chat.run_tool(Some("list_inbox"), &Map::new(), &mut Vec::new()).unwrap().contains("exemple-teams"));
}

#[test]
fn backlog_and_projects() {
    let c = common::ctx();
    let result = propose(&c, vec![
        tool("propose_project_create", json!({"title": "CRM"})),
        tool("propose_task_move", json!({"task_id": "T-004", "before": "T-001"})),
        tool("propose_project_account", json!({"project_id": "sap", "compte": "transverse"})),
        tool("propose_project_move", json!({"project_id": "sap"})),
        // erreurs renvoyées au modèle
        tool("propose_project_create", json!({"title": "migration sap"})),
        tool("propose_task_move", json!({"task_id": "T-004", "project": "sap"})),
        tool("propose_task_move", json!({"task_id": "T-004", "before": "T-004"})),
        tool("propose_project_account", json!({"project_id": "sap", "compte": "SAP-MIG"})),
    ]);
    let summaries = summaries(&result);
    assert!(summaries[..4].iter().all(Option::is_none), "{summaries:?}");
    assert!(summaries[4].unwrap().contains("existe déjà"));
    assert!(summaries[5].unwrap().contains("arguments invalides"), "le projet se change par propose_task_update");
    assert!(summaries[6].unwrap().contains("devant elle-même"));
    assert!(summaries[7].unwrap().contains("déjà dans cet état"));
    let proposals = &result["proposals"];
    assert_eq!(proposals[1], json!({"kind": "move_task", "task_id": "T-004", "title": "Rédiger la procédure de recette",
                                    "before": "T-001"}));
    assert_eq!(proposals[2]["compte"], "TRANSVERSE");

    apply_all(&c, &result, FakeLlm::new(Vec::<String>::new()));
    let chat = make_chat(&c, FakeLlm::new(Vec::<String>::new()));
    let backlog: Vec<Value> = chat.task_backlog().unwrap()["tasks"].as_array().unwrap().clone();
    let order: Vec<&str> = backlog.iter().map(|t| t["id"].as_str().unwrap()).collect();
    assert!(order.iter().position(|id| *id == "T-004") < order.iter().position(|id| *id == "T-001"));
    let projects = chat.list_projects().unwrap();
    let ids: Vec<&str> = projects.as_array().unwrap().iter().map(|p| p["id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec!["crm", "sap"]); // sap placé en fin de backlog
    assert_eq!(projects[1]["compte"], "TRANSVERSE");
}

#[test]
fn accounts_and_imputations() {
    let c = common::ctx();
    // appels parallèles : les propositions valides, puis les erreurs renvoyées au modèle
    let result = propose(&c, vec![
        fake_tool_calls(&[
            ("propose_account_create", json!({"title": "Congés", "alias": "CONGES", "categorie": "conges", "days": 25})),
            ("propose_account_update", json!({"alias": "sap-mig", "days": 45, "title": "Migration SAP S/4 — référentiel articles"})),
            ("propose_imputations", json!({"date": "2026-10-19", "hours": {
                "2026-10-19": {"sap-mig": 6, "TRANSVERSE": 2}, "2026-10-20": {"SAP-MIG": 8, "TRANSVERSE": 0}}})),
            ("propose_saisie", json!({"date": "2026-10-16", "done": true})),
        ]),
        fake_tool_calls(&[
            ("propose_account_create", json!({"title": "Doublon", "alias": "sap-mig"})),
            ("propose_account_update", json!({"alias": "SAP-MIG", "days": 40})),
            ("propose_imputations", json!({"date": "2026-10-19", "hours": {"2026-10-26": {"SAP-MIG": 8}}})),
            ("propose_imputations", json!({"date": "2026-10-19", "hours": {"2026-10-19": {"INCONNU": 8}}})),
            ("propose_imputations", json!({"date": "2026-10-19", "hours": {"2026-10-19": {"SAP-MIG": 20, "TRANSVERSE": 6}}})),
            ("propose_saisie", json!({"date": "2026-10-16", "done": false})),
            ("propose_account_create", json!({"title": "X", "alias": "X", "categorie": "autre"})),
        ]),
    ]);
    let summaries = summaries(&result);
    assert!(summaries[..4].iter().all(Option::is_none), "{summaries:?}");
    let errors: Vec<&str> = summaries[4..].iter().map(|s| s.unwrap()).collect();
    assert!(errors[0].contains("déjà utilisé"));
    assert!(errors[1].contains("Aucune modification"));
    assert!(errors[2].contains("n'appartient pas à la semaine"));
    assert!(errors[3].contains("Compte d'imputation introuvable"));
    assert!(errors[4].contains("Plus de 24 h"));
    assert!(errors[5].contains("Aucune saisie"));
    assert!(errors[6].contains("categorie invalide"));
    let proposals = &result["proposals"];
    // seuls les champs qui changent, avec leur valeur d'origine
    assert_eq!((&proposals[1]["title"], &proposals[1]["before"]), (&Value::Null, &json!({"days": 40.0})));
    assert_eq!(proposals[2]["week"], "2026-S43");
    assert_eq!(proposals[2]["hours"], json!({"2026-10-19": {"SAP-MIG": 6.0, "TRANSVERSE": 2.0}, "2026-10-20": {"SAP-MIG": 8.0}}));

    apply_all(&c, &result, FakeLlm::new(Vec::<String>::new()));
    let chat = make_chat(&c, FakeLlm::new(Vec::<String>::new()));
    let accounts = chat.list_accounts().unwrap();
    let find = |alias: &str| accounts.as_array().unwrap().iter().find(|a| a["alias"] == alias).cloned().unwrap();
    assert_eq!((find("CONGES")["categorie"].clone(), find("CONGES")["days"].clone()), (json!("conges"), json!(25.0)));
    assert_eq!(find("SAP-MIG")["days"], 45.0);
    let week = chat.get_imputations(Some("2026-10-19")).unwrap();
    assert_eq!(week["totals"]["2026-10-19"], 8.0);
    assert_eq!(chat.imputation_tracking().unwrap()["last_saisie"], "2026-10-16");
}

#[test]
fn inbox_and_reports() {
    let c = common::ctx();
    let analysis = json!({
        "echange": {"source": "teams", "date": "2026-10-16", "subject": "Accès production SAP", "summary": "Accès obtenus."},
        "new_tasks": [], "task_updates": [{"task_id": "T-003", "status": "terminee", "note": "accès obtenus"}],
        "journal_line": "Accès production SAP obtenus",
    });
    let result = propose(&c, vec![
        tool("propose_inbox_add", json!({"text": "Mail de Marc : jeux de test livrés.\n---\nTeams Julie : recette prête."})),
        tool("propose_inbox_discard", json!({"path": "00-inbox/exemple-teams.md"})),
        tool("propose_inbox_process", json!({"path": "00-inbox/exemple-teams.md"})),
        analysis.to_string(), // analyse demandée par propose_inbox_process
        tool("propose_weekly_report", json!({"date": "2026-10-15", "extra": "Point sécurité"})),
        tool("propose_task_report", json!({"task_id": "T-001"})),
        tool("propose_inbox_discard", json!({"path": "00-inbox/absent.md"})),
    ]);
    let summaries = summaries(&result);
    assert!(summaries[..5].iter().all(Option::is_none), "{summaries:?}");
    assert!(summaries[5].unwrap().contains("Élément d'inbox introuvable"));
    let proposals = result["proposals"].as_array().unwrap();
    let kinds: Vec<&str> = proposals.iter().map(|p| p["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, vec!["inbox_add", "inbox_discard", "inbox_apply", "weekly_report", "task_report"]);
    assert_eq!(proposals[2]["proposal"]["task_updates"][0]["task_id"], "T-003");
    assert_eq!(proposals[3]["week"], "2026-S42");

    // validation : on ajoute, on traite l'élément existant et on génère les rapports
    let llm = FakeLlm::new(["Semaine chargée.", "La migration avance."]);
    let chat = make_chat(&c, llm.clone());
    let apply = |i: usize| chat.apply(&serde_json::from_value::<ChatProposal>(proposals[i].clone()).unwrap(), day()).unwrap();
    assert!(apply(0)["message"].as_str().unwrap().starts_with("2 élément(s)"));
    let processed = apply(2);
    assert!(processed["path"].as_str().unwrap().starts_with("echanges/2026/10/"));
    assert_eq!(c.tasks.get("T-003").unwrap().task.status, "terminee");
    assert!(!c.vault.exists("00-inbox/exemple-teams.md"));
    let weekly = apply(3);
    assert_eq!(weekly["path"], "rapports/hebdo/2026-S42.md");
    assert!(c.vault.read_text("rapports/hebdo/2026-S42.md").unwrap().contains("Semaine chargée."));
    assert!(llm.calls()[0].messages[1].content.contains("Point sécurité"));
    let task_report = apply(4);
    assert!(c.vault.read_text(task_report["path"].as_str().unwrap()).unwrap().contains("La migration avance."));
}
