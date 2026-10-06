//! The `SoundcoreDriver` against SPEC §3.1's invariants, without any I/O: one transaction
//! in flight, everything else queued, nothing surviving a disconnect, and no byte sequence
//! that panics.

use junk_core::{
    Channel, ChannelSet, Driver, Input, Instant, Output, Outputs, Percent, ProtoError, ReqId,
};
use junk_soundcore::proto::{Ev, Req, Resp, SoundcoreDriver, TIMEOUT, TIMEOUT_TIMER};
use junk_soundcore::wire::{Command, Packet};
use junk_soundcore::{GATT, RFCOMM};

type Out = Output<Resp, Ev>;

/// The ten bytes a device-info request writes.
const REQUEST: [u8; 10] = [0x08, 0xee, 0, 0, 0, 0x01, 0x01, 0x0a, 0x00, 0x02];

/// The 39 bytes the speaker answered it with on 2026-09-15.
const REPLY: [u8; 39] = [
    0x09, 0xff, 0x00, 0x00, 0x01, 0x01, 0x01, 0x27, 0x00, 0x19, 0x04, 0x00, 0x00, 0x00, 0x01, 0x02,
    0x33, 0x2e, 0x30, 0x2e, 0x34, 0x41, 0x43, 0x43, 0x4c, 0x58, 0x58, 0x30, 0x30, 0x30, 0x30, 0x30,
    0x30, 0x30, 0x30, 0x30, 0x30, 0x78, 0x60,
];

fn step(driver: &mut SoundcoreDriver, input: Input<Req>) -> Vec<Out> {
    let mut out = Outputs::new();
    driver.handle(input, &mut out);
    out.into_vec()
}

/// A driver on a usable link.
fn connected() -> SoundcoreDriver {
    let mut driver = SoundcoreDriver::new();
    let outs = step(
        &mut driver,
        Input::Connected {
            resolved: [RFCOMM].into_iter().collect(),
            mtu: 668,
        },
    );
    // A byte stream pushes without being asked, so there is nothing to subscribe to and
    // nothing to say.
    assert_eq!(outs, []);
    assert_eq!(driver.mtu(), Some(668));
    driver
}

fn request(id: u32, req: Req) -> Input<Req> {
    Input::Request {
        id: ReqId(id),
        req,
        now: Instant::ZERO,
    }
}

fn rx(bytes: &[u8]) -> Input<Req> {
    Input::Rx {
        chan: RFCOMM,
        bytes: bytes.to_vec(),
    }
}

/// One device→host packet, encoded as it would come off the stream.
fn reply(command: Command, payload: &[u8]) -> Vec<u8> {
    let packet = Packet::device(command, payload);
    let mut bytes = vec![0; packet.encoded_len()];
    packet.encode_into(&mut bytes).expect("fits");
    bytes
}

fn tx(bytes: &[u8]) -> Out {
    Output::Tx {
        chan: RFCOMM,
        bytes: bytes.to_vec(),
    }
}

fn set_timer() -> Out {
    Output::SetTimer {
        id: TIMEOUT_TIMER,
        after: TIMEOUT,
    }
}

fn done(id: u32, result: Result<Resp, ProtoError>) -> Out {
    Output::Done {
        id: ReqId(id),
        result,
    }
}

/// The one event `outs` carries.
fn event(outs: Vec<Out>) -> Ev {
    match <[Out; 1]>::try_from(outs) {
        Ok([Output::Event(ev)]) => ev,
        other => panic!("expected one event, got {other:?}"),
    }
}

#[test]
fn a_device_info_request_writes_the_probes_bytes_and_is_answered_by_the_speakers_reply() {
    let mut driver = connected();
    assert_eq!(
        step(&mut driver, request(1, Req::DeviceInfo)),
        [tx(&REQUEST), set_timer()]
    );

    let outs = step(&mut driver, rx(&REPLY));
    let [
        Output::CancelTimer(TIMEOUT_TIMER),
        Output::Done { id, result },
    ] = &outs[..]
    else {
        panic!("{outs:?}");
    };
    assert_eq!(*id, ReqId(1));
    let Ok(Resp::DeviceInfo(info)) = result else {
        panic!("{result:?}");
    };
    assert_eq!(info.battery, Percent::new(80).expect("four fifths"));
    assert_eq!(info.serial, "ACCLXX0000000000x");
}

