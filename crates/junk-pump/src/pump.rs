//! The loop itself, its handle, and what both report.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::future::pending;
use std::mem;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use junk_core::{
    ChannelSet, Driver, Input, Instant, Link, LinkError, LinkEvent, Output, Outputs, ProtoError,
    ReqId, TimerId,
};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Interval, MissedTickBehavior};

/// How a [`Pump`] runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PumpConfig {
    /// Feed the driver [`Input::Tick`] this often, or never if `None` or zero.
    ///
    /// Ticks are the only way the driver sees the clock pass without something else
    /// happening. A tick that could not be delivered on time (the loop was busy) is delayed,
    /// not repeated: the next one comes a full period after the late one.
    pub tick_every: Option<Duration>,
}

/// What a [`Pump`] reports on its events channel, in the order it happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PumpEvent<Ev> {
    /// The link came up. Sent before the driver is told.
    Connected {
        /// Which declared channels the device has.
        resolved: ChannelSet,
        /// The negotiated ATT MTU, in bytes.
        mtu: u16,
    },
    /// The link is down and the driver has been told. The last event of a [`Pump::run`].
    Disconnected,
    /// Something the driver reported on its own.
    Event(Ev),
    /// A write, a subscription or a read the driver asked for failed. The loop goes on: if
    /// the device is truly gone the link will say so, and the driver's own timeout fails
    /// whatever request the operation belonged to. A failed read feeds no [`Input::Rx`].
    LinkError(LinkError),
}

/// Why [`Pump::run`] returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stop {
    /// The link dropped, or the driver asked for it to be dropped.
    Disconnected,
    /// [`Handle::shutdown`] was called, or every [`Handle`] was dropped.
    Shutdown,
}

/// Why [`Pump::run`] could not get going.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PumpError {
    /// [`Link::connect`] failed. The driver was not told anything.
    Connect(LinkError),
}

impl fmt::Display for PumpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PumpError::Connect(err) => write!(f, "could not connect: {err}"),
        }
    }
}

impl std::error::Error for PumpError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PumpError::Connect(err) => Some(err),
        }
    }
}

/// Why [`Handle::request`] did not get an answer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RequestError {
    /// The driver answered, and the answer is an error.
    Proto(ProtoError),
    /// The pump will never answer: it was shut down, or dropped, or it stopped running
    /// (its [`Pump::run`] returned) with the request still in flight.
    Stopped,
}

impl fmt::Display for RequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RequestError::Proto(err) => write!(f, "request failed: {err}"),
            RequestError::Stopped => f.write_str("the pump is no longer running requests"),
        }
    }
}

impl std::error::Error for RequestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RequestError::Proto(err) => Some(err),
            RequestError::Stopped => None,
        }
    }
}

impl From<ProtoError> for RequestError {
    fn from(err: ProtoError) -> Self {
        RequestError::Proto(err)
    }
}

/// The receiving end of a [`Pump`]'s events: what [`Pump::new`] hands back.
pub type Events<D> = mpsc::UnboundedReceiver<PumpEvent<<D as Driver>::Ev>>;

/// Where a [`Handle`] sends the driver's answer.
type Reply<D> = oneshot::Sender<Result<<D as Driver>::Resp, ProtoError>>;

/// What a [`Handle`] sends the pump.
enum Command<D: Driver> {
    /// A request for the driver, and where to put its answer.
    Request {
        /// The request.
        req: D::Req,
        /// Where the answer goes.
        reply: Reply<D>,
    },
    /// Stop.
    Shutdown,
}

/// The way to talk to a running [`Pump`]. Cheap to clone.
///
/// A handle lives on after the pump stops: its requests then wait for the next
/// [`Pump::run`], or fail with [`RequestError::Stopped`] once the pump is dropped.
pub struct Handle<D: Driver> {
    commands: mpsc::UnboundedSender<Command<D>>,
    shutdown: Arc<AtomicBool>,
}

