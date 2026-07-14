//! Speech-to-text for the TUI input bar (no LLM agent loop).
//!
//! Push-to-talk: hold Ctrl+Space → ffmpeg capture → release → Whisper transcript into input.
//! Speed presets pick a smaller/faster local Whisper model when the CLI backend is used.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Local Whisper model preference (API Whisper is fixed at whisper-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SttSpeed {
    /// Prefer larger local model (`small`).
    Slow,
    #[default]
    /// Balanced local model (`base`).
    Normal,
    /// Fastest local model (`tiny`).
    Fast,
}

impl SttSpeed {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "slow" | "s" | "long" => Some(Self::Slow),
            "normal" | "n" | "default" | "med" | "medium" => Some(Self::Normal),
            "fast" | "f" | "quick" | "short" => Some(Self::Fast),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Slow => "slow",
            Self::Normal => "normal",
            Self::Fast => "fast",
        }
    }

    /// Local whisper model size preference (faster = smaller).
    pub fn whisper_cli_model(self) -> &'static str {
        match self {
            Self::Slow => "small",
            Self::Normal => "base",
            Self::Fast => "tiny",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SttSettings {
    pub speed: SttSpeed,
    /// OpenAI-compatible base for Whisper API (default api.openai.com).
    pub api_base: String,
}

impl Default for SttSettings {
    fn default() -> Self {
        let speed = std::env::var("FORGE_STT_SPEED")
            .ok()
            .and_then(|s| SttSpeed::parse(&s))
            .unwrap_or_default();
        Self {
            speed,
            api_base: std::env::var("OPENAI_API_BASE")
                .or_else(|_| std::env::var("FORGE_STT_API_BASE"))
                .unwrap_or_else(|_| "https://api.openai.com/v1".into()),
        }
    }
}

impl SttSettings {
    pub fn set_speed(&mut self, speed: SttSpeed) {
        self.speed = speed;
    }
}

/// Result of a listen session.
#[derive(Debug, Clone)]
pub struct Transcript {
    pub text: String,
    pub secs: u32,
    pub backend: String,
}

/// Push-to-talk: ffmpeg process running until [`LiveRecorder::stop`].
pub struct LiveRecorder {
    child: Option<Child>,
    path: Option<PathBuf>,
    started: Instant,
}

impl LiveRecorder {
    /// Start capturing the mic into a temp WAV (no fixed duration; hard cap 120s).
    pub fn start() -> Result<Self, String> {
        let path = temp_wav_path()?;
        let child = spawn_mic_ffmpeg(&path, /*max_secs*/ 120)?;
        Ok(Self {
            child: Some(child),
            path: Some(path),
            started: Instant::now(),
        })
    }

    pub fn elapsed_secs(&self) -> u32 {
        self.started.elapsed().as_secs().min(u64::from(u32::MAX)) as u32
    }

    /// Stop capture and return the WAV path + duration. Caller must transcribe/delete.
    pub fn stop(mut self) -> Result<(PathBuf, u32), String> {
        let secs = self.elapsed_secs().max(1);
        let mut child = self
            .child
            .take()
            .ok_or_else(|| "recorder already stopped".to_string())?;
        let path = self
            .path
            .take()
            .ok_or_else(|| "recorder path missing".to_string())?;

        // Prefer graceful quit so the WAV header is finalized.
        if let Some(mut sin) = child.stdin.take() {
            let _ = sin.write_all(b"q");
            let _ = sin.flush();
        }
        // Give ffmpeg a moment, then force-kill if needed.
        let deadline = Instant::now() + std::time::Duration::from_millis(800);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(40));
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
            }
        }
        if !path.exists() || fs::metadata(&path).map(|m| m.len()).unwrap_or(0) < 100 {
            let _ = fs::remove_file(&path);
            return Err(
                "Recording too short or empty — hold the PTT key longer and check mic permissions."
                    .into(),
            );
        }
        // Prevent Drop from deleting the handed-off path.
        Ok((path, secs))
    }
}

impl Drop for LiveRecorder {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(path) = self.path.take() {
            let _ = fs::remove_file(path);
        }
    }
}

/// Transcribe an existing WAV (after push-to-talk stop). Deletes the file when done.
pub fn transcribe_wav_file(
    wav: PathBuf,
    settings: &SttSettings,
    api_key: Option<&str>,
    secs: u32,
) -> Result<Transcript, String> {
    let result = transcribe_wav(&wav, settings, api_key, secs);
    let _ = fs::remove_file(&wav);
    result
}

