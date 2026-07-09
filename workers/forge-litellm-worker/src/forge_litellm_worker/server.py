"""NDJSON wire server for Forge ↔ LiteLLM SDK.

stdout is reserved exclusively for NDJSON frames. LiteLLM (and any other library)
must not write to stdout — they pollute the protocol (blank lines / ANSI banners)
and break the Rust reader with errors like:
  protocol: EOF while parsing a value at line 1 column 0
"""

from __future__ import annotations

import json
import os
import sys
import traceback
from typing import Any, TextIO

from forge_litellm_worker.normalize import complete_result_from_litellm

WIRE_V = 1

# Real process streams — never rebind these for protocol I/O.
_PROTO_OUT: TextIO = sys.stdout
_PROTO_ERR: TextIO = sys.stderr


class _StderrOnly:
    """File-like that sends all writes to stderr (used to replace sys.stdout)."""

    def write(self, s: str) -> int:  # noqa: D401
        if not s:
            return 0
        return _PROTO_ERR.write(s)

    def flush(self) -> None:
        _PROTO_ERR.flush()

    def isatty(self) -> bool:
        return False

    def fileno(self) -> int:
        return _PROTO_ERR.fileno()


def _lock_stdout() -> None:
    """Redirect sys.stdout so accidental prints never corrupt the wire."""
    sys.stdout = _StderrOnly()  # type: ignore[assignment]


def emit(obj: dict) -> None:
    _PROTO_OUT.write(json.dumps(obj, default=str) + "\n")
    _PROTO_OUT.flush()


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


def _configure_litellm(litellm: Any) -> None:
    """Quiet LiteLLM banners that otherwise go to stdout."""
    try:
        litellm.suppress_debug_info = True
    except Exception:
        pass
    try:
        litellm.set_verbose = False
    except Exception:
        pass
    # Drop success logs if present
    for attr in ("success_callback", "failure_callback", "_async_success_callback"):
        try:
            setattr(litellm, attr, [])
        except Exception:
            pass


def handle_ping(mid: str) -> None:
    py_ver = f"{sys.version_info.major}.{sys.version_info.minor}.{sys.version_info.micro}"
    litellm_ver = "unavailable"
    try:
        import litellm

        _configure_litellm(litellm)
        litellm_ver = getattr(litellm, "__version__", None) or getattr(
            litellm, "version", "unknown"
        )
        if not isinstance(litellm_ver, str):
            litellm_ver = "unknown"
    except Exception:
        pass
    respond(
        mid,
        "ping",
        {"ok": True, "python_version": py_ver, "litellm_version": litellm_ver},
    )


def _credential_hint(model: str) -> str | None:
    """Detect missing / fixture credentials before calling upstream."""
    m = (model or "").lower()
    if m.startswith("xai/") or m.startswith("grok"):
        key = os.environ.get("XAI_API_KEY") or os.environ.get("GROK_CODE_XAI_API_KEY")
        if not key or not key.strip():
            return (
                "No XAI_API_KEY in worker env. Run `/connect xai` and finish browser/device "
                "login (not fixture), or export a real key from console.x.ai."
            )
        if key.strip().startswith("fixture-") or key.strip() == "fixture-access-token":
            return (
                "XAI_API_KEY is a Forge fixture token, not a real xAI credential. "
                "Run `forge connect xai` (or /connect xai), complete OAuth, then retry. "
                "Unset FORGE_CONNECT_OAUTH_FIXTURE if set."
            )
    if m.startswith("openai/"):
        if not (os.environ.get("OPENAI_API_KEY") or "").strip():
            return "OPENAI_API_KEY is not set for this worker."
    if m.startswith("anthropic/"):
        if not (os.environ.get("ANTHROPIC_API_KEY") or "").strip():
            return "ANTHROPIC_API_KEY is not set for this worker."
    return None


def _completion_kwargs(params: dict[str, Any], *, stream: bool) -> dict[str, Any]:
    model = params.get("model") or ""
    messages = params.get("messages") or []
    tools = params.get("tools") or None
    kwargs: dict[str, Any] = {
        "model": model,
        "messages": messages,
        "stream": stream,
    }
    if tools:
        kwargs["tools"] = tools
    if params.get("temperature") is not None:
        kwargs["temperature"] = params["temperature"]
    if params.get("max_tokens") is not None:
        kwargs["max_tokens"] = params["max_tokens"]
    extra = params.get("extra") or {}
    if isinstance(extra, dict):
        # Don't let caller force stream off when we need it
        extra = {k: v for k, v in extra.items() if k != "stream"}
        kwargs.update(extra)
    return kwargs


def _map_upstream_error(e: Exception) -> tuple[str, str]:
    msg = str(e)
    code = "upstream"
    low = msg.lower()
    if "auth" in low or "401" in low or "api key" in low or "unauthenticated" in low:
        code = "upstream_auth"
    elif "429" in low or "rate" in low:
        code = "upstream_rate_limit"
    return code, msg


