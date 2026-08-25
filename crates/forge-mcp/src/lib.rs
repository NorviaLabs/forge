//! MCP protocol (protocol-mcp.md) — CORE-02 Phase 1.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use forge_config::McpServerConfig;
use forge_tools::{Tool, ToolContext, ToolError, ToolRegistry};
use forge_types::{
    SideEffectClass, ToolDescriptor, ToolOutput, MCP_TOOL_NAME_PREFIX, SEARCH_TOOLS_TOOL_NAME,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use tokio::time::{timeout, Duration};
use tracing::warn;

const MCP_IO_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum McpError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("MCP {0} timed out")]
    Timeout(&'static str),
    #[error("server `{0}` not found")]
    NotFound(String),
}

/// In-process MCP-like tool source for tests (no real subprocess).
pub struct StaticMcpTool {
    pub server_id: String,
    pub tool_name: String,
    pub description: String,
    pub schema: Value,
    /// Declared authority for this test/in-process MCP tool. Unlike built-ins,
    /// MCP handlers are not assumed safe metadata operations.
    pub side_effect_class: SideEffectClass,
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
        self.side_effect_class
    }
    async fn call(&self, _ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        Ok((self.handler)(args))
    }
}

/// Namespaced MCP tool name: `mcp:<server_id>:<tool>`
pub fn mcp_tool_name(server_id: &str, tool: &str) -> String {
    format!("{MCP_TOOL_NAME_PREFIX}{server_id}:{tool}")
}

pub fn parse_mcp_tool_name(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix(MCP_TOOL_NAME_PREFIX)?;
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
    request_lock: Mutex<()>,
    server_id: String,
}

/// `fork()` in a multi-threaded process can race another thread's
/// close-after-write of the very executable being exec'd, so a
/// freshly-written script can transiently report ETXTBSY even though
/// nothing still holds it open for writing. This is a known kernel/libc
/// race (not a real conflict), so retry briefly before giving up.
async fn spawn_retrying_text_file_busy(
    command: &str,
    args: &[String],
) -> Result<Child, std::io::Error> {
    const ETXTBSY: i32 = 26;
    const MAX_ATTEMPTS: u32 = 5;
    for attempt in 1..=MAX_ATTEMPTS {
        match Command::new(command)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => return Ok(child),
            Err(err) if err.raw_os_error() == Some(ETXTBSY) && attempt < MAX_ATTEMPTS => {
                tokio::time::sleep(Duration::from_millis(10 * attempt as u64)).await;
            }
            Err(err) => return Err(err),
        }
    }
    unreachable!("loop always returns by the final attempt")
}

impl McpStdioClient {
    pub async fn spawn(cfg: &McpServerConfig) -> Result<Self, McpError> {
        if cfg.transport != "stdio" {
            return Err(McpError::Protocol(format!(
                "unsupported transport {}",
                cfg.transport
            )));
        }
        let mut child = spawn_retrying_text_file_busy(&cfg.command, &cfg.args).await?;
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
            request_lock: Mutex::new(()),
            server_id: cfg.id.clone(),
        };
        // initialize
        client
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": "forge", "version": "0.1.0" }
                }),
            )
            .await?;
        client
            .notify("notifications/initialized", json!({}))
            .await?;
        Ok(client)
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        let _request_guard = self.request_lock.lock().await;
        timeout(MCP_IO_TIMEOUT, self.notify_inner(method, params))
            .await
            .map_err(|_| McpError::Timeout("notification"))?
    }

    async fn notify_inner(&self, method: &str, params: Value) -> Result<(), McpError> {
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
        let _request_guard = self.request_lock.lock().await;
        timeout(MCP_IO_TIMEOUT, self.request_inner(method, params))
            .await
            .map_err(|_| McpError::Timeout("request"))?
    }

    async fn request_inner(&self, method: &str, params: Value) -> Result<Value, McpError> {
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
            outcome: Default::default(),
            content,
            is_error,
            exit_code: None,
            attachments: Vec::new(),
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
    /// Declared by the trusted MCP server configuration, not inferred from
    /// the remote tool's name or untrusted MCP metadata.
    pub side_effect_class: SideEffectClass,
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
        self.side_effect_class
    }
    async fn call(&self, _ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        self.client
            .call_tool(&self.remote_name, args)
            .await
            .map_err(|e| ToolError::Execution(e.to_string()))
    }
}

/// A server's `enabled_tools`/`disabled_tools` config, resolved once at
/// connect time so `register_into` doesn't need the original config list.
#[derive(Debug, Clone, Default)]
struct ToolFilter {
    enabled: Option<Vec<String>>,
    disabled: Option<Vec<String>>,
}

