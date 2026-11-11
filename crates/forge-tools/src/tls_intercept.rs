//! Credentials supplied by the gateway, so they never enter the sandbox.
//!
//! A confined process that must authenticate to a host (git over HTTPS to
//! GitHub) is given a placeholder instead of the user's token. The gateway
//! terminates TLS for that host with a certificate from a per-session
//! authority, replaces the placeholder with the real credential in the
//! request it forwards, and speaks real TLS to the host. A process that
//! reads its own environment or credential store finds nothing it can use
//! anywhere else.
//!
//! The authority exists in memory only and is trusted only by processes
//! Forge hands its certificate to (`GIT_SSL_CAINFO`, `SSL_CERT_FILE`, ...).
//! Only hosts with a registered credential are terminated; every other
//! tunnel stays opaque.
//!
//! A request to a terminated host that carries credentials of its own is
//! refused: it is not the user's identity, and an upload under somebody
//! else's token to an allowed host is how data would leave.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use base64::Engine as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

/// A credential the gateway supplies for `host` and its subdomains.
#[derive(Clone)]
pub(crate) struct Secret {
    pub host: String,
    pub password: String,
    /// What the confined process holds instead of `password`.
    pub placeholder: String,
}

struct SessionCa {
    cert: rcgen::Certificate,
    key: rcgen::KeyPair,
    leaves: Mutex<HashMap<String, Arc<rustls::ServerConfig>>>,
}

/// One session's interception state, shared by all its endpoints.
#[derive(Default)]
pub(crate) struct Intercept {
    ca: OnceLock<Option<SessionCa>>,
    secrets: Mutex<Vec<Secret>>,
    /// Extra roots for the upstream side. Tests only: production trusts the
    /// bundled web roots.
    upstream_roots: Mutex<Vec<CertificateDer<'static>>>,
}

impl std::fmt::Debug for Intercept {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the secrets.
        f.debug_struct("Intercept").finish_non_exhaustive()
    }
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn new_ca() -> Option<SessionCa> {
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).ok()?;
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Constrained(0));
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "Forge sandbox session authority");
    params.key_usages = vec![
        rcgen::KeyUsagePurpose::KeyCertSign,
        rcgen::KeyUsagePurpose::DigitalSignature,
    ];
    let key = rcgen::KeyPair::generate().ok()?;
    let cert = params.self_signed(&key).ok()?;
    Some(SessionCa {
        cert,
        key,
        leaves: Mutex::new(HashMap::new()),
    })
}

impl Intercept {
    fn ca(&self) -> Option<&SessionCa> {
        self.ca.get_or_init(new_ca).as_ref()
    }

    /// The session authority's certificate, PEM. `None` when it could not
    /// be created; nothing is intercepted then.
    pub(crate) fn ca_pem(&self) -> Option<String> {
        self.ca().map(|ca| ca.cert.pem())
    }

    /// Have the gateway supply `password` for `host` and its subdomains.
    /// Returns the placeholder to give the confined process; the same one
    /// for the life of the session.
    pub(crate) fn register(&self, host: &str, password: &str) -> String {
        let mut secrets = self
            .secrets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(known) = secrets.iter_mut().find(|secret| secret.host == host) {
            known.password = password.to_string();
            return known.placeholder.clone();
        }
        let placeholder = format!("forge-placeholder-{}", crate::ssh_broker::uuid_like_token());
        secrets.push(Secret {
            host: host.to_string(),
            password: password.to_string(),
            placeholder: placeholder.clone(),
        });
        placeholder
    }

    /// The credential the gateway supplies for `host`, if any.
    pub(crate) fn secret_for(&self, host: &str) -> Option<Secret> {
        let host = host.to_ascii_lowercase();
        self.secrets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .find(|secret| {
                host == secret.host
                    || host
                        .strip_suffix(&secret.host)
                        .is_some_and(|prefix| prefix.ends_with('.'))
            })
            .cloned()
    }

    /// The TLS identity the gateway presents as `host`.
    pub(crate) fn server_config(&self, host: &str) -> Option<Arc<rustls::ServerConfig>> {
        let ca = self.ca()?;
        let mut leaves = ca
            .leaves
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(config) = leaves.get(host) {
            return Some(Arc::clone(config));
        }
        let mut params = rcgen::CertificateParams::new(vec![host.to_string()]).ok()?;
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, host);
        params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
        let key = rcgen::KeyPair::generate().ok()?;
        let cert = params.signed_by(&key, &ca.cert, &ca.key).ok()?;
        let mut config = rustls::ServerConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .ok()?
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.der().clone()],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
            )
            .ok()?;
        // One request per connection, read as HTTP/1.1.
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        let config = Arc::new(config);
        leaves.insert(host.to_string(), Arc::clone(&config));
        Some(config)
    }

    /// How the gateway verifies the real host.
    pub(crate) fn client_config(&self) -> Option<Arc<rustls::ClientConfig>> {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        for root in self
            .upstream_roots
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
        {
            roots.add(root.clone()).ok()?;
        }
        let mut config = rustls::ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .ok()?
            .with_root_certificates(roots)
            .with_no_client_auth();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Some(Arc::new(config))
    }

    /// Trust `root` for upstream connections. For tests, whose upstream is
    /// a local server with a certificate no public authority signed.
    #[doc(hidden)]
    pub(crate) fn trust_upstream_root(&self, root: CertificateDer<'static>) {
        self.upstream_roots
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(root);
    }
}

