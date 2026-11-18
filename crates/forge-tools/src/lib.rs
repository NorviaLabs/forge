//! Tool protocol (tool-protocol.md) — CORE-01.

mod apply_patch;
mod builtins;
pub mod consent;
mod credentials;
mod edit;
pub mod egress;
pub mod egress_connect;
mod fast_file_tools;
mod git_network;
pub mod ingress;
mod own_git;
pub mod sign_broker;
pub mod ssh_broker;
mod tls_intercept;
pub mod write_gate;
pub mod write_grants;
#[doc(hidden)]
pub use git_network::set_egress_connect_exe;
pub use own_git::{
    install_git_network, register_workspace_egress, remote_write_targets, WorkspaceEgress,
};
mod orchestration;
mod output_filters;
mod registry;
pub mod sandbox;
#[cfg(test)]
mod schema_tests;
pub mod screenshots;
pub mod shell_spawn;
mod skills;
pub mod target;
mod unified_exec;
mod validation;
mod view_image;
pub mod web_app;
mod web_fetch;

pub use apply_patch::{ApplyPatchArgs, ApplyPatchTool};
pub use builtins::{
    default_builtins, is_provider_credential_env, provider_credential_env_names, run_shell_command,
    run_shell_command_observed, run_shell_command_unconfined, run_shell_command_with_egress,
    run_shell_command_with_egress_and_temp, run_shell_job, strip_credentials, AskUserQuestionTool,
    BashTool, GitTool, LsTool, ReadFileTool, ShellOutputCapture, ShellOutputSnapshot,
    UpdatePlanTool, WriteFileTool, PROVIDER_CREDENTIAL_ENV,
};
pub use builtins::{set_process_group, ProcessGroupGuard};
pub use consent::{ConsentFact, ConsentKind, ConsentRequest, SessionConsents};
pub use edit::{EditArgs, EditTool};
pub use orchestration::{
    AgentMessageAction, AgentMessageArgs, AgentMode, SendMessageTool, SetModelTierArgs,
    SetModelTierTool, SpawnAgentArgs, SpawnAgentTool, WaitAgentArgs, WaitAgentTool,
};
pub use output_filters::compress_tool_output;
pub use registry::{
    canonical_tool_name, canonicalize_tool_call, SessionTempDir, ToolContext, ToolRegistry,
    ValidatedToolCall,
};
pub use skills::{LoadSkillArgs, LoadSkillTool};
pub use unified_exec::{unified_exec_tools, ExecCommandTool, WriteStdinTool};
pub use validation::{
    compile_validator, validate_args, validation_error_signature, ValidationBudget,
};
pub use view_image::{ViewImageArgs, ViewImageTool};
pub use web_fetch::{web_fetch_tool, WebFetchArgs, WebFetchTool};

