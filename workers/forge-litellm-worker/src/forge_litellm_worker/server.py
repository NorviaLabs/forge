"""NDJSON wire server for Forge ↔ LiteLLM SDK."""

from __future__ import annotations

import json
import sys
import traceback
from typing import Any

from forge_litellm_worker.normalize import complete_result_from_litellm

WIRE_V = 1


def emit(obj: dict) -> None:
    sys.stdout.write(json.dumps(obj, default=str) + "\n")
    sys.stdout.flush()


def respond(mid: str, method: str, result: dict) -> None:
    emit({"v": WIRE_V, "id": mid, "type": "response", "method": method, "result": result})


def fail(mid: str, code: str, message: str) -> None:
    emit(
        {
            "v": WIRE_V,
            "id": mid,
            "type": "error",
            "error": {"code": code, "message": message},
        }
    )


def handle_ping(mid: str) -> None:
    py_ver = f"{sys.version_info.major}.{sys.version_info.minor}.{sys.version_info.micro}"
    litellm_ver = "unavailable"
    try:
        import litellm

        litellm_ver = getattr(litellm, "__version__", "unknown")
    except Exception:
        pass
    respond(
        mid,
        "ping",
        {"ok": True, "python_version": py_ver, "litellm_version": litellm_ver},
    )


def handle_complete(mid: str, params: dict[str, Any]) -> None:
    try:
        import litellm
    except ImportError:
        fail(
            mid,
            "internal",
            "litellm package not installed; pip install litellm",
        )
        return

    model = params.get("model") or ""
    messages = params.get("messages") or []
    tools = params.get("tools") or None
    kwargs: dict[str, Any] = {
        "model": model,
        "messages": messages,
    }
    if tools:
        kwargs["tools"] = tools
    if params.get("temperature") is not None:
        kwargs["temperature"] = params["temperature"]
    if params.get("max_tokens") is not None:
        kwargs["max_tokens"] = params["max_tokens"]
    extra = params.get("extra") or {}
    if isinstance(extra, dict):
        kwargs.update(extra)

    try:
        resp = litellm.completion(**kwargs)
        result = complete_result_from_litellm(resp)
        respond(mid, "complete", result)
    except Exception as e:  # noqa: BLE001 — map to wire error
        msg = str(e)
        code = "upstream"
        low = msg.lower()
        if "auth" in low or "401" in low or "api key" in low:
            code = "upstream_auth"
        elif "429" in low or "rate" in low:
            code = "upstream_rate_limit"
        fail(mid, code, msg)


def handle_line(line: str) -> bool:
    """Return False to stop the server."""
    line = line.strip()
    if not line:
        return True
    try:
        msg = json.loads(line)
    except json.JSONDecodeError as e:
        fail("0", "protocol", f"invalid json: {e}")
        return True

    mid = str(msg.get("id") or "0")
    if msg.get("v") not in (None, WIRE_V, 1):
        fail(mid, "protocol", f"unsupported wire version {msg.get('v')}")
        return True

    method = msg.get("method")
    params = msg.get("params") or {}

    if method == "ping":
        handle_ping(mid)
    elif method == "shutdown":
        respond(mid, "shutdown", {"ok": True})
        return False
    elif method == "complete":
        handle_complete(mid, params if isinstance(params, dict) else {})
    elif method == "complete_stream":
        # v1: fall back to non-streaming complete
        handle_complete(mid, params if isinstance(params, dict) else {})
    else:
        fail(mid, "protocol", f"unknown method {method}")
    return True


def run() -> None:
    for line in sys.stdin:
        try:
            if not handle_line(line):
                break
        except Exception:  # noqa: BLE001
            traceback.print_exc(file=sys.stderr)
            fail("0", "internal", "unhandled worker exception")


def main() -> None:
    run()