def _emit_text_delta(mid: str, text: str) -> None:
    if not text:
        return
    emit(
        {
            "v": WIRE_V,
            "id": mid,
            "type": "event",
            "params": {"kind": "text_delta", "text": text},
        }
    )


def _chunk_text(chunk: Any) -> str:
    """Extract assistant text delta from an OpenAI-style stream chunk."""
    try:
        if isinstance(chunk, dict):
            choices = chunk.get("choices") or []
        else:
            choices = getattr(chunk, "choices", None) or []
        if not choices:
            return ""
        c0 = choices[0]
        if isinstance(c0, dict):
            delta = c0.get("delta") or {}
            if isinstance(delta, dict):
                content = delta.get("content")
            else:
                content = getattr(delta, "content", None)
        else:
            delta = getattr(c0, "delta", None)
            content = getattr(delta, "content", None) if delta is not None else None
        if content is None:
            return ""
        return content if isinstance(content, str) else str(content)
    except Exception:
        return ""


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

    _configure_litellm(litellm)

    model = params.get("model") or ""
    hint = _credential_hint(str(model))
    if hint:
        fail(mid, "upstream_auth", hint)
        return

    kwargs = _completion_kwargs(params, stream=False)

    try:
        prev = sys.stdout
        sys.stdout = _StderrOnly()  # type: ignore[assignment]
        try:
            resp = litellm.completion(**kwargs)
        finally:
            sys.stdout = prev
        result = complete_result_from_litellm(resp)
        respond(mid, "complete", result)
    except Exception as e:  # noqa: BLE001
        code, msg = _map_upstream_error(e)
        fail(mid, code, msg)


def handle_complete_stream(mid: str, params: dict[str, Any]) -> None:
    """Stream text_delta events, then a final complete_stream response."""
    try:
        import litellm
    except ImportError:
        fail(
            mid,
            "internal",
            "litellm package not installed; pip install litellm",
        )
        return

    _configure_litellm(litellm)

    model = params.get("model") or ""
    hint = _credential_hint(str(model))
    if hint:
        fail(mid, "upstream_auth", hint)
        return

    kwargs = _completion_kwargs(params, stream=True)
    # Prefer including usage on final chunk when provider supports it
    kwargs.setdefault("stream_options", {"include_usage": True})

    text_parts: list[str] = []
    finish_reason: str | None = None
    usage_out: dict[str, Any] | None = None

    try:
        prev = sys.stdout
        sys.stdout = _StderrOnly()  # type: ignore[assignment]
        try:
            stream = litellm.completion(**kwargs)
            for chunk in stream:
                piece = _chunk_text(chunk)
                if piece:
                    text_parts.append(piece)
                    # emit() always writes to _PROTO_OUT (real stdout), safe while
                    # sys.stdout is redirected for LiteLLM noise.
                    _emit_text_delta(mid, piece)

                # finish_reason / usage from last chunks
                try:
                    if isinstance(chunk, dict):
                        choices = chunk.get("choices") or []
                        if choices:
                            fr = choices[0].get("finish_reason")
                            if fr:
                                finish_reason = fr
                        u = chunk.get("usage")
                    else:
                        choices = getattr(chunk, "choices", None) or []
                        if choices:
                            fr = getattr(choices[0], "finish_reason", None)
                            if fr:
                                finish_reason = str(fr)
                        u = getattr(chunk, "usage", None)
                    if u:
                        if isinstance(u, dict):
                            usage_out = {
                                "prompt_tokens": u.get("prompt_tokens") or 0,
                                "completion_tokens": u.get("completion_tokens") or 0,
                                "total_tokens": u.get("total_tokens")
                                or (
                                    (u.get("prompt_tokens") or 0)
                                    + (u.get("completion_tokens") or 0)
                                ),
                            }
                        else:
                            usage_out = {
                                "prompt_tokens": getattr(u, "prompt_tokens", 0) or 0,
                                "completion_tokens": getattr(u, "completion_tokens", 0)
                                or 0,
                                "total_tokens": getattr(u, "total_tokens", 0) or 0,
                            }
                except Exception:
                    pass
        finally:
            sys.stdout = prev

        full_text = "".join(text_parts)
        result: dict[str, Any] = {
            "text": full_text,
            "tool_calls": [],
            "usage": usage_out,
            "finish_reason": finish_reason or "stop",
            "ok": True,
        }
        respond(mid, "complete_stream", result)
    except Exception as e:  # noqa: BLE001
        code, msg = _map_upstream_error(e)
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
        handle_complete_stream(mid, params if isinstance(params, dict) else {})
    else:
        fail(mid, "protocol", f"unknown method {method}")
    return True


def run() -> None:
    _lock_stdout()
    for line in sys.stdin:
        try:
            if not handle_line(line):
                break
        except Exception:  # noqa: BLE001
            traceback.print_exc(file=_PROTO_ERR)
            fail("0", "internal", "unhandled worker exception")


def main() -> None:
    run()
