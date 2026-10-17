import datetime as dt
import json

from tempo.inbox import InboxProposal, InboxService, split_summaries
from tempo.llm import FakeLLM
from tempo.reports import ReportService, week_bounds
from tempo.vault import split_frontmatter

PROPOSAL = {
    "echange": {"source": "teams", "date": "2026-10-17", "subject": "Accès production SAP",
                "participants": ["Claire Martin", "Marc Leroy"], "projects": ["sap"],
                "thread": "acces-prod-sap", "summary": "- Claire a validé l'accès prod."},
    "new_tasks": [{"title": "Tester les comptes techniques", "project": "sap",
                   "priority": "haute", "status": "a_faire", "due": "2026-10-19"},
                  {"title": "Recevoir les jeux de test de Marc", "status": "attente",
                   "due": "mardi"}],
    "task_updates": [{"task_id": "T-003", "status": "en_cours", "note": "accès validés par Claire"},
                     {"task_id": "T-999", "status": "terminee", "note": "inventé"}],
    "journal_line": "Accès prod validés, comptes créés lundi",
}


def make_inbox(vault, index, tasks, journal, settings, responses):
    return InboxService(vault, index, tasks, journal, FakeLLM(responses), settings)


def test_split_summaries():
    assert split_summaries("a\n---\n\nb\n-----\n") == ["a", "b"]


def test_add_and_list(vault, index, tasks, journal, settings):
    inbox = make_inbox(vault, index, tasks, journal, settings, [])
    paths = inbox.add("Sujet: A\n---\nSujet: B")
    assert len(paths) == 2
    assert len(inbox.list()) == 3  # + l'exemple du vault


def test_analyze_filters_unknown_tasks(vault, index, tasks, journal, settings):
    llm = FakeLLM(["```json\n" + json.dumps(PROPOSAL) + "\n```"])
    inbox = InboxService(vault, index, tasks, journal, llm, settings)
    proposal = inbox.analyze("00-inbox/exemple-teams.md")
    assert [u.task_id for u in proposal.task_updates] == ["T-003"]
    assert proposal.new_tasks[1].due is None  # date invalide ignorée
    assert proposal.duplicate_of is None
    prompt = llm.calls[0]["messages"][1]["content"]
    assert "T-003 | Obtenir les accès production SAP" in prompt
    assert "planning-recette-sap" in prompt
    assert llm.calls[0]["json_mode"] is True


def test_analyze_retries_on_invalid_json(vault, index, tasks, journal, settings):
    llm = FakeLLM(["pas du json", json.dumps(PROPOSAL)])
    inbox = InboxService(vault, index, tasks, journal, llm, settings)
    assert inbox.analyze("00-inbox/exemple-teams.md").echange.thread == "acces-prod-sap"
    assert len(llm.calls) == 2


def test_apply_writes_notes_and_tasks(vault, index, tasks, journal, settings):
    inbox = make_inbox(vault, index, tasks, journal, settings, [])
    proposal = InboxProposal.model_validate(PROPOSAL)
    proposal.task_updates = proposal.task_updates[:1]
    proposal.new_tasks[1].accepted = False
    result = inbox.apply("00-inbox/exemple-teams.md", proposal)

    assert result["echange"] == "echanges/2026/10/2026-10-17-teams-acces-production-sap.md"
    assert result["created_tasks"] == ["T-005"]
    assert result["updated_tasks"] == ["T-003"]
    assert not vault.exists("00-inbox/exemple-teams.md")

    meta, body = split_frontmatter(vault.read_text(result["echange"]))
    assert meta["tasks"] == ["[[T-005]]", "[[T-003]]"]
    assert "## Texte original" in body and "Claire a validé" in body

    t3 = tasks.get("T-003")
    assert t3["status"] == "en_cours"
    assert "[[2026-10-17-teams-acces-production-sap]]" in t3["history"][-1]["text"]

    journal_text = vault.read_text(result["journal"])
    assert ("- [[2026-10-17-teams-acces-production-sap]] : Accès prod validés, comptes créés lundi"
            " → [[T-005]] [[T-003]]") in journal_text

    # le même résumé réimporté est signalé comme doublon
    inbox.add(vault.read_text(result["echange"]).split("## Texte original")[1])
    item = [i for i in inbox.list()][0]["path"]
    inbox.llm = FakeLLM([json.dumps(PROPOSAL)])
    assert inbox.analyze(item).duplicate_of == result["echange"]


def test_week_bounds():
    assert week_bounds(dt.date(2026, 10, 17)) == (dt.date(2026, 10, 12), dt.date(2026, 10, 18))


def test_weekly_report(vault, index, tasks, settings):
    llm = FakeLLM(["```markdown\n## Réunions et échanges\n- COPIL ([[2026-10-15-copil-sap]])\n"
                   "## Actions réalisées\n## Non terminé\n## Plan pour la semaine suivante\n```"])
    reports = ReportService(vault, index, tasks, llm, settings)
    result = reports.weekly(dt.date(2026, 10, 15), extra="Préparer la recette")
    assert result["path"] == "rapports/hebdo/2026-S42.md"

    context = llm.calls[0]["messages"][1]["content"]
    assert "[[2026-10-15-copil-sap]]" in context
    assert "[[2026-10-14-mail-planning-recette]]" in context
    assert "## Tâches terminées cette semaine\n- [[T-002]]" in context
    assert "[[T-003]] Obtenir les accès production SAP | projet: sap | statut: bloquee" in context
    assert "Préparer la recette" in context
    assert "## Texte original" not in context

    meta, body = split_frontmatter(vault.read_text(result["path"]))
    assert meta["week"] == "2026-S42"
    assert "```" not in body and "## Plan pour la semaine suivante" in body
    # une seconde génération ne remplace pas la première
    reports.llm = FakeLLM(["## Réunions et échanges"])
    assert reports.weekly(dt.date(2026, 10, 15))["path"] == "rapports/hebdo/2026-S42-2.md"


def test_task_report(vault, index, tasks, settings):
    llm = FakeLLM(["## Synthèse\nEn cours."])
    reports = ReportService(vault, index, tasks, llm, settings)
    result = reports.task_report("T-001")
    assert result["path"].startswith("rapports/taches/T-001-")
    context = llm.calls[0]["messages"][1]["content"]
    assert "Fiche de la tâche [[T-001]]" in context
    assert "[[2026-10-15-copil-sap]]" in context
    assert "Mapping des unités de mesure" in context
