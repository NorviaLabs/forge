use forge_types::ToolValidationError;
use jsonschema::Validator;
use serde_json::Value;
use std::collections::HashMap;

/// Coerce string values to match schema types (handles LLMs that stringify numbers).
fn coerce_args(schema: &Value, args: &mut Value) {
    if let (Some(schema_obj), Some(args_obj)) = (schema.as_object(), args.as_object_mut()) {
        if let Some(properties) = schema_obj.get("properties").and_then(|p| p.as_object()) {
            for (key, prop_schema) in properties {
                if let Some(arg_value) = args_obj.get_mut(key) {
                    if let Some(expected_type) = prop_schema.get("type").and_then(|t| t.as_str()) {
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
                                "boolean" => {
                                    match s.to_lowercase().as_str() {
                                        "true" | "1" => *arg_value = Value::Bool(true),
                                        "false" | "0" => *arg_value = Value::Bool(false),
                                        _ => {}
                                    }
                                }
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
        return Err(ToolValidationError {
            tool: tool.to_string(),
            path: if path.is_empty() { "$".into() } else { path },
            message: err.to_string(),
            schema_hint: Some(schema.to_string()),
        });
    }
    Ok(())
}

/// Per-turn validation failure budget (max 3 per tool name).
#[derive(Debug, Default)]
pub struct ValidationBudget {
    counts: HashMap<String, u32>,
    max: u32,
}

impl ValidationBudget {
    pub fn new(max: u32) -> Self {
        Self {
            counts: HashMap::new(),
            max,
        }
    }

    pub fn with_default_max() -> Self {
        Self::new(3)
    }

    /// Record a validation failure. Returns Err if budget exhausted.
    pub fn record_failure(&mut self, tool: &str) -> Result<u32, String> {
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
    fn budget_exhausts() {
        let mut b = ValidationBudget::new(3);
        assert!(b.record_failure("t").is_ok());
        assert!(b.record_failure("t").is_ok());
        assert!(b.record_failure("t").is_ok());
        assert!(b.record_failure("t").is_err());
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
}
