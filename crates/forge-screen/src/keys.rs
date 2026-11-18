//! Key chords (`ctrl+s`, `Return`, `alt+Tab`) as X keysyms.
//!
//! Keysyms are the X11 vocabulary; a macOS backend maps the same
//! [`KeyChord`] to virtual key codes.

/// A few well-known keysyms.
pub mod keysym {
    pub const RETURN: u32 = 0xff0d;
    pub const TAB: u32 = 0xff09;
    pub const ESCAPE: u32 = 0xff1b;
    pub const BACKSPACE: u32 = 0xff08;
    pub const DELETE: u32 = 0xffff;
    pub const HOME: u32 = 0xff50;
    pub const LEFT: u32 = 0xff51;
    pub const UP: u32 = 0xff52;
    pub const RIGHT: u32 = 0xff53;
    pub const DOWN: u32 = 0xff54;
    pub const PAGE_UP: u32 = 0xff55;
    pub const PAGE_DOWN: u32 = 0xff56;
    pub const END: u32 = 0xff57;
    pub const INSERT: u32 = 0xff63;
    pub const F1: u32 = 0xffbe;
    pub const SHIFT_L: u32 = 0xffe1;
    pub const CONTROL_L: u32 = 0xffe3;
    pub const ALT_L: u32 = 0xffe9;
    pub const SUPER_L: u32 = 0xffeb;
    pub const SPACE: u32 = 0x0020;
}

/// Modifiers held, then one key pressed and released.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyChord {
    /// Modifier keysyms, in the order they are pressed.
    pub modifiers: Vec<u32>,
    pub key: u32,
}

impl KeyChord {
    /// Parse `ctrl+shift+t`, `Return`, `alt+F4`, `super`, `a`, `é`.
    ///
    /// Modifier words: `ctrl`/`control`, `shift`, `alt`, `super`/`meta`/
    /// `cmd`/`win`. Key names are case-insensitive: `Return`/`Enter`, `Tab`,
    /// `Escape`/`Esc`, `BackSpace`, `Delete`, `Home`, `End`, `Page_Up`/
    /// `PageUp`, `Page_Down`/`PageDown`, `Up`, `Down`, `Left`, `Right`
    /// (also `ArrowUp`…), `Insert`, `space`, `F1`–`F12`. Any other single
    /// character is its own keysym. A lone modifier is a chord too.
    pub fn parse(chord: &str) -> Result<Self, String> {
        let parts: Vec<&str> = chord.split('+').map(str::trim).collect();
        let (key, modifier_words) = parts
            .split_last()
            .ok_or_else(|| format!("`{chord}` names no key"))?;
        if key.is_empty() {
            return Err(format!("`{chord}` names no key"));
        }
        let mut modifiers = Vec::new();
        for word in modifier_words {
            let sym = modifier_keysym(word)
                .ok_or_else(|| format!("`{word}` in `{chord}` is not a modifier"))?;
            if !modifiers.contains(&sym) {
                modifiers.push(sym);
            }
        }
        if let Some(sym) = modifier_keysym(key) {
            if !modifier_words.is_empty() {
                return Err(format!("`{chord}` names only modifiers"));
            }
            return Ok(Self {
                modifiers,
                key: sym,
            });
        }
        if let Some(sym) = named_keysym(key) {
            return Ok(Self {
                modifiers,
                key: sym,
            });
        }
        let mut chars = key.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => Ok(Self {
                modifiers,
                key: char_keysym(c),
            }),
            _ => Err(format!("`{key}` is not a key name or a single character")),
        }
    }
}

/// The keysym of a character: Latin-1 maps to itself, anything else to
/// the Unicode keysym range.
pub fn char_keysym(c: char) -> u32 {
    let code = c as u32;
    match code {
        0x20..=0x7e | 0xa0..=0xff => code,
        _ => 0x0100_0000 + code,
    }
}

