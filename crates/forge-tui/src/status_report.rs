//! Shared `/status` report data and rendering.

use crate::theme;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Row, Table, Widget};

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

/// Paint the report into `area`. Each run of fields is a label/value table,
/// and every run shares one label column so values align down the report.
pub fn render_status_report(rows: &[StatusRow], area: Rect, buf: &mut Buffer) {
    let label_w = rows
        .iter()
        .filter_map(|row| match row {
            StatusRow::Field { label, .. } => Some(crate::path_display::width(label)),
            _ => None,
        })
        .max()
        .unwrap_or(0)
        .min(usize::from(area.width) / 2) as u16;
    let mut y = area.y;
    let mut index = 0;
    while index < rows.len() && y < area.bottom() {
        let run = rows[index..]
            .iter()
            .take_while(|row| matches!(row, StatusRow::Field { .. }))
            .count();
        if run > 0 {
            let fields = rows[index..index + run].iter().filter_map(|row| match row {
                StatusRow::Field { label, value, note } => {
                    let mut value = vec![Span::styled(value.clone(), theme::text())];
                    if let Some(note) = note {
                        value.push(Span::raw("   "));
                        value.push(Span::styled(note.clone(), theme::metadata_style()));
                    }
                    Some(Row::new(vec![
                        Line::styled(label.clone(), theme::muted()),
                        Line::from(value),
                    ]))
                }
                _ => None,
            });
            let height = (run as u16).min(area.bottom() - y);
            Table::new(fields, [Constraint::Length(label_w), Constraint::Fill(1)])
                .column_spacing(2)
                .render(Rect { y, height, ..area }, buf);
            y += height;
            index += run;
            continue;
        }
        let lines = status_report_lines(&rows[index..=index], usize::from(area.width));
        let height = (lines.len() as u16).min(area.bottom() - y);
        Paragraph::new(lines).render(Rect { y, height, ..area }, buf);
        y += height;
        index += 1;
    }
}

/// Lines for headings, gaps and wrapped sections; fields render as a table in
/// [`render_status_report`].
fn status_report_lines(rows: &[StatusRow], width: usize) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for row in rows {
        match row {
            StatusRow::Gap => out.push(Line::from("")),
            StatusRow::Heading(text) => out.push(Line::from(Span::styled(
                text.to_uppercase(),
                theme::metadata_style().add_modifier(Modifier::BOLD),
            ))),
            StatusRow::Field { .. } => {}
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
                    if crate::path_display::width(&line) + crate::path_display::width(&piece)
                        > budget
                        && !line.is_empty()
                    {
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

#[cfg(test)]
mod render_tests {
    use super::*;

    /// Every field value starts in the same column, however long its label.
    #[test]
    fn status_fields_share_one_value_column() {
        let rows = vec![
            StatusRow::Heading("Session".into()),
            StatusRow::field("Model", "gpt"),
            StatusRow::field("Reasoning effort level", "high"),
            StatusRow::Gap,
            StatusRow::field_with_note("Context", "12%", "of 200k"),
        ];
        let area = Rect::new(0, 0, 60, 8);
        let mut buf = Buffer::empty(area);
        render_status_report(&rows, area, &mut buf);
        let row = |y: u16| -> String { (0..60).map(|x| buf[(x, y)].symbol()).collect() };
        assert!(row(0).starts_with("SESSION"));
        let starts: Vec<usize> = [(1, "gpt"), (2, "high"), (4, "12%")]
            .iter()
            .map(|(y, value)| row(*y).find(value).unwrap())
            .collect();
        assert!(starts.windows(2).all(|w| w[0] == w[1]), "{starts:?}");
        assert!(row(4).contains("of 200k"));
    }
}
