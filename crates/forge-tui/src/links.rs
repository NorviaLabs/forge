//! Terminal hyperlinks: which destinations may become clickable, and how the
//! OSC 8 sequence reaches the cells.
//!
//! Forge renders into a ratatui `Buffer`, where every grapheme owns a cell and
//! every column is accountable. An OSC 8 sequence is zero-width on screen but
//! its bytes are ordinary characters to `Span::width`, so a destination cannot
//! travel inside span content: the bytes would be measured as columns and every
//! wrapped line after the link would land in the wrong place. Destinations
//! therefore ride *beside* the text ([`HyperlinkLine`]) and the sequence is
//! written into the buffer's cells once layout has already been computed.
//!
//! Every destination here is model output. `FORGE-DESIGN.md` §9 states the rule
//! for the notification path — "Text is sanitized, not trusted… an embedded
//! `ESC` or `BEL` would terminate the OSC 9 sequence early and leave the
//! remainder to be read as terminal commands" — and the same hazard applies to
//! `OSC 8`: a destination is bytes pasted into an escape sequence. [`destination_for`]
//! is the single gate that decides what may be emitted at all.
//!
//! The module is public because the trait that renders the transcript
//! (`conversation::ConversationRender`) is public, and the row type it returns
//! has to be nameable by whatever calls it.

use std::num::NonZeroU16;
use std::ops::{Deref, DerefMut, Range};

use ratatui::buffer::{Buffer, CellDiffOption, CellWidth};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;

use crate::theme;

/// Whether this process should emit hyperlinks at all.
///
/// Resolved once: the environment cannot change under a running TUI, and the
/// answer is read on every paint. `TERM_PROGRAM` is the same source the
/// notification path trusts, and `$TMUX` alone is enough to say no — see
/// [`terminal_renders_hyperlinks`] for why a multiplexer is disqualifying
/// rather than merely unhelpful.
pub(crate) fn hyperlinks_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        terminal_renders_hyperlinks(
            crate::notify::term_program().as_deref(),
            std::env::var("TMUX").ok().as_deref(),
        )
    })
}

/// Longest destination worth emitting, in bytes.
///
/// The sequence is repeated in every covered cell, so a pathological URL is
/// paid for once per column of the link. Data URIs and generated blob links run
/// to kilobytes and are never worth clicking; past this they stay plain text.
const MAX_DESTINATION_BYTES: usize = 2048;

/// Whether the destination may become clickable at all.
///
/// Returns the destination to emit, or `None` for text that must stay inert.
///
/// Rejecting is deliberate, and stricter than repairing. A control byte, a
/// space, or a missing host inside a URL is either corruption or an injection
/// attempt; the honest reading of either is "this is not a link", and the label
/// still renders as ordinary text. Only `http` and `https` are accepted: every
/// other scheme hands the click to the OS or to another program (`file:`,
/// `mailto:`, `vscode:`, `javascript:`), which is a decision this module is not
/// entitled to make on the model's behalf.
pub(crate) fn destination_for(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_DESTINATION_BYTES {
        return None;
    }
    // `ESC` would restart a sequence, `BEL` and `ST` would end it early, and
    // newlines would split the escape across rows.
    if trimmed.chars().any(char::is_control) || trimmed.contains(char::is_whitespace) {
        return None;
    }
    let rest = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))?;
    // After the scheme the authority runs to the first `/`, `?` or `#`.
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    // Userinfo is allowed (`user@host`), but a bare `@` or empty host is not a
    // destination anyone can resolve.
    let host = authority.rsplit('@').next().unwrap_or_default();
    if host.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

/// The style an actionable link label carries.
///
/// `FORGE-DESIGN.md` §9: the underline is the promise that the destination may
/// actually be emitted, and the label carries a hue of its own so a link is
/// not just another underlined run. `accent` stays out of it because accent
/// means focus ("where am I") rather than "this is clickable", and the footer
/// already underlines its focused chips in that hue — a link in the same
/// colour would read as chrome. `theme::link_color` is the neutral-information
/// hue: in monochrome the underline still carries the affordance, so the
/// colour is an addition to the promise rather than the promise itself. The
/// explicit-markdown path, the bare-URL path and the prompt path all take this
/// one definition, so no change here can make them look like different kinds
/// of thing.
pub(crate) fn link_label_style() -> Style {
    Style::default()
        .fg(theme::link_color())
        .add_modifier(Modifier::UNDERLINED)
}

