//! Path-ref image metadata and magic-byte inspection.
//!
//! Bytes are not stored on the type. Callers re-read the file at request-build
//! time. Inspection is header-only: no decode/transcode stack.

use serde::{Deserialize, Serialize};

/// Hard cap for `view_image` and clipboard paste.
pub const MAX_IMAGE_BYTES: u64 = 5 * 1024 * 1024;

/// Workspace-relative image the transports re-read when building a request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageRef {
    /// Path relative to the session workspace root.
    pub path: String,
    /// Sniffed mime, e.g. `image/png`.
    pub mime: String,
    pub byte_len: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// Always `high` in v1. Stored so resume/transcript can show what was requested.
    #[serde(default = "default_detail")]
    pub detail: String,
}

fn default_detail() -> String {
    "high".into()
}

impl ImageRef {
    pub fn new(path: impl Into<String>, mime: impl Into<String>, byte_len: u64) -> Self {
        Self {
            path: path.into(),
            mime: mime.into(),
            byte_len,
            width: None,
            height: None,
            detail: default_detail(),
        }
    }

    pub fn with_dimensions(mut self, width: Option<u32>, height: Option<u32>) -> Self {
        self.width = width;
        self.height = height;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageMeta {
    pub mime: &'static str,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageInspectError {
    Empty,
    TooLarge { byte_len: u64 },
    Unsupported,
}

impl std::fmt::Display for ImageInspectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "file is empty"),
            Self::TooLarge { byte_len } => write!(
                f,
                "image is {byte_len} bytes; maximum allowed is {MAX_IMAGE_BYTES} bytes"
            ),
            Self::Unsupported => {
                write!(f, "unsupported image type (allowed: PNG, JPEG, GIF, WebP)")
            }
        }
    }
}

impl std::error::Error for ImageInspectError {}

/// 1×1 PNG used by tests across crates.
pub fn sample_png_bytes() -> Vec<u8> {
    vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08, 0xD7, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xDD, 0x8D, 0xB0, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ]
}

/// Inspect raw bytes: size cap, magic-byte mime, optional header dimensions.
pub fn inspect_image(bytes: &[u8]) -> Result<ImageMeta, ImageInspectError> {
    if bytes.is_empty() {
        return Err(ImageInspectError::Empty);
    }
    let byte_len = bytes.len() as u64;
    if byte_len > MAX_IMAGE_BYTES {
        return Err(ImageInspectError::TooLarge { byte_len });
    }
    if let Some(meta) = png_meta(bytes) {
        return Ok(meta);
    }
    if let Some(meta) = jpeg_meta(bytes) {
        return Ok(meta);
    }
    if let Some(meta) = gif_meta(bytes) {
        return Ok(meta);
    }
    if let Some(meta) = webp_meta(bytes) {
        return Ok(meta);
    }
    Err(ImageInspectError::Unsupported)
}

/// True when the leading bytes are an allowed image, ignoring the size cap.
/// Used by `read_file` so a huge PNG still points at `view_image`.
pub fn sniff_allowed_image(bytes: &[u8]) -> bool {
    png_meta(bytes).is_some()
        || jpeg_meta(bytes).is_some()
        || gif_meta(bytes).is_some()
        || webp_meta(bytes).is_some()
}

fn png_meta(bytes: &[u8]) -> Option<ImageMeta> {
    const SIG: &[u8] = b"\x89PNG\r\n\x1a\n";
    if bytes.len() < SIG.len() || !bytes.starts_with(SIG) {
        return None;
    }
    if bytes.len() < 24 {
        return Some(ImageMeta {
            mime: "image/png",
            width: None,
            height: None,
        });
    }
    if &bytes[12..16] != b"IHDR" {
        return Some(ImageMeta {
            mime: "image/png",
            width: None,
            height: None,
        });
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some(ImageMeta {
        mime: "image/png",
        width: Some(width),
        height: Some(height),
    })
}

fn jpeg_meta(bytes: &[u8]) -> Option<ImageMeta> {
    if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 || bytes[2] != 0xFF {
        return None;
    }
    let mut i = 2;
    while i + 9 < bytes.len() {
        if bytes[i] != 0xFF {
            break;
        }
        let marker = bytes[i + 1];
        if marker == 0xD8 || marker == 0xD9 {
            i += 2;
            continue;
        }
        if i + 4 > bytes.len() {
            break;
        }
        let len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
        if matches!(marker, 0xC0..=0xC2) && i + 9 < bytes.len() {
            let height = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]) as u32;
            let width = u16::from_be_bytes([bytes[i + 7], bytes[i + 8]]) as u32;
            return Some(ImageMeta {
                mime: "image/jpeg",
                width: Some(width),
                height: Some(height),
            });
        }
        i = i.saturating_add(2).saturating_add(len);
    }
    Some(ImageMeta {
        mime: "image/jpeg",
        width: None,
        height: None,
    })
}

