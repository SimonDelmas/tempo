import datetime as dt
import json

import pytest
from fastapi.testclient import TestClient

from tempo.imputations import ImputationService, balance_day
from tempo.llm import FakeLLM
from tempo.main import create_app
from tempo.reports import ReportService
from tempo.vault import NotFoundError, VaultError, split_frontmatter

MONDAY = dt.date(2026, 10, 12)


def make_service(vault, index, tasks, settings, responses=()):
    llm = FakeLLM(list(responses))
    reports = ReportService(vault, index, tasks, llm, settings)
    return ImputationService(vault, index, reports, llm, settings), llm


def test_accounts_and_project_link(vault, index, tasks, settings):
    service, _ = make_service(vault, index, tasks, settings)
    accounts = {a["alias"]: a for a in service.accounts()}
    assert accounts["SAP-MIG"]["days"] == 40
    assert accounts["SAP-MIG"]["projects"] == ["sap"]
    assert index.resolve_link("SAP-MIG") == "comptes/sap-mig.md"

    created = service.create_account("Formation", "FORM", 5)
    assert created["remaining_days"] == 5
    with pytest.raises(VaultError):
        service.create_account("Doublon", "form", 1)
    with pytest.raises(VaultError):
        service.create_account("Espace", "A B", 1)

    assert service.update_account("FORM", days=7.5)["days"] == 7.5
    assert service.set_project_account("sap", "FORM")["compte"] == "FORM"
    assert service.set_project_account("sap", None)["compte"] is None
    with pytest.raises(NotFoundError):
        service.set_project_account("sap", "INCONNU")


def test_save_week_and_consumption(vault, index, tasks, settings):
    service, _ = make_service(vault, index, tasks, settings)
    week = service.save_week(MONDAY + dt.timedelta(days=2), {
        "2026-10-12": {"sap-mig": 6, "TRANSVERSE": 2},
        "2026-10-13": {"SAP-MIG": 8, "TRANSVERSE": 0},
    }, comment="Recette")
    assert week["path"] == "imputations/2026/2026-S42.md"
    assert week["totals"]["2026-10-12"] == 8
    assert week["hours"]["2026-10-12"] == {"SAP-MIG": 6, "TRANSVERSE": 2}
    assert service.incomplete_days(dt.date(2026, 10, 14)) == ["2026-10-14"]

    accounts = {a["alias"]: a for a in service.accounts()}
    assert accounts["SAP-MIG"]["consumed_days"] == 1.75
    assert accounts["SAP-MIG"]["remaining_days"] == 38.25

    text = vault.read_text(week["path"])
    meta, body = split_frontmatter(text)
    assert meta["week"] == "2026-S42"
    assert "| [[SAP-MIG]] | 6 | 8 |  |  |  | 14 |" in body
    assert "## Justification" in body

    with pytest.raises(VaultError):
        service.save_week(MONDAY, {"2026-10-20": {"SAP-MIG": 8}})
    with pytest.raises(NotFoundError):
        service.save_week(MONDAY, {"2026-10-12": {"INCONNU": 8}})


def test_balance_day():
    assert balance_day({"A": 5, "B": 5}) == {"A": 4, "B": 4}
    assert balance_day({"A": 3, "B": 3, "C": 3}) == {"A": 3, "B": 2.5, "C": 2.5}
    assert balance_day({}) == {}


def test_propose_uses_weekly_report_and_balances(vault, index, tasks, settings):
    report = "---\ntype: rapport\nkind: hebdo\nweek: 2026-S42\n---\n\n## Actions réalisées\n- Recette SAP\n"
    vault.write("rapports/hebdo/2026-S42.md", report)
    index.upsert("rapports/hebdo/2026-S42.md")
    answer = {"days": [
        {"date": "2026-10-12", "entries": [{"compte": "SAP-MIG", "hours": 7},
                                           {"compte": "TRANSVERSE", "hours": 2}]},
        {"date": "2026-10-13", "entries": [{"compte": "sap-mig", "hours": 8},
                                           {"compte": "INVENTE", "hours": 4}]},
        {"date": "2026-10-18", "entries": [{"compte": "SAP-MIG", "hours": 8}]},
    ], "comment": "Semaine centrée sur la recette."}
    service, llm = make_service(vault, index, tasks, settings, [json.dumps(answer)])
    proposal = service.propose(MONDAY)
    assert sum(proposal["hours"]["2026-10-12"].values()) == 8
    assert proposal["hours"]["2026-10-13"] == {"SAP-MIG": 8}
    assert "2026-10-18" not in proposal["hours"]  # dimanche ignoré
    assert proposal["comment"] == "Semaine centrée sur la recette."
    prompt = llm.calls[0]["messages"][1]["content"]
    assert "SAP-MIG — Migration SAP S/4" in prompt
    assert "sap (Migration SAP) → SAP-MIG" in prompt
    assert "- Recette SAP" in prompt
    assert llm.calls[0]["json_mode"] is True


def test_api(settings):
    with TestClient(create_app(settings)) as client:
        assert {a["alias"] for a in client.get("/api/accounts").json()} >= {"SAP-MIG"}
        assert client.post("/api/accounts", json={"title": "Formation", "alias": "FORM",
                                                  "days": 3}).status_code == 200
        assert client.patch("/api/accounts/FORM", json={"days": 4}).json()["days"] == 4
        project = client.put("/api/projects/sap/account", json={"compte": "FORM"}).json()
        assert project["compte"] == "FORM"
        saved = client.put("/api/imputations", json={"date": "2026-10-14",
                                                      "hours": {"2026-10-12": {"FORM": 8}}})
        assert saved.json()["totals"]["2026-10-12"] == 8
        assert client.get("/api/imputations", params={"date": "2026-10-12"}).json()["week"] == "2026-S42"
        assert "imputation_missing" in client.get("/api/state").json()
        assert client.post("/api/imputations/propose", params={"date": "2026-10-12"}).status_code == 503
