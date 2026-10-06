//! A session with a ring: the pump and a script run together on one task, and the two
//! scripts a shell asks for.
//!
//! [`drive`] runs a [`Pump`] over a link, a script over its [`Handle`], and a drain of the
//! pump's events, all with `tokio::join!` on the calling task: no spawn, so the link needs
//! no `Send` and comes back afterwards through [`Pump::into_parts`] (a recording link's
//! lines are in it). [`ask`] is how a script asks one thing: an answer is reported, folded
//! into the [`Session`] and returned; a protocol error is reported, kept in
//! [`Session::failures`] and the script goes on; a pump that no longer runs requests ends
//! the script.
//!
//! Nothing here does any I/O of its own: what a shell wants to show, it shows from the
//! [`Progress`] it is handed.
//!
//! The script and the drain share the observer through a [`Mutex`] rather than a
//! `RefCell`, though they run on the one task and never contend for it: that is what keeps
//! the futures [`Send`], which `junk-ffi` needs to hand a whole session to uniffi's
//! runtime.

use std::pin::{Pin, pin};
use std::sync::{Mutex, PoisonError};
use std::task::{Context, Waker};
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use junk_colmi::Bpm;
use junk_colmi::ColmiDriver;
use junk_colmi::proto::{Ev, Req, Resp};
use junk_colmi::wire::{
    Capabilities, Notification, Platform, WorkoutAction, WorkoutDescriptor, WorkoutRecord,
    WorkoutTag,
};
use junk_core::{Battery, ChannelSet, Link, LinkError, ProtoError, Timestamp};
use junk_pump::{Handle, Pump, PumpConfig, PumpEvent, RequestError, Stop};
use tokio::sync::{mpsc, oneshot};

use crate::Samples;
use crate::clock::Clock;

/// Minutes in a day.
const MINUTES_PER_DAY: i64 = 24 * 60;

/// The OS major version `junk` tells the ring, as the app does.
const PHONE_OS_VERSION: u8 = 18;
/// The `lang` byte of the set-time frame, as the app sends it.
const SET_TIME_LANG: u8 = 1;
/// The sleep request body: `01 01`, what the app sends after its first sync.
const SLEEP_SELECTOR: [u8; 2] = [0x01, 0x01];
/// The `SpO2` request byte: `ff`, all history.
const SPO2_SELECTOR: u8 = 0xff;
/// The temperature request byte: `00`, today.
const TEMPERATURE_SELECTOR: u8 = 0;
/// How long [`live`] waits for the ring to say the record is stored.
const STORED_WAIT: Duration = Duration::from_secs(3);

/// What the session learned about the ring itself.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Device {
    /// The Firmware Revision string, `RT03CR_1.00.02_260319` on the R10.
    pub firmware: Option<String>,
    /// The Hardware Revision string, `RT03CR_V1.0` on the R10.
    pub hardware: Option<String>,
    /// The charge, as the ring last reported it.
    pub battery: Option<Battery>,
    /// What the ring says it can do; the `0x01` ack carries it.
    pub capabilities: Option<Capabilities>,
    /// The ATT MTU the link came up with.
    pub mtu: Option<u16>,
}

/// The outcome of a script: what it collected, what the ring is, and what it was refused.
///
/// A refusal does not end a script (a dialect without a feature refuses the request for
/// it), so a session that ran to the end can still have failures.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Session {
    /// Everything the ring gave.
    pub samples: Samples,
    /// What the ring is.
    pub device: Device,
    /// Every request the ring refused: the request, as the driver's own `Debug`, and why.
    pub failures: Vec<(String, ProtoError)>,
    /// The workout [`live`] recorded, once it has been fetched. Always `None` after a
    /// [`sync`], which starts no workout.
    pub workout: Option<Workout>,
}

/// What a script leaves behind: how it ended, what it collected, and the link back.
///
/// The session is here whether the script finished or not, so a shell can keep what was
/// collected before a ring wandered out of range; the link is here so that a recording
/// one's lines can be taken.
pub struct Run<L> {
    /// How the script ended.
    pub result: anyhow::Result<()>,
    /// What it collected, complete or not.
    pub session: Session,
    /// The link it ran over.
    pub link: L,
}