impl ToolFilter {
    fn from_config(cfg: &McpServerConfig) -> Self {
        Self {
            enabled: cfg.enabled_tools.clone(),
            disabled: cfg.disabled_tools.clone(),
        }
    }

    /// Applied to the server's own (unnamespaced) tool name — what a config
    /// author writes is what the MCP server itself calls the tool, not
    /// forge's `mcp:<server>:<tool>` wire name.
    fn allows(&self, name: &str) -> bool {
        if let Some(allow) = &self.enabled {
            if !allow.iter().any(|n| n == name) {
                return false;
            }
        }
        if let Some(deny) = &self.disabled {
            if deny.iter().any(|n| n == name) {
                return false;
            }
        }
        true
    }
}

pub struct McpManager {
    clients: HashMap<String, Arc<McpStdioClient>>,
    side_effect_classes: HashMap<String, SideEffectClass>,
    tool_filters: HashMap<String, ToolFilter>,
}

impl McpManager {
    pub fn new() -> Self {
        Self {
            clients: HashMap::new(),
            side_effect_classes: HashMap::new(),
            tool_filters: HashMap::new(),
        }
    }

    pub async fn connect_all(&mut self, servers: &[McpServerConfig]) -> Vec<String> {
        let mut errors = Vec::new();
        for s in servers {
            match McpStdioClient::spawn(s).await {
                Ok(c) => {
                    self.clients.insert(s.id.clone(), Arc::new(c));
                    self.side_effect_classes
                        .insert(s.id.clone(), s.side_effect_class);
                    self.tool_filters
                        .insert(s.id.clone(), ToolFilter::from_config(s));
                }
                Err(e) => errors.push(format!("{}: {e}", s.id)),
            }
        }
        errors
    }

    pub async fn register_into(&self, registry: &mut ToolRegistry) -> Result<(), McpError> {
        for (sid, client) in &self.clients {
            let tools = client.list_tools().await?;
            let filter = self.tool_filters.get(sid);
            let admitted = tools.len();
            let mut registered = 0usize;
            for info in tools {
                if filter.is_some_and(|f| !f.allows(&info.name)) {
                    continue;
                }
                registered += 1;
                let full = mcp_tool_name(sid, &info.name);
                let remote_name = info.name.clone();
                registry.register(Arc::new(RemoteMcpTool {
                    full_name: full,
                    info,
                    side_effect_class: self
                        .side_effect_classes
                        .get(sid)
                        .copied()
                        .unwrap_or(SideEffectClass::Exec),
                    client: client.clone(),
                    remote_name,
                }));
            }
            // `enabled_tools` naming a tool the server doesn't actually
            // expose reads as "nothing registered" with no other signal —
            // this is the one case worth surfacing since it's almost always
            // a typo, not an intentional empty allowlist.
            if registered == 0 && admitted > 0 && filter.is_some_and(|f| f.enabled.is_some()) {
                warn!(server = %sid, "enabled_tools matched none of this server's tools");
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

/// `search_tools` — the meta-tool `forge-core` promotes into the model's
/// tool list once MCP schema tokens cross its deferral budget (see
/// `CompactionPolicy::tool_schema_budget`). It lets the model discover a
/// deferred MCP tool by name and description without paying for every
/// deferred tool's full JSON schema on every request.
///
/// The catalog is a one-time snapshot taken right after MCP registration,
/// not a live registry lookup: forge only connects MCP servers once, at
/// session start, so nothing in the catalog can go stale mid-session.
pub struct SearchToolsTool {
    catalog: Vec<ToolDescriptor>,
}

#[async_trait]
impl Tool for SearchToolsTool {
    fn name(&self) -> &str {
        SEARCH_TOOLS_TOOL_NAME
    }
    fn description(&self) -> &str {
        "Find a connected MCP tool by keyword. Most MCP tools are hidden from your tool list \
         by default to save context — call this with a keyword describing what you need (a \
         server name, an action, a capability) to see matching tool names and descriptions, \
         then call the matching tool directly by the name this returns. If your first call to \
         a newly found tool gets the arguments wrong, the resulting error describes the \
         correct shape — the tool becomes fully visible, schema included, from then on."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "keyword to match against tool names and descriptions"
                }
            },
            "required": ["query"]
        })
    }
    fn side_effect_class(&self) -> SideEffectClass {
        SideEffectClass::Read
    }
    async fn call(&self, _ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();
        let matches: Vec<Value> = self
            .catalog
            .iter()
            .filter(|t| {
                query.is_empty()
                    || t.name.to_lowercase().contains(&query)
                    || t.description.to_lowercase().contains(&query)
            })
            .take(20)
            .map(|t| json!({"name": t.name, "description": t.description}))
            .collect();
        let content = if matches.is_empty() {
            format!(
                "No tools matched \"{query}\" out of {} tools across connected MCP servers.",
                self.catalog.len()
            )
        } else {
            serde_json::to_string_pretty(&matches).unwrap_or_default()
        };
        Ok(ToolOutput {
            outcome: Default::default(),
            content,
            is_error: false,
            exit_code: None,
            attachments: Vec::new(),
        })
    }
}

