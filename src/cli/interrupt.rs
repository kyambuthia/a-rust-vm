//! Ctrl-C handling for the interactive agent.
//!
//! While a turn runs, the first Ctrl-C sets the agent's cancel flag: the
//! running command's process group is killed, a streaming response stops at
//! its next chunk, and the session continues. A second Ctrl-C during the
//! same turn, or any Ctrl-C at the prompt, exits as before.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// Install the handler for the agent's cancel flag. Later calls are ignored.
pub(crate) fn install(cancel: Arc<AtomicBool>) {
    imp::install(cancel);
}

/// Marks a running turn for as long as it lives, after clearing any cancel
/// left over from an earlier turn.
pub(crate) struct TurnGuard(());

impl TurnGuard {
    pub(crate) fn begin(cancel: &AtomicBool) -> Self {
        cancel.store(false, std::sync::atomic::Ordering::Release);
        imp::set_turn_active(true);
        Self(())
    }
}

impl Drop for TurnGuard {
    fn drop(&mut self) {
        imp::set_turn_active(false);
    }
}

#[cfg(unix)]
mod imp {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

    const SIGINT: i32 = 2;
    static TURN_ACTIVE: AtomicBool = AtomicBool::new(false);
    /// The cancel flag, leaked on install so the handler can always read it.
    static CANCEL: AtomicPtr<AtomicBool> = AtomicPtr::new(std::ptr::null_mut());

    unsafe extern "C" {
        fn signal(signal: i32, handler: usize) -> usize;
        fn write(fd: i32, buffer: *const std::ffi::c_void, count: usize) -> isize;
        fn _exit(status: i32) -> !;
    }

    extern "C" fn on_interrupt(_signal: i32) {
        // Only async-signal-safe work here: atomics, write(2), and _exit(2).
        let cancel = CANCEL.load(Ordering::Acquire);
        if TURN_ACTIVE.load(Ordering::Acquire) && !cancel.is_null() {
            // SAFETY: the pointer comes from `Arc::into_raw` and is never
            // released, so it stays valid for the life of the process.
            let first = !unsafe { &*cancel }.swap(true, Ordering::AcqRel);
            if first {
                let message = b"\n[agent] cancelling; press Ctrl-C again to quit\n";
                // SAFETY: the buffer is a valid static byte string.
                unsafe { write(2, message.as_ptr().cast(), message.len()) };
                return;
            }
        }
        // SAFETY: `_exit` takes no pointers and never returns.
        unsafe { _exit(130) }
    }

    pub(super) fn install(cancel: Arc<AtomicBool>) {
        let pointer = Arc::into_raw(cancel) as *mut AtomicBool;
        if CANCEL
            .compare_exchange(
                std::ptr::null_mut(),
                pointer,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            // Already installed: drop the extra reference.
            // SAFETY: `pointer` came from `Arc::into_raw` just above.
            drop(unsafe { Arc::from_raw(pointer) });
            return;
        }
        // SAFETY: `on_interrupt` is an `extern "C" fn(i32)` that only does
        // async-signal-safe work, which is what `signal` requires.
        unsafe { signal(SIGINT, on_interrupt as extern "C" fn(i32) as usize) };
    }

    pub(super) fn set_turn_active(active: bool) {
        TURN_ACTIVE.store(active, Ordering::Release);
    }
}

#[cfg(not(unix))]
mod imp {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    pub(super) fn install(_cancel: Arc<AtomicBool>) {}
    pub(super) fn set_turn_active(_active: bool) {}
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    unsafe extern "C" {
        fn raise(signal: i32) -> i32;
    }

    #[test]
    fn first_interrupt_during_a_turn_cancels_instead_of_exiting() {
        let cancel = Arc::new(AtomicBool::new(true));
        install(cancel.clone());
        let guard = TurnGuard::begin(&cancel);
        assert!(
            !cancel.load(Ordering::Acquire),
            "a new turn clears the flag"
        );
        // SAFETY: raising SIGINT runs the handler installed above, which
        // only sets the flag on the first press of a turn.
        unsafe { raise(2) };
        assert!(cancel.load(Ordering::Acquire));
        drop(guard);
    }
}
