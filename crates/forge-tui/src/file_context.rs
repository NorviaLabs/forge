//! Active-file context attachment for agent requests.

use std::path::Path;

/// A context block attached to the next user message.
#[derive(Debug, Clone)]
pub struct FileAttachment {
    /// Repository-relative path for display and identification.
    pub rel_path: String,
    /// 1-based cursor line as shown to the user.
    pub cursor_line: usize,
}

impl FileAttachment {
    pub fn new(rel_path: String, cursor_line: usize) -> Self {
        Self {
            rel_path,
            cursor_line,
        }
    }

    /// A compact label shown in the input area. Line is 1-based for user display.
    pub fn label(&self) -> String {
        format!("Context: {}:{}", self.rel_path, self.cursor_line + 1)
    }
}

/// Build the attachment context block that gets prepended to the user message.
///
/// The `cursor_line` is 0-based internal, converted to 1-based in the output.
pub fn build_attachment_text(
    file_path: &Path,
    cursor_line: usize,
    rel_path: &str,
    max_excerpt_lines: usize,
) -> Result<String, AttachmentError> {
    let contents = std::fs::read_to_string(file_path).map_err(|_| AttachmentError::Unreadable)?;

    let lines: Vec<&str> = contents.lines().collect();
    let total_lines = lines.len();
    let cursor_1based = cursor_line + 1;

    // Compute excerpt range: ~half before, half after cursor.
    let half = max_excerpt_lines / 2;
    let start = cursor_line.saturating_sub(half);
    let end = (start + max_excerpt_lines).min(total_lines);
    let excerpt_start = start;
    let excerpt_end = end;

    let mut out = String::new();
    out.push_str(&format!(
        "Active file: {rel_path}\nCursor line: {cursor_1based}\n\n"
    ));

    if total_lines > max_excerpt_lines {
        out.push_str(&format!(
            "Relevant excerpt (lines {}-{} of {total_lines}):\n",
            excerpt_start + 1,
            excerpt_end,
        ));
    } else {
        out.push_str("File contents:\n");
    }

    for (i, line) in lines[excerpt_start..excerpt_end].iter().enumerate() {
        let line_num = excerpt_start + i + 1;
        let marker = if line_num == cursor_1based {
            " →"
        } else {
            "  "
        };
        out.push_str(&format!("{}{:>4}| {}\n", marker, line_num, line));
    }

    if total_lines > max_excerpt_lines {
        out.push_str(&format!(
            "\n[Excerpt truncated to {} lines. Full file has {total_lines} lines.]\n",
            max_excerpt_lines,
        ));
    }

    Ok(out)
}

#[derive(Debug)]
pub enum AttachmentError {
    Unreadable,
    OutsideRepository,
    Binary,
}

impl std::fmt::Display for AttachmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreadable => write!(f, "Unable to attach current file"),
            Self::OutsideRepository => write!(f, "File is outside the repository"),
            Self::Binary => write!(f, "Cannot attach binary files"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn build_attachment_text_small_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.rs");
        fs::write(&path, "line1\nline2\nline3\n").unwrap();

        let text = build_attachment_text(&path, 1, "test.rs", 100).unwrap();
        assert!(text.contains("Active file: test.rs"));
        assert!(text.contains("Cursor line: 2"));
        assert!(text.contains("File contents:"));
        assert!(text.contains("   1| line1"));
        assert!(text.contains(" →   2| line2"));
        assert!(text.contains("   3| line3"));
    }

    #[test]
    fn build_attachment_text_large_file_truncates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.rs");
        let mut content = String::new();
        for i in 0..300 {
            content.push_str(&format!("line {i}\n"));
        }
        fs::write(&path, &content).unwrap();

        let text = build_attachment_text(&path, 150, "large.rs", 100).unwrap();
        assert!(text.contains("Excerpt truncated"));
        assert!(text.contains("lines 101-200 of 300"));
        assert!(text.contains(" → 151| line 150"));
    }

    #[test]
    fn build_attachment_text_cursor_at_start() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("start.rs");
        let mut content = String::new();
        for i in 0..200 {
            content.push_str(&format!("line {i}\n"));
        }
        fs::write(&path, &content).unwrap();

        let text = build_attachment_text(&path, 0, "start.rs", 100).unwrap();
        assert!(text.contains(" →   1| line 0"));
        assert!(text.contains("lines 1-100 of 200"));
    }

    #[test]
    fn build_attachment_text_cursor_at_end() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("end.rs");
        let mut content = String::new();
        for i in 0..200 {
            content.push_str(&format!("line {i}\n"));
        }
        fs::write(&path, &content).unwrap();

        let text = build_attachment_text(&path, 199, "end.rs", 100).unwrap();
        assert!(text.contains(" → 200| line 199"));
        assert!(text.contains("lines 150-200 of 200"));
    }

    #[test]
    fn build_attachment_text_deleted_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gone.rs");
        // Don't write the file.
        match build_attachment_text(&path, 0, "gone.rs", 100) {
            Err(AttachmentError::Unreadable) => {}
            other => panic!("expected Unreadable, got {other:?}"),
        }
    }

    #[test]
    fn attachment_label_uses_one_based_line() {
        let a = FileAttachment::new("src/main.rs".into(), 0);
        assert_eq!(a.label(), "Context: src/main.rs:1");

        let a = FileAttachment::new("src/main.rs".into(), 41);
        assert_eq!(a.label(), "Context: src/main.rs:42");
    }
}
