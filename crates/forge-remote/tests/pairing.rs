//! Invariants: a pairing code works once and expires; the token a device
//! gets is never stored in plain text.

mod common;

use std::time::Duration;

use base64::Engine;
use common::Harness;
use forge_remote::pairing::{CODE_TTL, MAX_ATTEMPTS, REQUEST_TTL};
use forge_remote::token::{DeviceToken, TokenHash, TOKEN_PREFIX};
use serde_json::json;

fn wrong_code(code: &str) -> String {
    let first = if code.starts_with('0') { '1' } else { '0' };
    format!("{first}{}", &code[1..])
}

#[tokio::test]
async fn a_code_opens_a_request_and_no_token_exists_until_the_desktop_allows_it() {
    let harness = Harness::new();
    let (addr, client) = harness.enable().await;
    let info = harness.service.start_pairing().unwrap();
    assert_eq!(info.replaces, None);
    assert_eq!(
        info.link,
        format!(
            "forge-remote://{addr}/?code={}&fp={}",
            info.code,
            harness.pin().to_pin()
        )
    );
    assert_eq!(info.fingerprint, harness.pin().display_groups());

    let reply = client
        .post(
            "/v1/pair",
            None,
            json!({"code": info.code, "device_name": "Priya's iPhone"}),
        )
        .await
        .unwrap();
    assert_eq!(reply.status, 202, "{}", reply.text);
    assert!(
        reply.body.get("token").is_none(),
        "no token before the desktop decides"
    );
    let pairing_id = reply.body["pairing_id"].as_str().unwrap().to_string();
    let confirmation = reply.body["confirmation"].as_str().unwrap().to_string();
    assert_eq!(pairing_id.len(), 43);
    assert_eq!(confirmation.len(), 6);
    assert_eq!(
        reply
            .headers
            .get(hyper::header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("no-store")
    );

    let request = harness
        .service
        .status()
        .pairing_request
        .expect("the desktop is asked");
    assert_eq!(request.device_name, "Priya's iPhone");
    assert_eq!(
        request.confirmation, confirmation,
        "both screens show one number"
    );
    assert_eq!(
        harness.service.status().device,
        None,
        "nothing is paired yet"
    );

    let pending = client.collect(&pairing_id).await;
    assert_eq!(pending.status, 202);
    assert_eq!(pending.body, json!({"status": "pending"}));
    assert_eq!(
        pending
            .headers
            .get(hyper::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok()),
        Some("1")
    );

    harness
        .service
        .decide_pairing(&confirmation, true)
        .await
        .unwrap();
    assert_eq!(harness.service.status().pairing_request, None);
    let collected = client.collect(&pairing_id).await;
    assert_eq!(collected.status, 200, "{}", collected.text);
    let token = collected.body["token"].as_str().unwrap();
    assert!(DeviceToken::parse(token).is_some());
    assert_eq!(collected.body["device_name"], "Priya's iPhone");
    assert_eq!(
        collected
            .headers
            .get(hyper::header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("no-store")
    );
    assert_eq!(
        client.get("/v1/hello", Some(token)).await.unwrap().status,
        200
    );
    assert_eq!(
        client.collect(&pairing_id).await.status,
        404,
        "a token is collected once"
    );

    let again = client.pair(&info.code, "Another").await.unwrap_err();
    assert_eq!(again.status, 401, "the code worked once");
    assert_eq!(again.body, json!({"error": "invalid or expired code"}));
    assert!(harness
        .activity()
        .iter()
        .any(|line| line["event"] == "pairing_requested" && line["device"] == "Priya's iPhone"));
}

#[tokio::test]
async fn a_refused_request_pairs_nothing() {
    let harness = Harness::new();
    let (_, client) = harness.enable().await;
    let info = harness.service.start_pairing().unwrap();
    let started = client.pair(&info.code, "Intruder").await.unwrap();
    harness
        .service
        .decide_pairing(&started.confirmation, false)
        .await
        .unwrap();
    let refused = client.collect(&started.pairing_id).await;
    assert_eq!(refused.status, 403);
    assert_eq!(
        refused.body,
        json!({"error": "pairing was refused on the desktop"})
    );
    let status = harness.service.status();
    assert_eq!(status.device, None);
    assert_eq!(status.pairing_request, None);
    assert!(harness
        .activity()
        .iter()
        .any(|line| line["event"] == "pairing_declined" && line["device"] == "Intruder"));
}

#[tokio::test]
async fn an_unanswered_request_expires() {
    let harness = Harness::new();
    let (_, client) = harness.enable().await;
    let info = harness.service.start_pairing().unwrap();
    let started = client.pair(&info.code, "Phone").await.unwrap();
    harness.clock.advance(REQUEST_TTL);
    assert_eq!(client.collect(&started.pairing_id).await.status, 404);
    assert_eq!(harness.service.status().pairing_request, None);
    assert!(harness
        .service
        .decide_pairing(&started.confirmation, true)
        .await
        .is_err());
    assert_eq!(harness.service.status().device, None);
}

#[tokio::test]
async fn a_decision_for_another_number_changes_nothing() {
    let harness = Harness::new();
    let (_, client) = harness.enable().await;
    let info = harness.service.start_pairing().unwrap();
    let started = client.pair(&info.code, "Phone").await.unwrap();
    let other = if started.confirmation == "000000" {
        "111111"
    } else {
        "000000"
    };
    assert!(harness.service.decide_pairing(other, true).await.is_err());
    assert_eq!(
        client.collect(&started.pairing_id).await.status,
        202,
        "still pending"
    );
    assert_eq!(harness.service.status().device, None);
}

#[tokio::test]
async fn a_new_code_cancel_or_turning_off_ends_the_request() {
    for ending in ["new code", "cancel", "off"] {
        let harness = Harness::new();
        let (_, client) = harness.enable().await;
        let info = harness.service.start_pairing().unwrap();
        let started = client.pair(&info.code, "Phone").await.unwrap();
        match ending {
            "new code" => {
                harness.service.start_pairing().unwrap();
            }
            "cancel" => harness.service.cancel_pairing(),
            _ => {
                harness
                    .service
                    .configure(forge_remote::RemoteSettings {
                        enabled: false,
                        ..Harness::on()
                    })
                    .await
                    .unwrap();
            }
        }
        assert_eq!(harness.service.status().pairing_request, None, "{ending}");
        assert!(
            harness
                .service
                .decide_pairing(&started.confirmation, true)
                .await
                .is_err(),
            "{ending}"
        );
        if ending != "off" {
            assert_eq!(
                client.collect(&started.pairing_id).await.status,
                404,
                "{ending}"
            );
        }
    }
}

#[tokio::test]
async fn the_paired_device_keeps_working_until_the_new_one_collects() {
    let harness = Harness::new();
    let (_, client, old) = harness.paired().await;
    let session_id = forge_types::SessionId::new_v4();
    harness
        .backend
        .put(forge_remote::testing::session(session_id, "s"));
    let mut stream = client.events(session_id, Some(&old), None).await.unwrap();
    stream.next().await.unwrap();

    let info = harness.service.start_pairing().unwrap();
    assert_eq!(info.replaces.as_deref(), Some("Phone"));
    let started = client.pair(&info.code, "Tablet").await.unwrap();
    harness
        .service
        .decide_pairing(&started.confirmation, true)
        .await
        .unwrap();
    assert_eq!(
        client.get("/v1/hello", Some(&old)).await.unwrap().status,
        200,
        "allowed but not collected: the old device still works"
    );
    let new = client.token(&started.pairing_id).await.unwrap();
    assert_eq!(
        client.get("/v1/hello", Some(&old)).await.unwrap().status,
        401
    );
    assert_eq!(
        client.get("/v1/hello", Some(&new)).await.unwrap().status,
        200
    );
    assert!(stream.ends_within(Duration::from_secs(5)).await);
}

#[tokio::test]
async fn unknown_pairing_ids_all_look_alike() {
    let harness = Harness::new();
    let (_, client) = harness.enable().await;
    let info = harness.service.start_pairing().unwrap();
    client.pair(&info.code, "Phone").await.unwrap();
    let mut bodies = std::collections::HashSet::new();
    for id in ["A".repeat(43), "nope".into(), "x".repeat(400)] {
        let reply = client.collect(&id).await;
        assert_eq!(reply.status, 404, "{}", id.len());
        bodies.insert(reply.text);
    }
    assert_eq!(bodies.len(), 1);
    let post = client
        .send(forge_remote::testing::TestClient::request(
            "POST",
            &format!("/v1/pair/{}", "A".repeat(43)),
            None,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(post.status, 405);
}

#[tokio::test]
async fn an_expired_code_is_refused() {
    let harness = Harness::new();
    let (_, client) = harness.enable().await;
    let info = harness.service.start_pairing().unwrap();
    harness.clock.advance(CODE_TTL);
    let refused = client.pair(&info.code, "Phone").await.unwrap_err();
    assert_eq!(refused.status, 401);
    assert_eq!(refused.body, json!({"error": "invalid or expired code"}));
    assert_eq!(harness.service.status().device, None);
}

#[tokio::test]
async fn a_code_just_inside_its_lifetime_still_works() {
    let harness = Harness::new();
    let (_, client) = harness.enable().await;
    let info = harness.service.start_pairing().unwrap();
    harness.clock.advance(CODE_TTL - Duration::from_secs(1));
    assert!(client.pair(&info.code, "Phone").await.is_ok());
}

#[tokio::test]
async fn wrong_guesses_burn_the_code() {
    let harness = Harness::new();
    let (_, client) = harness.enable().await;
    let info = harness.service.start_pairing().unwrap();
    let wrong = wrong_code(&info.code);
    for _ in 0..MAX_ATTEMPTS {
        let refused = client.pair(&wrong, "Phone").await.unwrap_err();
        assert_eq!(refused.status, 401);
        assert_eq!(refused.body, json!({"error": "invalid or expired code"}));
    }
    let refused = client.pair(&info.code, "Phone").await.unwrap_err();
    assert_eq!(refused.status, 401, "the right code no longer works");
    assert_eq!(harness.service.status().pairing_expires_at, None);
}

#[tokio::test]
async fn of_several_devices_racing_with_one_code_exactly_one_pairs() {
    let harness = Harness::new();
    let (_, client) = harness.enable().await;
    let info = harness.service.start_pairing().unwrap();
    let attempts: Vec<_> = (0..8)
        .map(|n| {
            let (client, code) = (client.clone(), info.code.clone());
            tokio::spawn(async move { client.pair(&code, &format!("Device {n}")).await.is_ok() })
        })
        .collect();
    let mut winners = 0;
    for attempt in attempts {
        winners += usize::from(attempt.await.unwrap());
    }
    assert_eq!(winners, 1);
}

#[tokio::test]
async fn without_a_code_nothing_pairs() {
    let harness = Harness::new();
    let (_, client) = harness.enable().await;
    let refused = client.pair("ABCD-EFGH", "Phone").await.unwrap_err();
    assert_eq!(refused.status, 401);
}

#[tokio::test]
async fn a_bad_request_does_not_use_the_code_up() {
    let harness = Harness::new();
    let (_, client) = harness.enable().await;
    let info = harness.service.start_pairing().unwrap();
    for body in [
        json!({"code": info.code}),
        json!({"code": info.code, "device_name": ""}),
        json!({"code": info.code, "device_name": "line\nbreak"}),
        json!({"code": info.code, "device_name": "x".repeat(65)}),
        json!("not an object"),
    ] {
        let reply = client.post("/v1/pair", None, body.clone()).await.unwrap();
        assert_eq!(reply.status, 400, "{body}: {}", reply.text);
    }
    let reply = client
        .send(forge_remote::testing::TestClient::request(
            "POST",
            "/v1/pair",
            None,
            Some(b"{ not json".to_vec()),
        ))
        .await
        .unwrap();
    assert_eq!(reply.status, 400);
    assert!(
        client.pair(&info.code, "Phone").await.is_ok(),
        "malformed requests never count against the code"
    );
}

#[tokio::test]
async fn a_device_name_with_hidden_characters_is_shown_escaped() {
    let harness = Harness::new();
    let (_, client) = harness.enable().await;
    let info = harness.service.start_pairing().unwrap();
    let started = client.pair(&info.code, "Phone\u{202E}enohP").await.unwrap();
    assert_eq!(
        harness
            .service
            .status()
            .pairing_request
            .map(|r| r.device_name),
        Some("Phone⟨U+202E⟩enohP".into()),
        "the desktop is asked with the name escaped"
    );
    harness.allow_and_collect(&client, &started).await;
    assert_eq!(
        harness.service.status().device.map(|d| d.name),
        Some("Phone⟨U+202E⟩enohP".into())
    );
}

#[tokio::test]
async fn the_token_is_never_stored_in_plain_text() {
    let harness = Harness::new();
    let (_, client, token) = harness.paired().await;
    // Use it, so `last_seen` and the activity log are written too.
    assert_eq!(
        client.get("/v1/hello", Some(&token)).await.unwrap().status,
        200
    );
    harness.service.shutdown().await;

    let secret = &token[TOKEN_PREFIX.len()..];
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(secret)
        .unwrap();
    let forms = [
        token.clone(),
        secret.to_string(),
        hex::encode(&raw),
        hex::encode(&raw).to_uppercase(),
        base64::engine::general_purpose::STANDARD.encode(&raw),
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(&raw),
    ];
    let mut checked = 0;
    for entry in walk(&harness.temp.path().to_path_buf()) {
        let bytes = std::fs::read(&entry).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        for form in &forms {
            assert!(
                !text.contains(form.as_str()),
                "{} holds the token",
                entry.display()
            );
        }
        assert!(
            !bytes.windows(raw.len()).any(|w| w == raw.as_slice()),
            "{} holds the token's raw bytes",
            entry.display()
        );
        checked += 1;
    }
    assert!(
        checked >= 3,
        "settings, device and certificate files were checked"
    );

    let record: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(harness.dir().join("devices.json")).unwrap())
            .unwrap();
    let parsed = DeviceToken::parse(&token).unwrap();
    assert_eq!(record["token_sha256"], TokenHash::of(&parsed).to_hex());
}

fn walk(dir: &std::path::PathBuf) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(walk(&path));
        } else {
            files.push(path);
        }
    }
    files
}