impl<D: Driver> Handle<D> {
    /// Asks the driver `req` and waits for its one answer (invariant 4).
    ///
    /// Requests are fed to the driver in the order they were made, across every clone of
    /// the handle.
    ///
    /// # Errors
    ///
    /// [`RequestError::Proto`] with whatever the driver answered; [`RequestError::Stopped`]
    /// if the pump was shut down, was dropped, or stopped running with the request still
    /// in flight.
    pub async fn request(&self, req: D::Req) -> Result<D::Resp, RequestError> {
        if self.shutdown.load(Ordering::Acquire) {
            return Err(RequestError::Stopped);
        }
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(Command::Request { req, reply })
            .map_err(|_| RequestError::Stopped)?;
        match answer.await {
            Ok(result) => result.map_err(RequestError::Proto),
            Err(_) => Err(RequestError::Stopped),
        }
    }

    /// Stops the pump: it drops the link, tells the driver, fails every request in flight
    /// and returns [`Stop::Shutdown`] from [`Pump::run`].
    ///
    /// Shutting down is final and idempotent: every later call does nothing, every later
    /// [`Handle::request`] fails with [`RequestError::Stopped`], and every later
    /// [`Pump::run`] returns [`Stop::Shutdown`] at once without connecting.
    pub fn shutdown(&self) {
        if !self.shutdown.swap(true, Ordering::AcqRel) {
            // Only the first call sends; the flag already carries the decision, and a
            // second message would end a later `run` that has nothing to do with it.
            let _ = self.commands.send(Command::Shutdown);
        }
    }
}

// Not derived: that would demand `D: Clone` / `D: Debug`, and a handle holds no `D`.
impl<D: Driver> Clone for Handle<D> {
    fn clone(&self) -> Self {
        Handle {
            commands: self.commands.clone(),
            shutdown: Arc::clone(&self.shutdown),
        }
    }
}

impl<D: Driver> fmt::Debug for Handle<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Handle")
            .field("shutdown", &self.shutdown.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

/// What woke the loop.
enum Wake<D: Driver> {
    /// The link had something.
    Link(LinkEvent),
    /// This timer's deadline passed.
    Timer(TimerId),
    /// A handle sent this, or (`None`) every handle is gone.
    Command(Option<Command<D>>),
    /// The tick period elapsed.
    Tick,
}

/// Whether the outputs of a step asked for the link to go.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Flow {
    /// Keep going.
    Continue,
    /// The driver emitted [`Output::Disconnect`]; the link has been dropped.
    Disconnect,
}

/// The loop: one [`Driver`], one [`Link`], and the only code that runs them.
///
/// Build one with [`Pump::new`], then [`Pump::run`] it. `run` returns when the link goes
/// away or a [`Handle`] shuts the pump down; it may be called again afterwards to reconnect
/// with the same driver and link (invariant 6 says the driver must cope).
pub struct Pump<D: Driver, L: Link> {
    driver: D,
    link: L,
    config: PumpConfig,
    /// What [`Instant::ZERO`] means: when the pump was created.
    origin: tokio::time::Instant,
    commands: mpsc::UnboundedReceiver<Command<D>>,
    events: mpsc::UnboundedSender<PumpEvent<D::Ev>>,
    shutdown: Arc<AtomicBool>,
    /// The next [`ReqId`] to hand out.
    next_id: u32,
    /// Requests the driver has not answered yet.
    pending: BTreeMap<ReqId, Reply<D>>,
    /// Armed timers and when they fire.
    timers: BTreeMap<TimerId, tokio::time::Instant>,
    /// The values of the reads the driver asked for, as the inputs they become, in order:
    /// fed before anything else is waited for (invariant 3).
    reads: VecDeque<Input<D::Req>>,
    /// The driver's sink, kept between steps so its buffer is reused.
    outputs: Outputs<D::Resp, D::Ev>,
}

impl<D: Driver, L: Link> Pump<D, L> {
    /// A pump over `driver` and `link`, not yet running, with a handle to it and the
    /// receiving end of its events.
    ///
    /// Events are never dropped on the pump's side; a receiver that is not read grows. A
    /// dropped receiver is fine: events are then discarded.
    #[must_use]
    pub fn new(driver: D, link: L, config: PumpConfig) -> (Pump<D, L>, Handle<D>, Events<D>) {
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let pump = Pump {
            driver,
            link,
            config,
            origin: tokio::time::Instant::now(),
            commands: command_rx,
            events: event_tx,
            shutdown: Arc::clone(&shutdown),
            next_id: 0,
            pending: BTreeMap::new(),
            timers: BTreeMap::new(),
            reads: VecDeque::new(),
            outputs: Outputs::new(),
        };
        let handle = Handle {
            commands: command_tx,
            shutdown,
        };
        (pump, handle, event_rx)
    }

