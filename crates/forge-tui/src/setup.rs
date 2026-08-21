//! Pre-session setup shell: theme, then directory trust. No `path_for`.

use std::io::{self, stdout};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crossterm::cursor::SetCursorStyle;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use forge_config::{grant_trust, persist_committed_theme, DEFAULT_THEME_ID, HOME_PROJECTS_DIR};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};
use ratatui::Terminal;

use crate::overlays::{handle_overlay_key, Key, Overlay, OverlayAction, OverlayWidget};
use crate::theme;
use crate::theme_preview::render_theme_preview;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupResult {
    Completed,
    Canceled,
}

pub struct SetupRequest {
    pub run_theme: bool,
    pub run_trust: bool,
    pub cwd: PathBuf,
}

enum Screen {
    Theme(Box<Overlay>),
    Trust { selected: usize },
}

pub fn run_setup(request: SetupRequest) -> io::Result<SetupResult> {
    if !request.run_theme && !request.run_trust {
        return Ok(SetupResult::Completed);
    }
    let registry = crate::theme_registry::ThemeRegistry::load(None);
    let theme_id = registry.resolve_startup_id(DEFAULT_THEME_ID);
    theme::install(registry, theme_id);

    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        SetCursorStyle::SteadyBlock,
        EnableMouseCapture
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = run_setup_loop(&mut terminal, &request);
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    result
}

fn run_setup_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    request: &SetupRequest,
) -> io::Result<SetupResult> {
    let display = forge_config::trust_display_path(&request.cwd);
    let wide = is_wide_root(Path::new(&display));
    let mut screen = if request.run_theme {
        Screen::Theme(Box::new(Overlay::theme_open(DEFAULT_THEME_ID)))
    } else {
        Screen::Trust { selected: 0 }
    };

    loop {
        terminal.draw(|f| draw_setup(f.area(), f.buffer_mut(), &screen, &display, wide))?;
        if !event::poll(Duration::from_millis(200))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
            continue;
        }
        let Some(mapped) = map_key(key.code, key.modifiers) else {
            continue;
        };
        match &mut screen {
            Screen::Theme(overlay) => match handle_overlay_key(overlay, mapped) {
                OverlayAction::Close => return Ok(SetupResult::Canceled),
                OverlayAction::PreviewTheme(id) => theme::set_active(id),
                OverlayAction::SelectTheme(id) => {
                    theme::set_active(&id);
                    if persist_committed_theme(&id).is_err() {
                        continue;
                    }
                    if request.run_trust {
                        screen = Screen::Trust { selected: 0 };
                    } else {
                        return Ok(SetupResult::Completed);
                    }
                }
                _ => {}
            },
            Screen::Trust { selected } => match mapped {
                Key::Esc => return Ok(SetupResult::Canceled),
                Key::Up | Key::Down => *selected = 1 - *selected,
                Key::Enter => {
                    if *selected == 1 {
                        return Ok(SetupResult::Canceled);
                    }
                    if grant_trust(&request.cwd).is_err() {
                        continue;
                    }
                    return Ok(SetupResult::Completed);
                }
                _ => {}
            },
        }
    }
}

fn map_key(code: KeyCode, modifiers: KeyModifiers) -> Option<Key> {
    match code {
        KeyCode::Esc => Some(Key::Esc),
        KeyCode::Enter => Some(Key::Enter),
        KeyCode::Up => Some(Key::Up),
        KeyCode::Down => Some(Key::Down),
        KeyCode::Char('j') if !modifiers.contains(KeyModifiers::CONTROL) => Some(Key::Down),
        KeyCode::Char('k') if !modifiers.contains(KeyModifiers::CONTROL) => Some(Key::Up),
        _ => None,
    }
}

fn is_wide_root(path: &Path) -> bool {
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    let Ok(home) = home.canonicalize() else {
        return path == home.as_path();
    };
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    path == home || path == home.join(HOME_PROJECTS_DIR)
}

fn draw_setup(
    area: Rect,
    buf: &mut ratatui::buffer::Buffer,
    screen: &Screen,
    display: &str,
    wide: bool,
) {
    theme::fill(area, buf, theme::canvas());
    match screen {
        Screen::Theme(overlay) => draw_theme_setup(area, buf, overlay),
        Screen::Trust { selected } => draw_trust_setup(area, buf, display, wide, *selected),
    }
}

