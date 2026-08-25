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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommandCompletion {
    pub(crate) command: String,
    pub(crate) exit_code: Option<i32>,
}

#[derive(Debug)]
struct PendingTerminalCommand {
    command: String,
    marker: String,
    output: Vec<u8>,
}

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
    input_line: String,
    pending_command: Option<PendingTerminalCommand>,
    command_completion: Option<CommandCompletion>,
    hidden_status_marker: Option<String>,
    command_sequence: u64,
}

impl InteractiveTerminal {
    pub(crate) fn spawn(cwd: &Path, cols: u16, rows: u16) -> io::Result<Self> {
        let shell = default_shell();
        let pty = native_pty_system()
            .openpty(PtySize {
                rows: rows.max(2),
                cols: cols.max(20),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(other)?;
        let mut command = CommandBuilder::new(&shell);
        for argument in shell_args(&shell) {
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
            input_line: String::new(),
            pending_command: None,
            command_completion: None,
            hidden_status_marker: None,
            command_sequence: 0,
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
            self.process_output(&bytes);
            changed = true;
        }
        if changed {
            self.display = self.screen.screen().contents();
            if let Some(marker) = self.hidden_status_marker.as_deref() {
                self.display = strip_status_wrapper_display(&self.display, &self.shell, marker);
            }
        }
        let child_running = match self.child.try_wait() {
            Ok(Some(_)) | Err(_) => false,
            Ok(None) => true,
        };
        if !child_running && self.running {
            self.running = false;
            if let Some(command) = self.pending_command.take() {
                self.command_completion = Some(CommandCompletion {
                    command: command.command,
                    exit_code: None,
                });
            }
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
        if self.pending_command.is_some() {
            self.write(bytes)?;
            return Ok(false);
        }
        let Some(newline) = bytes.iter().position(|byte| matches!(byte, b'\r' | b'\n')) else {
            let (forward, close) = feed_pending_command(&mut self.input_line, bytes);
            if !forward.is_empty() {
                self.write(&forward)?;
            }
            return Ok(close);
        };

        let (forward, close) = feed_pending_command(&mut self.input_line, &bytes[..newline]);
        if !forward.is_empty() {
            self.write(&forward)?;
        }
        if close {
            let (forward, close) = feed_pending_command(
                &mut self.input_line,
                &bytes[newline..newline.saturating_add(1)],
            );
            if !forward.is_empty() {
                self.write(&forward)?;
            }
            return Ok(close);
        }

        let line = std::mem::take(&mut self.input_line);
        if line.trim().is_empty() {
            if !line.is_empty() {
                self.write(&[LINE_KILL])?;
            }
            return Ok(false);
        }
        if is_panel_close_command(&line) {
            self.write(&[LINE_KILL])?;
            return Ok(true);
        }
        self.track_submitted_command(&line)?;
        if newline + 1 < bytes.len() {
            self.write(&bytes[newline + 1..])?;
        }
        Ok(false)
    }

    pub(crate) fn start_command(&mut self, command: &str) -> io::Result<()> {
        if !self.running {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "embedded terminal shell has exited",
            ));
        }
        if self.pending_command.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "embedded terminal is busy",
            ));
        }
        self.command_sequence = self.command_sequence.wrapping_add(1);
        let marker = format!("__FORGE_STATUS_{}__", self.command_sequence);
        self.hidden_status_marker = Some(marker.clone());
        let script = command_script(&self.shell, command, &marker);
        self.write(script.as_bytes())?;
        self.pending_command = Some(PendingTerminalCommand {
            command: command.to_owned(),
            marker,
            output: Vec::new(),
        });
        Ok(())
    }

    fn track_submitted_command(&mut self, command: &str) -> io::Result<()> {
        if self.pending_command.is_some() {
            return Ok(());
        }
        self.command_sequence = self.command_sequence.wrapping_add(1);
        let marker = format!("__FORGE_STATUS_{}__", self.command_sequence);
        self.hidden_status_marker = Some(marker.clone());
        let separator = command_separator(&self.shell);
        let mut suffix =
            format!("{separator}{}\r", command_suffix(&self.shell, &marker)).into_bytes();
        suffix.push(b'\r');
        self.write(&suffix)?;
        self.pending_command = Some(PendingTerminalCommand {
            command: command.to_owned(),
            marker,
            output: Vec::new(),
        });
        Ok(())
    }

    pub(crate) fn take_command_completion(&mut self) -> Option<CommandCompletion> {
        self.command_completion.take()
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
        if let Some(marker) = self.hidden_status_marker.as_deref() {
            self.display = strip_status_wrapper_display(&self.display, &self.shell, marker);
        }
        Ok(())
    }

    pub(crate) fn display_output(&self) -> &str {
        &self.display
    }

    pub(crate) fn cursor_position(&self) -> (u16, u16) {
        let (row, column) = self.screen.screen().cursor_position();
        (column, row)
    }

    fn process_output(&mut self, bytes: &[u8]) {
        let Some(pending) = self.pending_command.as_mut() else {
            self.screen.process(bytes);
            return;
        };
        pending.output.extend_from_slice(bytes);
        if let Some((start, end, exit_code)) = find_completion(&pending.output, &pending.marker) {
            let mut visible = Vec::with_capacity(pending.output.len() - (end - start));
            visible.extend_from_slice(&pending.output[..start]);
            visible.extend_from_slice(&pending.output[end..]);
            visible = strip_status_wrapper_bytes(visible, &self.shell, &pending.marker);
            let command = pending.command.clone();
            self.screen.process(&visible);
            self.pending_command = None;
            self.command_completion = Some(CommandCompletion { command, exit_code });
            return;
        }
        let keep = pending.marker.len().saturating_add(16);
        if pending.output.len() > keep {
            let split_at = pending.output.len() - keep;
            let visible = strip_status_wrapper_bytes(
                pending.output.drain(..split_at).collect(),
                &self.shell,
                &pending.marker,
            );
            self.screen.process(&visible);
        }
    }
}

