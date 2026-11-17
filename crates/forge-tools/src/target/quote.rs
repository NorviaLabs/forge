//! POSIX shell quoting: the one way any string reaches a remote shell.
//!
//! Every command Forge sends over SSH is text a remote `sh` parses, so every
//! path, pattern and argument inside it goes through [`quote`]. Single quotes
//! make everything literal except a single quote itself, which is closed,
//! escaped and reopened: `it's` becomes `'it'\''s'`.

/// `text` as one literal POSIX shell word.
pub fn quote(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('\'');
    for c in text.chars() {
        if c == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(c);
        }
    }
    quoted.push('\'');
    quoted
}

/// `argv` as a command line of literal words.
pub fn quote_argv<S: AsRef<str>>(argv: &[S]) -> String {
    argv.iter()
        .map(|word| quote(word.as_ref()))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a real `sh` makes of `word`: it must be exactly the original.
    fn through_sh(word: &str) -> String {
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("printf %s {}", quote(word)))
            .output()
            .expect("sh runs");
        assert!(out.status.success(), "sh failed on {word:?}");
        String::from_utf8(out.stdout).expect("utf-8")
    }

    const HOSTILE: &[&str] = &[
        "",
        "plain",
        "two words",
        "it's",
        "'",
        "''",
        "\"double\"",
        "$(touch /tmp/forge-quote-pwned)",
        "`touch /tmp/forge-quote-pwned`",
        "${HOME}",
        "*",
        "~",
        "-rf",
        "--",
        "a\nb",
        "tab\there",
        "back\\slash",
        ";|&<>(){}[]!#",
        "ünïcödé ✓",
        "trailing\\",
    ];

    #[test]
    fn every_string_survives_a_real_shell_unchanged() {
        for word in HOSTILE {
            assert_eq!(&through_sh(word), word, "{word:?}");
        }
        assert!(
            !std::path::Path::new("/tmp/forge-quote-pwned").exists(),
            "nothing in a quoted word ever runs"
        );
    }

    #[test]
    fn quoting_is_single_quotes_with_escaped_single_quotes() {
        assert_eq!(quote("plain"), "'plain'");
        assert_eq!(quote(""), "''");
        assert_eq!(quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn an_argv_keeps_its_word_boundaries() {
        let argv = ["printf", "[%s]", "two words", "it's", "-x", ""];
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(quote_argv(&argv))
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8(out.stdout).unwrap(),
            "[two words][it's][-x][]"
        );
    }

    #[test]
    fn random_strings_round_trip() {
        // A deterministic generator: no new dependency, same cases each run.
        let mut seed: u64 = 0x5eed_f00d;
        let alphabet: Vec<char> = " \t\n'\"\\$`!*?~-;|&<>(){}[]#%^=+,.:/abcXYZ019é✓"
            .chars()
            .collect();
        for _ in 0..200 {
            let mut word = String::new();
            for _ in 0..(seed % 24) {
                seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                word.push(alphabet[(seed >> 33) as usize % alphabet.len()]);
            }
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            assert_eq!(through_sh(&word), word, "{word:?}");
        }
    }
}
