//! The pump end to end: `FakeDriver` through a `ChannelLink` to a `FakePeer`, on paused time.
//!
//! Every test runs the pump and its scenario on the one test task with `join!`; nothing is
//! spawned and no real time passes. The "device" is served explicitly by the scenario where
//! a test needs it ([`serve`]), so the scenario can also hold the device end still.

use std::cell::RefCell;
use std::future::Future;
use std::pin::pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use junk_core::{
    Bytes, Channel, ChannelSet, Driver, GattMap, Input, Instant, Link, LinkError, LinkEvent,
    Output, Outputs, ProtoError,
};
use junk_fake::{CMD, EVT, EXTRA, Ev, FakeDriver, FakePeer, GATT, Req, Resp, TIMEOUT};
use junk_pump::channel_link::{ChannelLink, DEFAULT_MTU, PeerSide};
use junk_pump::{Events, Handle, Pump, PumpConfig, PumpError, PumpEvent, RequestError, Stop};
use tokio::sync::mpsc;
use tokio::task::yield_now;
use tokio::time::advance;

const MS: Duration = Duration::from_millis(1);

/// Everything a test needs: a pump over a channel link, and the device end of it.
struct Rig<D: Driver> {
    pump: Pump<D, ChannelLink>,
    handle: Handle<D>,
    events: Events<D>,
    peer: PeerSide,
    fake: FakePeer,
}

fn rig<D: Driver>(driver: D, config: PumpConfig) -> Rig<D> {
    let (link, peer) = ChannelLink::pair();
    let (pump, handle, events) = Pump::new(driver, link, config);
    Rig {
        pump,
        handle,
        events,
        peer,
        fake: FakePeer::new(),
    }
}

fn channels(chans: &[Channel]) -> ChannelSet {
    chans.iter().copied().collect()
}

fn all() -> ChannelSet {
    channels(&[CMD, EVT, EXTRA])
}

fn connected<Ev>() -> PumpEvent<Ev> {
    PumpEvent::Connected {
        resolved: all(),
        mtu: DEFAULT_MTU,
    }
}

/// Every event reported so far.
fn drain<Ev>(events: &mut mpsc::UnboundedReceiver<PumpEvent<Ev>>) -> Vec<PumpEvent<Ev>> {
    let mut out = Vec::new();
    while let Ok(event) = events.try_recv() {
        out.push(event);
    }
    out
}

/// Polls `fut` once, with a waker that does nothing.
fn poll_once<F: Future>(fut: F) -> Poll<F::Output> {
    let mut fut = pin!(fut);
    fut.as_mut().poll(&mut Context::from_waker(Waker::noop()))
}

/// Lets everything else on the task run for a bit, without moving the clock.
async fn settle() {
    for _ in 0..4 {
        yield_now().await;
    }
}

/// Moves the clock by `by` and lets the pump act on it.
async fn advance_by(by: Duration) {
    advance(by).await;
    settle().await;
}

/// Serves the next `n` writes the way the device would, each one answered before the next
/// is read, and returns them.
async fn serve(peer: &mut PeerSide, fake: &mut FakePeer, n: usize) -> Vec<(Channel, Bytes)> {
    let mut writes = Vec::new();
    for _ in 0..n {
        let (chan, bytes) = peer.next_write().await.expect("the link is gone");
        // The driver serialises: nothing else may be waiting while this one is unanswered.
        assert!(poll_once(peer.next_write()).is_pending());
        for (chan, bytes) in fake.on_write(chan, &bytes) {
            assert!(peer.notify(chan, bytes));
        }
        writes.push((chan, bytes));
    }
    writes
}

/// A driver that records every input it is fed, then delegates.
struct Recording<D: Driver> {
    inner: D,
    log: Log<D>,
}

type Log<D> = Rc<RefCell<Vec<Input<<D as Driver>::Req>>>>;

fn recording<D: Driver>(inner: D) -> (Recording<D>, Log<D>) {
    let log = Log::<D>::default();
    let driver = Recording {
        inner,
        log: Rc::clone(&log),
    };
    (driver, log)
}

impl<D: Driver> Driver for Recording<D>
where
    D::Req: Clone,
{
    type Req = D::Req;
    type Resp = D::Resp;
    type Ev = D::Ev;

    const GATT: &'static GattMap = D::GATT;

    fn handle(&mut self, input: Input<D::Req>, out: &mut Outputs<D::Resp, D::Ev>) {
        self.log.borrow_mut().push(input.clone());
        self.inner.handle(input, out);
    }
}