use async_trait::async_trait;
use forge_types::{
    ExecutionOutcome, SideEffectClass, ToolDescriptor, ToolOutput, ToolValidationError,
};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ToolError {
    #[error(transparent)]
    Validation(#[from] ToolValidationError),
    #[error("unknown tool `{0}`")]
    Unknown(String),
    #[error("execution failed: {0}")]
    Execution(String),
    #[error("sandbox denied command: {reason}")]
    SandboxDenied {
        content: String,
        reason: String,
        denied_host: Option<String>,
    },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// The tool needs the user's consent before it acts (screen control, a
    /// form submit). The session raises it as an approval and re-runs the
    /// call once it is granted.
    #[error("needs your consent: {}", .0.summary)]
    ConsentRequired(Box<ConsentRequest>),
}

impl ToolError {
    /// Maps an error surfacing from `ToolRegistry::call` (i.e. the tool
    /// never produced a `ToolOutput` at all) to the outcome that should be
    /// recorded. `Validation` errors are a protocol-shape retry mechanism
    /// handled separately by callers and are not expected here.
    pub fn as_outcome(&self) -> ExecutionOutcome {
        match self {
            ToolError::Validation(_) => ExecutionOutcome::Failed { exit_code: None },
            ToolError::Unknown(_)
            | ToolError::Execution(_)
            | ToolError::SandboxDenied { .. }
            | ToolError::Io(_) => ExecutionOutcome::SpawnFailed {
                reason: self.to_string(),
            },
            ToolError::ConsentRequired(request) => ExecutionOutcome::Denied {
                reason: format!("waiting for consent: {}", request.summary),
            },
        }
    }
}

/// A tool's input schema as the model needs it: the schema generator's
/// boilerplate (`$schema`, titles, `default: null`, number `format`s such as
/// `uint32`, the root description the tool's own description already gives)
/// removed, and
/// definitions referenced through `$defs` written in place. Optional fields
/// keep accepting `null`.
pub fn compact_schema(mut schema: Value) -> Value {
    let defs = schema
        .as_object_mut()
        .and_then(|root| root.remove("$defs"))
        .unwrap_or(Value::Null);
    if let Some(root) = schema.as_object_mut() {
        root.remove("$schema");
        root.remove("description");
    }
    inline_and_trim(&mut schema, &defs, &mut Vec::new());
    if schema.to_string().contains("\"$ref\"") {
        // Something could not be written in place: keep the definitions.
        if let (Some(root), false) = (schema.as_object_mut(), defs.is_null()) {
            root.insert("$defs".into(), defs);
        }
    }
    schema
}

/// A number format `schemars` writes for a Rust integer or float type.
fn is_number_format(format: &str) -> bool {
    matches!(
        format,
        "int"
            | "int8"
            | "int16"
            | "int32"
            | "int64"
            | "int128"
            | "uint"
            | "uint8"
            | "uint16"
            | "uint32"
            | "uint64"
            | "uint128"
            | "float"
            | "double"
    )
}

fn definition<'a>(reference: &str, defs: &'a Value) -> Option<&'a Value> {
    defs.get(reference.strip_prefix("#/$defs/")?)
}

/// Whether writing `definition` into `node` (a schema with a `$ref` to it)
/// would overwrite one of the node's own rules: a key both have with
/// different values. Annotations may differ; the node's own wins.
fn conflicts_with(node: &serde_json::Map<String, Value>, definition: &Value) -> bool {
    const ANNOTATIONS: [&str; 8] = [
        "description",
        "title",
        "default",
        "examples",
        "deprecated",
        "readOnly",
        "writeOnly",
        "$comment",
    ];
    let Some(fields) = definition.as_object() else {
        return false;
    };
    fields.iter().any(|(key, value)| {
        !ANNOTATIONS.contains(&key.as_str()) && node.get(key).is_some_and(|own| own != value)
    })
}