/// Why a request to a terminated host is not forwarded.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ForeignCredentials;

/// Replace the placeholder with the real credential in `headers`.
///
/// An `Authorization` header that does not carry the placeholder is somebody
/// else's credential and is refused. A request with no `Authorization` is
/// forwarded as it is: the host answers it as anonymous.
pub(crate) fn supply_credentials(
    secret: &Secret,
    headers: Vec<(&str, &str)>,
) -> Result<Vec<(String, String)>, ForeignCredentials> {
    let engine = base64::engine::general_purpose::STANDARD;
    let mut supplied = Vec::with_capacity(headers.len());
    for (name, value) in headers {
        let mut value = value.to_string();
        if name.eq_ignore_ascii_case("authorization") {
            let basic = value
                .split_once(' ')
                .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("basic"))
                .and_then(|(_, encoded)| engine.decode(encoded.trim()).ok())
                .and_then(|decoded| String::from_utf8(decoded).ok());
            match basic {
                Some(pair) if pair.contains(&secret.placeholder) => {
                    let pair = pair.replace(&secret.placeholder, &secret.password);
                    value = format!("Basic {}", engine.encode(pair));
                }
                None if value.contains(&secret.placeholder) => {
                    value = value.replace(&secret.placeholder, &secret.password);
                }
                _ => return Err(ForeignCredentials),
            }
        } else if value.contains(&secret.placeholder) {
            value = value.replace(&secret.placeholder, &secret.password);
        }
        supplied.push((name.to_string(), value));
    }
    Ok(supplied)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret() -> Secret {
        Secret {
            host: "example.test".into(),
            password: "real-token".into(),
            placeholder: "forge-placeholder-abc".into(),
        }
    }

    #[test]
    fn a_credential_covers_its_host_and_subdomains_only() {
        let intercept = Intercept::default();
        let placeholder = intercept.register("example.test", "real-token");
        assert!(placeholder.starts_with("forge-placeholder-"));
        assert_eq!(intercept.register("example.test", "rotated"), placeholder);
        for host in ["example.test", "api.example.test", "API.Example.Test"] {
            assert_eq!(intercept.secret_for(host).unwrap().password, "rotated");
        }
        for host in ["notexample.test", "example.test.evil.test", "test"] {
            assert!(intercept.secret_for(host).is_none(), "{host}");
        }
    }

    #[test]
    fn the_placeholder_becomes_the_credential_in_each_scheme() {
        let engine = base64::engine::general_purpose::STANDARD;
        let basic = format!("Basic {}", engine.encode("me:forge-placeholder-abc"));
        let supplied = supply_credentials(
            &secret(),
            vec![("Authorization", &basic), ("Accept", "*/*")],
        )
        .unwrap();
        assert_eq!(
            supplied[0].1,
            format!("Basic {}", engine.encode("me:real-token"))
        );
        assert_eq!(supplied[1], ("Accept".to_string(), "*/*".to_string()));

        for (name, value, expected) in [
            (
                "authorization",
                "token forge-placeholder-abc",
                "token real-token",
            ),
            (
                "Authorization",
                "Bearer forge-placeholder-abc",
                "Bearer real-token",
            ),
            ("PRIVATE-TOKEN", "forge-placeholder-abc", "real-token"),
        ] {
            let supplied = supply_credentials(&secret(), vec![(name, value)]).unwrap();
            assert_eq!(supplied[0].1, expected);
        }
        // Anonymous requests pass untouched.
        assert!(supply_credentials(&secret(), vec![("Accept", "*/*")]).is_ok());
    }

    #[test]
    fn credentials_that_are_not_the_users_are_refused() {
        let engine = base64::engine::general_purpose::STANDARD;
        let other = format!("Basic {}", engine.encode("attacker:ghp_theirs"));
        for value in [other.as_str(), "token ghp_theirs", "Bearer x"] {
            assert_eq!(
                supply_credentials(&secret(), vec![("Authorization", value)]),
                Err(ForeignCredentials),
                "{value}"
            );
        }
    }

    #[test]
    fn the_authority_signs_a_certificate_per_host() {
        let intercept = Intercept::default();
        let pem = intercept.ca_pem().unwrap();
        assert!(pem.contains("BEGIN CERTIFICATE"));
        let first = intercept.server_config("example.test").unwrap();
        let again = intercept.server_config("example.test").unwrap();
        assert!(Arc::ptr_eq(&first, &again));
        assert!(intercept.client_config().is_some());
    }
}
