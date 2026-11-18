//! Key chords (`Enter`, `ctrl+a`, `shift+Tab`) as DevTools key events.

/// DevTools modifier bits.
pub const ALT: u32 = 1;
pub const CTRL: u32 = 2;
pub const META: u32 = 4;
pub const SHIFT: u32 = 8;

/// One key press, as `Input.dispatchKeyEvent` wants it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyPress {
    /// `key` (`"Enter"`, `"a"`).
    pub key: String,
    /// `code` (`"Enter"`, `"KeyA"`).
    pub code: String,
    /// `windowsVirtualKeyCode`.
    pub key_code: u32,
    /// Text the key inserts, when it inserts any and no modifier other than
    /// Shift is held.
    pub text: Option<String>,
    pub modifiers: u32,
}

impl KeyPress {
    /// Whether this is a plain Enter, which submits the form its field is in.
    pub fn is_enter(&self) -> bool {
        self.key == "Enter" && self.modifiers == 0
    }
}

/// Parse a chord: modifiers (`ctrl`, `control`, `alt`, `shift`, `meta`,
/// `cmd`, `super`) joined with `+` to one key: a named key (`Enter`, `Tab`,
/// `Escape`, `Backspace`, `Delete`, `ArrowUp`…, `Home`, `End`, `PageUp`,
/// `PageDown`, `Space`, `F1`–`F12`) or a single character.
pub fn parse_chord(chord: &str) -> Result<KeyPress, String> {
    let parts: Vec<&str> = chord.split('+').map(str::trim).collect();
    let (key, modifier_words) = parts
        .split_last()
        .ok_or_else(|| format!("`{chord}` names no key"))?;
    let mut modifiers = 0;
    for word in modifier_words {
        modifiers |=
            modifier_bit(word).ok_or_else(|| format!("`{word}` in `{chord}` is not a modifier"))?;
    }
    if key.is_empty() {
        return Err(format!("`{chord}` names no key"));
    }
    if modifier_bit(key).is_some() && !modifier_words.is_empty() {
        return Err(format!("`{chord}` names only modifiers"));
    }
    let shift = modifiers & SHIFT != 0;
    let plain = modifiers & !SHIFT == 0;
    if let Some((name, code, key_code, text)) = named_key(key) {
        return Ok(KeyPress {
            key: name.to_string(),
            code: code.to_string(),
            key_code,
            text: text.filter(|_| plain).map(str::to_string),
            modifiers,
        });
    }
    let mut chars = key.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return Err(format!("`{key}` is not a key name or a single character"));
    };
    let (code, key_code) = character_code(c);
    let shown = if shift && c.is_ascii_lowercase() {
        c.to_ascii_uppercase()
    } else {
        c
    };
    Ok(KeyPress {
        key: shown.to_string(),
        code,
        key_code,
        text: plain.then(|| shown.to_string()),
        modifiers,
    })
}

fn modifier_bit(word: &str) -> Option<u32> {
    match word.to_ascii_lowercase().as_str() {
        "ctrl" | "control" => Some(CTRL),
        "alt" | "option" => Some(ALT),
        "shift" => Some(SHIFT),
        "meta" | "cmd" | "command" | "super" | "win" => Some(META),
        _ => None,
    }
}

/// `(key, code, keyCode, text)` of a named key.
fn named_key(name: &str) -> Option<(&'static str, &'static str, u32, Option<&'static str>)> {
    let lower = name.to_ascii_lowercase();
    let key = match lower.as_str() {
        "enter" | "return" => ("Enter", "Enter", 13, Some("\r")),
        "tab" => ("Tab", "Tab", 9, None),
        "escape" | "esc" => ("Escape", "Escape", 27, None),
        "backspace" => ("Backspace", "Backspace", 8, None),
        "delete" | "del" => ("Delete", "Delete", 46, None),
        "space" => (" ", "Space", 32, Some(" ")),
        "arrowup" | "up" => ("ArrowUp", "ArrowUp", 38, None),
        "arrowdown" | "down" => ("ArrowDown", "ArrowDown", 40, None),
        "arrowleft" | "left" => ("ArrowLeft", "ArrowLeft", 37, None),
        "arrowright" | "right" => ("ArrowRight", "ArrowRight", 39, None),
        "home" => ("Home", "Home", 36, None),
        "end" => ("End", "End", 35, None),
        "pageup" => ("PageUp", "PageUp", 33, None),
        "pagedown" => ("PageDown", "PageDown", 34, None),
        "insert" => ("Insert", "Insert", 45, None),
        _ => {
            let number: u32 = lower.strip_prefix('f')?.parse().ok()?;
            const F: [&str; 12] = [
                "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12",
            ];
            let name = F.get(number.checked_sub(1)? as usize)?;
            return Some((name, name, 111 + number, None));
        }
    };
    Some(key)
}