fn shell_args(shell: &str) -> &'static [&'static str] {
    let shell_name = Path::new(shell)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(shell)
        .to_ascii_lowercase();
    if shell_name == "cmd.exe" || shell_name == "cmd" {
        &["/Q"]
    } else if matches!(
        shell_name.as_str(),
        "powershell.exe" | "powershell" | "pwsh.exe" | "pwsh"
    ) {
        &["-NoLogo", "-NoExit"]
    } else {
        INTERACTIVE_SHELL_ARGS
    }
}

impl Drop for InteractiveTerminal {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn command_script(shell: &str, command: &str, marker: &str) -> String {
    format!(
        "{command}{}{suffix}\n",
        command_separator(shell),
        suffix = command_suffix(shell, marker)
    )
}

fn command_separator(shell: &str) -> &'static str {
    let shell_name = Path::new(shell)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(shell)
        .to_ascii_lowercase();
    if shell_name == "cmd.exe" || shell_name == "cmd" {
        " & "
    } else if matches!(
        shell_name.as_str(),
        "powershell.exe" | "powershell" | "pwsh.exe" | "pwsh"
    ) {
        "; $__forge_success = $?; $__forge_exit = $LASTEXITCODE; "
    } else {
        "; "
    }
}

fn command_suffix(shell: &str, marker: &str) -> String {
    let shell_name = Path::new(shell)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(shell)
        .to_ascii_lowercase();
    if shell_name == "cmd.exe" || shell_name == "cmd" {
        return format!("call echo {marker}%%ERRORLEVEL%%\r\n");
    }
    if shell_name == "powershell.exe"
        || shell_name == "powershell"
        || shell_name == "pwsh.exe"
        || shell_name == "pwsh"
    {
        return format!(
            "$__forge_status = if ($__forge_success) {{ 0 }} else {{ if ($__forge_exit -ne $null) {{ $__forge_exit }} else {{ 1 }} }}\nWrite-Output \"{marker}$__forge_status\"\n"
        );
    }
    format!("__forge_status=$?\nprintf '\\n{marker}%s\\n' \"$__forge_status\"\n")
}

fn status_wrapper_parts(shell: &str, marker: &str) -> Vec<String> {
    format!(
        "{}{}",
        command_separator(shell),
        command_suffix(shell, marker)
    )
    .split('\n')
    .map(|part| part.trim_end_matches('\r'))
    .filter(|part| !part.is_empty())
    .map(str::to_owned)
    .collect()
}

fn strip_status_wrapper_display(display: &str, shell: &str, marker: &str) -> String {
    status_wrapper_parts(shell, marker)
        .into_iter()
        .fold(display.to_owned(), |display, part| {
            display.replace(&part, "")
        })
}

fn strip_status_wrapper_bytes(mut output: Vec<u8>, shell: &str, marker: &str) -> Vec<u8> {
    for part in status_wrapper_parts(shell, marker) {
        remove_bytes(&mut output, part.as_bytes());
    }
    output
}

fn remove_bytes(output: &mut Vec<u8>, needle: &[u8]) {
    if needle.is_empty() {
        return;
    }
    let mut filtered = Vec::with_capacity(output.len());
    let mut offset = 0;
    while let Some(relative) = output[offset..]
        .windows(needle.len())
        .position(|window| window == needle)
    {
        let start = offset + relative;
        filtered.extend_from_slice(&output[offset..start]);
        offset = start + needle.len();
    }
    filtered.extend_from_slice(&output[offset..]);
    *output = filtered;
}

