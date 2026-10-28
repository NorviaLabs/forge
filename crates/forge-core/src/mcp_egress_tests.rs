//! A sandboxed MCP server's egress refusals through a real session: the
//! refused host raises the host-grant prompt, the grant re-runs the same MCP
//! call, and the durable audit trail correlates every step by call id.
//!
//! Hermetic: the server is a shell script run inside the OS sandbox, and the
//! "remote" host is a loopback listener this test owns.

use std::path::Path;
use std::sync::Arc;

use forge_config::McpServerConfig;
use forge_durable::AuditKind;
use forge_model::MockModelClient;
use forge_tools::egress::{EgressPolicy, EgressShared};
use forge_tools::ToolRegistry;
use forge_types::{GrantDecision, ModelResponse, SideEffectClass, ToolCall};
use serde_json::json;
use tempfile::tempdir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::{AgentSession, LoopConfig, ModelResponseApplication};

/// A stdio MCP server whose `fetch` tool runs `probe` and flags the call
/// failed unless the probe printed `status=200`, and whose `tolerant` tool
/// runs the same probe but always reports success.
fn fixture_server(dir: &Path) -> String {
    use std::os::unix::fs::PermissionsExt;

    let script = dir.join("mcp-server.sh");
    std::fs::write(
        &script,
        r#"#!/bin/bash
probe="$1"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"capabilities":{}}}'
      ;;
    *'"method":"tools/list"'*)
      printf '%s\n' "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"tools\":[{\"name\":\"fetch\",\"inputSchema\":{\"type\":\"object\"}},{\"name\":\"tolerant\",\"inputSchema\":{\"type\":\"object\"}}]}}"
      ;;
    *'"method":"tools/call"'*)
      out=$(/bin/bash "$probe" 2>/dev/null | tr -d '"\\\n')
      failed=false
      case "$line" in
        *'"name":"fetch"'*) case "$out" in *status=200*) ;; *) failed=true ;; esac ;;
      esac
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"%s"}],"isError":%s}}\n' "$id" "$out" "$failed"
      ;;
  esac
done
"#,
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&script).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&script, permissions).unwrap();
    script.to_string_lossy().into_owned()
}

/// A plain-HTTP upstream on loopback answering `200 ok` to every request.
async fn local_upstream() -> u16 {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf).await;
                let _ = stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    )
                    .await;
            });
        }
    });
    port
}

/// A session whose registry holds the sandboxed fixture server's tools, with
/// the server and the session gateway on one live egress state that denies
/// every host. `None` where the host cannot confine a server.
async fn session_with_sandboxed_server(
    dir: &Path,
) -> Option<(AgentSession, forge_mcp::McpManager)> {
    if let Err(error) = forge_tools::sandbox::availability() {
        eprintln!("skipping: this host cannot confine an MCP server: {error:?}");
        return None;
    }
    let upstream = local_upstream().await;
    let server_dir = dir.join("server");
    std::fs::create_dir_all(&server_dir).unwrap();
    let probe = dir.join("probe.sh");
    std::fs::write(
        &probe,
        format!(
            "/usr/bin/curl -q -s -o /dev/null -w 'status=%{{http_code}}' --max-time 5 \
             --proxy \"$HTTP_PROXY\" http://127.0.0.1:{upstream}/\n"
        ),
    )
    .unwrap();
    let cfg = McpServerConfig {
        id: "fixture".into(),
        transport: "stdio".into(),
        command: fixture_server(&server_dir),
        args: vec![probe.to_string_lossy().into_owned()],
        side_effect_class: SideEffectClass::Exec,
        sandbox: true,
        ..Default::default()
    };
    let shared = EgressShared::new(EgressPolicy::new(), Default::default());
    let mut manager = forge_mcp::McpManager::new().with_egress_shared(shared.clone());
    let errors = manager.connect_all_in_workspace(&[cfg], dir).await;
    assert!(errors.is_empty(), "{errors:?}");
    let mut tools = ToolRegistry::new();
    assert!(manager.register_into(&mut tools).await.is_empty());
    let mut session = AgentSession::create(
        LoopConfig {
            workspace: dir.to_path_buf(),
            journal_dir: dir.join("j"),
            enable_context_lifecycle: true,
            enable_governance: true,
            ..Default::default()
        },
        Arc::new(MockModelClient::script(vec![])),
        tools,
    )
    .await
    .unwrap();
    session.adopt_egress_state(shared).await;
    if session.egress_grant().is_none() {
        eprintln!("skipping: this host cannot start the egress gateway");
        return None;
    }
    Some((session, manager))
}

/// Ask the session to run one MCP call; MCP tools are gated, so this stops
/// on the governance prompt.
async fn request_mcp_call(session: &mut AgentSession, id: &str, tool: &str) {
    session.append_user_message("use the server").await.unwrap();
    let application = session
        .begin_model_response_application(ModelResponse {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: id.into(),
                name: tool.into(),
                arguments: json!({}),
            }],
            usage: None,
            thinking: None,
        })
        .await
        .unwrap();
    assert!(
        matches!(application, ModelResponseApplication::Finished(_)),
        "an MCP call must wait for approval first"
    );
    let payload = session.pending_hitl().expect("governance prompt");
    assert_eq!(payload.call_id, id);
    assert_eq!(payload.denied_host, None);
}

