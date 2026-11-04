//! The Claude Agent wire against a scripted `claude` on in-memory pipes.
//!
//! The fake answers like Claude Code does on the wire: `initialize`, a
//! `system/init` per turn, streamed API messages, and — for a prompt that
//! asks for a tool — a held MCP `tools/call` that only completes once Forge
//! sends the tool's result.

use std::sync::{Arc, Mutex as StdMutex};

use forge_types::{Message, MessageRole, SideEffectClass, ToolCall, ToolDescriptor};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::process::{ClaudeProcess, Launch, SessionMode};
use super::{ClaudeAgentSessions, Spawner};
use crate::native::tests::test_client;
use crate::native::NativeModelClient;
use crate::{ModelClient, ModelRequest};

const MODEL: &str = "claude_agent/claude-haiku-5-5";

/// What the fakes saw.
#[derive(Default)]
struct Record {
    /// `(session mode, launch key)` per process started.
    spawns: Vec<(SessionMode, String)>,
    /// Every line Forge wrote, per process.
    received: Vec<Vec<Value>>,
}

fn client(record: &Arc<StdMutex<Record>>) -> NativeModelClient {
    let mut client = test_client(
        forge_connect::builtin::builtin_profile("claude_agent"),
        "https://api.anthropic.com",
        None,
    );
    let record = Arc::clone(record);
    let spawner: Spawner = Arc::new(move |launch: Launch, session: SessionMode| {
        let record = Arc::clone(&record);
        Box::pin(async move {
            let index = {
                let mut record = record.lock().unwrap();
                record.spawns.push((session.clone(), launch.key()));
                record.received.push(Vec::new());
                record.received.len() - 1
            };
            let (forge_side, fake_side) = tokio::io::duplex(1 << 20);
            let (forge_read, forge_write) = tokio::io::split(forge_side);
            let (fake_read, fake_write) = tokio::io::split(fake_side);
            tokio::spawn(fake_claude(fake_read, fake_write, record, index));
            let mut process =
                ClaudeProcess::attach(&launch, forge_read, Box::new(forge_write), None);
            process.initialize(&launch).await?;
            Ok(process)
        })
    });
    client.claude_sessions = ClaudeAgentSessions {
        spawner: Some(spawner),
        ..Default::default()
    };
    client
}

async fn fake_claude(
    reader: impl tokio::io::AsyncRead + Unpin,
    mut writer: impl tokio::io::AsyncWrite + Unpin,
    record: Arc<StdMutex<Record>>,
    index: usize,
) {
    let mut lines = BufReader::new(reader).lines();
    // The MCP request id of a held tools/call, and its tool-use id.
    let mut held: Option<String> = None;
    // A turn that never finishes on its own.
    let mut stalled = false;
    let mut calls = 0;
    while let Ok(Some(line)) = lines.next_line().await {
        let message: Value = serde_json::from_str(&line).unwrap();
        record.lock().unwrap().received[index].push(message.clone());
        let mut out: Vec<Value> = Vec::new();
        match message["type"].as_str() {
            Some("control_request") => {
                let id = message["request_id"].clone();
                let response = match message["request"]["subtype"].as_str() {
                    Some("initialize") => json!({
                        "claude_code_version": "2.1.318",
                        "account": {"email": "a@example.com", "subscriptionType": "Claude Max"},
                        "models": [],
                    }),
                    Some("interrupt") => json!({"still_queued": []}),
                    _ => json!({}),
                };
                out.push(json!({"type": "control_response", "response": {"subtype": "success", "request_id": id, "response": response}}));
                if message["request"]["subtype"] == "interrupt"
                    && (held.take().is_some() || std::mem::take(&mut stalled))
                {
                    // The interrupted turn's leftovers, then its result.
                    out.push(stream(json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "LEFTOVER"}})));
                    out.push(json!({"type": "result", "subtype": "success", "is_error": false, "result": ""}));
                }
            }
            Some("control_response") => {
                // Forge answered the held tools/call.
                if held.as_deref() == message["response"]["request_id"].as_str() {
                    held = None;
                    let result = &message["response"]["response"]["mcp_response"]["result"];
                    let text = result["content"][0]["text"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();
                    out.extend(text_reply(&format!("got: {text}")));
                }
            }
            Some("user") => {
                let text: String = message["message"]["content"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|block| block["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("|");
                out.push(json!({"type": "system", "subtype": "init", "mcp_servers": [{"name": "forge", "status": "connected"}]}));
                if text.contains("use tool") {
                    calls += 1;
                    let tool_id = format!("toolu_{calls}");
                    let request_id = format!("mcp-{calls}");
                    out.extend([
                        stream(json!({"type": "message_start", "message": {"id": format!("msg_t{calls}"), "usage": {"input_tokens": 10, "output_tokens": 1}}})),
                        stream(json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": tool_id, "name": "mcp__forge__lookup", "input": {}}})),
                        stream(json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "{\"key\":\"a\"}"}})),
                        json!({"type": "assistant", "parent_tool_use_id": null, "message": {"id": format!("msg_t{calls}"), "content": [{"type": "tool_use", "id": tool_id, "name": "mcp__forge__lookup", "input": {"key": "a"}}]}}),
                        stream(json!({"type": "message_stop"})),
                        json!({"type": "control_request", "request_id": request_id, "request": {"subtype": "mcp_message", "server_name": "forge", "message": {"jsonrpc": "2.0", "id": calls, "method": "tools/call", "params": {"name": "lookup", "arguments": {"key": "a"}, "_meta": {"claudecode/toolUseId": tool_id}}}}}),
                    ]);
                    held = Some(request_id);
                } else if text.contains("stall") {
                    out.push(stream(json!({"type": "message_start", "message": {"id": "msg_s", "usage": {"input_tokens": 1}}})));
                    stalled = true;
                } else {
                    out.extend(text_reply(&format!("echo: {text}")));
                }
            }
            _ => {}
        }
        for message in out {
            let mut line = message.to_string();
            line.push('\n');
            if writer.write_all(line.as_bytes()).await.is_err() {
                return;
            }
        }
        let _ = writer.flush().await;
    }
}