fn gif_meta(bytes: &[u8]) -> Option<ImageMeta> {
    if bytes.len() < 10 || !(bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
        return None;
    }
    let width = u16::from_le_bytes([bytes[6], bytes[7]]) as u32;
    let height = u16::from_le_bytes([bytes[8], bytes[9]]) as u32;
    Some(ImageMeta {
        mime: "image/gif",
        width: Some(width),
        height: Some(height),
    })
}

fn webp_meta(bytes: &[u8]) -> Option<ImageMeta> {
    if bytes.len() < 16 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return None;
    }
    let chunk = bytes.get(12..16)?;
    let (width, height) = if chunk == b"VP8X" && bytes.len() >= 30 {
        let w = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], 0]).saturating_add(1);
        let h = u32::from_le_bytes([bytes[27], bytes[28], bytes[29], 0]).saturating_add(1);
        (Some(w), Some(h))
    } else if chunk == b"VP8 "
        && bytes.len() >= 30
        && bytes[23] == 0x9D
        && bytes[24] == 0x01
        && bytes[25] == 0x2A
    {
        let w = u16::from_le_bytes([bytes[26], bytes[27]]) as u32 & 0x3FFF;
        let h = u16::from_le_bytes([bytes[28], bytes[29]]) as u32 & 0x3FFF;
        (Some(w), Some(h))
    } else if chunk == b"VP8L" && bytes.len() >= 25 {
        let bits = u32::from_le_bytes([bytes[21], bytes[22], bytes[23], bytes[24]]);
        let w = (bits & 0x3FFF) + 1;
        let h = ((bits >> 14) & 0x3FFF) + 1;
        (Some(w), Some(h))
    } else {
        (None, None)
    };
    Some(ImageMeta {
        mime: "image/webp",
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_round_trip_meta() {
        let bytes = sample_png_bytes();
        let meta = inspect_image(&bytes).unwrap();
        assert_eq!(meta.mime, "image/png");
        assert_eq!(meta.width, Some(1));
        assert_eq!(meta.height, Some(1));
    }

    #[test]
    fn gif_header_dimensions() {
        let mut bytes = b"GIF89a".to_vec();
        bytes.extend_from_slice(&10u16.to_le_bytes());
        bytes.extend_from_slice(&20u16.to_le_bytes());
        bytes.extend_from_slice(&[0, 0, 0]);
        let meta = inspect_image(&bytes).unwrap();
        assert_eq!(meta.mime, "image/gif");
        assert_eq!(meta.width, Some(10));
        assert_eq!(meta.height, Some(20));
    }

    #[test]
    fn jpeg_magic_is_enough() {
        let bytes = [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        let meta = inspect_image(&bytes).unwrap();
        assert_eq!(meta.mime, "image/jpeg");
    }

    #[test]
    fn rejects_empty_and_unknown() {
        assert_eq!(inspect_image(&[]), Err(ImageInspectError::Empty));
        assert_eq!(
            inspect_image(b"not an image"),
            Err(ImageInspectError::Unsupported)
        );
    }

    #[test]
    fn rejects_oversize() {
        let mut bytes = sample_png_bytes();
        bytes.resize(MAX_IMAGE_BYTES as usize + 1, 0);
        match inspect_image(&bytes) {
            Err(ImageInspectError::TooLarge { byte_len }) => {
                assert_eq!(byte_len, MAX_IMAGE_BYTES + 1);
            }
            other => panic!("expected too-large, got {other:?}"),
        }
        assert!(sniff_allowed_image(&bytes));
    }
}
