//! Typed failures for credential verification and reachability probes.
//!
//! These checks previously returned `Result<(), String>`, which forced callers to
//! parse message text to tell an auth rejection from a rate limit or an
//! unreachable host. The variants below carry the HTTP status where the server
//! produced one, so that decision is structural.
//!
//! `Display` output is deliberately byte-identical to the strings these checks
//! used to return, and — as before — never contains the credential.

use thiserror::Error;

/// A credential-verification or reachability probe failed.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum VerifyError {
    #[error("API key is empty")]
    EmptyKey,

    #[error("API key looks too short ({len} chars). {guidance}")]
    KeyTooShort { len: usize, guidance: &'static str },

    /// Server rejected the credential outright (401/403). Retrying will not help.
    #[error("{provider} rejected the API key (unauthorized). {guidance}")]
    Unauthorized {
        provider: &'static str,
        guidance: &'static str,
    },

    /// Server rejected the credential and reported a status.
    #[error("{provider} rejected the API key (HTTP {status}). {guidance}")]
    Rejected {
        provider: &'static str,
        status: u16,
        guidance: &'static str,
    },

    /// Non-success status that is not an authentication failure.
    #[error("{provider} key verification failed (HTTP {status}).{}", .guidance.map(|g| format!(" {g}")).unwrap_or_default())]
    Status {
        provider: &'static str,
        status: u16,
        guidance: Option<&'static str>,
    },

    /// An OAuth token could not be parsed, or lacked a required claim.
    #[error("{0}")]
    MalformedToken(&'static str),

    /// Reached the service but it reported itself unhealthy.
    #[error("{provider} responded HTTP {status} at {endpoint}. {guidance}")]
    Unhealthy {
        provider: &'static str,
        status: u16,
        endpoint: String,
        guidance: &'static str,
    },

    /// Could not reach the service at all. `detail` is the transport error, which
    /// is why this variant carries a string: the category is what callers branch
    /// on, and the text is only for the operator.
    #[error("{message}")]
    Unreachable {
        provider: &'static str,
        message: String,
    },
}

impl VerifyError {
    /// HTTP status the server returned, when there was one.
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Rejected { status, .. }
            | Self::Status { status, .. }
            | Self::Unhealthy { status, .. } => Some(*status),
            Self::Unauthorized { .. } => None,
            _ => None,
        }
    }

    /// The credential itself was refused — re-prompting the user is the fix.
    pub fn is_auth_failure(&self) -> bool {
        match self {
            Self::EmptyKey | Self::KeyTooShort { .. } | Self::Unauthorized { .. } => true,
            Self::Rejected { status, .. } => *status == 401 || *status == 403,
            _ => false,
        }
    }

    /// The failure looks transient — a network fault or a server-side error — so
    /// a retry may succeed without user action.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Unreachable { .. } => true,
            Self::Status { status, .. } | Self::Unhealthy { status, .. } => *status >= 500,
            _ => false,
        }
    }
}
