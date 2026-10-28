//! Wire protocols (design §4): one module per `WireKind`, chosen by `match`.
//!
//! Each protocol owns its body, endpoint path, stream parsing with
//! truncation detection, error mapping, prompt projection, and cache key:
//!
//! - `chat_completions`: `<base>/chat/completions`; a missing `[DONE]` or
//!   `finish_reason = length` is truncation.
//! - `responses`: `<base>/responses` on a gateway, or the ChatGPT flavor's
//!   `<base>/codex/responses`; a missing `response.completed` is truncation.
//! - `anthropic_messages`: `<base>/v1/messages`; a missing `message_stop`
//!   or `stop_reason = max_tokens` is truncation.

use forge_connect::{Attach, WireKind};
use forge_types::ModelResponse;

use super::{anthropic_messages, chat_completions, responses, NativeModelClient, Target};
use crate::{ModelError, ModelRequest, StreamEventTx};

/// Send `req` over `wire` and parse the reply.
pub(super) async fn complete(
    client: &NativeModelClient,
    req: ModelRequest,
    target: Target,
    wire: WireKind,
    tx: Option<StreamEventTx>,
) -> Result<ModelResponse, ModelError> {
    match wire {
        WireKind::ChatCompletions => chat_completions::complete(client, req, target, tx).await,
        WireKind::AnthropicMessages => anthropic_messages::complete(client, req, target, tx).await,
        // The ChatGPT flavor owns its own Responses endpoint and headers.
        WireKind::Responses if target.attach == Attach::BearerAccountId => {
            responses::complete_chatgpt(client, req, target, tx).await
        }
        WireKind::Responses => responses::complete_gateway(client, req, target, tx).await,
    }
}

/// The compared prompt (tools, system, messages) as `wire` sends it.
pub(super) fn prompt_projection(
    client: &NativeModelClient,
    req: &ModelRequest,
    wire: WireKind,
) -> serde_json::Value {
    match wire {
        WireKind::AnthropicMessages => anthropic_messages::prompt_projection(client, req),
        WireKind::Responses => {
            // The wire takes the bare model id.
            let bare = req
                .model
                .split_once('/')
                .map_or(req.model.as_str(), |(_, m)| m);
            crate::prompt_wire::prompt_object_from_body(responses::request_body(
                client,
                req,
                bare,
                &responses::tool_aliases(req),
            ))
        }
        WireKind::ChatCompletions => crate::prompt_wire::openai_compat_prompt_cached(req, client),
    }
}

/// The prompt-cache namespace for `wire`.
pub(super) fn cache_key(wire: WireKind) -> &'static str {
    match wire {
        WireKind::AnthropicMessages => "anthropic_messages",
        WireKind::Responses => "codex",
        WireKind::ChatCompletions => "openai_compat",
    }
}
