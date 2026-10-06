//! The six invariants of SPEC §3.1, pinned on the fake device family without any I/O.

use std::collections::VecDeque;

use junk_core::{
    Battery, Bytes, Channel, ChannelSet, Driver, Input, Instant, Output, Outputs, Percent,
    ProtoError, ReqId, TimerId,
};
use junk_fake::{
    CMD, EVT, EXTRA, Ev, FakeDriver, FakePeer, Frame, GATT, Req, Resp, TIMEOUT, TIMEOUT_TIMER,
};

type Out = Output<Resp, Ev>;

fn step(driver: &mut FakeDriver, input: Input<Req>) -> Vec<Out> {
    let mut out = Outputs::new();
    driver.handle(input, &mut out);
    out.into_vec()
}

fn channels(chans: &[Channel]) -> ChannelSet {
    chans.iter().copied().collect()
}

fn connected(chans: &[Channel]) -> Input<Req> {
    Input::Connected {
        resolved: channels(chans),
        mtu: 23,
    }
}

fn connect(driver: &mut FakeDriver) -> Vec<Out> {
    step(driver, connected(&[CMD, EVT, EXTRA]))
}

fn request(id: u32, req: Req) -> Input<Req> {
    Input::Request {
        id: ReqId(id),
        req,
        now: Instant::ZERO,
    }
}

fn rx(chan: Channel, bytes: &[u8]) -> Input<Req> {
    Input::Rx {
        chan,
        bytes: bytes.to_vec(),
    }
}

fn tx(bytes: &[u8]) -> Out {
    Output::Tx {
        chan: CMD,
        bytes: bytes.to_vec(),
    }
}

fn set_timer() -> Out {
    Output::SetTimer {
        id: TIMEOUT_TIMER,
        after: TIMEOUT,
    }
}

fn cancel_timer() -> Out {
    Output::CancelTimer(TIMEOUT_TIMER)
}

fn done(id: u32, result: Result<Resp, ProtoError>) -> Out {
    Output::Done {
        id: ReqId(id),
        result,
    }
}

fn unparsed(chan: Channel, bytes: &[u8]) -> Out {
    Output::Event(Ev::Unparsed {
        chan,
        bytes: bytes.to_vec(),
    })
}

/// Hands every `Tx` in `outs` to the peer and returns its notifications as driver inputs.
fn forward(peer: &mut FakePeer, outs: &[Out]) -> Vec<Input<Req>> {
    outs.iter()
        .filter_map(|out| match out {
            Output::Tx { chan, bytes } => Some(peer.on_write(*chan, bytes)),
            Output::Subscribe(_)
            | Output::Read(_)
            | Output::SetTimer { .. }
            | Output::CancelTimer(_)
            | Output::Event(_)
            | Output::Done { .. }
            | Output::Disconnect => None,
        })
        .flatten()
        .map(|(chan, bytes)| Input::Rx { chan, bytes })
        .collect()
}

/// A tiny xorshift64, so the randomised tests need no dev-dependencies.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: usize) -> usize {
        let n = u64::try_from(n).unwrap();
        usize::try_from(self.next_u64() % n).unwrap()
    }

    fn byte(&mut self) -> u8 {
        self.next_u64().to_le_bytes()[0]
    }

    fn bytes(&mut self, len: usize) -> Bytes {
        (0..len).map(|_| self.byte()).collect()
    }
}

#[test]
fn gatt_is_the_drivers_map() {
    assert_eq!(<FakeDriver as Driver>::GATT, &GATT);
    assert_eq!(GATT.required(), channels(&[CMD, EVT]));
}

