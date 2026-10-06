//! The two scripts over the sessions this stack recorded against a real ring, served back
//! by a `TraceLink`: the same requests, in the same order, and the same answers, offline.

use std::fmt::Write as _;
use std::fs;

use chrono::{FixedOffset, TimeZone};
use junk_app::{Clock, Progress, Session, Workout};
use junk_colmi::Bpm;
use junk_colmi::channel_by_name;
use junk_colmi::proto::Ev;
use junk_colmi::wire::WorkoutTag;
use junk_core::{Channel, Percent};
use junk_pump::trace_link::TraceLink;
use junk_trace::Trace;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/colmi-r10");

/// The zone both fixtures were recorded in: a UTC-4 zone in summer.
const TZ_MIN: i32 = -4 * 60;

fn link(name: &str) -> TraceLink {
    let text = fs::read_to_string(format!("{FIXTURES}/{name}")).expect("the fixture is readable");
    let trace = Trace::parse(&text).unwrap_or_else(|err| panic!("{err}"));
    TraceLink::new(&trace, channel_by_name)
}

/// The wall clock at `hour:minute:second` on 2026-09-06 in the fixtures' zone.
fn clock(hour: u32, minute: u32, second: u32) -> Clock {
    let at = FixedOffset::east_opt(TZ_MIN * 60)
        .expect("in range")
        .with_ymd_and_hms(2026, 9, 6, hour, minute, second)
        .single()
        .expect("unambiguous");
    Clock::at(&at)
}

/// What a run reported, in order, as short strings: enough to tell a failure from an
/// answer without repeating the whole protocol.
#[derive(Default)]
struct Reported {
    failed: Vec<String>,
    events: Vec<String>,
    connected: Option<(usize, u16)>,
    /// Every link failure, store timeout and interruption: a recorded session had none, so
    /// these must stay empty.
    troubles: Vec<String>,
}

impl Reported {
    fn observe(&mut self, progress: &Progress<'_>) {
        match progress {
            Progress::Connected { resolved, mtu } => self.connected = Some((resolved.len(), *mtu)),
            Progress::Failed { req, err } => self.failed.push(format!("{req:?}: {err}")),
            Progress::Event(ev) => self.events.push(event_kind(ev).to_owned()),
            Progress::LinkError(err) => self.troubles.push(format!("link error: {err}")),
            Progress::WorkoutStoreTimeout => self.troubles.push("store timeout".to_owned()),
            Progress::Interrupted => self.troubles.push("interrupted".to_owned()),
            Progress::Asking(_) | Progress::Answered { .. } | Progress::Disconnected => {}
        }
    }
}

/// The name of an event's kind, for tallies.
fn event_kind(ev: &Ev) -> &'static str {
    match ev {
        Ev::Capabilities(_) => "Capabilities",
        Ev::PacketSize(_) => "PacketSize",
        Ev::Notification(_) => "Notification",
        Ev::Battery(_) => "Battery",
        Ev::Workout { .. } => "Workout",
        Ev::Text(_) => "Text",
        Ev::UnexpectedReply(_) => "UnexpectedReply",
        Ev::UnexpectedBigData(_) => "UnexpectedBigData",
        Ev::Unparsed { .. } => "Unparsed",
        Ev::MissingChannels(_) => "MissingChannels",
        Ev::UnknownFirmware(_) => "UnknownFirmware",
    }
}

/// The writes the link served, as the trace writes them: `<channel id> <hex>`.
fn writes(link: &TraceLink) -> Vec<String> {
    link.writes()
        .iter()
        .map(|(Channel(id), bytes)| format!("{id} {}", hex(bytes)))
        .collect()
}

/// The `tx` lines of `name`, in the same form.
fn expected(name: &str) -> Vec<String> {
    let text = fs::read_to_string(format!("{FIXTURES}/{name}")).expect("the fixture is readable");
    let trace = Trace::parse(&text).unwrap_or_else(|err| panic!("{err}"));
    trace
        .data()
        .filter(|line| line.dir == junk_trace::Direction::Tx)
        .map(|line| {
            let Channel(id) = channel_by_name(&line.chan).expect("a channel of this family");
            format!("{id} {}", hex(&line.bytes))
        })
        .collect()
}

/// `bytes` as lowercase hex, no separators, as the traces write them.
fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

