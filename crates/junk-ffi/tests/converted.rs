//! The two recorded sessions, driven offline and converted the way the boundary converts
//! them: what a phone would actually be handed for the ring this stack was built against.
//!
//! `crates/junk-app/tests/recorded.rs` proves the *session* replays write for write; this
//! proves the numbers survive the crossing — that the counts are the counts, that a time
//! crosses both as the ring's own wall clock and as the instant it was, and that the ring's
//! capability bitmap becomes three plain flags.
//!
//! No hardware: a [`TraceLink`] serves a captured trace back.

use std::fs;
use std::sync::Mutex;

use chrono::{FixedOffset, TimeZone};
use junk_app::Clock;
use junk_ffi::{Bpm, LiveResult, Observer, Progress, Stop, SyncResult};
use junk_pump::trace_link::TraceLink;
use junk_trace::Trace;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/colmi-r10");

/// The zone both fixtures were recorded in: a UTC-4 zone in summer.
const TZ_MIN: i32 = -4 * 60;

/// The trace `name`, served back as a link.
fn link(name: &str) -> TraceLink {
    let text = fs::read_to_string(format!("{FIXTURES}/{name}")).expect("the fixture is readable");
    let trace = Trace::parse(&text).unwrap_or_else(|err| panic!("{err}"));
    TraceLink::new(&trace, junk_colmi::channel_by_name)
}

/// The wall clock the sync fixture ran on: 2026-09-06 02:10:24 in its zone. The set-time
/// frame the trace recorded carries it, and the writes only match if it is this one.
fn clock() -> Clock {
    let at = FixedOffset::east_opt(TZ_MIN * 60)
        .expect("in range")
        .with_ymd_and_hms(2026, 9, 6, 2, 10, 24)
        .single()
        .expect("unambiguous");
    Clock::at(&at)
}

/// An [`Observer`] that keeps every step it was handed, as a phone's would not but a test
/// wants to.
#[derive(Default)]
struct Watched(Mutex<Vec<Progress>>);

impl Observer for Watched {
    fn step(&self, progress: Progress) {
        self.0.lock().expect("not poisoned").push(progress);
    }
}

impl Watched {
    /// What it saw, in order.
    fn steps(&self) -> std::sync::MutexGuard<'_, Vec<Progress>> {
        self.0.lock().expect("not poisoned")
    }
}