fn transcribe_wav(
    wav: &Path,
    settings: &SttSettings,
    api_key: Option<&str>,
    secs: u32,
) -> Result<Transcript, String> {
    let (text, backend) = if let Some(key) = api_key.map(str::trim).filter(|k| !k.is_empty()) {
        match transcribe_openai(wav, key, &settings.api_base) {
            Ok(t) => (t, "openai-whisper".to_string()),
            Err(e) => {
                if which("whisper").is_some() {
                    let t = transcribe_local_whisper(wav, settings.speed.whisper_cli_model())?;
                    (
                        t,
                        format!(
                            "whisper-cli/{} (api failed: {e})",
                            settings.speed.whisper_cli_model()
                        ),
                    )
                } else {
                    return Err(format!(
                        "Whisper API failed ({e}). Set OPENAI_API_KEY or install `whisper` CLI."
                    ));
                }
            }
        }
    } else if which("whisper").is_some() {
        let t = transcribe_local_whisper(wav, settings.speed.whisper_cli_model())?;
        (
            t,
            format!("whisper-cli/{}", settings.speed.whisper_cli_model()),
        )
    } else {
        return Err(
            "Speech-to-text needs OPENAI_API_KEY (Whisper API) or a local `whisper` CLI.\n\
Run `forge connect openai --key …` or export OPENAI_API_KEY."
                .into(),
        );
    };
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("No speech detected (empty transcript).".into());
    }
    Ok(Transcript {
        text,
        secs,
        backend,
    })
}

fn temp_wav_path() -> Result<PathBuf, String> {
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let dir = std::env::temp_dir();
    Ok(dir.join(format!("forge-stt-{t}.wav")))
}