/// A driver that replays a fixed list of outputs on `Connected` and answers every request
/// with `Ok(())`.
struct Scripted {
    on_connected: Vec<Output<(), &'static str>>,
}

impl Driver for Scripted {
    type Req = ();
    type Resp = ();
    type Ev = &'static str;

    const GATT: &'static GattMap = &GATT;

    fn handle(&mut self, input: Input<()>, out: &mut Outputs<(), &'static str>) {
        match input {
            Input::Connected { .. } => {
                for output in self.on_connected.clone() {
                    out.push(output);
                }
            }
            Input::Request { id, .. } => out.done(id, Ok(())),
            Input::Disconnected | Input::Rx { .. } | Input::Tick { .. } | Input::Timer(_) => {}
        }
    }
}

#[tokio::test(start_paused = true)]
async fn round_trips() {
    let Rig {
        mut pump,
        handle,
        mut events,
        mut peer,
        mut fake,
    } = rig(FakeDriver::new(), PumpConfig::default());

    let (stop, ()) = tokio::join!(pump.run(), async {
        let (resp, writes) =
            tokio::join!(handle.request(Req::Ping), serve(&mut peer, &mut fake, 1));
        assert_eq!(resp, Ok(Resp::Pong));
        assert_eq!(writes, vec![(CMD, vec![0x01, 0x00])]);

        let (resp, _) = tokio::join!(handle.request(Req::Get(7)), serve(&mut peer, &mut fake, 1));
        assert_eq!(resp, Ok(Resp::Value(fake.value(7))));

        let payload: Bytes = (0..30).collect();
        let (resp, _) = tokio::join!(
            handle.request(Req::Echo(payload.clone())),
            serve(&mut peer, &mut fake, 1)
        );
        assert_eq!(resp, Ok(Resp::Echo(payload)));

        handle.shutdown();
    });
    assert_eq!(stop, Ok(Stop::Shutdown));
    assert_eq!(peer.subscriptions(), vec![EVT]);
    assert_eq!(
        drain(&mut events),
        vec![connected(), PumpEvent::Disconnected]
    );
}

#[tokio::test(start_paused = true)]
async fn concurrent_requests_are_answered_one_at_a_time() {
    let Rig {
        mut pump,
        handle,
        mut peer,
        mut fake,
        ..
    } = rig(FakeDriver::new(), PumpConfig::default());

    let (stop, ()) = tokio::join!(pump.run(), async {
        let (ping, get, echo, writes) = tokio::join!(
            handle.request(Req::Ping),
            handle.request(Req::Get(7)),
            handle.request(Req::Echo(vec![1, 2, 3])),
            serve(&mut peer, &mut fake, 3),
        );
        assert_eq!(ping, Ok(Resp::Pong));
        assert_eq!(get, Ok(Resp::Value(0x0707_0707)));
        assert_eq!(echo, Ok(Resp::Echo(vec![1, 2, 3])));
        assert_eq!(
            writes,
            vec![
                (CMD, vec![0x01, 0x00]),
                (CMD, vec![0x02, 0x01, 7]),
                (CMD, vec![0x03, 0x03, 1, 2, 3]),
            ]
        );
        // Nothing else was written.
        assert!(poll_once(peer.next_write()).is_pending());
        handle.shutdown();
    });
    assert_eq!(stop, Ok(Stop::Shutdown));
}

#[tokio::test(start_paused = true)]
async fn timeout_comes_from_the_driver_after_exactly_its_timer() {
    let Rig {
        mut pump,
        handle,
        mut peer,
        mut fake,
        ..
    } = rig(FakeDriver::new(), PumpConfig::default());
    fake.set_mute(true);

    let (stop, ()) = tokio::join!(pump.run(), async {
        let mut req = pin!(handle.request(Req::Ping));
        assert!(poll_once(req.as_mut()).is_pending());
        serve(&mut peer, &mut fake, 1).await;

        advance_by(TIMEOUT.checked_sub(MS).unwrap()).await;
        assert!(poll_once(req.as_mut()).is_pending());

        advance_by(MS).await;
        assert_eq!(
            poll_once(req.as_mut()),
            Poll::Ready(Err(RequestError::Proto(ProtoError::Timeout)))
        );
        handle.shutdown();
    });
    assert_eq!(stop, Ok(Stop::Shutdown));
}