/// Byte ranges of the bare `http`/`https` URLs in `text`, each with the
/// destination it may become.
///
/// Markdown is not the only way a URL reaches prose. A model writes
/// `https://example.com` on its own far more often than it writes
/// `[example](https://example.com)`, and a person pastes a URL into a prompt
/// with no markup at all. Both are text the reader may want to open, and
/// neither had a destination before.
///
/// [`destination_for`] stays the single gate: a range is returned only once it
/// has passed policy, so a caller attaches what it is handed without
/// re-checking anything. The scan is conservative at both ends on purpose. A
/// run may only begin at a boundary, so `notaurlhttps://x` is not carved up
/// mid-word, and trailing sentence punctuation stays in the prose, so
/// `see https://example.com.` links the host and not the full stop. A closing
/// bracket is trimmed only when the run does not open one:
/// `(https://example.com)` loses its parenthesis while
/// `https://en.wikipedia.org/wiki/Foo_(bar)` keeps its own.
pub(crate) fn autolink_matches(text: &str) -> Vec<(Range<usize>, String)> {
    // A scheme is only recognized in lowercase, which is what keeps an
    // uppercased heading label from being read as a destination whose text no
    // longer matches it.
    const SCHEMES: [&str; 2] = ["https://", "http://"];
    let mut matches = Vec::new();
    let mut cursor = 0usize;
    while cursor < text.len() {
        let rest = &text[cursor..];
        let Some(offset) = SCHEMES.iter().filter_map(|scheme| rest.find(*scheme)).min() else {
            break;
        };
        let start = cursor + offset;
        // A run belongs to the word it starts, not the one it interrupts.
        if text[..start]
            .chars()
            .next_back()
            .is_some_and(char::is_alphanumeric)
        {
            cursor = start + 1;
            continue;
        }
        let candidate = trim_url_tail(&text[start..url_run_end(text, start)]);
        match destination_for(candidate) {
            Some(destination) => {
                let end = start + candidate.len();
                matches.push((start..end, destination));
                cursor = end;
            }
            // Not a destination after all (`https://`, an injected byte): step
            // one byte in and keep looking rather than skipping the rest of
            // the run, which may hold a real URL.
            None => cursor = start + 1,
        }
    }
    matches
}

/// One past the last byte of the URL run beginning at `start`.
fn url_run_end(text: &str, start: usize) -> usize {
    text[start..]
        .char_indices()
        .find(|(_, c)| is_url_delimiter(*c))
        .map_or(text.len(), |(offset, _)| start + offset)
}

/// Characters that end a URL run. Whitespace always does; the quotes, the
/// backtick and the angle brackets are what wrap a URL in prose or in markup.
fn is_url_delimiter(c: char) -> bool {
    c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | '`')
}

/// Drop the punctuation that belongs to the sentence rather than the URL.
fn trim_url_tail(candidate: &str) -> &str {
    let trimmed = candidate.trim_end_matches(|c: char| {
        matches!(
            c,
            '.' | ',' | ';' | ':' | '!' | '?' | ']' | '}' | '\'' | '"'
        )
    });
    let mut out = trimmed;
    while out.ends_with(')') && out.matches(')').count() > out.matches('(').count() {
        out = &out[..out.len() - 1];
    }
    out
}

