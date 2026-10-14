use forge_model::{common_prefix_len, snapshot_prompt, strip_cache_control};
use serde_json::json;
use sha2::{Digest, Sha256};

#[test]
fn snapshot_digest_matches_the_emitted_bytes_after_inline_stripping() {
    let wire = json!({
        "tools": [{
            "name": "read_file",
            "cache_control": {"type": "ephemeral"},
            "input_schema": {"type": "object"}
        }],
        "instructions": [
            {"type": "text", "text": "stay focused", "cache_control": {"type": "ephemeral"}}
        ],
        "input": [{
            "role": "user",
            "content": [
                {"type": "input_text", "text": "hello", "cache_control": {"type": "ephemeral"}}
            ]
        }]
    });

    let snapshot = snapshot_prompt(&wire);
    assert_eq!(
        snapshot.sha256,
        format!("{:x}", Sha256::digest(&snapshot.bytes))
    );
    assert_eq!(
        snapshot.bytes,
        snapshot_prompt(&strip_cache_control(&wire)).bytes
    );
    assert!(!String::from_utf8_lossy(&snapshot.bytes).contains("cache_control"));
}

#[test]
fn codex_input_items_remain_append_only_snapshot_parts() {
    let first = json!({
        "instructions": "stay focused",
        "input": [{"role": "user", "content": "one"}]
    });
    let second = json!({
        "instructions": "stay focused",
        "input": [
            {"role": "user", "content": "one"},
            {"role": "assistant", "content": "two"}
        ]
    });

    let a = snapshot_prompt(&first);
    let b = snapshot_prompt(&second);
    assert_eq!(common_prefix_len(&a.bytes, &b.bytes), a.bytes.len());
}
