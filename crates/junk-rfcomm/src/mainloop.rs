//! Turning the main run loop: the one thing a host of this link has to do for itself.
//!
//! `IOBluetooth` schedules an RFCOMM channel's `NSStream`s on `[NSRunLoop mainRunLoop]`, so
//! a process only ever hears from the channel while its **main** thread is turning that run
//! loop. That is not a thing a library can do on a host's behalf — nobody but the host owns
//! its main thread — so this module is the two lines of it a host would otherwise copy out
//! of `examples/probe.rs`.
//!
//! # The recipe
//!
//! Put the async work on a second thread and leave the main thread here:
//!
//! ```no_run
//! # use std::sync::atomic::{AtomicBool, Ordering};
//! # use std::sync::Arc;
//! let done = Arc::new(AtomicBool::new(false));
//! let flag = Arc::clone(&done);
//! let worker = std::thread::spawn(move || {
//!     let runtime = tokio::runtime::Builder::new_current_thread()
//!         .enable_all()
//!         .build()
//!         .expect("a tokio runtime");
//!     // runtime.block_on(async { … the pump, over an RfcommLink … });
//!     flag.store(true, Ordering::Release);
//! });
//! junk_rfcomm::turn_main_loop(|| done.load(Ordering::Acquire));
//! worker.join().expect("the worker thread");
//! ```
//!
//! A shell whose main thread has nothing else to do can call
//! [`run_main_loop_forever`] instead and let the worker thread end the process.
//!
//! Both must be called **on the main thread**: `NSRunLoop currentRunLoop` is the main run
//! loop only there, and Apple forbids running another thread's. Called anywhere else they
//! turn the wrong run loop, and the channel still never opens.

use std::time::Duration;

/// How long one turn waits when the run loop has nothing to run.
///
/// It only bounds how often `until` is asked: a turn returns as soon as a source fires, so
/// nothing the speaker says waits on it.
pub const TURN: Duration = Duration::from_millis(50);

/// Turns the main run loop until `until` answers `true`, asking it once per [`TURN`] at
/// worst.
///
/// Call it on the main thread, and nowhere else; see the [module docs](self).
pub fn turn_main_loop(until: impl Fn() -> bool) {
    while !until() {
        turn();
    }
}

/// Turns the main run loop and never returns.
///
/// For a shell whose main thread has nothing else to do and whose worker thread ends the
/// process. Call it on the main thread, and nowhere else; see the [module docs](self).
pub fn run_main_loop_forever() -> ! {
    loop {
        turn();
    }
}

/// One turn of this thread's run loop, at most [`TURN`] long.
#[cfg(target_os = "macos")]
fn turn() {
    use objc2_foundation::{NSDate, NSDefaultRunLoopMode, NSRunLoop};

    // SAFETY: a Foundation constant, initialised before any Rust code runs.
    let mode = unsafe { NSDefaultRunLoopMode };
    let until = NSDate::dateWithTimeIntervalSinceNow(TURN.as_secs_f64());
    if !NSRunLoop::currentRunLoop().runMode_beforeDate(mode, &until) {
        // No input source at all, so `runMode:` returned at once: sleep the slice rather
        // than spin. IOBluetooth attaches its own source as soon as it has one.
        std::thread::sleep(TURN);
    }
}

/// There is no run loop to turn where there is no `IOBluetooth`; [`RfcommLink`](crate::RfcommLink)
/// reports [`RfcommError::Unsupported`](crate::RfcommError::Unsupported) soon enough, and a
/// host written to this recipe still works when it is ported.
#[cfg(not(target_os = "macos"))]
fn turn() {
    std::thread::sleep(TURN);
}
