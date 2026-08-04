use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::mpsc::{self, Receiver};
use std::thread;

const MAX_OUTPUT: usize = 64 * 1024;

pub(crate) struct InteractiveTerminal {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    output_rx: Receiver<Vec<u8>>,
    pub(crate) output: String,
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
            output: String::new(),
            running: true,
            shell,
        })
    }

    pub(crate) fn poll(&mut self) {
        while let Ok(bytes) = self.output_rx.try_recv() {
            self.output.push_str(&String::from_utf8_lossy(&bytes));
            if self.output.len() > MAX_OUTPUT {
                let start = self.output.len() - MAX_OUTPUT;
                let boundary = self.output.floor_char_boundary(start);
                self.output.drain(..boundary);
            }
        }
        match self.child.try_wait() {
            Ok(Some(_)) | Err(_) => self.running = false,
            Ok(None) => {}
        }
    }

    pub(crate) fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()
    }

    pub(crate) fn resize(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        self.master
            .resize(PtySize {
                rows: rows.max(2),
                cols: cols.max(20),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(other)
    }

    pub(crate) fn display_output(&self) -> String {
        strip_terminal_controls(&self.output)
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

fn strip_terminal_controls(input: &str) -> String {
    let mut output = String::new();
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
        } else if ch == '\r' {
            if let Some(start) = output.rfind('\n') {
                output.truncate(start + 1);
            } else {
                output.clear();
            }
        } else if ch == '\u{8}' {
            output.pop();
        } else if ch == '\t' || ch == '\n' || !ch.is_control() {
            output.push(ch);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::strip_terminal_controls;
    use super::InteractiveTerminal;
    use std::thread;
    use std::time::Duration;
    use tempfile::tempdir;

    #[test]
    fn strips_common_terminal_sequences_and_repaints() {
        assert_eq!(
            strip_terminal_controls("one\r two\x1b[31m!\x1b[0m\n"),
            " two!\n"
        );
    }

    #[test]
    fn shell_accepts_input_and_returns_output() {
        let dir = tempdir().unwrap();
        let mut terminal = InteractiveTerminal::spawn(dir.path(), 80, 8).unwrap();
        terminal
            .write(b"printf forge-terminal-test; exit\n")
            .unwrap();
        for _ in 0..50 {
            terminal.poll();
            if terminal.display_output().contains("forge-terminal-test") {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!(
            "shell did not produce expected output: {:?}",
            terminal.display_output()
        );
    }
}