#[test]
fn a_raw_command_number_matches_its_named_reply() {
    let mut driver = connected();
    assert_eq!(
        step(
            &mut driver,
            request(
                1,
                Req::Raw {
                    command: Command::Other(0x0101),
                    payload: vec![],
                }
            ),
        ),
        [tx(&REQUEST), set_timer()]
    );
    assert_eq!(
        step(&mut driver, rx(&REPLY)),
        [
            Output::CancelTimer(TIMEOUT_TIMER),
            done(1, Ok(Resp::Raw(REPLY.to_vec()))),
        ]
    );
}

#[test]
fn only_one_transaction_is_in_flight_and_the_rest_wait_their_turn() {
    let mut driver = connected();
    assert_eq!(
        step(&mut driver, request(1, Req::DeviceInfo)),
        [tx(&REQUEST), set_timer()]
    );
    // The second request is encoded and queued, and writes nothing yet.
    assert_eq!(
        step(
            &mut driver,
            request(
                2,
                Req::Raw {
                    command: Command::GetEqualizer,
                    payload: vec![],
                }
            )
        ),
        []
    );

    // The first answer starts the second transaction in the same step.
    let outs = step(&mut driver, rx(&REPLY));
    assert_eq!(outs.len(), 4);
    assert_eq!(outs[0], Output::CancelTimer(TIMEOUT_TIMER));
    assert!(matches!(outs[1], Output::Done { id: ReqId(1), .. }));
    assert_eq!(
        outs[2],
        tx(&[0x08, 0xee, 0, 0, 0, 0x02, 0x89, 0x0a, 0, 0x8b])
    );
    assert_eq!(outs[3], set_timer());

    let equalizer = reply(Command::GetEqualizer, &[1, 2, 3]);
    assert_eq!(
        step(&mut driver, rx(&equalizer)),
        [
            Output::CancelTimer(TIMEOUT_TIMER),
            done(2, Ok(Resp::Raw(equalizer.clone()))),
        ]
    );
}

#[test]
fn a_disconnect_fails_everything_and_leaves_a_fresh_driver() {
    let mut driver = connected();
    step(&mut driver, request(1, Req::DeviceInfo));
    step(&mut driver, request(2, Req::DeviceInfo));

    assert_eq!(
        step(&mut driver, Input::Disconnected),
        [
            Output::CancelTimer(TIMEOUT_TIMER),
            done(1, Err(ProtoError::Disconnected)),
            done(2, Err(ProtoError::Disconnected)),
        ]
    );
    assert_eq!(driver, SoundcoreDriver::new());
    assert_eq!(driver.mtu(), None);
    // And a request without a link is refused rather than queued.
    assert_eq!(
        step(&mut driver, request(3, Req::DeviceInfo)),
        [done(3, Err(ProtoError::Disconnected))]
    );
}

#[test]
fn the_timer_fails_the_transaction_it_guards_and_nothing_else() {
    let mut driver = connected();
    step(&mut driver, request(1, Req::DeviceInfo));
    // A timer id the driver never arms is not its business.
    assert_eq!(step(&mut driver, Input::Timer(junk_core::TimerId(9))), []);
    assert_eq!(
        step(&mut driver, Input::Timer(TIMEOUT_TIMER)),
        [done(1, Err(ProtoError::Timeout))]
    );
    // It fired once; a second one answers nothing.
    assert_eq!(step(&mut driver, Input::Timer(TIMEOUT_TIMER)), []);
}

#[test]
fn a_link_without_the_byte_stream_is_refused() {
    let mut driver = SoundcoreDriver::new();
    let outs = step(
        &mut driver,
        Input::Connected {
            resolved: ChannelSet::EMPTY,
            mtu: 668,
        },
    );
    let missing: ChannelSet = [RFCOMM].into_iter().collect();
    assert_eq!(
        outs,
        [
            Output::Event(Ev::MissingChannels(missing)),
            Output::Disconnect
        ]
    );
    assert_eq!(driver.mtu(), None);
    assert_eq!(GATT.required(), missing);
}

