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

/// Whether this process may allocate a pseudo-terminal at all.
///
/// Some execution environments (CI sandboxes, agent harnesses) deny the
/// `openpty` syscall outright. Probed once and cached so every terminal
/// test can degrade to a skip instead of a panic on such hosts.
#[cfg(test)]
pub(crate) fn pty_allocation_available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        native_pty_system()
            .openpty(PtySize {
                rows: 2,
                cols: 20,
                pixel_width: 0,
                pixel_height: 0,
            })
            .is_ok()
    })
}

#[derive(Debug)]
struct PendingTerminalCommand {
    command: String,
    marker: String,
    output: Vec<u8>,
}

pub(crate) struct InteractiveTerminal {
    #[cfg(test)]
    cwd: std::path::PathBuf,
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
    input_line_trusted: bool,
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
            #[cfg(test)]
            cwd: cwd.to_path_buf(),
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
            input_line_trusted: true,
            pending_command: None,
            command_completion: None,
            hidden_status_marker: None,
            command_sequence: 0,
        })
    }

    #[cfg(test)]
    pub(crate) fn cwd(&self) -> &Path {
        &self.cwd
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
        self.scroll_to_live();
        self.writer.write_all(bytes)?;
        self.writer.flush()
    }

    /// Forward typed or pasted input, but treat a submitted `exit` as a request
    /// to close the panel instead of killing the login shell.
    pub(crate) fn consume_input(&mut self, bytes: &[u8]) -> io::Result<bool> {
        if self.pending_command.is_some() || self.alternate_screen() {
            self.write(bytes)?;
            return Ok(false);
        }
        // Cursor motion, history and completion make the shadow line unknown.
        // Never interpret a partial reconstruction as the panel's bare `exit`.
        if bytes.iter().any(|byte| {
            byte.is_ascii_control()
                && !matches!(byte, b'\r' | b'\n' | 0x7f | 0x08 | 0x15 | 0x03 | 0x17)
        }) {
            self.input_line_trusted = false;
        }
        if !self.input_line_trusted {
            self.write(bytes)?;
            if bytes
                .iter()
                .any(|byte| matches!(byte, b'\r' | b'\n' | 0x03))
            {
                self.input_line.clear();
                self.input_line_trusted = true;
            }
            return Ok(false);
        }
        // Manual input belongs to the shell. Appending status scripts corrupts
        // history, multiline input, completion and cursor-edited commands.
        let (forward, close) = feed_pending_command(&mut self.input_line, bytes);
        if !forward.is_empty() {
            self.write(&forward)?;
        }
        Ok(close)
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

    pub(crate) fn paste(&mut self, data: &str) -> io::Result<bool> {
        if !self.screen.screen().bracketed_paste() {
            self.input_line_trusted = false;
            self.write(data.as_bytes())?;
            return Ok(false);
        }
        let mut bytes = b"\x1b[200~".to_vec();
        bytes.extend_from_slice(data.as_bytes());
        bytes.extend_from_slice(b"\x1b[201~");
        self.input_line_trusted = false;
        self.write(&bytes)?;
        Ok(false)
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

    pub(crate) fn render(
        &self,
        area: ratatui::layout::Rect,
        buf: &mut ratatui::buffer::Buffer,
        focused: bool,
    ) {
        use ratatui::style::{Color, Modifier};
        let color = |value, fallback| match value {
            vt100::Color::Default => fallback,
            vt100::Color::Idx(index) => Color::Indexed(index),
            vt100::Color::Rgb(red, green, blue) => Color::Rgb(red, green, blue),
        };
        let base = crate::theme::text().patch(crate::theme::panel());
        for row in 0..area.height {
            for column in 0..area.width {
                let Some(cell) = self.screen.screen().cell(row, column) else {
                    continue;
                };
                let mut style = base
                    .fg(color(cell.fgcolor(), base.fg.unwrap_or(Color::Reset)))
                    .bg(color(cell.bgcolor(), base.bg.unwrap_or(Color::Reset)));
                for (enabled, modifier) in [
                    (cell.bold(), Modifier::BOLD),
                    (cell.dim(), Modifier::DIM),
                    (cell.italic(), Modifier::ITALIC),
                    (cell.underline(), Modifier::UNDERLINED),
                    (cell.inverse(), Modifier::REVERSED),
                ] {
                    if enabled {
                        style = style.add_modifier(modifier);
                    }
                }
                let target = &mut buf[(area.x + column, area.y + row)];
                // Explicit !commands sanitize their instrumentation in display.
                // The panel already painted that text; apply attributes without
                // putting hidden status scripts back into its character cells.
                if self.hidden_status_marker.is_none() {
                    target.reset();
                    target.set_symbol(if cell.has_contents() {
                        cell.contents()
                    } else {
                        " "
                    });
                }
                target.set_style(ratatui::style::Style::default().remove_modifier(Modifier::all()));
                target.set_style(style);
            }
        }
        if focused && self.cursor_visible() {
            let (column, row) = self.cursor_position();
            if column < area.width && row < area.height {
                buf[(area.x + column, area.y + row)].set_style(crate::theme::caret());
            }
        }
    }

    pub(crate) fn scroll(&mut self, delta: isize) {
        if self.screen.screen().alternate_screen() {
            return;
        }
        let offset = self
            .screen
            .screen()
            .scrollback()
            .saturating_add_signed(delta);
        self.screen.screen_mut().set_scrollback(offset);
        self.display = self.screen.screen().contents();
    }

    fn scroll_to_live(&mut self) {
        if self.screen.screen().scrollback() > 0 {
            self.screen.screen_mut().set_scrollback(0);
            self.display = self.screen.screen().contents();
        }
    }

    pub(crate) fn cursor_visible(&self) -> bool {
        self.running
            && self.screen.screen().scrollback() == 0
            && !self.screen.screen().hide_cursor()
    }

    pub(crate) fn alternate_screen(&self) -> bool {
        self.screen.screen().alternate_screen()
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
        // Keep a whole possible wrapper fragment across PTY read boundaries.
        // Filtering only the drained prefix could split a script in two and
        // leave its suffix visible (especially after wrapping or resizing).
        let keep = status_wrapper_parts(&self.shell, &pending.marker)
            .iter()
            .map(String::len)
            .max()
            .unwrap_or(0)
            .saturating_add(pending.marker.len())
            .saturating_add(16);
        pending.output = strip_status_wrapper_bytes(
            std::mem::take(&mut pending.output),
            &self.shell,
            &pending.marker,
        );
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
    format!("__forge_status=$?; printf '\\n{marker}%s\\n' \"$__forge_status\"")
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
        .fold(display.to_owned(), |mut display, part| {
            // Screen contents insert line breaks when echoed scripts wrap.
            // Match across those breaks so a resize cannot expose the wrapper.
            while let Some((start, end)) = display.char_indices().find_map(|(start, _)| {
                let mut cursor = start;
                for byte in part.bytes() {
                    while display.as_bytes().get(cursor) == Some(&b'\n') {
                        cursor += 1;
                    }
                    if display.as_bytes().get(cursor) != Some(&byte) {
                        return None;
                    }
                    cursor += 1;
                }
                Some((start, cursor))
            }) {
                display.replace_range(start..end, "");
            }
            display
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
    fn copy_command_writes_the_destination_file() {
        if !super::pty_allocation_available() {
            eprintln!("skipping: this host denies PTY allocation");
            return;
        }
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("source.txt"), "copied\n").unwrap();
        let mut terminal = InteractiveTerminal::spawn(dir.path(), 80, 8).unwrap();
        terminal
            .start_command("cp source.txt destination.txt")
            .unwrap();
        for _ in 0..100 {
            terminal.poll();
            if terminal.take_command_completion().is_some() {
                assert_eq!(
                    std::fs::read_to_string(dir.path().join("destination.txt")).unwrap(),
                    "copied\n"
                );
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!(
            "copy command did not complete: {:?}",
            terminal.display_output()
        );
    }

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
    fn empty_and_blank_manual_submissions_are_forwarded_unchanged() {
        let mut line = String::new();
        let (forward, close) = super::feed_pending_command(&mut line, b"");
        assert!(forward.is_empty());
        assert!(!close);

        line.push_str("   ");
        assert_eq!(
            super::feed_pending_command(&mut line, b"\r"),
            (b"\r".to_vec(), false)
        );
        assert!(line.is_empty());
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
        if !super::pty_allocation_available() {
            eprintln!("skipping: this host denies PTY allocation");
            return;
        }
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
    fn blank_enter_reaches_the_live_shell() {
        if !super::pty_allocation_available() {
            eprintln!("skipping: this host denies PTY allocation");
            return;
        }
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
        assert_ne!(terminal.display_output(), before);
    }

    #[test]
    fn bang_command_reports_exit_status_and_keeps_output() {
        if !super::pty_allocation_available() {
            eprintln!("skipping: this host denies PTY allocation");
            return;
        }
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
    fn typed_commands_and_history_recall_reach_shell_without_status_scripts() {
        if !super::pty_allocation_available() {
            eprintln!("skipping: this host denies PTY allocation");
            return;
        }
        let dir = tempdir().unwrap();
        let mut terminal = InteractiveTerminal::spawn(dir.path(), 80, 8).unwrap();
        terminal.consume_input(b"printf 'typed-output\\n'").unwrap();
        terminal.consume_input(b"\r").unwrap();
        for _ in 0..100 {
            terminal.poll();
            if terminal.display_output().contains("typed-output\n") {
                assert!(terminal.take_command_completion().is_none());
                assert!(terminal.pending_command.is_none());
                assert!(!terminal.display_output().contains("__forge_status"));
                assert!(!terminal
                    .display_output()
                    .contains("printf '\\n__FORGE_STATUS_1__%s\\n' \"$__forge_status\""));
                terminal.consume_input(b"\x1b[A\r").unwrap();
                for _ in 0..20 {
                    terminal.poll();
                    thread::sleep(Duration::from_millis(10));
                }
                assert!(!terminal.display_output().contains("__FORGE_STATUS"));
                assert!(
                    terminal.display_output().matches("typed-output\n").count() >= 2,
                    "history recall must execute the user's command: {:?}",
                    terminal.display_output()
                );
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
    fn scrollback_exposes_history_and_typing_returns_to_live_screen() {
        if !super::pty_allocation_available() {
            return;
        }
        let dir = tempdir().unwrap();
        let mut terminal = InteractiveTerminal::spawn(dir.path(), 40, 4).unwrap();
        terminal.screen = vt100::Parser::new(4, 40, super::SCROLLBACK_LINES);
        terminal
            .screen
            .process(b"first\r\nsecond\r\nthird\r\nfourth\r\nfifth\r\n");
        terminal.scroll(3);
        assert!(terminal.display_output().contains("first"));
        assert!(!terminal.cursor_visible());
        terminal.write(b"\x03").unwrap();
        assert_eq!(terminal.screen.screen().scrollback(), 0);
        assert!(terminal.display_output().contains("fifth"));
    }

    #[test]
    fn cursor_edited_exit_text_does_not_hide_the_panel() {
        if !super::pty_allocation_available() {
            return;
        }
        let dir = tempdir().unwrap();
        let mut terminal = InteractiveTerminal::spawn(dir.path(), 40, 4).unwrap();
        terminal.consume_input(b"echo suffix").unwrap();
        terminal.consume_input(b"\x01").unwrap();
        assert!(!terminal.consume_input(b"exit\r").unwrap());
    }

    #[test]
    fn explicit_command_wrappers_stay_hidden_across_split_pty_reads() {
        if !super::pty_allocation_available() {
            return;
        }
        let dir = tempdir().unwrap();
        let mut terminal = InteractiveTerminal::spawn(dir.path(), 40, 4).unwrap();
        let marker = "__FORGE_STATUS_TEST__";
        terminal.pending_command = Some(super::PendingTerminalCommand {
            command: "echo test".into(),
            marker: marker.into(),
            output: Vec::new(),
        });
        let wrapped = format!(
            "echo test{}{}\r\nvisible-output\r\n{marker}0\r\n",
            super::command_separator(&terminal.shell),
            super::command_suffix(&terminal.shell, marker)
        );
        for bytes in wrapped.as_bytes().chunks(3) {
            terminal.process_output(bytes);
        }
        let output = terminal.screen.screen().contents();
        assert!(output.contains("visible-output"), "{output:?}");
        assert!(!output.contains("forge_status"), "{output:?}");
        assert!(!output.contains("FORGE_STATUS"), "{output:?}");
        assert_eq!(
            terminal.take_command_completion().unwrap().exit_code,
            Some(0)
        );
    }

    #[test]
    fn terminal_render_preserves_ansi_attributes_unicode_and_cursor_text() {
        if !super::pty_allocation_available() {
            return;
        }
        let dir = tempdir().unwrap();
        let mut terminal = InteractiveTerminal::spawn(dir.path(), 40, 4).unwrap();
        terminal.screen = vt100::Parser::new(4, 40, 0);
        terminal.screen.process(b"\x1b[31;1mFAIL\x1b[0m\r\n");
        terminal.screen.process("é界".as_bytes());
        terminal.screen.process(b"\x1b[2;1H");
        let area = ratatui::layout::Rect::new(2, 1, 40, 4);
        let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 44, 6));
        terminal.render(area, &mut buf, true);
        assert_eq!(buf[(2, 1)].symbol(), "F");
        assert_eq!(buf[(2, 1)].fg, ratatui::style::Color::Indexed(1));
        assert!(buf[(2, 1)]
            .modifier
            .contains(ratatui::style::Modifier::BOLD));
        assert_eq!(buf[(2, 2)].symbol(), "é", "cursor must not erase text");
        assert_eq!(buf[(3, 2)].symbol(), "界");
        terminal.screen.process(b"\x1b[?25l");
        terminal.render(area, &mut buf, true);
        assert_eq!(buf[(2, 2)].style().bg, crate::theme::panel().bg);
        terminal.hidden_status_marker = Some("hidden".into());
        terminal.screen.process(b"\x1b[1;1H__forge_status");
        terminal.render(area, &mut buf, false);
        assert_eq!(
            buf[(2, 1)].symbol(),
            "F",
            "sanitized text must not be replaced with instrumentation"
        );
    }

    #[test]
    fn command_scripts_preserve_shell_specific_syntax() {
        assert_eq!(
            super::command_script("/bin/sh", "printf hi", "__FORGE_STATUS_1__"),
            "printf hi; __forge_status=$?; printf '\\n__FORGE_STATUS_1__%s\\n' \"$__forge_status\"\n"
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
    fn wrapped_status_scripts_stay_hidden_after_resize() {
        let marker = "__FORGE_STATUS_1__";
        let wrapper = format!("; {}", super::command_suffix("zsh", marker));
        let split = wrapper.find("forge_status").unwrap() + 4;
        let display = format!(
            "prompt echo hi{}\n{}\nhi\nprompt",
            &wrapper[..split],
            &wrapper[split..]
        );
        assert_eq!(
            super::strip_status_wrapper_display(&display, "zsh", marker),
            "prompt echo hi\nhi\nprompt"
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
