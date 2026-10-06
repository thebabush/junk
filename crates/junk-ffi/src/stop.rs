//! The button that ends a live session.

use std::sync::Arc;

use tokio::sync::watch;

/// A stop button for [`live`](crate::live): the app holds one and presses it from a button.
///
/// **Stopping is not an abort.** [`Stop::stop`] ends the *stream*; the session then does
/// what it always does — stops the workout on the ring, waits for the ring to say the
/// record is stored, and fetches that record with its heart rates — so a stopped `live`
/// returns a [`LiveResult`](crate::LiveResult) like any other, and the wearer's workout is
/// properly closed rather than left running. Dropping the whole call instead is what
/// abandons a workout on the ring.
///
/// Pressing it before or during the call both work: a `Stop` already stopped when `live`
/// begins makes the stream end at once, and the session goes straight to stopping the
/// workout. Pressing it twice is pressing it once. One `Stop` belongs to one `live`.
#[derive(uniffi::Object)]
pub struct Stop {
    /// `true` once the button has been pressed. A watch rather than a `Notify` so that a
    /// press before anything waits is still seen: the value is there to be read.
    pressed: watch::Sender<bool>,
}

#[uniffi::export]
impl Stop {
    /// A button that has not been pressed.
    #[must_use]
    #[uniffi::constructor]
    pub fn new() -> Arc<Stop> {
        Arc::new(Stop {
            pressed: watch::channel(false).0,
        })
    }

    /// Ends the live stream, now or as soon as it starts.
    ///
    /// The session still stops the workout on the ring and fetches its record: see the
    /// [type's own documentation](Stop). Safe to call from any thread, and from outside the
    /// session's own task — that is the point of it.
    pub fn stop(&self) {
        self.pressed.send_replace(true);
    }
}

impl Stop {
    /// The future [`junk_app::live`] takes as its `until`: it finishes once [`Stop::stop`]
    /// has been called, whether that was before this was asked for or long after.
    pub fn pressed(&self) -> impl Future<Output = ()> + Send + use<> {
        let mut pressed = self.pressed.subscribe();
        async move {
            // The only sender lives in the `Stop` the caller holds, so the error case is
            // a `Stop` dropped mid-session: nothing left to wait for either way.
            let _ = pressed.wait_for(|pressed| *pressed).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_press_is_seen_before_the_wait_and_after_it() {
        // Pressed first: the wait is already over when it is asked for.
        let stop = Stop::new();
        stop.stop();
        stop.pressed().await;

        // Pressed twice: still just stopped.
        stop.stop();
        stop.pressed().await;

        // Pressed later: the wait was there first.
        let stop = Stop::new();
        let waiting = stop.pressed();
        let pressing = Arc::clone(&stop);
        let press = tokio::spawn(async move { pressing.stop() });
        waiting.await;
        press.await.expect("the press did not panic");
    }

    #[tokio::test]
    async fn a_button_nobody_presses_never_finishes() {
        let stop = Stop::new();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), stop.pressed())
                .await
                .is_err()
        );
    }
}