fn modifier_keysym(word: &str) -> Option<u32> {
    match word.to_ascii_lowercase().as_str() {
        "ctrl" | "control" => Some(keysym::CONTROL_L),
        "shift" => Some(keysym::SHIFT_L),
        "alt" | "option" => Some(keysym::ALT_L),
        "super" | "meta" | "cmd" | "command" | "win" => Some(keysym::SUPER_L),
        _ => None,
    }
}

fn named_keysym(name: &str) -> Option<u32> {
    use keysym::*;
    let lower = name.to_ascii_lowercase();
    let sym = match lower.as_str() {
        "return" | "enter" => RETURN,
        "tab" => TAB,
        "escape" | "esc" => ESCAPE,
        "backspace" => BACKSPACE,
        "delete" | "del" => DELETE,
        "home" => HOME,
        "end" => END,
        "page_up" | "pageup" | "prior" => PAGE_UP,
        "page_down" | "pagedown" | "next" => PAGE_DOWN,
        "up" | "arrowup" => UP,
        "down" | "arrowdown" => DOWN,
        "left" | "arrowleft" => LEFT,
        "right" | "arrowright" => RIGHT,
        "insert" => INSERT,
        "space" => SPACE,
        _ => {
            let number: u32 = lower.strip_prefix('f')?.parse().ok()?;
            if !(1..=12).contains(&number) {
                return None;
            }
            F1 + number - 1
        }
    };
    Some(sym)
}

#[cfg(test)]
mod tests {
    use super::keysym::*;
    use super::*;

    #[test]
    fn named_keys_and_modifiers() {
        assert_eq!(
            KeyChord::parse("ctrl+s").unwrap(),
            KeyChord {
                modifiers: vec![CONTROL_L],
                key: 's' as u32
            }
        );
        assert_eq!(
            KeyChord::parse("Control+Shift+T").unwrap(),
            KeyChord {
                modifiers: vec![CONTROL_L, SHIFT_L],
                key: 'T' as u32
            }
        );
        assert_eq!(KeyChord::parse("alt+F4").unwrap().key, F1 + 3);
        assert_eq!(KeyChord::parse("alt+F4").unwrap().modifiers, vec![ALT_L]);
        assert_eq!(KeyChord::parse("cmd+c").unwrap().modifiers, vec![SUPER_L]);
        for (name, sym) in [
            ("Return", RETURN),
            ("enter", RETURN),
            ("Tab", TAB),
            ("esc", ESCAPE),
            ("BackSpace", BACKSPACE),
            ("Delete", DELETE),
            ("PageDown", PAGE_DOWN),
            ("page_up", PAGE_UP),
            ("ArrowLeft", LEFT),
            ("Down", DOWN),
            ("Home", HOME),
            ("End", END),
            ("space", SPACE),
            ("F12", F1 + 11),
        ] {
            assert_eq!(KeyChord::parse(name).unwrap().key, sym, "{name}");
        }
    }

    #[test]
    fn a_lone_modifier_is_pressed_as_the_key() {
        assert_eq!(
            KeyChord::parse("super").unwrap(),
            KeyChord {
                modifiers: vec![],
                key: SUPER_L
            }
        );
    }

    #[test]
    fn characters_map_to_keysyms() {
        assert_eq!(char_keysym('a'), 0x61);
        assert_eq!(char_keysym('é'), 0xe9, "Latin-1 is its own keysym");
        assert_eq!(
            char_keysym('€'),
            0x0100_20ac,
            "beyond Latin-1: the Unicode range"
        );
        assert_eq!(KeyChord::parse("é").unwrap().key, 0xe9);
    }

    #[test]
    fn nonsense_is_refused() {
        for chord in ["", "+", "ctrl+", "hyper+a", "ctrl+ab", "F13", "ctrl++"] {
            assert!(KeyChord::parse(chord).is_err(), "{chord:?}");
        }
    }
}
