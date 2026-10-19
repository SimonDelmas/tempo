import datetime as dt
import json

import pytest
from fastapi.testclient import TestClient

from tempo.assist import AssistService
from tempo.llm import FakeLLM
from tempo.main import create_app
from tempo.vault import VaultError

NOTE = "reunions/2026/2026-10-15-copil-sap.md"


def make(vault, index, tasks, settings, responses):
    llm = FakeLLM(responses)
    return AssistService(vault, index, tasks, llm, settings), llm


def test_extract_tasks_cleans_drafts(vault, index, tasks, settings):
    drafts = {"tasks": [
        {"title": "Rédiger la procédure de recette", "project": "sap", "duplicate_of": "T-004"},
        {"title": "Relancer Marc pour les jeux de test", "priority": "urgente", "due": "vendredi",
         "duplicate_of": "T-999"},
        {"title": "  "},
    ]}
    assist, llm = make(vault, index, tasks, settings, [json.dumps(drafts)])
    result = assist.extract_tasks(NOTE, today=dt.date(2026, 10, 19))["tasks"]
    assert [t["title"] for t in result] == ["Rédiger la procédure de recette",
                                            "Relancer Marc pour les jeux de test"]
    assert result[0]["duplicate_of"] == "T-004"
    assert result[1] == {**result[1], "priority": "normale", "due": None, "duplicate_of": None}
    prompt = llm.calls[0]["messages"]
    assert "2026-10-19" in prompt[0]["content"]
    assert "T-004 : Rédiger la procédure de recette" in prompt[1]["content"]
    assert "Recette maintenue au 28 octobre" in prompt[1]["content"]
    assert "sap" in prompt[1]["content"].split("\n")[0]


def test_transform(vault, index, tasks, settings):
    assist, llm = make(vault, index, tasks, settings, ["```markdown\n- point 1\n```"])
    assert assist.transform("resumer", "un long texte") == {"text": "- point 1"}
    assert llm.calls[0]["model"] == settings.model_fast
    with pytest.raises(VaultError):
        assist.transform("traduire", "x")
    with pytest.raises(VaultError):
        assist.transform("resumer", "  ")


def test_assist_api_and_task_origin(settings):
    llm = FakeLLM([json.dumps({"tasks": []}), "Texte reformulé"])
    with TestClient(create_app(settings, llm)) as client:
        assert client.post("/api/assist/extract-tasks", params={"path": NOTE}).json() == {"tasks": []}
        text = client.post("/api/assist/text", json={"action": "reformuler", "text": "x"}).json()
        assert text == {"text": "Texte reformulé"}
        task = client.post("/api/tasks", json={"title": "Relancer Marc",
                                               "origin": "2026-10-15-copil-sap"}).json()
        assert "créée depuis [[2026-10-15-copil-sap]]" in task["content"]
