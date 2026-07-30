use forge_types::ToolValidationError;
use jsonschema::Validator;
use serde_json::Value;
use std::collections::HashMap;

/// Extract the coercible type from a JSON Schema `"type"` value.
/// Handles both `"type": "integer"` and `"type": ["integer", "null"]` (nullable).
fn schema_type(type_value: &Value) -> Option<&str> {
    if let Some(s) = type_value.as_str() {
        return Some(s);
    }
    if let Some(arr) = type_value.as_array() {
        return arr.iter().find_map(|v| {
            let s = v.as_str()?;
            if s == "null" {
                None
            } else {
                Some(s)
            }
        });
    }
    None
}

fn schema_type_label(type_value: &Value) -> String {
    if let Some(s) = type_value.as_str() {
        return s.to_string();
    }
    if let Some(arr) = type_value.as_array() {
        let parts: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
        if !parts.is_empty() {
            return parts.join(" or ");
        }
    }
    "unknown".into()
}

fn json_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Look up a property schema from a JSON Pointer-like path (`/offset` or `offset`).
fn property_schema<'a>(schema: &'a Value, path: &str) -> Option<&'a Value> {
    let key = path.trim().trim_start_matches('/').split('/').next()?;
    if key.is_empty() {
        return None;
    }
    schema.pointer(&format!("/properties/{key}"))
}

/// Build a minimal schema-shaped example object for retry feedback (types only).
fn schema_example(schema: &Value) -> Value {
    let mut obj = serde_json::Map::new();
    if let Some(props) = schema.get("properties").and_then(|p| p.as_object()) {
        for (key, prop) in props {
            let example = match prop.get("type").and_then(schema_type) {
                Some("integer") => Value::from(0),
                Some("number") => Value::from(0.0),
                Some("boolean") => Value::Bool(false),
                Some("array") => Value::Array(vec![]),
                Some("object") => Value::Object(serde_json::Map::new()),
                _ => Value::String(String::new()),
            };
            obj.insert(key.clone(), example);
        }
    }
    Value::Object(obj)
}

