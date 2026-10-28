//! Typed failures for credential verification and reachability probes.
//!
//! These checks previously returned `Result<(), String>`, which forced callers to
//! parse message text to tell an auth rejection from a rate limit or an
//! unreachable host. The variants below carry the HTTP status where the server
//! produced one, so that decision is structural.
//!
//! `Display` output is deliberately byte-identical to the strings these checks
//! used to return, and — as before — never contains the credential.

use std::borrow::Cow;

use thiserror::Error;

/// A credential-verification or reachability probe failed.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum VerifyError {
    #[error("API key is empty")]
    EmptyKey,

    #[error("API key looks too short ({len} chars). {guidance}")]
    KeyTooShort {
        len: usize,
        guidance: Cow<'static, str>,
    },

    /// Server rejected the credential outright (401/403). Retrying will not help.
    #[error("{provider} rejected the API key (unauthorized). {guidance}")]
    Unauthorized {
        provider: Cow<'static, str>,
        guidance: Cow<'static, str>,
    },

    /// Server rejected the credential and reported a status.
    #[error("{provider} rejected the API key (HTTP {status}). {guidance}")]
    Rejected {
        provider: Cow<'static, str>,
        status: u16,
        guidance: Cow<'static, str>,
    },

    /// Non-success status that is not an authentication failure.
    #[error("{provider} key verification failed (HTTP {status}).{}", .guidance.as_ref().map(|g| format!(" {g}")).unwrap_or_default())]
    Status {
        provider: Cow<'static, str>,
        status: u16,
        guidance: Option<Cow<'static, str>>,
    },

    /// An OAuth token could not be parsed, or lacked a required claim.
    #[error("{0}")]
    MalformedToken(&'static str),

    /// Reached the service but it reported itself unhealthy.
    #[error("{provider} responded HTTP {status} at {endpoint}. {guidance}")]
    Unhealthy {
        provider: Cow<'static, str>,
        status: u16,
        endpoint: String,
        guidance: Cow<'static, str>,
    },

    /// Could not reach the service at all. `detail` is the transport error, which
    /// is why this variant carries a string: the category is what callers branch
    /// on, and the text is only for the operator.
    #[error("{message}")]
    Unreachable {
        provider: Cow<'static, str>,
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

/// The `openai-models` probe: `GET <base>/models` with a Bearer key. A 401 or
/// 403 is a rejected key; any other non-success status, or no answer, is not.
pub(crate) fn probe_openai_models(
    base_url: &str,
    key: &str,
    provider: &str,
    guidance: &str,
) -> Result<(), VerifyError> {
    let base = base_url.trim().trim_end_matches('/');
    let url = format!("{base}/models");
    let response = ureq::get(&url)
        .header("Authorization", &format!("Bearer {key}"))
        .header(
            "User-Agent",
            &format!("forge-connect/{}", env!("CARGO_PKG_VERSION")),
        )
        .config()
        // Never follow redirects: a hop could forward the credential.
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_per_call(Some(std::time::Duration::from_secs(15)))
        .build()
        .call()
        .map_err(|error| VerifyError::Unreachable {
            provider: provider.to_string().into(),
            message: format!(
                "Could not reach {provider} to verify key ({error}). Check network access to {base}."
            ),
        })?;
    let status = response.status().as_u16();
    match status {
        200..=299 => Ok(()),
        401 | 403 => Err(VerifyError::Rejected {
            provider: provider.to_string().into(),
            status,
            guidance: guidance.to_string().into(),
        }),
        _ => Err(VerifyError::Status {
            provider: provider.to_string().into(),
            status,
            guidance: None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The module contract is that `Display` output stays byte-identical to the
    /// strings these checks returned before they were given types, because
    /// callers (and the TUI) surface them verbatim. Pin every variant.
    #[test]
    fn display_strings_are_stable() {
        assert_eq!(VerifyError::EmptyKey.to_string(), "API key is empty");

        assert_eq!(
            VerifyError::KeyTooShort {
                len: 4,
                guidance: "Paste the full key.".into(),
            }
            .to_string(),
            "API key looks too short (4 chars). Paste the full key."
        );

        assert_eq!(
            VerifyError::Unauthorized {
                provider: "OpenAI".into(),
                guidance: "Check the key.".into(),
            }
            .to_string(),
            "OpenAI rejected the API key (unauthorized). Check the key."
        );

        assert_eq!(
            VerifyError::Rejected {
                provider: "OpenAI".into(),
                status: 403,
                guidance: "Check the key.".into(),
            }
            .to_string(),
            "OpenAI rejected the API key (HTTP 403). Check the key."
        );

        assert_eq!(
            VerifyError::MalformedToken("token is missing the account claim").to_string(),
            "token is missing the account claim"
        );

        assert_eq!(
            VerifyError::Unhealthy {
                provider: "Ollama".into(),
                status: 503,
                endpoint: "http://localhost:11434/api/tags".into(),
                guidance: "Is it running?".into(),
            }
            .to_string(),
            "Ollama responded HTTP 503 at http://localhost:11434/api/tags. Is it running?"
        );

        assert_eq!(
            VerifyError::Unreachable {
                provider: "Ollama".into(),
                message: "connection refused".into(),
            }
            .to_string(),
            "connection refused"
        );
    }

    /// `Status` is the one variant whose message changes shape: the trailing
    /// guidance is appended only when present, with a single leading space and
    /// no dangling separator when absent.
    #[test]
    fn status_display_appends_guidance_only_when_present() {
        assert_eq!(
            VerifyError::Status {
                provider: "xAI".into(),
                status: 500,
                guidance: Some("Try again shortly.".into()),
            }
            .to_string(),
            "xAI key verification failed (HTTP 500). Try again shortly."
        );
        assert_eq!(
            VerifyError::Status {
                provider: "xAI".into(),
                status: 500,
                guidance: None,
            }
            .to_string(),
            "xAI key verification failed (HTTP 500)."
        );
    }

    #[test]
    fn status_is_reported_only_when_the_server_supplied_one() {
        assert_eq!(
            VerifyError::Rejected {
                provider: "p".into(),
                status: 401,
                guidance: "g".into(),
            }
            .status(),
            Some(401)
        );
        assert_eq!(
            VerifyError::Status {
                provider: "p".into(),
                status: 429,
                guidance: None,
            }
            .status(),
            Some(429)
        );
        assert_eq!(
            VerifyError::Unhealthy {
                provider: "p".into(),
                status: 503,
                endpoint: "e".into(),
                guidance: "g".into(),
            }
            .status(),
            Some(503)
        );

        // `Unauthorized` is semantically a 401/403 but carries no status field,
        // so it deliberately reports None rather than inventing one.
        assert_eq!(
            VerifyError::Unauthorized {
                provider: "p".into(),
                guidance: "g".into(),
            }
            .status(),
            None
        );
        assert_eq!(VerifyError::EmptyKey.status(), None);
        assert_eq!(
            VerifyError::KeyTooShort {
                len: 1,
                guidance: "g".into()
            }
            .status(),
            None
        );
        assert_eq!(VerifyError::MalformedToken("m").status(), None);
        assert_eq!(
            VerifyError::Unreachable {
                provider: "p".into(),
                message: "m".into(),
            }
            .status(),
            None
        );
    }

    #[test]
    fn auth_failures_are_the_ones_a_new_credential_would_fix() {
        assert!(VerifyError::EmptyKey.is_auth_failure());
        assert!(VerifyError::KeyTooShort {
            len: 2,
            guidance: "g".into()
        }
        .is_auth_failure());
        assert!(VerifyError::Unauthorized {
            provider: "p".into(),
            guidance: "g".into(),
        }
        .is_auth_failure());

        // `Rejected` is an auth failure only for the two credential-refusal
        // statuses; any other status is a server problem, not a bad key.
        for status in [401, 403] {
            assert!(
                VerifyError::Rejected {
                    provider: "p".into(),
                    status,
                    guidance: "g".into(),
                }
                .is_auth_failure(),
                "HTTP {status} should count as an auth failure"
            );
        }
        for status in [400, 429, 500, 503] {
            assert!(
                !VerifyError::Rejected {
                    provider: "p".into(),
                    status,
                    guidance: "g".into(),
                }
                .is_auth_failure(),
                "HTTP {status} should not be treated as a bad credential"
            );
        }

        assert!(!VerifyError::Unreachable {
            provider: "p".into(),
            message: "m".into(),
        }
        .is_auth_failure());
        assert!(!VerifyError::MalformedToken("m").is_auth_failure());
    }

    #[test]
    fn retryable_covers_transport_faults_and_server_errors_only() {
        // A transport fault never reached the server, so a retry is reasonable.
        assert!(VerifyError::Unreachable {
            provider: "p".into(),
            message: "connection reset".into(),
        }
        .is_retryable());

        // 5xx is the server's problem; 4xx is the request's, so it is not.
        for status in [500, 502, 503] {
            assert!(
                VerifyError::Status {
                    provider: "p".into(),
                    status,
                    guidance: None,
                }
                .is_retryable(),
                "HTTP {status} should be retryable"
            );
            assert!(VerifyError::Unhealthy {
                provider: "p".into(),
                status,
                endpoint: "e".into(),
                guidance: "g".into(),
            }
            .is_retryable());
        }
        for status in [400, 401, 404, 429] {
            assert!(
                !VerifyError::Status {
                    provider: "p".into(),
                    status,
                    guidance: None,
                }
                .is_retryable(),
                "HTTP {status} should not be retryable"
            );
        }

        // A refused or malformed credential will not fix itself.
        assert!(!VerifyError::EmptyKey.is_retryable());
        assert!(!VerifyError::Unauthorized {
            provider: "p".into(),
            guidance: "g".into(),
        }
        .is_retryable());
        assert!(!VerifyError::Rejected {
            provider: "p".into(),
            status: 401,
            guidance: "g".into(),
        }
        .is_retryable());
    }

    /// Auth failure and retryable are meant to be disjoint: a caller decides
    /// between re-prompting and retrying, so nothing may claim both.
    #[test]
    fn no_error_is_both_an_auth_failure_and_retryable() {
        let all = [
            VerifyError::EmptyKey,
            VerifyError::KeyTooShort {
                len: 3,
                guidance: "g".into(),
            },
            VerifyError::Unauthorized {
                provider: "p".into(),
                guidance: "g".into(),
            },
            VerifyError::Rejected {
                provider: "p".into(),
                status: 401,
                guidance: "g".into(),
            },
            VerifyError::Rejected {
                provider: "p".into(),
                status: 500,
                guidance: "g".into(),
            },
            VerifyError::Status {
                provider: "p".into(),
                status: 500,
                guidance: None,
            },
            VerifyError::MalformedToken("m"),
            VerifyError::Unhealthy {
                provider: "p".into(),
                status: 503,
                endpoint: "e".into(),
                guidance: "g".into(),
            },
            VerifyError::Unreachable {
                provider: "p".into(),
                message: "m".into(),
            },
        ];
        for e in &all {
            assert!(
                !(e.is_auth_failure() && e.is_retryable()),
                "{e:?} claims to be both an auth failure and retryable"
            );
        }
    }

    /// The module header promises the rendered message never contains the
    /// credential. `KeyTooShort` is the only variant derived from the key, and
    /// it must report the length rather than the value.
    #[test]
    fn short_key_message_reports_length_not_the_key() {
        let secret = "sk-supersecret";
        let rendered = VerifyError::KeyTooShort {
            len: secret.len(),
            guidance: "Paste the full key.".into(),
        }
        .to_string();
        assert!(
            !rendered.contains(secret),
            "rendered message must not echo the credential: {rendered}"
        );
        assert!(rendered.contains(&secret.len().to_string()));
    }
}
