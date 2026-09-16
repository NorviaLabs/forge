//! Read an image from the local OS clipboard (not OSC 52 / bracketed paste).
//!
//! Every platform runs an OS helper — `osascript`, `wl-paste` / `xclip`,
//! `powershell` — and hands it the scratch file to fill: `scratch_dir` on
//! macOS, a Rust-chosen name inside `%TEMP%` on Windows. Naming that file here,
//! rather than letting the helper choose a location such as AppleScript's
//! `temporary items folder` — which an unprivileged child process can be
//! refused with `-54` — keeps the write somewhere Forge can vouch for, keeps
//! concurrent pastes (and concurrent Forge instances) off each other's files,
//! and leaves nothing behind on the way out.

use std::path::Path;
use std::process::Command;

/// The one message for a clipboard that genuinely holds no image.
const CLIPBOARD_NOT_AN_IMAGE: &str = "clipboard is not an image";

/// Raised by the macOS reader when no image flavor is on the clipboard, so
/// "there is no image" stays distinct from "reading it failed".
#[cfg(any(target_os = "macos", target_os = "windows"))]
const NO_IMAGE_MARKER: &str = "FORGE_CLIPBOARD_NO_IMAGE";

/// PNG/JPEG/GIF/WebP bytes from the machine running Forge.
///
/// `scratch_dir` hosts the reader's transient file, which is removed before
/// this returns. Failures carry the reader's own words: a refused write or a
/// denied Automation grant reads as itself instead of as an empty clipboard.
pub fn read_os_clipboard_image(scratch_dir: &Path) -> Result<Vec<u8>, String> {
    #[cfg(target_os = "macos")]
    {
        read_macos(scratch_dir)
    }
    #[cfg(target_os = "linux")]
    {
        let _ = scratch_dir;
        read_linux()
    }
    #[cfg(target_os = "windows")]
    {
        read_windows(scratch_dir)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = scratch_dir;
        Err("clipboard image paste is not supported on this platform".into())
    }
}

/// A scratch file name unique to this process *and* this call, so two Forge
/// instances — or two pastes in a row — never share one spill file.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn scratch_name(extension: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    format!(
        "forge-clipboard-{}-{stamp}-{sequence}.{extension}",
        std::process::id()
    )
}

/// Fold a failed reader into one feedback line. The no-image marker reads as
/// the ordinary empty-clipboard message; every other failure keeps its own
/// reason, because a masked `-54` or TCC denial is what makes an image paste
/// impossible to diagnose.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn reader_failure(stderr: &str) -> String {
    if stderr.contains(NO_IMAGE_MARKER) {
        return CLIPBOARD_NOT_AN_IMAGE.to_string();
    }
    let detail = stderr
        .lines()
        .find_map(|line| line.split_once("execution error: ").map(|(_, rest)| rest))
        .or_else(|| stderr.lines().find(|line| !line.trim().is_empty()))
        .map(str::trim)
        .filter(|detail| !detail.is_empty())
        .unwrap_or("unknown error");
    format!("clipboard image read failed · {detail}")
}

/// Tries the clipboard's own image flavors, in the order a model attachment
/// wants them. `PNGf` and `JPEG` are already model-legal; `TIFF` is the flavor
/// several macOS apps publish alone, and is converted after it lands.
#[cfg(target_os = "macos")]
const MACOS_SCRIPT: &str = r#"on run argv
	set payload to missing value
	set flavor to ""
	try
		set payload to the clipboard as «class PNGf»
		set flavor to "png"
	end try
	if payload is missing value then
		try
			set payload to the clipboard as «class JPEG»
			set flavor to "jpeg"
		end try
	end if
	if payload is missing value then
		try
			set payload to the clipboard as «class TIFF»
			set flavor to "tiff"
		end try
	end if
	if payload is missing value then error "FORGE_CLIPBOARD_NO_IMAGE"
	set out to open for access (POSIX file (item 1 of argv)) with write permission
	set eof out to 0
	write payload to out
	close access out
	return flavor
end run"#;

#[cfg(target_os = "macos")]
fn read_macos(scratch_dir: &Path) -> Result<Vec<u8>, String> {
    let raw = scratch_dir.join(scratch_name("dat"));
    let converted = raw.with_extension("png");
    let result = run_macos_reader(&raw).and_then(|flavor| {
        let source = if flavor == "tiff" {
            convert_tiff(&raw, &converted)?;
            converted.as_path()
        } else {
            raw.as_path()
        };
        std::fs::read(source).map_err(|err| format!("clipboard image read failed · {err}"))
    });
    let _ = std::fs::remove_file(&raw);
    let _ = std::fs::remove_file(&converted);
    result
}

