//! Terminal lifecycle guard — ensures raw mode, alternate screen, cursor and
//! keyboard flags are restored on normal shutdown, returned errors and panics.

use std::io::stdout;
use std::panic;
use std::sync::Arc;

use crossterm::cursor::Show;
use crossterm::event::{DisableBracketedPaste, PopKeyboardEnhancementFlags};
use crossterm::terminal::{disable_raw_mode, LeaveAlternateScreen};
use crossterm::ExecutableCommand;

/// Best-effort terminal restoration. Safe to call multiple times and from
/// panicking contexts; individual failures are ignored so that one broken
/// capability does not prevent restoring the others.
pub fn restore_terminal() {
    let mut stdout = stdout();
    let _ = disable_raw_mode();
    let _ = stdout.execute(PopKeyboardEnhancementFlags);
    let _ = stdout.execute(DisableBracketedPaste);
    let _ = stdout.execute(LeaveAlternateScreen);
    let _ = stdout.execute(Show);
}

/// Re-initialise the terminal after an external-editor session.
/// This re-enters alternate screen, re-enables raw mode, and restores
/// keyboard enhancement flags.
pub fn reinit_terminal() -> Result<(), Box<dyn std::error::Error>> {
    use crossterm::event::{
        EnableBracketedPaste, KeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    };
    use crossterm::terminal::{enable_raw_mode, EnterAlternateScreen};
    use crossterm::ExecutableCommand;

    enable_raw_mode()?;
    let mut stdout = stdout();
    stdout.execute(EnterAlternateScreen)?;
    stdout.execute(EnableBracketedPaste)?;
    stdout.execute(PushKeyboardEnhancementFlags(
        KeyboardEnhancementFlags::REPORT_EVENT_TYPES
            | KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES,
    ))?;
    Ok(())
}

/// Guard that installs a panic hook restoring the terminal and chains to the
/// previous hook. Dropping the guard restores the terminal and reinstates the
/// previous panic hook.
pub struct TerminalGuard {
    previous_hook: Arc<dyn Fn(&panic::PanicHookInfo<'_>) + Send + Sync + 'static>,
    restore: Arc<dyn Fn() + Send + Sync + 'static>,
}

impl TerminalGuard {
    /// Install using the real crossterm restoration.
    pub fn install() -> Self {
        Self::with_restore(restore_terminal)
    }

    /// Install with an arbitrary restore function. Used by tests to inject
    /// mocked terminal operations.
    pub fn with_restore<F>(restore: F) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        let previous_hook: Arc<dyn Fn(&panic::PanicHookInfo<'_>) + Send + Sync + 'static> =
            Arc::from(panic::take_hook());
        let hook_for_panic = Arc::clone(&previous_hook);
        let restore = Arc::new(restore);
        let restore_for_hook = Arc::clone(&restore);
        panic::set_hook(Box::new(move |info| {
            // Contain any failure inside the hook so a buggy restore or
            // previous hook does not turn a recoverable panic into an abort.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                restore_for_hook();
            }));
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                hook_for_panic(info);
            }));
        }));
        Self {
            previous_hook,
            restore,
        }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        (self.restore)();
        let previous_hook = Arc::clone(&self.previous_hook);
        let _ = panic::take_hook();
        panic::set_hook(Box::new(move |info| previous_hook(info)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    // Panic-hook tests are global; run them serially.
    static PANIC_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn restore_is_called_on_drop() {
        let count = Arc::new(AtomicUsize::new(0));
        let count_for_guard = Arc::clone(&count);
        let _guard = TerminalGuard::with_restore(move || {
            count_for_guard.fetch_add(1, Ordering::SeqCst);
        });
        drop(_guard);
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn repeated_cleanup_is_safe() {
        let count = Arc::new(AtomicUsize::new(0));
        let count_for_guard = Arc::clone(&count);
        let guard = TerminalGuard::with_restore(move || {
            count_for_guard.fetch_add(1, Ordering::SeqCst);
        });
        drop(guard);
        // A second guard performs another best-effort cleanup with the same
        // injected restore; this must not panic.
        let count_for_guard2 = Arc::clone(&count);
        let guard2 = TerminalGuard::with_restore(move || {
            count_for_guard2.fetch_add(1, Ordering::SeqCst);
        });
        drop(guard2);
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn panic_hook_chains_and_restores() {
        let _lock = PANIC_TEST_LOCK.lock().unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let count_for_guard = Arc::clone(&count);
        let _guard = TerminalGuard::with_restore(move || {
            count_for_guard.fetch_add(1, Ordering::SeqCst);
        });

        let result = std::panic::catch_unwind(|| {
            panic!("controlled test panic");
        });
        assert!(result.is_err());
        // The hook fires once during the panic.
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn previous_panic_hook_is_restored() {
        let _lock = PANIC_TEST_LOCK.lock().unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let count_for_hook = Arc::clone(&count);
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            count_for_hook.fetch_add(1, Ordering::SeqCst);
            previous(info);
        }));

        let guard_count = Arc::new(AtomicUsize::new(0));
        let guard_count_for_guard = Arc::clone(&guard_count);
        let guard = TerminalGuard::with_restore(move || {
            guard_count_for_guard.fetch_add(1, Ordering::SeqCst);
        });
        drop(guard);

        let result = std::panic::catch_unwind(|| {
            panic!("hook restoration test panic");
        });
        assert!(result.is_err());
        assert_eq!(guard_count.load(Ordering::SeqCst), 1);
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
}
