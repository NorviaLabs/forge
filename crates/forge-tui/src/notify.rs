//! Out-of-band notification for background work.
//!
//! Forge has no way to reach an operator who is looking at another window. This
//! is that path: when a background task blocks on approval or stops, and the
//! terminal does not have focus, a notification goes out.
//!
//! Two channels, and they are not equivalents:
//!
//! - **OSC 9** (`ESC ] 9 ; text BEL`) asks the terminal to raise a desktop
//!   notification *carrying the text*. Ghostty, Kitty, iTerm2 and WezTerm
//!   forward it to the OS notification centre; several of them do so over SSH.
//!   Everywhere else it is ignored.
//! - **BEL** rings the bell. Universal, and says nothing — no text, and many
//!   people have it silenced.
//!
//! Because neither is detectable by probing, the mode and the terminal's
//! identity decide (see [`NotifyMode`]), and the whole thing is gated on focus:
//! notifying someone who is already looking at Forge is noise.
//!
//! State lives in thread-locals rather than on `TuiApp`, mirroring
//! [`crate::theme`]. The alternative — another field on the runtime config —
//! would have touched every one of its call sites to carry a value that is read
//! in exactly one place.

use std::cell::{Cell, RefCell};

use forge_config::NotifyMode;

/// Terminals whose `TERM_PROGRAM` is known to render an OSC 9 notification.
///
/// Matched case-insensitively. Deliberately short: a wrong entry means a
/// notification that silently does nothing, which is the failure this list
/// exists to avoid.
pub const OSC9_TERMINALS: &[&str] = &["ghostty", "kitty", "iterm.app", "wezterm", "warpterminal"];

/// Longest notification text sent to the terminal, in characters. A terminal
/// notification is a glance, not a transcript.
const MAX_TEXT_CHARS: usize = 120;

/// Which channels a mode resolves to for a given terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Channels {
    pub osc9: bool,
    pub bell: bool,
}

impl Channels {
    pub fn any(self) -> bool {
        self.osc9 || self.bell
    }
}

/// Resolve a mode against the terminal's identity.
///
/// `term_program` is `TERM_PROGRAM`, which terminals that support OSC 9
/// generally set. `None` means "unknown", which `auto` treats as unsupported.
pub fn channels_for(mode: NotifyMode, term_program: Option<&str>) -> Channels {
    let known = term_program.is_some_and(terminal_supports_osc9);
    match mode {
        NotifyMode::Off => Channels::default(),
        NotifyMode::Bell => Channels {
            osc9: false,
            bell: true,
        },
        NotifyMode::Osc9 => Channels {
            osc9: true,
            bell: false,
        },
        NotifyMode::Both => Channels {
            osc9: true,
            bell: true,
        },
        // Silence — not the bell — when the terminal is unknown.
        NotifyMode::Auto => Channels {
            osc9: known,
            bell: false,
        },
    }
}

/// Whether a terminal identity is on the known-good list.
pub fn terminal_supports_osc9(term_program: &str) -> bool {
    let probe = term_program.trim().to_ascii_lowercase();
    OSC9_TERMINALS.iter().any(|known| *known == probe)
}

/// Make a message safe to embed in an escape sequence.
///
/// This is not cosmetic. The text comes from task labels, which come from the
/// model, and an embedded `ESC` or `BEL` would terminate the sequence early and
/// leave the remainder to be interpreted as terminal commands. Control
/// characters are dropped and runs of whitespace collapsed, so a notification
/// can never do more than display.
pub fn sanitize(message: &str) -> String {
    let cleaned: String = message
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_TEXT_CHARS {
        return collapsed;
    }
    let kept: String = collapsed.chars().take(MAX_TEXT_CHARS - 1).collect();
    format!("{kept}…")
}

/// The OSC 9 sequence for a desktop notification.
pub fn osc9_sequence(message: &str) -> String {
    format!("\x1b]9;{}\x07", sanitize(message))
}

/// The bell.
pub const BELL: &str = "\x07";

