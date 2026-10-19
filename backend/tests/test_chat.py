import datetime as dt
import json

from fastapi.testclient import TestClient

from tempo.chat import MAX_STEPS, ChatMessage, ChatService
from tempo.llm import FakeLLM
from tempo.main import create_app


def tool(tool_name, **args):
    return json.dumps({"action": "tool", "tool": tool_name, "args": args})


def answer(text):
    return json.dumps({"action": "answer", "answer": text})


def make_chat(vault, index, tasks, settings, responses):
    llm = FakeLLM(responses)
    return ChatService(vault, index, tasks, llm, settings), llm


def ask(chat, question):
    return chat.ask([ChatMessage(role="user", content=question)], today=dt.date(2026, 10, 19))


def test_tools_read_vault(vault, index, tasks, settings):
    chat, _ = make_chat(vault, index, tasks, settings, [])
    hits = chat.search_notes("recette")
    assert any(h["lien"] == "[[T-004]]" for h in hits)
    note = chat.read_note("[[2026-10-15-copil-sap]]")
    assert note["type"] == "reunion" and "Décisions" in note["contenu"]
    assert chat.read_note("reunions/2026/2026-10-15-copil-sap.md")["titre"] == note["titre"]
    assert {t["id"] for t in chat.list_tasks(status="en_cours")} == {"T-001"}
    assert chat.get_task("T-001")["history"]
    assert all(n["type"] == "reunion" for n in chat.list_notes(type="reunion"))
    assert chat.task_events("2026-10-13", "2026-10-15")


def test_tool_errors_are_returned_to_the_model(vault, index, tasks, settings):
    chat, _ = make_chat(vault, index, tasks, settings, [])
    assert chat.run_tool("delete_note", {}).startswith("Erreur : outil inconnu")
    assert chat.run_tool("read_note", {"nom": "x"}).startswith("Erreur : arguments invalides")
    assert chat.run_tool("read_note", {"name": "inexistante"}).startswith("Erreur : Aucune note")
    assert chat.run_tool("search_notes", {"query": "zzzzz"}) == "Aucun résultat."


def test_agent_loop_calls_tools_then_answers(vault, index, tasks, settings):
    chat, llm = make_chat(vault, index, tasks, settings, [
        tool("search_notes", query="COPIL"),
        tool("read_note", name="2026-10-15-copil-sap"),
        answer("La recette est maintenue au 28 octobre [[2026-10-15-copil-sap]] ([[T-404]])."),
    ])
    result = ask(chat, "Quelle est la date de recette ?")
    assert [s["tool"] for s in result["steps"]] == ["search_notes", "read_note"]
    assert result["links"] == {"2026-10-15-copil-sap": "reunions/2026/2026-10-15-copil-sap.md"}
    system = llm.calls[0]["messages"][0]["content"]
    assert "2026-10-19 (lundi)" in system and "task_events" in system
    last = llm.calls[-1]["messages"]
    assert last[-1]["content"].startswith("Résultat de read_note")
    assert all(c["json_mode"] for c in llm.calls)


def test_agent_loop_stops_after_max_steps(vault, index, tasks, settings):
    responses = [tool("list_tasks")] * MAX_STEPS + [answer("Voici les tâches.")]
    chat, llm = make_chat(vault, index, tasks, settings, responses)
    result = ask(chat, "Mes tâches ?")
    assert len(result["steps"]) == MAX_STEPS
    assert result["answer"] == "Voici les tâches."
    assert "Nombre maximal" in llm.calls[-1]["messages"][-1]["content"]


def test_chat_api(settings):
    llm = FakeLLM([answer("Bonjour")])
    with TestClient(create_app(settings, llm)) as client:
        ok = client.post("/api/chat", json={"messages": [{"role": "user", "content": "Salut"}]})
        assert ok.json()["answer"] == "Bonjour"
        bad = client.post("/api/chat", json={"messages": [{"role": "assistant", "content": "x"}]})
        assert bad.status_code == 400
