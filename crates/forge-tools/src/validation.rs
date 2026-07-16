use forge_types::ToolValidationError;
use jsonschema::Validator;
use serde_json::Value;
use std::collections::HashMap;

/// Validate `args` against a JSON Schema object. Fail closed.
pub fn validate_args(tool: &str, schema: &Value, args: &Value) -> Result<(), ToolValidationError> {
    let validator = Validator::new(schema).map_err(|e| ToolValidationError {
        tool: tool.to_string(),
        path: "$".into(),
        message: format!("invalid tool schema: {e}"),
        schema_hint: None,
    })?;

    if let Err(err) = validator.validate(args) {
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
}
