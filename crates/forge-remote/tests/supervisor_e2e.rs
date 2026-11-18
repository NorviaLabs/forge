//! End to end against real sessions: Forge's `ProjectService` with the
//! scripted fake provider in a scratch Git repository, the remote service
//! on top, and a client pinned like a phone. No Tauri, nothing of the
//! user's touched.
//!
//! One journey, because the scripted model answers steps in order:
//! a prompt streams its reply; a gated call is allowed once from the
//! device; another is denied; a held reply is stopped.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use forge_remote::hub::HubConfig;
use forge_remote::testing::{EventStream, SseEvent, TestClient};
use forge_remote::tls::{Fingerprint, CERT_FILE};
use forge_remote::{
    Limits, ManualClock, Reach, RemoteService, RemoteSettings, ServiceOptions, SupervisorBackend,
};
use forge_session::{ProjectService, ProjectServiceHandle, ProjectServiceOptions};
use forge_types::SessionId;
use serde_json::json;

const SCENARIO: &str = r#"{"strict": true, "steps": [
  {"text": {"text": "remote pong", "chunks": ["remote ", "pong"]}},
  {"tool_calls": {"calls": [{"id": "approve-me", "name": "git", "arguments": {"subcommand": "reset", "args": ["--hard", "HEAD"]}}]}},
  {"text": {"text": "after approve"}},
  {"tool_calls": {"calls": [{"id": "deny-me", "name": "git", "arguments": {"subcommand": "clean", "args": ["-f"]}}]}},
  {"text": {"text": "after deny"}},
  {"hold": {"id": "stop-me", "before": "working on it", "then": {"text": {"text": "held answer"}}}}
]}"#;

const DEVICE: &str = "E2E Phone";

fn git(repo: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .status()
        .expect("git runs");
    assert!(status.success(), "git {args:?}");
}

/// A trusted scratch repository opened in a private `ProjectService`.
async fn project(scratch: &Path) -> (ProjectServiceHandle, forge_session::OpenProjectRef) {
    let repo = scratch.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("README.md"), "e2e\n").unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "e2e@forge.invalid"]);
    git(&repo, &["config", "user.name", "Forge E2E"]);
    git(&repo, &["add", "README.md"]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    let scenario = scratch.join("scenario.json");
    std::fs::write(&scenario, SCENARIO).unwrap();
    let store = scratch.join("trust.toml");
    forge_config::grant_trust_at(&store, &repo).unwrap();
    let mut cfg = forge_config::Config {
        resolved_workspace: repo.clone(),
        workspace_root: Some(repo.display().to_string()),
        ..Default::default()
    };
    cfg.model.mock = true;
    cfg.model.mock_script = Some(scenario);
    let projects = ProjectService::spawn(ProjectServiceOptions {
        registry: forge_session::ProjectRegistry::at(scratch.join("projects.toml")),
        trust_store: Some(store),
        load_config: Arc::new(move |_root| Ok(cfg.clone())),
    })
    .unwrap();
    let added = projects.add(repo).await.unwrap();
    projects.open(added.id).await.unwrap();
    let open = projects.project(added.id).await.unwrap();
    (projects, open)
}

/// Read events until one satisfies `done`, checking that every id is the
/// next one in sequence. Returns everything read.
async fn until(
    stream: &mut EventStream,
    last_seq: &mut u64,
    what: &str,
    done: impl Fn(&SseEvent) -> bool,
) -> Vec<SseEvent> {
    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        let event = stream
            .next_within(left)
            .await
            .unwrap_or_else(|| panic!("timed out waiting for {what}; saw {seen:#?}"));
        assert_eq!(
            event.seq(),
            *last_seq + 1,
            "events arrive in order with no gap: {event:?}"
        );
        *last_seq = event.seq();
        let finished = done(&event);
        seen.push(event);
        if finished {
            return seen;
        }
    }
}

fn has_assistant(event: &SseEvent, text: &str) -> bool {
    event.event == "session"
        && event.data["messages"].as_array().is_some_and(|m| {
            m.iter()
                .any(|m| m["role"] == "assistant" && m["text"] == text)
        })
}

fn settled(event: &SseEvent) -> bool {
    event.event == "session"
        && matches!(
            event.data["turn_state"].as_str(),
            Some("completed" | "idle" | "failed" | "cancelled" | "interrupted")
        )
}

