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


def _opencode_key() -> str | None:
    key = (
        os.environ.get("OPENCODE_ZEN_API_KEY")
        or os.environ.get("OPENCODE_API_KEY")
        or os.environ.get("OPENCODE_GO_API_KEY")
        or ""
    ).strip()
    return key or None


def _opencode_go_base() -> str | None:
    base = (
        os.environ.get("OPENCODE_API_BASE")
        or os.environ.get("OPENCODE_GO_API_BASE")
        or ""
    ).strip()
    return base or None


def _opencode_zen_base() -> str | None:
    base = (os.environ.get("OPENCODE_ZEN_API_BASE") or "").strip()
    return base or None


def _is_opencode_go_model(model: str) -> bool:
    m = (model or "").lower()
    # Legacy bare `opencode/` prefix routes to Go.
    return m.startswith("opencode-go/") or (
        m.startswith("opencode/") and not m.startswith("opencode-zen/")
    )


def _is_opencode_zen_model(model: str) -> bool:
    return (model or "").lower().startswith("opencode-zen/")


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
    if _is_opencode_zen_model(m):
        key = _opencode_key()
        base = _opencode_zen_base()
        if not key:
            return (
                "No OPENCODE_API_KEY in worker env. Run `forge connect opencode_zen --key …` "
                "(or /connect opencode_zen), paste a key from https://opencode.ai/auth."
            )
        if not base:
            return (
                "OPENCODE_ZEN_API_BASE is not set. Re-run `forge connect opencode_zen` so Forge "
                "exports https://opencode.ai/zen/v1 for the LiteLLM worker."
            )
        if len(key) < 16:
            return (
                "OPENCODE_API_KEY looks too short. Get a real key from "
                "https://opencode.ai/auth and reconnect."
            )
        return None
    if _is_opencode_go_model(m):
        key = _opencode_key()
        base = _opencode_go_base()
        if not key:
            return (
                "No OPENCODE_API_KEY in worker env. Run `forge connect opencode_go --key …` "
                "(or /connect opencode_go), paste a key from https://opencode.ai/auth."
            )
        if not base:
            return (
                "OPENCODE_API_BASE is not set. Re-run `forge connect opencode_go` so Forge "
                "exports https://opencode.ai/zen/go/v1 for the LiteLLM worker."
            )
        if len(key) < 16:
            return (
                "OPENCODE_API_KEY looks too short. Get a real key from "
                "https://opencode.ai/auth and reconnect."
            )
        return None
    if m.startswith("openai/") and not _is_opencode_go_model(m) and not _is_opencode_zen_model(m):
        if not (os.environ.get("OPENAI_API_KEY") or "").strip():
            return (
                "OPENAI_API_KEY is not set for this worker. "
                "Run `forge connect openai --key …` (or /connect openai)."
            )
    if m.startswith("anthropic/"):
        if not (os.environ.get("ANTHROPIC_API_KEY") or "").strip():
            return (
                "ANTHROPIC_API_KEY is not set for this worker. "
                "Run `forge connect anthropic --key …` (or /connect anthropic)."
            )
    if m.startswith("ollama/") or m.startswith("ollama_chat/"):
        # Local server — key optional; base defaults to localhost.
        return None
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

    # OpenCode Zen / Go: OpenAI-compatible endpoints. Rewrite prefix → openai/<id>
    # and inject the matching api_base + key.
    oc_key = _opencode_key()
    if oc_key and _is_opencode_zen_model(model):
        mid = model.split("/", 1)[1] if "/" in model else model
        zen_base = _opencode_zen_base() or "https://opencode.ai/zen/v1"
        kwargs["model"] = f"openai/{mid}"
        kwargs["api_base"] = zen_base.rstrip("/")
        kwargs["api_key"] = oc_key
    elif oc_key and _is_opencode_go_model(model):
        mid = model.split("/", 1)[1] if "/" in model else model
        go_base = _opencode_go_base() or "https://opencode.ai/zen/go/v1"
        kwargs["model"] = f"openai/{mid}"
        kwargs["api_base"] = go_base.rstrip("/")
        kwargs["api_key"] = oc_key

    # Ollama: ensure api_base points at local daemon when OLLAMA_API_BASE is set.
    mlow = (model or "").lower()
    if mlow.startswith("ollama/") or mlow.startswith("ollama_chat/"):
        ollama_base = (os.environ.get("OLLAMA_API_BASE") or "").strip()
        if ollama_base and "api_base" not in kwargs:
            kwargs["api_base"] = ollama_base.rstrip("/")

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


