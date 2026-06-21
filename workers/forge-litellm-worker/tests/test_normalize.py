from forge_litellm_worker.normalize import complete_result_from_litellm, content_to_text


def test_content_string():
    assert content_to_text("hi") == "hi"


def test_content_parts():
    assert content_to_text([{"type": "text", "text": "A"}, {"type": "text", "text": "B"}]) == "AB"


def test_openai_shape():
    raw = {
        "choices": [
            {
                "message": {
                    "content": "hello",
                    "tool_calls": [
                        {
                            "id": "1",
                            "function": {"name": "read_file", "arguments": '{"path":"a"}'},
                        }
                    ],
                },
                "finish_reason": "tool_calls",
            }
        ],
        "usage": {"prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3},
    }
    r = complete_result_from_litellm(raw)
    assert r["text"] == "hello"
    assert r["tool_calls"][0]["name"] == "read_file"
    assert r["tool_calls"][0]["arguments"]["path"] == "a"


def test_forge_shape_passthrough():
    r = complete_result_from_litellm({"text": "x", "tool_calls": []})
    assert r["text"] == "x"
