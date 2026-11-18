//! The built-in tools' schemas: what the model is sent for each, checked
//! against the schema generated from the tool's argument type.

use schemars::JsonSchema;
use serde_json::Value;

/// The schema `schemars` generates for `T`, as it is before any compaction.
fn generated<T: JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).expect("a schema serializes")
}

/// Every built-in tool, with the schema generated from its argument type.
fn generated_schemas() -> Vec<(&'static str, Value)> {
    vec![
        ("read_file", generated::<crate::builtins::ReadFileArgs>()),
        ("ls", generated::<crate::builtins::LsArgs>()),
        (
            "view_image",
            generated::<crate::view_image::ViewImageArgs>(),
        ),
        ("write_file", generated::<crate::builtins::WriteFileArgs>()),
        (
            "apply_patch",
            generated::<crate::apply_patch::ApplyPatchArgs>(),
        ),
        ("bash", generated::<crate::builtins::BashArgs>()),
        ("git", generated::<crate::builtins::GitArgs>()),
        (
            "background_run",
            generated::<crate::builtins::BackgroundRunArgs>(),
        ),
        (
            "exec_command",
            generated::<crate::unified_exec::ExecCommandArgs>(),
        ),
        (
            "write_stdin",
            generated::<crate::unified_exec::WriteStdinArgs>(),
        ),
        ("update_plan", generated::<forge_types::UpdatePlanArgs>()),
        (
            "ask_user_question",
            generated::<forge_types::AskUserQuestionArgs>(),
        ),
        (
            "request_unconfined_retry",
            generated::<crate::builtins::RequestUnconfinedRetryArgs>(),
        ),
        (
            "spawn_agent",
            generated::<crate::orchestration::SpawnAgentArgs>(),
        ),
        (
            "send_message",
            generated::<crate::orchestration::AgentMessageArgs>(),
        ),
        (
            "wait_agent",
            generated::<crate::orchestration::WaitAgentArgs>(),
        ),
        (
            "set_model_tier",
            generated::<crate::orchestration::SetModelTierArgs>(),
        ),
        ("load_skill", generated::<crate::skills::LoadSkillArgs>()),
        ("web_fetch", generated::<crate::web_fetch::WebFetchArgs>()),
        ("glob", generated::<crate::fast_file_tools::GlobArgs>()),
        ("grep", generated::<crate::fast_file_tools::FffGrepArgs>()),
        ("edit", generated::<crate::edit::EditArgs>()),
    ]
}

/// The table names every built-in tool, and only those: a new tool without
/// an entry fails here rather than going unchecked below.
#[test]
fn every_built_in_tool_is_in_the_table() {
    let mut tools: Vec<String> = crate::default_builtins()
        .iter()
        .map(|tool| tool.name().to_string())
        .collect();
    let mut table: Vec<String> = generated_schemas()
        .into_iter()
        .map(|(name, _)| name.to_string())
        .collect();
    tools.sort();
    table.sort();
    assert_eq!(tools, table);
}

/// What the model is sent for each built-in tool carries no generator
/// boilerplate, and all of it together stays within budget (12,758
/// characters before compaction).
#[test]
fn built_in_schemas_are_compact() {
    let mut total = 0;
    for tool in crate::default_builtins() {
        let schema = serde_json::to_string(&tool.input_schema()).expect("a schema serializes");
        for noise in [
            "\"$schema\"",
            "\"title\"",
            "\"default\":null",
            "\"format\":\"int",
            "\"format\":\"uint",
            "\"format\":\"float\"",
            "\"format\":\"double\"",
        ] {
            assert!(
                !schema.contains(noise),
                "{}: {noise} in {schema}",
                tool.name()
            );
        }
        total += schema.len();
    }
    let budget = 10_000;
    assert!(total <= budget, "{total} characters (budget {budget})");
}

/// The paths of the errors the registry's validation finds.
fn errors(schema: &Value, arguments: &Value) -> Vec<String> {
    let validator = crate::validation::compile_validator(schema).expect("a schema compiles");
    validator
        .iter_errors(arguments)
        .map(|error| error.instance_path().to_string())
        .collect()
}

/// The schema each built-in reports accepts and rejects exactly what the
/// schema generated from its argument type does, so a call that validated
/// before still validates and one that was refused is still refused.
#[test]
fn built_in_schemas_validate_exactly_as_generated() {
    let tools = crate::default_builtins();
    for (name, generated) in generated_schemas() {
        let tool = tools
            .iter()
            .find(|tool| tool.name() == name)
            .expect("the table names a built-in");
        forge_test_support::schema_check::assert_same_validation(
            name,
            &generated,
            &tool.input_schema(),
            &errors,
        );
    }
}