/// `expanding` holds the definitions being written in place around this
/// point: a reference back to one of them (a recursive type) stays a `$ref`.
fn inline_and_trim(value: &mut Value, defs: &Value, expanding: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            map.remove("title");
            // The generator's number formats (`uint32`, `double`) only
            // restate the Rust type; a format that says what a string is
            // (`uri`, `date-time`) stays.
            if map
                .get("format")
                .and_then(Value::as_str)
                .is_some_and(is_number_format)
            {
                map.remove("format");
            }
            if map.get("default") == Some(&Value::Null) {
                map.remove("default");
            }
            // `anyOf: [{$ref: E}, {type: null}]` for an enum E: the enum, with
            // null allowed.
            let nullable_enum = map.get("anyOf").and_then(Value::as_array).and_then(|any| {
                let [first, second] = any.as_slice() else {
                    return None;
                };
                let (reference, null) = if first.get("$ref").is_some() {
                    (first, second)
                } else {
                    (second, first)
                };
                if null.get("type") != Some(&Value::String("null".into())) {
                    return None;
                }
                let resolved = definition(reference.get("$ref")?.as_str()?, defs)?;
                let mut values = resolved.get("enum")?.as_array()?.clone();
                values.push(Value::Null);
                Some(values)
            });
            if let Some(values) = nullable_enum {
                map.remove("anyOf");
                map.insert("enum".into(), Value::Array(values));
            }
            let mut expanded = false;
            let reference = map.get("$ref").and_then(Value::as_str).map(str::to_string);
            if let Some(reference) = reference.filter(|reference| !expanding.contains(reference)) {
                let resolved = definition(&reference, defs)
                    .filter(|resolved| !conflicts_with(map, resolved))
                    .cloned();
                if let Some(resolved) = resolved {
                    map.remove("$ref");
                    if let Value::Object(fields) = resolved {
                        for (key, field) in fields {
                            map.entry(key).or_insert(field);
                        }
                    }
                    expanding.push(reference);
                    expanded = true;
                }
            }
            for (keyword, field) in map.iter_mut() {
                match keyword.as_str() {
                    // Literal values: data, never schema.
                    "enum" | "const" | "default" | "examples" => {}
                    // Names mapped to schemas: the names are not keywords.
                    "properties" | "patternProperties" | "$defs" | "definitions"
                    | "dependentSchemas" => {
                        if let Value::Object(named) = field {
                            for schema in named.values_mut() {
                                inline_and_trim(schema, defs, expanding);
                            }
                        }
                    }
                    _ => inline_and_trim(field, defs, expanding),
                }
            }
            if expanded {
                expanding.pop();
            }
        }
        Value::Array(items) => {
            for item in items {
                inline_and_trim(item, defs, expanding);
            }
        }
        _ => {}
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> Value;
    fn side_effect_class(&self) -> SideEffectClass;
    fn idempotent(&self) -> bool {
        false
    }
    /// Safe to execute concurrently with other read-only tool calls.
    fn parallel_safe(&self) -> bool {
        false
    }

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError>;

    /// Whether the tool can be used right now: turned on, and supported by
    /// this machine. An unavailable tool is not offered to the model (unless
    /// it was already called in the conversation, which providers require to
    /// stay declared) and a call to it is refused.
    fn is_available(&self, _ctx: &ToolContext) -> bool {
        true
    }

    /// Whether the tool only makes sense for a model that can see images
    /// (it returns screenshots to look at).
    fn requires_image_input(&self) -> bool {
        false
    }

    /// Release any process handles that can outlive one tool call.
    ///
    /// Most tools are stateless. Session-scoped tools such as
    /// `exec_command` override this hook so a repository session can retire
    /// all of its child processes before its worktree is removed.
    async fn shutdown(&self) {}

    /// Stop session-owned work that may outlive one tool call when its turn is
    /// cancelled. Stateless tools keep the no-op default.
    async fn cancel_session(&self, _session_id: forge_types::SessionId) {}

    /// The session's turn ended (finished, failed or cancelled; not paused
    /// on an approval). Tools holding something for the turn let it go.
    async fn turn_ended(&self, _session_id: forge_types::SessionId) {}

    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: self.name().to_string(),
            description: self.description().to_string(),
            input_schema: self.input_schema(),
            side_effect_class: self.side_effect_class(),
            idempotent: self.idempotent(),
        }
    }

    /// Start any workspace-scoped background work this tool wants ready
    /// before the first call. Default is a no-op.
    fn warm_workspace(&self, _root: &std::path::Path) {}
}

#[cfg(test)]
mod tests {
    use super::{Tool, ToolError};
    use forge_types::{ExecutionOutcome, SideEffectClass, ToolValidationError};
    use serde_json::json;

    struct ReadOnlyTool;

    #[async_trait::async_trait]
    impl Tool for ReadOnlyTool {
        fn name(&self) -> &str {
            "test"
        }

        fn description(&self) -> &str {
            "test tool"
        }

        fn input_schema(&self) -> serde_json::Value {
            json!({"type": "object"})
        }

        fn side_effect_class(&self) -> SideEffectClass {
            SideEffectClass::Read
        }

        fn parallel_safe(&self) -> bool {
            true
        }

        async fn call(
            &self,
            _ctx: &super::ToolContext,
            _args: serde_json::Value,
        ) -> Result<forge_types::ToolOutput, ToolError> {
            unreachable!()
        }
    }

