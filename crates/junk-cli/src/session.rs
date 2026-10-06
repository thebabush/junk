//! Showing a session as it happens: one line per [`Progress`] the app's scripts report.
//!
//! `junk_app` runs the session, knows nothing about a terminal, and has the words for
//! every answer and event ([`summary`], [`describe_event`]); this is the observer the CLI
//! hands it, and all it decides is *where* a line goes. Answers go to stdout, what the ring
//! says on its own and every failure to stderr, exactly as they did when the scripts lived
//! here. What only a terminal shows — the live `<seconds> <bpm>` line and what a workout's
//! own answers add — is [`print_live`]'s.

use std::time::Instant;

use junk_app::{Progress, channel_names, describe_event, summary, workout_summary};
use junk_colmi::Bpm;
use junk_colmi::proto::{Ev, Req, Resp};
use junk_colmi::wire::{WorkoutAction, WorkoutTag};

/// Prints one step of a script.
pub fn print(progress: &Progress<'_>) {
    match progress {
        Progress::Connected { resolved, mtu } => {
            eprintln!("connected: mtu {mtu}, channels {}", channel_names(resolved));
        }
        Progress::Disconnected => eprintln!("disconnected"),
        Progress::Event(ev) => eprintln!("{}", describe_event(ev)),
        Progress::Answered { req, resp } => {
            println!("{}: {}", junk_app::describe_req(req), summary(resp));
        }
        Progress::Failed { req, err } => eprintln!("{}: {err}", junk_app::describe_req(req)),
        Progress::LinkError(err) => eprintln!("link error: {err}"),
        Progress::WorkoutStoreTimeout => {
            eprintln!("no 'workout stored' notification in time; fetching anyway");
        }
        Progress::Interrupted => eprintln!("stopping"),
        Progress::Asking(_) => {}
    }
}

/// Prints one step of the live script.
///
/// The stream's samples become `<seconds since start> <bpm>`, deduplicated on their
/// sequence number, which repeats every ten frames or so; `last_seq` is the last one
/// printed. The workout's own answers say a little more than their summary line does.
pub fn print_live(progress: &Progress<'_>, started: Instant, last_seq: &mut Option<u8>) {
    match progress {
        Progress::Event(Ev::Workout { seq, bpm, .. }) => {
            if *last_seq != Some(*seq) {
                // `-` while the sensor has no reading, so the line still says the stream is alive.
                let bpm = match bpm {
                    Bpm::Valid(value) => value.to_string(),
                    Bpm::Invalid => "-".to_owned(),
                };
                println!("{} {bpm}", started.elapsed().as_secs());
                *last_seq = Some(*seq);
            }
        }
        Progress::Answered { req, resp } => {
            print(progress);
            if let Some(line) = describe_workout_step(req, resp) {
                println!("{line}");
            }
        }
        other => print(other),
    }
}

/// What a live answer says beyond its summary line: the ring's own clock at the start, the
/// record the list ends on, and how long the detail's series is.
fn describe_workout_step(req: &Req, resp: &Resp) -> Option<String> {
    match (req, resp) {
        (
            Req::WorkoutCtl {
                action: WorkoutAction::Start,
                ..
            },
            Resp::WorkoutCtl { start },
        ) => Some(start.map_or_else(
            || "started; the ack carried no time".to_owned(),
            |at| format!("started at ring clock {at}"),
        )),
        (Req::WorkoutList { .. }, Resp::Workouts(list)) => list
            .records
            .iter()
            .max_by_key(|record| record.get(WorkoutTag::StartTime))
            .map(|record| format!("latest record: {}", workout_summary(record))),
        (Req::WorkoutDetail { .. }, Resp::WorkoutDetail { heart_rates, .. }) => {
            Some(format!("heart rates: {}", heart_rates.len()))
        }
        _ => None,
    }
}

/// The name of each event kind, for tallies.
pub fn event_kind(ev: &Ev) -> &'static str {
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
