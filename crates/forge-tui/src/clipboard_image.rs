//! Read an image from the local OS clipboard (not OSC 52 / bracketed paste).

use std::process::Command;

/// PNG/JPEG/GIF/WebP bytes from the machine running Forge.
pub fn read_os_clipboard_image() -> Result<Vec<u8>, String> {
    #[cfg(target_os = "macos")]
    {
        read_macos()
    }
    #[cfg(target_os = "linux")]
    {
        read_linux()
    }
    #[cfg(target_os = "windows")]
    {
        read_windows()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        Err("clipboard image paste is not supported on this platform".into())
    }
}

#[cfg(target_os = "macos")]
fn read_macos() -> Result<Vec<u8>, String> {
    let script = r#"
set png_data to missing value
try
    set png_data to the clipboard as «class PNGf»
end try
if png_data is missing value then
    try
        set png_data to the clipboard as «class JPEG»
    end try
end if
if png_data is missing value then error "no image"
set tmp to POSIX path of (path to temporary items folder) & "forge-clipboard.png"
set out to open for access (POSIX file tmp) with write permission
set eof out to 0
write png_data to out
close access out
return tmp
"#;
    let output = Command::new("osascript")
        .args(["-e", script])
        .output()
        .map_err(|err| format!("osascript: {err}"))?;
    if !output.status.success() {
        return Err("clipboard is not an image".into());
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        return Err("clipboard is not an image".into());
    }
    std::fs::read(&path).map_err(|err| format!("read clipboard temp: {err}"))
}

#[cfg(target_os = "linux")]
fn read_linux() -> Result<Vec<u8>, String> {
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
            if output.status.success() && !output.stdout.is_empty() {
                return Ok(output.stdout);
            }
        }
    }
    Err("clipboard is not an image".into())
}

#[cfg(target_os = "windows")]
fn read_windows() -> Result<Vec<u8>, String> {
    let script = r#"
Add-Type -AssemblyName System.Windows.Forms
$img = [System.Windows.Forms.Clipboard]::GetImage()
if ($img -eq $null) { exit 2 }
$tmp = Join-Path $env:TEMP 'forge-clipboard.png'
$img.Save($tmp, [System.Drawing.Imaging.ImageFormat]::Png)
Write-Output $tmp
"#;
    let output = Command::new("powershell")
        .args(["-NoProfile", "-Command", script])
        .output()
        .map_err(|err| format!("powershell: {err}"))?;
    if !output.status.success() {
        return Err("clipboard is not an image".into());
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    std::fs::read(&path).map_err(|err| format!("read clipboard temp: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_clipboard_helper_does_not_panic() {
        // Best-effort: CI machines usually have no image on the clipboard.
        let _ = read_os_clipboard_image();
    }
}
