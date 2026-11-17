//! Device tokens: 256 random bits the device presents as a bearer token.
//!
//! Forge keeps only a token's SHA-256 ([`TokenHash`]); the token itself is
//! handed to the device once, when it pairs, and never written anywhere. A
//! hash is enough here (no slow KDF): the token is high-entropy random, not
//! a password, so the hash cannot be reversed by guessing.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Every token starts with this, so a token is recognisable in a leak scan
/// and a format change can be told apart.
pub const TOKEN_PREFIX: &str = "fr1_";

/// Random bytes in a token.
pub const TOKEN_BYTES: usize = 32;

/// Base64url characters (no padding) for [`TOKEN_BYTES`].
const ENCODED_LEN: usize = 43;

/// A device's bearer token. Its `Debug` never shows the secret.
#[derive(Clone, PartialEq, Eq)]
pub struct DeviceToken(String);

impl std::fmt::Debug for DeviceToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DeviceToken(<redacted>)")
    }
}

impl DeviceToken {
    /// A fresh token from the operating system's random source.
    pub fn generate() -> Self {
        let mut bytes = [0u8; TOKEN_BYTES];
        getrandom::fill(&mut bytes).expect("the operating system's random source");
        Self(format!("{TOKEN_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes)))
    }

    /// A well-formed token: the prefix and 43 base64url characters (no
    /// padding) that decode to exactly [`TOKEN_BYTES`] bytes.
    pub fn parse(text: &str) -> Option<Self> {
        let body = text.strip_prefix(TOKEN_PREFIX)?;
        if body.len() != ENCODED_LEN
            || !body
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return None;
        }
        // Decoding also rejects a last character with stray low bits, so
        // each token has exactly one spelling.
        let decoded = URL_SAFE_NO_PAD.decode(body).ok()?;
        (decoded.len() == TOKEN_BYTES).then(|| Self(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The SHA-256 of a token's text: all Forge stores.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct TokenHash(pub [u8; 32]);

impl std::fmt::Debug for TokenHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TokenHash({})", self.to_hex())
    }
}

impl TokenHash {
    pub fn of(token: &DeviceToken) -> Self {
        Self(Sha256::digest(token.as_str().as_bytes()).into())
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    pub fn from_hex(text: &str) -> Option<Self> {
        let bytes = hex::decode(text).ok()?;
        Some(Self(bytes.try_into().ok()?))
    }

    /// Whether `token` hashes to this, compared in constant time.
    pub fn matches(&self, token: &DeviceToken) -> bool {
        Self::of(token).0.ct_eq(&self.0).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_is_the_prefix_and_43_url_safe_characters() {
        let token = DeviceToken::generate();
        let text = token.as_str();
        assert!(text.starts_with(TOKEN_PREFIX), "{text}");
        let body = &text[TOKEN_PREFIX.len()..];
        assert_eq!(body.len(), 43, "{text}");
        assert!(body
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
        assert_eq!(DeviceToken::parse(text), Some(token));
    }

    #[test]
    fn every_token_is_new() {
        let tokens: std::collections::HashSet<String> = (0..64)
            .map(|_| DeviceToken::generate().as_str().to_string())
            .collect();
        assert_eq!(tokens.len(), 64);
    }

    #[test]
    fn malformed_tokens_are_refused() {
        let good = DeviceToken::generate().as_str().to_string();
        let body = &good[TOKEN_PREFIX.len()..];
        for bad in [
            String::new(),
            body.to_string(),
            format!("fr2_{body}"),
            format!("{good}A"),
            good[..good.len() - 1].to_string(),
            format!("{good} "),
            format!(" {good}"),
            format!("{TOKEN_PREFIX}{}", "=".repeat(43)),
            format!("{TOKEN_PREFIX}{}+", &body[..42]),
        ] {
            assert_eq!(DeviceToken::parse(&bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_hash_is_the_sha256_of_the_token_text() {
        let token = DeviceToken::generate();
        let hash = TokenHash::of(&token);
        let expected = sha2::Sha256::digest(token.as_str().as_bytes());
        assert_eq!(hash.0.as_slice(), &expected[..]);
        assert_eq!(hash.to_hex().len(), 64);
        assert_eq!(TokenHash::from_hex(&hash.to_hex()), Some(hash));
        assert_eq!(TokenHash::from_hex("zz"), None);
        assert_eq!(TokenHash::from_hex(&"a".repeat(62)), None);
    }

    #[test]
    fn a_hash_matches_only_its_own_token() {
        let token = DeviceToken::generate();
        let hash = TokenHash::of(&token);
        assert!(hash.matches(&token));
        assert!(!hash.matches(&DeviceToken::generate()));
    }

    #[test]
    fn debug_output_never_shows_the_secret() {
        let token = DeviceToken::generate();
        let shown = format!("{token:?}");
        assert!(!shown.contains(&token.as_str()[TOKEN_PREFIX.len()..]));
    }
}
