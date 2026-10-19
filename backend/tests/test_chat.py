import datetime as dt
import json

from fastapi.testclient import TestClient

from tempo.chat import MAX_STEPS, ChatMessage, ChatProposal, ChatService, partial_answer
from tempo.llm import FakeLLM
from tempo.main import create_app


def tool(tool_name, **args):
    return json.dumps({"action": "tool", "tool": tool_name, "args": args})


def answer(text):
    return json.dumps({"action": "answer", "answer": text})


def make_chat(vault, index, tasks, settings, responses, journal=None, chunk=0):
    llm = FakeLLM(responses, chunk=chunk)
    return ChatService(vault, index, tasks, journal, llm, settings), llm


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
    run = lambda name, args: chat.run_tool(name, args, [])  # noqa: E731
    assert run("delete_note", {}).startswith("Erreur : outil inconnu")
    assert run("read_note", {"nom": "x"}).startswith("Erreur : arguments invalides")
    assert run("read_note", {"name": "inexistante"}).startswith("Erreur : Aucune note")
    assert run("search_notes", {"query": "zzzzz"}) == "Aucun résultat."


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


def test_proposals_are_validated_and_returned(vault, index, tasks, journal, settings):
    chat, _ = make_chat(vault, index, tasks, settings, [
        tool("propose_task", title="Préparer la recette", project="sap", due="2026-10-27"),
        tool("propose_task_update", task_id="T-003", status="inconnu"),
        tool("propose_task_update", task_id="T-003", status="en_cours", note="accès obtenus"),
        tool("propose_journal_line", line="Point recette avec Marc"),
        answer("Je propose une tâche, une mise à jour de [[T-003]] et une ligne de journal."),
    ], journal=journal)
    files_before = sorted(vault.list_files())
    result = ask(chat, "Crée une tâche pour la recette et passe T-003 en cours")
    assert sorted(vault.list_files()) == files_before  # rien n'est écrit avant validation
    assert [p["kind"] for p in result["proposals"]] == ["create_task", "update_task", "journal"]
    assert result["proposals"][1]["title"] == "Obtenir les accès production SAP"
    assert result["steps"][1]["summary"].startswith("Erreur : status invalide")


def test_apply_proposals(vault, index, tasks, journal, settings):
    chat, _ = make_chat(vault, index, tasks, settings, [], journal=journal)
    today = dt.date(2026, 10, 19)
    created = chat.apply(ChatProposal(kind="create_task", title="Préparer la recette",
                                      project="sap", due="2026-10-27"), today)
    task = tasks.get(created["message"].split()[1])
    assert task["project"] == "sap" and task["due"] == "2026-10-27"
    chat.apply(ChatProposal(kind="update_task", task_id="T-003", status="en_cours",
                            note="accès obtenus"), today)
    assert tasks.get("T-003")["status"] == "en_cours"
    assert "accès obtenus" in tasks.get("T-003")["content"]
    path = chat.apply(ChatProposal(kind="journal", line="Point recette"), today)["path"]
    assert path == "journal/2026/2026-10-19.md"
    assert "- Point recette" in vault.read_text(path)


def test_partial_answer():
    assert partial_answer('{"action": "tool", "tool": "x"') is None
    assert partial_answer('{"action": "answer", "answer": "Bon') == "Bon"
    assert partial_answer(r'{"action": "answer", "answer": "a\nb \"c\" \u00e9') == 'a\nb "c" é'
    assert partial_answer('{"action": "answer", "answer": "fin", "x": 1}') == "fin"
    assert partial_answer('{"action": "answer", "answer": "coupé \\') == "coupé "


def test_stream_emits_steps_deltas_and_done(vault, index, tasks, settings):
    chat, _ = make_chat(vault, index, tasks, settings, [
        tool("list_tasks", status="bloquee"),
        answer("La tâche [[T-003]] est bloquée."),
    ], chunk=7)
    events = list(chat.stream([ChatMessage(role="user", content="Bloquées ?")]))
    assert events[0] == {"type": "step", "tool": "list_tasks", "args": {"status": "bloquee"},
                         "summary": None}
    deltas = [e["text"] for e in events if e["type"] == "delta"]
    assert len(deltas) > 1 and "".join(deltas) == "La tâche [[T-003]] est bloquée."
    assert events[-1]["type"] == "done"
    assert events[-1]["links"] == {"T-003": "taches/T-003-obtenir-acces-prod-sap.md"}


def test_stream_recovers_from_invalid_json(vault, index, tasks, settings):
    chat, _ = make_chat(vault, index, tasks, settings, ["pas du json", answer("Rattrapé")])
    assert ask(chat, "?")["answer"] == "Rattrapé"


def test_chat_stream_and_apply_api(settings):
    llm = FakeLLM([tool("propose_journal_line", line="Test"), answer("C'est proposé.")])
    with TestClient(create_app(settings, llm)) as client:
        response = client.post("/api/chat/stream", json={"messages": [{"role": "user", "content": "x"}]})
        events = [json.loads(line[6:]) for line in response.text.splitlines() if line.startswith("data: ")]
        assert events[-1]["type"] == "done"
        proposal = events[-1]["proposals"][0]
        applied = client.post("/api/chat/apply", json=proposal).json()
        assert applied["path"].startswith("journal/")
    with TestClient(create_app(settings)) as client:  # API LLM non configurée
        failing = client.post("/api/chat/stream", json={"messages": [{"role": "user", "content": "x"}]})
        assert json.loads(failing.text.strip()[6:])["type"] == "error"
