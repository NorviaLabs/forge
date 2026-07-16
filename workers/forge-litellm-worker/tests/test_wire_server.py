import io
import json

from forge_litellm_worker import server


def capture_handle(line: str) -> tuple[bool, str]:
    output = io.StringIO()
    previous = server._PROTO_OUT
    server._PROTO_OUT = output
    try:
        keep_running = server.handle_line(line)
    finally:
        server._PROTO_OUT = previous
    return keep_running, output.getvalue()


def test_ping():
    keep_running, response = capture_handle(
        json.dumps(
            {"v": 1, "id": "1", "type": "request", "method": "ping", "params": {}}
        )
    )
    assert keep_running
    out = json.loads(response.strip())
    assert out["type"] == "response"
    assert out["result"]["ok"] is True


def test_shutdown_stops():
    keep_running, _ = capture_handle(
        json.dumps(
            {
                "v": 1,
                "id": "2",
                "type": "request",
                "method": "shutdown",
                "params": {},
            }
        )
    )
    assert keep_running is False