/// One step of a script, as it happens.
///
/// A shell prints these, or turns them into a callback; the script itself neither knows
/// nor cares.
#[derive(Debug)]
pub enum Progress<'a> {
    /// The link came up.
    Connected {
        /// Which declared channels the ring has.
        resolved: &'a ChannelSet,
        /// The negotiated ATT MTU, in bytes.
        mtu: u16,
    },
    /// A request is about to go to the ring.
    Asking(&'a Req),
    /// The ring answered it.
    Answered {
        /// What was asked.
        req: &'a Req,
        /// What came back.
        resp: &'a Resp,
    },
    /// The ring refused it. The script goes on.
    Failed {
        /// What was asked.
        req: &'a Req,
        /// Why not.
        err: ProtoError,
    },
    /// Something the ring said on its own.
    Event(&'a Ev),
    /// A write, a subscription or a read did not go through. The script goes on: the
    /// driver's own timeout fails whatever request it belonged to.
    LinkError(&'a LinkError),
    /// The ring did not say the workout record was stored; it is fetched anyway.
    WorkoutStoreTimeout,
    /// The caller's `until` future fired and the live stream stopped early.
    Interrupted,
    /// The link is down; the session is over.
    Disconnected,
}

/// Why [`stream`] stopped.
enum StreamEnd {
    /// The time asked for passed.
    Deadline,
    /// The caller's `until` future fired.
    Interrupted,
    /// The link went away.
    Disconnected,
}

/// One workout, as [`live`] fetched it back after stopping it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workout {
    /// The stored record: its start, duration and totals, as the ring lists them.
    pub record: WorkoutRecord,
    /// How the samples are laid out.
    pub descriptor: WorkoutDescriptor,
    /// The heart rates, one per sampling interval.
    pub heart_rates: Vec<Bpm>,
}

/// Runs a pump over `link` together with `script`, on this task, until both are done.
///
/// `script` gets a handle to the pump; `observe` sees every pump event as it comes. When
/// the pump's run ends (a shutdown, a disconnect, a failed connect) the handle is shut down
/// and the pump run once more, which fails every request still queued at once, so a script
/// waiting on one sees [`RequestError::Stopped`] rather than waiting for ever.
///
/// Returns the outcome and the link: a failed connect is the outcome whatever the script
/// said; otherwise the script's own.
///
/// # Errors
///
/// Whatever the script failed with, or [`PumpError`](junk_pump::PumpError) if the link
/// could not be connected.
pub async fn drive<L, F, O>(
    link: L,
    script: impl FnOnce(Handle<ColmiDriver>) -> F,
    observe: O,
) -> (anyhow::Result<()>, L)
where
    L: Link,
    F: Future<Output = anyhow::Result<()>>,
    O: FnMut(PumpEvent<Ev>),
{
    let (mut pump, handle, mut events) = Pump::new(ColmiDriver::new(), link, PumpConfig::default());
    let (done_tx, done_rx) = oneshot::channel::<()>();

    let pump_run = async {
        let result = pump.run().await;
        // Nothing runs the requests still queued: fail them now rather than leave the
        // script waiting on them. A run after a shutdown drops them and returns at once.
        handle.shutdown();
        let _ = pump.run().await;
        drop(done_tx);
        result
    };
    let drain = async move {
        let mut done_rx = done_rx;
        let mut observe = observe;
        loop {
            tokio::select! {
                biased;
                event = events.recv() => match event {
                    Some(event) => observe(event),
                    None => break,
                },
                _ = &mut done_rx => {
                    while let Ok(event) = events.try_recv() {
                        observe(event);
                    }
                    break;
                }
            }
        }
    };
    // Whatever the script's outcome, the pump must stop once it is done, or the join below
    // would wait on a connected, idle pump for ever.
    let script_run = async {
        let result = script(handle.clone()).await;
        handle.shutdown();
        result
    };

    let (pump_result, (), script_result) = tokio::join!(pump_run, drain, script_run);
    let (_driver, link) = pump.into_parts();
    let result = match (pump_result, script_result) {
        (Err(err), _) => Err(anyhow!("{err}")),
        (Ok(Stop::Disconnected), Err(err)) => Err(err.context("the link dropped")),
        (Ok(Stop::Disconnected | Stop::Shutdown), result) => result,
    };
    (result, link)
}

/// `req` in a few words: what a shell shows next to its answer or its failure, and what
/// [`Session::failures`] records it as.
#[must_use]
pub fn describe_req(req: &Req) -> String {
    match req {
        Req::SetTime { .. } => "set time".to_owned(),
        Req::Battery => "battery".to_owned(),
        Req::PhoneName { .. } => "phone name".to_owned(),
        Req::ReadPrefs => "read prefs".to_owned(),
        Req::WritePrefs(_) => "write prefs".to_owned(),
        Req::Version => "version".to_owned(),
        Req::ReadGoals => "goals".to_owned(),
        Req::ReadAutoHrPref => "auto hr pref".to_owned(),
        Req::ReadAutoSpo2Pref => "auto spo2 pref".to_owned(),
        Req::ReadAutoStressPref => "auto stress pref".to_owned(),
        Req::ReadAutoHrvPref => "auto hrv pref".to_owned(),
        Req::TodayTotals => "today totals".to_owned(),
        Req::WorkoutCtl { action, sport_type } => {
            format!("workout {action:?} sport {sport_type}").to_lowercase()
        }
        Req::ManualHrStart { .. } => "manual hr start".to_owned(),
        Req::ManualHrStop { .. } => "manual hr stop".to_owned(),
        Req::Raw { cmd, .. } => format!("raw {:#04x}", cmd.byte()),
        Req::RawBigData { kind, .. } => format!("raw big data {:#04x}", kind.byte()),
        Req::HrLog { day_start } => format!("hr log {}", crate::date_of(*day_start)),
        Req::Stress { days_ago, .. } => format!("stress {days_ago} days ago"),
        Req::Hrv { days_ago, .. } => format!("hrv {days_ago} days ago"),
        Req::Activity { days_ago, .. } => format!("activity {days_ago} days ago"),
        Req::Sleep { .. } => "sleep".to_owned(),
        Req::Spo2 { .. } => "spo2".to_owned(),
        Req::Temperature { .. } => "temperature".to_owned(),
        Req::WorkoutList { since } => format!("workout list since {since}"),
        Req::WorkoutDetail { sport_type, start } => {
            format!("workout detail sport {sport_type} start {start}")
        }
    }
}

/// Ends a script early because it was stopped: says so and shuts the pump down.
fn finish_early<O: FnMut(Progress<'_>)>(handle: &Handle<ColmiDriver>, observe: &Mutex<O>) {
    report(observe, Progress::Interrupted);
    handle.shutdown();
}

/// Whether `until` has finished, without waiting for it.
///
/// A script asks between requests, so stopping never abandons one in flight: the ring
/// always finishes answering what it was asked before the session ends.
fn stopped<F: Future<Output = ()>>(until: &mut Pin<&mut F>) -> bool {
    let mut cx = Context::from_waker(Waker::noop());
    until.as_mut().poll(&mut cx).is_ready()
}

/// Asks the ring `req` and reports.
///
/// An answer is reported as [`Progress::Answered`], folded into `session` and returned. A
/// protocol error is reported as [`Progress::Failed`], kept in [`Session::failures`] and
/// `None` returned: the session goes on. A pump that no longer runs requests
/// ([`RequestError::Stopped`]) is an error: the session is over.
///
/// # Errors
///
/// [`RequestError::Stopped`].
async fn ask<O: FnMut(Progress<'_>)>(
    handle: &Handle<ColmiDriver>,
    session: &mut Session,
    observe: &Mutex<O>,
    req: Req,
) -> anyhow::Result<Option<Resp>> {
    report(observe, Progress::Asking(&req));
    match handle.request(req.clone()).await {
        Ok(resp) => {
            report(
                observe,
                Progress::Answered {
                    req: &req,
                    resp: &resp,
                },
            );
            session.samples.absorb(&resp);
            learn(&mut session.device, &resp);
            Ok(Some(resp))
        }
        Err(RequestError::Proto(err)) => {
            report(observe, Progress::Failed { req: &req, err });
            session.failures.push((describe_req(&req), err));
            Ok(None)
        }
        Err(err @ RequestError::Stopped) => Err(anyhow!("{}: {err}", describe_req(&req))),
    }
}

/// Adds what `resp` says about the ring itself.
fn learn(device: &mut Device, resp: &Resp) {
    match resp {
        Resp::Version { firmware, hardware } => {
            device.firmware = Some(firmware.clone());
            device.hardware = Some(hardware.clone());
        }
        Resp::Battery(battery) => device.battery = Some(*battery),
        Resp::Capabilities(caps) => device.capabilities = Some(*caps),
        Resp::Ack
        | Resp::Prefs(_)
        | Resp::Goals { .. }
        | Resp::AutoPref { .. }
        | Resp::TodayTotals { .. }
        | Resp::WorkoutCtl { .. }
        | Resp::Raw(_)
        | Resp::RawBigData(_)
        | Resp::HrLog(_)
        | Resp::Stress(_)
        | Resp::Hrv(_)
        | Resp::Activity(_)
        | Resp::Sleep(_)
        | Resp::Spo2(_)
        | Resp::Temperature(_)
        | Resp::Workouts(_)
        | Resp::WorkoutDetail { .. } => {}
    }
}

/// Hands `progress` to the observer.
///
/// The observer is shared between the script and the drain, which run on the one task, so
/// the lock is never contended and a `RefCell` would do; it is a [`Mutex`] because that is
/// what makes the whole session future [`Send`], which `junk-ffi` needs to hand it to
/// uniffi's runtime. A lock poisoned by an observer that panicked is taken anyway: the
/// session is not what went wrong.
fn report<O: FnMut(Progress<'_>)>(observe: &Mutex<O>, progress: Progress<'_>) {
    (observe.lock().unwrap_or_else(PoisonError::into_inner))(progress);
}

/// Turns one pump event into a [`Progress`], remembering the MTU the link came up with.
fn relay<O: FnMut(Progress<'_>)>(
    observe: &Mutex<O>,
    mtu: &Mutex<Option<u16>>,
    event: &PumpEvent<Ev>,
) {
    match event {
        PumpEvent::Connected {
            resolved,
            mtu: negotiated,
        } => {
            *mtu.lock().unwrap_or_else(PoisonError::into_inner) = Some(*negotiated);
            report(
                observe,
                Progress::Connected {
                    resolved,
                    mtu: *negotiated,
                },
            );
        }
        PumpEvent::Disconnected => report(observe, Progress::Disconnected),
        PumpEvent::Event(ev) => report(observe, Progress::Event(ev)),
        PumpEvent::LinkError(err) => report(observe, Progress::LinkError(err)),
    }
}

/// The app's session: the preamble, then `days` days of logs, then the big-data
/// collections and the workout list.
///
/// `clock` is the wall clock at the start: it sets the ring's time and places "today".
/// The link comes back so that a recording link's lines can be taken.
///
/// # Errors
///
/// If the link could not be connected, or the pump stopped with a request in flight.
pub async fn sync<L: Link>(
    link: L,
    clock: Clock,
    days: u8,
    until: impl Future<Output = ()>,
    observe: impl FnMut(Progress<'_>),
) -> Run<L> {
    let observe = Mutex::new(observe);
    let mut session = Session::default();
    let mtu = Mutex::new(None);
    let (result, link) = drive(
        link,
        |handle| sync_script(handle, clock, days, until, &mut session, &observe),
        |event| relay(&observe, &mtu, &event),
    )
    .await;
    session.device.mtu = *mtu.lock().unwrap_or_else(PoisonError::into_inner);
    Run {
        result,
        session,
        link,
    }
}

/// The sync script: the app's session, then `days` days of logs, then the big-data
/// collections and the workout list, then shutdown.
async fn sync_script<O: FnMut(Progress<'_>)>(
    handle: Handle<ColmiDriver>,
    clock: Clock,
    days: u8,
    until: impl Future<Output = ()>,
    session: &mut Session,
    observe: &Mutex<O>,
) -> anyhow::Result<()> {
    let mut until = pin!(until);
    let today = clock.midnight();
    let preamble = [
        Req::PhoneName {
            platform: Platform::Ios,
            os_version: PHONE_OS_VERSION,
            name: Vec::new(),
        },
        Req::SetTime {
            at: clock.timestamp(),
            second: clock.second(),
            lang: SET_TIME_LANG,
        },
        Req::Version,
        Req::Battery,
        Req::ReadAutoHrPref,
        Req::ReadAutoSpo2Pref,
        Req::ReadAutoStressPref,
        Req::ReadAutoHrvPref,
        Req::ReadGoals,
    ];
    for req in preamble {
        ask(&handle, session, observe, req).await?;
    }
    // Stopping is checked between requests, so what the ring is answering is never
    // abandoned and what was collected is kept.
    for days_ago in 0..days {
        if stopped(&mut until) {
            finish_early(&handle, observe);
            return Ok(());
        }
        let day_start = Timestamp {
            local_minute: today
                .local_minute
                .saturating_sub(i64::from(days_ago) * MINUTES_PER_DAY),
            ..today
        };
        for req in [
            Req::activity_day(days_ago, today),
            Req::HrLog { day_start },
            Req::Stress { days_ago, today },
            Req::Hrv { days_ago, today },
        ] {
            ask(&handle, session, observe, req).await?;
        }
    }
    if stopped(&mut until) {
        finish_early(&handle, observe);
        return Ok(());
    }
    // A dialect without one of these refuses it as unsupported, which `ask` reports and
    // goes on from.
    for req in [
        Req::Sleep {
            today,
            selector: SLEEP_SELECTOR.to_vec(),
        },
        Req::Spo2 {
            today,
            selector: SPO2_SELECTOR,
        },
        Req::Temperature {
            today,
            selector: TEMPERATURE_SELECTOR,
        },
        Req::WorkoutList { since: 0 },
    ] {
        ask(&handle, session, observe, req).await?;
    }
    handle.shutdown();
    Ok(())
}

/// Stage C: start a workout, let it stream for `seconds`, stop it, and fetch the record
/// the ring stored, with its heart rates.
///
/// The stream's samples arrive as [`Progress::Event`] with
/// [`Ev::Workout`](junk_colmi::proto::Ev::Workout); their sequence number repeats every
/// ten frames or so, so a shell that shows them dedupes on it. `Ctrl-C` stops the stream
/// early. A workout under about a minute is not stored, and then there is no record.
///
/// The stream stops when `seconds` pass, when `until` finishes, or when the link goes
/// away, whichever comes first: a shell passes Ctrl-C, a cancel button, or
/// [`std::future::pending`] for none.
///
/// # Errors
///
/// If the link could not be connected, the pump stopped with a request in flight, or the
/// ring answered the start, the list or the record with something that makes no sense.
pub async fn live<L: Link>(
    link: L,
    sport_type: u8,
    seconds: u64,
    until: impl Future<Output = ()>,
    observe: impl FnMut(Progress<'_>),
) -> Run<L> {
    let observe = Mutex::new(observe);
    let mut session = Session::default();
    let mtu = Mutex::new(None);
    // The script needs the events too, to see the live samples stop and the ring say the
    // record is stored; the observer passes them on.
    let (tx, rx) = mpsc::unbounded_channel();
    let (result, link) = drive(
        link,
        |handle| {
            live_script(
                handle,
                rx,
                sport_type,
                seconds,
                until,
                &mut session,
                &observe,
            )
        },
        |event| {
            relay(&observe, &mtu, &event);
            let _ = tx.send(event);
        },
    )
    .await;
    session.device.mtu = *mtu.lock().unwrap_or_else(PoisonError::into_inner);
    Run {
        result,
        session,
        link,
    }
}

/// The live script: start the workout, let it stream until time is up or `until` fires,
/// stop it, wait for the ring to store the record, fetch the record and its detail, shut
/// down.
async fn live_script<O: FnMut(Progress<'_>)>(
    handle: Handle<ColmiDriver>,
    mut events: mpsc::UnboundedReceiver<PumpEvent<Ev>>,
    sport_type: u8,
    seconds: u64,
    until: impl Future<Output = ()>,
    session: &mut Session,
    observe: &Mutex<O>,
) -> anyhow::Result<()> {
    let start = Req::WorkoutCtl {
        action: WorkoutAction::Start,
        sport_type,
    };
    // The ring's own start time is the cursor for the record list afterwards: only this
    // session's record comes back, however many older ones the ring keeps. The ring lists
    // records strictly newer than the cursor (the app sends its newest known start and
    // gets nothing back), so the cursor is one second before the start.
    let since = match ask(&handle, session, observe, start).await? {
        Some(Resp::WorkoutCtl { start: Some(at) }) => at.saturating_sub(1),
        Some(Resp::WorkoutCtl { start: None }) => 0,
        Some(other) => bail!("the start was answered with {other:?}"),
        None => bail!("the ring refused to start the workout"),
    };

    if let StreamEnd::Interrupted = stream(&mut events, seconds, until).await {
        report(observe, Progress::Interrupted);
    }

    let stop = Req::WorkoutCtl {
        action: WorkoutAction::Stop,
        sport_type,
    };
    ask(&handle, session, observe, stop).await?;
    // The `73 07` notification only says the ring is ready; fetching without it is worth a
    // try, since the record may be stored anyway.
    if !wait_stored(&mut events).await {
        report(observe, Progress::WorkoutStoreTimeout);
    }

    let Some(Resp::Workouts(summary)) =
        ask(&handle, session, observe, Req::WorkoutList { since }).await?
    else {
        bail!("no workout list");
    };
    let Some(record) = summary
        .records
        .iter()
        .max_by_key(|record| record.get(WorkoutTag::StartTime))
        .cloned()
    else {
        bail!("the ring lists no workout records");
    };
    let start = record
        .get(WorkoutTag::StartTime)
        .and_then(|start| u32::try_from(start).ok())
        .context("the record has no start time the detail request can carry")?;
    let detail = Req::WorkoutDetail {
        sport_type: record.sport_type,
        start,
    };
    if let Some(Resp::WorkoutDetail {
        descriptor,
        heart_rates,
    }) = ask(&handle, session, observe, detail).await?
    {
        session.workout = Some(Workout {
            record,
            descriptor,
            heart_rates,
        });
    }
    handle.shutdown();
    Ok(())
}

/// Drains the pump's events until `seconds` pass, Ctrl-C, or the link is gone. The samples
/// themselves reached the shell through the observer as they came.
async fn stream(
    events: &mut mpsc::UnboundedReceiver<PumpEvent<Ev>>,
    seconds: u64,
    until: impl Future<Output = ()>,
) -> StreamEnd {
    let deadline = tokio::time::sleep(Duration::from_secs(seconds));
    tokio::pin!(deadline);
    tokio::pin!(until);
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Some(PumpEvent::Disconnected) | None => return StreamEnd::Disconnected,
                Some(
                    PumpEvent::Connected { .. } | PumpEvent::Event(_) | PumpEvent::LinkError(_),
                ) => {}
            },
            () = &mut deadline => return StreamEnd::Deadline,
            () = &mut until => return StreamEnd::Interrupted,
        }
    }
}

/// Waits up to [`STORED_WAIT`] for the ring to say the record is stored.
async fn wait_stored(events: &mut mpsc::UnboundedReceiver<PumpEvent<Ev>>) -> bool {
    tokio::time::timeout(STORED_WAIT, async {
        while let Some(event) = events.recv().await {
            match event {
                PumpEvent::Event(Ev::Notification(Notification::WorkoutStored)) => return true,
                PumpEvent::Disconnected => return false,
                PumpEvent::Connected { .. } | PumpEvent::Event(_) | PumpEvent::LinkError(_) => {}
            }
        }
        false
    })
    .await
    .unwrap_or(false)
}
