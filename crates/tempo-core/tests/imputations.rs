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

    // sans catégorie dans le fichier : projet
    assert_eq!(accounts["SAP-MIG"].categorie, "projet");
    assert_eq!(accounts["TRANSVERSE"].categorie, "interne");

    let created = service.create_account("Formation", "FORM", "interne", 5.0, "Formations Rust et SAP").unwrap();
    assert_eq!(created.remaining_days, 5.0);
    assert_eq!(created.categorie, "interne");
    assert!(c.vault.read_text(&created.path).unwrap().contains("\ncategorie: interne\ndays: 5\n"));
    assert!(matches!(service.create_account("Doublon", "form", "", 1.0, ""), Err(Error::Vault(_))));
    assert!(matches!(service.create_account("Espace", "A B", "", 1.0, ""), Err(Error::Vault(_))));
    assert!(matches!(service.create_account("Vacances", "VAC", "repos", 1.0, ""), Err(Error::Vault(_))));
    assert_eq!(service.create_account("Congés payés", "CP", "Congès", 25.0, "").unwrap().categorie, "conges");
    // classés par catégorie puis par alias
    let order: Vec<String> = service.accounts().unwrap().into_iter().map(|a| a.alias).collect();
    assert_eq!(order, ["SAP-MIG", "FORM", "TRANSVERSE", "CP"]);
    assert_eq!(service.update_account("FORM", None, Some("projet"), None, None).unwrap().categorie, "projet");
    assert!(matches!(service.update_account("FORM", None, Some("autre"), None, None), Err(Error::Vault(_))));

    assert_eq!(created.description, "Formations Rust et SAP");
    assert!(c.vault.read_text(&created.path).unwrap().contains("\ndescription: Formations Rust et SAP\n"));

    assert_eq!(service.update_account("FORM", None, None, Some(7.5), None).unwrap().days, 7.5);
    assert_eq!(service.update_account("FORM", None, None, None, Some(" Plan de formation ")).unwrap().description, "Plan de formation");
    let cleared = service.update_account("FORM", None, None, None, Some("")).unwrap();
    assert_eq!(cleared.description, "");
    assert!(!c.vault.read_text(&cleared.path).unwrap().contains("description"));
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
fn export_csv_between_dates() {
    let c = common::ctx();
    let service = make_service(&c, FakeLlm::new(Vec::<String>::new()));
    service
        .save_week(monday(), &hours(json!({"2026-10-12": {"TRANSVERSE": 1.5, "SAP-MIG": 6.5}, "2026-10-16": {"SAP-MIG": 8}})), "")
        .unwrap();
    service.save_week(day(19), &hours(json!({"2026-10-19": {"SAP-MIG": 8}})), "").unwrap();

    let csv = service.export_csv(day(12), day(16)).unwrap();
    let lines: Vec<&str> = csv.trim_start_matches('\u{feff}').lines().collect();
    assert_eq!(
        lines,
        vec![
            "Date;Compte;Nom;Catégorie;Heures",
            "2026-10-12;SAP-MIG;Migration SAP S/4 — référentiel articles;projet;6,5",
            "2026-10-12;TRANSVERSE;Activités transverses (réunions d'équipe, support, admin);interne;1,5",
            "2026-10-16;SAP-MIG;Migration SAP S/4 — référentiel articles;projet;8",
        ]
    );
    assert!(csv.starts_with('\u{feff}'));
    assert_eq!(service.export_csv(day(1), day(9)).unwrap().lines().count(), 1);
    assert!(matches!(service.export_csv(day(16), day(12)), Err(Error::Vault(_))));
}

fn day(d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, d).unwrap()
}

