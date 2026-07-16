//! LiteLLM worker wire protocol (litellm-wire.md) — NDJSON v1.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const WIRE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WireType {
    Request,
    Response,
    Event,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireEnvelope {
    pub v: u32,
    pub id: String,
    #[serde(rename = "type")]
    pub msg_type: WireType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<WireErrorBody>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WireErrorBody {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompleteParams {
    pub model: String,
    pub messages: Vec<Value>,
    #[serde(default)]
    pub tools: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Value>,
}

impl WireEnvelope {
    pub fn request(id: impl Into<String>, method: impl Into<String>, params: Value) -> Self {
        Self {
            v: WIRE_VERSION,
            id: id.into(),
            msg_type: WireType::Request,
            method: Some(method.into()),
            params: Some(params),
            result: None,
            error: None,
        }
    }

    pub fn ping(id: impl Into<String>) -> Self {
        Self::request(id, "ping", Value::Object(Default::default()))
    }

    pub fn shutdown(id: impl Into<String>) -> Self {
        Self::request(id, "shutdown", Value::Object(Default::default()))
    }

    pub fn complete(
        id: impl Into<String>,
        params: &CompleteParams,
    ) -> Result<Self, serde_json::Error> {
        Ok(Self::request(id, "complete", serde_json::to_value(params)?))
    }

    pub fn complete_stream(
        id: impl Into<String>,
        params: &CompleteParams,
    ) -> Result<Self, serde_json::Error> {
        Ok(Self::request(
            id,
            "complete_stream",
            serde_json::to_value(params)?,
        ))
    }

    pub fn encode_line(&self) -> Result<String, serde_json::Error> {
        let mut s = serde_json::to_string(self)?;
        s.push('\n');
        Ok(s)
    }

    pub fn decode_line(line: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(line.trim())
    }

    pub fn is_error(&self) -> bool {
        matches!(self.msg_type, WireType::Error) || self.error.is_some()
    }
}

/// Known error codes from litellm-wire.md.
pub mod error_codes {
    pub const PROTOCOL: &str = "protocol";
    pub const INVALID_PARAMS: &str = "invalid_params";
    pub const UPSTREAM_AUTH: &str = "upstream_auth";
    pub const UPSTREAM_RATE_LIMIT: &str = "upstream_rate_limit";
    pub const UPSTREAM: &str = "upstream";
    pub const INTERNAL: &str = "internal";
    pub const CANCELLED: &str = "cancelled";
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ping_roundtrip() {
        let line = WireEnvelope::ping("1").encode_line().unwrap();
        assert!(line.ends_with('\n'));
        let env = WireEnvelope::decode_line(&line).unwrap();
        assert_eq!(env.v, 1);
        assert_eq!(env.id, "1");
        assert_eq!(env.msg_type, WireType::Request);
        assert_eq!(env.method.as_deref(), Some("ping"));
    }

    #[test]
    fn complete_params_serialize() {
        let p = CompleteParams {
            model: "openai/gpt-4o".into(),
            messages: vec![json!({"role":"user","content":"hi"})],
            tools: vec![],
            temperature: None,
            max_tokens: Some(100),
            extra: None,
        };
        let env = WireEnvelope::complete("9", &p).unwrap();
        let line = env.encode_line().unwrap();
        let back = WireEnvelope::decode_line(&line).unwrap();
        assert_eq!(back.method.as_deref(), Some("complete"));
        let params = back.params.unwrap();
        assert_eq!(params["model"].as_str().unwrap(), "openai/gpt-4o");
    }

    #[test]
    fn error_envelope() {
        let env = WireEnvelope {
            v: 1,
            id: "2".into(),
            msg_type: WireType::Error,
            method: None,
            params: None,
            result: None,
            error: Some(WireErrorBody {
                code: error_codes::UPSTREAM_AUTH.into(),
                message: "bad key".into(),
                data: None,
            }),
        };
        assert!(env.is_error());
        let line = env.encode_line().unwrap();
        let back = WireEnvelope::decode_line(&line).unwrap();
        assert_eq!(back.error.unwrap().code, "upstream_auth");
    }

    #[test]
    fn stream_event_shape() {
        let env = WireEnvelope {
            v: 1,
            id: "s".into(),
            msg_type: WireType::Event,
            method: None,
            params: Some(json!({"kind":"text_delta","text":"Hi"})),
            result: None,
            error: None,
        };
        let line = env.encode_line().unwrap();
        let back = WireEnvelope::decode_line(&line).unwrap();
        assert_eq!(back.msg_type, WireType::Event);
        assert_eq!(back.params.unwrap()["kind"], "text_delta");
    }

    #[test]
    fn rejects_non_json() {
        assert!(WireEnvelope::decode_line("not-json").is_err());
    }
}
