//! The service's TLS identity: a self-signed certificate made once and kept,
//! whose SHA-256 fingerprint the device pins from the pairing link.
//!
//! Pinning replaces a certificate authority: the device trusts exactly this
//! certificate, whatever name or address it reached it by, so a look-alike
//! server on the network cannot stand in. The server speaks TLS 1.3 only.

use std::path::Path;
use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::fsutil::write_private;

pub const CERT_FILE: &str = "cert.der";
pub const KEY_FILE: &str = "key.der";
const PIN_PREFIX: &str = "sha256:";

/// A certificate's SHA-256.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fingerprint(pub [u8; 32]);

impl std::fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Fingerprint({})", self.to_hex())
    }
}

impl Fingerprint {
    pub fn of_der(der: &[u8]) -> Self {
        Self(Sha256::digest(der).into())
    }

    /// 64 lowercase hex digits.
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// The form the pairing link carries: `sha256:<hex>`.
    pub fn to_pin(&self) -> String {
        format!("{PIN_PREFIX}{}", self.to_hex())
    }

    /// Parse `sha256:<64 hex digits>` (either case).
    pub fn parse_pin(text: &str) -> Option<Self> {
        let digits = text.strip_prefix(PIN_PREFIX)?;
        if digits.len() != 64 {
            return None;
        }
        Some(Self(hex::decode(digits).ok()?.try_into().ok()?))
    }

    /// Sixteen groups of four uppercase hex digits, for reading aloud.
    pub fn display_groups(&self) -> String {
        self.to_hex()
            .to_uppercase()
            .as_bytes()
            .chunks(4)
            .map(|group| String::from_utf8_lossy(group).into_owned())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    #[error("cannot keep the remote control certificate: {0}")]
    Io(#[from] std::io::Error),
    #[error("cannot make the remote control certificate: {0}")]
    Generate(String),
    #[error("cannot configure TLS: {0}")]
    Config(String),
}

pub struct TlsIdentity {
    cert: CertificateDer<'static>,
    key: PrivatePkcs8KeyDer<'static>,
    fingerprint: Fingerprint,
}

impl TlsIdentity {
    /// The certificate and key in `dir`, made (and kept, `0600`) the first
    /// time. Unreadable or damaged files are replaced with a new identity;
    /// a paired device then fails its pin and must pair again.
    pub fn load_or_create(dir: &Path) -> Result<Self, TlsError> {
        if let Some(identity) = Self::load(dir) {
            return Ok(identity);
        }
        let identity = Self::generate()?;
        // The key first: a certificate without its key is never left alone
        // looking valid. A crash between the two writes makes a mismatched
        // pair, which `load` refuses and replaces.
        write_private(&dir.join(KEY_FILE), identity.key.secret_pkcs8_der())?;
        write_private(&dir.join(CERT_FILE), identity.cert.as_ref())?;
        Ok(identity)
    }

    /// A new identity in `dir`, replacing the old one: devices pinned to
    /// the old certificate can no longer connect.
    pub fn replace(dir: &Path) -> Result<Self, TlsError> {
        let identity = Self::generate()?;
        write_private(&dir.join(KEY_FILE), identity.key.secret_pkcs8_der())?;
        write_private(&dir.join(CERT_FILE), identity.cert.as_ref())?;
        Ok(identity)
    }

    fn load(dir: &Path) -> Option<Self> {
        let cert = CertificateDer::from(std::fs::read(dir.join(CERT_FILE)).ok()?);
        let key = PrivatePkcs8KeyDer::from(std::fs::read(dir.join(KEY_FILE)).ok()?);
        let certified = rustls::sign::CertifiedKey::from_der(
            vec![cert.clone()],
            PrivateKeyDer::Pkcs8(key.clone_key()),
            &provider(),
        )
        .ok()?;
        certified.keys_match().ok()?;
        let fingerprint = Fingerprint::of_der(&cert);
        Some(Self {
            cert,
            key,
            fingerprint,
        })
    }

    fn generate() -> Result<Self, TlsError> {
        let generate = |e: rcgen::Error| TlsError::Generate(e.to_string());
        let key = rcgen::KeyPair::generate().map_err(generate)?;
        let mut params = rcgen::CertificateParams::new(vec![
            "forge-remote".to_string(),
            "localhost".to_string(),
        ])
        .map_err(generate)?;
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "Forge remote control");
        let cert = params.self_signed(&key).map_err(generate)?;
        let cert = cert.der().clone();
        Ok(Self {
            fingerprint: Fingerprint::of_der(&cert),
            cert,
            key: PrivatePkcs8KeyDer::from(key.serialize_der()),
        })
    }

    pub fn fingerprint(&self) -> Fingerprint {
        self.fingerprint
    }

    /// TLS 1.3 only, no client certificates.
    pub fn server_config(&self) -> Result<Arc<rustls::ServerConfig>, TlsError> {
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(provider()))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|e| TlsError::Config(e.to_string()))?
            .with_no_client_auth()
            .with_single_cert(
                vec![self.cert.clone()],
                PrivateKeyDer::Pkcs8(self.key.clone_key()),
            )
            .map_err(|e| TlsError::Config(e.to_string()))?;
        Ok(Arc::new(config))
    }
}