fn files_contain(dir: &Path, needle: &[u8]) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.filter_map(Result::ok).any(|entry| {
        let path: PathBuf = entry.path();
        if path.is_dir() {
            files_contain(&path, needle)
        } else {
            std::fs::read(&path)
                .map(|bytes| bytes.windows(needle.len()).any(|w| w == needle))
                .unwrap_or(false)
        }
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_paired_device_follows_prompts_approves_denies_and_stops_a_real_session() {
    let scratch = tempfile::tempdir().unwrap();
    let (projects, open) = project(scratch.path()).await;
    let service = RemoteService::new(ServiceOptions {
        dir: scratch.path().join("remote"),
        backend: SupervisorBackend::new(projects.clone()),
        clock: Arc::new(ManualClock::new()),
        limits: Limits::default(),
        hub: HubConfig::default(),
    });
    let status = service
        .configure(RemoteSettings {
            enabled: true,
            reach: Reach::Loopback,
            address: None,
            port: 0,
        })
        .await
        .unwrap();
    let addr = status.listening.expect("listening");
    let pin =
        Fingerprint::of_der(&std::fs::read(scratch.path().join("remote").join(CERT_FILE)).unwrap());
    let client = TestClient::new(addr, pin);
    let info = service.start_pairing().unwrap();
    // The device redeems the code; the desktop allows it; the device
    // collects its token.
    let started = client.pair(&info.code, DEVICE).await.unwrap();
    assert_eq!(
        service.status().pairing_request.map(|r| r.confirmation),
        Some(started.confirmation.clone()),
        "the desktop shows the number the device shows"
    );
    service
        .decide_pairing(&started.confirmation, true)
        .await
        .unwrap();
    let token = client.token(&started.pairing_id).await.unwrap();

    // The project's session is listed.
    let list = client.get("/v1/sessions", Some(&token)).await.unwrap();
    assert_eq!(list.status, 200, "{}", list.text);
    let listed = list.body["sessions"].as_array().unwrap();
    assert!(
        !listed.is_empty(),
        "the open project has a session: {}",
        list.text
    );
    assert!(listed.iter().all(|s| s["project"] == "repo"));
    let session_id: SessionId = listed[0]["session_id"].as_str().unwrap().parse().unwrap();

    let mut stream = client
        .events(session_id, Some(&token), None)
        .await
        .unwrap_or_else(|r| panic!("{} {}", r.status, r.text));
    let first = stream.next().await.expect("the session first");
    assert_eq!((first.event.as_str(), first.seq()), ("session", 1));
    let mut last = 1;
    let prompt = format!("/v1/sessions/{session_id}/prompt");
    let approval = format!("/v1/sessions/{session_id}/approval");

    // 1. A prompt from the device reaches the session; its reply streams.
    let reply = client
        .post(
            &prompt,
            Some(&token),
            json!({"text": "ping from the phone"}),
        )
        .await
        .unwrap();
    assert_eq!(reply.status, 202, "{}", reply.text);
    let seen = until(&mut stream, &mut last, "the scripted reply", |e| {
        has_assistant(e, "remote pong") && settled(e)
    })
    .await;
    assert!(
        seen.iter().any(|e| e.event == "session"
            && e.data["messages"].as_array().is_some_and(|m| m
                .iter()
                .any(|m| m["role"] == "user" && m["text"] == "ping from the phone"))),
        "the device's prompt is in the transcript"
    );
    let streamed: String = seen
        .iter()
        .filter(|e| e.event == "text")
        .map(|e| e.data["delta"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(streamed, "remote pong", "the reply streamed as text events");

    // 2. A gated call waits; the device allows it once.
    client
        .post(&prompt, Some(&token), json!({"text": "reset the tree"}))
        .await
        .unwrap();
    let seen = until(&mut stream, &mut last, "the approval", |e| {
        e.event == "session" && e.data["pending_approval"]["call_id"] == "approve-me"
    })
    .await;
    let pending = &seen.last().unwrap().data["pending_approval"];
    assert_eq!(pending["tool"], "git");
    assert_eq!(pending["kind"], "governance");
    assert_eq!(
        pending["remote_options"],
        json!(["allow_once", "deny", "deny_with_note"])
    );
    assert_eq!(pending["desktop_required"], true);
    let stale = client
        .post(
            &approval,
            Some(&token),
            json!({"call_id": "not-this-one", "decision": "allow_once"}),
        )
        .await
        .unwrap();
    assert_eq!(stale.status, 409, "{}", stale.text);
    let allowed = client
        .post(
            &approval,
            Some(&token),
            json!({"call_id": "approve-me", "decision": "allow_once"}),
        )
        .await
        .unwrap();
    assert_eq!(allowed.status, 202, "{}", allowed.text);
    until(&mut stream, &mut last, "the turn after approval", |e| {
        has_assistant(e, "after approve") && settled(e)
    })
    .await;
    // The session journals live in the project's runtime storage
    // (`.forge/local/` here, where the configured `journal_dir` resolves).
    let journals = open.project.root.join(".forge");
    assert!(
        files_contain(&journals, format!("remote:{DEVICE}").as_bytes()),
        "the journal names the device that approved"
    );

    // 3. Another gated call; the device denies it.
    client
        .post(&prompt, Some(&token), json!({"text": "clean the tree"}))
        .await
        .unwrap();
    until(&mut stream, &mut last, "the second approval", |e| {
        e.event == "session" && e.data["pending_approval"]["call_id"] == "deny-me"
    })
    .await;
    let denied = client
        .post(
            &approval,
            Some(&token),
            json!({"call_id": "deny-me", "decision": "deny"}),
        )
        .await
        .unwrap();
    assert_eq!(denied.status, 202, "{}", denied.text);
    let seen = until(&mut stream, &mut last, "the turn after the denial", |e| {
        has_assistant(e, "after deny") && settled(e)
    })
    .await;
    let tool_said = seen.last().unwrap().data["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "tool")
        .any(|m| {
            m["text"]
                .as_str()
                .is_some_and(|t| t.contains(&format!("denied by remote:{DEVICE}")))
        });
    assert!(tool_said, "the agent is told the device denied it");

    // 4. A held reply; the device stops it.
    client
        .post(&prompt, Some(&token), json!({"text": "take your time"}))
        .await
        .unwrap();
    until(&mut stream, &mut last, "the held reply to start", |e| {
        e.event == "text"
            && e.data["delta"]
                .as_str()
                .is_some_and(|d| d.contains("working on it"))
    })
    .await;
    let stop = client
        .send(TestClient::request(
            "POST",
            &format!("/v1/sessions/{session_id}/stop"),
            Some(&token),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(stop.status, 202, "{}", stop.text);
    let seen = until(&mut stream, &mut last, "the stopped turn", settled).await;
    assert!(
        !seen.iter().any(|e| has_assistant(e, "held answer")),
        "the stopped reply never finishes"
    );

    // 5. Scroll-back: the first message of the session, by page, and every
    // update named how many messages there are.
    let last_session = seen.iter().rev().find(|e| e.event == "session").unwrap();
    let total = last_session.data["message_total"].as_u64().unwrap();
    assert!(total >= 8, "the journey left a transcript: {total}");
    let first = client
        .get(
            &format!("/v1/sessions/{session_id}/messages?before=1&limit=1"),
            Some(&token),
        )
        .await
        .unwrap();
    assert_eq!(first.status, 200, "{}", first.text);
    assert_eq!(first.body["message_offset"], 0);
    assert_eq!(first.body["messages"][0]["text"], "ping from the phone");

    service.shutdown().await;
    projects.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_privileged_request_offers_the_device_only_a_denial() {
    // The filter itself, on the payload shapes a real session raises.
    let escalation = forge_types::HitlPayload {
        call_id: "c".into(),
        tool: "bash".into(),
        args_redacted: json!({"command": "make install"}),
        reason: "the sandbox refused a write".into(),
        failure: None,
        sandbox_escalation: true,
        denied_host: None,
        denied_path: None,
        target: None,
        consent: None,
    };
    let approval = forge_remote::supervisor::remote_approval(&escalation);
    assert_eq!(
        approval.remote_options,
        [
            forge_remote::protocol::RemoteDecision::Deny,
            forge_remote::protocol::RemoteDecision::DenyWithNote
        ]
    );
    assert!(approval.desktop_required);
}
