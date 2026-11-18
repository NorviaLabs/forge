//! `[browser]`, `[desktop]` and `[web_check]`: the computer-use features.
//!
//! Browser and desktop control act outside the OS sandbox on the user's
//! behalf, so both are off by default. Web check runs the project's app in
//! the sandbox and reaches only that app, so it is on by default. All three
//! are honoured only from the user's own config (or a file named with
//! `--config`); a project `forge.toml` cannot change them. The desktop app
//! writes these sections with [`write_computer_use_settings`].

use std::path::Path;

use serde::{Deserialize, Serialize};

/// `[browser]`: drive an installed Chromium-family browser.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserConfig {
    #[serde(default)]
    pub enabled: bool,
    /// The browser program. Detected when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<String>,
    /// Run without a window. Off: the user can watch the agent's browser.
    #[serde(default)]
    pub headless: bool,
}

/// `[desktop]`: see the screen and drive the real mouse and keyboard.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopConfig {
    #[serde(default)]
    pub enabled: bool,
}

/// `[web_check]`: run the project's web app in the sandbox and check its
/// pages in a headless browser that reaches only that app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebCheckConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for WebCheckConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

fn default_true() -> bool {
    true
}

/// Both sections, as the settings screen reads and writes them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComputerUseSettings {
    #[serde(default)]
    pub browser: BrowserConfig,
    #[serde(default)]
    pub desktop: DesktopConfig,
    #[serde(default)]
    pub web_check: WebCheckConfig,
}

/// Read the two sections from a config file. A missing file is the default
/// (both off).
pub fn read_computer_use_settings(path: &Path) -> std::io::Result<ComputerUseSettings> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Default::default()),
        Err(error) => return Err(error),
    };
    parse_settings(&text)
}

fn invalid(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.into())
}

fn parse_settings(text: &str) -> std::io::Result<ComputerUseSettings> {
    let table: toml::Table = toml::from_str(text).map_err(|error| invalid(error.to_string()))?;
    let browser = match table.get("browser").cloned() {
        Some(value) => value
            .try_into()
            .map_err(|error: toml::de::Error| invalid(format!("[browser]: {error}")))?,
        None => BrowserConfig::default(),
    };
    let desktop = match table.get("desktop").cloned() {
        Some(value) => value
            .try_into()
            .map_err(|error: toml::de::Error| invalid(format!("[desktop]: {error}")))?,
        None => DesktopConfig::default(),
    };
    let web_check = match table.get("web_check").cloned() {
        Some(value) => value
            .try_into()
            .map_err(|error: toml::de::Error| invalid(format!("[web_check]: {error}")))?,
        None => WebCheckConfig::default(),
    };
    Ok(ComputerUseSettings {
        browser,
        desktop,
        web_check,
    })
}

/// The `[name]` a header line opens, for a plain table or one of its
/// subtables (`[browser.x]`); `None` for any other line.
fn header_root(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let inner = trimmed.strip_prefix('[')?;
    // `[[array]]` headers are tables too, for this purpose.
    let inner = inner.strip_prefix('[').unwrap_or(inner);
    let end = inner.find(']')?;
    let name = inner[..end].trim();
    let root = name
        .split('.')
        .next()
        .unwrap_or(name)
        .trim()
        .trim_matches('"');
    Some(root.to_string())
}

fn render(settings: &ComputerUseSettings) -> String {
    let string = |value: &str| toml::Value::String(value.to_string()).to_string();
    let mut out = String::from("[browser]\n");
    out.push_str(&format!("enabled = {}\n", settings.browser.enabled));
    if let Some(executable) = &settings.browser.executable {
        out.push_str(&format!("executable = {}\n", string(executable)));
    }
    out.push_str(&format!("headless = {}\n", settings.browser.headless));
    out.push_str("\n[desktop]\n");
    out.push_str(&format!("enabled = {}\n", settings.desktop.enabled));
    out.push_str("\n[web_check]\n");
    out.push_str(&format!("enabled = {}\n", settings.web_check.enabled));
    out
}

