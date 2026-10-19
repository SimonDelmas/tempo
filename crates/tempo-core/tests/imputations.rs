mod common;

use std::collections::HashMap;
use std::sync::Arc;

use axum::http::{Method, StatusCode};
use chrono::NaiveDate;
use common::{Ctx, TestApp};
use serde_json::json;
use tempo_core::imputations::{Hours, ImputationService};
use tempo_core::llm::FakeLlm;
use tempo_core::reports::ReportService;
use tempo_core::vault::split_frontmatter;
use tempo_core::Error;

fn monday() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, 12).unwrap()
}

fn make_service(c: &Ctx, llm: Arc<FakeLlm>) -> ImputationService {
    let reports = Arc::new(ReportService::new(c.vault.clone(), c.index.clone(), c.tasks.clone(), llm.clone(), c.settings.clone()));
    ImputationService::new(c.vault.clone(), c.index.clone(), reports, llm, c.settings.clone())
}

fn hours(value: serde_json::Value) -> Hours {
    serde_json::from_value(value).unwrap()
}

#[test]
fn accounts_and_project_link() {
    let c = common::ctx();
    let service = make_service(&c, FakeLlm::new(Vec::<String>::new()));
    let accounts: HashMap<String, _> = service.accounts().unwrap().into_iter().map(|a| (a.alias.clone(), a)).collect();
    assert_eq!(accounts["SAP-MIG"].days, 40.0);
    assert_eq!(accounts["SAP-MIG"].projects, vec!["sap"]);
    assert_eq!(c.index.resolve_link("SAP-MIG").unwrap().as_deref(), Some("comptes/sap-mig.md"));

    let created = service.create_account("Formation", "FORM", 5.0).unwrap();
    assert_eq!(created.remaining_days, 5.0);
    assert!(c.vault.read_text(&created.path).unwrap().contains("\ndays: 5\n"));
    assert!(matches!(service.create_account("Doublon", "form", 1.0), Err(Error::Vault(_))));
    assert!(matches!(service.create_account("Espace", "A B", 1.0), Err(Error::Vault(_))));

    assert_eq!(service.update_account("FORM", None, Some(7.5)).unwrap().days, 7.5);
    assert_eq!(service.set_project_account("sap", Some("FORM")).unwrap().compte.as_deref(), Some("FORM"));
    assert_eq!(service.set_project_account("sap", None).unwrap().compte, None);
    assert!(matches!(service.set_project_account("sap", Some("INCONNU")), Err(Error::NotFound(_))));
}

#[test]
fn save_week_and_consumption() {
    let c = common::ctx();
    let service = make_service(&c, FakeLlm::new(Vec::<String>::new()));
    let week = service
        .save_week(
            NaiveDate::from_ymd_opt(2026, 10, 14).unwrap(),
            &hours(json!({"2026-10-12": {"sap-mig": 6, "TRANSVERSE": 2}, "2026-10-13": {"SAP-MIG": 8, "TRANSVERSE": 0}})),
            "Recette",
        )
        .unwrap();
    assert_eq!(week.path.as_deref(), Some("imputations/2026/2026-S42.md"));
    assert_eq!(week.totals["2026-10-12"], 8.0);
    assert_eq!(week.hours["2026-10-12"], hours(json!({"x": {"SAP-MIG": 6, "TRANSVERSE": 2}}))["x"]);
    assert_eq!(service.incomplete_days(NaiveDate::from_ymd_opt(2026, 10, 14).unwrap()).unwrap(), vec!["2026-10-14"]);

    let accounts: HashMap<String, _> = service.accounts().unwrap().into_iter().map(|a| (a.alias.clone(), a)).collect();
    assert_eq!(accounts["SAP-MIG"].consumed_days, 1.75);
    assert_eq!(accounts["SAP-MIG"].remaining_days, 38.25);

    let text = c.vault.read_text(week.path.as_deref().unwrap()).unwrap();
    let (meta, body) = split_frontmatter(&text);
    assert_eq!(meta["week"], "2026-S42");
    assert!(body.contains("| [[SAP-MIG]] | 6 | 8 |  |  |  | 14 |"), "{body}");
    assert!(body.contains("## Justification"));

    assert!(matches!(service.save_week(monday(), &hours(json!({"2026-10-20": {"SAP-MIG": 8}})), ""), Err(Error::Vault(_))));
    assert!(matches!(service.save_week(monday(), &hours(json!({"2026-10-12": {"INCONNU": 8}})), ""), Err(Error::NotFound(_))));
}