fn stream(event: Value) -> Value {
    json!({"type": "stream_event", "event": event, "parent_tool_use_id": null})
}

fn text_reply(text: &str) -> Vec<Value> {
    vec![
        stream(
            json!({"type": "message_start", "message": {"id": "msg_text", "usage": {"input_tokens": 5, "cache_read_input_tokens": 20, "output_tokens": 1}}}),
        ),
        stream(
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        ),
        stream(
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text}}),
        ),
        stream(json!({"type": "message_delta", "usage": {"output_tokens": 7}})),
        stream(json!({"type": "message_stop"})),
        json!({"type": "result", "subtype": "success", "is_error": false, "result": text}),
    ]
}

fn lookup() -> ToolDescriptor {
    ToolDescriptor {
        name: "lookup".into(),
        description: "Look up a marker.".into(),
        input_schema: json!({"type": "object", "properties": {"key": {"type": "string"}}}),
        side_effect_class: SideEffectClass::Read,
        idempotent: true,
    }
}

fn request(messages: Vec<Message>, session: Option<&str>) -> ModelRequest {
    ModelRequest {
        messages: messages.into(),
        tools: vec![lookup()],
        model: MODEL.into(),
        workspace_root: std::env::temp_dir(),
        session_id: session.map(str::to_string),
        reasoning_effort: None,
        thinking_enabled: true,
        prompt_cache: true,
        side_call: false,
    }
}

fn user(text: &str) -> Message {
    Message::new(MessageRole::User, text)
}

fn assistant(text: &str) -> Message {
    Message::new(MessageRole::Assistant, text)
}

/// The user messages one process received, as their joined text.
fn user_texts(record: &Arc<StdMutex<Record>>, process: usize) -> Vec<String> {
    record.lock().unwrap().received[process]
        .iter()
        .filter(|message| message["type"] == "user")
        .map(|message| {
            assert_eq!(message["client_composed"], json!(true));
            message["message"]["content"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|block| block["text"].as_str())
                .collect::<Vec<_>>()
                .join("|")
        })
        .collect()
}

#[tokio::test]
async fn a_session_keeps_one_process_and_sends_only_new_messages() {
    let record = Arc::new(StdMutex::new(Record::default()));
    let client = client(&record);
    let system = Message::new(MessageRole::System, "Forge rules");

    let first = client
        .complete(request(vec![system.clone(), user("hi")], Some("s1")))
        .await
        .unwrap();
    assert_eq!(first.text, "echo: hi");
    let usage = first.usage.unwrap();
    assert_eq!(usage.prompt_tokens, 25);
    assert_eq!(usage.completion_tokens, 7);

    let second = client
        .complete(request(
            vec![system, user("hi"), assistant("echo: hi"), user("again")],
            Some("s1"),
        ))
        .await
        .unwrap();
    assert_eq!(second.text, "echo: again");

    let record_guard = record.lock().unwrap();
    assert_eq!(record_guard.spawns.len(), 1, "the process is reused");
    assert!(matches!(record_guard.spawns[0].0, SessionMode::New(_)));
    let init = &record_guard.received[0][0];
    assert_eq!(init["request"]["subtype"], "initialize");
    assert!(init["request"]["appendSystemPrompt"]
        .as_str()
        .unwrap()
        .ends_with("Forge rules"));
    assert_eq!(
        init["request"]["sdkMcpServerManifests"]["forge"]["toolsListResult"]["tools"][0]["name"],
        "lookup"
    );
    drop(record_guard);
    assert_eq!(user_texts(&record, 0), ["hi", "again"]);
}