fn find_completion(output: &[u8], marker: &str) -> Option<(usize, usize, Option<i32>)> {
    let marker = marker.as_bytes();
    let mut offset = 0;
    while let Some(relative) = output[offset..]
        .windows(marker.len())
        .position(|window| window == marker)
    {
        let start = offset + relative;
        let mut cursor = start + marker.len();
        let digits_start = cursor;
        while output.get(cursor).is_some_and(u8::is_ascii_digit) {
            cursor += 1;
        }
        if cursor > digits_start && matches!(output.get(cursor), Some(b'\n') | Some(b'\r')) {
            let digits_end = cursor;
            if output.get(cursor) == Some(&b'\r') && output.get(cursor + 1) == Some(&b'\n') {
                cursor += 2;
            } else {
                cursor += 1;
            }
            let code = std::str::from_utf8(&output[digits_start..digits_end])
                .ok()
                .and_then(|value| value.parse().ok());
            return Some((start, cursor, code));
        }
        offset = start + marker.len();
    }
    None
}

fn other(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

fn default_shell() -> String {
    if cfg!(windows) {
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into())
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "sh".into())
    }
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
    fn blank_submission_does_not_create_another_shell_prompt() {
        let mut line = String::new();
        let (forward, close) = super::feed_pending_command(&mut line, b"");
        assert!(forward.is_empty());
        assert!(!close);

        line.push_str("   ");
        assert!(line.trim().is_empty());
        assert!(!line.is_empty());
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

    #[test]
    fn blank_enter_does_not_change_live_shell_output() {
        let dir = tempdir().unwrap();
        let mut terminal = InteractiveTerminal::spawn(dir.path(), 80, 8).unwrap();
        for _ in 0..50 {
            terminal.poll();
            if !terminal.display_output().is_empty() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let before = terminal.display_output().to_owned();
        terminal.consume_input(b"\r").unwrap();
        for _ in 0..10 {
            terminal.poll();
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(terminal.display_output(), before);
    }

    #[test]
    fn bang_command_reports_exit_status_and_keeps_output() {
        let dir = tempdir().unwrap();
        let mut terminal = InteractiveTerminal::spawn(dir.path(), 80, 8).unwrap();
        terminal
            .start_command("printf 'bang-output\\n'; false")
            .unwrap();
        for _ in 0..100 {
            terminal.poll();
            if let Some(completion) = terminal.take_command_completion() {
                assert_eq!(completion.command, "printf 'bang-output\\n'; false");
                assert_eq!(completion.exit_code, Some(1));
                assert!(terminal.display_output().contains("bang-output"));
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("command did not complete: {:?}", terminal.display_output());
    }

    #[test]
    fn typed_command_reports_status_without_feeding_marker_to_process() {
        let dir = tempdir().unwrap();
        let mut terminal = InteractiveTerminal::spawn(dir.path(), 80, 8).unwrap();
        terminal.consume_input(b"printf 'typed-output\\n'").unwrap();
        terminal.consume_input(b"\r").unwrap();
        for _ in 0..100 {
            terminal.poll();
            if let Some(completion) = terminal.take_command_completion() {
                assert_eq!(completion.command, "printf 'typed-output\\n'");
                assert_eq!(completion.exit_code, Some(0));
                assert!(terminal.display_output().contains("typed-output"));
                assert!(!terminal.display_output().contains("__forge_status"));
                assert!(!terminal
                    .display_output()
                    .contains("printf '\\n__FORGE_STATUS_1__%s\\n' \"$__forge_status\""));
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!(
            "typed command did not complete: {:?}",
            terminal.display_output()
        );
    }

    #[test]
    fn command_scripts_preserve_shell_specific_syntax() {
        assert_eq!(
            super::command_script("/bin/sh", "printf hi", "__FORGE_STATUS_1__"),
            "printf hi; __forge_status=$?\nprintf '\\n__FORGE_STATUS_1__%s\\n' \"$__forge_status\"\n\n"
        );
        assert!(
            super::command_script("powershell.exe", "Write-Output hi", "m")
                .contains("Write-Output \"m$__forge_status\"")
        );
        assert!(
            super::command_script("cmd.exe", "echo hi", "m").contains("call echo m%%ERRORLEVEL%%")
        );
    }

    #[test]
    fn completion_marker_requires_digits_and_handles_crlf() {
        let output = b"echo __FORGE_STATUS_1__%s\r\n__FORGE_STATUS_1__7\r\n";
        let (start, end, code) = super::find_completion(output, "__FORGE_STATUS_1__").unwrap();
        assert_eq!(code, Some(7));
        assert_eq!(&output[start..end], b"__FORGE_STATUS_1__7\r\n");
        assert!(super::find_completion(b"__FORGE_STATUS_1__%s\n", "__FORGE_STATUS_1__").is_none());
    }
}