#[test]
fn what_the_speaker_says_unasked_becomes_an_event() {
    let mut driver = connected();
    assert_eq!(
        event(step(
            &mut driver,
            rx(&reply(Command::NotifyBatteryInfo, &[3]))
        )),
        Ev::Battery(Percent::new(60).expect("three fifths"))
    );
    assert_eq!(
        event(step(
            &mut driver,
            rx(&reply(Command::NotifyChargingInfo, &[1]))
        )),
        Ev::Charging(true)
    );
    assert_eq!(
        event(step(
            &mut driver,
            rx(&reply(Command::NotifyChargingInfo, &[0]))
        )),
        Ev::Charging(false)
    );
    assert_eq!(
        event(step(
            &mut driver,
            rx(&reply(Command::NotifyVolumeInfo, &[25]))
        )),
        Ev::Volume(25)
    );
    assert_eq!(
        event(step(
            &mut driver,
            rx(&reply(Command::NotifyPlaybackInfo, &[1, 2]))
        )),
        Ev::Playback(vec![1, 2])
    );
}

#[test]
fn a_notification_that_arrives_mid_transaction_is_still_news() {
    let mut driver = connected();
    step(&mut driver, request(1, Req::DeviceInfo));
    assert_eq!(
        event(step(
            &mut driver,
            rx(&reply(Command::NotifyBatteryInfo, &[5]))
        )),
        Ev::Battery(Percent::FULL),
        "the transaction wants 0x0101 and takes nothing else"
    );
    // And the request it did not answer is still in flight.
    let outs = step(&mut driver, rx(&REPLY));
    assert!(matches!(outs[1], Output::Done { id: ReqId(1), .. }));
}

#[test]
fn nothing_the_speaker_could_send_panics() {
    let mut driver = connected();
    // Not a packet at all: a bad checksum, a bad start word, too few bytes.
    let mut corrupt = REPLY;
    corrupt[38] ^= 0xff;
    for bytes in [
        &corrupt[..],
        &[0xee, 0x08, 0, 0, 0, 1, 1, 10, 0, 2],
        &[],
        &[0x09],
    ] {
        assert_eq!(
            event(step(&mut driver, rx(bytes))),
            Ev::Unparsed {
                bytes: bytes.to_vec()
            }
        );
    }
    // A packet, but with a payload its command cannot take.
    for bad in [
        reply(Command::NotifyBatteryInfo, &[6]),
        reply(Command::NotifyBatteryInfo, &[]),
        reply(Command::NotifyChargingInfo, &[1, 2]),
        reply(Command::NotifyVolumeInfo, &[]),
    ] {
        assert_eq!(
            event(step(&mut driver, rx(&bad))),
            Ev::Unparsed { bytes: bad.clone() }
        );
    }
    // A well-formed reply nobody asked for, and a host packet coming back at us.
    let unwanted = reply(Command::GetEqualizer, &[0]);
    assert_eq!(
        event(step(&mut driver, rx(&unwanted))),
        Ev::Unexpected(unwanted.clone())
    );
    assert_eq!(
        event(step(&mut driver, rx(&REQUEST))),
        Ev::Unexpected(REQUEST.to_vec())
    );
    // A channel this family does not declare cannot carry one of its packets.
    assert_eq!(
        event(step(
            &mut driver,
            Input::Rx {
                chan: Channel(7),
                bytes: REPLY.to_vec(),
            }
        )),
        Ev::Unparsed {
            bytes: REPLY.to_vec()
        }
    );
}

#[test]
fn a_reply_the_transaction_cannot_read_fails_its_request() {
    let mut driver = connected();
    step(&mut driver, request(1, Req::DeviceInfo));
    let mut payload = [0u8; 29];
    payload[1] = 9;
    assert_eq!(
        step(&mut driver, rx(&reply(Command::GetDeviceInfo, &payload))),
        [
            Output::CancelTimer(TIMEOUT_TIMER),
            done(
                1,
                Err(ProtoError::Malformed(
                    "device info: battery is not a count of fifths"
                ))
            ),
        ]
    );
}

#[test]
fn the_tick_is_nothing_to_this_driver() {
    let mut driver = connected();
    assert_eq!(
        step(&mut driver, Input::Tick { now: Instant::ZERO }),
        [],
        "the speaker is never asked the time"
    );
}