#[test]
fn connected_subscribes_or_refuses() {
    let mut driver = FakeDriver::new();
    assert_eq!(driver.mtu(), None);
    assert_eq!(connect(&mut driver), vec![Output::Subscribe(EVT)]);
    assert_eq!(driver.mtu(), Some(23));

    let mut driver = FakeDriver::new();
    assert_eq!(
        step(&mut driver, connected(&[CMD, EVT])),
        vec![Output::Subscribe(EVT)]
    );

    let mut driver = FakeDriver::new();
    assert_eq!(
        step(&mut driver, connected(&[CMD, EXTRA])),
        vec![
            Output::Event(Ev::MissingChannels(channels(&[EVT]))),
            Output::Disconnect,
        ]
    );
    assert_eq!(driver.mtu(), None);
    assert_eq!(
        step(&mut driver, request(1, Req::Ping)),
        vec![done(1, Err(ProtoError::Disconnected))]
    );
    assert_eq!(
        step(&mut driver, request(2, Req::Get(3))),
        vec![done(2, Err(ProtoError::Disconnected))]
    );
}

#[test]
fn request_before_connected_is_refused() {
    let mut driver = FakeDriver::new();
    assert_eq!(
        step(&mut driver, request(1, Req::Ping)),
        vec![done(1, Err(ProtoError::Disconnected))]
    );
}

#[test]
fn ping_round_trip() {
    let mut driver = FakeDriver::new();
    let mut peer = FakePeer::new();
    connect(&mut driver);

    let outs = step(&mut driver, request(1, Req::Ping));
    assert_eq!(outs, vec![tx(&[0x01, 0x00]), set_timer()]);

    let replies = forward(&mut peer, &outs);
    assert_eq!(replies, vec![rx(EVT, &[0x81, 0x00])]);
    let [reply] = replies.try_into().unwrap();
    assert_eq!(
        step(&mut driver, reply),
        vec![cancel_timer(), done(1, Ok(Resp::Pong))]
    );
}

#[test]
fn get_round_trip_and_key_mismatch() {
    let mut driver = FakeDriver::new();
    let mut peer = FakePeer::new();
    connect(&mut driver);

    let outs = step(&mut driver, request(1, Req::Get(0x11)));
    assert_eq!(outs, vec![tx(&[0x02, 0x01, 0x11]), set_timer()]);
    for reply in forward(&mut peer, &outs) {
        assert_eq!(
            step(&mut driver, reply),
            vec![cancel_timer(), done(1, Ok(Resp::Value(0x1111_1111)))]
        );
    }

    peer.set_value(0x11, 0xdead_beef);
    let outs = step(&mut driver, request(2, Req::Get(0x11)));
    for reply in forward(&mut peer, &outs) {
        assert_eq!(
            step(&mut driver, reply),
            vec![cancel_timer(), done(2, Ok(Resp::Value(0xdead_beef)))]
        );
    }

    step(&mut driver, request(3, Req::Get(0x11)));
    assert_eq!(
        step(&mut driver, rx(EVT, &[0x82, 0x05, 0x12, 1, 2, 3, 4])),
        vec![
            cancel_timer(),
            done(3, Err(ProtoError::Malformed("get: key mismatch")))
        ]
    );
}

