//! What a session reports as it runs, and who it reports to.
//!
//! [`junk_app::Progress`] borrows the driver's own types, which do not cross the boundary;
//! this is the same story told in strings and numbers. The words are `junk-app`'s
//! ([`describe_req`](junk_app::describe_req), [`summary`](junk_app::summary),
//! [`describe_event`](junk_app::describe_event)), so a phone shows exactly what the CLI
//! prints — all that differs is where the line goes.

use std::panic::{self, AssertUnwindSafe};

use junk_app::{channel_label, describe_event, describe_req, summary};
use junk_colmi::proto::Ev;

use crate::records::Bpm;

/// One step of a session, as it happens.
#[derive(uniffi::Enum)]
pub enum Progress {
    /// The link came up.
    Connected {
        /// Which of the ring's declared channels resolved, by their trace names.
        channels: Vec<String>,
        /// The negotiated ATT MTU, in bytes.
        mtu: u16,
    },
    /// A request is about to go to the ring.
    Asking {
        /// What is being asked, in a few words: `battery`, `hr log 2026-09-06`.
        what: String,
    },
    /// The ring answered it.
    Answered {
        /// What was asked.
        what: String,
        /// What came back, in one line: `736 samples`, `84%, not charging`.
        summary: String,
    },
    /// The ring refused it. The session goes on, and the refusal is in
    /// [`SyncResult::failures`](crate::SyncResult::failures) at the end.
    Failed {
        /// What was asked.
        what: String,
        /// Why not.
        why: String,
    },
    /// Something the ring said on its own, in one line. A live workout sample is a
    /// [`Progress::WorkoutSample`] instead; everything else the ring volunteers is here.
    Event {
        /// What it said.
        text: String,
    },
    /// One heart rate from a running workout: what a live view plots.
    ///
    /// The sequence number repeats every ten frames or so and the ring resends, so a view
    /// that plots each sample once dedupes on it.
    WorkoutSample {
        /// The ring's sequence number for this sample.
        seq: u8,
        /// The reading, or [`Bpm::Invalid`] while the sensor has none yet.
        bpm: Bpm,
    },
    /// A write, a subscription or a read did not go through. The session goes on: the
    /// driver's own timeout fails whatever request it belonged to.
    LinkError {
        /// What went wrong.
        why: String,
    },
    /// The ring did not say the workout record was stored; it is fetched anyway.
    WorkoutStoreTimeout,
    /// The [`Stop`](crate::Stop) was pressed and the live stream ended early. The workout
    /// is still stopped on the ring and its record still fetched.
    Interrupted,
    /// The link is down; the session is over.
    Disconnected,
}

impl From<&junk_app::Progress<'_>> for Progress {
    fn from(progress: &junk_app::Progress<'_>) -> Progress {
        match progress {
            junk_app::Progress::Connected { resolved, mtu } => Progress::Connected {
                channels: resolved.into_iter().map(channel_label).collect(),
                mtu: *mtu,
            },
            junk_app::Progress::Asking(req) => Progress::Asking {
                what: describe_req(req),
            },
            junk_app::Progress::Answered { req, resp } => Progress::Answered {
                what: describe_req(req),
                summary: summary(resp),
            },
            junk_app::Progress::Failed { req, err } => Progress::Failed {
                what: describe_req(req),
                why: err.to_string(),
            },
            junk_app::Progress::Event(Ev::Workout { seq, bpm, .. }) => Progress::WorkoutSample {
                seq: *seq,
                bpm: Bpm::from(*bpm),
            },
            junk_app::Progress::Event(ev) => Progress::Event {
                text: describe_event(ev),
            },
            junk_app::Progress::LinkError(err) => Progress::LinkError {
                why: err.to_string(),
            },
            junk_app::Progress::WorkoutStoreTimeout => Progress::WorkoutStoreTimeout,
            junk_app::Progress::Interrupted => Progress::Interrupted,
            junk_app::Progress::Disconnected => Progress::Disconnected,
        }
    }
}

/// Where a session's steps go: the app implements this and watches its ring work.
///
/// [`Observer::step`] is called from the session's own task, between the ring's answers, so
/// **it must not block**: anything slow (a database write, a redraw that waits) holds up
/// the session itself. Hand the value on and return.
///
/// Every call is on the same task, one at a time and in order, so an implementation needs
/// no locking of its own beyond what its own state wants.
#[uniffi::export(with_foreign)]
pub trait Observer: Send + Sync {
    /// One step of the session.
    fn step(&self, progress: Progress);
}