#[test]
fn propose_uses_weekly_report_and_balances() {
    let c = common::ctx();
    let report = "---\ntype: rapport\nkind: hebdo\nweek: 2026-S42\n---\n\n## Actions réalisées\n- Recette SAP\n";
    c.vault.write("rapports/hebdo/2026-S42.md", report, None, false).unwrap();
    c.index.upsert("rapports/hebdo/2026-S42.md").unwrap();
    let answer = json!({"days": [
        {"date": "2026-10-12", "entries": [{"compte": "SAP-MIG", "hours": 7}, {"compte": "TRANSVERSE", "hours": 2}]},
        {"date": "2026-10-13", "entries": [{"compte": "sap-mig", "hours": 8}, {"compte": "INVENTE", "hours": 4}]},
        {"date": "2026-10-18", "entries": [{"compte": "SAP-MIG", "hours": 8}]},
    ], "comment": "Semaine centrée sur la recette."});
    let llm = FakeLlm::new([answer.to_string()]);
    let service = make_service(&c, llm.clone());
    let proposal = service.propose(monday()).unwrap();
    let day12: f64 = proposal["hours"]["2026-10-12"].as_object().unwrap().values().map(|v| v.as_f64().unwrap()).sum();
    assert_eq!(day12, 8.0);
    assert_eq!(proposal["hours"]["2026-10-13"], json!({"SAP-MIG": 8}));
    assert!(proposal["hours"].get("2026-10-18").is_none()); // dimanche ignoré
    assert_eq!(proposal["comment"], "Semaine centrée sur la recette.");
    let call = &llm.calls()[0];
    let prompt = &call.messages[1].content;
    assert!(prompt.contains("SAP-MIG — Migration SAP S/4"));
    assert!(prompt.contains("sap (Migration SAP) → SAP-MIG"));
    assert!(prompt.contains("- Recette SAP"));
    assert!(call.json_mode);
}

#[tokio::test]
async fn api() {
    let app = TestApp::new(None, None);
    let accounts = app.get("/api/accounts").await.json();
    assert!(accounts.as_array().unwrap().iter().any(|a| a["alias"] == "SAP-MIG"));
    let created = app.post("/api/accounts", Some(json!({"title": "Formation", "alias": "FORM", "days": 3}))).await;
    assert_eq!(created.status, StatusCode::OK);
    assert_eq!(app.call(Method::PATCH, "/api/accounts/FORM", Some(json!({"days": 4}))).await.json()["days"], 4.0);
    let project = app.call(Method::PUT, "/api/projects/sap/account", Some(json!({"compte": "FORM"}))).await.json();
    assert_eq!(project["compte"], "FORM");
    let saved =
        app.call(Method::PUT, "/api/imputations", Some(json!({"date": "2026-10-14", "hours": {"2026-10-12": {"FORM": 8}}}))).await;
    assert_eq!(saved.json()["totals"]["2026-10-12"], 8.0);
    assert_eq!(app.get("/api/imputations?date=2026-10-12").await.json()["week"], "2026-S42");
    assert!(app.get("/api/state").await.json().get("imputation_missing").is_some());
    assert_eq!(app.post("/api/imputations/propose?date=2026-10-12", None).await.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(app.get("/api/imputations?date=demain").await.status, StatusCode::BAD_REQUEST);
}