#[test]
fn echo_reassembles_chunks() {
    let mut driver = FakeDriver::new();
    let mut peer = FakePeer::new();
    connect(&mut driver);

    let payload: Bytes = (1..=10).collect();
    let outs = step(&mut driver, request(1, Req::Echo(payload.clone())));
    let mut expected = vec![0x03, 10];
    expected.extend_from_slice(&payload);
    assert_eq!(outs, vec![tx(&expected), set_timer()]);

    let replies = forward(&mut peer, &outs);
    assert_eq!(
        replies,
        vec![
            rx(EVT, &[0x83, 6, 0, 3, 1, 2, 3, 4]),
            rx(EVT, &[0x83, 6, 1, 3, 5, 6, 7, 8]),
            rx(EVT, &[0x83, 4, 2, 3, 9, 10]),
        ]
    );
    let mut replies = replies.into_iter();
    assert_eq!(step(&mut driver, replies.next().unwrap()), vec![]);
    assert_eq!(step(&mut driver, replies.next().unwrap()), vec![]);
    assert_eq!(
        step(&mut driver, replies.next().unwrap()),
        vec![cancel_timer(), done(1, Ok(Resp::Echo(payload)))]
    );

    peer.set_chunk(1);
    let payload: Bytes = vec![5, 4, 3, 2, 1];
    let outs = step(&mut driver, request(2, Req::Echo(payload.clone())));
    let replies = forward(&mut peer, &outs);
    assert_eq!(replies.len(), 5);
    let mut outs = Vec::new();
    for reply in replies {
        outs.extend(step(&mut driver, reply));
    }
    assert_eq!(outs, vec![cancel_timer(), done(2, Ok(Resp::Echo(payload)))]);

    peer.set_chunk(0);
    let outs = step(&mut driver, request(3, Req::Echo(vec![])));
    let replies = forward(&mut peer, &outs);
    assert_eq!(replies, vec![rx(EVT, &[0x83, 2, 0, 1])]);
    for reply in replies {
        assert_eq!(
            step(&mut driver, reply),
            vec![cancel_timer(), done(3, Ok(Resp::Echo(vec![])))]
        );
    }
}

#[test]
fn echo_rejects_bad_chunks_and_long_payloads() {
    let mut driver = FakeDriver::new();
    connect(&mut driver);

    step(&mut driver, request(1, Req::Echo(vec![1, 2, 3])));
    assert_eq!(
        step(&mut driver, rx(EVT, &[0x83, 3, 1, 3, 0xaa])),
        vec![
            cancel_timer(),
            done(1, Err(ProtoError::Malformed("echo: out of order")))
        ]
    );

    step(&mut driver, request(2, Req::Echo(vec![1, 2, 3])));
    assert_eq!(step(&mut driver, rx(EVT, &[0x83, 3, 0, 3, 0xaa])), vec![]);
    assert_eq!(
        step(&mut driver, rx(EVT, &[0x83, 3, 2, 3, 0xaa])),
        vec![
            cancel_timer(),
            done(2, Err(ProtoError::Malformed("echo: out of order")))
        ]
    );

    step(&mut driver, request(3, Req::Echo(vec![1, 2, 3])));
    assert_eq!(
        step(&mut driver, rx(EVT, &[0x83, 2, 0, 0])),
        vec![
            cancel_timer(),
            done(3, Err(ProtoError::Malformed("echo: empty")))
        ]
    );

    assert_eq!(
        step(&mut driver, request(4, Req::Echo(vec![0; 254]))),
        vec![done(
            4,
            Err(ProtoError::Unsupported("echo: payload too long"))
        )]
    );
    // The driver is idle: the next request starts at once.
    assert_eq!(
        step(&mut driver, request(5, Req::Ping)),
        vec![tx(&[0x01, 0x00]), set_timer()]
    );
    // And an oversized request while busy is refused at once too, without touching the queue.
    assert_eq!(
        step(&mut driver, request(6, Req::Echo(vec![0; 300]))),
        vec![done(
            6,
            Err(ProtoError::Unsupported("echo: payload too long"))
        )]
    );
    assert_eq!(
        step(&mut driver, rx(EVT, &[0x81, 0x00])),
        vec![cancel_timer(), done(5, Ok(Resp::Pong))]
    );
}