def _emit_delta(mid: str, kind: str, text: str) -> None:
    if not text:
        return
    emit(
        {
            "v": WIRE_V,
            "id": mid,
            "type": "event",
            "params": {"kind": kind, "text": text},
        }
    )


def _emit_text_delta(mid: str, text: str) -> None:
    _emit_delta(mid, "text_delta", text)


def _emit_thinking_delta(mid: str, text: str) -> None:
    _emit_delta(mid, "thinking_delta", text)


def _delta_obj(chunk: Any) -> Any:
    try:
        if isinstance(chunk, dict):
            choices = chunk.get("choices") or []
        else:
            choices = getattr(chunk, "choices", None) or []
        if not choices:
            return None
        c0 = choices[0]
        if isinstance(c0, dict):
            return c0.get("delta") or {}
        return getattr(c0, "delta", None)
    except Exception:
        return None


def _chunk_text(chunk: Any) -> str:
    """Extract assistant text delta from an OpenAI-style stream chunk."""
    try:
        delta = _delta_obj(chunk)
        if delta is None:
            return ""
        if isinstance(delta, dict):
            content = delta.get("content")
        else:
            content = getattr(delta, "content", None)
        if content is None:
            return ""
        return content if isinstance(content, str) else str(content)
    except Exception:
        return ""


def _as_text(val: Any) -> str:
    if val is None:
        return ""
    if isinstance(val, str):
        return val
    return str(val)


def _chunk_thinking(chunk: Any) -> str:
    """Extract reasoning/thinking delta (Grok, DeepSeek-R1, o-series, Claude thinking)."""
    try:
        delta = _delta_obj(chunk)
        if delta is None:
            return ""
        # Common LiteLLM / provider fields on delta
        if isinstance(delta, dict):
            for key in (
                "reasoning_content",
                "reasoning",
                "thinking",
                "reasoning_text",
            ):
                t = _as_text(delta.get(key))
                if t:
                    return t
            # thinking_blocks: list of {type, thinking|text}
            blocks = delta.get("thinking_blocks")
            if isinstance(blocks, list):
                parts: list[str] = []
                for b in blocks:
                    if isinstance(b, dict):
                        parts.append(
                            _as_text(b.get("thinking") or b.get("text") or b.get("content"))
                        )
                    else:
                        parts.append(_as_text(getattr(b, "thinking", None) or getattr(b, "text", None)))
                return "".join(parts)
            # reasoning_details (some OpenAI-compatible shapes)
            details = delta.get("reasoning_details")
            if isinstance(details, list):
                parts = []
                for d in details:
                    if isinstance(d, dict):
                        parts.append(_as_text(d.get("text") or d.get("content")))
                return "".join(parts)
        else:
            for attr in ("reasoning_content", "reasoning", "thinking", "reasoning_text"):
                t = _as_text(getattr(delta, attr, None))
                if t:
                    return t
            blocks = getattr(delta, "thinking_blocks", None)
            if blocks:
                parts = []
                for b in blocks:
                    if isinstance(b, dict):
                        parts.append(_as_text(b.get("thinking") or b.get("text")))
                    else:
                        parts.append(
                            _as_text(getattr(b, "thinking", None) or getattr(b, "text", None))
                        )
                return "".join(parts)
        return ""
    except Exception:
        return ""


def _message_thinking(message: Any) -> str:
    """Extract full thinking/reasoning from a completed message object."""
    if message is None:
        return ""
    if isinstance(message, dict):
        for key in ("reasoning_content", "reasoning", "thinking", "reasoning_text"):
            t = _as_text(message.get(key))
            if t:
                return t
        blocks = message.get("thinking_blocks")
        if isinstance(blocks, list):
            parts = []
            for b in blocks:
                if isinstance(b, dict):
                    parts.append(_as_text(b.get("thinking") or b.get("text") or b.get("content")))
            return "".join(parts)
        return ""
    for attr in ("reasoning_content", "reasoning", "thinking", "reasoning_text"):
        t = _as_text(getattr(message, attr, None))
        if t:
            return t
    blocks = getattr(message, "thinking_blocks", None) or []
    parts = []
    for b in blocks:
        if isinstance(b, dict):
            parts.append(_as_text(b.get("thinking") or b.get("text")))
        else:
            parts.append(_as_text(getattr(b, "thinking", None) or getattr(b, "text", None)))
    return "".join(parts)


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


