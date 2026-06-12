# Forge

Open-source, enterprise-ready **AI agent harness**: scaffolding around foundation models for reliable long-horizon work (especially repository-native software engineering).

**Phase 1 (implemented):** coding agent — schema-validated tools, MCP, multi-provider models, durable SQLite journal, headless CLI + REPL.

## Quick start

```bash
cargo build --release -p forge-cli
./target/release/forge status
./target/release/forge --mock run "hello"
./target/release/forge --mock repl
```

With a real API key:

```bash
export FORGE_API_KEY=...
export FORGE_MODEL_PROVIDER=openai_compatible   # or anthropic | xai
export FORGE_MODEL_ID=gpt-4.1-mini
./target/release/forge run "Summarize this repo"
```

Optional project config: `forge.toml` (see [docs/designs/configuration.md](./docs/designs/configuration.md)).

```bash
cargo test    # full Phase 1 suite
```

## Crates (Phase 1)

| Crate | Design |
|-------|--------|
| `forge-config` | configuration.md |
| `forge-tools` | tool-protocol.md |
| `forge-model` | model-providers.md |
| `forge-durable` | durable-execution.md |
| `forge-core` | agent-loop.md |
| `forge-mcp` | protocol-mcp.md |
| `forge-tui` | tui-commands.md (+ exit codes) |
| `forge-cli` | surfaces.md (binary) |

## Documentation

| Doc | Description |
|-----|-------------|
| [docs/prd.md](./docs/prd.md) | Product requirements |
| [docs/architecture.md](./docs/architecture.md) | System architecture, flows, stack decisions |
| [docs/designs/README.md](./docs/designs/README.md) | Design docs by phase |
| [docs/ui.md](./docs/ui.md) | TUI UI reference and mockups |

## License

[MIT](./LICENSE)