#[test]
fn requests_queue_and_start_in_order() {
    let mut driver = FakeDriver::new();
    let mut peer = FakePeer::new();
    connect(&mut driver);

    let first = step(&mut driver, request(1, Req::Ping));
    assert_eq!(first, vec![tx(&[0x01, 0x00]), set_timer()]);
    assert_eq!(step(&mut driver, request(2, Req::Get(7))), vec![]);
    assert_eq!(step(&mut driver, request(3, Req::Echo(vec![9]))), vec![]);

    let mut pending: VecDeque<Input<Req>> = forward(&mut peer, &first).into();
    let mut answered = Vec::new();
    while let Some(reply) = pending.pop_front() {
        let outs = step(&mut driver, reply);
        pending.extend(forward(&mut peer, &outs));
        answered.push(outs);
    }
    assert_eq!(
        answered,
        vec![
            vec![
                cancel_timer(),
                done(1, Ok(Resp::Pong)),
                tx(&[0x02, 0x01, 7]),
                set_timer(),
            ],
            vec![
                cancel_timer(),
                done(2, Ok(Resp::Value(0x0707_0707))),
                tx(&[0x03, 0x01, 9]),
                set_timer(),
            ],
            vec![cancel_timer(), done(3, Ok(Resp::Echo(vec![9])))],
        ]
    );
}

#[test]
fn timeout_fails_the_request_and_late_replies_are_noise() {
    let mut driver = FakeDriver::new();
    let mut peer = FakePeer::new();
    peer.set_mute(true);
    connect(&mut driver);

    let outs = step(&mut driver, request(1, Req::Ping));
    assert_eq!(forward(&mut peer, &outs), vec![]);
    assert_eq!(step(&mut driver, Input::Timer(TimerId(5))), vec![]);
    assert_eq!(
        step(&mut driver, Input::Timer(TIMEOUT_TIMER)),
        vec![done(1, Err(ProtoError::Timeout))]
    );
    assert_eq!(
        step(&mut driver, rx(EVT, &[0x81, 0x00])),
        vec![unparsed(EVT, &[0x81, 0x00])]
    );

    // A timeout with something queued starts the next request in the same step.
    step(&mut driver, request(2, Req::Ping));
    step(&mut driver, request(3, Req::Get(1)));
    assert_eq!(
        step(&mut driver, Input::Timer(TIMEOUT_TIMER)),
        vec![
            done(2, Err(ProtoError::Timeout)),
            tx(&[0x02, 0x01, 1]),
            set_timer(),
        ]
    );
    peer.set_mute(false);
    let outs = step(&mut driver, Input::Timer(TIMEOUT_TIMER));
    assert_eq!(outs, vec![done(3, Err(ProtoError::Timeout))]);
}

#[test]
fn disconnect_fails_everything_and_the_driver_is_reusable() {
    let mut driver = FakeDriver::new();
    let mut peer = FakePeer::new();
    connect(&mut driver);
    step(&mut driver, request(1, Req::Ping));
    step(&mut driver, request(2, Req::Get(1)));
    step(&mut driver, request(3, Req::Echo(vec![1])));

    assert_eq!(
        step(&mut driver, Input::Disconnected),
        vec![
            cancel_timer(),
            done(1, Err(ProtoError::Disconnected)),
            done(2, Err(ProtoError::Disconnected)),
            done(3, Err(ProtoError::Disconnected)),
        ]
    );
    assert_eq!(driver, FakeDriver::new());
    assert_eq!(step(&mut driver, Input::Disconnected), vec![]);
    assert_eq!(
        step(&mut driver, request(4, Req::Ping)),
        vec![done(4, Err(ProtoError::Disconnected))]
    );

    assert_eq!(connect(&mut driver), vec![Output::Subscribe(EVT)]);
    assert_eq!(step(&mut driver, Input::Timer(TIMEOUT_TIMER)), vec![]);
    let outs = step(&mut driver, request(5, Req::Ping));
    assert_eq!(outs, vec![tx(&[0x01, 0x00]), set_timer()]);
    for reply in forward(&mut peer, &outs) {
        assert_eq!(
            step(&mut driver, reply),
            vec![cancel_timer(), done(5, Ok(Resp::Pong))]
        );
    }
}