    #[test]
    fn tool_errors_map_to_their_recorded_outcomes() {
        let validation = ToolError::Validation(ToolValidationError {
            tool: "read_file".into(),
            path: "path".into(),
            message: "required".into(),
            schema_hint: None,
        });
        assert_eq!(
            validation.as_outcome(),
            ExecutionOutcome::Failed { exit_code: None }
        );

        for error in [
            ToolError::Unknown("missing".into()),
            ToolError::Execution("failed".into()),
            ToolError::SandboxDenied {
                content: "denied".into(),
                reason: "policy".into(),
                denied_host: Some("example.test".into()),
            },
            ToolError::Io(std::io::Error::other("broken")),
        ] {
            assert!(matches!(
                error.as_outcome(),
                ExecutionOutcome::SpawnFailed { .. }
            ));
        }
    }

    /// A consent request is not a failure of the tool: it is recorded as a
    /// denial until the user answers.
    #[test]
    fn a_consent_request_is_recorded_as_denied() {
        let error = ToolError::ConsentRequired(Box::new(forge_types::ConsentRequest::new(
            forge_types::ConsentKind::FormSubmit,
            "example.com",
            "Submit the form",
        )));
        assert!(error.to_string().contains("Submit the form"));
        assert!(matches!(
            error.as_outcome(),
            ExecutionOutcome::Denied { .. }
        ));
    }

    /// Rules beside a reference apply together with the definition's. Where
    /// one would overwrite the other the reference stays; beside a mere
    /// annotation it is still written in place.
    #[test]
    fn a_compact_schema_keeps_every_rule_beside_a_reference() {
        let generated = serde_json::json!({
            "$defs": {"Small": {"type": "integer", "maximum": 10}},
            "type": "object",
            "properties": {
                "n": {"$ref": "#/$defs/Small", "maximum": 100},
                "m": {"$ref": "#/$defs/Small", "description": "A small number."}
            }
        });
        let compact = super::compact_schema(generated.clone());
        assert_eq!(
            compact["properties"]["m"],
            serde_json::json!({"type": "integer", "maximum": 10, "description": "A small number."}),
            "an annotation does not stop inlining"
        );
        let before = crate::validation::compile_validator(&generated).unwrap();
        let after = crate::validation::compile_validator(&compact).unwrap();
        for sample in [
            serde_json::json!({"n": 5}),
            serde_json::json!({"n": 50}),
            serde_json::json!({"n": 500}),
            serde_json::json!({"m": 50}),
        ] {
            assert_eq!(
                after.is_valid(&sample),
                before.is_valid(&sample),
                "{sample}"
            );
        }
    }

