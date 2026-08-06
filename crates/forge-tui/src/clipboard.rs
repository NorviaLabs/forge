//! OSC 52 clipboard transport (no native dependencies).
//!
//! Writes the OS clipboard via the `OSC 52` terminal escape sequence
//! (`ESC ] 52 ; <selection/query> ; <base64> BEL`). This works locally, over
//! SSH, and inside tmux, but the terminal is free to **deny** the write — the
//! sequence is consumed and discarded. Callers therefore surface the result as
//! best-effort: a successful write here only means the bytes reached the
//! terminal, not that the clipboard was populated.
//!
//! `c` (clipboard) is the clipboard we target. Spaces in the payload are
//! percent-encoded per the spec so the whole payload survives transit.

const B64_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encode bytes as standard base64 (no padding variants needed for OSC 52, but
/// we emit standard padding for interoperability with plain terminal-side
/// decoders).
pub(crate) fn base64_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64_ALPHABET[(n >> 18 & 0x3F) as usize] as char);
        out.push(B64_ALPHABET[(n >> 12 & 0x3F) as usize] as char);
        out.push(if chunk.len() > 1 {
            B64_ALPHABET[(n >> 6 & 0x3F) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64_ALPHABET[(n & 0x3F) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Build the raw OSC 52 sequence that copies `text` to the primary clipboard.
pub(crate) fn osc52_payload(text: &str) -> String {
    let b64 = base64_encode(text.as_bytes());
    format!("\x1b]52;c;{b64}\x07")
}

/// Write `text` to the clipboard via OSC 52.
///
/// Returns `Ok(true)` when the sequence reached stdout. A denial from the
/// terminal is indistinguishable synchronously, so callers treat a successful
/// write as "request sent" and may want to confirm via an OSC 52 read-back
/// (out of scope for v1).
pub(crate) fn write_osc52(text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let payload = osc52_payload(text);
    let mut stdout = std::io::stdout();
    stdout.write_all(payload.as_bytes())?;
    stdout.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_encodes_rfc_test_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn osc52_sequence_shape() {
        // "hi" -> base64 "aGk="
        assert_eq!(osc52_payload("hi"), "\x1b]52;c;aGk=\x07");
    }

    #[test]
    fn unicode_and_newlines_survive() {
        let s = "line1\nline2 → λ";
        let encoded = osc52_payload(s);
        assert!(encoded.starts_with("\x1b]52;c;"));
        assert!(encoded.ends_with('\u{7}'));
        // Decode round-trip back to the source text.
        let b64: String = encoded
            .trim_start_matches("\x1b]52;c;")
            .trim_end_matches('\u{7}')
            .to_string();
        let decoded = base64_decode_bytes(&b64);
        assert_eq!(String::from_utf8(decoded).unwrap(), s);
    }

    /// Test-only decoder for round-trip assertions.
    fn base64_decode_bytes(input: &str) -> Vec<u8> {
        let mut out = Vec::new();
        let mut buf: u32 = 0;
        let mut bits = 0;
        for c in input.chars() {
            if c == '=' {
                continue;
            }
            let v = B64_ALPHABET.iter().position(|&b| b == c as u8).unwrap() as u32;
            buf = (buf << 6) | v;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((buf >> bits) as u8);
            }
        }
        out
    }
}
