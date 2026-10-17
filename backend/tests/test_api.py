from fastapi.testclient import TestClient

from tempo.main import create_app


def test_api_flow(settings):
    with TestClient(create_app(settings)) as client:
        files = client.get("/api/files").json()
        assert any(f["path"] == "projets/sap.md" for f in files)

        note = client.get("/api/notes", params={"path": "projets/sap.md"}).json()
        ok = client.put("/api/notes", json={"path": "projets/sap.md",
                                            "content": note["content"] + "\nAjout\n",
                                            "expected_hash": note["hash"]})
        assert ok.status_code == 200
        conflict = client.put("/api/notes", json={"path": "projets/sap.md", "content": "x",
                                                  "expected_hash": note["hash"]})
        assert conflict.status_code == 409
        assert conflict.json()["current_content"].endswith("Ajout\n")

        created = client.post("/api/notes", json={"type": "reunion", "title": "Point hebdo",
                                                  "date": "2026-10-19"}).json()
        assert created["path"] == "reunions/2026/2026-10-19-point-hebdo.md"

        today = client.post("/api/journal/today", params={"date": "2026-10-17"}).json()
        assert today["path"] == "journal/2026/2026-10-17.md"

        task = client.post("/api/tasks", json={"title": "Nouvelle tâche", "project": "sap"}).json()
        patched = client.patch(f"/api/tasks/{task['id']}", json={"changes": {"status": "en_cours"}})
        assert patched.json()["status"] == "en_cours"
        assert client.get("/api/tasks", params={"status": "en_cours"}).json()[-1]["id"] == task["id"]

        links = client.get("/api/links", params={"path": "taches/T-004-rediger-procedure-recette.md"})
        assert any(n["stem"] == "2026-10-15-copil-sap" for n in links.json()["backlinks"])
        assert client.get("/api/resolve", params={"name": "T-004"}).status_code == 200
        assert client.get("/api/resolve", params={"name": "inconnu"}).status_code == 404

        assert client.get("/api/notes", params={"path": "../x.md"}).status_code == 400
        # sans configuration, les fonctions IA renvoient une erreur explicite
        response = client.post("/api/inbox/analyze", params={"path": "00-inbox/exemple-teams.md"})
        assert response.status_code == 503
        assert "VIBE_BASE_URL" in response.json()["detail"]