/// Coerce string values to match schema types (handles LLMs that stringify numbers).
/// Only pure numeric/boolean strings are coerced — composite malformed strings are left intact
/// so schema validation rejects them.
fn coerce_args(schema: &Value, args: &mut Value) {
    if let (Some(schema_obj), Some(args_obj)) = (schema.as_object(), args.as_object_mut()) {
        if let Some(properties) = schema_obj.get("properties").and_then(|p| p.as_object()) {
            for (key, prop_schema) in properties {
                if let Some(arg_value) = args_obj.get_mut(key) {
                    if let Some(expected_type) = prop_schema.get("type").and_then(schema_type) {
                        if arg_value.is_string() {
                            let s = arg_value.as_str().unwrap();
                            match expected_type {
                                "integer" => {
                                    if let Ok(n) = s.parse::<i64>() {
                                        *arg_value = Value::from(n);
                                    }
                                }
                                "number" => {
                                    if let Ok(n) = s.parse::<f64>() {
                                        *arg_value = serde_json::Number::from_f64(n)
                                            .map(Value::Number)
                                            .unwrap_or_else(|| arg_value.clone());
                                    }
                                }
                                "boolean" => match s.to_lowercase().as_str() {
                                    "true" | "1" => *arg_value = Value::Bool(true),
                                    "false" | "0" => *arg_value = Value::Bool(false),
                                    _ => {}
                                },
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Validate `args` against a JSON Schema object. Fail closed.
pub fn validate_args(tool: &str, schema: &Value, args: &Value) -> Result<(), ToolValidationError> {
    let mut args = args.clone();
    coerce_args(schema, &mut args);

    let validator = Validator::new(schema).map_err(|e| ToolValidationError {
        tool: tool.to_string(),
        path: "$".into(),
        message: format!("invalid tool schema: {e}"),
        schema_hint: None,
    })?;

    if let Err(err) = validator.validate(&args) {
        let path = err.instance_path.to_string();
        let path = if path.is_empty() {
            "$".to_string()
        } else {
            path
        };

        let expected = property_schema(schema, &path)
            .and_then(|p| p.get("type"))
            .map(schema_type_label)
            .unwrap_or_else(|| "see schema".into());

        let actual = path
            .trim_start_matches('/')
            .split('/')
            .next()
            .and_then(|key| args.get(key))
            .map(json_type_name)
            .unwrap_or("unknown");

        let example = schema_example(schema);
        let message = format!(
            "{err}. Expected {expected}, received {actual}. \
             Submit a new structured tool call with native JSON types, for example: {example}"
        );

        return Err(ToolValidationError {
            tool: tool.to_string(),
            path,
            message,
            schema_hint: Some(schema.to_string()),
        });
    }
    Ok(())
}

/// Canonical signature for repeated invalid calls (tool + field + error class).
pub fn validation_error_signature(tool: &str, path: &str, message: &str) -> String {
    let class = if message.contains("not of type") || message.contains("not of types") {
        "type_mismatch"
    } else if message.contains("required") {
        "required"
    } else {
        "other"
    };
    format!("{tool}|{path}|{class}")
}

/// Per-turn validation failure budget (max 3 per tool name and per error signature).
#[derive(Debug, Clone)]
pub struct ValidationBudget {
    counts: HashMap<String, u32>,
    signatures: HashMap<String, u32>,
    max: u32,
}

impl Default for ValidationBudget {
    fn default() -> Self {
        Self::with_default_max()
    }
}

impl ValidationBudget {
    pub fn new(max: u32) -> Self {
        Self {
            counts: HashMap::new(),
            signatures: HashMap::new(),
            max,
        }
    }

    pub fn with_default_max() -> Self {
        Self::new(3)
    }

    /// Record a validation failure. Returns Ok(retry_number) or Err if budget exhausted.
    pub fn record_failure(&mut self, tool: &str) -> Result<u32, String> {
        self.record_failure_with_signature(tool, None)
    }

    /// Record failure with optional canonical signature for repeated-error detection.
    pub fn record_failure_with_signature(
        &mut self,
        tool: &str,
        signature: Option<&str>,
    ) -> Result<u32, String> {
        if let Some(sig) = signature {
            let sc = self.signatures.entry(sig.to_string()).or_insert(0);
            *sc += 1;
            if *sc > self.max {
                return Err(format!(
                    "validation retry budget exceeded for repeated error `{sig}` (max {})",
                    self.max
                ));
            }
        }
        let c = self.counts.entry(tool.to_string()).or_insert(0);
        *c += 1;
        if *c > self.max {
            Err(format!(
                "validation retry budget exceeded for tool `{tool}` (max {})",
                self.max
            ))
        } else {
            Ok(*c)
        }
    }

    pub fn reset_turn(&mut self) {
        self.counts.clear();
        self.signatures.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn read_file_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "offset": { "type": ["integer", "null"] },
                "limit": { "type": ["integer", "null"] }
            },
            "required": ["path"]
        })
    }

    #[test]
    fn rejects_wrong_type() {
        let schema = json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"]
        });
        let err = validate_args("read_file", &schema, &json!({"path": 123})).unwrap_err();
        assert_eq!(err.tool, "read_file");
        assert!(!err.message.is_empty());
    }

    #[test]
    fn accepts_valid() {
        let schema = json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"]
        });
        validate_args("read_file", &schema, &json!({"path": "a.rs"})).unwrap();
    }

    #[test]
    fn accepts_valid_read_file_with_integers() {
        validate_args(
            "read_file",
            &read_file_schema(),
            &json!({"path": "README.md", "offset": 1, "limit": 100}),
        )
        .unwrap();
    }

    #[test]
    fn rejects_malformed_composite_offset_string() {
        // Exact observed failure class — must not be salvaged.
        let err = validate_args(
            "read_file",
            &read_file_schema(),
            &json!({"path": "README.md", "offset": "1arglimit\">100"}),
        )
        .unwrap_err();
        assert_eq!(err.path, "/offset");
        assert!(
            err.message.contains("received string") || err.message.contains("not of type"),
            "{}",
            err.message
        );
        assert!(
            err.message.contains("Expected"),
            "actionable expected type missing: {}",
            err.message
        );
        assert!(
            err.message.contains("path"),
            "schema example missing: {}",
            err.message
        );
    }

    #[test]
    fn rejects_malformed_composite_offset_retry_variant() {
        let err = validate_args(
            "read_file",
            &read_file_schema(),
            &json!({"path": "README.md", "offset": "1arglimit\">50"}),
        )
        .unwrap_err();
        assert_eq!(err.path, "/offset");
        let sig1 = validation_error_signature("read_file", &err.path, &err.message);
        let err2 = validate_args(
            "read_file",
            &read_file_schema(),
            &json!({"path": "README.md", "offset": "1arglimit\">100"}),
        )
        .unwrap_err();
        let sig2 = validation_error_signature("read_file", &err2.path, &err2.message);
        assert_eq!(sig1, sig2, "same error class should share signature");
    }

    #[test]
    fn rejects_non_numeric_string_offset() {
        let err = validate_args(
            "read_file",
            &read_file_schema(),
            &json!({"path": "x", "offset": "abc"}),
        )
        .unwrap_err();
        assert_eq!(err.path, "/offset");
    }

    #[test]
    fn rejects_array_offset() {
        let err = validate_args(
            "read_file",
            &read_file_schema(),
            &json!({"path": "x", "offset": [1]}),
        )
        .unwrap_err();
        assert_eq!(err.path, "/offset");
    }

    #[test]
    fn rejects_object_offset() {
        let err = validate_args(
            "read_file",
            &read_file_schema(),
            &json!({"path": "x", "offset": {"value": 1}}),
        )
        .unwrap_err();
        assert_eq!(err.path, "/offset");
    }

    #[test]
    fn budget_exhausts() {
        let mut b = ValidationBudget::new(3);
        assert!(b.record_failure("t").is_ok());
        assert!(b.record_failure("t").is_ok());
        assert!(b.record_failure("t").is_ok());
        assert!(b.record_failure("t").is_err());
    }

    #[test]
    fn repeated_signature_exhausts_budget() {
        let mut b = ValidationBudget::new(2);
        let sig = "read_file|/offset|type_mismatch";
        assert!(b
            .record_failure_with_signature("read_file", Some(sig))
            .is_ok());
        assert!(b
            .record_failure_with_signature("read_file", Some(sig))
            .is_ok());
        assert!(b
            .record_failure_with_signature("read_file", Some(sig))
            .is_err());
    }

    #[test]
    fn coerces_string_integer() {
        let schema = json!({
            "type": "object",
            "properties": { "offset": { "type": "integer" } }
        });
        validate_args("test", &schema, &json!({"offset": "500"})).unwrap();
    }

    #[test]
    fn coerces_string_boolean() {
        let schema = json!({
            "type": "object",
            "properties": { "flag": { "type": "boolean" } }
        });
        validate_args("test", &schema, &json!({"flag": "true"})).unwrap();
    }

    #[test]
    fn coerces_nullable_integer() {
        let schema = json!({
            "type": "object",
            "properties": { "limit": { "type": ["integer", "null"] } }
        });
        validate_args("test", &schema, &json!({"limit": "500"})).unwrap();
    }

    #[test]
    fn coerces_nullable_number() {
        let schema = json!({
            "type": "object",
            "properties": { "weight": { "type": ["number", "null"] } }
        });
        validate_args("test", &schema, &json!({"weight": "1.5"})).unwrap();
    }

    #[test]
    fn coerces_nullable_boolean() {
        let schema = json!({
            "type": "object",
            "properties": { "flag": { "type": ["boolean", "null"] } }
        });
        validate_args("test", &schema, &json!({"flag": "true"})).unwrap();
    }

    #[test]
    fn does_not_salvage_composite_numeric_string() {
        let schema = json!({
            "type": "object",
            "properties": { "offset": { "type": "integer" } }
        });
        // Must not extract "1" or "100" from composite garbage.
        assert!(validate_args("test", &schema, &json!({"offset": "1arglimit\">100"})).is_err());
    }
}
