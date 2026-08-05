use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::mpsc::{self, Receiver};
use std::thread;

const SCROLLBACK_LINES: usize = 1_024;

pub(crate) struct InteractiveTerminal {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    output_rx: Receiver<Vec<u8>>,
    screen: vt100::Parser,
    pub(crate) running: bool,
    pub(crate) shell: String,
}

impl InteractiveTerminal {
    pub(crate) fn spawn(cwd: &Path, cols: u16, rows: u16) -> io::Result<Self> {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".into());
        let pty = native_pty_system()
            .openpty(PtySize {
                rows: rows.max(2),
                cols: cols.max(20),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(other)?;
        let mut command = CommandBuilder::new(&shell);
        command.arg("-l");
        command.cwd(cwd);
        let child = pty.slave.spawn_command(command).map_err(other)?;
        let mut reader = pty.master.try_clone_reader().map_err(other)?;
        let writer = pty.master.take_writer().map_err(other)?;
        let (output_tx, output_rx) = mpsc::channel();
        thread::Builder::new()
            .name("forge-terminal-reader".into())
            .spawn(move || {
                let mut buffer = [0_u8; 4096];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) | Err(_) => break,
                        Ok(count) => {
                            if output_tx.send(buffer[..count].to_vec()).is_err() {
                                break;
                            }
                        }
                    }
                }
            })
            .map_err(other)?;
        Ok(Self {
            master: pty.master,
            writer,
            child,
            output_rx,
            screen: vt100::Parser::new(rows.max(2), cols.max(20), SCROLLBACK_LINES),
            running: true,
            shell,
        })
    }

    /// Drain output that the reader thread has made available.
    ///
    /// Returns whether the rendered terminal state changed. The PTY reader
    /// cannot wake crossterm's input poll, so the event loop uses this result
    /// to repaint while it is waiting for keyboard input.
    pub(crate) fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(bytes) = self.output_rx.try_recv() {
            self.screen.process(&bytes);
            changed = true;
        }
        let child_running = match self.child.try_wait() {
            Ok(Some(_)) | Err(_) => false,
            Ok(None) => true,
        };
        if !child_running && self.running {
            self.running = false;
            changed = true;
        }
        changed
    }

    pub(crate) fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()
    }

    pub(crate) fn resize(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        let rows = rows.max(2);
        let cols = cols.max(20);
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(other)?;
        self.screen.set_size(rows, cols);
        Ok(())
    }

    pub(crate) fn display_output(&self) -> String {
        self.screen.screen().contents()
    }
}

impl Drop for InteractiveTerminal {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn other(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::InteractiveTerminal;
    use std::thread;
    use std::time::Duration;
    use tempfile::tempdir;

    #[test]
    fn terminal_emulator_preserves_line_endings_and_cursor_repaints() {
        let mut terminal = vt100::Parser::new(3, 20, 0);
        terminal.process(b"one\r two\x1b[31m!\x1b[0m\r\nthree\r\n");
        assert_eq!(terminal.screen().contents(), " two!\nthree");
    }

    #[test]
    fn terminal_emulator_preserves_pty_crlf_output_lines() {
        let mut terminal = vt100::Parser::new(4, 20, 0);
        terminal.process(b"AGENTS.md\r\nCargo.toml\r\ncrates\r\n");
        assert_eq!(
            terminal.screen().contents(),
            "AGENTS.md\nCargo.toml\ncrates"
        );
    }

    #[test]
    fn shell_accepts_input_and_returns_output() {
        let dir = tempdir().unwrap();
        let mut terminal = InteractiveTerminal::spawn(dir.path(), 80, 8).unwrap();
        terminal
            .write(b"printf 'forge-terminal-first\\nforge-terminal-second\\n'; exit\n")
            .unwrap();
        for _ in 0..50 {
            terminal.poll();
            let rendered = terminal.display_output();
            if rendered.contains("forge-terminal-first\nforge-terminal-second") {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!(
            "shell did not render expected output: {:?}",
            terminal.display_output()
        );
    }
}