#[test]
fn saisies_define_periods() {
    let c = common::ctx();
    let service = make_service(&c, FakeLlm::new(Vec::<String>::new()));
    // sans saisie : pas de période, le compteur porte sur la semaine en cours
    assert!(service.tracking(day(14)).unwrap().periods.is_empty());
    assert_eq!(service.missing_days(day(14)).unwrap(), 3);

    // S41 complète sauf le vendredi (4 h), S42 : lundi et mardi saisis, mardi à 10 h
    let s41 = hours(json!({
        "2026-10-05": {"SAP-MIG": 8}, "2026-10-06": {"SAP-MIG": 6, "TRANSVERSE": 2},
        "2026-10-07": {"SAP-MIG": 8}, "2026-10-08": {"TRANSVERSE": 8}, "2026-10-09": {"SAP-MIG": 4},
    }));
    service.save_week(day(5), &s41, "").unwrap();
    service.save_week(day(12), &hours(json!({"2026-10-12": {"SAP-MIG": 8}, "2026-10-13": {"TRANSVERSE": 10}})), "").unwrap();

    service.add_saisie(day(4), day(14)).unwrap();
    service.add_saisie(day(11), day(14)).unwrap();
    let tracking = service.add_saisie(day(4), day(14)).unwrap(); // doublon ignoré
    assert_eq!(tracking.saisies, vec!["2026-10-11", "2026-10-04"]);
    assert_eq!(tracking.last_saisie.as_deref(), Some("2026-10-11"));
    assert_eq!(tracking.path.as_deref(), Some("imputations/saisies.md"));
    assert_eq!(tracking.periods.len(), 2);

    // période ouverte : du lendemain de la dernière saisie à aujourd'hui
    let open = &tracking.periods[0];
    assert!(open.open);
    assert_eq!((open.start.as_str(), open.end.as_str()), ("2026-10-12", "2026-10-14"));
    assert_eq!((open.workdays, open.complete_days, open.complete), (3, 1, false));
    assert_eq!(open.incomplete_weeks.len(), 1);
    let gap = &open.incomplete_weeks[0];
    assert_eq!(gap.week, "2026-S42");
    assert_eq!(gap.start, "2026-10-12");
    assert_eq!(gap.days, vec!["2026-10-13", "2026-10-14"]);
    assert_eq!((gap.missing_hours, gap.extra_hours), (8.0, 2.0));
    assert_eq!(service.missing_days(day(14)).unwrap(), 2);

    // période close : bilan par compte, trié par heures décroissantes
    let closed = &tracking.periods[1];
    assert!(!closed.open);
    assert_eq!((closed.start.as_str(), closed.end.as_str()), ("2026-10-05", "2026-10-11"));
    assert_eq!((closed.workdays, closed.complete_days), (5, 4));
    assert_eq!((closed.total_hours, closed.expected_hours), (36.0, 40.0));
    let bilan: Vec<(&str, f64, f64)> = closed.accounts.iter().map(|a| (a.alias.as_str(), a.hours, a.days)).collect();
    assert_eq!(bilan, vec![("SAP-MIG", 26.0, 3.25), ("TRANSVERSE", 10.0, 1.25)]);
    assert_eq!(closed.accounts[0].title, "Migration SAP S/4 — référentiel articles");
    assert_eq!(closed.accounts[1].share, 0.278);
    assert_eq!(closed.incomplete_weeks[0].days, vec!["2026-10-09"]);
    assert_eq!(closed.incomplete_weeks[0].missing_hours, 4.0);

    // fichier Markdown lisible, relu tel quel
    let text = c.vault.read_text("imputations/saisies.md").unwrap();
    assert!(text.contains("dates: [2026-10-04, 2026-10-11]"), "{text}");
    assert!(text.contains("- 2026-10-11\n- 2026-10-04\n"), "{text}");

    // saisie du jour : plus de période ouverte
    assert!(service.add_saisie(day(14), day(14)).unwrap().periods.iter().all(|p| !p.open));
    assert_eq!(service.missing_days(day(14)).unwrap(), 0);
    let tracking = service.remove_saisie(day(14), day(14)).unwrap();
    assert!(tracking.periods[0].open);
    assert!(matches!(service.remove_saisie(day(1), day(14)), Err(Error::NotFound(_))));
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
    let patched = app.call(Method::PATCH, "/api/accounts/FORM", Some(json!({"description": "Montée en compétence"}))).await;
    assert_eq!(patched.json()["description"], "Montée en compétence");
    assert_eq!(patched.json()["days"], 4.0);
    assert_eq!(patched.json()["categorie"], "projet");
    let patched = app.call(Method::PATCH, "/api/accounts/FORM", Some(json!({"categorie": "conges"}))).await;
    assert_eq!(patched.json()["categorie"], "conges");
    let invalid = app.call(Method::PATCH, "/api/accounts/FORM", Some(json!({"categorie": "autre"}))).await;
    assert_eq!(invalid.status, StatusCode::BAD_REQUEST);
    let project = app.call(Method::PUT, "/api/projects/sap/account", Some(json!({"compte": "FORM"}))).await.json();
    assert_eq!(project["compte"], "FORM");
    let created = app.post("/api/projects", Some(json!({"title": "Refonte du portail"}))).await.json();
    assert_eq!(created["id"], "refonte-du-portail");
    assert_eq!(created["path"], "projets/refonte-du-portail.md");
    assert!(app.get("/api/projects").await.json().as_array().unwrap().iter().any(|p| p["id"] == "refonte-du-portail"));
    assert_eq!(app.post("/api/projects", Some(json!({"title": "migration sap"}))).await.status, StatusCode::BAD_REQUEST);
    assert_eq!(app.post("/api/projects", Some(json!({"title": " "}))).await.status, StatusCode::BAD_REQUEST);
    let saved =
        app.call(Method::PUT, "/api/imputations", Some(json!({"date": "2026-10-14", "hours": {"2026-10-12": {"FORM": 8}}}))).await;
    assert_eq!(saved.json()["totals"]["2026-10-12"], 8.0);
    assert_eq!(app.get("/api/imputations?date=2026-10-12").await.json()["week"], "2026-S42");
    assert!(app.get("/api/state").await.json().get("imputation_missing").is_some());
    assert_eq!(app.get("/api/imputations/saisies").await.json()["periods"], json!([]));
    let tracking = app.post("/api/imputations/saisies", Some(json!({"date": "2026-10-09"}))).await.json();
    assert_eq!(tracking["saisies"], json!(["2026-10-09"]));
    assert_eq!(tracking["periods"][0]["open"], true);
    assert_eq!(app.post("/api/imputations/saisies", Some(json!({"date": "hier"}))).await.status, StatusCode::BAD_REQUEST);
    let removed = app.call(Method::DELETE, "/api/imputations/saisies?date=2026-10-09", None).await.json();
    assert_eq!(removed["saisies"], json!([]));
    assert_eq!(app.post("/api/imputations/propose?date=2026-10-12", None).await.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(app.get("/api/imputations?date=demain").await.status, StatusCode::BAD_REQUEST);

    let (csv, headers) = app.call_raw(Method::GET, "/api/imputations/export?start=2026-10-01&end=2026-10-31", Vec::new()).await;
    assert_eq!(csv.status, StatusCode::OK);
    assert!(csv.text.contains("2026-10-12;FORM;Formation;conges;8"), "{}", csv.text);
    assert_eq!(headers["content-type"], "text/csv; charset=utf-8");
    assert!(headers["content-disposition"].to_str().unwrap().contains("imputations_2026-10-01_2026-10-31.csv"));
    let reversed = app.get("/api/imputations/export?start=2026-10-31&end=2026-10-01").await;
    assert_eq!(reversed.status, StatusCode::BAD_REQUEST);
}
