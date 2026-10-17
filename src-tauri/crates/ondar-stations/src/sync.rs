//! The one place a `Mutex` is locked.

use std::sync::{Mutex, MutexGuard};

/// Lock `m`, propagating poison as a panic: `.claude/rules/rust.md`'s one exemption from
/// `clippy::unwrap_used`. A poisoned lock means the thread that held it has already panicked; the
/// release profile (`panic = "abort"`) ended the process before anyone could observe it, and a
/// debug build propagates the failure here instead of continuing on half-updated state. The lint
/// refuses a bare `.lock().unwrap()` anywhere else in the crate.
#[allow(
    clippy::unwrap_used,
    reason = "poison propagation: the holder already panicked; release aborts"
)]
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap()
}
