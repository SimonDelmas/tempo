import json

import httpx
from openai import OpenAI
from pydantic import BaseModel

from tempo.llm import OpenAICompatibleLLM, ensure_json_hint


class Answer(BaseModel):
    ok: bool


def test_ensure_json_hint_targets_user_message():
    # le message système ne compte pas (il devient `instructions` dans l'API Responses)
    messages = [{"role": "system", "content": "un objet json"},
                {"role": "user", "content": "Résumé à traiter"}]
    result = ensure_json_hint(messages)
    assert result[0] == messages[0]
    assert result[1]["role"] == "user" and "json" in result[1]["content"]
    assert messages[1]["content"] == "Résumé à traiter"  # pas de mutation
    already = [{"role": "user", "content": "un objet json"}]
    assert ensure_json_hint(already) is already
    assert ensure_json_hint([{"role": "system", "content": "x"}])[-1]["role"] == "user"


def test_json_mode_request_contains_json_word():
    sent = []

    def handler(request: httpx.Request) -> httpx.Response:
        sent.append(json.loads(request.content))
        return httpx.Response(200, json={
            "id": "x", "object": "chat.completion", "created": 0, "model": "m",
            "choices": [{"index": 0, "finish_reason": "stop",
                         "message": {"role": "assistant", "content": '{"ok": true}'}}]})

    llm = OpenAICompatibleLLM("http://vibe.test/v1", "key")
    llm.client = OpenAI(base_url="http://vibe.test/v1", api_key="key",
                        http_client=httpx.Client(transport=httpx.MockTransport(handler)))
    result = llm.chat_json([{"role": "system", "content": "Réponds en objet json"},
                            {"role": "user", "content": "Résumé"}], model="m", schema=Answer)

    assert result.ok is True
    body = sent[0]
    assert body["response_format"] == {"type": "json_object"}
    assert any("json" in m["content"] for m in body["messages"] if m["role"] == "user")
