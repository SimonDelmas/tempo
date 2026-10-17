import shutil
from pathlib import Path

import pytest

from tempo.config import PROJECT_ROOT, Settings
from tempo.index import Index
from tempo.journal import JournalService
from tempo.tasks import TaskService
from tempo.vault import Vault


@pytest.fixture
def vault_dir(tmp_path: Path) -> Path:
    target = tmp_path / "vault"
    shutil.copytree(PROJECT_ROOT / "vault-exemple", target)
    return target


@pytest.fixture
def settings(vault_dir: Path) -> Settings:
    return Settings(vault=vault_dir, watch=False)


@pytest.fixture
def vault(vault_dir: Path) -> Vault:
    return Vault(vault_dir)


@pytest.fixture
def index(vault: Vault) -> Index:
    idx = Index(vault, vault.root / ".tempo" / "index.db")
    idx.rebuild()
    return idx


@pytest.fixture
def tasks(vault: Vault, index: Index) -> TaskService:
    return TaskService(vault, index)


@pytest.fixture
def journal(vault: Vault, index: Index) -> JournalService:
    return JournalService(vault, index)