/// Whether the surrounding terminal renders `OSC 8` at all.
///
/// Forge cannot probe for this, so it keeps a known-good list, exactly as the
/// notification path does with `OSC 9` (`notify::OSC9_TERMINALS`). The lists
/// differ on purpose: Alacritty and VS Code render hyperlinks but not
/// notifications, so neither appears over there.
///
/// A multiplexer defeats the check. `tmux` only forwards hyperlinks from 3.4
/// onwards and only when the user has added
/// `set -ga terminal-features "*:hyperlinks"`, and `TERM_PROGRAM` still names
/// the *outer* terminal either way, so a tmux session on an older server would
/// be falsely allowlisted. `screen` cannot forward them at all. Both stay
/// inert: a dead underline costs nothing, while escape bytes in a multiplexer
/// that cannot interpret them risk reaching the screen as garbage.
pub(crate) fn terminal_renders_hyperlinks(
    term_program: Option<&str>,
    multiplexer: Option<&str>,
) -> bool {
    if multiplexer.is_some_and(|value| !value.trim().is_empty()) {
        return false;
    }
    let Some(program) = term_program else {
        return false;
    };
    const TERMINALS: &[&str] = &[
        "ghostty",
        "kitty",
        "iterm.app",
        "wezterm",
        "alacritty",
        "vscode",
        "vscode-insiders",
    ];
    let program = program.trim().to_ascii_lowercase();
    TERMINALS.contains(&program.as_str())
}

/// One clickable run of columns on a rendered row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalHyperlink {
    /// Columns on the row, in reading order, zero-based from the pane's left
    /// edge. Half-open, like every other column range in the renderer.
    pub columns: Range<usize>,
    pub destination: String,
}

impl TerminalHyperlink {
    pub fn new(columns: Range<usize>, destination: impl Into<String>) -> Self {
        Self {
            columns,
            destination: destination.into(),
        }
    }
}

/// A rendered row plus the destinations hidden inside its columns.
///
/// Derefs to the `Line` it wraps so the renderer keeps measuring, painting and
/// scrolling ordinary text; the link table is the only extra cargo.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HyperlinkLine {
    pub line: Line<'static>,
    pub links: Vec<TerminalHyperlink>,
}

impl HyperlinkLine {
    pub fn new(line: Line<'static>) -> Self {
        Self {
            line,
            links: Vec::new(),
        }
    }

    pub fn with_links(line: Line<'static>, links: Vec<TerminalHyperlink>) -> Self {
        Self { line, links }
    }
}

impl From<Line<'static>> for HyperlinkLine {
    fn from(line: Line<'static>) -> Self {
        Self::new(line)
    }
}

impl Deref for HyperlinkLine {
    type Target = Line<'static>;

    fn deref(&self) -> &Self::Target {
        &self.line
    }
}

impl DerefMut for HyperlinkLine {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.line
    }
}

/// The sequence that attaches `destination` to `text`.
///
/// `BEL` terminates the sequence rather than `ST`, matching `clipboard::write_osc52`
/// — terminals accept both, and the shorter form is what the adoption matrix
/// tests with.
pub(crate) fn osc8(destination: &str, text: &str) -> String {
    format!("\x1b]8;;{destination}\x07{text}\x1b]8;;\x07")
}

