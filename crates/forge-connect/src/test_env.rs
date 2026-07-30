//! Serialised environment access for tests.
//!
//! Environment variables are process-global, and `cargo test` runs a crate's
//! tests in one process across several threads. A test that sets a variable
//! another test reads is therefore a data race, and the failure surfaces far
//! from its cause — typically as an unrelated assertion that only fails
//! sometimes, and often only on a machine with a different core count.
//!
//! `EnvGuard` takes a single crate-wide lock, clears the named variables so the
//! test starts from a known state regardless of the developer's ambient
//! environment, and restores the previous values on drop — including on panic.
//!
//! **There must be exactly one lock per process for this to work.** A
//! module-private lock only serialises the module that declares it, while the
//! variables it protects are process-wide, so a second module mutating the same
//! variable bypasses it completely. That is why this lives here rather than
//! being repeated per module.

use std::sync::{Mutex, MutexGuard, OnceLock};

/// Exclusive, restoring access to a set of environment variables.
///
/// Hold this for as long as the variables must stay stable. Every variable the
/// test reads or writes should be named in `new`, so it is both cleared up front
/// and restored afterwards.
pub(crate) struct EnvGuard {
    _lock: MutexGuard<'static, ()>,
    saved: Vec<(String, Option<String>)>,
}

impl EnvGuard {
    /// Lock the environment, clear `keys`, and remember their prior values.
    pub(crate) fn new(keys: &[&str]) -> Self {
        static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        // Recover from poisoning rather than propagating it: one panicking test
        // must not cascade into every other test that touches the environment.
        let lock = ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let saved = keys
            .iter()
            .map(|key| {
                let previous = std::env::var(key).ok();
                std::env::remove_var(key);
                (key.to_string(), previous)
            })
            .collect();
        Self { _lock: lock, saved }
    }

    /// Set one of the guarded variables for the lifetime of the guard.
    pub(crate) fn set(&self, key: &str, value: &str) {
        debug_assert!(
            self.saved.iter().any(|(k, _)| k == key),
            "`{key}` must be named in EnvGuard::new so it is restored on drop"
        );
        std::env::set_var(key, value);
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, previous) in &self.saved {
            match previous {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A distinct key per test. Sharing one would mean each test seeding it before
    // acquiring the lock, which is precisely the race this guard exists to prevent —
    // and it did fail that way under an 8-thread run before being split.
    const RESTORE_KEY: &str = "FORGE_TEST_ENV_GUARD_RESTORE";
    const ABSENT_KEY: &str = "FORGE_TEST_ENV_GUARD_ABSENT";

    #[test]
    fn clears_on_entry_and_restores_on_drop() {
        const KEY: &str = RESTORE_KEY;
        std::env::set_var(KEY, "outer");
        {
            let guard = EnvGuard::new(&[KEY]);
            assert!(
                std::env::var(KEY).is_err(),
                "guard must clear the variable on entry"
            );
            guard.set(KEY, "inner");
            assert_eq!(std::env::var(KEY).unwrap(), "inner");
        }
        assert_eq!(
            std::env::var(KEY).unwrap(),
            "outer",
            "guard must restore the prior value on drop"
        );
        std::env::remove_var(KEY);
    }

    #[test]
    fn restores_absence_when_the_variable_was_unset() {
        const KEY: &str = ABSENT_KEY;
        std::env::remove_var(KEY);
        {
            let guard = EnvGuard::new(&[KEY]);
            guard.set(KEY, "temporary");
        }
        assert!(
            std::env::var(KEY).is_err(),
            "a variable that was unset must be unset again after drop"
        );
    }
}
