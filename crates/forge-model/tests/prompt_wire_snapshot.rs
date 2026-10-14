use forge_model::{snapshot_prompt, strip_cache_control};
use serde_json::json;
use sha2::{Digest, Sha256};

#[test]
fn snapshot_hash_is_digest_of_emitted_bytes() {
    let wire = json!({
        "tools": [{"name": "read_file", "cache_control": {"type": "ephemeral"}}],
        "system": [{"type": "text", "text": "sys", "cache_control": {"type": "ephemeral"}}],
        "messages": [
            {
                "role": "user",
                "content": [
                    {"type": "text", "text": "hello", "cache_control": {"type": "ephemeral"}},
                    {"type": "text", "text": "world"}
                ]
            }
        ],
        "model": "ignored-by-snapshot-only-when-not-projected"
    });

    let snapshot = snapshot_prompt(&wire);
    let expected = format!("{:x}", Sha256::digest(&snapshot.bytes));

    assert_eq!(snapshot.sha256, expected);
    assert!(!String::from_utf8_lossy(&snapshot.bytes).contains("cache_control"));
}

#[test]
fn snapshot_matches_stripped_prompt_bytes_for_codex_shape() {
    let wire = json!({
        "instructions": [
            {"type": "text", "text": "You are Forge.", "cache_control": {"type": "ephemeral"}}
        ],
        "input": [
            {"role": "user", "content": [{"type": "input_text", "text": "fix it"}]},
            {"role": "assistant", "content": [{"type": "output_text", "text": "done", "cache_control": {"type": "ephemeral"}}]}
        ],
        "tools": [{"name": "apply_patch"}],
        "cache_control": {"type": "ephemeral"}
    });

    let snapshot = snapshot_prompt(&wire);
    let stripped_snapshot = snapshot_prompt(&strip_cache_control(&wire));

    assert_eq!(snapshot.bytes, stripped_snapshot.bytes);
    assert_eq!(snapshot.sha256, stripped_snapshot.sha256);
    assert!(String::from_utf8_lossy(&snapshot.bytes).contains("input_text"));
}
