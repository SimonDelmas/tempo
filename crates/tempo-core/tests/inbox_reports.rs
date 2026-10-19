mod common;

use std::sync::Arc;

use chrono::NaiveDate;
use common::Ctx;
use serde_json::{json, Value};
use tempo_core::inbox::{InboxProposal, InboxService};
use tempo_core::llm::FakeLlm;
use tempo_core::reports::ReportService;
use tempo_core::vault::split_frontmatter;

fn proposal() -> Value {
    json!({
        "echange": {"source": "teams", "date": "2026-10-17", "subject": "Accès production SAP",
                    "participants": ["Claire Martin", "Marc Leroy"], "projects": ["sap"],
                    "thread": "acces-prod-sap", "summary": "- Claire a validé l'accès prod."},
        "new_tasks": [{"title": "Tester les comptes techniques", "project": "sap",
                       "priority": "haute", "status": "a_faire", "due": "2026-10-19"},
                      {"title": "Recevoir les jeux de test de Marc", "status": "attente", "due": "mardi"}],
        "task_updates": [{"task_id": "T-003", "status": "en_cours", "note": "accès validés par Claire"},
                         {"task_id": "T-999", "status": "terminee", "note": "inventé"}],
        "journal_line": "Accès prod validés, comptes créés lundi",
    })
}

fn make_inbox(c: &Ctx, llm: Arc<FakeLlm>) -> InboxService {
    InboxService::new(c.vault.clone(), c.index.clone(), c.tasks.clone(), c.journal.clone(), llm, c.settings.clone())
}

fn make_reports(c: &Ctx, llm: Arc<FakeLlm>) -> ReportService {
    ReportService::new(c.vault.clone(), c.index.clone(), c.tasks.clone(), llm, c.settings.clone())
}

#[test]
fn add_and_list() {
    let c = common::ctx();
    let inbox = make_inbox(&c, FakeLlm::new(Vec::<String>::new()));
    let paths = inbox.add("Sujet: A\n---\nSujet: B").unwrap();
    assert_eq!(paths.len(), 2);
    assert_eq!(inbox.list().unwrap().len(), 3); // + l'exemple du vault
}

#[test]
fn analyze_filters_unknown_tasks() {
    let c = common::ctx();
    let llm = FakeLlm::new([format!("```json\n{}\n```", proposal())]);
    let inbox = make_inbox(&c, llm.clone());
    let result = inbox.analyze("00-inbox/exemple-teams.md").unwrap();
    let ids: Vec<&str> = result.task_updates.iter().map(|u| u.task_id.as_str()).collect();
    assert_eq!(ids, vec!["T-003"]);
    assert_eq!(result.new_tasks[1].due, None); // date invalide ignorée
    assert_eq!(result.duplicate_of, None);
    let call = &llm.calls()[0];
    assert!(call.messages[1].content.contains("T-003 | Obtenir les accès production SAP"));
    assert!(call.messages[1].content.contains("planning-recette-sap"));
    assert!(call.json_mode);
}

#[test]
fn analyze_retries_on_invalid_json() {
    let c = common::ctx();
    let llm = FakeLlm::new(["pas du json".to_string(), proposal().to_string()]);
    let inbox = make_inbox(&c, llm.clone());
    assert_eq!(inbox.analyze("00-inbox/exemple-teams.md").unwrap().echange.thread, "acces-prod-sap");
    assert_eq!(llm.calls().len(), 2);
}

