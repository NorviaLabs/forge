"""Map LiteLLM responses to Forge wire result shape."""

from __future__ import annotations

import json
from typing import Any


def content_to_text(content: Any) -> str:
    if content is None:
        return ""
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        parts: list[str] = []
        for p in content:
            if isinstance(p, dict) and p.get("type") == "text":
                parts.append(str(p.get("text") or ""))
            elif isinstance(p, dict) and "text" in p:
                parts.append(str(p["text"]))
        return "".join(parts)
    return str(content)


def parse_arguments(raw: Any) -> dict:
    if raw is None:
        return {}
    if isinstance(raw, dict):
        return raw
    if isinstance(raw, str):
        if not raw.strip():
            return {}
        return json.loads(raw)
    return {}


def complete_result_from_litellm(resp: Any) -> dict:
    """Normalize litellm ModelResponse-like object or dict to Forge result."""
    if isinstance(resp, dict):
        data = resp
    else:
        # litellm ModelResponse
        data = resp.model_dump() if hasattr(resp, "model_dump") else dict(resp)

    # Already forge-shaped
    if "text" in data and "choices" not in data:
        return {
            "text": data.get("text") or "",
            "tool_calls": data.get("tool_calls") or [],
            "usage": data.get("usage"),
            "finish_reason": data.get("finish_reason"),
        }

    choices = data.get("choices") or []
    if not choices:
        raise ValueError("missing choices")
    message = choices[0].get("message") or {}
    text = content_to_text(message.get("content"))
    tool_calls = []
    for i, tc in enumerate(message.get("tool_calls") or []):
        fn = tc.get("function") or {}
        tool_calls.append(
            {
                "id": tc.get("id") or f"call_{i}",
                "name": fn.get("name") or "",
                "arguments": parse_arguments(fn.get("arguments")),
            }
        )
    usage = data.get("usage")
    usage_out = None
    if usage:
        if isinstance(usage, dict):
            usage_out = {
                "prompt_tokens": usage.get("prompt_tokens") or 0,
                "completion_tokens": usage.get("completion_tokens") or 0,
                "total_tokens": usage.get("total_tokens")
                or ((usage.get("prompt_tokens") or 0) + (usage.get("completion_tokens") or 0)),
            }
        else:
            usage_out = {
                "prompt_tokens": getattr(usage, "prompt_tokens", 0) or 0,
                "completion_tokens": getattr(usage, "completion_tokens", 0) or 0,
                "total_tokens": getattr(usage, "total_tokens", 0) or 0,
            }
    finish = choices[0].get("finish_reason")
    return {
        "text": text,
        "tool_calls": tool_calls,
        "usage": usage_out,
        "finish_reason": finish,
    }