    /// Connects and runs the loop until the link is gone or a handle stops it.
    ///
    /// On success the events channel saw [`PumpEvent::Connected`] first and
    /// [`PumpEvent::Disconnected`] last, and the driver saw [`Input::Connected`] first and
    /// [`Input::Disconnected`] last, so every request in flight was answered by the driver
    /// (invariant 4). Requests that were queued but not yet fed, and any the driver did not
    /// answer, fail with [`RequestError::Stopped`]. Timers are forgotten.
    ///
    /// Returns [`Stop::Shutdown`] at once, without connecting, if a handle already shut
    /// the pump down.
    ///
    /// # Errors
    ///
    /// [`PumpError::Connect`] if [`Link::connect`] failed; the driver was not told
    /// anything, and queued requests wait for the next call.
    pub async fn run(&mut self) -> Result<Stop, PumpError> {
        let result = self.session().await;
        self.timers.clear();
        self.pending.clear();
        self.reads.clear();
        result
    }

    /// The driver.
    #[must_use]
    pub fn driver(&self) -> &D {
        &self.driver
    }

    /// Takes the driver and the link back. Every request still queued or in flight fails
    /// with [`RequestError::Stopped`].
    #[must_use]
    pub fn into_parts(self) -> (D, L) {
        (self.driver, self.link)
    }

    /// One connection: from `connect` to the step that tells the driver it is over.
    async fn session(&mut self) -> Result<Stop, PumpError> {
        if self.shutdown.load(Ordering::Acquire) {
            self.fail_queued();
            return Ok(Stop::Shutdown);
        }
        let (resolved, mtu) = self
            .link
            .connect(D::GATT)
            .await
            .map_err(PumpError::Connect)?;
        self.emit(PumpEvent::Connected { resolved, mtu });
        if self.step(Input::Connected { resolved, mtu }).await == Flow::Disconnect {
            return Ok(self.finish(Stop::Disconnected).await);
        }

        // A zero period means no ticks; tokio would panic on it.
        let period = self.config.tick_every.filter(|every| !every.is_zero());
        let mut tick = period.map(|every| {
            let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
            tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
            tick
        });

        loop {
            // The value of a read is the next input: it goes in before the loop waits on
            // anything, so nothing comes between a step and what it asked to read.
            if let Some(input) = self.reads.pop_front() {
                if self.step(input).await == Flow::Disconnect {
                    return Ok(self.finish(Stop::Disconnected).await);
                }
                continue;
            }
            let deadline = self
                .timers
                .iter()
                .min_by_key(|(_, at)| **at)
                .map(|(id, at)| (*id, *at));
            // Biased: the link before the timers before the handles before the tick, always.
            // The `link.next()` future lives only for this wait; it is dropped before any
            // output is applied, which is why `Link::next` must be cancel-safe.
            let wake = tokio::select! {
                biased;
                event = self.link.next() => Wake::Link(event),
                id = wait_timer(deadline) => Wake::Timer(id),
                command = self.commands.recv() => Wake::Command(command),
                () = wait_tick(tick.as_mut()) => Wake::Tick,
            };

            let flow = match wake {
                Wake::Link(LinkEvent::Rx { chan, bytes }) => {
                    self.step(Input::Rx { chan, bytes }).await
                }
                Wake::Link(LinkEvent::Disconnected) => {
                    return Ok(self.finish(Stop::Disconnected).await);
                }
                Wake::Timer(id) => {
                    self.timers.remove(&id);
                    self.step(Input::Timer(id)).await
                }
                Wake::Command(Some(Command::Request { req, reply })) => {
                    let id = ReqId(self.next_id);
                    self.next_id = self.next_id.wrapping_add(1);
                    // A collision needs four billion requests in flight; the older one,
                    // if any, just sees `Stopped`.
                    self.pending.insert(id, reply);
                    let now = self.now();
                    self.step(Input::Request { id, req, now }).await
                }
                Wake::Command(Some(Command::Shutdown) | None) => {
                    // Every handle gone is a shutdown too; remember it so a later `run`
                    // does not connect only to stop again.
                    self.shutdown.store(true, Ordering::Release);
                    self.link.disconnect().await;
                    return Ok(self.finish(Stop::Shutdown).await);
                }
                Wake::Tick => {
                    let now = self.now();
                    self.step(Input::Tick { now }).await
                }
            };
            if flow == Flow::Disconnect {
                return Ok(self.finish(Stop::Disconnected).await);
            }
        }
    }