#[cfg(target_os = "macos")]
fn run_macos_reader(dest: &Path) -> Result<String, String> {
    let output = Command::new("osascript")
        .args(["-e", MACOS_SCRIPT])
        .arg(dest)
        .output()
        .map_err(|err| format!("osascript: {err}"))?;
    if !output.status.success() {
        return Err(reader_failure(&String::from_utf8_lossy(&output.stderr)));
    }
    let flavor = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if flavor.is_empty() {
        return Err(CLIPBOARD_NOT_AN_IMAGE.to_string());
    }
    Ok(flavor)
}

/// `sips` ships with macOS and reads the TIFF by content, not by extension, so
/// the raw spill keeps its `dat` name.
#[cfg(target_os = "macos")]
fn convert_tiff(raw: &Path, png: &Path) -> Result<(), String> {
    let output = Command::new("sips")
        .args(["-s", "format", "png"])
        .arg(raw)
        .arg("--out")
        .arg(png)
        .output()
        .map_err(|err| format!("sips: {err}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(reader_failure(&String::from_utf8_lossy(&output.stderr)))
    }
}

#[cfg(target_os = "linux")]
fn read_linux() -> Result<Vec<u8>, String> {
    let mut helper_installed = false;
    for (cmd, args) in [
        ("wl-paste", &["--type", "image/png"][..]),
        ("wl-paste", &["--type", "image/jpeg"][..]),
        (
            "xclip",
            &["-selection", "clipboard", "-t", "image/png", "-o"][..],
        ),
        (
            "xclip",
            &["-selection", "clipboard", "-t", "image/jpeg", "-o"][..],
        ),
    ] {
        if let Ok(output) = Command::new(cmd).args(args).output() {
            helper_installed = true;
            if output.status.success() && !output.stdout.is_empty() {
                return Ok(output.stdout);
            }
        }
    }
    if helper_installed {
        Err(CLIPBOARD_NOT_AN_IMAGE.to_string())
    } else {
        Err("no clipboard helper found · install wl-clipboard or xclip".into())
    }
}

/// The scratch name is substituted rather than passed as an argument: `$args`
/// binding under `-Command` is PowerShell-version dependent, and the name is
/// `[a-z0-9.-]` only, so it needs no quoting.
#[cfg(target_os = "windows")]
const WINDOWS_SCRIPT: &str = r#"
Add-Type -AssemblyName System.Windows.Forms
$img = [System.Windows.Forms.Clipboard]::GetImage()
if ($img -eq $null) { exit 2 }
$tmp = Join-Path $env:TEMP 'FORGE_CLIPBOARD_SCRATCH'
$img.Save($tmp, [System.Drawing.Imaging.ImageFormat]::Png)
Write-Output $tmp
"#;

#[cfg(target_os = "windows")]
fn read_windows(_scratch_dir: &Path) -> Result<Vec<u8>, String> {
    let script = WINDOWS_SCRIPT.replace("FORGE_CLIPBOARD_SCRATCH", &scratch_name("png"));
    let output = Command::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .output()
        .map_err(|err| format!("powershell: {err}"))?;
    if output.status.code() == Some(2) {
        return Err(CLIPBOARD_NOT_AN_IMAGE.to_string());
    }
    if !output.status.success() {
        return Err(reader_failure(&String::from_utf8_lossy(&output.stderr)));
    }
    // The script's own artifact, which the old fixed `$env:TEMP` name made a
    // shared file: read it back, then retire it.
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let bytes = std::fs::read(&path).map_err(|err| format!("clipboard image read failed · {err}"));
    let _ = std::fs::remove_file(&path);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_clipboard_helper_does_not_panic() {
        // Best-effort: CI machines usually have no image on the clipboard.
        let _ = read_os_clipboard_image(&std::env::temp_dir());
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn a_missing_flavor_reads_as_an_empty_clipboard() {
        let stderr = format!("6:22: execution error: {NO_IMAGE_MARKER} (-2700)\n");
        assert_eq!(reader_failure(&stderr), CLIPBOARD_NOT_AN_IMAGE);
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn any_other_reader_failure_keeps_its_own_reason() {
        // A refused temp write used to report "clipboard is not an image",
        // which was the one thing it was not.
        let stderr = "351:405: execution error: File permission error. (-54)\n";
        assert_eq!(
            reader_failure(stderr),
            "clipboard image read failed · File permission error. (-54)"
        );
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn scratch_names_are_unique_and_keep_their_extension() {
        let names: std::collections::HashSet<String> =
            (0..8).map(|_| scratch_name("dat")).collect();
        assert_eq!(names.len(), 8);
        for name in names {
            assert!(name.starts_with("forge-clipboard-"), "{name}");
            assert!(name.ends_with(".dat"), "{name}");
        }
    }
}