#[test]
fn connected_on_top_of_a_live_link_is_a_new_link() {
    let mut driver = FakeDriver::new();
    connect(&mut driver);
    step(&mut driver, request(1, Req::Ping));
    step(&mut driver, request(2, Req::Ping));
    assert_eq!(
        step(&mut driver, connected(&[CMD, EVT])),
        vec![
            cancel_timer(),
            done(1, Err(ProtoError::Disconnected)),
            done(2, Err(ProtoError::Disconnected)),
            Output::Subscribe(EVT),
        ]
    );
    assert_eq!(
        step(&mut driver, request(3, Req::Ping)),
        vec![tx(&[0x01, 0x00]), set_timer()]
    );
}

#[test]
fn unsolicited_frames_use_the_model_types() {
    let mut driver = FakeDriver::new();
    let mut peer = FakePeer::new();
    connect(&mut driver);

    let (chan, bytes) = FakePeer::battery(64);
    assert_eq!(
        step(&mut driver, rx(chan, &bytes)),
        vec![Output::Event(Ev::Battery(Battery {
            percent: Percent::new(64).unwrap(),
            charging: false,
        }))]
    );
    let (chan, bytes) = FakePeer::battery(200);
    assert_eq!(bytes, vec![0x91, 0x01, 200]);
    assert_eq!(
        step(&mut driver, rx(chan, &bytes)),
        vec![unparsed(EVT, &bytes)]
    );

    for expected in 1..=3 {
        let (chan, bytes) = peer.heartbeat();
        assert_eq!(
            step(&mut driver, rx(chan, &bytes)),
            vec![Output::Event(Ev::Heartbeat(expected))]
        );
    }

    // Unsolicited frames do not disturb an active transaction.
    step(&mut driver, request(1, Req::Ping));
    let (chan, bytes) = peer.heartbeat();
    assert_eq!(
        step(&mut driver, rx(chan, &bytes)),
        vec![Output::Event(Ev::Heartbeat(4))]
    );
    assert_eq!(
        step(&mut driver, rx(EVT, &[0x81, 0x00])),
        vec![cancel_timer(), done(1, Ok(Resp::Pong))]
    );
}

#[test]
fn out_of_context_frames_are_unparsed_and_change_nothing() {
    let mut driver = FakeDriver::new();
    connect(&mut driver);

    // A reply with no transaction to answer.
    assert_eq!(
        step(&mut driver, rx(EVT, &[0x82, 0x05, 1, 0, 0, 0, 0])),
        vec![unparsed(EVT, &[0x82, 0x05, 1, 0, 0, 0, 0])]
    );

    step(&mut driver, request(1, Req::Ping));
    // A reply of the wrong kind, a host command, and valid bytes on the wrong channels.
    for (chan, bytes) in [
        (EVT, vec![0x82, 0x05, 1, 0, 0, 0, 0]),
        (EVT, vec![0x83, 0x02, 0, 1]),
        (EVT, vec![0x01, 0x00]),
        (EVT, vec![0x03, 0x01, 9]),
        (CMD, vec![0x81, 0x00]),
        (EXTRA, vec![0x81, 0x00]),
        (Channel(200), vec![0x90, 0x04, 1, 2, 3, 4]),
    ] {
        assert_eq!(
            step(&mut driver, rx(chan, &bytes)),
            vec![unparsed(chan, &bytes)]
        );
    }
    assert_eq!(
        step(&mut driver, rx(EVT, &[0x81, 0x00])),
        vec![cancel_timer(), done(1, Ok(Resp::Pong))]
    );
}