/// Hands one step to `observer`, and does not let it end the session.
///
/// uniffi 0.32 catches panics where Rust is *entered*: every scaffolding call runs inside
/// `rust_call_with_out_status`'s `catch_unwind`, and an exported async function is polled
/// inside it too, so a panic in here can never unwind into Swift — it becomes an
/// `UnexpectedError` on the call. That is exactly the trouble. A `step` the Swift side
/// fails in a way the binding cannot carry comes back as a non-success call status, and
/// uniffi's own `LiftReturn::handle_callback_unexpected_error` panics on it by default
/// (`Observer::step` returns nothing, so there is no error type to convert it into). That
/// panic unwinds up through the session script, and uniffi then never polls the future
/// again: a whole sync lost to one bad callback, with everything the ring already answered
/// thrown away with it. Catching it here costs a step nobody saw and keeps the session.
pub(crate) fn report(observer: &dyn Observer, progress: &junk_app::Progress<'_>) {
    let progress = Progress::from(progress);
    let _ = panic::catch_unwind(AssertUnwindSafe(|| observer.step(progress)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use junk_colmi::proto::{Req, Resp};
    use junk_colmi::wire::Notification;
    use junk_core::{Channel, ChannelSet, LinkError, ProtoError};
    use std::sync::Mutex;

    /// An observer that keeps what it was handed.
    #[derive(Default)]
    struct Seen(Mutex<Vec<Progress>>);

    impl Observer for Seen {
        fn step(&self, progress: Progress) {
            self.0.lock().expect("not poisoned").push(progress);
        }
    }

    /// An observer that panics, as a foreign one whose error could not be carried does.
    struct Panics;

    impl Observer for Panics {
        fn step(&self, _progress: Progress) {
            panic!("the app's callback blew up");
        }
    }

    #[test]
    fn every_step_crosses_as_words_and_numbers() {
        let mut resolved = ChannelSet::EMPTY;
        resolved.insert(junk_colmi::V1_WRITE);
        resolved.insert(Channel(200));
        let req = Req::Battery;
        let resp = Resp::Ack;
        let err = LinkError::NotConnected;
        let ev = Ev::Notification(Notification::WorkoutStored);
        let sample = Ev::Workout {
            sport_type: 7,
            flag: 1,
            seq: 4,
            bpm: junk_colmi::Bpm::Valid(58),
        };

        let steps: Vec<Progress> = [
            junk_app::Progress::Connected {
                resolved: &resolved,
                mtu: 247,
            },
            junk_app::Progress::Asking(&req),
            junk_app::Progress::Answered {
                req: &req,
                resp: &resp,
            },
            junk_app::Progress::Failed {
                req: &req,
                err: ProtoError::Unsupported("spo2"),
            },
            junk_app::Progress::Event(&ev),
            junk_app::Progress::Event(&sample),
            junk_app::Progress::LinkError(&err),
            junk_app::Progress::WorkoutStoreTimeout,
            junk_app::Progress::Interrupted,
            junk_app::Progress::Disconnected,
        ]
        .iter()
        .map(Progress::from)
        .collect();

        let [
            connected,
            asking,
            answered,
            failed,
            event,
            workout,
            link_error,
            timeout,
            interrupted,
            disconnected,
        ] = &steps[..]
        else {
            panic!("one step each, in order: {}", steps.len());
        };
        assert!(matches!(
            connected,
            Progress::Connected { channels, mtu: 247 } if channels == &["v1.write", "unknown.200"]
        ));
        assert!(matches!(asking, Progress::Asking { what } if what == "battery"));
        assert!(
            matches!(answered, Progress::Answered { what, summary } if what == "battery" && summary == "ack")
        );
        assert!(
            matches!(failed, Progress::Failed { what, why } if what == "battery" && *why == ProtoError::Unsupported("spo2").to_string())
        );
        assert!(
            matches!(event, Progress::Event { text } if text == "notification: workout stored")
        );
        assert!(matches!(
            workout,
            Progress::WorkoutSample {
                seq: 4,
                bpm: Bpm::Valid { bpm: 58 }
            }
        ));
        assert!(
            matches!(link_error, Progress::LinkError { why } if *why == LinkError::NotConnected.to_string())
        );
        assert!(matches!(timeout, Progress::WorkoutStoreTimeout));
        assert!(matches!(interrupted, Progress::Interrupted));
        assert!(matches!(disconnected, Progress::Disconnected));
    }

    #[test]
    fn an_observer_that_panics_does_not_end_the_session() {
        let req = Req::Battery;
        report(&Panics, &junk_app::Progress::Asking(&req));

        // And a working one still gets what it was handed.
        let seen = Seen::default();
        report(&seen, &junk_app::Progress::Interrupted);
        let seen = seen.0.into_inner().expect("not poisoned");
        assert!(matches!(seen[..], [Progress::Interrupted]));
    }
}
