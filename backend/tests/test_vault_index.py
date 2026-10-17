import pytest

from tempo.vault import (ConflictError, VaultError, append_to_section, content_hash,
                         join_frontmatter, split_frontmatter)


def test_frontmatter_roundtrip():
    text = "---\ntype: tache\nstatus: en_cours\n---\n\nCorps\n"
    meta, body = split_frontmatter(text)
    assert meta == {"type": "tache", "status": "en_cours"}
    assert join_frontmatter(meta, body) == text


def test_append_to_section_inserts_before_next_heading():
    text = "# T\n\n## Échanges\n- a\n\n## Fait aujourd'hui\n"
    result = append_to_section(text, "## Échanges", "- b")
    assert result == "# T\n\n## Échanges\n- a\n- b\n\n## Fait aujourd'hui\n"


def test_append_to_section_creates_missing_section():
    assert append_to_section("Corps\n", "## Historique", "- x") == "Corps\n\n## Historique\n- x\n"


def test_path_outside_vault_is_rejected(vault):
    with pytest.raises(VaultError):
        vault.resolve("../secret.md")
    with pytest.raises(VaultError):
        vault.resolve("notes/script.py")


def test_write_detects_external_change(vault):
    rel = "sujets/mapping-unites-de-mesure.md"
    stale = content_hash("ancienne version")
    with pytest.raises(ConflictError):
        vault.write(rel, "nouveau", expected_hash=stale)
    current = vault.read(rel)
    assert vault.write(rel, "nouveau", expected_hash=current["hash"]) == content_hash("nouveau")


def test_index_types_links_and_search(index):
    notes = {n["path"]: n for n in index.list_notes()}
    assert notes["journal/2026/2026-10-15.md"]["type"] == "journal"
    assert notes["taches/T-001-migration-referentiel-articles.md"]["alias"] == "T-001"
    assert index.resolve_link("T-001") == "taches/T-001-migration-referentiel-articles.md"
    assert index.resolve_link("2026-10-15-copil-sap") == "reunions/2026/2026-10-15-copil-sap.md"

    backlinks = {n["path"] for n in index.backlinks("taches/T-001-migration-referentiel-articles.md")}
    assert "reunions/2026/2026-10-15-copil-sap.md" in backlinks
    assert "sujets/mapping-unites-de-mesure.md" in backlinks

    results = index.search("rejets")
    assert any(r["path"] == "journal/2026/2026-10-16.md" for r in results)
    # recherche insensible aux accents
    assert index.search("echanges") or index.search("reunion")


def test_index_week_filter(index):
    paths = [n["path"] for n in index.list_notes(type="journal", start="2026-10-15", end="2026-10-16")]
    assert paths == ["journal/2026/2026-10-15.md", "journal/2026/2026-10-16.md"]
