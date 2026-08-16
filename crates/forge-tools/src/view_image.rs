//! `view_image` — load a workspace image as a path-ref attachment.

use async_trait::async_trait;
use forge_types::{inspect_image, ImageRef, SideEffectClass, ToolOutput};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::AsyncReadExt;

use crate::builtins::schema_for;
use crate::registry::ToolContext;
use crate::{Tool, ToolError};

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ViewImageArgs {
    /// Path relative to workspace root (or absolute under workspace).
    pub path: String,
    /// v1 accepts only `high` (default). `original` and any other value error.
    #[serde(default)]
    pub detail: Option<String>,
}

pub struct ViewImageTool;

#[async_trait]
impl Tool for ViewImageTool {
    fn name(&self) -> &str {
        "view_image"
    }

    fn description(&self) -> &str {
        "Load a local image file (PNG, JPEG, GIF, or WebP) so you can see it. \
         Path must be inside the workspace. Use this instead of read_file for images."
    }

    fn input_schema(&self) -> Value {
        schema_for::<ViewImageArgs>()
    }

    fn side_effect_class(&self) -> SideEffectClass {
        SideEffectClass::Read
    }

    fn idempotent(&self) -> bool {
        true
    }

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        if !ctx.image_input {
            let model = if ctx.active_model.is_empty() {
                "the active model".to_string()
            } else {
                ctx.active_model.clone()
            };
            return Ok(ToolOutput::denied(format!(
                "view_image is not allowed because the active model ({model}) does not support image inputs"
            )));
        }

        let a: ViewImageArgs = serde_json::from_value(args).map_err(|e| {
            ToolError::Execution(format!("internal deserialize after validation: {e}"))
        })?;
        match a.detail.as_deref() {
            None | Some("high") => {}
            Some(other) => {
                return Ok(ToolOutput::failed_exit(
                    format!(
                        "view_image.detail only supports `high`; omit `detail` for the default. got `{other}`"
                    ),
                    None,
                ));
            }
        }

        let abs = ctx.resolve_path(&a.path)?;
        let file_len = tokio::fs::metadata(&abs).await?.len();
        if file_len > forge_types::MAX_IMAGE_BYTES {
            return Err(ToolError::Execution(format!(
                "image is {file_len} bytes; maximum allowed is {} bytes",
                forge_types::MAX_IMAGE_BYTES
            )));
        }
        let mut file = tokio::fs::File::open(&abs).await?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).await?;
        let meta = inspect_image(&bytes).map_err(|err| ToolError::Execution(err.to_string()))?;

        let rel = workspace_rel_path(&ctx.workspace_root, &abs).unwrap_or_else(|| a.path.clone());
        let image = ImageRef::new(rel.clone(), meta.mime, bytes.len() as u64)
            .with_dimensions(meta.width, meta.height);

        let mut summary = format!(
            "image loaded · {} · {}",
            display_size(bytes.len() as u64),
            rel
        );
        if let (Some(w), Some(h)) = (meta.width, meta.height) {
            summary = format!("{summary} · {w}×{h}");
        }

        Ok(ToolOutput {
            content: summary,
            is_error: false,
            exit_code: None,
            outcome: Some(forge_types::ExecutionOutcome::Success),
            attachments: vec![image],
        })
    }
}

pub(crate) fn workspace_rel_path(root: &std::path::Path, abs: &std::path::Path) -> Option<String> {
    let root = root.canonicalize().ok()?;
    let abs = abs.canonicalize().ok()?;
    let rel = abs.strip_prefix(&root).ok()?;
    Some(rel.to_string_lossy().replace('\\', "/"))
}

fn display_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{} KB", (bytes + 512) / 1024)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validation::validate_args;
    use forge_types::sample_png_bytes;
    use serde_json::json;
    use tempfile::tempdir;

    fn ctx_with_image(dir: &std::path::Path, allowed: bool) -> ToolContext {
        let mut ctx = ToolContext::new(dir.to_path_buf());
        ctx.image_input = allowed;
        ctx.active_model = "anthropic/claude-sonnet".into();
        ctx
    }

    #[tokio::test]
    async fn loads_png_as_path_ref() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("shot.png");
        std::fs::write(&path, sample_png_bytes()).unwrap();
        let ctx = ctx_with_image(dir.path(), true);
        let out = ViewImageTool
            .call(&ctx, json!({"path": "shot.png"}))
            .await
            .unwrap();
        assert!(!out.is_error);
        assert!(out.content.contains("shot.png"));
        assert!(!out.content.contains("data:"));
        assert_eq!(out.attachments.len(), 1);
        assert_eq!(out.attachments[0].path, "shot.png");
        assert_eq!(out.attachments[0].mime, "image/png");
        assert_eq!(out.attachments[0].detail, "high");
    }

    #[tokio::test]
    async fn rejects_original_detail() {
        let dir = tempdir().unwrap();
        let ctx = ctx_with_image(dir.path(), true);
        let out = ViewImageTool
            .call(&ctx, json!({"path": "shot.png", "detail": "original"}))
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("only supports `high`"));
        assert!(out.attachments.is_empty());
    }

    #[tokio::test]
    async fn rejects_when_model_has_no_image_input() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("shot.png"), sample_png_bytes()).unwrap();
        let ctx = ctx_with_image(dir.path(), false);
        let out = ViewImageTool
            .call(&ctx, json!({"path": "shot.png"}))
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("does not support image inputs"));
        assert!(out.content.contains("anthropic/claude-sonnet"));
        assert!(out.attachments.is_empty());
    }

    #[tokio::test]
    async fn rejects_oversized_file_before_reading() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("huge.png");
        {
            let file = std::fs::File::create(&path).unwrap();
            file.set_len(forge_types::MAX_IMAGE_BYTES + 1).unwrap();
        }
        let ctx = ctx_with_image(dir.path(), true);
        let err = ViewImageTool
            .call(&ctx, json!({"path": "huge.png"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("maximum allowed"), "{err}");
    }

    #[tokio::test]
    async fn rejects_outside_workspace() {
        let dir = tempdir().unwrap();
        let ctx = ctx_with_image(dir.path(), true);
        let err = ViewImageTool
            .call(&ctx, json!({"path": "/etc/passwd"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("escapes workspace"));
    }

    #[test]
    fn schema_requires_path() {
        let t = ViewImageTool;
        validate_args("view_image", &t.input_schema(), &json!({})).unwrap_err();
    }
}
