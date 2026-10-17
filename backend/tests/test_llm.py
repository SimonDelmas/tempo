import json

import httpx
from openai import OpenAI
from pydantic import BaseModel

from tempo.llm import OpenAICompatibleLLM, ensure_json_hint


class Answer(BaseModel):
    ok: bool


def test_ensure_json_hint_adds_lowercase_json():
    messages = [{"role": "system", "content": "Réponds en JSON"}]
    result = ensure_json_hint(messages)
    assert len(result) == 2 and "json" in result[-1]["content"]
    already = [{"role": "user", "content": "un objet json"}]
    assert ensure_json_hint(already) is already


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
    result = llm.chat_json([{"role": "user", "content": "Réponds en JSON"}], model="m", schema=Answer)

    assert result.ok is True
    body = sent[0]
    assert body["response_format"] == {"type": "json_object"}
    assert any("json" in m["content"] for m in body["messages"])
