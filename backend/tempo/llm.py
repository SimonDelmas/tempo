"""Client LLM : API compatible OpenAI (Vibe) + implémentation factice pour les tests."""

import json
import re
from collections.abc import Callable

from pydantic import BaseModel, ValidationError

from .config import Settings


class LLMError(Exception):
    pass


class LLMNotConfigured(LLMError):
    def __init__(self):
        super().__init__("API LLM non configurée : renseignez VIBE_BASE_URL et VIBE_API_KEY dans .env")


def extract_json(raw: str) -> str:
    """Tolère les réponses entourées de ```json … ``` ou de texte parasite."""
    raw = raw.strip()
    fenced = re.search(r"```(?:json)?\s*(.*?)```", raw, re.S)
    if fenced:
        raw = fenced.group(1).strip()
    start, end = raw.find("{"), raw.rfind("}")
    return raw[start:end + 1] if start != -1 and end > start else raw


def strip_fences(text: str) -> str:
    match = re.fullmatch(r"\s*```(?:markdown|md)?\s*\n(.*?)\n```\s*", text, re.S)
    return match.group(1) if match else text.strip()


class LLM:
    def chat(self, messages: list[dict], *, model: str, json_mode: bool = False,
             temperature: float = 0.2) -> str:
        raise NotImplementedError

    def chat_json[T: BaseModel](self, messages: list[dict], *, model: str, schema: type[T],
                                retries: int = 1) -> T:
        msgs = list(messages)
        for attempt in range(retries + 1):
            raw = self.chat(msgs, model=model, json_mode=True)
            try:
                return schema.model_validate_json(extract_json(raw))
            except (ValidationError, json.JSONDecodeError) as exc:
                if attempt == retries:
                    raise LLMError(f"Réponse JSON invalide du modèle : {exc}") from exc
                msgs += [
                    {"role": "assistant", "content": raw},
                    {"role": "user", "content": "Ta réponse ne respecte pas le schéma attendu "
                     f"({exc}). Réponds uniquement avec le JSON corrigé."},
                ]
        raise AssertionError("unreachable")


class OpenAICompatibleLLM(LLM):
    def __init__(self, base_url: str, api_key: str, ca_bundle: str | None = None,
                 timeout: float = 180):
        import httpx
        from openai import OpenAI

        http_client = httpx.Client(verify=ca_bundle) if ca_bundle else None
        self.client = OpenAI(base_url=base_url, api_key=api_key, timeout=timeout,
                             http_client=http_client)

    def chat(self, messages, *, model, json_mode=False, temperature=0.2):
        from openai import OpenAIError

        kwargs = {"response_format": {"type": "json_object"}} if json_mode else {}
        try:
            response = self.client.chat.completions.create(
                model=model, messages=messages, temperature=temperature, **kwargs)
        except OpenAIError as exc:
            raise LLMError(f"Erreur de l'API LLM : {exc}") from exc
        return response.choices[0].message.content or ""


class FakeLLM(LLM):
    """Renvoie des réponses prédéfinies (liste ou fonction) et mémorise les appels."""

    def __init__(self, responses: list[str] | Callable[[list[dict], str], str]):
        self.responses = responses
        self.calls: list[dict] = []

    def chat(self, messages, *, model, json_mode=False, temperature=0.2):
        self.calls.append({"messages": messages, "model": model, "json_mode": json_mode})
        if callable(self.responses):
            return self.responses(messages, model)
        return self.responses.pop(0)


class DisabledLLM(LLM):
    def chat(self, messages, *, model, json_mode=False, temperature=0.2):
        raise LLMNotConfigured()


def build_llm(settings: Settings) -> LLM:
    if not settings.llm_enabled:
        return DisabledLLM()
    assert settings.llm_base_url and settings.llm_api_key
    return OpenAICompatibleLLM(settings.llm_base_url, settings.llm_api_key, settings.llm_ca_bundle)
