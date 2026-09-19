# Forge Architecture

Forge is a Rust workspace for a terminal AI coding agent, editor, and shell. The diagram below reflects the workspace crate boundaries and the primary request/turn lifecycle.

```mermaid
flowchart TB
    user([Developer])
    subgraph app[Application boundary]
        cli[forge-cli\nCLI entrypoint]
        tui[forge-tui\nFull-screen terminal UI]
        session[forge-session\nSession assembly, resume, headless]
    end

    subgraph runtime[Agent runtime]
        core[forge-core\nAgent coordinator, turns, queues\nstreaming, permissions, persistence]
        transcript[forge-transcript\nRenderable conversation projection]
        tools[forge-tools\nValidated tool protocol and execution]
        mcp[forge-mcp\nMCP client bridge]
        model[forge-model\nNative provider transports]
    end

    subgraph services[Workspace and policy services]
        context[forge-context\nContext offload and handoff]
        workspace[forge-workspace\nFiles, Git status, attachments]
        search[forge-search\nWorkspace indexing and search]
        syntax[forge-syntax\nTree-sitter highlighting/refactoring]
        governance[forge-governance\nACL, vault injection, sandbox, audit]
        connect[forge-connect\nProvider profiles and credentials]
        config[forge-config\nTOML and environment configuration]
    end

    subgraph persistence[Durability and shared contracts]
        durable[forge-durable\nEvent journal and crash recovery]
        storage[forge-storage\nRuntime storage resolver]
        types[forge-types\nShared domain types and IDs]
    end

    subgraph external[External boundaries]
        providers[(Model providers)]
        mcpservers[(MCP servers)]
        repo[(Workspace repository)]
        local[(.forge/local session data)]
    end

    user --> cli
    cli --> tui
    cli --> session
    tui --> session
    tui --> transcript
    tui --> workspace
    tui --> syntax
    tui --> context
    session --> core
    session --> config
    session --> connect
    session --> model
    session --> tools
    session --> mcp
    session --> durable
    session --> storage

    core --> model
    core --> tools
    core --> governance
    core --> context
    core --> durable
    core --> storage
    core --> types
    transcript --> core
    transcript --> tools
    transcript --> types

    tools --> search
    tools --> context
    tools --> workspace
    tools --> config
    tools --> types
    mcp --> tools
    mcp --> config
    model --> connect
    model --> config
    model --> types
    context --> storage
    context --> types
    governance --> types
    durable --> types
    storage --> types
    config --> types
    connect --> types

    model --> providers
    mcp --> mcpservers
    workspace --> repo
    search --> repo
    durable --> local
    storage --> local
    connect --> local

    classDef entry fill:#264653,stroke:#8bd3dd,color:#fff
    classDef runtime fill:#3a506b,stroke:#a8dadc,color:#fff
    classDef service fill:#4b3f72,stroke:#cdb4db,color:#fff
    classDef data fill:#2d6a4f,stroke:#95d5b2,color:#fff
    classDef external fill:#5c4033,stroke:#f4a261,color:#fff

    class cli,tui,session entry
    class core,transcript,tools,mcp,model runtime
    class context,workspace,search,syntax,governance,connect,config service
    class durable,storage,types data
    class providers,mcpservers,repo,local external
```

## Runtime flow

1. `forge-cli` parses command-line options and starts the Tokio runtime.
2. `forge-tui` provides the interactive terminal experience, while `forge-session` assembles either an interactive or headless session.
3. `forge-core` coordinates turns: it streams model output, validates and executes tools, applies governance checks, and journals durable events.
4. `forge-model` connects to model providers; `forge-mcp` connects external MCP servers; workspace tools operate on the repository through `forge-workspace` and `forge-search`.
5. `forge-durable` and `forge-storage` preserve resumable session state under repository-local `.forge/local/` storage (or the configured application-data fallback).

`forge-types` is the shared contract layer. `forge-test-support` is intentionally omitted from the runtime diagram because it is test-only infrastructure.
