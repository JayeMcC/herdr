//! One process-global lock for the process-global environment.
//!
//! Rust runs a crate's tests as threads in a SINGLE process, and
//! `std::env::set_var` mutates state shared by every one of them. Nine
//! modules in this crate independently grew a `fn env_lock()` around their
//! own `set_var` calls. Each one is module-private, so each serialises its
//! own tests against each other and against NOTHING ELSE -- while all nine
//! write the same `XDG_CONFIG_HOME`, `XDG_STATE_HOME` and
//! `HERDR_CONFIG_PATH`. Two tests in two different modules would take two
//! different mutexes, both believe themselves exclusive, and interleave.
//!
//! The symptom was a suite that could not answer "did my change break
//! something": the failing SET shifted between identical runs of an
//! unchanged tree, and every failing test passed when run alone.
//!
//! So the lock belongs to the resource, not to the module. Everything that
//! touches the process environment in tests takes THIS lock.
//!
//! Prefer [`ScopedEnv`] over a bare guard: it restores every variable it set
//! when it drops, including on a panic, so a failing test cannot leak its
//! environment into whatever runs next.

use std::ffi::{OsStr, OsString};
use std::sync::{Mutex, MutexGuard, OnceLock};

/// The one mutex guarding the process environment. Every module-private
/// `env_lock()` in this crate now returns THIS, so two tests in two different
/// modules can no longer each hold their own lock and interleave.
///
/// Poison is cleared on the way out. Callers overwhelmingly write
/// `env_lock().lock().unwrap()`, which on a poisoned mutex panics with
/// `PoisonError` -- so ONE test that fails while holding the lock converts
/// every later test into a second, fake failure and buries the original
/// cause. Poisoning tells us nothing useful here: the guarded data is `()`,
/// and each test installs the environment it needs rather than inheriting
/// it. Clearing the flag keeps a real failure to ONE reported failure.
pub(crate) fn env_mutex() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let mutex = LOCK.get_or_init(|| Mutex::new(()));
    if mutex.is_poisoned() {
        mutex.clear_poison();
    }
    mutex
}

/// Takes the lock, recovering rather than propagating poison: one test
/// panicking while holding it must not convert every later test into a
/// second failure and bury the original.
pub(crate) fn env_lock() -> MutexGuard<'static, ()> {
    env_mutex()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Holds the env lock and remembers the previous value of every variable it
/// touched, restoring them on drop.
pub(crate) struct ScopedEnv {
    _guard: MutexGuard<'static, ()>,
    saved: Vec<(OsString, Option<OsString>)>,
}

impl ScopedEnv {
    /// Takes the process-global env lock. Blocks until no other test holds it.
    pub(crate) fn new() -> Self {
        Self {
            _guard: env_lock(),
            saved: Vec::new(),
        }
    }

    /// Sets a variable for the lifetime of this guard.
    pub(crate) fn set(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
        let key = key.as_ref().to_os_string();
        self.remember(&key);
        // Safe only because the env lock is held: no sibling test is
        // reading or writing the environment concurrently.
        std::env::set_var(&key, value);
        self
    }

    /// Removes a variable for the lifetime of this guard.
    pub(crate) fn remove(&mut self, key: impl AsRef<OsStr>) -> &mut Self {
        let key = key.as_ref().to_os_string();
        self.remember(&key);
        // Safe only because the env lock is held, as above.
        std::env::remove_var(&key);
        self
    }

    /// Records the ORIGINAL value only. Setting the same key twice inside one
    /// scope must still restore what the process had before the scope opened.
    fn remember(&mut self, key: &OsString) {
        if self.saved.iter().any(|(seen, _)| seen == key) {
            return;
        }
        self.saved.push((key.clone(), std::env::var_os(key)));
    }
}

impl Drop for ScopedEnv {
    fn drop(&mut self) {
        // Reverse order, so a key set twice ends on its original value.
        for (key, previous) in self.saved.drain(..).rev() {
            // The guard still holds the env lock until it drops.
            match previous {
                Some(value) => std::env::set_var(&key, value),
                None => std::env::remove_var(&key),
            }
        }
    }
}
