"""Client LLM : API compatible OpenAI (Vibe) + implémentation factice pour les tests."""

import json
import re
from collections.abc import Callable, Iterator

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


JSON_HINT = "Réponds uniquement avec un objet json valide."


def ensure_json_hint(messages: list[dict]) -> list[dict]:
    """Les modèles OpenAI refusent `json_object` si les messages d'entrée ne contiennent pas
    « json » en minuscules. Vibe passe par l'API Responses, où le message système devient
    `instructions` et ne compte pas : la consigne doit donc figurer dans un message utilisateur."""
    if any("json" in str(m.get("content", "")) for m in messages if m.get("role") != "system"):
        return messages
    result = list(messages)
    last_user = max((i for i, m in enumerate(result) if m.get("role") == "user"), default=None)
    if last_user is None:
        return [*result, {"role": "user", "content": JSON_HINT}]
    message = result[last_user]
    result[last_user] = {**message, "content": f"{message['content']}\n\n{JSON_HINT}"}
    return result


def strip_fences(text: str) -> str:
    match = re.fullmatch(r"\s*```(?:markdown|md)?\s*\n(.*?)\n```\s*", text, re.S)
    return match.group(1) if match else text.strip()


class LLM:
    def chat(self, messages: list[dict], *, model: str, json_mode: bool = False,
             temperature: float = 0.2) -> str:
        raise NotImplementedError

    def chat_stream(self, messages: list[dict], *, model: str, json_mode: bool = False,
                    temperature: float = 0.2) -> Iterator[str]:
        """Réponse par morceaux ; par défaut, un seul morceau."""
        yield self.chat(messages, model=model, json_mode=json_mode, temperature=temperature)

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
        if json_mode:
            messages = ensure_json_hint(messages)
        try:
            response = self.client.chat.completions.create(
                model=model, messages=messages, temperature=temperature, **kwargs)
        except OpenAIError as exc:
            raise LLMError(f"Erreur de l'API LLM : {exc}") from exc
        return response.choices[0].message.content or ""

    def chat_stream(self, messages, *, model, json_mode=False, temperature=0.2):
        from openai import OpenAIError

        kwargs = {"response_format": {"type": "json_object"}} if json_mode else {}
        if json_mode:
            messages = ensure_json_hint(messages)
        try:
            stream = self.client.chat.completions.create(
                model=model, messages=messages, temperature=temperature, stream=True, **kwargs)
            for chunk in stream:
                if chunk.choices and chunk.choices[0].delta.content:
                    yield chunk.choices[0].delta.content
        except OpenAIError as exc:
            raise LLMError(f"Erreur de l'API LLM : {exc}") from exc


class FakeLLM(LLM):
    """Renvoie des réponses prédéfinies (liste ou fonction) et mémorise les appels.
    Avec `chunk`, `chat_stream` découpe chaque réponse en morceaux de cette taille."""

    def __init__(self, responses: list[str] | Callable[[list[dict], str], str], chunk: int = 0):
        self.responses = responses
        self.chunk = chunk
        self.calls: list[dict] = []

    def chat(self, messages, *, model, json_mode=False, temperature=0.2):
        self.calls.append({"messages": messages, "model": model, "json_mode": json_mode})
        if callable(self.responses):
            return self.responses(messages, model)
        return self.responses.pop(0)

    def chat_stream(self, messages, *, model, json_mode=False, temperature=0.2):
        text = self.chat(messages, model=model, json_mode=json_mode, temperature=temperature)
        size = self.chunk or len(text) or 1
        for i in range(0, len(text), size):
            yield text[i:i + size]


class DisabledLLM(LLM):
    def chat(self, messages, *, model, json_mode=False, temperature=0.2):
        raise LLMNotConfigured()


def build_llm(settings: Settings) -> LLM:
    if not settings.llm_enabled:
        return DisabledLLM()
    assert settings.llm_base_url and settings.llm_api_key
    return OpenAICompatibleLLM(settings.llm_base_url, settings.llm_api_key, settings.llm_ca_bundle)