fn provider() -> rustls::crypto::CryptoProvider {
    rustls::crypto::ring::default_provider()
}

/// Trusts exactly one certificate, by its SHA-256.
#[derive(Debug)]
struct PinnedVerifier {
    pin: Fingerprint,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl rustls::client::danger::ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let presented = Fingerprint::of_der(end_entity);
        if bool::from(presented.0.ct_eq(&self.pin.0)) {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        // The config below never offers TLS 1.2.
        Err(rustls::Error::PeerIncompatible(
            rustls::PeerIncompatible::Tls12NotOffered,
        ))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// What a device does: trust exactly the certificate with this fingerprint,
/// over TLS 1.3, ignoring names. Handshake signatures are still verified.
pub fn pinned_client_config(pin: Fingerprint) -> Arc<rustls::ClientConfig> {
    let provider = Arc::new(provider());
    let config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .expect("the ring provider supports TLS 1.3")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedVerifier { pin, provider }))
        .with_no_client_auth();
    Arc::new(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A handshake over an in-memory pipe; `Ok` with what the server echoed.
    async fn handshake(
        server: Arc<rustls::ServerConfig>,
        client: Arc<rustls::ClientConfig>,
    ) -> Result<Vec<u8>, String> {
        let (a, b) = tokio::io::duplex(64 * 1024);
        let acceptor = tokio_rustls::TlsAcceptor::from(server);
        let serve = tokio::spawn(async move {
            let mut stream = acceptor.accept(a).await.map_err(|e| e.to_string())?;
            let mut buf = [0u8; 4];
            stream
                .read_exact(&mut buf)
                .await
                .map_err(|e| e.to_string())?;
            stream.write_all(&buf).await.map_err(|e| e.to_string())?;
            stream.flush().await.map_err(|e| e.to_string())?;
            Ok::<_, String>(())
        });
        let connector = tokio_rustls::TlsConnector::from(client);
        let name = rustls::pki_types::ServerName::try_from("forge-remote.invalid").unwrap();
        let result = async {
            let mut stream = connector
                .connect(name, b)
                .await
                .map_err(|e| e.to_string())?;
            stream.write_all(b"ping").await.map_err(|e| e.to_string())?;
            let mut buf = [0u8; 4];
            stream
                .read_exact(&mut buf)
                .await
                .map_err(|e| e.to_string())?;
            Ok(buf.to_vec())
        }
        .await;
        serve.abort();
        result
    }

    #[test]
    fn fingerprints_print_and_parse() {
        let fingerprint = Fingerprint::of_der(b"certificate bytes");
        let pin = fingerprint.to_pin();
        assert!(pin.starts_with("sha256:"));
        assert_eq!(pin.len(), "sha256:".len() + 64);
        assert_eq!(Fingerprint::parse_pin(&pin), Some(fingerprint));
        assert_eq!(
            Fingerprint::parse_pin(&pin.to_uppercase().replace("SHA256", "sha256")),
            Some(fingerprint)
        );
        assert_eq!(
            Fingerprint::parse_pin(&fingerprint.to_hex()),
            None,
            "the prefix is required"
        );
        assert_eq!(Fingerprint::parse_pin("sha256:abc"), None);
        let groups = fingerprint.display_groups();
        assert_eq!(groups.split(' ').count(), 16);
        assert!(groups
            .split(' ')
            .all(|g| g.len() == 4 && g == g.to_uppercase()));
    }

    #[test]
    fn the_identity_is_made_once_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let first = TlsIdentity::load_or_create(dir.path()).unwrap();
        let second = TlsIdentity::load_or_create(dir.path()).unwrap();
        assert_eq!(first.fingerprint(), second.fingerprint());
        let cert = std::fs::read(dir.path().join(CERT_FILE)).unwrap();
        assert_eq!(Fingerprint::of_der(&cert), first.fingerprint());
    }

