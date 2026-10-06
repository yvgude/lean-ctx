//! User-initiated cooperative cancellation (Ctrl-C / SIGINT).
//!
//! Long-running CLI builds (notably dense embedding via CUDA) spend most of
//! their time inside ONNX Runtime FFI (`session.run()`). If the default SIGINT
//! disposition kills the process mid-kernel, the CUDA context is never torn
//! down cleanly: the process lingers as a zombie and its VRAM stays allocated
//! until the driver reclaims it (observed under WSL2 GPU passthrough).
//!
//! This module installs a *cooperative* SIGINT handler: the first Ctrl-C only
//! flips a global flag. Cancellable loops poll [`is_cancelled`] **between** FFI
//! calls and return early, so control is back in Rust code — not inside a CUDA
//! kernel — when the process exits. That lets the driver reclaim VRAM on a
//! clean exit. A second Ctrl-C forces an immediate `_exit` for the impatient.
//!
//! The handler body only performs async-signal-safe operations (atomic stores
//! and, on the second signal, `libc::_exit`).

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

static CANCELLED: AtomicBool = AtomicBool::new(false);
static HANDLER_INSTALLED: AtomicBool = AtomicBool::new(false);
static SIGNAL_COUNT: AtomicU8 = AtomicU8::new(0);

/// `true` once the user has requested cancellation via Ctrl-C.
pub fn is_cancelled() -> bool {
    CANCELLED.load(Ordering::Relaxed)
}

/// Clear the cancellation state before starting a fresh cancellable operation.
pub fn reset() {
    CANCELLED.store(false, Ordering::SeqCst);
    SIGNAL_COUNT.store(0, Ordering::SeqCst);
}

#[cfg(unix)]
extern "C" fn handle_sigint(_sig: libc::c_int) {
    CANCELLED.store(true, Ordering::SeqCst);
    let count = SIGNAL_COUNT
        .fetch_add(1, Ordering::SeqCst)
        .saturating_add(1);
    if count >= 2 {
        // Second Ctrl-C: the user wants out now. `_exit` is async-signal-safe
        // and terminates the process without running (unsafe-in-a-handler)
        // destructors; the OS/driver reclaims the CUDA context on death.
        // SAFETY: `_exit` is async-signal-safe (POSIX.1-2017 §2.4.3) and does
        // not run C++ destructors, Rust `Drop` impls, or atexit handlers.
        // We call it only after the second SIGINT, where graceful shutdown has
        // already been requested and the user explicitly wants immediate exit.
        unsafe { libc::_exit(130) };
    }
}

/// Install the cooperative SIGINT handler (idempotent).
///
/// Call this at the start of a long-running, cancellable CLI operation. The
/// daemon/MCP server never calls it, so [`is_cancelled`] stays `false` there
/// and background embedding is unaffected.
pub fn install_ctrlc_handler() {
    #[cfg(unix)]
    if HANDLER_INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    #[cfg(not(unix))]
    let _ = HANDLER_INSTALLED.swap(true, Ordering::SeqCst);
    // SAFETY: `handle_sigint` only performs async-signal-safe work (atomic
    // stores, and `libc::_exit` on the second signal). Registering it replaces
    // the default terminate-immediately disposition with a cooperative one.
    #[cfg(unix)]
    unsafe {
        libc::signal(
            libc::SIGINT,
            handle_sigint as *const () as libc::sighandler_t,
        );
    }
}
