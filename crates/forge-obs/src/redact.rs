use serde_json::{json, Value};

const SECRET_KEYS: &[&str] = &[
    "token",
    "api_key",
    "apikey",
    "password",
    "secret",
    "authorization",
    "auth",
    "bearer",
];

#[derive(Debug, Clone, Default)]
pub struct Redactor;

impl Redactor {
    pub fn redact_value(&self, _key: &str, v: &Value) -> Value {
        match v {
            Value::Object(map) => {
                let mut out = serde_json::Map::new();
                for (k, val) in map {
                    if SECRET_KEYS.iter().any(|s| k.eq_ignore_ascii_case(s)) {
                        out.insert(k.clone(), json!("[REDACTED]"));
                    } else {
                        out.insert(k.clone(), self.redact_value(k, val));
                    }
                }
                Value::Object(out)
            }
            Value::Array(a) => Value::Array(a.iter().map(|x| self.redact_value("", x)).collect()),
            other => other.clone(),
        }
    }

    pub fn redact_attr(&self, key: &str, value: &str) -> String {
        if SECRET_KEYS.iter().any(|s| key.eq_ignore_ascii_case(s)) {
            "[REDACTED]".into()
        } else if value.to_ascii_lowercase().contains("bearer ") {
            "[REDACTED]".into()
        } else {
            value.into()
        }
    }
}
