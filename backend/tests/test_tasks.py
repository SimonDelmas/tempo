import pytest

from tempo.tasks import parse_history
from tempo.vault import VaultError, split_frontmatter


def test_list_and_get(tasks):
    ids = [t["id"] for t in tasks.list()]
    assert ids == ["T-001", "T-002", "T-003", "T-004"]
    t3 = tasks.get("T-003")
    assert t3["status"] == "bloquee"
    assert t3["due"] == "2026-10-20"
    assert len(t3["history"]) == 3


def test_create_assigns_next_id(tasks, vault):
    task = tasks.create("Tester les comptes techniques", project="sap", due="2026-10-19",
                        origin="2026-10-17-teams-acces", today="2026-10-17")
    assert task["id"] == "T-005"
    assert task["path"].startswith("taches/T-005-tester-les-comptes")
    meta, body = split_frontmatter(vault.read_text(task["path"]))
    assert meta["status"] == "a_faire"
    assert "- 2026-10-17 : créée depuis [[2026-10-17-teams-acces]]" in body
    assert tasks.next_id() == "T-006"


def test_update_status_appends_history(tasks, vault):
    task = tasks.update("T-003", {"status": "en_cours"}, note="accès validés", today="2026-10-17")
    assert task["status"] == "en_cours"
    assert task["history"][-1] == {"date": "2026-10-17",
                                   "text": "statut bloquee → en_cours — accès validés"}
    # le frontmatter reste lisible et l'historique reste en fin de fichier
    text = vault.read_text(task["path"])
    assert text.startswith("---\nid: T-003\n")
    assert text.rstrip().endswith("accès validés")


def test_update_rejects_invalid_values(tasks):
    with pytest.raises(VaultError):
        tasks.update("T-001", {"status": "fini"})
    with pytest.raises(VaultError):
        tasks.update("T-001", {"due": "demain"})


def test_events_between_dates(tasks):
    events = tasks.events("2026-10-14", "2026-10-16")
    assert ("2026-10-15", "T-002", "statut en_cours → terminee ([[2026-10-15-copil-sap]])") in {
        (e["date"], e["task_id"], e["text"]) for e in events}
    assert all("2026-10-14" <= e["date"] <= "2026-10-16" for e in events)


def test_parse_history_ignores_other_sections():
    text = "## Notes\n- 2026-01-01 : pas un événement\n\n## Historique\n- 2026-02-02 : créée\n"
    assert parse_history(text) == [{"date": "2026-02-02", "text": "créée"}]