    /// Drops every request still queued in the channel so its requester sees
    /// [`RequestError::Stopped`] now rather than when the pump is dropped.
    fn fail_queued(&mut self) {
        while let Ok(command) = self.commands.try_recv() {
            drop(command);
        }
    }

    /// Tells the driver the link is gone and reports it; the last thing a session does.
    async fn finish(&mut self, stop: Stop) -> Stop {
        // The link is already down, so a `Disconnect` in these outputs changes nothing.
        self.step(Input::Disconnected).await;
        self.emit(PumpEvent::Disconnected);
        stop
    }

    /// Feeds `input` and applies every output of that step, in order, before returning.
    async fn step(&mut self, input: Input<D::Req>) -> Flow {
        self.driver.handle(input, &mut self.outputs);
        let mut flow = Flow::Continue;
        // Taken out so the link and the maps can be borrowed while it is drained; put back
        // afterwards so its buffer is kept.
        let mut outputs = mem::take(&mut self.outputs);
        for output in outputs.drain() {
            if self.apply(output).await == Flow::Disconnect {
                flow = Flow::Disconnect;
            }
        }
        self.outputs = outputs;
        flow
    }

    /// Applies one output.
    async fn apply(&mut self, output: Output<D::Resp, D::Ev>) -> Flow {
        match output {
            Output::Tx { chan, bytes } => {
                if let Err(err) = self.link.write(chan, &bytes).await {
                    self.emit(PumpEvent::LinkError(err));
                }
            }
            Output::Subscribe(chan) => {
                if let Err(err) = self.link.subscribe(chan).await {
                    self.emit(PumpEvent::LinkError(err));
                }
            }
            Output::Read(chan) => match self.link.read(chan).await {
                Ok(bytes) => self.reads.push_back(Input::Rx { chan, bytes }),
                Err(err) => self.emit(PumpEvent::LinkError(err)),
            },
            Output::SetTimer { id, after } => {
                self.timers.insert(id, tokio::time::Instant::now() + after);
            }
            Output::CancelTimer(id) => {
                self.timers.remove(&id);
            }
            Output::Event(ev) => self.emit(PumpEvent::Event(ev)),
            Output::Done { id, result } => {
                // An id nobody is waiting for (already answered, or the requester went
                // away) is ignored.
                if let Some(reply) = self.pending.remove(&id) {
                    let _ = reply.send(result);
                }
            }
            Output::Disconnect => {
                self.link.disconnect().await;
                return Flow::Disconnect;
            }
        }
        Flow::Continue
    }

    /// Reports `event`; a dropped receiver means nobody wants it.
    fn emit(&self, event: PumpEvent<D::Ev>) {
        let _ = self.events.send(event);
    }

    /// The clock as the driver sees it: milliseconds since the pump was created.
    fn now(&self) -> Instant {
        Instant::ZERO + (tokio::time::Instant::now() - self.origin)
    }
}

/// Waits for `deadline` and yields its timer id; waits forever if there is none.
async fn wait_timer(deadline: Option<(TimerId, tokio::time::Instant)>) -> TimerId {
    match deadline {
        Some((id, at)) => {
            tokio::time::sleep_until(at).await;
            id
        }
        None => pending().await,
    }
}

/// Waits for the next tick; waits forever if ticks are off.
async fn wait_tick(tick: Option<&mut Interval>) {
    match tick {
        Some(tick) => {
            tick.tick().await;
        }
        None => pending().await,
    }
}