#[tokio::test(start_paused = true)]
async fn a_timer_cancelled_by_a_reply_in_the_same_cycle_never_fires() {
    let (driver, log) = recording(FakeDriver::new());
    let Rig {
        mut pump,
        handle,
        mut peer,
        mut fake,
        ..
    } = rig(driver, PumpConfig::default());
    fake.set_mute(true);

    let (stop, ()) = tokio::join!(pump.run(), async {
        let mut req = pin!(handle.request(Req::Ping));
        assert!(poll_once(req.as_mut()).is_pending());
        serve(&mut peer, &mut fake, 1).await;

        // Both the reply and the deadline become ready before the pump runs again.
        assert!(peer.notify(EVT, vec![0x81, 0x00]));
        advance_by(TIMEOUT + MS).await;

        assert_eq!(req.await, Ok(Resp::Pong));
        handle.shutdown();
    });
    assert_eq!(stop, Ok(Stop::Shutdown));

    let log = log.borrow();
    assert!(log.contains(&Input::Rx {
        chan: EVT,
        bytes: vec![0x81, 0x00],
    }));
    assert!(
        !log.iter().any(|input| matches!(input, Input::Timer(_))),
        "{log:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn device_disconnect_fails_the_request_and_the_pump_can_reconnect() {
    let Rig {
        mut pump,
        handle,
        mut events,
        mut peer,
        mut fake,
    } = rig(FakeDriver::new(), PumpConfig::default());
    fake.set_mute(true);

    let (stop, ()) = tokio::join!(pump.run(), async {
        let mut req = pin!(handle.request(Req::Ping));
        assert!(poll_once(req.as_mut()).is_pending());
        serve(&mut peer, &mut fake, 1).await;
        peer.disconnect();
        assert_eq!(
            req.await,
            Err(RequestError::Proto(ProtoError::Disconnected))
        );
    });
    assert_eq!(stop, Ok(Stop::Disconnected));
    assert!(!peer.is_connected());
    assert_eq!(
        drain(&mut events),
        vec![connected(), PumpEvent::Disconnected]
    );

    // Same pump, same driver, same link: a new session.
    fake.set_mute(false);
    let (stop, ()) = tokio::join!(pump.run(), async {
        let (resp, _) = tokio::join!(handle.request(Req::Ping), serve(&mut peer, &mut fake, 1));
        assert_eq!(resp, Ok(Resp::Pong));
        handle.shutdown();
    });
    assert_eq!(stop, Ok(Stop::Shutdown));
    assert_eq!(
        drain(&mut events),
        vec![connected(), PumpEvent::Disconnected]
    );
    assert_eq!(peer.subscriptions(), vec![EVT, EVT]);
}

#[tokio::test(start_paused = true)]
async fn driver_requested_disconnect() {
    let Rig {
        mut pump,
        mut events,
        mut peer,
        ..
    } = rig(FakeDriver::new(), PumpConfig::default());
    peer.set_resolved(channels(&[CMD, EXTRA]));

    assert_eq!(pump.run().await, Ok(Stop::Disconnected));
    assert!(!peer.is_connected());
    assert_eq!(
        drain(&mut events),
        vec![
            PumpEvent::Connected {
                resolved: channels(&[CMD, EXTRA]),
                mtu: DEFAULT_MTU,
            },
            PumpEvent::Event(Ev::MissingChannels(channels(&[EVT]))),
            PumpEvent::Disconnected,
        ]
    );
    assert!(peer.subscriptions().is_empty());
}

#[tokio::test(start_paused = true)]
async fn shutdown_fails_the_request_and_drops_the_link() {
    let Rig {
        mut pump,
        handle,
        mut events,
        mut peer,
        mut fake,
    } = rig(FakeDriver::new(), PumpConfig::default());
    fake.set_mute(true);

    let (stop, ()) = tokio::join!(pump.run(), async {
        let mut req = pin!(handle.request(Req::Ping));
        assert!(poll_once(req.as_mut()).is_pending());
        serve(&mut peer, &mut fake, 1).await;
        handle.shutdown();
        handle.shutdown();
        assert_eq!(
            req.await,
            Err(RequestError::Proto(ProtoError::Disconnected))
        );
    });
    assert_eq!(stop, Ok(Stop::Shutdown));
    assert!(!peer.is_connected());
    assert_eq!(
        drain(&mut events),
        vec![connected(), PumpEvent::Disconnected]
    );

    // Shut down is shut down: no new session, no new requests.
    assert_eq!(pump.run().await, Ok(Stop::Shutdown));
    assert!(drain(&mut events).is_empty());
    assert_eq!(handle.request(Req::Ping).await, Err(RequestError::Stopped));
}

#[tokio::test(start_paused = true)]
async fn dropping_every_handle_shuts_down() {
    let Rig {
        mut pump,
        handle,
        mut events,
        peer,
        ..
    } = rig(FakeDriver::new(), PumpConfig::default());
    let other = handle.clone();

    let (stop, ()) = tokio::join!(pump.run(), async {
        drop(handle);
        settle().await;
        drop(other);
    });
    assert_eq!(stop, Ok(Stop::Shutdown));
    assert!(!peer.is_connected());
    assert_eq!(
        drain(&mut events),
        vec![connected(), PumpEvent::Disconnected]
    );
}

#[tokio::test(start_paused = true)]
async fn unsolicited_frames_become_events() {
    let Rig {
        mut pump,
        handle,
        mut events,
        peer,
        mut fake,
    } = rig(FakeDriver::new(), PumpConfig::default());

    let (stop, ()) = tokio::join!(pump.run(), async {
        assert_eq!(events.recv().await, Some(connected()));
        let (chan, bytes) = fake.heartbeat();
        assert!(peer.notify(chan, bytes));
        assert_eq!(
            events.recv().await,
            Some(PumpEvent::Event(Ev::Heartbeat(1)))
        );
        handle.shutdown();
    });
    assert_eq!(stop, Ok(Stop::Shutdown));
}

#[tokio::test(start_paused = true)]
async fn ticks_carry_millis_since_the_pump_was_created() {
    let (driver, log) = recording(FakeDriver::new());
    let Rig {
        mut pump,
        handle,
        peer: _peer,
        ..
    } = rig(
        driver,
        PumpConfig {
            tick_every: Some(100 * MS),
        },
    );
    // The clock starts at creation, not at `run`.
    advance(10 * MS).await;

    let (stop, ()) = tokio::join!(pump.run(), async {
        for _ in 0..7 {
            advance_by(50 * MS).await;
        }
        handle.shutdown();
    });
    assert_eq!(stop, Ok(Stop::Shutdown));

    let ticks: Vec<Instant> = log
        .borrow()
        .iter()
        .filter_map(|input| match input {
            Input::Tick { now } => Some(*now),
            _ => None,
        })
        .collect();
    assert_eq!(ticks, vec![Instant(110), Instant(210), Instant(310)]);
}

/// What the device end, the events channel and the driver saw, in the order they saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Obs {
    Write(Channel, Bytes),
    Subscribe(Channel),
    Read(Channel),
    Event(&'static str),
    /// An `Rx` fed to the driver, noted by [`Observed`].
    Rx(Channel, Bytes),
}

/// A `ChannelLink` that logs every write and subscription, noting first any event the
/// pump reported since the previous one, so the log has the pump's true order.
struct Logged {
    inner: ChannelLink,
    events: Rc<RefCell<Option<Events<Scripted>>>>,
    log: Rc<RefCell<Vec<Obs>>>,
}

impl Logged {
    fn note_events(&self) {
        if let Some(events) = self.events.borrow_mut().as_mut() {
            for event in drain(events) {
                if let PumpEvent::Event(name) = event {
                    self.log.borrow_mut().push(Obs::Event(name));
                }
            }
        }
    }
}

impl Link for Logged {
    async fn connect(&mut self, gatt: &GattMap) -> Result<(ChannelSet, u16), LinkError> {
        self.inner.connect(gatt).await
    }

    async fn write(&mut self, chan: Channel, bytes: &[u8]) -> Result<(), LinkError> {
        self.note_events();
        self.log.borrow_mut().push(Obs::Write(chan, bytes.to_vec()));
        self.inner.write(chan, bytes).await
    }

    async fn subscribe(&mut self, chan: Channel) -> Result<(), LinkError> {
        self.note_events();
        self.log.borrow_mut().push(Obs::Subscribe(chan));
        self.inner.subscribe(chan).await
    }

    async fn read(&mut self, chan: Channel) -> Result<Bytes, LinkError> {
        self.note_events();
        self.log.borrow_mut().push(Obs::Read(chan));
        self.inner.read(chan).await
    }

    async fn disconnect(&mut self) {
        self.inner.disconnect().await;
    }

    async fn next(&mut self) -> LinkEvent {
        self.inner.next().await
    }
}

/// A driver that notes every `Rx` it is fed in the same log a [`Logged`] link writes,
/// then delegates, so the log has the true order of link operations and inputs.
struct Observed<D: Driver> {
    inner: D,
    log: Rc<RefCell<Vec<Obs>>>,
}

impl<D: Driver> Driver for Observed<D> {
    type Req = D::Req;
    type Resp = D::Resp;
    type Ev = D::Ev;

    const GATT: &'static GattMap = D::GATT;

    fn handle(&mut self, input: Input<D::Req>, out: &mut Outputs<D::Resp, D::Ev>) {
        if let Input::Rx { chan, bytes } = &input {
            self.log.borrow_mut().push(Obs::Rx(*chan, bytes.clone()));
        }
        self.inner.handle(input, out);
    }
}

#[tokio::test(start_paused = true)]
async fn outputs_are_applied_in_order() {
    let (inner, mut peer) = ChannelLink::pair();
    let events_slot = Rc::new(RefCell::new(None));
    let log = Rc::new(RefCell::new(Vec::new()));
    let link = Logged {
        inner,
        events: Rc::clone(&events_slot),
        log: Rc::clone(&log),
    };
    let driver = Scripted {
        on_connected: vec![
            Output::Tx {
                chan: CMD,
                bytes: vec![0xa],
            },
            Output::Subscribe(EVT),
            Output::Tx {
                chan: CMD,
                bytes: vec![0xb],
            },
            Output::Event("x"),
        ],
    };
    let (mut pump, handle, events) = Pump::new(driver, link, PumpConfig::default());
    *events_slot.borrow_mut() = Some(events);

    let (stop, ()) = tokio::join!(pump.run(), async {
        assert_eq!(peer.next_write().await, Some((CMD, vec![0xa])));
        assert_eq!(peer.next_write().await, Some((CMD, vec![0xb])));
        handle.shutdown();
    });
    assert_eq!(stop, Ok(Stop::Shutdown));

    let mut events = events_slot.borrow_mut().take().unwrap();
    for event in drain(&mut events) {
        if let PumpEvent::Event(name) = event {
            log.borrow_mut().push(Obs::Event(name));
        }
    }
    assert_eq!(
        *log.borrow(),
        vec![
            Obs::Write(CMD, vec![0xa]),
            Obs::Subscribe(EVT),
            Obs::Write(CMD, vec![0xb]),
            Obs::Event("x"),
        ]
    );
    assert_eq!(peer.subscriptions(), vec![EVT]);
}

#[tokio::test(start_paused = true)]
async fn a_read_comes_back_as_the_next_input_after_the_steps_outputs() {
    let (inner, mut peer) = ChannelLink::pair();
    peer.set_value(EXTRA, vec![0x5a, 0x5b]);
    let events_slot = Rc::new(RefCell::new(None));
    let log = Rc::new(RefCell::new(Vec::new()));
    let link = Logged {
        inner,
        events: Rc::clone(&events_slot),
        log: Rc::clone(&log),
    };
    // The read comes first in the step, the write after it: the write must still reach
    // the peer before the driver sees the value (invariant 3).
    let driver = Observed {
        inner: Scripted {
            on_connected: vec![
                Output::Read(EXTRA),
                Output::Tx {
                    chan: CMD,
                    bytes: vec![0xb],
                },
            ],
        },
        log: Rc::clone(&log),
    };
    let (mut pump, handle, events) = Pump::new(driver, link, PumpConfig::default());
    *events_slot.borrow_mut() = Some(events);

    let (stop, ()) = tokio::join!(pump.run(), async {
        assert_eq!(peer.next_write().await, Some((CMD, vec![0xb])));
        settle().await;
        handle.shutdown();
    });
    assert_eq!(stop, Ok(Stop::Shutdown));
    assert_eq!(
        *log.borrow(),
        vec![
            Obs::Read(EXTRA),
            Obs::Write(CMD, vec![0xb]),
            Obs::Rx(EXTRA, vec![0x5a, 0x5b]),
        ]
    );
    assert_eq!(peer.reads(), vec![EXTRA]);

    // The link drained the earlier events as it noted them; no read failed.
    let mut events = events_slot.borrow_mut().take().unwrap();
    assert_eq!(drain(&mut events), vec![PumpEvent::Disconnected]);
}

#[tokio::test(start_paused = true)]
async fn a_failed_read_is_reported_and_feeds_nothing() {
    let (driver, log) = recording(Scripted {
        on_connected: vec![Output::Read(EXTRA), Output::Read(Channel(200))],
    });
    let Rig {
        mut pump,
        handle,
        mut events,
        peer,
        ..
    } = rig(driver, PumpConfig::default());

    let (stop, ()) = tokio::join!(pump.run(), async {
        assert_eq!(handle.request(()).await, Ok(()));
        handle.shutdown();
    });
    assert_eq!(stop, Ok(Stop::Shutdown));
    assert_eq!(
        drain(&mut events),
        vec![
            connected(),
            PumpEvent::LinkError(LinkError::Io("no value".into())),
            PumpEvent::LinkError(LinkError::UnknownChannel(Channel(200))),
            PumpEvent::Disconnected,
        ]
    );
    assert!(
        !log.borrow()
            .iter()
            .any(|input| matches!(input, Input::Rx { .. })),
        "{:?}",
        log.borrow()
    );
    assert_eq!(peer.reads(), vec![EXTRA]);
}

#[tokio::test(start_paused = true)]
async fn a_failed_write_is_reported_and_the_loop_goes_on() {
    let driver = Scripted {
        on_connected: vec![
            Output::Tx {
                chan: Channel(200),
                bytes: vec![1],
            },
            Output::Subscribe(EVT),
        ],
    };
    let Rig {
        mut pump,
        handle,
        mut events,
        peer,
        ..
    } = rig(driver, PumpConfig::default());

    let (stop, ()) = tokio::join!(pump.run(), async {
        assert_eq!(handle.request(()).await, Ok(()));
        handle.shutdown();
    });
    assert_eq!(stop, Ok(Stop::Shutdown));
    assert_eq!(
        drain(&mut events),
        vec![
            connected(),
            PumpEvent::LinkError(LinkError::UnknownChannel(Channel(200))),
            PumpEvent::Disconnected,
        ]
    );
    assert_eq!(peer.subscriptions(), vec![EVT]);
}

#[tokio::test(start_paused = true)]
async fn connect_failure_and_requests_that_never_run() {
    let Rig {
        mut pump,
        handle,
        mut events,
        mut peer,
        ..
    } = rig(FakeDriver::new(), PumpConfig::default());
    peer.set_connect_error(Some(LinkError::Io("no adapter".into())));

    let mut before = pin!(handle.request(Req::Ping));
    assert!(poll_once(before.as_mut()).is_pending());

    let err = pump.run().await.unwrap_err();
    assert_eq!(err, PumpError::Connect(LinkError::Io("no adapter".into())));
    assert!(err.to_string().contains("no adapter"));
    assert!(drain(&mut events).is_empty());
    assert!(!peer.is_connected());

    let mut after = pin!(handle.request(Req::Get(1)));
    assert!(poll_once(after.as_mut()).is_pending());

    drop(pump);
    assert_eq!(before.await, Err(RequestError::Stopped));
    assert_eq!(after.await, Err(RequestError::Stopped));
    assert_eq!(handle.request(Req::Ping).await, Err(RequestError::Stopped));
}

#[tokio::test(start_paused = true)]
async fn into_parts_hands_back_the_driver_and_the_link() {
    let Rig {
        mut pump,
        handle,
        mut peer,
        mut fake,
        ..
    } = rig(FakeDriver::new(), PumpConfig::default());

    let (stop, ()) = tokio::join!(pump.run(), async {
        let (resp, _) = tokio::join!(handle.request(Req::Ping), serve(&mut peer, &mut fake, 1));
        assert_eq!(resp, Ok(Resp::Pong));
        handle.shutdown();
    });
    assert_eq!(stop, Ok(Stop::Shutdown));
    assert_eq!(pump.driver().mtu(), None);

    let (driver, mut link) = pump.into_parts();
    assert_eq!(driver, FakeDriver::new());
    assert_eq!(link.write(CMD, &[1]).await, Err(LinkError::NotConnected));
}

#[test]
fn errors_display_and_convert() {
    assert_eq!(
        RequestError::from(ProtoError::Timeout),
        RequestError::Proto(ProtoError::Timeout)
    );
    let proto = RequestError::Proto(ProtoError::Timeout);
    assert!(proto.to_string().contains(&ProtoError::Timeout.to_string()));
    assert!(std::error::Error::source(&proto).is_some());
    assert!(!RequestError::Stopped.to_string().is_empty());
    assert!(std::error::Error::source(&RequestError::Stopped).is_none());

    let connect = PumpError::Connect(LinkError::NotConnected);
    assert!(
        connect
            .to_string()
            .contains(&LinkError::NotConnected.to_string())
    );
    assert!(std::error::Error::source(&connect).is_some());
}