def _merge_tool_call_delta(
    acc: dict[int, dict[str, Any]], raw_tcs: Any
) -> None:
    """Accumulate streamed tool_call deltas by index into forge-shaped calls."""
    if not raw_tcs:
        return
    try:
        for tc in raw_tcs:
            if isinstance(tc, dict):
                idx = int(tc.get("index") if tc.get("index") is not None else 0)
                entry = acc.setdefault(
                    idx,
                    {"id": "", "name": "", "arguments": ""},
                )
                if tc.get("id"):
                    entry["id"] = str(tc["id"])
                fn = tc.get("function") or {}
                if isinstance(fn, dict):
                    if fn.get("name"):
                        entry["name"] = str(fn["name"])
                    if fn.get("arguments"):
                        entry["arguments"] = str(entry.get("arguments") or "") + str(
                            fn["arguments"]
                        )
            else:
                idx = int(getattr(tc, "index", 0) or 0)
                entry = acc.setdefault(
                    idx,
                    {"id": "", "name": "", "arguments": ""},
                )
                tid = getattr(tc, "id", None)
                if tid:
                    entry["id"] = str(tid)
                fn = getattr(tc, "function", None)
                if fn is not None:
                    name = getattr(fn, "name", None)
                    if name:
                        entry["name"] = str(name)
                    args = getattr(fn, "arguments", None)
                    if args:
                        entry["arguments"] = str(entry.get("arguments") or "") + str(args)
    except Exception:
        pass


def _finalize_tool_calls(acc: dict[int, dict[str, Any]]) -> list[dict[str, Any]]:
    out: list[dict[str, Any]] = []
    for i in sorted(acc.keys()):
        e = acc[i]
        args_raw = e.get("arguments") or "{}"
        try:
            arguments = json.loads(args_raw) if isinstance(args_raw, str) else (args_raw or {})
            if not isinstance(arguments, dict):
                arguments = {"_raw": arguments}
        except Exception:
            arguments = {"_raw": args_raw}
        out.append(
            {
                "id": e.get("id") or f"call_{i}",
                "name": e.get("name") or "",
                "arguments": arguments,
            }
        )
    return out


def handle_complete_stream(mid: str, params: dict[str, Any]) -> None:
    """Stream thinking_delta + text_delta events, then a final complete_stream response."""
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
    thinking_parts: list[str] = []
    tool_acc: dict[int, dict[str, Any]] = {}
    finish_reason: str | None = None
    usage_out: dict[str, Any] | None = None

    try:
        prev = sys.stdout
        sys.stdout = _StderrOnly()  # type: ignore[assignment]
        try:
            stream = litellm.completion(**kwargs)
            for chunk in stream:
                think = _chunk_thinking(chunk)
                if think:
                    thinking_parts.append(think)
                    _emit_thinking_delta(mid, think)

                piece = _chunk_text(chunk)
                if piece:
                    text_parts.append(piece)
                    # emit() always writes to _PROTO_OUT (real stdout), safe while
                    # sys.stdout is redirected for LiteLLM noise.
                    _emit_text_delta(mid, piece)

                # finish_reason / usage / tool_calls from stream chunks
                try:
                    if isinstance(chunk, dict):
                        choices = chunk.get("choices") or []
                        if choices:
                            c0 = choices[0]
                            fr = c0.get("finish_reason")
                            if fr:
                                finish_reason = fr
                            delta = c0.get("delta") or {}
                            if isinstance(delta, dict):
                                _merge_tool_call_delta(tool_acc, delta.get("tool_calls"))
                        u = chunk.get("usage")
                    else:
                        choices = getattr(chunk, "choices", None) or []
                        if choices:
                            c0 = choices[0]
                            fr = getattr(c0, "finish_reason", None)
                            if fr:
                                finish_reason = str(fr)
                            delta = getattr(c0, "delta", None)
                            if delta is not None:
                                tcs = (
                                    delta.get("tool_calls")
                                    if isinstance(delta, dict)
                                    else getattr(delta, "tool_calls", None)
                                )
                                _merge_tool_call_delta(tool_acc, tcs)
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
        full_thinking = "".join(thinking_parts) or None
        # Prefer streamed thinking; if provider only attached it at the end, keep it
        result: dict[str, Any] = {
            "text": full_text,
            "tool_calls": _finalize_tool_calls(tool_acc),
            "usage": usage_out,
            "finish_reason": finish_reason or "stop",
            "thinking": full_thinking,
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