#[tokio::test]
async fn tool_calls_park_the_turn_until_forge_sends_results() {
    let record = Arc::new(StdMutex::new(Record::default()));
    let client = client(&record);
    let mut messages = vec![user("please use tool")];

    let step = client
        .complete(request(messages.clone(), Some("s2")))
        .await
        .unwrap();
    assert_eq!(step.tool_calls.len(), 1);
    assert_eq!(step.tool_calls[0].name, "lookup");
    assert_eq!(step.tool_calls[0].arguments, json!({"key": "a"}));

    let mut reply = assistant("");
    reply.tool_calls = step.tool_calls.clone();
    messages.push(reply);
    let call = &step.tool_calls[0];
    messages.push(Message::from_tool_output(
        call,
        &forge_types::ToolOutput::success("MARKER"),
    ));
    let done = client
        .complete(request(messages, Some("s2")))
        .await
        .unwrap();
    assert_eq!(done.text, "got: MARKER");
    assert!(done.tool_calls.is_empty());
    // The result went back over MCP; no second user message was sent.
    assert_eq!(user_texts(&record, 0), ["please use tool"]);
    assert_eq!(record.lock().unwrap().spawns.len(), 1);
}

#[tokio::test]
async fn a_cancelled_tool_interrupts_and_starts_a_new_turn() {
    let record = Arc::new(StdMutex::new(Record::default()));
    let client = client(&record);
    let mut messages = vec![user("use tool now")];
    let step = client
        .complete(request(messages.clone(), Some("s3")))
        .await
        .unwrap();
    let mut reply = assistant("");
    reply.tool_calls = step.tool_calls.clone();
    messages.push(reply);
    messages.push(Message::from_tool_output(
        &step.tool_calls[0],
        &forge_types::ToolOutput::cancelled("stopped"),
    ));
    messages.push(user("do something else"));

    let next = client
        .complete(request(messages, Some("s3")))
        .await
        .unwrap();
    let texts = user_texts(&record, 0);
    assert_eq!(texts.len(), 2);
    assert!(texts[1].contains("<tool-result>") && texts[1].contains("stopped"));
    assert!(texts[1].ends_with("do something else"));
    assert!(next.text.starts_with("echo: "));
    let received = &record.lock().unwrap().received[0];
    assert!(received
        .iter()
        .any(|message| message["request"]["subtype"] == "interrupt"));
}

#[tokio::test]
async fn history_claude_did_not_write_is_sent_as_a_checkpoint() {
    let record = Arc::new(StdMutex::new(Record::default()));
    let client = client(&record);
    // Another model answered first; this is Claude's first turn.
    let mut other = assistant("");
    other.tool_calls = vec![ToolCall {
        id: "call_9".into(),
        name: "lookup".into(),
        arguments: json!({"key": "b"}),
    }];
    let mut result = Message::new(MessageRole::Tool, "BETA");
    result.tool_call_id = Some("call_9".into());
    result.name = Some("lookup".into());
    let response = client
        .complete(request(
            vec![user("start"), other, result, user("continue")],
            Some("s4"),
        ))
        .await
        .unwrap();
    assert!(response.text.contains("conversation-checkpoint"));
    let texts = user_texts(&record, 0);
    assert!(texts[0].contains("<tool-result>") && texts[0].contains("BETA"));
    assert!(texts[0].ends_with("continue"));
}

#[tokio::test]
async fn an_edited_history_starts_a_new_claude_conversation() {
    let record = Arc::new(StdMutex::new(Record::default()));
    let client = client(&record);
    client
        .complete(request(vec![user("one")], Some("s5")))
        .await
        .unwrap();
    // The user undid "one" and asked something else.
    client
        .complete(request(vec![user("two")], Some("s5")))
        .await
        .unwrap();
    let spawns = &record.lock().unwrap().spawns;
    assert_eq!(spawns.len(), 2);
    match (&spawns[0].0, &spawns[1].0) {
        (SessionMode::New(a), SessionMode::New(b)) => assert_ne!(a, b),
        other => panic!("expected two new conversations, got {other:?}"),
    }
}