/// A fixed script that exercises every input kind, for the determinism test.
fn script() -> Vec<Input<Req>> {
    let script = vec![
        request(1, Req::Ping),
        connected(&[CMD, EVT, EXTRA]),
        request(2, Req::Ping),
        request(3, Req::Get(4)),
        Input::Tick { now: Instant(5) },
        rx(EVT, &[0x90, 0x04, 1, 0, 0, 0]),
        rx(EVT, &[0x81, 0x00]),
        rx(EVT, &[0x82, 0x05, 4, 1, 2, 3, 4]),
        request(4, Req::Echo(vec![1, 2, 3, 4, 5])),
        rx(EVT, &[0x83, 0x06, 0, 2, 1, 2, 3, 4]),
        rx(CMD, &[0x83, 0x03, 1, 2, 5]),
        rx(EVT, &[0xff, 0x00]),
        rx(EVT, &[0x83, 0x03, 1, 2, 5]),
        request(5, Req::Get(1)),
        request(6, Req::Ping),
        Input::Timer(TimerId(9)),
        Input::Timer(TIMEOUT_TIMER),
        rx(EVT, &[0x82, 0x05, 1, 0, 0, 0, 0]),
        rx(EVT, &[0x81, 0x00]),
        request(7, Req::Echo(vec![0; 254])),
        request(8, Req::Ping),
        request(9, Req::Get(2)),
        Input::Disconnected,
        Input::Disconnected,
        request(10, Req::Ping),
        connected(&[CMD, EXTRA]),
        request(11, Req::Ping),
        Input::Timer(TIMEOUT_TIMER),
        connected(&[CMD, EVT]),
        rx(EVT, &[0x91, 0x01, 50]),
        rx(EVT, &[0x91, 0x01, 150]),
        request(12, Req::Echo(vec![])),
        rx(EVT, &[0x83, 0x02, 0, 0]),
        request(13, Req::Echo(vec![7])),
        rx(EVT, &[0x83, 0x02, 0, 1]),
        rx(EVT, &[0x83, 0x03, 0, 1, 7]),
        request(14, Req::Get(0)),
        connected(&[CMD, EVT, EXTRA]),
        request(15, Req::Ping),
        Input::Disconnected,
    ];
    assert_eq!(script.len(), 40);
    script
}

#[test]
fn handle_is_deterministic() {
    let run = || {
        let mut driver = FakeDriver::new();
        script()
            .into_iter()
            .map(|input| step(&mut driver, input))
            .collect::<Vec<_>>()
    };
    let (first, second) = (run(), run());
    assert_eq!(first, second);
    // The script actually answered everything it asked.
    let answered = first
        .iter()
        .flatten()
        .filter(|out| matches!(out, Output::Done { .. }))
        .count();
    assert_eq!(answered, 15);
}

/// One random session: any input in any order, every `Tx` answered by a peer eventually.
fn random_session(seed: u64) {
    let mut rng = Rng(seed);
    let mut driver = FakeDriver::new();
    let mut peer = FakePeer::new();
    let mut pending: VecDeque<(Channel, Bytes)> = VecDeque::new();
    let mut requested = Vec::new();
    let mut answered = Vec::new();

    let mut absorb = |outs: Vec<Out>, pending: &mut VecDeque<(Channel, Bytes)>| {
        for out in outs {
            match out {
                Output::Tx { chan, bytes } => pending.extend(peer.on_write(chan, &bytes)),
                Output::Done { id, .. } => answered.push(id),
                Output::Subscribe(_)
                | Output::Read(_)
                | Output::SetTimer { .. }
                | Output::CancelTimer(_)
                | Output::Event(_)
                | Output::Disconnect => {}
            }
        }
    };

    for n in 0..300 {
        let input = match rng.below(6) {
            0 => {
                let all = [CMD, EVT, EXTRA];
                let resolved = all.into_iter().filter(|_| rng.below(4) != 0).collect();
                let mtu = u16::from(rng.byte());
                Input::Connected { resolved, mtu }
            }
            1 => Input::Disconnected,
            2 => match pending.pop_front() {
                Some((chan, bytes)) if rng.below(2) == 0 => Input::Rx { chan, bytes },
                Some(_) | None => {
                    let len = rng.below(41);
                    Input::Rx {
                        chan: Channel(rng.byte() % 4),
                        bytes: rng.bytes(len),
                    }
                }
            },
            3 => Input::Timer(TimerId(rng.byte() % 3)),
            4 => {
                let req = match rng.below(4) {
                    0 => Req::Ping,
                    1 => Req::Get(rng.byte()),
                    2 => {
                        let len = rng.below(13);
                        Req::Echo(rng.bytes(len))
                    }
                    _ => Req::Echo(rng.bytes(254)),
                };
                requested.push(ReqId(n));
                request(n, req)
            }
            _ => Input::Tick {
                now: Instant(u64::from(n)),
            },
        };
        absorb(step(&mut driver, input), &mut pending);
    }
    absorb(step(&mut driver, Input::Disconnected), &mut pending);

    let mut expected = requested;
    expected.sort_unstable();
    let mut got = answered.clone();
    got.sort_unstable();
    assert_eq!(got, expected, "seed {seed:#x}");
    got.dedup();
    assert_eq!(
        got.len(),
        answered.len(),
        "seed {seed:#x}: an id was answered twice"
    );
}

