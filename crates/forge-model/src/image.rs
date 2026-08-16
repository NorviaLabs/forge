//! Load path-ref images at request-build time and emit provider content parts.

use std::path::{Path, PathBuf};

use forge_types::{inspect_image, ImageRef, Message};
use serde_json::{json, Value};

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub struct LoadedImage {
    pub mime: String,
    pub bytes: Vec<u8>,
}

pub fn resolve_image_path(workspace: &Path, rel: &str) -> PathBuf {
    let requested = Path::new(rel);
    if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        workspace.join(requested)
    }
}

pub fn load_image_ref(workspace: &Path, image: &ImageRef) -> Result<LoadedImage, String> {
    let path = resolve_image_path(workspace, &image.path);
    let bytes = std::fs::read(&path).map_err(|err| err.to_string())?;
    let meta = inspect_image(&bytes).map_err(|err| err.to_string())?;
    Ok(LoadedImage {
        mime: meta.mime.to_string(),
        bytes,
    })
}

/// Copy readable attachments into `cache_dir` and retarget `ImageRef.path`
/// to that snapshot. Unreadable attachments become a note on `content` and
/// are dropped. Call once at insert so later disk edits cannot change the wire.
pub fn freeze_attachments(
    workspace: &Path,
    cache_dir: &Path,
    content: &mut String,
    attachments: Vec<ImageRef>,
) -> Vec<ImageRef> {
    if attachments.is_empty() {
        return attachments;
    }
    let mut kept = Vec::with_capacity(attachments.len());
    let mut notes = Vec::new();
    for image in attachments {
        match snapshot_image_ref(workspace, cache_dir, &image) {
            Ok(frozen) => kept.push(frozen),
            Err(_) => notes.push(format!("image at `{}` is no longer available", image.path)),
        }
    }
    if !notes.is_empty() {
        if !content.is_empty() {
            content.push('\n');
        }
        content.push_str(&notes.join("\n"));
    }
    kept
}

/// Drop attachments that cannot be re-read and append a model-visible note.
/// Used by tests and as the missing-file path inside [`freeze_attachments`].
pub fn apply_missing_image_notes(messages: &mut [Message], workspace: &Path) {
    for message in messages {
        if message.attachments.is_empty() {
            continue;
        }
        let mut kept = Vec::new();
        let mut notes = Vec::new();
        for image in message.attachments.drain(..) {
            match load_image_ref(workspace, &image) {
                Ok(_) => kept.push(image),
                Err(_) => notes.push(format!("image at `{}` is no longer available", image.path)),
            }
        }
        message.attachments = kept;
        if notes.is_empty() {
            continue;
        }
        if !message.content.is_empty() {
            message.content.push('\n');
        }
        message.content.push_str(&notes.join("\n"));
    }
}

fn snapshot_image_ref(
    workspace: &Path,
    cache_dir: &Path,
    image: &ImageRef,
) -> Result<ImageRef, String> {
    let loaded = load_image_ref(workspace, image)?;
    std::fs::create_dir_all(cache_dir).map_err(|err| err.to_string())?;
    let ext = extension_for(&image.path, &loaded.mime);
    let dest = cache_dir.join(format!("{}{ext}", uuid::Uuid::new_v4()));
    std::fs::write(&dest, &loaded.bytes).map_err(|err| err.to_string())?;
    let path = dest
        .strip_prefix(workspace)
        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| dest.to_string_lossy().replace('\\', "/"));
    Ok(ImageRef {
        path,
        mime: loaded.mime,
        byte_len: loaded.bytes.len() as u64,
        width: image.width,
        height: image.height,
        detail: image.detail.clone(),
    })
}

fn extension_for(path: &str, mime: &str) -> &'static str {
    match mime {
        "image/png" => ".png",
        "image/jpeg" => ".jpg",
        "image/gif" => ".gif",
        "image/webp" => ".webp",
        _ => Path::new(path)
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| match ext {
                "png" => ".png",
                "jpg" | "jpeg" => ".jpg",
                "gif" => ".gif",
                "webp" => ".webp",
                _ => "",
            })
            .unwrap_or(""),
    }
}

pub fn data_url(mime: &str, bytes: &[u8]) -> String {
    format!("data:{mime};base64,{}", base64_encode(bytes))
}

pub fn openai_image_part(mime: &str, bytes: &[u8]) -> Value {
    json!({
        "type": "image_url",
        "image_url": { "url": data_url(mime, bytes) }
    })
}

pub fn anthropic_image_part(mime: &str, bytes: &[u8]) -> Value {
    json!({
        "type": "image",
        "source": {
            "type": "base64",
            "media_type": mime,
            "data": base64_encode(bytes)
        }
    })
}

pub fn codex_input_image_part(mime: &str, bytes: &[u8]) -> Value {
    json!({
        "type": "input_image",
        "image_url": data_url(mime, bytes),
        "detail": "high"
    })
}

fn base64_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64[(n >> 18 & 0x3F) as usize] as char);
        out.push(B64[(n >> 12 & 0x3F) as usize] as char);
        out.push(if chunk.len() > 1 {
            B64[(n >> 6 & 0x3F) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[(n & 0x3F) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_types::{sample_png_bytes, ImageRef, Message, MessageRole};
    use tempfile::tempdir;

    #[test]
    fn missing_file_is_noted_and_dropped() {
        let dir = tempdir().unwrap();
        let mut msg = Message::new(MessageRole::User, "compare this")
            .with_attachments(vec![ImageRef::new("gone.png", "image/png", 12)]);
        apply_missing_image_notes(std::slice::from_mut(&mut msg), dir.path());
        assert!(msg.attachments.is_empty());
        assert!(msg.content.contains("no longer available"));
        assert!(msg.content.contains("gone.png"));
        assert!(!msg.content.contains("data:"));
    }

    #[test]
    fn freeze_copies_into_cache_and_retargets_path() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("shot.png"), sample_png_bytes()).unwrap();
        let cache = dir.path().join("cache");
        let mut content = "see".to_string();
        let frozen = freeze_attachments(
            dir.path(),
            &cache,
            &mut content,
            vec![ImageRef::new("shot.png", "image/png", 1)],
        );
        assert_eq!(frozen.len(), 1);
        assert_ne!(frozen[0].path, "shot.png");
        assert_eq!(content, "see");
        std::fs::remove_file(dir.path().join("shot.png")).unwrap();
        load_image_ref(dir.path(), &frozen[0]).expect("snapshot still readable");
    }

    #[test]
    fn freeze_missing_file_bakes_a_note() {
        let dir = tempdir().unwrap();
        let cache = dir.path().join("cache");
        let mut content = "compare this".to_string();
        let frozen = freeze_attachments(
            dir.path(),
            &cache,
            &mut content,
            vec![ImageRef::new("gone.png", "image/png", 12)],
        );
        assert!(frozen.is_empty());
        assert!(content.contains("no longer available"));
        assert!(content.contains("gone.png"));
    }

    #[test]
    fn present_png_stays_attached() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("shot.png"), sample_png_bytes()).unwrap();
        let mut msg = Message::new(MessageRole::User, "see").with_attachments(vec![ImageRef::new(
            "shot.png",
            "image/png",
            1,
        )]);
        apply_missing_image_notes(std::slice::from_mut(&mut msg), dir.path());
        assert_eq!(msg.attachments.len(), 1);
        assert_eq!(msg.content, "see");
    }

    #[test]
    fn data_url_omits_raw_in_log_preview_shape() {
        let url = data_url("image/png", b"abc");
        assert!(url.starts_with("data:image/png;base64,"));
        assert!(!url.contains("abc"));
    }
}
