"""Provider-neutral reasoning-effort policy."""

from __future__ import annotations

import os
from typing import Any

VALID_EFFORTS = frozenset(
    ("auto", "minimal", "low", "medium", "high", "xhigh", "max")
)


def configured_effort(extra: dict[str, Any] | None = None) -> str:
    explicit = extra.get("reasoning_effort") if isinstance(extra, dict) else None
    effort = (
        str(explicit or os.environ.get("FORGE_REASONING_EFFORT") or "")
        .strip()
        .lower()
    )
    return effort if effort in VALID_EFFORTS else ""


def codex_effort(extra: dict[str, Any] | None = None) -> str | None:
    effort = configured_effort(extra)
    if not effort or effort == "auto":
        return None
    return "xhigh" if effort == "max" else effort


def apply_litellm_effort(
    kwargs: dict[str, Any],
    model: str,
    effort: str | None = None,
) -> None:
    """Apply only parameters supported by the selected provider/model family."""
    effort = (effort if effort is not None else configured_effort()).strip().lower()
    if not effort or effort == "auto":
        return
    normalized = "low" if effort == "minimal" else effort
    lowered = model.lower()

    if lowered.startswith("anthropic/"):
        model_id = lowered.split("/", 1)[-1]
        supports_effort = any(
            marker in model_id
            for marker in (
                "sonnet-5",
                "opus-4-8",
                "opus-4-7",
                "opus-4-6",
                "sonnet-4-6",
                "opus-4-5",
                "fable-5",
                "mythos",
            )
        )
        if supports_effort:
            if normalized == "xhigh" and any(
                marker in model_id for marker in ("4-6", "opus-4-5")
            ):
                normalized = "high"
            kwargs["output_config"] = {"effort": normalized}
        return

    if lowered.startswith("xai/") or lowered.startswith("grok"):
        model_id = lowered.split("/", 1)[-1]
        supports_effort = any(
            marker in model_id for marker in ("grok-4.3", "grok-4.5", "grok-4.20")
        )
        if supports_effort:
            if "multi-agent" in model_id:
                normalized = "xhigh" if normalized in ("xhigh", "max") else normalized
            elif normalized in ("xhigh", "max"):
                normalized = "high"
            kwargs["reasoning_effort"] = normalized
        return

    if lowered.startswith(("openai/", "azure/")):
        model_id = lowered.split("/", 1)[-1]
        if model_id.startswith(("gpt-5", "o1", "o3", "o4")):
            kwargs["reasoning_effort"] = "xhigh" if normalized == "max" else normalized
        return

    if lowered.startswith(("opencode-go/", "opencode-zen/", "openrouter/")):
        kwargs["reasoning_effort"] = "xhigh" if normalized == "max" else normalized
