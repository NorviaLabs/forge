//! MCP protocol (protocol-mcp.md) — CORE-02 Phase 1.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use forge_config::McpServerConfig;
use forge_tools::{Tool, ToolContext, ToolError, ToolRegistry};
use forge_types::{SideEffectClass, ToolOutput};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use tracing::warn;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum McpError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("server `{0}` not found")]
    NotFound(String),
}

/// In-process MCP-like tool source for tests (no real subprocess).
pub struct StaticMcpTool {
    pub server_id: String,
    pub tool_name: String,
    pub description: String,
    pub schema: Value,
    pub handler: Box<dyn Fn(Value) -> ToolOutput + Send + Sync>,
}

#[async_trait]
impl Tool for StaticMcpTool {
    fn name(&self) -> &str {
        // stored as full namespaced name in registry via wrapper
        &self.tool_name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn input_schema(&self) -> Value {
        self.schema.clone()
    }
    fn side_effect_class(&self) -> SideEffectClass {
        SideEffectClass::Meta
    }
    async fn call(&self, _ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        Ok((self.handler)(args))
    }
}

/// Namespaced MCP tool name: `mcp:<server_id>:<tool>`
pub fn mcp_tool_name(server_id: &str, tool: &str) -> String {
    format!("mcp:{server_id}:{tool}")
}

pub fn parse_mcp_tool_name(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix("mcp:")?;
    let (sid, tool) = rest.split_once(':')?;
    Some((sid, tool))
}

/// Register static (test/demo) MCP tools into a registry with namespacing.
pub fn register_static_mcp(
    registry: &mut ToolRegistry,
    server_id: &str,
    tools: Vec<StaticMcpTool>,
) {
    for mut t in tools {
        let full = mcp_tool_name(server_id, &t.tool_name);
        t.tool_name = full.clone();
        registry.register(Arc::new(t));
    }
}

/// Minimal stdio JSON-RPC MCP client (tools/list + tools/call).
pub struct McpStdioClient {
    #[allow(dead_code)]
    child: Child,
    stdin: Mutex<ChildStdin>,
    stdout: Mutex<BufReader<ChildStdout>>,
    next_id: Mutex<u64>,
    server_id: String,
}

impl McpStdioClient {
    pub async fn spawn(cfg: &McpServerConfig) -> Result<Self, McpError> {
        if cfg.transport != "stdio" {
            return Err(McpError::Protocol(format!(
                "unsupported transport {}",
                cfg.transport
            )));
        }
        let mut child = Command::new(&cfg.command)
            .args(&cfg.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::Protocol("missing stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::Protocol("missing stdout".into()))?;
        let client = Self {
            child,
            stdin: Mutex::new(stdin),
            stdout: Mutex::new(BufReader::new(stdout)),
            next_id: Mutex::new(1),
            server_id: cfg.id.clone(),
        };
        // initialize
        let _ = client
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": "forge", "version": "0.1.0" }
                }),
            )
            .await;
        let _ = client.notify("notifications/initialized", json!({})).await;
        Ok(client)
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        let msg = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        let mut line = serde_json::to_string(&msg)?;
        line.push('\n');
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(line.as_bytes()).await?;
        stdin.flush().await?;
        Ok(())
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = {
            let mut n = self.next_id.lock().await;
            let id = *n;
            *n += 1;
            id
        };
        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let mut line = serde_json::to_string(&msg)?;
        line.push('\n');
        {
            let mut stdin = self.stdin.lock().await;
            stdin.write_all(line.as_bytes()).await?;
            stdin.flush().await?;
        }
        let mut stdout = self.stdout.lock().await;
        let mut buf = String::new();
        loop {
            buf.clear();
            let n = stdout.read_line(&mut buf).await?;
            if n == 0 {
                return Err(McpError::Protocol("eof from mcp server".into()));
            }
            let v: Value = serde_json::from_str(buf.trim())?;
            if v.get("id").and_then(|i| i.as_u64()) == Some(id) {
                if let Some(err) = v.get("error") {
                    return Err(McpError::Protocol(err.to_string()));
                }
                return Ok(v.get("result").cloned().unwrap_or(Value::Null));
            }
        }
    }

    pub async fn list_tools(&self) -> Result<Vec<McpToolInfo>, McpError> {
        let result = self.request("tools/list", json!({})).await?;
        let tools = result
            .get("tools")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default();
        let mut out = Vec::new();
        for t in tools {
            let name = t
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            if name.is_empty() {
                continue;
            }
            let description = t
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .to_string();
            let schema = t
                .get("inputSchema")
                .cloned()
                .unwrap_or_else(|| json!({"type": "object"}));
            // Fail closed: require object schema
            if !schema.is_object() {
                warn!(tool = %name, "skipping MCP tool without object schema");
                continue;
            }
            out.push(McpToolInfo {
                name,
                description,
                input_schema: schema,
            });
        }
        Ok(out)
    }

