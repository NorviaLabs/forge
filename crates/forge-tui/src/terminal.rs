//! Terminal lifecycle guard — ensures raw mode, alternate screen, cursor and
//! keyboard flags are restored on normal shutdown, returned errors and panics.

use std::io::stdout;
use std::panic;
use std::sync::Arc;

use crossterm::cursor::{SetCursorStyle, Show};
use crossterm::event::{DisableBracketedPaste, DisableMouseCapture, PopKeyboardEnhancementFlags};
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
    // Mouse capture must be relinquished before leaving the alternate screen so
    // the user's next shell session is not left reporting mouse events.
    let _ = stdout.execute(DisableMouseCapture);
    let _ = stdout.execute(LeaveAlternateScreen);
    let _ = stdout.execute(SetCursorStyle::DefaultUserShape);
    let _ = stdout.execute(Show);
}

/// Re-initialise the terminal after an external-editor session.
/// This re-enters alternate screen, re-enables raw mode, and restores
/// keyboard enhancement flags.
pub fn reinit_terminal() -> Result<(), Box<dyn std::error::Error>> {
    use crossterm::event::{
        EnableBracketedPaste, EnableMouseCapture, KeyboardEnhancementFlags,
        PushKeyboardEnhancementFlags,
    };
    use crossterm::terminal::{enable_raw_mode, EnterAlternateScreen};
    use crossterm::ExecutableCommand;

    enable_raw_mode()?;
    let mut stdout = stdout();
    stdout.execute(EnterAlternateScreen)?;
    stdout.execute(SetCursorStyle::SteadyBlock)?;
    stdout.execute(EnableBracketedPaste)?;
    stdout.execute(EnableMouseCapture)?;
    stdout.execute(PushKeyboardEnhancementFlags(
        KeyboardEnhancementFlags::REPORT_EVENT_TYPES
            | KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES,
    ))?;
    Ok(())
}

/// Clear the physical terminal after re-entering the TUI so stale editor
/// contents do not survive the first redraw.
pub fn clear_terminal() -> Result<(), Box<dyn std::error::Error>> {
    use crossterm::terminal::ClearType;
    use crossterm::ExecutableCommand;

    let mut stdout = stdout();
    stdout.execute(crossterm::terminal::Clear(ClearType::All))?;
    stdout.execute(crossterm::cursor::MoveTo(0, 0))?;
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
    use std::sync::{Mutex, MutexGuard};
    use std::thread::ThreadId;

    /// The panic hook is process-global, and **every** `TerminalGuard` swaps it,
    /// so every test here must hold this lock — not only the ones that panic.
    static PANIC_TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Poisoning is recovered rather than propagated: a panic-hook test that fails
    /// would otherwise poison the lock and fail every sibling with a cascade that
    /// hides the original failure.
    fn lock_panic_hook() -> MutexGuard<'static, ()> {
        PANIC_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The boxed shape `panic::set_hook` accepts.
    type BoxedHook = Box<dyn Fn(&panic::PanicHookInfo<'_>) + Send + Sync>;

    /// Reinstates the process panic hook on drop.
    ///
    /// A test that installs a hook without putting the previous one back leaks it
    /// into every later test in this binary. Restoring from `Drop` means it goes
    /// back even when an assertion in the test body fails.
    struct RestorePanicHook(Option<BoxedHook>);

    impl Drop for RestorePanicHook {
        fn drop(&mut self) {
            if let Some(hook) = self.0.take() {
                let _ = panic::take_hook();
                panic::set_hook(hook);
            }
        }
    }

    /// A restore callback that counts only invocations on `owner`'s thread.
    ///
    /// `PANIC_TEST_LOCK` serialises this module, but it cannot stop a test
    /// *elsewhere* in the binary from panicking — an ordinary assertion failure is
    /// a panic, and it runs through whichever hook is currently installed. Counting
    /// unconditionally therefore made these tests fail whenever some unrelated test
    /// failed, turning one real failure into several and pointing at the wrong one.
    /// Attributing by raising thread keeps the count to this test's own panic.
    fn count_on_owner_thread(
        owner: ThreadId,
        counter: Arc<AtomicUsize>,
    ) -> impl Fn() + Send + Sync + 'static {
        move || {
            if std::thread::current().id() == owner {
                counter.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    #[test]
    fn restore_is_called_on_drop() {
        // Holds the lock even though it never panics: constructing a guard swaps
        // the global hook, which would disturb a concurrent panic-hook test.
        let _lock = lock_panic_hook();
        let count = Arc::new(AtomicUsize::new(0));
        let owner = std::thread::current().id();
        let _guard = TerminalGuard::with_restore(count_on_owner_thread(owner, Arc::clone(&count)));
        drop(_guard);
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn repeated_cleanup_is_safe() {
        let _lock = lock_panic_hook();
        let count = Arc::new(AtomicUsize::new(0));
        let owner = std::thread::current().id();
        let guard = TerminalGuard::with_restore(count_on_owner_thread(owner, Arc::clone(&count)));
        drop(guard);
        // A second guard performs another best-effort cleanup with the same
        // injected restore; this must not panic.
        let guard2 = TerminalGuard::with_restore(count_on_owner_thread(owner, Arc::clone(&count)));
        drop(guard2);
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn panic_hook_chains_and_restores() {
        let _lock = lock_panic_hook();
        let count = Arc::new(AtomicUsize::new(0));
        let owner = std::thread::current().id();
        let _guard = TerminalGuard::with_restore(count_on_owner_thread(owner, Arc::clone(&count)));

        let result = std::panic::catch_unwind(|| {
            panic!("controlled test panic");
        });
        assert!(result.is_err());
        // The hook fires once during this thread's panic.
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn previous_panic_hook_is_restored() {
        let _lock = lock_panic_hook();
        let owner = std::thread::current().id();
        let count = Arc::new(AtomicUsize::new(0));

        // Share the real hook: it is chained to by the counting hook below *and*
        // reinstated afterwards, so it cannot simply be moved into the closure.
        let previous: Arc<dyn Fn(&panic::PanicHookInfo<'_>) + Send + Sync> =
            Arc::from(panic::take_hook());
        let previous_for_restore = Arc::clone(&previous);
        let counted = count_on_owner_thread(owner, Arc::clone(&count));
        panic::set_hook(Box::new(move |info| {
            counted();
            previous(info);
        }));
        // Reinstated when this test returns, pass or fail, so the counting hook
        // does not leak into the rest of the binary.
        let _restore = RestorePanicHook(Some(Box::new(move |info| previous_for_restore(info))));

        let guard_count = Arc::new(AtomicUsize::new(0));
        let guard =
            TerminalGuard::with_restore(count_on_owner_thread(owner, Arc::clone(&guard_count)));
        drop(guard);

        let result = std::panic::catch_unwind(|| {
            panic!("hook restoration test panic");
        });

        assert!(result.is_err());
        // The guard's restore ran exactly once, from its own drop.
        assert_eq!(guard_count.load(Ordering::SeqCst), 1);
        // Dropping the guard put this test's hook back, so it observed the panic.
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn clear_terminal_is_safe() {
        assert!(clear_terminal().is_ok());
    }
}
