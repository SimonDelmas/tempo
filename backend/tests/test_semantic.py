from fastapi.testclient import TestClient

from tempo.chat import ChatService
from tempo.llm import FakeLLM, LLMError
from tempo.main import create_app
from tempo.semantic import CHUNK_CHARS, SemanticIndex, chunk_text, hashing_embedder


ZEPPELIN = "sujets/zeppelin.md"


def add_zeppelin(vault, index):
    """note aux mots introuvables ailleurs, pour un classement déterministe"""
    vault.write(ZEPPELIN, "---\ntype: sujet\n---\n\n# Zeppelin\n\nDirigeable gonflé à l'hélium, "
                          "nacelle et hangar d'amarrage.\n")
    index.upsert(ZEPPELIN)


def make(vault, index, embedder=None, model="test"):
    return SemanticIndex(vault, index, embedder or hashing_embedder(),
                         vault.root / ".tempo" / "vectors.db", model)


def test_chunk_text():
    body = "\n\n".join(f"Paragraphe {i} " + "x" * 300 for i in range(10))
    chunks = chunk_text("Titre", body)
    assert len(chunks) > 1 and all(c.startswith("Titre\n\n") for c in chunks)
    assert all(len(c) <= CHUNK_CHARS + 10 for c in chunks)
    assert chunk_text("Vide", "") == ["Vide"]
    assert len(chunk_text("Long", "y" * (CHUNK_CHARS * 2 + 5))) == 3


def test_disabled_without_embedder(vault, index):
    semantic = SemanticIndex(vault, index, None, vault.root / ".tempo" / "vectors.db")
    assert not semantic.enabled
    assert semantic.sync() == 0 and semantic.search("x") == [] and semantic.related("x") == []
    assert semantic.status()["enabled"] is False


def test_sync_is_incremental(vault, index):
    calls = []
    embed = hashing_embedder()

    def counting(texts):
        calls.append(len(texts))
        return embed(texts)

    semantic = make(vault, index, counting)
    total = semantic.sync()
    assert total == semantic.status()["total"] == semantic.status()["indexed"] > 5
    assert semantic.sync() == 0  # rien n'a changé
    path = "sujets/mapping-unites-de-mesure.md"
    vault.write(path, vault.read_text(path) + "\nNouvelle conversion des palettes.\n")
    index.upsert(path)
    assert semantic.sync() == 1
    vault.delete(path)
    index.remove(path)
    semantic.sync()
    assert all(r["path"] != path for r in semantic.search("unités de mesure palettes"))


def test_search_and_related(vault, index):
    add_zeppelin(vault, index)
    semantic = make(vault, index)
    semantic.sync()
    results = semantic.search("hangar du dirigeable à hélium")
    assert results[0]["path"] == ZEPPELIN
    assert 0 < results[0]["score"] <= 1 and results[0]["snippet"].startswith("Dirigeable")
    assert results == sorted(results, key=lambda r: -r["score"])
    related = semantic.related("taches/T-003-obtenir-acces-prod-sap.md")
    paths = {r["path"] for r in related}
    assert "taches/T-003-obtenir-acces-prod-sap.md" not in paths
    linked = {n["path"] for n in index.backlinks("taches/T-003-obtenir-acces-prod-sap.md")}
    assert not paths & linked  # déjà liées : pas suggérées


def test_model_change_resets_vectors(vault, index):
    make(vault, index, model="a").sync()
    semantic = make(vault, index, hashing_embedder(64), model="b")
    assert semantic.status()["indexed"] == 0
    assert semantic.sync() > 0 and semantic.search("recette")


def test_embedding_errors_are_reported(vault, index):
    def failing(texts):
        raise LLMError("API indisponible")

    semantic = make(vault, index, failing)
    assert semantic.sync() == 0
    assert semantic.status()["error"] == "API indisponible"


def test_chat_semantic_tool(vault, index, tasks, journal, settings):
    add_zeppelin(vault, index)
    semantic = make(vault, index)
    semantic.sync()
    chat = ChatService(vault, index, tasks, journal, FakeLLM([]), settings, semantic=semantic)
    assert "semantic_search" in chat.system_prompt(__import__("datetime").date(2026, 10, 19))
    hits = chat.semantic_search("nacelle de dirigeable")
    assert hits[0]["lien"] == "[[zeppelin]]" and "proximite" in hits[0]
    assert "semantic_search" not in ChatService(vault, index, tasks, journal, FakeLLM([]), settings).tools


def test_semantic_api(settings):
    with TestClient(create_app(settings, FakeLLM([]), hashing_embedder())) as client:
        assert client.get("/api/config").json()["semantic_enabled"] is True
        synced = client.post("/api/semantic/sync").json()
        assert synced["synced"] == synced["total"] == synced["indexed"]
        hits = client.get("/api/semantic/search", params={"q": "procédure de recette"}).json()
        assert "taches/T-004-rediger-procedure-recette.md" in [h["path"] for h in hits[:3]]
        assert isinstance(client.get("/api/semantic/related",
                                     params={"path": "projets/sap.md"}).json(), list)
    with TestClient(create_app(settings, FakeLLM([]))) as client:
        assert client.get("/api/config").json()["semantic_enabled"] is False
