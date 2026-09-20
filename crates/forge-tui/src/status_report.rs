//! Shared `/status` report data and rendering.

use crate::theme;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

/// One row of the `/status` report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusRow {
    Heading(String),
    Field {
        label: String,
        value: String,
        note: Option<String>,
    },
    Section {
        label: String,
        items: Vec<String>,
    },
    Gap,
}

impl StatusRow {
    pub fn field(label: &str, value: impl Into<String>) -> Self {
        Self::Field {
            label: label.into(),
            value: value.into(),
            note: None,
        }
    }

    pub fn field_with_note(label: &str, value: impl Into<String>, note: impl Into<String>) -> Self {
        Self::Field {
            label: label.into(),
            value: value.into(),
            note: Some(note.into()),
        }
    }
}

const STATUS_LABEL_WIDTH: usize = 16;

pub fn status_report_lines(rows: &[StatusRow], width: usize) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for row in rows {
        match row {
            StatusRow::Gap => out.push(Line::from("")),
            StatusRow::Heading(text) => out.push(Line::from(Span::styled(
                text.to_uppercase(),
                theme::metadata_style().add_modifier(Modifier::BOLD),
            ))),
            StatusRow::Field { label, value, note } => {
                let pad = STATUS_LABEL_WIDTH
                    .saturating_sub(label.chars().count())
                    .max(1);
                let mut spans = vec![
                    Span::styled(label.clone(), theme::muted()),
                    Span::raw(" ".repeat(pad)),
                    Span::styled(value.clone(), theme::text()),
                ];
                if let Some(note) = note {
                    spans.push(Span::raw("   "));
                    spans.push(Span::styled(note.clone(), theme::metadata_style()));
                }
                out.push(Line::from(spans));
            }
            StatusRow::Section { label, items } => {
                out.push(Line::from(Span::styled(
                    label.to_uppercase(),
                    theme::metadata_style().add_modifier(Modifier::BOLD),
                )));
                let indent = "  ";
                let budget = width.saturating_sub(indent.len()).max(8);
                let mut line = String::new();
                for (i, item) in items.iter().enumerate() {
                    let piece = if i + 1 == items.len() {
                        item.clone()
                    } else {
                        format!("{item} · ")
                    };
                    if line.chars().count() + piece.chars().count() > budget && !line.is_empty() {
                        out.push(Line::from(Span::styled(
                            format!("{indent}{line}"),
                            theme::text_secondary(),
                        )));
                        line = String::new();
                    }
                    line.push_str(&piece);
                }
                if !line.is_empty() {
                    out.push(Line::from(Span::styled(
                        format!("{indent}{line}"),
                        theme::text_secondary(),
                    )));
                }
            }
        }
    }
    out
}

pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}