#[test]
fn apply_writes_notes_and_tasks() {
    let c = common::ctx();
    let inbox = make_inbox(&c, FakeLlm::new(Vec::<String>::new()));
    let mut proposal: InboxProposal = serde_json::from_value(proposal()).unwrap();
    proposal.task_updates.truncate(1);
    proposal.new_tasks[1].accepted = false;
    let result = inbox.apply("00-inbox/exemple-teams.md", &proposal).unwrap();

    assert_eq!(result["echange"], "echanges/2026/10/2026-10-17-teams-acces-production-sap.md");
    assert_eq!(result["created_tasks"], json!(["T-005"]));
    assert_eq!(result["updated_tasks"], json!(["T-003"]));
    assert!(!c.vault.exists("00-inbox/exemple-teams.md"));

    let echange = c.vault.read_text(result["echange"].as_str().unwrap()).unwrap();
    let (meta, body) = split_frontmatter(&echange);
    assert_eq!(meta["tasks"], json!(["[[T-005]]", "[[T-003]]"]));
    assert!(echange.contains("\ndate: 2026-10-17\n"));
    assert!(body.contains("## Texte original") && body.contains("Claire a validé"));

    let t3 = c.tasks.get("T-003").unwrap();
    assert_eq!(t3.task.status, "en_cours");
    assert!(t3.history.last().unwrap().text.contains("[[2026-10-17-teams-acces-production-sap]]"));

    let journal = c.vault.read_text(result["journal"].as_str().unwrap()).unwrap();
    assert!(journal.contains(
        "- [[2026-10-17-teams-acces-production-sap]] : Accès prod validés, comptes créés lundi → [[T-005]] [[T-003]]"
    ));

    // le même résumé réimporté est signalé comme doublon
    inbox.add(echange.split("## Texte original").nth(1).unwrap()).unwrap();
    let item = inbox.list().unwrap()[0].path.clone();
    let again = make_inbox(&c, FakeLlm::new([proposal_text()]));
    assert_eq!(again.analyze(&item).unwrap().duplicate_of.as_deref(), result["echange"].as_str());
}

fn proposal_text() -> String {
    proposal().to_string()
}

#[test]
fn weekly_report() {
    let c = common::ctx();
    let llm = FakeLlm::new([
        "```markdown\n## Réunions et échanges\n- COPIL ([[2026-10-15-copil-sap]])\n## Actions réalisées\n## Non terminé\n## Plan pour la semaine suivante\n```",
    ]);
    let reports = make_reports(&c, llm.clone());
    let result = reports.weekly(NaiveDate::from_ymd_opt(2026, 10, 15), "Préparer la recette").unwrap();
    assert_eq!(result["path"], "rapports/hebdo/2026-S42.md");

    let context = &llm.calls()[0].messages[1].content;
    assert!(context.contains("[[2026-10-15-copil-sap]]"));
    assert!(context.contains("[[2026-10-14-mail-planning-recette]]"));
    assert!(context.contains("## Tâches terminées cette semaine\n- [[T-002]]"));
    assert!(context.contains("[[T-003]] Obtenir les accès production SAP | projet: sap | statut: bloquee"));
    assert!(context.contains("Préparer la recette"));
    assert!(!context.contains("## Texte original"));

    let text = c.vault.read_text(result["path"].as_str().unwrap()).unwrap();
    let (meta, body) = split_frontmatter(&text);
    assert_eq!(meta["week"], "2026-S42");
    assert!(!body.contains("```") && body.contains("## Plan pour la semaine suivante"));
    assert!(body.contains("# Rapport hebdomadaire 2026-S42 (12/10 – 18/10)"));
    // une seconde génération ne remplace pas la première
    let again = make_reports(&c, FakeLlm::new(["## Réunions et échanges"]));
    assert_eq!(again.weekly(NaiveDate::from_ymd_opt(2026, 10, 15), "").unwrap()["path"], "rapports/hebdo/2026-S42-2.md");
}

#[test]
fn task_report() {
    let c = common::ctx();
    let llm = FakeLlm::new(["## Synthèse\nEn cours."]);
    let reports = make_reports(&c, llm.clone());
    let result = reports.task_report("T-001").unwrap();
    assert!(result["path"].as_str().unwrap().starts_with("rapports/taches/T-001-"));
    let context = &llm.calls()[0].messages[1].content;
    assert!(context.contains("Fiche de la tâche [[T-001]]"));
    assert!(context.contains("[[2026-10-15-copil-sap]]"));
    assert!(context.contains("Mapping des unités de mesure"));
}