#[tokio::test]
async fn the_recorded_sync_crosses_the_boundary_whole() {
    let watched = Watched::default();
    let run = junk_app::sync(
        link("junk-sync-2026-09-06.trace"),
        clock(),
        7,
        std::future::pending(),
        |progress| watched.step(Progress::from(&progress)),
    )
    .await;
    run.result.unwrap_or_else(|err| panic!("{err:#}"));
    let result = SyncResult::from(&run.session);

    // Every kind, in the numbers the ring gave.
    // Every slot of every day's log up to its last reading, the empty ones `Invalid`.
    assert_eq!(result.hr.len(), 884);
    assert_eq!(
        result
            .hr
            .iter()
            .filter(|sample| matches!(sample.bpm, Bpm::Valid { .. }))
            .count(),
        736,
        "the readings the ring actually took"
    );
    assert_eq!(result.steps.len(), 41);
    assert_eq!(result.stress.len(), 123);
    assert_eq!(result.hrv.len(), 62);
    assert_eq!(result.spo2.len(), 50);
    assert_eq!(result.temperature.len(), 5);
    assert_eq!(result.sleep.len(), 1);
    assert_eq!(result.sleep[0].stages.len(), 24);
    assert_eq!(result.workouts.len(), 10);
    // The ring refused nothing; a refusal would be here, not an error.
    assert!(result.failures.is_empty());

    // A time crosses twice over: the wall clock the CSVs carry, and the instant it was.
    // Midnight of 2026-09-06 in UTC-4 is 2026-09-06T04:00:00Z.
    let first = &result.hr[0];
    assert_eq!(first.at, "2026-09-06T00:00:00.000-04:00");
    assert_eq!(first.at_utc_ms, 1_788_667_200_000);

    let device = result.device;
    assert_eq!(device.firmware.as_deref(), Some("RT03CR_1.00.02_260319"));
    assert_eq!(device.hardware.as_deref(), Some("RT03CR_V1.0"));
    assert_eq!(device.battery_percent, Some(84));
    assert_eq!(device.charging, Some(false));
    assert_eq!(device.mtu, Some(247));
    // The `01` ack's bitmap, as three plain flags.
    assert!(device.temperature);
    assert!(device.spo2);
    assert!(device.new_sleep_protocol);

    // The link came up on the six channels the trace names, and the ring's two volunteered
    // events crossed as text; nothing went wrong.
    let steps = watched.steps();
    let connected = steps
        .iter()
        .find_map(|step| match step {
            Progress::Connected { channels, mtu } => Some((channels.len(), *mtu)),
            _ => None,
        })
        .expect("the link came up");
    assert_eq!(connected, (6, 247));
    let events: Vec<&str> = steps
        .iter()
        .filter_map(|step| match step {
            Progress::Event { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        events,
        [
            "packet size: 244",
            "capabilities: temperature yes, spo2 yes, new sleep protocol yes",
        ]
    );
    assert!(
        !steps.iter().any(|step| matches!(
            step,
            Progress::Failed { .. }
                | Progress::LinkError { .. }
                | Progress::WorkoutStoreTimeout
                | Progress::Interrupted
        )),
        "a recorded session had none of these"
    );
}

#[tokio::test]
async fn the_recorded_live_session_crosses_with_its_workout() {
    let watched = Watched::default();
    // A `Stop` nobody presses, and zero seconds: the stream ends at once and the script
    // still stops the workout and fetches what the ring stored.
    let stop = Stop::new();
    let run = junk_app::live(
        link("junk-live-2026-09-06.trace"),
        7,
        0,
        stop.pressed(),
        |progress| watched.step(Progress::from(&progress)),
    )
    .await;
    run.result.unwrap_or_else(|err| panic!("{err:#}"));
    let result = LiveResult::from(&run.session);

    let workout = result.workout.expect("the ring stored the workout");
    assert_eq!(workout.sample_seconds, 1);
    assert_eq!(workout.heart_rates.len(), 65);
    assert!(
        workout
            .heart_rates
            .iter()
            .all(|bpm| matches!(bpm, Bpm::Valid { bpm } if (56..=60).contains(bpm))),
        "{:?}",
        workout.heart_rates
    );
    assert_eq!(workout.record.sport_type, 7);
    // The `77 01` ack's own timestamp, on the ring's clock.
    assert_eq!(workout.record.start_ring, 1_788_632_166);
    assert_eq!(workout.record.duration_s, Some(75));

    // The list the record came from is a sample too, and the session had no failures.
    assert_eq!(result.session.workouts.len(), 1);
    assert!(result.session.failures.is_empty());

    // The stream reaches a live view as plain `seq`/`bpm` pairs, one per frame the trace
    // has, duplicates included: deduping on `seq` is the view's own business.
    let steps = watched.steps();
    let samples: Vec<(u8, Bpm)> = steps
        .iter()
        .filter_map(|step| match step {
            Progress::WorkoutSample { seq, bpm } => Some((*seq, *bpm)),
            _ => None,
        })
        .collect();
    assert_eq!(samples.len(), 83);
    // The ring streams no reading until it has one — eleven frames here — and 56..=60
    // after that, which is the range the record it stores covers.
    assert_eq!(
        samples
            .iter()
            .filter(|(_, bpm)| *bpm == Bpm::Invalid)
            .count(),
        12
    );
    assert!(
        samples.iter().all(|(_, bpm)| match bpm {
            Bpm::Invalid => true,
            Bpm::Valid { bpm } => (56..=60).contains(bpm),
        }),
        "{samples:?}"
    );
    // The sequence numbers are the ring's own, and it resends: 76 of them over 83 frames.
    assert_eq!(samples.first(), Some(&(0, Bpm::Invalid)));
    assert_eq!(samples.last(), Some(&(75, Bpm::Valid { bpm: 56 })));
    // A workout frame is a sample and nothing else; the ring's other news is still text.
    assert!(
        !steps
            .iter()
            .any(|step| matches!(step, Progress::Event { text } if text.starts_with("workout: ")))
    );
    assert!(steps.iter().any(
        |step| matches!(step, Progress::Event { text } if text == "notification: workout stored")
    ));
}

#[tokio::test]
async fn a_stop_pressed_before_the_call_interrupts_the_stream_and_still_fetches() {
    let watched = Watched::default();
    let stop = Stop::new();
    stop.stop();
    // A minute of streaming asked for, so the deadline is not what ends it: only the
    // already-pressed `Stop` can.
    let run = junk_app::live(
        link("junk-live-2026-09-06.trace"),
        7,
        60,
        stop.pressed(),
        |progress| watched.step(Progress::from(&progress)),
    )
    .await;
    run.result.unwrap_or_else(|err| panic!("{err:#}"));
    let result = LiveResult::from(&run.session);

    let steps = watched.steps();
    assert!(
        steps
            .iter()
            .any(|step| matches!(step, Progress::Interrupted)),
        "the stream said it was cut short"
    );
    // Interrupted is not aborted: the workout was still stopped on the ring and fetched.
    let workout = result.workout.expect("the record was fetched anyway");
    assert_eq!(workout.heart_rates.len(), 65);
    assert!(
        steps.iter().any(
            |step| matches!(step, Progress::Answered { what, .. } if what == "workout stop sport 7")
        ),
        "the workout was stopped: {:?}",
        steps
            .iter()
            .filter_map(|step| match step {
                Progress::Answered { what, .. } => Some(what.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
    );
}