/// Attach `hyperlinks` to the row that was just rendered at `area`.
///
/// Called after a line has been painted, because the escape bytes carry no
/// width: the cell's own symbol is replaced by the same symbol wrapped in the
/// sequence, so the column it occupies and its screen width are unchanged.
/// Columns past the pane edge are skipped — a link clipped by a narrow pane
/// must not push an escape into the next row.
pub(crate) fn mark_buffer_hyperlinks(
    buf: &mut Buffer,
    area: Rect,
    hyperlinks: &[TerminalHyperlink],
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    for hyperlink in hyperlinks {
        // Re-checked even though `destination_for` gated this upstream: this is
        // the function that writes bytes into the terminal, and a struct any
        // caller can build cannot be the only gate between model output and an
        // escape sequence. A destination carrying `BEL` would end the sequence
        // early and leave the remainder to be read as terminal commands, so the
        // writer refuses what the policy refuses and no future caller can
        // reopen that hole by constructing the struct directly.
        if destination_for(&hyperlink.destination).is_none() {
            continue;
        }
        let start = hyperlink.columns.start as u16;
        let end = (hyperlink.columns.end as u16).min(area.width);
        for column in start..end {
            let Some(cell) = buf.cell_mut(Position::new(area.x + column, area.y)) else {
                continue;
            };
            let symbol = cell.symbol().to_string();
            // A cell holding nothing but the tail of a double-width glyph is
            // skipped by the backend, so wrapping it in a sequence would show
            // up as literal bytes.
            if symbol.is_empty() || symbol == " " {
                continue;
            }
            let Some(width) = NonZeroU16::new(cell.cell_width()) else {
                continue;
            };
            cell.set_symbol(&osc8(&hyperlink.destination, &symbol))
                .set_diff_option(CellDiffOption::ForcedWidth(width));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A link has to be tellable apart from any other underlined run — the
    /// footer's focused chips are underlined too, and in the accent. The hue
    /// is the difference; the underline is what survives colour loss.
    #[test]
    fn a_link_is_not_just_an_underlined_run() {
        let link = link_label_style();
        assert!(link.add_modifier.contains(Modifier::UNDERLINED));
        assert_eq!(link.fg, Some(theme::link_color()));
        assert_ne!(
            link.fg,
            Some(theme::text_primary_color()),
            "plain underlined body text must not read as a link"
        );
        assert_ne!(
            link.fg,
            Some(theme::accent_color()),
            "focus owns the accent; a link is not focus"
        );
    }

    #[test]
    fn only_http_and_https_become_clickable() {
        assert_eq!(
            destination_for("https://example.com/a?b=1#c"),
            Some("https://example.com/a?b=1#c".to_string())
        );
        assert_eq!(
            destination_for("http://example.com"),
            Some("http://example.com".to_string())
        );
        for rejected in [
            "mailto:someone@example.com",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "vscode://file/tmp/x.rs",
            "ssh://host/x",
            "data:text/html;base64,PHNjcmlwdD4=",
            "//example.com",
            "example.com",
        ] {
            assert_eq!(destination_for(rejected), None, "{rejected} was accepted");
        }
    }

    /// The whole point of the gate: a destination is bytes inside an escape
    /// sequence, so a control character would end it early and the remainder
    /// would be read as terminal commands.
    #[test]
    fn control_bytes_and_spaces_are_never_emitted() {
        assert_eq!(destination_for("https://example.com/\u{7}rm -rf /"), None);
        assert_eq!(destination_for("https://example.com/\u{1b}]2;x"), None);
        assert_eq!(destination_for("https://example.com/a b"), None);
        assert_eq!(destination_for("https://example.com/a\nb"), None);
        // Whitespace around the whole URL is trimmed, not treated as content.
        assert_eq!(
            destination_for("  https://example.com  "),
            Some("https://example.com".to_string())
        );
    }

    #[test]
    fn a_destination_without_a_host_is_not_a_link() {
        assert_eq!(destination_for("https://"), None);
        assert_eq!(destination_for("https:///path"), None);
        assert_eq!(destination_for("https://@/path"), None);
        assert_eq!(destination_for(""), None);
        // Userinfo still resolves to a host.
        assert_eq!(
            destination_for("https://user@example.com/x"),
            Some("https://user@example.com/x".to_string())
        );
    }

    #[test]
    fn a_bare_url_in_prose_is_found() {
        let text = "see https://example.com/docs for the API";
        let matches = autolink_matches(text);
        assert_eq!(matches.len(), 1, "{matches:?}");
        assert_eq!(&text[matches[0].0.clone()], "https://example.com/docs");
        assert_eq!(matches[0].1, "https://example.com/docs");
    }

    /// The full stop and the comma belong to the sentence, not the destination.
    #[test]
    fn a_bare_url_leaves_the_sentence_punctuation_behind() {
        let text = "read (https://example.com/a), then https://example.com/b.";
        let matches = autolink_matches(text);
        let found: Vec<&str> = matches
            .iter()
            .map(|(range, _)| &text[range.clone()])
            .collect();
        assert_eq!(found, ["https://example.com/a", "https://example.com/b"]);
    }

    /// A URL that opens its own bracket keeps it; one merely wrapped in
    /// parentheses does not take the closing one along.
    #[test]
    fn bracket_balance_decides_where_a_url_ends() {
        let text = "https://en.wikipedia.org/wiki/Foo_(bar)";
        let matches = autolink_matches(text);
        assert_eq!(&text[matches[0].0.clone()], text);
    }

    /// A scheme is only a URL where a word starts, and only with a host.
    #[test]
    fn mid_word_and_hostless_schemes_are_not_destinations() {
        assert!(autolink_matches("xhttps://example.com").is_empty());
        assert!(autolink_matches("ahttps://example.com b").is_empty());
        assert!(autolink_matches("https:// and http:// nothing").is_empty());
    }

    /// Quote marks wrap a URL without joining it.
    #[test]
    fn quotes_wrap_a_url_without_joining_it() {
        let text = "see \"https://example.com\" now";
        let matches = autolink_matches(text);
        assert_eq!(&text[matches[0].0.clone()], "https://example.com");
    }

    /// Detection never widens policy: every range came through the same gate,
    /// so a scheme the renderer refuses stays plain text here too.
    #[test]
    fn autolinking_is_gated_by_the_same_policy_as_markdown() {
        assert!(autolink_matches("https://").is_empty());
        assert!(autolink_matches("see file:///etc/passwd now").is_empty());
        assert!(autolink_matches("mailto:someone@example.com").is_empty());
        assert!(autolink_matches("javascript:alert(1)").is_empty());
    }

    #[test]
    fn an_oversized_destination_stays_plain_text() {
        let long = format!("https://example.com/{}", "a".repeat(MAX_DESTINATION_BYTES));
        assert_eq!(destination_for(&long), None);
    }

    #[test]
    fn the_sequence_wraps_the_text_and_closes_itself() {
        assert_eq!(
            osc8("https://example.com", "site"),
            "\x1b]8;;https://example.com\x07site\x1b]8;;\x07"
        );
    }

    #[test]
    fn only_known_terminals_are_offered_hyperlinks() {
        assert!(terminal_renders_hyperlinks(Some("ghostty"), None));
        assert!(terminal_renders_hyperlinks(Some("iTerm.app"), None));
        assert!(terminal_renders_hyperlinks(Some(" WezTerm "), None));
        // Alacritty renders hyperlinks even though it is not on the OSC 9 list.
        assert!(terminal_renders_hyperlinks(Some("alacritty"), None));
        // Terminal.app has never implemented OSC 8.
        assert!(!terminal_renders_hyperlinks(Some("Apple_Terminal"), None));
        assert!(!terminal_renders_hyperlinks(None, None));
        assert!(!terminal_renders_hyperlinks(
            Some("some-new-terminal"),
            None
        ));
    }

    /// `TERM_PROGRAM` names the outer terminal inside a multiplexer, so the
    /// allowlist cannot be trusted there.
    #[test]
    fn a_multiplexer_defeats_the_terminal_allowlist() {
        assert!(!terminal_renders_hyperlinks(Some("ghostty"), Some("1")));
        assert!(!terminal_renders_hyperlinks(Some("iterm.app"), Some("x")));
        assert!(terminal_renders_hyperlinks(Some("ghostty"), Some("")));
    }

    #[test]
    fn hyperlink_lines_behave_like_the_lines_they_wrap() {
        let mut line = HyperlinkLine::new(Line::from("hello"));
        assert_eq!(line.width(), 5);
        line.spans.clear();
        assert!(line.spans.is_empty());
        assert!(line.links.is_empty());
    }

    #[test]
    fn marking_covers_exactly_the_link_columns() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 1));
        buf.set_string(0, 0, "see site!", ratatui::style::Style::default());
        let hyperlinks = vec![TerminalHyperlink::new(4..8, "https://example.com")];
        mark_buffer_hyperlinks(&mut buf, Rect::new(0, 0, 10, 1), &hyperlinks);

        assert_eq!(
            buf.cell(Position::new(4, 0)).unwrap().symbol(),
            "\x1b]8;;https://example.com\x07s\x1b]8;;\x07"
        );
        assert_eq!(
            buf.cell(Position::new(7, 0)).unwrap().symbol(),
            "\x1b]8;;https://example.com\x07e\x1b]8;;\x07"
        );
        // The columns either side are untouched, so the escape covers the link
        // and nothing else.
        assert_eq!(buf.cell(Position::new(3, 0)).unwrap().symbol(), " ");
        assert_eq!(buf.cell(Position::new(8, 0)).unwrap().symbol(), "!");
    }

    #[test]
    fn hyperlink_diffs_preserve_unicode_and_repaint_plain_text() {
        use ratatui::backend::{Backend, CrosstermBackend};

        let area = Rect::new(0, 0, 30, 2);
        let mut previous = Buffer::empty(area);
        let mut terminal = vt100::Parser::new(area.height, area.width, 0);
        for (label, linked) in [
            ("界e\u{301} docs", true),
            ("界e\u{301} next", true),
            ("plain", false),
        ] {
            let text = format!("see {label}!");
            let mut next = Buffer::empty(area);
            next.set_string(0, 0, &text, ratatui::style::Style::default());
            next.set_string(0, 1, "next row", ratatui::style::Style::default());
            if linked {
                mark_buffer_hyperlinks(
                    &mut next,
                    area,
                    &[TerminalHyperlink::new(
                        4..4 + label.cell_width() as usize,
                        "https://example.com",
                    )],
                );
                assert_eq!(next[(4, 0)].cell_width(), 2);
                assert_eq!(next[(6, 0)].cell_width(), 1);
            }
            let mut output = Vec::new();
            CrosstermBackend::new(&mut output)
                .draw(previous.diff(&next).into_iter())
                .unwrap();
            terminal.process(&output);
            let contents = terminal
                .screen()
                .contents()
                .lines()
                .map(str::trim_end)
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(contents, format!("{text}\nnext row"));
            assert!(next.diff(&next).is_empty());
            previous = next;
        }
    }

    #[test]
    fn marking_stops_at_the_pane_edge() {
        // A link whose tail is clipped by the pane must not write past it.
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 1));
        buf.set_string(0, 0, "abcd", ratatui::style::Style::default());
        mark_buffer_hyperlinks(
            &mut buf,
            Rect::new(0, 0, 4, 1),
            &[TerminalHyperlink::new(2..9, "https://example.com")],
        );
        assert!(buf.cell(Position::new(2, 0)).unwrap().symbol().len() > 1);
        assert!(buf.cell(Position::new(3, 0)).unwrap().symbol().len() > 1);
        assert_eq!(buf.cell(Position::new(0, 0)).unwrap().symbol(), "a");
    }

    /// The writer is the last gate: a struct built directly, bypassing the
    /// policy, must still not reach the terminal as escape bytes.
    #[test]
    fn the_writer_refuses_a_destination_the_policy_would() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 8, 1));
        buf.set_string(0, 0, "clickme", ratatui::style::Style::default());
        mark_buffer_hyperlinks(
            &mut buf,
            Rect::new(0, 0, 8, 1),
            &[
                TerminalHyperlink::new(0..4, "https://example.com/\u{7}evil"),
                TerminalHyperlink::new(4..7, "mailto:someone@example.com"),
            ],
        );
        let written: String = (0..8)
            .map(|x| buf.cell(Position::new(x, 0)).unwrap().symbol())
            .collect();
        assert_eq!(written, "clickme ");
    }

    #[test]
    fn marking_offsets_the_pane_origin() {
        // Marking inside a pane must address the pane's own coordinates.
        let mut buf = Buffer::empty(Rect::new(0, 0, 12, 3));
        buf.set_string(5, 1, "go", ratatui::style::Style::default());
        mark_buffer_hyperlinks(
            &mut buf,
            Rect::new(5, 1, 7, 1),
            &[TerminalHyperlink::new(0..2, "https://example.com")],
        );
        assert_eq!(
            buf.cell(Position::new(5, 1)).unwrap().symbol(),
            "\x1b]8;;https://example.com\x07g\x1b]8;;\x07"
        );
        assert_eq!(buf.cell(Position::new(0, 0)).unwrap().symbol(), " ");
    }
}