fn draw_theme_setup(area: Rect, buf: &mut ratatui::buffer::Buffer, overlay: &Overlay) {
    let Overlay::Theme {
        selected,
        current,
        items,
    } = overlay
    else {
        OverlayWidget { overlay }.render(area, buf);
        return;
    };
    let title = Paragraph::new(Line::from(vec![
        Span::styled("FORGE", theme::brand().add_modifier(Modifier::BOLD)),
        Span::styled("  Choose a theme", theme::text()),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(theme::border())
            .style(theme::panel())
            .title(Span::styled(
                format!(" Theme · {} ", crate::hints::hint_text(crate::hints::THEME)),
                theme::brand(),
            )),
    );
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(8)])
        .split(area);
    title.render(chunks[0], buf);
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(36), Constraint::Min(28)])
        .split(chunks[1]);
    crate::overlays::render_theme_dock(*selected, current, items, body[0], buf);
    if let Some((id, _)) = items.get(*selected) {
        render_theme_preview(id, body[1], buf);
    }
}

fn draw_trust_setup(
    area: Rect,
    buf: &mut ratatui::buffer::Buffer,
    display: &str,
    wide: bool,
    selected: usize,
) {
    let mut lines = vec![
        Line::from(Span::styled("Trust this folder?", theme::brand())),
        Line::from(""),
        Line::from(Span::styled("Accessing workspace:", theme::muted())),
        Line::from(Span::styled(
            crate::path_display::elide_path(display, TRUST_PATH_WIDTH),
            theme::text(),
        )),
        Line::from(""),
        Line::from(
            "Forge may read, edit, and run tools with this folder as the working directory.",
        ),
        Line::from("Every current and future subdirectory is trusted without asking again."),
        Line::from("Sibling folders and git worktrees next to this path are not trusted."),
    ];
    if wide {
        lines.push(Line::from(Span::styled(
            "Confirming this path trusts every project you later create under it.",
            theme::warn(),
        )));
    }
    lines.push(Line::from(""));
    lines.push(choice_line(selected == 0, "Yes, I trust this folder"));
    lines.push(choice_line(selected == 1, "No, exit"));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!(
            "{}  ·  no project files written if you leave",
            crate::hints::hint_text(crate::hints::TRUST)
        ),
        theme::muted(),
    )));
    // Sized to what it holds, with a column of inset on each side. It used to
    // take 70% of the height whatever it contained — twenty-eight rows for ten
    // rows of content — with text flush against the border.
    let height = (lines.len() as u16).saturating_add(4);
    let r = crate::overlays::centered_content_rect(area, TRUST_CARD_WIDTH, height, area.height);
    Paragraph::new(lines)
        .wrap(ratatui::widgets::Wrap { trim: true })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::border())
                .style(theme::panel())
                .padding(ratatui::widgets::Padding::new(2, 2, 1, 1))
                .title(Span::styled(" Trust ", theme::brand())),
        )
        .render(r, buf);
}

/// Widest the trust card is allowed to draw.
const TRUST_CARD_WIDTH: u16 = 84;

/// Columns the workspace path may take before it is elided. Without this the
/// path wrapped and left a fragment (`2`) alone on the next line.
const TRUST_PATH_WIDTH: usize = 76;

fn choice_line(on: bool, label: &str) -> Line<'static> {
    if on {
        Line::from(Span::styled(
            format!("▶ {label}"),
            theme::focused_selection_style(),
        ))
    } else {
        Line::from(Span::styled(format!("  {label}"), theme::text()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn wide_root_detects_home_and_projects() {
        let _lock = crate::app::tests::helpers::lock_test_env();
        let Some(home) = dirs::home_dir() else {
            return;
        };
        assert!(is_wide_root(&home));
        assert!(is_wide_root(&home.join(HOME_PROJECTS_DIR)));
        let other = TempDir::new().unwrap();
        assert!(!is_wide_root(other.path()));
    }

    #[test]
    fn skip_setup_when_nothing_pending() {
        let dir = TempDir::new().unwrap();
        let result = run_setup(SetupRequest {
            run_theme: false,
            run_trust: false,
            cwd: dir.path().to_path_buf(),
        })
        .unwrap();
        assert_eq!(result, SetupResult::Completed);
    }
}