    #[test]
    fn a_damaged_identity_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let first = TlsIdentity::load_or_create(dir.path()).unwrap();
        std::fs::write(dir.path().join(KEY_FILE), b"not a key").unwrap();
        let second = TlsIdentity::load_or_create(dir.path()).unwrap();
        assert_ne!(first.fingerprint(), second.fingerprint());
        second.server_config().unwrap();
    }

    #[test]
    fn a_replaced_identity_is_new_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let first = TlsIdentity::load_or_create(dir.path()).unwrap();
        let second = TlsIdentity::replace(dir.path()).unwrap();
        assert_ne!(first.fingerprint(), second.fingerprint());
        let loaded = TlsIdentity::load_or_create(dir.path()).unwrap();
        assert_eq!(loaded.fingerprint(), second.fingerprint());
    }

    #[cfg(unix)]
    #[test]
    fn the_private_key_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        TlsIdentity::load_or_create(dir.path()).unwrap();
        let mode = std::fs::metadata(dir.path().join(KEY_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[tokio::test]
    async fn a_client_pinned_to_the_certificate_connects() {
        let dir = tempfile::tempdir().unwrap();
        let identity = TlsIdentity::load_or_create(dir.path()).unwrap();
        let echoed = handshake(
            identity.server_config().unwrap(),
            pinned_client_config(identity.fingerprint()),
        )
        .await
        .unwrap();
        assert_eq!(echoed, b"ping");
    }

    #[tokio::test]
    async fn a_client_pinned_to_another_certificate_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let identity = TlsIdentity::load_or_create(dir.path()).unwrap();
        let other = tempfile::tempdir().unwrap();
        let impostor = TlsIdentity::load_or_create(other.path()).unwrap();
        let result = handshake(
            impostor.server_config().unwrap(),
            pinned_client_config(identity.fingerprint()),
        )
        .await;
        assert!(result.is_err(), "a look-alike server must fail the pin");
    }

    #[tokio::test]
    async fn the_server_speaks_tls_1_3_only() {
        let dir = tempfile::tempdir().unwrap();
        let identity = TlsIdentity::load_or_create(dir.path()).unwrap();
        // A TLS 1.2-only client that trusts anything: the server must still
        // refuse it for its protocol version.
        #[derive(Debug)]
        struct AnyCert(Arc<rustls::crypto::CryptoProvider>);
        impl rustls::client::danger::ServerCertVerifier for AnyCert {
            fn verify_server_cert(
                &self,
                _: &CertificateDer<'_>,
                _: &[CertificateDer<'_>],
                _: &rustls::pki_types::ServerName<'_>,
                _: &[u8],
                _: rustls::pki_types::UnixTime,
            ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
                Ok(rustls::client::danger::ServerCertVerified::assertion())
            }
            fn verify_tls12_signature(
                &self,
                message: &[u8],
                cert: &CertificateDer<'_>,
                dss: &rustls::DigitallySignedStruct,
            ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error>
            {
                rustls::crypto::verify_tls12_signature(
                    message,
                    cert,
                    dss,
                    &self.0.signature_verification_algorithms,
                )
            }
            fn verify_tls13_signature(
                &self,
                message: &[u8],
                cert: &CertificateDer<'_>,
                dss: &rustls::DigitallySignedStruct,
            ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error>
            {
                rustls::crypto::verify_tls13_signature(
                    message,
                    cert,
                    dss,
                    &self.0.signature_verification_algorithms,
                )
            }
            fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
                self.0.signature_verification_algorithms.supported_schemes()
            }
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let old_client = rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS12])
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AnyCert(provider)))
            .with_no_client_auth();
        let result = handshake(identity.server_config().unwrap(), Arc::new(old_client)).await;
        assert!(result.is_err(), "TLS 1.2 must be refused");
    }
}