    /// A type that refers to itself (a node whose `next` is another node)
    /// cannot be written out in place: the reference stays, with its
    /// definition, and the schema validates as before.
    #[test]
    fn a_compact_schema_survives_a_recursive_type() {
        let generated = serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "$defs": {
                "Node": {
                    "type": "object",
                    "properties": {
                        "next": {"anyOf": [{"$ref": "#/$defs/Node"}, {"type": "null"}]}
                    },
                    "additionalProperties": false
                }
            },
            "type": "object",
            "properties": {"root": {"$ref": "#/$defs/Node"}},
            "required": ["root"]
        });
        let compact = super::compact_schema(generated.clone());
        assert!(compact.to_string().contains("\"$ref\""), "{compact}");
        assert!(compact.get("$defs").is_some(), "{compact}");
        let before = crate::validation::compile_validator(&generated).unwrap();
        let after = crate::validation::compile_validator(&compact).unwrap();
        for sample in [
            serde_json::json!({"root": {}}),
            serde_json::json!({"root": {"next": {"next": {"next": null}}}}),
            serde_json::json!({"root": {"next": {"next": 5}}}),
            serde_json::json!({"root": {"next": {"other": 1}}}),
        ] {
            assert_eq!(
                after.is_valid(&sample),
                before.is_valid(&sample),
                "{sample}"
            );
        }
    }

    /// Names are not keywords: an argument called `title` (or `format`,
    /// `default`, `enum`) is kept, and so are literal values inside `enum`,
    /// `const` and `default`, which are data, not schema.
    #[test]
    fn a_compact_schema_keeps_arguments_named_like_keywords() {
        let generated = serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "title": {"type": "string", "title": "Title"},
                "format": {"type": "string"},
                "default": {"type": ["string", "null"], "default": null},
                "enum": {"type": "string"},
                "shape": {
                    "enum": [{"title": "circle"}],
                    "default": {"title": "circle"}
                }
            },
            "required": ["title"]
        });
        let compact = super::compact_schema(generated.clone());
        let properties = compact["properties"].as_object().unwrap();
        for name in ["title", "format", "default", "enum", "shape"] {
            assert!(properties.contains_key(name), "{name} is gone: {compact}");
        }
        assert_eq!(
            compact["properties"]["title"],
            serde_json::json!({"type": "string"})
        );
        assert_eq!(
            compact["properties"]["shape"],
            serde_json::json!({"enum": [{"title": "circle"}], "default": {"title": "circle"}}),
            "literal values are data"
        );
        let before = crate::validation::compile_validator(&generated).unwrap();
        let after = crate::validation::compile_validator(&compact).unwrap();
        let call = serde_json::json!({"title": "Fix the build", "format": "md", "default": null});
        assert!(before.is_valid(&call));
        assert!(after.is_valid(&call), "a valid call is still valid");
    }

    /// Only the generator's number formats are noise; a format that says
    /// what a string is (a URI, a date) stays.
    #[test]
    fn a_compact_schema_keeps_formats_that_mean_something() {
        let compact = super::compact_schema(serde_json::json!({
            "type": "object",
            "properties": {
                "link": {"type": "string", "format": "uri"},
                "when": {"type": "string", "format": "date-time"},
                "count": {"type": "integer", "format": "uint32"},
                "offset": {"type": "integer", "format": "int64"},
                "ratio": {"type": "number", "format": "double"}
            }
        }));
        assert_eq!(compact["properties"]["link"]["format"], "uri");
        assert_eq!(compact["properties"]["when"]["format"], "date-time");
        for number in ["count", "offset", "ratio"] {
            assert!(
                compact["properties"][number].get("format").is_none(),
                "{number}: {}",
                compact["properties"][number]
            );
        }
    }

    #[test]
    fn a_compact_schema_keeps_only_what_the_model_needs() {
        let generated = serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "ClickArgs",
            "type": "object",
            "additionalProperties": false,
            "$defs": {"Button": {"type": "string", "enum": ["left", "right"]}},
            "properties": {
                "button": {
                    "anyOf": [{"$ref": "#/$defs/Button"}, {"type": "null"}],
                    "default": null,
                    "description": "Which button."
                },
                "x": {"type": "integer", "format": "int32", "description": "x."},
                "double": {"type": ["boolean", "null"], "default": null}
            },
            "required": ["x"]
        });
        let compact = super::compact_schema(generated);
        assert_eq!(
            compact,
            serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "button": {"enum": ["left", "right", null], "description": "Which button."},
                    "x": {"type": "integer", "description": "x."},
                    "double": {"type": ["boolean", "null"]}
                },
                "required": ["x"]
            })
        );
    }

    #[test]
    fn computer_use_hooks_default_to_available_and_text_only() {
        let ctx = super::ToolContext::new(std::path::PathBuf::from("."));
        assert!(ReadOnlyTool.is_available(&ctx));
        assert!(!ReadOnlyTool.requires_image_input());
        assert!(!ctx
            .consents
            .is_granted(forge_types::ConsentKind::ScreenControl, "screen"));
    }

    #[test]
    fn default_tool_contract_builds_a_descriptor() {
        let descriptor = ReadOnlyTool.descriptor();
        assert_eq!(descriptor.name, "test");
        assert_eq!(descriptor.description, "test tool");
        assert!(descriptor.input_schema.is_object());
        assert!(!descriptor.idempotent);
        assert!(ReadOnlyTool.parallel_safe());
        ReadOnlyTool.warm_workspace(std::path::Path::new("."));
    }
}
