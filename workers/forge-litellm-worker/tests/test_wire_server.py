import io
import json
from contextlib import redirect_stdout

from forge_litellm_worker.server import handle_line


def test_ping():
    buf = io.StringIO()
    with redirect_stdout(buf):
        assert handle_line(json.dumps({"v": 1, "id": "1", "type": "request", "method": "ping", "params": {}}))
    out = json.loads(buf.getvalue().strip())
    assert out["type"] == "response"
    assert out["result"]["ok"] is True


def test_shutdown_stops():
    buf = io.StringIO()
    with redirect_stdout(buf):
        assert handle_line(json.dumps({"v": 1, "id": "2", "type": "request", "method": "shutdown", "params": {}})) is False
