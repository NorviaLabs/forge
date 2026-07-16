from forge_litellm_worker.effort import apply_litellm_effort, configured_effort


def routed(model: str, effort: str) -> dict:
    kwargs: dict = {}
    apply_litellm_effort(kwargs, model, effort)
    return kwargs


def test_openai_reasoning_models_receive_effort() -> None:
    assert routed("openai/gpt-5.4", "xhigh") == {"reasoning_effort": "xhigh"}
    assert routed("openai/gpt-4o", "high") == {}


def test_anthropic_uses_output_config_and_clamps_older_models() -> None:
    assert routed("anthropic/claude-sonnet-5", "xhigh") == {
        "output_config": {"effort": "xhigh"}
    }
    assert routed("anthropic/claude-sonnet-4-6", "xhigh") == {
        "output_config": {"effort": "high"}
    }
    assert routed("anthropic/claude-sonnet-4-5", "high") == {}


def test_xai_uses_only_supported_efforts_and_models() -> None:
    assert routed("xai/grok-4.5", "xhigh") == {"reasoning_effort": "high"}
    assert routed("xai/grok-4.20-multi-agent", "max") == {
        "reasoning_effort": "xhigh"
    }
    assert routed("xai/grok-3", "high") == {}


def test_minimal_maps_to_low_where_not_supported() -> None:
    assert routed("xai/grok-4.5", "minimal") == {"reasoning_effort": "low"}
    assert routed("anthropic/claude-opus-4-8", "minimal") == {
        "output_config": {"effort": "low"}
    }


def test_invalid_effort_is_ignored() -> None:
    assert configured_effort({"reasoning_effort": "ultra"}) == ""