    pub async fn call_tool(&self, name: &str, args: Value) -> Result<ToolOutput, McpError> {
        let result = self
            .request("tools/call", json!({ "name": name, "arguments": args }))
            .await?;
        let is_error = result
            .get("isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let content = result
            .get("content")
            .map(|c| c.to_string())
            .unwrap_or_else(|| result.to_string());
        Ok(ToolOutput {
            content,
            is_error,
            exit_code: None,
        })
    }

    pub fn server_id(&self) -> &str {
        &self.server_id
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolInfo {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Live MCP tool bound to a stdio client.
pub struct RemoteMcpTool {
    pub full_name: String,
    pub info: McpToolInfo,
    pub client: Arc<McpStdioClient>,
    pub remote_name: String,
}

#[async_trait]
impl Tool for RemoteMcpTool {
    fn name(&self) -> &str {
        &self.full_name
    }
    fn description(&self) -> &str {
        &self.info.description
    }
    fn input_schema(&self) -> Value {
        self.info.input_schema.clone()
    }
    fn side_effect_class(&self) -> SideEffectClass {
        SideEffectClass::Meta
    }
    async fn call(&self, _ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        self.client
            .call_tool(&self.remote_name, args)
            .await
            .map_err(|e| ToolError::Execution(e.to_string()))
    }
}

pub struct McpManager {
    clients: HashMap<String, Arc<McpStdioClient>>,
}

impl McpManager {
    pub fn new() -> Self {
        Self {
            clients: HashMap::new(),
        }
    }

    pub async fn connect_all(&mut self, servers: &[McpServerConfig]) -> Vec<String> {
        let mut errors = Vec::new();
        for s in servers {
            match McpStdioClient::spawn(s).await {
                Ok(c) => {
                    self.clients.insert(s.id.clone(), Arc::new(c));
                }
                Err(e) => errors.push(format!("{}: {e}", s.id)),
            }
        }
        errors
    }

    pub async fn register_into(&self, registry: &mut ToolRegistry) -> Result<(), McpError> {
        for (sid, client) in &self.clients {
            let tools = client.list_tools().await?;
            for info in tools {
                let full = mcp_tool_name(sid, &info.name);
                let remote_name = info.name.clone();
                registry.register(Arc::new(RemoteMcpTool {
                    full_name: full,
                    info,
                    client: client.clone(),
                    remote_name,
                }));
            }
        }
        Ok(())
    }
}

impl Default for McpManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_tools::ValidationBudget;
    use serde_json::json;

    #[cfg(unix)]
    fn fixture_server() -> tempfile::TempDir {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("mcp-server.sh");
        std::fs::write(
            &script,
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"capabilities":{}}}'
      ;;
    *'"method":"tools/list"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo","description":"Echo text","inputSchema":{"type":"object"}},{"name":"bad","inputSchema":"invalid"},{"description":"missing name"}]}}'
      ;;
    *'"method":"tools/call"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"hello"}],"isError":true}}'
      ;;
  esac
done
"#,
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(script, permissions).unwrap();
        directory
    }

    #[test]
    fn namespace_roundtrip() {
        let n = mcp_tool_name("demo", "echo");
        assert_eq!(n, "mcp:demo:echo");
        let (s, t) = parse_mcp_tool_name(&n).unwrap();
        assert_eq!(s, "demo");
        assert_eq!(t, "echo");
        assert!(parse_mcp_tool_name("not-mcp").is_none());
        assert!(parse_mcp_tool_name("mcp:server-only").is_none());
        let (s, t) = parse_mcp_tool_name("mcp:demo:tool:with:colons").unwrap();
        assert_eq!(s, "demo");
        assert_eq!(t, "tool:with:colons");
    }

    #[tokio::test]
    async fn static_mcp_tool_registers_and_runs() {
        let mut reg = ToolRegistry::new();
        register_static_mcp(
            &mut reg,
            "demo",
            vec![StaticMcpTool {
                server_id: "demo".into(),
                tool_name: "echo".into(),
                description: "echo".into(),
                schema: json!({
                    "type": "object",
                    "properties": { "text": { "type": "string" } },
                    "required": ["text"]
                }),
                handler: Box::new(|args| ToolOutput {
                    content: args
                        .get("text")
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .to_string(),
                    is_error: false,
                    exit_code: None,
                }),
            }],
        );
        assert!(reg.names().contains(&"mcp:demo:echo".to_string()));
        let ctx = ToolContext::new(std::env::current_dir().unwrap());
        let mut b = ValidationBudget::with_default_max();
        let out = reg
            .call(&ctx, "mcp:demo:echo", json!({"text": "hi"}), &mut b)
            .await
            .unwrap();
        assert_eq!(out.content, "hi");
    }

    #[tokio::test]
    async fn static_mcp_validates_schema() {
        let mut reg = ToolRegistry::new();
        register_static_mcp(
            &mut reg,
            "demo",
            vec![StaticMcpTool {
                server_id: "demo".into(),
                tool_name: "echo".into(),
                description: "echo".into(),
                schema: json!({
                    "type": "object",
                    "properties": { "text": { "type": "string" } },
                    "required": ["text"]
                }),
                handler: Box::new(|_| ToolOutput {
                    content: "x".into(),
                    is_error: false,
                    exit_code: None,
                }),
            }],
        );
        let ctx = ToolContext::new(std::env::current_dir().unwrap());
        let mut b = ValidationBudget::with_default_max();
        let err = reg
            .call(&ctx, "mcp:demo:echo", json!({"text": 1}), &mut b)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn stdio_client_lists_filters_and_calls_tools() {
        let directory = fixture_server();
        let cfg = McpServerConfig {
            id: "fixture".into(),
            transport: "stdio".into(),
            command: directory
                .path()
                .join("mcp-server.sh")
                .to_string_lossy()
                .into_owned(),
            args: Vec::new(),
        };
        let client = McpStdioClient::spawn(&cfg).await.unwrap();

        assert_eq!(client.server_id(), "fixture");
        let tools = client.list_tools().await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");
        assert_eq!(tools[0].description, "Echo text");
        let output = client
            .call_tool("echo", json!({"text": "hello"}))
            .await
            .unwrap();
        assert!(output.is_error);
        assert!(output.content.contains("hello"));
    }

    #[tokio::test]
    async fn spawn_rejects_unsupported_transport() {
        let cfg = McpServerConfig {
            id: "bad".into(),
            transport: "http".into(),
            command: "true".into(),
            args: Vec::new(),
        };
        match McpStdioClient::spawn(&cfg).await {
            Err(McpError::Protocol(message)) => {
                assert!(message.contains("unsupported transport"));
            }
            Ok(_) => panic!("expected unsupported transport error"),
            Err(other) => panic!("expected protocol error, got {other:?}"),
        }
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn manager_connects_registers_and_invokes_remote_tools() {
        let directory = fixture_server();
        let cfg = McpServerConfig {
            id: "fixture".into(),
            transport: "stdio".into(),
            command: directory
                .path()
                .join("mcp-server.sh")
                .to_string_lossy()
                .into_owned(),
            args: Vec::new(),
        };

        let mut manager = McpManager::new();
        assert!(manager.connect_all(&[cfg]).await.is_empty());

        let mut registry = ToolRegistry::new();
        manager.register_into(&mut registry).await.unwrap();
        assert!(registry.names().contains(&"mcp:fixture:echo".to_string()));

        let ctx = ToolContext::new(std::env::current_dir().unwrap());
        let mut budget = ValidationBudget::with_default_max();
        let output = registry
            .call(
                &ctx,
                "mcp:fixture:echo",
                json!({"text": "hello"}),
                &mut budget,
            )
            .await
            .unwrap();
        assert!(output.is_error);
        assert!(output.content.contains("hello"));
    }

    #[tokio::test]
    async fn manager_default_and_connect_all_reports_spawn_failures() {
        let mut manager = McpManager::default();
        let errors = manager
            .connect_all(&[McpServerConfig {
                id: "missing".into(),
                transport: "stdio".into(),
                command: "/no/such/mcp-server".into(),
                args: Vec::new(),
            }])
            .await;
        assert_eq!(errors.len(), 1);
        assert!(errors[0].starts_with("missing:"));

        let mut registry = ToolRegistry::new();
        manager.register_into(&mut registry).await.unwrap();
        assert!(registry.names().is_empty());
    }

    #[tokio::test]
    async fn static_mcp_tool_exposes_metadata() {
        let tool = StaticMcpTool {
            server_id: "demo".into(),
            tool_name: "mcp:demo:echo".into(),
            description: "echo back".into(),
            schema: json!({"type": "object"}),
            handler: Box::new(|_| ToolOutput {
                content: "ok".into(),
                is_error: false,
                exit_code: None,
            }),
        };
        assert_eq!(tool.name(), "mcp:demo:echo");
        assert_eq!(tool.description(), "echo back");
        assert_eq!(tool.side_effect_class(), SideEffectClass::Meta);
        assert!(tool.input_schema().is_object());
    }
}
