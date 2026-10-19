import os
from dataclasses import dataclass
from pathlib import Path

from dotenv import load_dotenv

PROJECT_ROOT = Path(__file__).resolve().parents[2]


@dataclass
class Settings:
    vault: Path
    llm_base_url: str | None = None
    llm_api_key: str | None = None
    llm_ca_bundle: str | None = None
    model_fast: str = "gpt-luna"
    model_smart: str = "claude-opus"
    # embeddings (recherche sémantique) : API compatible OpenAI, désactivés sans modèle
    embed_model: str | None = None
    embed_base_url: str | None = None
    embed_api_key: str | None = None
    host: str = "127.0.0.1"
    port: int = 8765
    watch: bool = True

    @property
    def llm_enabled(self) -> bool:
        return bool(self.llm_base_url and self.llm_api_key)

    @property
    def embed_enabled(self) -> bool:
        return bool(self.embed_model and self.embed_base_url and self.embed_api_key)


def load_settings() -> Settings:
    load_dotenv(PROJECT_ROOT / ".env")
    vault = os.getenv("TEMPO_VAULT") or str(PROJECT_ROOT / "vault-exemple")
    return Settings(
        vault=Path(vault).expanduser().resolve(),
        llm_base_url=os.getenv("VIBE_BASE_URL") or None,
        llm_api_key=os.getenv("VIBE_API_KEY") or None,
        llm_ca_bundle=os.getenv("VIBE_CA_BUNDLE") or None,
        model_fast=os.getenv("TEMPO_MODEL_FAST", "gpt-luna"),
        model_smart=os.getenv("TEMPO_MODEL_SMART", "claude-opus"),
        embed_model=os.getenv("TEMPO_EMBED_MODEL") or None,
        embed_base_url=os.getenv("TEMPO_EMBED_BASE_URL") or os.getenv("VIBE_BASE_URL") or None,
        embed_api_key=os.getenv("TEMPO_EMBED_API_KEY") or os.getenv("VIBE_API_KEY") or None,
        host=os.getenv("TEMPO_HOST", "127.0.0.1"),
        port=int(os.getenv("TEMPO_PORT", "8765")),
    )