/// Replace the `[browser]`, `[desktop]` and `[web_check]` sections of `path`, keeping every
/// other line of the file as it was. Creates the file (and its directory)
/// when missing. The result is checked to parse before it is written.
pub fn write_computer_use_settings(
    path: &Path,
    settings: &ComputerUseSettings,
) -> std::io::Result<()> {
    let existing = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error),
    };
    // A file that does not parse is the user's to fix; rewriting it could
    // lose what they meant.
    toml::from_str::<toml::Table>(&existing)
        .map_err(|error| invalid(format!("{} does not parse: {error}", path.display())))?;

    let mut kept = String::new();
    let mut skipping = false;
    for line in existing.lines() {
        if let Some(root) = header_root(line) {
            skipping = matches!(root.as_str(), "browser" | "desktop" | "web_check");
        }
        if !skipping {
            kept.push_str(line);
            kept.push('\n');
        }
    }
    let mut text = kept.trim_end().to_string();
    if !text.is_empty() {
        text.push_str("\n\n");
    }
    text.push_str(&render(settings));

    // Dotted keys or inline tables elsewhere could still set these: only a
    // file that reads back as what was asked for is written.
    if parse_settings(&text)? != *settings {
        return Err(invalid(format!(
            "{} sets [browser], [desktop] or [web_check] in a form Forge cannot edit; change it by hand",
            path.display()
        )));
    }
    // A config kept elsewhere (dotfiles) is written where the link points,
    // so the link stays; the file keeps its permissions.
    let target = match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => std::fs::canonicalize(path)?,
        _ => path.to_path_buf(),
    };
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let permissions = std::fs::metadata(&target)
        .ok()
        .map(|meta| meta.permissions());
    let temp = target.with_extension("toml.forge-tmp");
    std::fs::write(&temp, &text)?;
    if let Some(permissions) = permissions {
        std::fs::set_permissions(&temp, permissions)?;
    }
    std::fs::rename(&temp, &target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn both_features_are_off_by_default() {
        let settings = ComputerUseSettings::default();
        assert!(!settings.browser.enabled);
        assert!(!settings.browser.headless);
        assert!(settings.browser.executable.is_none());
        assert!(!settings.desktop.enabled);
    }

    /// Web check stays inside the sandbox and reaches only the app: on
    /// unless the user turns it off.
    #[test]
    fn web_check_is_on_by_default() {
        assert!(WebCheckConfig::default().enabled);
        assert!(ComputerUseSettings::default().web_check.enabled);
    }

    #[test]
    fn a_file_without_web_check_reads_as_on() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[browser]\nenabled = true\n").unwrap();
        assert!(read_computer_use_settings(&path).unwrap().web_check.enabled);
    }

    /// Turning web check off is written, read back, and replaced (not
    /// repeated) by the next save.
    #[test]
    fn web_check_off_is_written_and_read_back() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut settings = ComputerUseSettings::default();
        settings.web_check.enabled = false;
        write_computer_use_settings(&path, &settings).unwrap();
        assert_eq!(read_computer_use_settings(&path).unwrap(), settings);
        settings.browser.enabled = true;
        write_computer_use_settings(&path, &settings).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("[web_check]").count(), 1, "{text}");
        assert_eq!(read_computer_use_settings(&path).unwrap(), settings);
    }

    #[test]
    fn a_missing_file_reads_as_off() {
        let dir = tempdir().unwrap();
        let settings = read_computer_use_settings(&dir.path().join("none.toml")).unwrap();
        assert_eq!(settings, ComputerUseSettings::default());
    }

    #[test]
    fn writing_creates_the_file_and_reads_back() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("forge").join("config.toml");
        let settings = ComputerUseSettings {
            browser: BrowserConfig {
                enabled: true,
                executable: Some("/usr/bin/chromium".into()),
                headless: true,
            },
            desktop: DesktopConfig { enabled: true },
            web_check: WebCheckConfig::default(),
        };
        write_computer_use_settings(&path, &settings).unwrap();
        assert_eq!(read_computer_use_settings(&path).unwrap(), settings);
    }

    /// The user's other settings, comments included, survive a save, and
    /// a second save replaces the sections rather than adding more.
    #[test]
    fn writing_keeps_the_rest_of_the_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "# my settings\n[model]\nrequest_timeout_secs = 30\n\n[browser]\nenabled = false\n# old note\n\n[subagents]\nmodel_selection = \"inherit\"\n",
        )
        .unwrap();
        let mut settings = ComputerUseSettings::default();
        settings.browser.enabled = true;
        write_computer_use_settings(&path, &settings).unwrap();
        settings.desktop.enabled = true;
        write_computer_use_settings(&path, &settings).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# my settings"));
        assert!(text.contains("request_timeout_secs = 30"));
        assert!(text.contains("model_selection = \"inherit\""));
        assert_eq!(text.matches("[browser]").count(), 1, "{text}");
        assert_eq!(text.matches("[desktop]").count(), 1, "{text}");
        assert_eq!(read_computer_use_settings(&path).unwrap(), settings);
        let parsed: toml::Table = toml::from_str(&text).unwrap();
        assert_eq!(
            parsed["model"]["request_timeout_secs"].as_integer(),
            Some(30)
        );
    }

    /// A `config.toml` kept elsewhere (dotfiles) stays a link to it, and a
    /// private file stays private.
    #[test]
    fn writing_follows_a_symlink_and_keeps_the_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let real = dir.path().join("dotfiles-config.toml");
        std::fs::write(&real, "[model]\nrequest_timeout_secs = 9\n").unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = dir.path().join("config.toml");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let mut settings = ComputerUseSettings::default();
        settings.desktop.enabled = true;
        write_computer_use_settings(&link, &settings).unwrap();

        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(read_computer_use_settings(&real).unwrap(), settings);
        let mode = std::fs::metadata(&real).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    /// A file that does not parse is left alone rather than overwritten.
    #[test]
    fn an_unparsable_file_is_not_overwritten() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[model\nbroken").unwrap();
        assert!(write_computer_use_settings(&path, &ComputerUseSettings::default()).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[model\nbroken");
    }
}