#[tokio::test]
async fn the_recorded_sync_runs_again_write_for_write() {
    const TRACE: &str = "junk-sync-2026-09-06.trace";
    let mut reported = Reported::default();
    // The wall clock the session ran on: the set-time frame the trace recorded carries it,
    // and the write only matches if the script sends the same one.
    let run = junk_app::sync(
        link(TRACE),
        clock(2, 10, 24),
        7,
        std::future::pending(),
        |progress| reported.observe(&progress),
    )
    .await;
    run.result.unwrap_or_else(|err| panic!("{err:#}"));
    let (session, link): (Session, _) = (run.session, run.link);

    assert_eq!(writes(&link), expected(TRACE), "every write, in order");
    assert_eq!(link.writes().len(), 41);
    assert_eq!(link.unserved(), 0, "the whole trace was served");
    assert_eq!(
        reported.troubles,
        Vec::<String>::new(),
        "nothing went wrong"
    );
    assert_eq!(
        reported.failed,
        Vec::<String>::new(),
        "the ring refused none"
    );
    assert!(session.failures.is_empty());
    assert_eq!(reported.connected, Some((6, 247)));

    let device = &session.device;
    assert_eq!(device.firmware.as_deref(), Some("RT03CR_1.00.02_260319"));
    assert_eq!(device.hardware.as_deref(), Some("RT03CR_V1.0"));
    let battery = device.battery.expect("the ring answered the battery read");
    assert_eq!(battery.percent, Percent::new(84).expect("in range"));
    assert!(!battery.charging);
    let caps = device.capabilities.expect("the `01` ack carries them");
    assert!(caps.temperature && caps.spo2() && caps.new_sleep_protocol);
    assert_eq!(device.mtu, Some(247));

    let samples = &session.samples;
    assert!(!samples.is_empty());
    // Every slot of every day's log up to its last reading, the empty ones as
    // `Bpm::Invalid`.
    assert_eq!(samples.hr.len(), 884);
    assert_eq!(
        samples
            .hr
            .iter()
            .filter(|sample| matches!(sample.bpm, Bpm::Valid(_)))
            .count(),
        736,
        "the readings the ring actually took"
    );
    assert_eq!(samples.steps.len(), 41);
    assert_eq!(samples.stress.len(), 123);
    assert_eq!(samples.hrv.len(), 62);
    assert_eq!(samples.spo2.len(), 50);
    assert_eq!(samples.temperature.len(), 5);
    assert_eq!(samples.sleep.len(), 1);
    assert_eq!(samples.sleep[0].stages.len(), 24);
    assert_eq!(samples.workouts.len(), 10);

    // The ring volunteers its packet size after connect, and the `01` ack that answers the
    // set-time is reported as news too.
    assert_eq!(reported.events, ["PacketSize", "Capabilities"]);
}

/// A sync stopped before it starts keeps the preamble it already asked for, says it was
/// interrupted, and ends without asking for a day.
#[tokio::test]
async fn a_stopped_sync_keeps_what_it_collected() {
    const TRACE: &str = "junk-sync-2026-09-06.trace";
    let mut reported = Reported::default();
    let run = junk_app::sync(
        link(TRACE),
        clock(2, 10, 24),
        7,
        std::future::ready(()),
        |progress| reported.observe(&progress),
    )
    .await;
    run.result.unwrap_or_else(|err| panic!("{err:#}"));

    assert_eq!(reported.troubles, ["interrupted"]);
    // The nine of the preamble, and not one of the seven days that follow it.
    assert_eq!(run.link.writes().len(), 9);
    assert!(run.session.samples.is_empty(), "no day was asked for");
    assert_eq!(
        run.session.device.firmware.as_deref(),
        Some("RT03CR_1.00.02_260319"),
        "what the preamble did learn is kept"
    );
}

#[tokio::test]
async fn the_recorded_live_session_stops_the_workout_and_fetches_its_record() {
    const TRACE: &str = "junk-live-2026-09-06.trace";
    let mut reported = Reported::default();
    // Zero seconds: the stream ends at once, and the script still stops the workout and
    // fetches what the ring stored.
    let run = junk_app::live(link(TRACE), 7, 0, std::future::pending(), |progress| {
        reported.observe(&progress);
    })
    .await;
    run.result.unwrap_or_else(|err| panic!("{err:#}"));
    let (session, link) = (run.session, run.link);
    let workout = session.workout.clone();

    assert_eq!(writes(&link), expected(TRACE), "every write, in order");
    assert_eq!(link.writes().len(), 4);
    assert_eq!(link.unserved(), 0);
    assert_eq!(
        reported.troubles,
        Vec::<String>::new(),
        "nothing went wrong"
    );
    assert_eq!(reported.failed, Vec::<String>::new());
    assert!(session.failures.is_empty());

    let Workout {
        record,
        descriptor,
        heart_rates,
    } = workout.expect("the ring stored the workout");
    assert_eq!(record.sport_type, 7);
    // The `77 01` ack's own timestamp, 0x6a9c5c66; the list was asked from one second
    // before it, which is why this record and no older one came back.
    assert_eq!(record.get(WorkoutTag::StartTime), Some(1_788_632_166));
    assert_eq!(record.get(WorkoutTag::Duration), Some(75));
    assert_eq!(descriptor.package_count, 1);
    assert_eq!(descriptor.sample_second, 1);
    assert_eq!(heart_rates.len(), 65);
    assert!(
        heart_rates
            .iter()
            .all(|bpm| matches!(bpm, Bpm::Valid(56..=60))),
        "{heart_rates:?}"
    );

    // The stream is reported sample by sample as it comes, and the ring says when it has
    // stored the record; showing them is the shell's business.
    let workouts = reported
        .events
        .iter()
        .filter(|kind| *kind == "Workout")
        .count();
    assert_eq!(
        workouts, 83,
        "every `78 07` frame the trace has, duplicates included"
    );
    assert_eq!(
        reported.events.last().map(String::as_str),
        Some("Notification")
    );
    assert_eq!(
        session.samples.workouts,
        [record],
        "the list is a sample too"
    );
}