thread_local! {
    static MODE: Cell<NotifyMode> = const { Cell::new(NotifyMode::Auto) };
    /// Focused until a terminal says otherwise. A terminal that never reports
    /// focus changes therefore never notifies, which is the safe direction:
    /// silence beats an alert on every turn.
    static FOCUSED: Cell<bool> = const { Cell::new(true) };
    static CAPTURED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// Install the configured mode (call once at startup).
pub fn install(mode: NotifyMode) {
    MODE.with(|slot| slot.set(mode));
}

pub fn mode() -> NotifyMode {
    MODE.with(Cell::get)
}

/// Record whether the terminal window has focus.
pub fn set_focused(focused: bool) {
    FOCUSED.with(|slot| slot.set(focused));
}

pub fn is_focused() -> bool {
    FOCUSED.with(Cell::get)
}

/// The terminal's self-reported identity, when it offers one.
pub fn term_program() -> Option<String> {
    std::env::var("TERM_PROGRAM").ok()
}

/// Notify, if the operator is away and the terminal can show it.
///
/// Returns whether anything was emitted, which is what the tests assert on —
/// the alternative is asserting on bytes written to a terminal nobody is
/// reading.
pub fn notify(message: &str) -> bool {
    if is_focused() {
        return false;
    }
    let channels = channels_for(mode(), term_program().as_deref());
    if !channels.any() {
        return false;
    }
    let text = sanitize(message);
    if text.is_empty() {
        return false;
    }
    CAPTURED.with(|captured| captured.borrow_mut().push(text.clone()));
    emit(channels, &text);
    true
}

/// Write to the real terminal.
#[cfg(not(test))]
fn emit(channels: Channels, text: &str) {
    use std::io::Write;
    let mut out = std::io::stdout();
    if channels.osc9 {
        let _ = out.write_all(osc9_sequence(text).as_bytes());
    }
    if channels.bell {
        let _ = out.write_all(BELL.as_bytes());
    }
    let _ = out.flush();
}

/// Tests read [`captured`] instead; writing to stdout would corrupt the test
/// harness's own output.
#[cfg(test)]
fn emit(_channels: Channels, _text: &str) {}

/// Everything emitted on this thread since the last [`take_captured`].
#[cfg(test)]
pub fn captured() -> Vec<String> {
    CAPTURED.with(|captured| captured.borrow().clone())
}

#[cfg(test)]
pub fn take_captured() -> Vec<String> {
    CAPTURED.with(|captured| std::mem::take(&mut *captured.borrow_mut()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `auto` is the whole reason the allowlist exists: it has to be silent
    /// rather than bell on a terminal nobody has verified.
    #[test]
    fn auto_notifies_only_on_a_known_terminal() {
        assert_eq!(
            channels_for(NotifyMode::Auto, Some("ghostty")),
            Channels {
                osc9: true,
                bell: false
            }
        );
        assert_eq!(
            channels_for(NotifyMode::Auto, Some("Apple_Terminal")),
            Channels::default(),
            "an unknown terminal gets silence, not a bell"
        );
        assert_eq!(
            channels_for(NotifyMode::Auto, None),
            Channels::default(),
            "no TERM_PROGRAM is not evidence of support"
        );
    }

    #[test]
    fn terminal_matching_ignores_case_and_padding() {
        assert!(terminal_supports_osc9("  Ghostty "));
        assert!(terminal_supports_osc9("iTerm.app"));
        assert!(!terminal_supports_osc9("ghostty-tmux"));
    }

    #[test]
    fn explicit_modes_do_not_depend_on_the_terminal() {
        for term in [None, Some("Apple_Terminal"), Some("ghostty")] {
            assert!(channels_for(NotifyMode::Bell, term).bell);
            assert!(channels_for(NotifyMode::Osc9, term).osc9);
            let both = channels_for(NotifyMode::Both, term);
            assert!(both.osc9 && both.bell);
            assert!(!channels_for(NotifyMode::Off, term).any());
        }
    }

    #[test]
    fn every_mode_round_trips_through_its_config_string() {
        for (text, mode) in [
            ("auto", NotifyMode::Auto),
            ("bell", NotifyMode::Bell),
            ("osc9", NotifyMode::Osc9),
            ("both", NotifyMode::Both),
            ("off", NotifyMode::Off),
        ] {
            assert_eq!(NotifyMode::parse(text).unwrap(), mode);
        }
        assert!(NotifyMode::parse("loud").is_err());
    }

    /// Control characters in a label must not escape into the terminal.
    #[test]
    fn sanitize_strips_control_characters() {
        let hostile = "explore\u{1b}]52;c;evil\u{07}done";
        let safe = sanitize(hostile);
        assert!(!safe.contains('\u{1b}'), "escape survived: {safe:?}");
        assert!(!safe.contains('\u{07}'), "bell survived: {safe:?}");
        assert_eq!(safe, "explore ]52;c;evil done");
    }

    #[test]
    fn sanitize_collapses_whitespace_and_truncates() {
        assert_eq!(sanitize("a \n\t  b"), "a b");
        let long = "x".repeat(MAX_TEXT_CHARS * 2);
        let out = sanitize(&long);
        assert_eq!(out.chars().count(), MAX_TEXT_CHARS);
        assert!(out.ends_with('…'));
    }

    /// A notification with nothing to say is not worth a bell.
    #[test]
    fn an_empty_message_notifies_nobody() {
        assert_eq!(sanitize("   "), "");
        assert!(osc9_sequence("").contains("]9;"));
    }

    #[test]
    fn the_osc9_sequence_is_well_formed() {
        assert_eq!(
            osc9_sequence("explore needs approval"),
            "\x1b]9;explore needs approval\x07"
        );
    }

    /// Notifying the operator who is looking at Forge is the noise this whole
    /// feature is supposed to avoid.
    #[test]
    fn a_focused_terminal_is_never_notified() {
        let _ = take_captured();
        install(NotifyMode::Bell);
        set_focused(true);
        assert!(!notify("should not appear"));
        assert!(captured().is_empty());

        set_focused(false);
        assert!(notify("should appear"));
        assert_eq!(captured(), vec!["should appear".to_string()]);

        // Leave the thread-local as the rest of the suite expects it.
        set_focused(true);
        install(NotifyMode::Auto);
    }

    #[test]
    fn an_away_operator_is_notified_only_when_a_channel_exists() {
        let _ = take_captured();
        set_focused(false);

        // `auto`'s resolution is asserted against `channels_for` above, which
        // does not depend on the terminal this test happens to run in; here
        // only the mode gate is under test.
        install(NotifyMode::Off);
        assert!(!notify("off means off"));
        assert!(captured().is_empty());

        install(NotifyMode::Osc9);
        assert!(
            notify("an explicit channel always emits"),
            "explicit mode must emit"
        );
        assert_eq!(captured().len(), 1);

        set_focused(true);
        install(NotifyMode::Auto);
    }
}
