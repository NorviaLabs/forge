"""ChatGPT-authenticated Codex Responses transport using Forge-owned OAuth."""

from __future__ import annotations

import hashlib
import json
import os
import re
import urllib.error
import urllib.request
import uuid
from typing import Any, Callable

from forge_litellm_worker.effort import codex_effort

CODEX_URL = "https://chatgpt.com/backend-api/codex/responses"


def is_codex_model(model: str) -> bool:
    return (model or "").lower().startswith("openai-codex/")


def _credentials() -> tuple[str, str]:
    access_token = (os.environ.get("FORGE_CODEX_ACCESS_TOKEN") or "").strip()
    account_id = (os.environ.get("FORGE_CODEX_ACCOUNT_ID") or "").strip()
    if not access_token or not account_id:
        raise RuntimeError("No Forge ChatGPT session found. Run `/connect openai_codex`.")
    return access_token, account_id


def _content_text(content: Any) -> str:
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return "".join(
            str(item.get("text") or item.get("content") or "")
            for item in content
            if isinstance(item, dict)
        )
    return "" if content is None else str(content)


_VALID_TOOL_NAME = re.compile(r"^[A-Za-z0-9_-]{1,64}$")


def _tool_name_maps(
    tools: list[dict[str, Any]],
) -> tuple[dict[str, str], dict[str, str]]:
    """Create stable, collision-resistant aliases accepted by Responses API."""
    original_to_alias: dict[str, str] = {}
    alias_to_original: dict[str, str] = {}
    for tool in tools:
        function = tool.get("function") or tool
        original = str(function.get("name") or "")
        if not original:
            continue
        if _VALID_TOOL_NAME.fullmatch(original):
            alias = original
        else:
            stem = re.sub(r"[^A-Za-z0-9_-]", "_", original).strip("_") or "tool"
            digest = hashlib.sha256(original.encode("utf-8")).hexdigest()[:8]
            alias = f"{stem[:55]}_{digest}"
        # A valid original can still collide with a generated alias.
        if alias in alias_to_original and alias_to_original[alias] != original:
            digest = hashlib.sha256(original.encode("utf-8")).hexdigest()[:12]
            alias = f"{alias[:51]}_{digest}"
        original_to_alias[original] = alias
        alias_to_original[alias] = original
    return original_to_alias, alias_to_original


def _input_items(
    messages: list[dict[str, Any]], tool_aliases: dict[str, str]
) -> tuple[str, list[dict[str, Any]]]:
    instructions: list[str] = []
    items: list[dict[str, Any]] = []
    for message in messages:
        role = str(message.get("role") or "user")
        text = _content_text(message.get("content"))
        if role in ("system", "developer"):
            if text:
                instructions.append(text)
            continue
        if role == "tool":
            items.append(
                {
                    "type": "function_call_output",
                    "call_id": str(message.get("tool_call_id") or ""),
                    "output": text,
                }
            )
            continue
        if text:
            content_type = "output_text" if role == "assistant" else "input_text"
            items.append(
                {
                    "type": "message",
                    "role": "assistant" if role == "assistant" else "user",
                    "content": [{"type": content_type, "text": text}],
                }
            )
        if role == "assistant":
            for call in message.get("tool_calls") or []:
                function = call.get("function") or {}
                arguments = function.get("arguments") or "{}"
                if not isinstance(arguments, str):
                    arguments = json.dumps(arguments)
                items.append(
                    {
                        "type": "function_call",
                        "call_id": str(call.get("id") or ""),
                        "name": tool_aliases.get(
                            str(function.get("name") or ""),
                            str(function.get("name") or ""),
                        ),
                        "arguments": arguments,
                    }
                )
    return "\n\n".join(instructions) or "You are a helpful coding assistant.", items


def _tools(
    tools: list[dict[str, Any]], tool_aliases: dict[str, str]
) -> list[dict[str, Any]]:
    out: list[dict[str, Any]] = []
    for tool in tools:
        function = tool.get("function") or tool
        name = function.get("name")
        if not name:
            continue
        out.append(
            {
                "type": "function",
                "name": tool_aliases[str(name)],
                "description": function.get("description") or "",
                "parameters": function.get("parameters") or {"type": "object", "properties": {}},
            }
        )
    return out


