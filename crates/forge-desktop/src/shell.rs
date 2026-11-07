//! Interactive shell for the desktop terminal: one PTY per session, raw bytes
//! to the webview's terminal emulator, keystrokes back. No agent involvement,
//! no sandbox: this is the operator's own shell in the session workspace.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Mutex;

use base64::Engine;
use forge_types::SessionId;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ShellEvent {
    /// Base64 PTY bytes; the page decodes to `Uint8Array` for xterm.js.
    Output {
        data: String,
    },
    Exit {
        code: Option<i32>,
    },
}

struct ShellHandle {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    size: (u16, u16),
}

/// Every session's live shell, keyed by session. A window owns at most one
/// visible terminal, but switching sessions keeps the old PTY alive until the
/// page closes it or the app exits.
#[derive(Default)]
pub struct Shells {
    handles: Mutex<HashMap<SessionId, ShellHandle>>,
}

impl Shells {
    pub fn open<F>(
        &self,
        session_id: SessionId,
        root: &Path,
        cols: u16,
        rows: u16,
        mut emit: F,
    ) -> Result<(), String>
    where
        F: FnMut(ShellEvent) -> bool + Send + 'static,
    {
        self.close(session_id);
        let size = (cols.clamp(20, 500), rows.clamp(2, 200));
        let pty = native_pty_system()
            .openpty(PtySize {
                rows: size.1,
                cols: size.0,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| format!("cannot open a terminal: {error}"))?;

        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        let mut command = CommandBuilder::new(&shell);
        command.arg("-il");
        command.cwd(root);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        let child = pty
            .slave
            .spawn_command(command)
            .map_err(|error| format!("cannot start {shell}: {error}"))?;
        let mut reader = pty
            .master
            .try_clone_reader()
            .map_err(|error| format!("cannot read the terminal: {error}"))?;
        let writer = pty
            .master
            .take_writer()
            .map_err(|error| format!("cannot write to the terminal: {error}"))?;

        std::thread::Builder::new()
            .name("forge-desktop-pty".into())
            .spawn(move || {
                let mut buffer = [0_u8; 8192];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) | Err(_) => break,
                        Ok(count) => {
                            let data =
                                base64::engine::general_purpose::STANDARD.encode(&buffer[..count]);
                            if !emit(ShellEvent::Output { data }) {
                                break;
                            }
                        }
                    }
                }
                let _ = emit(ShellEvent::Exit { code: None });
            })
            .map_err(|error| format!("cannot start the terminal reader: {error}"))?;

        self.handles.lock().unwrap().insert(
            session_id,
            ShellHandle {
                master: pty.master,
                writer,
                child,
                size,
            },
        );
        Ok(())
    }

    pub fn write(&self, session_id: SessionId, data: &[u8]) -> Result<(), String> {
        let mut handles = self.handles.lock().unwrap();
        let handle = handles
            .get_mut(&session_id)
            .ok_or_else(|| "the terminal is not open".to_string())?;
        handle
            .writer
            .write_all(data)
            .and_then(|()| handle.writer.flush())
            .map_err(|error| format!("terminal write failed: {error}"))
    }

    pub fn resize(&self, session_id: SessionId, cols: u16, rows: u16) -> Result<(), String> {
        let mut handles = self.handles.lock().unwrap();
        let handle = handles
            .get_mut(&session_id)
            .ok_or_else(|| "the terminal is not open".to_string())?;
        let size = (cols.clamp(20, 500), rows.clamp(2, 200));
        if size == handle.size {
            return Ok(());
        }
        handle
            .master
            .resize(PtySize {
                rows: size.1,
                cols: size.0,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| format!("terminal resize failed: {error}"))?;
        handle.size = size;
        Ok(())
    }

    pub fn close(&self, session_id: SessionId) {
        if let Some(mut handle) = self.handles.lock().unwrap().remove(&session_id) {
            let _ = handle.child.kill();
        }
    }

    pub fn close_all(&self) {
        let handles: Vec<SessionId> = self.handles.lock().unwrap().keys().copied().collect();
        for session_id in handles {
            self.close(session_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// A real PTY round trip. Hosts that deny `openpty` skip rather than fail.
    #[test]
    fn a_shell_round_trips_a_command() {
        let shells = Shells::default();
        let session = SessionId::new_v4();
        let (tx, rx) = std::sync::mpsc::channel();
        let root = std::env::temp_dir();
        if shells
            .open(session, &root, 80, 24, move |event| tx.send(event).is_ok())
            .is_err()
        {
            return;
        }
        shells
            .write(session, b"echo forge-$((2+2))\n")
            .expect("write to the shell");

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut seen = String::new();
        while Instant::now() < deadline && !seen.contains("forge-4") {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(ShellEvent::Output { data }) => {
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(data)
                        .unwrap();
                    seen.push_str(&String::from_utf8_lossy(&bytes));
                }
                Ok(ShellEvent::Exit { .. })
                | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    break;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
        shells.close(session);
        assert!(seen.contains("forge-4"), "shell output: {seen:?}");
    }
}
