# forge-litellm-worker

Phase 5 worker: **LiteLLM Python SDK** (library) over stdio NDJSON. Not the LiteLLM Proxy.

```bash
pip install -e .
python -m forge_litellm_worker
```

Forge spawns this process when `provider = "litellm"`.
