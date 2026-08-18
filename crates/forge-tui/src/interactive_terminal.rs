use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::mpsc::{self, Receiver};
use std::thread;

const SCROLLBACK_LINES: usize = 1_024;
const INTERACTIVE_SHELL_ARGS: &[&str] = &["-il"];
/// Cap queued PTY chunks so a command that outpaces terminal rendering applies
/// backpressure to its reader instead of growing process memory without bound.
const OUTPUT_QUEUE_CAPACITY: usize = 64;
/// Leave time for input and drawing when a program emits a large burst.
const MAX_OUTPUT_CHUNKS_PER_POLL: usize = 64;
/// Readline/zsh kill-to-start-of-line. Used to wipe a typed `exit` we never
/// want the shell to execute.
const LINE_KILL: u8 = 0x15;

pub(crate) struct InteractiveTerminal {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    output_rx: Receiver<Vec<u8>>,
    screen: vt100::Parser,
    display: String,
    size: (u16, u16),
    pub(crate) running: bool,
    pub(crate) shell: String,
    pending_command: String,
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
        for argument in INTERACTIVE_SHELL_ARGS {
            command.arg(*argument);
        }
        command.cwd(cwd);
        let child = pty.slave.spawn_command(command).map_err(other)?;
        let mut reader = pty.master.try_clone_reader().map_err(other)?;
        let writer = pty.master.take_writer().map_err(other)?;
        let (output_tx, output_rx) = mpsc::sync_channel(OUTPUT_QUEUE_CAPACITY);
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
            display: String::new(),
            size: (cols.max(20), rows.max(2)),
            running: true,
            shell,
            pending_command: String::new(),
        })
    }

    /// Drain output that the reader thread has made available.
    ///
    /// Returns whether the rendered terminal state changed. The PTY reader
    /// cannot wake crossterm's input poll, so the event loop uses this result
    /// to repaint while it is waiting for keyboard input.
    pub(crate) fn poll(&mut self) -> bool {
        let mut changed = false;
        for _ in 0..MAX_OUTPUT_CHUNKS_PER_POLL {
            let Ok(bytes) = self.output_rx.try_recv() else {
                break;
            };
            self.screen.process(&bytes);
            changed = true;
        }
        if changed {
            self.display = self.screen.screen().contents();
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

    /// Forward typed or pasted input, but treat a submitted `exit` as a request
    /// to close the panel instead of killing the login shell.
    pub(crate) fn consume_input(&mut self, bytes: &[u8]) -> io::Result<bool> {
        let (forward, close) = feed_pending_command(&mut self.pending_command, bytes);
        if !forward.is_empty() {
            self.write(&forward)?;
        }
        Ok(close)
    }

    pub(crate) fn resize(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        let rows = rows.max(2);
        let cols = cols.max(20);
        if self.size == (cols, rows) {
            return Ok(());
        }
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(other)?;
        self.screen.screen_mut().set_size(rows, cols);
        self.size = (cols, rows);
        self.display = self.screen.screen().contents();
        Ok(())
    }

    pub(crate) fn display_output(&self) -> &str {
        &self.display
    }

    pub(crate) fn cursor_position(&self) -> (u16, u16) {
        let (row, column) = self.screen.screen().cursor_position();
        (column, row)
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

fn is_panel_close_command(line: &str) -> bool {
    line.trim() == "exit"
}

fn erase_last_word(line: &mut String) {
    let without_trailing = line.trim_end_matches(char::is_whitespace);
    let prefix_len = without_trailing
        .rmatch_indices(char::is_whitespace)
        .next()
        .map(|(idx, ws)| idx + ws.len())
        .unwrap_or(0);
    line.truncate(prefix_len);
}

/// Track the in-progress prompt line so a submitted `exit` can close the
/// panel without reaching the shell. Returns bytes to write and whether the
/// caller should close the panel after writing them.
fn feed_pending_command(line: &mut String, bytes: &[u8]) -> (Vec<u8>, bool) {
    let mut forward = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\r' | b'\n' => {
                if is_panel_close_command(line) {
                    line.clear();
                    forward.push(LINE_KILL);
                    return (forward, true);
                }
                forward.push(bytes[i]);
                line.clear();
                i += 1;
            }
            0x7f | 0x08 => {
                forward.push(bytes[i]);
                let _ = line.pop();
                i += 1;
            }
            0x15 | 0x03 => {
                forward.push(bytes[i]);
                line.clear();
                i += 1;
            }
            0x17 => {
                forward.push(bytes[i]);
                erase_last_word(line);
                i += 1;
            }
            b if b.is_ascii_control() => {
                forward.push(bytes[i]);
                line.clear();
                i += 1;
            }
            _ => {
                let rest = &bytes[i..];
                if let Some(ch) = std::str::from_utf8(rest)
                    .ok()
                    .and_then(|s| s.chars().next())
                {
                    if !ch.is_control() {
                        line.push(ch);
                        let len = ch.len_utf8();
                        forward.extend_from_slice(&bytes[i..i + len]);
                        i += len;
                        continue;
                    }
                }
                forward.push(bytes[i]);
                line.clear();
                i += 1;
            }
        }
    }
    (forward, false)
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
    fn interactive_shell_args_enable_filename_completion() {
        assert_eq!(super::INTERACTIVE_SHELL_ARGS, &["-il"]);
    }

    #[test]
    fn submitted_exit_closes_panel_instead_of_reaching_the_shell() {
        let mut line = String::new();
        assert_eq!(
            super::feed_pending_command(&mut line, b"exit\r"),
            (vec![b'e', b'x', b'i', b't', super::LINE_KILL], true)
        );
        assert!(line.is_empty());

        line = String::from("ex");
        assert_eq!(
            super::feed_pending_command(&mut line, b"it\n"),
            (vec![b'i', b't', super::LINE_KILL], true)
        );
        assert!(line.is_empty());
    }

    #[test]
    fn other_commands_and_partial_exit_still_reach_the_shell() {
        let mut line = String::new();
        assert_eq!(
            super::feed_pending_command(&mut line, b"ls\r"),
            (b"ls\r".to_vec(), false)
        );
        assert!(line.is_empty());

        line = String::new();
        assert_eq!(
            super::feed_pending_command(&mut line, b"exit 0\r"),
            (b"exit 0\r".to_vec(), false)
        );
        assert!(line.is_empty());

        line = String::new();
        assert_eq!(
            super::feed_pending_command(&mut line, b"ex"),
            (b"ex".to_vec(), false)
        );
        assert_eq!(line, "ex");
        assert_eq!(
            super::feed_pending_command(&mut line, &[0x7f]),
            (vec![0x7f], false)
        );
        assert_eq!(line, "e");
    }

    #[test]
    fn vt100_cursor_position_is_row_then_column() {
        let mut terminal = vt100::Parser::new(3, 20, 0);
        terminal.process(b"ab\r\n12345");
        assert_eq!(terminal.screen().cursor_position(), (1, 5));
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
