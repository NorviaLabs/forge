use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

use crate::diff_view::{DiffEntry, DiffSide};
use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitChangeRow {
    Group(DiffSide),
    File(usize),
}

pub struct GitChangesList<'a> {
    pub entries: &'a [DiffEntry],
    /// Diff entry index; group headings are not selectable.
    pub selected: usize,
    pub focused: bool,
    pub hover: Option<usize>,
}

impl GitChangesList<'_> {
    pub fn rows(entries: &[DiffEntry]) -> Vec<GitChangeRow> {
        let mut rows = Vec::new();
        let mut prior = None;
        for (index, entry) in entries.iter().enumerate() {
            let side = entry.side.unwrap_or(DiffSide::Unstaged);
            if prior != Some(side) {
                rows.push(GitChangeRow::Group(side));
                prior = Some(side);
            }
            rows.push(GitChangeRow::File(index));
        }
        rows
    }

    pub fn file_index_at(entries: &[DiffEntry], row: usize) -> Option<usize> {
        match Self::rows(entries).get(row) {
            Some(GitChangeRow::File(index)) => Some(*index),
            _ => None,
        }
    }

    pub fn row_for_file(entries: &[DiffEntry], selected: usize, height: usize) -> usize {
        let rows = Self::rows(entries);
        let file_row = rows
            .iter()
            .position(|row| *row == GitChangeRow::File(selected))
            .unwrap_or(0);
        let start = file_row
            .saturating_sub(height / 2)
            .min(rows.len().saturating_sub(height));
        file_row.saturating_sub(start)
    }

    pub fn absolute_file_at(
        entries: &[DiffEntry],
        visible_row: usize,
        selected: usize,
        height: usize,
    ) -> Option<usize> {
        let rows = Self::rows(entries);
        let selected_row = rows
            .iter()
            .position(|row| *row == GitChangeRow::File(selected))
            .unwrap_or(0);
        let start = selected_row
            .saturating_sub(height / 2)
            .min(rows.len().saturating_sub(height));
        Self::file_index_at(entries, start + visible_row)
    }

    #[cfg(test)]
    pub fn render_text(entries: &[DiffEntry], selected: usize, width: u16, height: u16) -> String {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    GitChangesList {
                        entries,
                        selected,
                        focused: true,
                        hover: None,
                    },
                    frame.area(),
                );
            })
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
    }
}

impl Widget for GitChangesList<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .border_style(theme::panel_border())
            .style(theme::panel());
        let inner = block.inner(area);
        block.render(area, buf);
        theme::fill(inner, buf, theme::panel());
        let rows = Self::rows(self.entries);
        let file_row = rows
            .iter()
            .position(|row| *row == GitChangeRow::File(self.selected))
            .unwrap_or(0);
        let start = file_row
            .saturating_sub(inner.height as usize / 2)
            .min(rows.len().saturating_sub(inner.height as usize));
        let lines: Vec<Line> = rows
            .into_iter()
            .skip(start)
            .take(inner.height as usize)
            .map(|row| match row {
                GitChangeRow::Group(side) => {
                    let label = match side {
                        DiffSide::Staged => "STAGED",
                        DiffSide::Unstaged => "UNSTAGED",
                    };
                    Line::styled(
                        format!(" {label}"),
                        theme::metadata_style().add_modifier(Modifier::BOLD),
                    )
                }
                GitChangeRow::File(index) => {
                    let entry = &self.entries[index];
                    let selected = index == self.selected;
                    let hovered = self.hover == Some(index) && !selected;
                    let style = if selected && self.focused {
                        theme::selected_row()
                    } else if hovered {
                        theme::surface_hover().add_modifier(Modifier::BOLD)
                    } else {
                        theme::text()
                    };
                    Line::from(vec![
                        Span::styled(if selected && self.focused { "> " } else { "  " }, style),
                        Span::styled(format!("{} ", entry.marker), theme::metadata_style()),
                        Span::styled(entry.path.to_string_lossy().into_owned(), style),
                    ])
                }
            })
            .collect();
        Paragraph::new(lines).render(inner, buf);
    }
}