def _request_body(
    params: dict[str, Any], tool_aliases: dict[str, str] | None = None
) -> dict[str, Any]:
    model = str(params.get("model") or "").split("/", 1)[-1]
    raw_tools = params.get("tools") or []
    if tool_aliases is None:
        tool_aliases, _ = _tool_name_maps(raw_tools)
    instructions, items = _input_items(params.get("messages") or [], tool_aliases)
    body: dict[str, Any] = {
        "model": model,
        "store": False,
        "stream": True,
        "instructions": instructions,
        "input": items,
        "include": ["reasoning.encrypted_content"],
        "tool_choice": "auto",
        "parallel_tool_calls": True,
        "text": {"verbosity": "low"},
    }
    tools = _tools(raw_tools, tool_aliases)
    if tools:
        body["tools"] = tools
    effort = codex_effort(params.get("extra"))
    if effort:
        body["reasoning"] = {"effort": effort, "summary": "auto"}
    return body


def complete_stream(
    params: dict[str, Any],
    on_text: Callable[[str], None],
    on_thinking: Callable[[str], None],
) -> dict[str, Any]:
    token, account_id = _credentials()
    tool_aliases, original_tool_names = _tool_name_maps(params.get("tools") or [])
    request_id = str(uuid.uuid4())
    request = urllib.request.Request(
        CODEX_URL,
        data=json.dumps(_request_body(params, tool_aliases)).encode("utf-8"),
        method="POST",
        headers={
            "Authorization": f"Bearer {token}",
            "chatgpt-account-id": account_id,
            "originator": "forge",
            "OpenAI-Beta": "responses=experimental",
            "accept": "text/event-stream",
            "content-type": "application/json",
            "session-id": request_id,
            "x-client-request-id": request_id,
            "User-Agent": "forge",
        },
    )
    text_parts: list[str] = []
    thinking_parts: list[str] = []
    tool_calls: list[dict[str, Any]] = []
    usage: dict[str, Any] | None = None
    finish_reason = "stop"
    try:
        response = urllib.request.urlopen(request, timeout=300)
        for raw_line in response:
            line = raw_line.decode("utf-8", errors="replace").strip()
            if not line.startswith("data:"):
                continue
            payload = line[5:].strip()
            if not payload or payload == "[DONE]":
                continue
            event = json.loads(payload)
            kind = event.get("type")
            if kind == "response.output_text.delta":
                delta = str(event.get("delta") or "")
                text_parts.append(delta)
                on_text(delta)
            elif kind in ("response.reasoning_summary_text.delta", "response.reasoning_text.delta"):
                delta = str(event.get("delta") or "")
                thinking_parts.append(delta)
                on_thinking(delta)
            elif kind == "response.output_item.done":
                item = event.get("item") or {}
                if item.get("type") == "function_call":
                    arguments = item.get("arguments") or "{}"
                    try:
                        parsed_arguments = json.loads(arguments) if isinstance(arguments, str) else arguments
                    except Exception:
                        parsed_arguments = {"_raw": arguments}
                    tool_calls.append(
                        {
                            "id": item.get("call_id") or item.get("id") or "",
                            "name": original_tool_names.get(
                                str(item.get("name") or ""),
                                str(item.get("name") or ""),
                            ),
                            "arguments": parsed_arguments,
                        }
                    )
            elif kind == "response.completed":
                completed = event.get("response") or {}
                raw_usage = completed.get("usage") or {}
                if raw_usage:
                    prompt = raw_usage.get("input_tokens") or 0
                    completion = raw_usage.get("output_tokens") or 0
                    usage = {
                        "prompt_tokens": prompt,
                        "completion_tokens": completion,
                        "total_tokens": raw_usage.get("total_tokens") or prompt + completion,
                    }
                finish_reason = completed.get("status") or finish_reason
            elif kind in ("error", "response.failed"):
                detail = event.get("error") or event.get("response") or event
                raise RuntimeError(json.dumps(detail, default=str))
    except urllib.error.HTTPError as exc:
        detail = exc.read(4096).decode("utf-8", errors="replace")
        if exc.code == 401:
            raise RuntimeError(
                "Forge's Codex login expired. Run `/connect openai_codex` again."
            ) from exc
        raise RuntimeError(f"Codex subscription request failed (HTTP {exc.code}): {detail}") from exc

    return {
        "text": "".join(text_parts),
        "thinking": "".join(thinking_parts) or None,
        "tool_calls": tool_calls,
        "usage": usage,
        "finish_reason": finish_reason,
        "ok": True,
    }
