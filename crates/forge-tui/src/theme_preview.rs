//! Compact TUI snippet for the theme picker: status, syntax, diff, chat,
//! approval, composer, and footer tokens of the focused theme.

use crate::theme::{self, Palette};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, Borders, Widget};

pub fn render_theme_preview(theme_id: &str, area: Rect, buf: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let palette = theme::palette(theme_id);
    let syntax = theme::syntax_theme_for(theme_id);
    ratatui::widgets::Clear.render(area, buf);
    theme::fill(
        area,
        buf,
        Style::default().bg(palette.panel).fg(palette.text),
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(palette.border))
        .style(Style::default().bg(palette.panel))
        .title(Span::styled(
            " Preview ",
            Style::default()
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let mut y = inner.y;
    let max_y = inner.y.saturating_add(inner.height);
    let x = inner.x;
    let w = inner.width;

    put(
        buf,
        x,
        y,
        w,
        "⌂ ~/demo  ·  ⎇ main*",
        Style::default().fg(palette.text).bg(palette.panel),
    );
    y = y.saturating_add(1);
    if y >= max_y {
        return;
    }

    write_syntax_sample(buf, x, y, w, &syntax, palette.canvas);
    y = y.saturating_add(1);
    if y >= max_y {
        return;
    }
    put(
        buf,
        x,
        y,
        w,
        "-  let theme = \"dark\";",
        Style::default().fg(palette.danger).bg(palette.diff_remove),
    );
    y = y.saturating_add(1);
    if y >= max_y {
        return;
    }
    put(
        buf,
        x,
        y,
        w,
        "+  row.tag = \"connected\";",
        Style::default().fg(palette.ok).bg(palette.diff_add),
    );
    y = y.saturating_add(1);
    if y >= max_y {
        return;
    }

    put(
        buf,
        x,
        y,
        w,
        "› Ship the theme picker.",
        Style::default().fg(palette.text).bg(palette.panel),
    );
    if w > 2 {
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_style(Style::default().fg(palette.accent).bg(palette.panel));
        }
    }
    y = y.saturating_add(1);
    if y >= max_y {
        return;
    }
    put(
        buf,
        x,
        y,
        w,
        "Preview is live. Nothing written yet.",
        Style::default().fg(palette.agent).bg(palette.panel),
    );
    y = y.saturating_add(1);
    if y >= max_y {
        return;
    }

    put(
        buf,
        x,
        y,
        w,
        "⏸ approval · bash",
        Style::default()
            .fg(palette.warn)
            .bg(palette.panel)
            .add_modifier(Modifier::BOLD),
    );
    y = y.saturating_add(1);
    if y >= max_y {
        return;
    }
    put(
        buf,
        x,
        y,
        w,
        "  git push origin main",
        Style::default().fg(palette.text).bg(palette.panel),
    );
    // Approval card uses the waiting border as a left rule.
    if let Some(cell) = buf.cell_mut((x, y.saturating_sub(1))) {
        cell.set_style(
            Style::default()
                .fg(palette.waiting_border)
                .bg(palette.panel),
        );
    }
    y = y.saturating_add(1);
    if y >= max_y {
        return;
    }
    put(
        buf,
        x,
        y,
        w,
        "▶ Run once",
        Style::default()
            .fg(palette.selection_fg)
            .bg(palette.selection),
    );
    y = y.saturating_add(1);
    if y >= max_y {
        return;
    }

    put(
        buf,
        x,
        y,
        w,
        "> What does this project do?",
        Style::default().fg(palette.dim).bg(palette.panel_alt),
    );
    if let Some(cell) = buf.cell_mut((x, y)) {
        cell.set_style(
            Style::default()
                .fg(palette.accent)
                .bg(palette.panel_alt)
                .add_modifier(Modifier::BOLD),
        );
    }
    y = y.saturating_add(1);
    if y >= max_y {
        return;
    }
    put(
        buf,
        x,
        y,
        w,
        "● Ready   ok  wait  error  info",
        Style::default().fg(palette.muted).bg(palette.canvas),
    );
    paint_footer_tokens(buf, x, y, w, &palette);
}

fn write_syntax_sample(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    w: u16,
    syntax: &forge_syntax::HighlightTheme,
    bg: ratatui::style::Color,
) {
    // "fn ready(id: &str) {"
    let parts: [(&str, (u8, u8, u8)); 8] = [
        ("fn ", syntax.keyword),
        ("ready", syntax.function),
        ("(", syntax.punctuation),
        ("id", syntax.variable),
        (": ", syntax.punctuation),
        ("&str", syntax.type_),
        (") ", syntax.punctuation),
        ("{", syntax.punctuation),
    ];
    let mut col = x;
    let end = x.saturating_add(w);
    for (text, rgb) in parts {
        if col >= end {
            break;
        }
        let style = theme::syntax_segment(rgb, Some(bg));
        put(buf, col, y, end.saturating_sub(col), text, style);
        col = col.saturating_add(text.chars().count() as u16);
    }
}

fn paint_footer_tokens(buf: &mut Buffer, x: u16, y: u16, w: u16, palette: &Palette) {
    if w < 20 {
        return;
    }
    // Overlay token colors on the already-written footer line.
    // "● Ready   ok  wait  error  info"
    let tokens = [
        (0u16, 1u16, palette.ok),
        (2, 5, palette.ok),
        (10, 2, palette.ok),
        (14, 4, palette.warn),
        (20, 5, palette.danger),
        (27, 4, palette.info),
    ];
    for (start, len, color) in tokens {
        if start >= w {
            continue;
        }
        for i in 0..len {
            let col = x.saturating_add(start).saturating_add(i);
            if col >= x.saturating_add(w) {
                break;
            }
            if let Some(cell) = buf.cell_mut((col, y)) {
                cell.set_style(Style::default().fg(color).bg(palette.canvas));
            }
        }
    }
}

fn put(buf: &mut Buffer, x: u16, y: u16, width: u16, text: &str, style: Style) {
    if width == 0 {
        return;
    }
    buf.set_stringn(x, y, text, width as usize, style);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme_registry::ThemeRegistry;
    use forge_config::{THEME_FORGE_DARK, THEME_FORGE_LIGHT};
    use ratatui::layout::Rect;

    fn buffer_text(buf: &Buffer, area: Rect) -> String {
        let mut out = String::new();
        for y in area.y..area.y.saturating_add(area.height) {
            for x in area.x..area.x.saturating_add(area.width) {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn preview_shows_status_syntax_diff_approval_and_composer() {
        crate::theme::install(ThemeRegistry::load(None), THEME_FORGE_DARK);
        let area = Rect::new(0, 0, 48, 14);
        let mut buf = Buffer::empty(area);
        render_theme_preview(THEME_FORGE_DARK, area, &mut buf);
        let text = buffer_text(&buf, area);
        assert!(text.contains("Preview"), "{text}");
        assert!(text.contains("~/demo"), "{text}");
        assert!(text.contains("fn "), "{text}");
        assert!(text.contains("connected"), "{text}");
        assert!(text.contains("approval"), "{text}");
        assert!(text.contains("What does this project do?"), "{text}");
        assert!(text.contains("Ready"), "{text}");
    }

    #[test]
    fn preview_uses_focused_theme_colors() {
        crate::theme::install(ThemeRegistry::load(None), THEME_FORGE_DARK);
        let area = Rect::new(0, 0, 40, 12);
        let mut dark = Buffer::empty(area);
        let mut light = Buffer::empty(area);
        render_theme_preview(THEME_FORGE_DARK, area, &mut dark);
        render_theme_preview(THEME_FORGE_LIGHT, area, &mut light);
        let dark_bg = dark[(2, 1)].bg;
        let light_bg = light[(2, 1)].bg;
        assert_ne!(
            dark_bg, light_bg,
            "preview must not reuse one theme's surface for every id"
        );
    }
}