fn tool_result<'a>(session: &'a AgentSession, id: &str) -> Option<&'a str> {
    session
        .messages
        .iter()
        .rev()
        .find(|message| message.tool_call_id.as_deref() == Some(id))
        .map(|message| message.content.as_str())
}

/// The server's request to a host outside the allowlist fails the call; the
/// operator gets the host-grant prompt naming that host, "allow for this
/// session" re-runs the same MCP call, which now succeeds, and the audit
/// trail records each step under the call id, egress events included.
#[tokio::test]
async fn an_mcp_server_refused_a_host_prompts_and_the_grant_reruns_the_call() {
    let dir = tempdir().unwrap();
    let Some((mut session, _manager)) = session_with_sandboxed_server(dir.path()).await else {
        return;
    };
    request_mcp_call(&mut session, "call-mcp", "mcp:fixture:fetch").await;

    session
        .decide_hitl(GrantDecision::AllowOnce, "test")
        .await
        .unwrap();
    let payload = session
        .pending_hitl()
        .cloned()
        .expect("the refused host must raise the host-grant prompt");
    assert_eq!(payload.call_id, "call-mcp");
    assert_eq!(payload.tool, "mcp:fixture:fetch");
    assert_eq!(payload.denied_host.as_deref(), Some("127.0.0.1"));
    assert!(!payload.sandbox_escalation);
    assert!(
        payload.reason.contains("MCP server `fixture`"),
        "{payload:?}"
    );

    session
        .decide_hitl(GrantDecision::AllowHostForSession, "test")
        .await
        .unwrap();
    assert!(session.pending_hitl().is_none());
    let result = tool_result(&session, "call-mcp").expect("the retry records a result");
    assert!(result.contains("status=200"), "{result}");

    let rows = session.audit_events().await.unwrap();
    let steps: Vec<_> = rows
        .iter()
        .filter(|row| row.kind != AuditKind::Egress)
        .map(|row| (row.kind, row.call_id.as_deref()))
        .collect();
    assert_eq!(
        steps,
        [
            (AuditKind::Authorize, Some("call-mcp")),
            (AuditKind::HitlRequest, Some("call-mcp")),
            (AuditKind::HitlResolution, Some("call-mcp")),
            (AuditKind::ToolResult, Some("call-mcp")),
            (AuditKind::HitlRequest, Some("call-mcp")),
            (AuditKind::HostGrant, Some("call-mcp")),
            (AuditKind::HitlResolution, Some("call-mcp")),
            (AuditKind::ToolResult, Some("call-mcp")),
        ],
        "{rows:#?}"
    );
    let steps: Vec<_> = rows
        .iter()
        .filter(|row| row.kind != AuditKind::Egress)
        .collect();
    assert_eq!(steps[1].detail["kind"], "governance");
    assert_eq!(steps[2].detail["decision"], "allow_once");
    assert_eq!(steps[3].detail["outcome"], "sandbox_denied");
    assert_eq!(steps[3].detail["denied_host"], "127.0.0.1");
    assert_eq!(steps[4].detail["kind"], "network_host");
    assert_eq!(steps[4].detail["denied_host"], "127.0.0.1");
    assert_eq!(steps[5].detail["pattern"], "127.0.0.1");
    assert_eq!(steps[6].detail["decision"], "allow_host_for_session");
    assert_eq!(steps[7].detail["outcome"], "ok");

    // The server's connections carry the call id, not just `mcp:fixture`:
    // the refusal is recorded before the grant, the tunnel after it.
    let position = |pred: &dyn Fn(&forge_durable::AuditRow) -> bool| {
        rows.iter().position(pred).expect("row present")
    };
    let refused = position(&|row| {
        row.kind == AuditKind::Egress
            && row.call_id.as_deref() == Some("call-mcp")
            && row.detail["outcome"] == "denied"
    });
    let grant = position(&|row| row.kind == AuditKind::HostGrant);
    let allowed = position(&|row| {
        row.kind == AuditKind::Egress
            && row.call_id.as_deref() == Some("call-mcp")
            && row.detail["outcome"] == "allowed"
    });
    assert!(refused < grant && grant < allowed, "{rows:#?}");
    assert_eq!(rows[refused].detail["host"], "127.0.0.1");
}

/// A call that succeeds although the server's request was refused is a plain
/// result: no host prompt for a host the server did not need.
#[tokio::test]
async fn an_mcp_call_that_tolerates_a_refusal_does_not_prompt() {
    let dir = tempdir().unwrap();
    let Some((mut session, _manager)) = session_with_sandboxed_server(dir.path()).await else {
        return;
    };
    request_mcp_call(&mut session, "call-ok", "mcp:fixture:tolerant").await;

    session
        .decide_hitl(GrantDecision::AllowOnce, "test")
        .await
        .unwrap();
    assert!(session.pending_hitl().is_none(), "no host prompt expected");
    let result = tool_result(&session, "call-ok").expect("the call records a result");
    assert!(result.contains("status=403"), "{result}");

    let rows = session.audit_events().await.unwrap();
    assert!(
        rows.iter().any(|row| row.kind == AuditKind::Egress
            && row.call_id.as_deref() == Some("call-ok")
            && row.detail["outcome"] == "denied"),
        "the refusal is still audited under the call: {rows:#?}"
    );
    let result_row = rows
        .iter()
        .find(|row| row.kind == AuditKind::ToolResult)
        .unwrap();
    assert_eq!(result_row.detail["outcome"], "ok");
}