#[test]
fn every_request_gets_exactly_one_done() {
    for run in 0..200 {
        random_session(0x9e37_79b9_7f4a_7c15 ^ run);
    }
}

#[test]
fn garbage_never_panics_and_never_touches_the_transaction() {
    for busy in [false, true] {
        let mut rng = Rng(0x2545_f491_4f6c_dd1d);
        let mut driver = FakeDriver::new();
        connect(&mut driver);
        if busy {
            assert_eq!(
                step(&mut driver, request(1, Req::Ping)),
                vec![tx(&[0x01, 0x00]), set_timer()]
            );
        }
        for len in 0..=40 {
            for chan in 0..=3 {
                let chan = Channel(chan);
                let mut bytes = rng.bytes(len);
                if Frame::parse(&bytes).is_some() {
                    // Random bytes that happen to be a frame are not garbage; break them.
                    bytes[0] = 0xff;
                }
                assert_eq!(
                    step(&mut driver, rx(chan, &bytes)),
                    vec![unparsed(chan, &bytes)],
                    "busy {busy}, len {len}, {chan:?}"
                );
            }
        }
        if busy {
            assert_eq!(
                step(&mut driver, rx(EVT, &[0x81, 0x00])),
                vec![cancel_timer(), done(1, Ok(Resp::Pong))]
            );
        } else {
            assert_eq!(
                step(&mut driver, request(1, Req::Ping)),
                vec![tx(&[0x01, 0x00]), set_timer()]
            );
        }
    }
}

#[test]
fn peer_never_panics_and_mute_swallows_replies() {
    let mut rng = Rng(0x0123_4567_89ab_cdef);
    let mut peer = FakePeer::new();
    for _ in 0..2000 {
        let chan = Channel(rng.byte() % 4);
        let len = rng.below(41);
        let bytes = rng.bytes(len);
        for (reply_chan, reply) in peer.on_write(chan, &bytes) {
            assert_eq!(reply_chan, EVT);
            assert!(matches!(Frame::parse(&reply), Some(Frame::Reply(_))));
        }
    }
    assert_eq!(peer.on_write(EVT, &[0x01, 0x00]), vec![]);
    assert_eq!(peer.on_write(CMD, &[0x81, 0x00]), vec![]);
    assert_eq!(peer.on_write(CMD, &[0x01, 0x01]), vec![]);
    let mut long_echo = vec![0x03, 0xfe];
    long_echo.extend_from_slice(&[0; 254]);
    assert_eq!(peer.on_write(CMD, &long_echo), vec![]);

    peer.set_mute(true);
    assert_eq!(peer.on_write(CMD, &[0x01, 0x00]), vec![]);
    peer.set_mute(false);
    assert_eq!(
        peer.on_write(CMD, &[0x01, 0x00]),
        vec![(EVT, vec![0x81, 0x00])]
    );
}
