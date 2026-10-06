//! The exchange this stack captured from a real Motion 300 on 2026-09-15, served back by a
//! `TraceLink`: the same write, and the same answer, with no speaker in the room.
//!
//! The link below is a `Framed` over the `TraceLink`, because RFCOMM is a byte stream and
//! `SoundcoreFraming` is what cuts it back into packets — the same two pieces a real
//! session puts over `junk-rfcomm`. `crates/junk-app/tests/recorded.rs` drives the Colmi
//! family the same way over its own fixtures.
//!
//! This is the test that would have caught the reversed start-of-packet: the driver's
//! write is asserted byte for byte against the ten bytes the speaker actually answered, so
//! a driver sending `ee 08 …` fails here instead of on the hardware.

use std::fs;

use junk_core::{Channel, ChannelSet, Percent};
use junk_pump::framed::Framed;
use junk_pump::trace_link::TraceLink;
use junk_pump::{Pump, PumpConfig, PumpEvent, Stop};
use junk_soundcore::proto::{Ev, Req, Resp};
use junk_soundcore::{RFCOMM, SoundcoreDriver, SoundcoreFraming, channel_by_name};
use junk_trace::Trace;

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/soundcore-motion-300/probe-2026-09-15.trace"
);

/// The ten bytes the probe sent: `0x0101`, no payload, checksum `0x02`.
const REQUEST: [u8; 10] = [0x08, 0xee, 0, 0, 0, 0x01, 0x01, 0x0a, 0x00, 0x02];

/// The RFCOMM channel's MTU, as the `connected` event line records it.
const MTU: u16 = 668;

fn link() -> Framed<TraceLink, SoundcoreFraming> {
    let text = fs::read_to_string(FIXTURE).expect("the fixture is readable");
    let trace = Trace::parse(&text).unwrap_or_else(|err| panic!("{err}"));
    let inner = TraceLink::new(&trace, channel_by_name);
    assert_eq!(inner.unknown_channels(), 0, "the fixture names its channel");
    Framed::new(inner, SoundcoreFraming)
}

#[tokio::test]
async fn the_captured_probe_runs_again_write_for_write() {
    let (mut pump, handle, mut events) =
        Pump::new(SoundcoreDriver::new(), link(), PumpConfig::default());
    let script = async {
        let answer = handle.request(Req::DeviceInfo).await;
        handle.shutdown();
        answer
    };
    let (stopped, answer) = tokio::join!(pump.run(), script);
    assert_eq!(stopped, Ok(Stop::Shutdown));

    let Resp::DeviceInfo(info) = answer.unwrap_or_else(|err| panic!("{err}")) else {
        panic!("a device-info request is answered by device info");
    };
    assert_eq!(info.volume, 25);
    assert_eq!(info.battery, Percent::new(80).expect("four fifths"));
    assert!(!info.charging);
    assert!(!info.playing);
    assert!(!info.voice_prompts);
    assert!(info.auto_power_off);
    assert_eq!(info.auto_power_off_duration, 2);
    assert_eq!(info.firmware, "3.0.4");
    assert_eq!(info.serial, "ACCLXX0000000000x");

    let (_driver, link) = pump.into_parts();
    let trace_link = link.into_inner();
    assert_eq!(
        trace_link.writes(),
        [(RFCOMM, REQUEST.to_vec())],
        "the one write, byte for byte as the speaker was sent it"
    );
    assert_eq!(trace_link.unserved(), 0, "the whole trace was served");

    // The link came up as the `connected` event line describes it, and the speaker said
    // nothing the driver could not place.
    let mut reported = Vec::new();
    while let Ok(event) = events.try_recv() {
        reported.push(event);
    }
    let resolved: ChannelSet = [RFCOMM].into_iter().collect();
    assert_eq!(
        reported.first(),
        Some(&PumpEvent::Connected { resolved, mtu: MTU })
    );
    let unplaced: Vec<&Ev> = reported
        .iter()
        .filter_map(|event| match event {
            PumpEvent::Event(ev) => Some(ev),
            PumpEvent::Connected { .. } | PumpEvent::Disconnected | PumpEvent::LinkError(_) => None,
        })
        .collect();
    assert!(unplaced.is_empty(), "{unplaced:?}");
}

/// The framing is doing real work here: the same reply delivered a byte at a time is one
/// packet to the driver, not 39 scraps.
#[tokio::test]
async fn the_reply_is_one_packet_however_the_radio_chunks_it() {
    let (mut pump, handle, _events) = Pump::new(
        SoundcoreDriver::new(),
        Framed::with_limit(one_byte_at_a_time(), SoundcoreFraming, 64),
        PumpConfig::default(),
    );
    let script = async {
        let answer = handle.request(Req::DeviceInfo).await;
        handle.shutdown();
        answer
    };
    let (_stopped, answer) = tokio::join!(pump.run(), script);
    let Resp::DeviceInfo(info) = answer.unwrap_or_else(|err| panic!("{err}")) else {
        panic!("a device-info request is answered by device info");
    };
    assert_eq!(info.serial, "ACCLXX0000000000x");
}

/// The fixture with its one `rx` line split into 39 one-byte lines: the same stream, in the
/// least convenient chunks a radio could deliver it in.
fn one_byte_at_a_time() -> TraceLink {
    let text = fs::read_to_string(FIXTURE).expect("the fixture is readable");
    let mut trace = Trace::parse(&text).unwrap_or_else(|err| panic!("{err}"));
    let mut lines = Vec::new();
    for line in trace.lines.drain(..) {
        match line {
            junk_trace::Line::Data(data) if data.dir == junk_trace::Direction::Rx => {
                lines.extend(data.bytes.iter().map(|byte| {
                    junk_trace::Line::Data(junk_trace::DataLine {
                        bytes: vec![*byte],
                        ..data.clone()
                    })
                }));
            }
            other => lines.push(other),
        }
    }
    TraceLink::new(&Trace { lines }, channel_by_name)
}

/// A channel the family does not name is dropped rather than served: a trace recorded from
/// some other device is not this one's.
#[test]
fn a_foreign_channel_is_not_this_familys() {
    assert_eq!(channel_by_name("rfcomm"), Some(RFCOMM));
    assert_eq!(channel_by_name("v1.write"), None);
    assert_eq!(RFCOMM, Channel(0));
}