#[tokio::test]
async fn changed_tools_restart_the_process_and_resume_the_conversation() {
    let record = Arc::new(StdMutex::new(Record::default()));
    let client = client(&record);
    client
        .complete(request(vec![user("one")], Some("s6")))
        .await
        .unwrap();
    let mut next = request(
        vec![user("one"), assistant("echo: one"), user("two")],
        Some("s6"),
    );
    next.tools.push(ToolDescriptor {
        name: "mcp:github:issue".into(),
        ..lookup()
    });
    let response = client.complete(next).await.unwrap();
    assert_eq!(response.text, "echo: two");
    let spawns = &record.lock().unwrap().spawns;
    assert_eq!(spawns.len(), 2);
    match (&spawns[0].0, &spawns[1].0) {
        (SessionMode::New(a), SessionMode::Resume(b)) => assert_eq!(a, b),
        other => panic!("expected a resume, got {other:?}"),
    }
}

#[tokio::test]
async fn side_calls_run_in_a_throwaway_process() {
    let record = Arc::new(StdMutex::new(Record::default()));
    let client = client(&record);
    client
        .complete(request(vec![user("hi")], Some("s7")))
        .await
        .unwrap();
    let mut compaction = request(
        vec![
            user("hi"),
            assistant("echo: hi"),
            user("Summarize the conversation"),
        ],
        Some("s7"),
    );
    compaction.side_call = true;
    let summary = client.complete(compaction).await.unwrap();
    assert!(summary.text.ends_with("Summarize the conversation"));
    {
        let record = record.lock().unwrap();
        assert_eq!(record.spawns.len(), 2);
        assert_eq!(record.spawns[1].0, SessionMode::Ephemeral);
        let init = &record.received[1][0];
        assert!(init["request"].get("sdkMcpServers").is_none(), "no tools");
    }
    // The session's own conversation continues untouched.
    let next = client
        .complete(request(
            vec![user("hi"), assistant("echo: hi"), user("next")],
            Some("s7"),
        ))
        .await
        .unwrap();
    assert_eq!(next.text, "echo: next");
    assert_eq!(user_texts(&record, 0), ["hi", "next"]);
}

/// Against the installed, signed-in `claude`. Spends a little of the
/// login's usage, so it only runs on request:
/// `cargo test -p forge-model --locked live_claude -- --ignored`.
#[tokio::test]
#[ignore = "runs the real `claude` and uses its login"]
async fn live_claude_runs_a_tool_round_trip() {
    let client = test_client(
        forge_connect::builtin::builtin_profile("claude_agent"),
        "https://api.anthropic.com",
        None,
    );
    let session = uuid::Uuid::new_v4().to_string();
    let mut messages = vec![user(
        "Call the lookup tool once with key \"a\". Then reply with only the marker it returns.",
    )];
    let mut live = request(messages.clone(), Some(&session));
    live.reasoning_effort = Some("low".into());
    let step = client.complete(live.clone()).await.unwrap();
    assert_eq!(step.tool_calls.len(), 1, "{step:?}");
    assert_eq!(step.tool_calls[0].name, "lookup");

    let mut reply = assistant(&step.text);
    reply.tool_calls = step.tool_calls.clone();
    messages.push(reply);
    messages.push(Message::from_tool_output(
        &step.tool_calls[0],
        &forge_types::ToolOutput::success("COPPER-OTTER"),
    ));
    live.messages = messages.clone().into();
    let done = client.complete(live.clone()).await.unwrap();
    assert!(done.text.contains("COPPER-OTTER"), "{done:?}");
    assert!(done.usage.is_some());

    let mut side = live;
    messages.push(assistant(&done.text));
    messages.push(user("Reply with only the word SUMMARY."));
    side.messages = messages.into();
    side.side_call = true;
    let summary = client.complete(side).await.unwrap();
    assert!(summary.text.contains("SUMMARY"), "{summary:?}");
}

#[tokio::test]
async fn a_dropped_request_interrupts_claude_and_skips_its_leftovers() {
    let record = Arc::new(StdMutex::new(Record::default()));
    let client = client(&record);
    // The user stops Forge mid-reply: the request future is dropped.
    let stopped = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        client.complete(request(vec![user("stall forever")], Some("s8"))),
    )
    .await;
    assert!(stopped.is_err());

    let next = client
        .complete(request(
            vec![user("stall forever"), user("hello again")],
            Some("s8"),
        ))
        .await
        .unwrap();
    assert_eq!(next.text, "echo: hello again");
    assert_eq!(record.lock().unwrap().spawns.len(), 1);
    let received = &record.lock().unwrap().received[0];
    let interrupt = received
        .iter()
        .position(|message| message["request"]["subtype"] == "interrupt")
        .expect("interrupt sent");
    let second_turn = received
        .iter()
        .rposition(|message| message["type"] == "user")
        .unwrap();
    assert!(
        interrupt < second_turn,
        "the interrupt reaches Claude first"
    );
}
