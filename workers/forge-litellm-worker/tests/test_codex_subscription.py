from forge_litellm_worker.codex_subscription import _request_body, _tool_name_maps


def test_namespaced_tools_use_reversible_api_safe_aliases() -> None:
    tools = [
        {
            "type": "function",
            "function": {
                "name": "mcp:demo:echo",
                "parameters": {"type": "object"},
            },
        },
        {
            "type": "function",
            "function": {
                "name": "mcp/demo/echo",
                "parameters": {"type": "object"},
            },
        },
    ]
    forward, reverse = _tool_name_maps(tools)

    assert len(set(forward.values())) == 2
    assert all(len(alias) <= 64 for alias in forward.values())
    assert all(alias.replace("_", "").replace("-", "").isalnum() for alias in forward.values())
    assert all(reverse[alias] == original for original, alias in forward.items())

    body = _request_body(
        {
            "model": "openai-codex/gpt-5.6-sol",
            "tools": tools,
            "messages": [
                {
                    "role": "assistant",
                    "tool_calls": [
                        {
                            "id": "call-1",
                            "function": {
                                "name": "mcp:demo:echo",
                                "arguments": "{}",
                            },
                        }
                    ],
                }
            ],
        },
        forward,
    )
    assert body["tools"][0]["name"] == forward["mcp:demo:echo"]
    assert body["input"][0]["name"] == forward["mcp:demo:echo"]


def test_reasoning_effort_from_environment(monkeypatch) -> None:
    monkeypatch.setenv("FORGE_REASONING_EFFORT", "high")
    body = _request_body(
        {
            "model": "openai-codex/gpt-5.6-sol",
            "messages": [{"role": "user", "content": "hello"}],
        }
    )
    assert body["reasoning"] == {"effort": "high", "summary": "auto"}