/// Registers `search_tools` against every MCP tool already in `registry` —
/// call once, after every configured MCP server has finished registering.
/// A registry with no MCP tools gets no `search_tools` entry: there would
/// be nothing for it to find.
pub fn install_search_tools(registry: &mut ToolRegistry) {
    let catalog: Vec<ToolDescriptor> = registry
        .list_descriptors()
        .iter()
        .filter(|t| t.name.starts_with(MCP_TOOL_NAME_PREFIX))
        .cloned()
        .collect();
    if catalog.is_empty() {
        return;
    }
    registry.register(Arc::new(SearchToolsTool { catalog }));
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
   i=0
   while [ "$i" -lt 4096 ]; do
     printf '%s\n' 'mcp server diagnostic' >&2
     i=$((i + 1))
   done
   id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"capabilities":{}}}'
      ;;
    *'"method":"tools/list"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo","description":"Echo text","inputSchema":{"type":"object"}},{"name":"bad","inputSchema":"invalid"},{"description":"missing name"}]}}'
      ;;
    *'"method":"tools/call"'*)
      printf '%s\n' "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"hello\"}],\"isError\":true}}"
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

    #[cfg(unix)]
    fn fixture_server_two_tools() -> tempfile::TempDir {
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
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo","description":"Echo text","inputSchema":{"type":"object"}},{"name":"ping","description":"Ping","inputSchema":{"type":"object"}}]}}'
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

    fn fixture_cfg(directory: &tempfile::TempDir) -> McpServerConfig {
        McpServerConfig {
            id: "fixture".into(),
            transport: "stdio".into(),
            command: directory
                .path()
                .join("mcp-server.sh")
                .to_string_lossy()
                .into_owned(),
            args: Vec::new(),
            side_effect_class: SideEffectClass::Exec,
            enabled_tools: None,
            disabled_tools: None,
        }
    }

    #[test]
    fn tool_filter_enabled_admits_only_the_named_tools() {
        let filter = ToolFilter {
            enabled: Some(vec!["echo".into()]),
            disabled: None,
        };
        assert!(filter.allows("echo"));
        assert!(!filter.allows("ping"));
    }

    #[test]
    fn tool_filter_disabled_drops_the_named_tools_and_admits_the_rest() {
        let filter = ToolFilter {
            enabled: None,
            disabled: Some(vec!["ping".into()]),
        };
        assert!(filter.allows("echo"));
        assert!(!filter.allows("ping"));
    }

    #[test]
    fn tool_filter_disabled_wins_over_enabled_for_the_same_name() {
        let filter = ToolFilter {
            enabled: Some(vec!["echo".into(), "ping".into()]),
            disabled: Some(vec!["ping".into()]),
        };
        assert!(filter.allows("echo"));
        assert!(!filter.allows("ping"));
    }

    #[test]
    fn tool_filter_default_admits_everything() {
        assert!(ToolFilter::default().allows("anything"));
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn enabled_tools_narrows_what_register_into_registers() {
        let directory = fixture_server_two_tools();
        let mut cfg = fixture_cfg(&directory);
        cfg.enabled_tools = Some(vec!["echo".into()]);

        let mut manager = McpManager::new();
        assert!(manager.connect_all(&[cfg]).await.is_empty());

        let mut registry = ToolRegistry::new();
        manager.register_into(&mut registry).await.unwrap();
        let names = registry.names();
        assert!(names.contains(&"mcp:fixture:echo".to_string()));
        assert!(
            !names.contains(&"mcp:fixture:ping".to_string()),
            "ping should have been filtered out by enabled_tools: {names:?}"
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn disabled_tools_drops_only_the_named_tool() {
        let directory = fixture_server_two_tools();
        let mut cfg = fixture_cfg(&directory);
        cfg.disabled_tools = Some(vec!["ping".into()]);

        let mut manager = McpManager::new();
        assert!(manager.connect_all(&[cfg]).await.is_empty());

        let mut registry = ToolRegistry::new();
        manager.register_into(&mut registry).await.unwrap();
        let names = registry.names();
        assert!(names.contains(&"mcp:fixture:echo".to_string()));
        assert!(!names.contains(&"mcp:fixture:ping".to_string()));
    }

    #[test]
    fn install_search_tools_registers_nothing_when_there_are_no_mcp_tools() {
        let mut reg = ToolRegistry::new();
        install_search_tools(&mut reg);
        assert!(reg.get(SEARCH_TOOLS_TOOL_NAME).is_none());
    }

    #[test]
    fn install_search_tools_registers_once_an_mcp_tool_exists() {
        let mut reg = ToolRegistry::new();
        register_static_mcp(
            &mut reg,
            "demo",
            vec![StaticMcpTool {
                server_id: "demo".into(),
                tool_name: "alpha".into(),
                description: "does alpha things".into(),
                schema: json!({"type": "object"}),
                side_effect_class: SideEffectClass::Exec,
                handler: Box::new(|_| ToolOutput {
                    outcome: Default::default(),
                    content: "ok".into(),
                    is_error: false,
                    exit_code: None,
                    attachments: Vec::new(),
                }),
            }],
        );
        install_search_tools(&mut reg);
        assert!(reg.get(SEARCH_TOOLS_TOOL_NAME).is_some());
    }

    #[tokio::test]
    async fn search_tools_matches_by_name_or_description_and_ignores_case() {
        let tool = SearchToolsTool {
            catalog: vec![
                ToolDescriptor {
                    name: "mcp:demo:alpha".into(),
                    description: "does alpha things".into(),
                    input_schema: json!({"type": "object"}),
                    side_effect_class: SideEffectClass::Exec,
                    idempotent: false,
                },
                ToolDescriptor {
                    name: "mcp:demo:beta".into(),
                    description: "unrelated capability".into(),
                    input_schema: json!({"type": "object"}),
                    side_effect_class: SideEffectClass::Exec,
                    idempotent: false,
                },
            ],
        };
        let ctx = ToolContext::new(std::env::current_dir().unwrap());

        let out = tool.call(&ctx, json!({"query": "ALPHA"})).await.unwrap();
        assert!(out.content.contains("mcp:demo:alpha"), "{}", out.content);
        assert!(!out.content.contains("mcp:demo:beta"), "{}", out.content);

        let out = tool
            .call(&ctx, json!({"query": "nothing matches this"}))
            .await
            .unwrap();
        assert!(out.content.contains("No tools matched"), "{}", out.content);
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
                side_effect_class: SideEffectClass::Exec,
                handler: Box::new(|args| ToolOutput {
                    outcome: Default::default(),
                    content: args
                        .get("text")
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .to_string(),
                    is_error: false,
                    exit_code: None,
                    attachments: Vec::new(),
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
                side_effect_class: SideEffectClass::Exec,
                handler: Box::new(|_| ToolOutput {
                    outcome: Default::default(),
                    content: "x".into(),
                    is_error: false,
                    exit_code: None,
                    attachments: Vec::new(),
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
            side_effect_class: SideEffectClass::Exec,
            enabled_tools: None,
            disabled_tools: None,
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
    #[cfg(unix)]
    async fn concurrent_requests_keep_matching_responses() {
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
            side_effect_class: SideEffectClass::Exec,
            enabled_tools: None,
            disabled_tools: None,
        };
        let client = McpStdioClient::spawn(&cfg).await.unwrap();
        let (first, second) = tokio::join!(
            client.call_tool("echo", json!({"n": 1})),
            client.call_tool("echo", json!({"n": 2}))
        );
        assert!(first.unwrap().content.contains("hello"));
        assert!(second.unwrap().content.contains("hello"));
    }

    #[tokio::test]
    async fn spawn_rejects_unsupported_transport() {
        let cfg = McpServerConfig {
            id: "bad".into(),
            transport: "http".into(),
            command: "true".into(),
            args: Vec::new(),
            side_effect_class: SideEffectClass::Exec,
            enabled_tools: None,
            disabled_tools: None,
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
            side_effect_class: SideEffectClass::Exec,
            enabled_tools: None,
            disabled_tools: None,
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
                side_effect_class: SideEffectClass::Exec,
                enabled_tools: None,
                disabled_tools: None,
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
            side_effect_class: SideEffectClass::Read,
            handler: Box::new(|_| ToolOutput {
                outcome: Default::default(),
                content: "ok".into(),
                is_error: false,
                exit_code: None,
                attachments: Vec::new(),
            }),
        };
        assert_eq!(tool.name(), "mcp:demo:echo");
        assert_eq!(tool.description(), "echo back");
        assert_eq!(tool.side_effect_class(), SideEffectClass::Read);
        assert!(tool.input_schema().is_object());
    }
}