#[tokio::test]
async fn pairing_is_recorded_without_the_token() {
    let harness = Harness::new();
    let (_, _, token) = harness.paired().await;
    let lines = harness.activity();
    assert!(lines
        .iter()
        .any(|line| line["event"] == "paired" && line["device"] == "Phone"));
    assert!(!serde_json::to_string(&lines).unwrap().contains(&token));
}

#[tokio::test]
async fn guesses_are_recorded_and_a_burnt_code_is_explained() {
    let harness = Harness::new();
    let (_, client) = harness.enable().await;
    let info = harness.service.start_pairing().unwrap();
    let wrong = wrong_code(&info.code);
    for _ in 0..MAX_ATTEMPTS {
        client.pair(&wrong, "Intruder").await.unwrap_err();
    }
    let refusals: Vec<_> = harness
        .activity()
        .into_iter()
        .filter(|line| line["event"] == "pairing_refused")
        .collect();
    assert!(!refusals.is_empty(), "wrong codes leave a trace");
    assert!(
        refusals.len() <= 2,
        "summarised, not one line each: {refusals:?}"
    );
    let status = harness.service.status();
    assert_eq!(status.pairing_expires_at, None);
    assert!(
        status
            .pairing_error
            .as_deref()
            .is_some_and(|why| why.contains("wrong")),
        "{status:?}"
    );
    harness.service.start_pairing().unwrap();
    assert_eq!(
        harness.service.status().pairing_error,
        None,
        "a new code clears it"
    );
}
