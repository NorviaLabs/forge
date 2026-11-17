//! `TaskKind`: what ran a turn — the top-level session, a built-in subagent
//! (writer or read-only alike), or a named custom agent — and the text it is
//! stored and asked for as.

use forge_types::TaskKind;

#[test]
fn each_kind_has_one_text_form_and_parses_back() {
    let cases = [
        (TaskKind::Main, "main"),
        (TaskKind::Subagent, "subagent"),
        (TaskKind::Custom("reviewer".into()), "custom:reviewer"),
        (
            TaskKind::Custom("test-fixer-2".into()),
            "custom:test-fixer-2",
        ),
    ];
    for (kind, text) in cases {
        assert_eq!(kind.to_string(), text);
        assert_eq!(text.parse::<TaskKind>().unwrap(), kind, "{text}");
    }
}

#[test]
fn anything_else_is_refused() {
    for text in [
        "",
        "unknown",
        "read_only",
        "writer",
        "subagent:writer",
        "Main",
        "custom:",
        "Custom:reviewer",
        "custom:Reviewer",
        "custom:a b",
        "custom:a:b",
        " main",
    ] {
        assert!(
            text.parse::<TaskKind>().is_err(),
            "`{text}` must not parse as a task kind"
        );
    }
}

#[test]
fn serializes_as_its_text_form() {
    assert_eq!(
        serde_json::to_value(TaskKind::Custom("reviewer".into())).unwrap(),
        serde_json::json!("custom:reviewer")
    );
    assert_eq!(
        serde_json::to_value(TaskKind::Main).unwrap(),
        serde_json::json!("main")
    );
    let back: TaskKind = serde_json::from_value(serde_json::json!("subagent")).unwrap();
    assert_eq!(back, TaskKind::Subagent);
    assert!(serde_json::from_value::<TaskKind>(serde_json::json!("unknown")).is_err());
}

#[test]
fn kinds_sort_main_then_subagent_then_custom_agents_by_name() {
    let mut kinds = vec![
        TaskKind::Custom("zeta".into()),
        TaskKind::Subagent,
        TaskKind::Custom("alpha".into()),
        TaskKind::Main,
    ];
    kinds.sort();
    assert_eq!(
        kinds,
        [
            TaskKind::Main,
            TaskKind::Subagent,
            TaskKind::Custom("alpha".into()),
            TaskKind::Custom("zeta".into()),
        ]
    );
}
