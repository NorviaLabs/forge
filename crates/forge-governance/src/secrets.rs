use serde_json::{json, Value};
use std::collections::HashMap;
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct SecretRef {
    pub name: String,
    pub env_key: String,
}

#[derive(Debug, Clone, Default)]
pub struct SecretMaterial {
    values: HashMap<String, String>,
}

impl SecretMaterial {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(|s| s.as_str())
    }

    pub fn insert(&mut self, name: String, value: String) {
        self.values.insert(name, value);
    }
}

#[derive(Debug, Error)]
pub enum VaultError {
    #[error("secret `{0}` not found (env `{1}`)")]
    Missing(String, String),
}

pub trait SecretBroker: Send + Sync {
    fn materialize(&self, keys: &[SecretRef]) -> Result<SecretMaterial, VaultError>;
}

/// Phase 2 vault stand-in: environment variables (SEC-01).
pub struct EnvSecretBroker;

impl EnvSecretBroker {
    pub fn new() -> Self {
        Self
    }
}

impl Default for EnvSecretBroker {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretBroker for EnvSecretBroker {
    fn materialize(&self, keys: &[SecretRef]) -> Result<SecretMaterial, VaultError> {
        let mut m = SecretMaterial::default();
        for k in keys {
            match std::env::var(&k.env_key) {
                Ok(v) => m.insert(k.name.clone(), v),
                Err(_) => return Err(VaultError::Missing(k.name.clone(), k.env_key.clone())),
            }
        }
        Ok(m)
    }
}

const SECRET_KEYS: &[&str] = &[
    "token",
    "api_key",
    "apikey",
    "password",
    "secret",
    "authorization",
    "auth",
];

pub fn redact_value(v: &Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, val) in map {
                if SECRET_KEYS.iter().any(|s| k.eq_ignore_ascii_case(s)) {
                    out.insert(k.clone(), json!("[REDACTED]"));
                } else {
                    out.insert(k.clone(), redact_value(val));
                }
            }
            Value::Object(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(redact_value).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_nested() {
        let v = json!({"a": {"password": "x"}, "b": 1});
        let r = redact_value(&v);
        assert_eq!(r["a"]["password"], "[REDACTED]");
        assert_eq!(r["b"], 1);
    }
}