fn which(bin: &str) -> Option<PathBuf> {
    if let Ok(p) = std::env::var("PATH") {
        for d in p.split(':') {
            let cand = Path::new(d).join(bin);
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

/// Spawn ffmpeg writing mono 16 kHz WAV. Hard-caps at `max_secs` (safety).
/// Stdin is piped so we can send `q` to stop early (push-to-talk).
fn spawn_mic_ffmpeg(path: &Path, max_secs: u32) -> Result<Child, String> {
    let ffmpeg = which("ffmpeg").ok_or_else(|| {
        "ffmpeg not found. Install ffmpeg to capture microphone audio for dictation.".to_string()
    })?;

    let input = std::env::var("FORGE_STT_INPUT").unwrap_or_else(|_| default_ffmpeg_input());
    let max = max_secs.clamp(1, 120).to_string();
    let custom = std::env::var("FORGE_STT_INPUT").is_ok();

    let mut cmd = Command::new(ffmpeg);
    if cfg!(target_os = "macos") && !custom {
        cmd.args([
            "-y",
            "-f",
            "avfoundation",
            "-i",
            &input,
            "-t",
            &max,
            "-ar",
            "16000",
            "-ac",
            "1",
            "-c:a",
            "pcm_s16le",
        ]);
    } else if cfg!(target_os = "linux") && !custom {
        cmd.args([
            "-y",
            "-f",
            "pulse",
            "-i",
            &input,
            "-t",
            &max,
            "-ar",
            "16000",
            "-ac",
            "1",
            "-c:a",
            "pcm_s16le",
        ]);
    } else {
        cmd.args([
            "-y",
            "-i",
            &input,
            "-t",
            &max,
            "-ar",
            "16000",
            "-ac",
            "1",
            "-c:a",
            "pcm_s16le",
        ]);
    }
    cmd.arg(path);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    cmd.spawn()
        .map_err(|e| format!("failed to start ffmpeg: {e}"))
}

/// True when this key event is the push-to-talk binding (default: Ctrl+Space).
///
/// Override with `FORGE_STT_PTT=ctrl+space` (only binding today) or disable with `FORGE_STT_PTT=off`.
pub fn is_ptt_key(code: crossterm::event::KeyCode, mods: crossterm::event::KeyModifiers) -> bool {
    use crossterm::event::{KeyCode, KeyModifiers};
    let binding = std::env::var("FORGE_STT_PTT")
        .unwrap_or_else(|_| "ctrl+space".into())
        .to_ascii_lowercase();
    if binding == "off" || binding == "none" || binding == "disable" {
        return false;
    }
    // Default / documented: Ctrl+Space
    matches!(code, KeyCode::Char(' ')) && mods.contains(KeyModifiers::CONTROL)
}

fn default_ffmpeg_input() -> String {
    if cfg!(target_os = "macos") {
        // avfoundation: "video_device:audio_device" — none:0 = no video, first audio
        ":0".into()
    } else if cfg!(target_os = "linux") {
        "default".into()
    } else {
        "default".into()
    }
}

fn transcribe_openai(wav: &Path, api_key: &str, api_base: &str) -> Result<String, String> {
    // Use curl for multipart (portable; no extra crate).
    let curl = which("curl").ok_or_else(|| "curl not found (needed for Whisper API)".to_string())?;
    let base = api_base.trim().trim_end_matches('/');
    let url = format!("{base}/audio/transcriptions");
    let out = Command::new(curl)
        .args([
            "-sS",
            "-X",
            "POST",
            &url,
            "-H",
            &format!("Authorization: Bearer {api_key}"),
            "-F",
            &format!("file=@{};type=audio/wav", wav.display()),
            "-F",
            "model=whisper-1",
            "-F",
            "response_format=json",
        ])
        .output()
        .map_err(|e| format!("curl whisper: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let body = String::from_utf8_lossy(&out.stdout);
        return Err(format!(
            "Whisper API HTTP error: {} {}",
            err.trim(),
            body.chars().take(200).collect::<String>()
        ));
    }
    let body = String::from_utf8_lossy(&out.stdout);
    parse_whisper_json(&body)
}

fn parse_whisper_json(body: &str) -> Result<String, String> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("whisper JSON: {e} — {body}"))?;
    if let Some(t) = v.get("text").and_then(|t| t.as_str()) {
        return Ok(t.to_string());
    }
    if let Some(err) = v.get("error") {
        return Err(format!("whisper API: {err}"));
    }
    Err(format!("whisper response missing text: {body}"))
}

fn transcribe_local_whisper(wav: &Path, model: &str) -> Result<String, String> {
    let whisper = which("whisper").ok_or_else(|| "whisper CLI not found".to_string())?;
    let out_dir = wav.parent().unwrap_or_else(|| Path::new("/tmp"));
    let stem = wav
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("forge-stt");
    let out = Command::new(whisper)
        .args([
            wav.to_str().unwrap_or(""),
            "--model",
            model,
            "--output_format",
            "txt",
            "--output_dir",
            out_dir.to_str().unwrap_or("/tmp"),
            "--fp16",
            "False",
        ])
        .output()
        .map_err(|e| format!("whisper CLI: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "whisper CLI failed: {}",
            err.chars().take(300).collect::<String>()
        ));
    }
    let txt_path = out_dir.join(format!("{stem}.txt"));
    let text = fs::read_to_string(&txt_path).map_err(|e| format!("read transcript: {e}"))?;
    let _ = fs::remove_file(txt_path);
    Ok(text)
}

/// Resolve a Whisper API key: env, then connect store `openai` profile.
pub fn resolve_stt_api_key(store: &forge_connect::CredentialStore) -> Option<String> {
    if let Ok(k) = std::env::var("OPENAI_API_KEY") {
        if !k.trim().is_empty() {
            return Some(k);
        }
    }
    if let Ok(k) = std::env::var("FORGE_STT_API_KEY") {
        if !k.trim().is_empty() {
            return Some(k);
        }
    }
    store.get_api_key("openai").ok().flatten().filter(|k| !k.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_parse_and_models() {
        assert_eq!(SttSpeed::parse("fast"), Some(SttSpeed::Fast));
        assert_eq!(SttSpeed::Fast.whisper_cli_model(), "tiny");
        assert_eq!(SttSpeed::Slow.whisper_cli_model(), "small");
    }

    #[test]
    fn parse_whisper_json_text() {
        let t = parse_whisper_json(r#"{"text":"hello world"}"#).unwrap();
        assert_eq!(t, "hello world");
    }

    #[test]
    fn ptt_key_is_ctrl_space() {
        use crossterm::event::{KeyCode, KeyModifiers};
        assert!(is_ptt_key(KeyCode::Char(' '), KeyModifiers::CONTROL));
        assert!(!is_ptt_key(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(!is_ptt_key(KeyCode::Char('a'), KeyModifiers::CONTROL));
    }
}