/// `code` and `keyCode` of a character key on a US layout.
fn character_code(c: char) -> (String, u32) {
    if c.is_ascii_alphabetic() {
        let upper = c.to_ascii_uppercase();
        return (format!("Key{upper}"), upper as u32);
    }
    if c.is_ascii_digit() {
        return (format!("Digit{c}"), c as u32);
    }
    let (code, key_code) = match c {
        '-' => ("Minus", 189),
        '=' => ("Equal", 187),
        '[' => ("BracketLeft", 219),
        ']' => ("BracketRight", 221),
        '\\' => ("Backslash", 220),
        ';' => ("Semicolon", 186),
        '\'' => ("Quote", 222),
        ',' => ("Comma", 188),
        '.' => ("Period", 190),
        '/' => ("Slash", 191),
        '`' => ("Backquote", 192),
        _ => ("", 0),
    };
    (code.to_string(), key_code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_inserts_a_carriage_return() {
        let enter = parse_chord("Enter").unwrap();
        assert_eq!(enter.key, "Enter");
        assert_eq!(enter.code, "Enter");
        assert_eq!(enter.key_code, 13);
        assert_eq!(enter.text.as_deref(), Some("\r"));
        assert_eq!(enter.modifiers, 0);
        assert!(enter.is_enter());
        assert!(
            parse_chord("return").unwrap().is_enter(),
            "names are case-insensitive"
        );
        assert!(!parse_chord("shift+Enter").unwrap().is_enter());
    }

    #[test]
    fn modifiers_combine_and_suppress_text() {
        let select_all = parse_chord("ctrl+a").unwrap();
        assert_eq!(select_all.key, "a");
        assert_eq!(select_all.code, "KeyA");
        assert_eq!(select_all.key_code, 65);
        assert_eq!(select_all.modifiers, CTRL);
        assert_eq!(select_all.text, None, "ctrl+a inserts nothing");

        let chord = parse_chord("Control+Shift+Alt+Meta+t").unwrap();
        assert_eq!(chord.modifiers, CTRL | SHIFT | ALT | META);
        assert_eq!(parse_chord("cmd+c").unwrap().modifiers, META);

        let capital = parse_chord("shift+a").unwrap();
        assert_eq!(capital.key, "A");
        assert_eq!(capital.text.as_deref(), Some("A"));
    }

    #[test]
    fn named_keys_have_their_codes() {
        for (name, key, code, key_code) in [
            ("Tab", "Tab", "Tab", 9),
            ("Escape", "Escape", "Escape", 27),
            ("esc", "Escape", "Escape", 27),
            ("Backspace", "Backspace", "Backspace", 8),
            ("Delete", "Delete", "Delete", 46),
            ("ArrowDown", "ArrowDown", "ArrowDown", 40),
            ("PageDown", "PageDown", "PageDown", 34),
            ("Home", "Home", "Home", 36),
            ("Space", " ", "Space", 32),
            ("F5", "F5", "F5", 116),
            ("1", "1", "Digit1", 49),
        ] {
            let press = parse_chord(name).unwrap();
            assert_eq!(
                (press.key.as_str(), press.code.as_str(), press.key_code),
                (key, code, key_code),
                "{name}"
            );
        }
    }

    #[test]
    fn nonsense_is_refused() {
        for chord in ["", "ctrl+", "hyper+a", "ctrl+ab", "Enterr", "ctrl+shift"] {
            assert!(parse_chord(chord).is_err(), "{chord:?}");
        }
    }
}
