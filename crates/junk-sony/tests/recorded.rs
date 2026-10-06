//! The driver through `junk-pump`, over a `Framed` and a `TraceLink`: init and one status
//! read against `fixtures/sony-wh1000xm4/synthetic-init-status.trace`.
//!
//! **That trace is synthetic.** It was not captured from an XM4 or any other headset: it is
//! the fake headset of `tests/proto.rs`, whose replies are written out from the layouts
//! of Sony's own app, recorded as a trace. What this proves is the plumbing: that the driver's
//! writes, byte for byte, are what the trace expects, that `SonyFraming` hands the driver
//! whole frames however the stream is chunked, and that the pump's timers and the driver's
//! agree. It proves nothing about what a real headset says. Replace the fixture with a
//! capture when there is one.

use std::fs;

use junk_core::ChannelSet;
use junk_pump::framed::Framed;
use junk_pump::trace_link::TraceLink;
use junk_pump::{Pump, PumpConfig, PumpEvent, Stop};
use junk_sony::payload::{AudioCodec, FunctionType};
use junk_sony::proto::{Ev, Reading, Req, Resp};
use junk_sony::{RFCOMM, SonyDriver, SonyFraming, channel_by_name};
use junk_trace::Trace;

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/sony-wh1000xm4/synthetic-init-status.trace"
);

fn trace() -> Trace {
    let text = fs::read_to_string(FIXTURE).expect("the fixture is readable");
    Trace::parse(&text).unwrap_or_else(|err| panic!("{err}"))
}

fn link() -> Framed<TraceLink, SonyFraming> {
    let inner = TraceLink::new(&trace(), channel_by_name);
    assert_eq!(inner.unknown_channels(), 0, "the fixture names its channel");
    Framed::new(inner, SonyFraming)
}

/// The fixture with every `rx` line cut into one-byte lines: the least convenient chunks a
/// radio could deliver the same stream in.
fn one_byte_at_a_time() -> TraceLink {
    let mut trace = trace();
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

#[tokio::test]
async fn the_synthetic_session_runs_through_the_pump_write_for_write() {
    let (mut pump, handle, mut events) =
        Pump::new(SonyDriver::new(), link(), PumpConfig::default());
    let script = async {
        let answer = handle.request(Req::Status).await;
        handle.shutdown();
        answer
    };
    let (stopped, answer) = tokio::join!(pump.run(), script);
    assert_eq!(stopped, Ok(Stop::Shutdown));

    let Resp::Status(status) = answer.unwrap_or_else(|err| panic!("{err}")) else {
        panic!("a status request is answered by a status");
    };
    assert_eq!(status.device.protocol_version, Reading::Value(0x4010));
    assert_eq!(
        status.device.model,
        Reading::Value("WH-1000XM4".to_string())
    );
    assert!(
        status
            .device
            .functions
            .value()
            .is_some_and(|f| f.contains(&FunctionType::NoiseCancellingAndAmbientSoundMode))
    );
    assert_eq!(status.codec, Reading::Value(AudioCodec::Ldac));
    assert_eq!(status.serial, Reading::Value("ABCDE".to_string()));

    let (_driver, link) = pump.into_parts();
    let trace_link = link.into_inner();
    assert_eq!(trace_link.unserved(), 0, "the whole trace was served");
    // Every write is a tx line, in order: the driver wrote exactly what the trace has.
    let tx_lines = trace()
        .data()
        .filter(|l| l.dir == junk_trace::Direction::Tx)
        .count();
    assert_eq!(trace_link.writes().len(), tx_lines);
    assert!(trace_link.writes().iter().all(|(chan, _)| *chan == RFCOMM));

    // The link came up as the `connected` line says, and the only thing the driver said on
    // its own is the protocol version.
    let mut reported = Vec::new();
    while let Ok(event) = events.try_recv() {
        reported.push(event);
    }
    let resolved: ChannelSet = [RFCOMM].into_iter().collect();
    assert_eq!(
        reported.first(),
        Some(&PumpEvent::Connected { resolved, mtu: 668 })
    );
    let said: Vec<&Ev> = reported
        .iter()
        .filter_map(|event| match event {
            PumpEvent::Event(ev) => Some(ev),
            PumpEvent::Connected { .. } | PumpEvent::Disconnected | PumpEvent::LinkError(_) => None,
        })
        .collect();
    assert_eq!(
        said,
        [&Ev::ProtocolVersion {
            version: 0x4010,
            supported: true
        }]
    );
}

/// The framing is doing real work: the same session with every reply delivered a byte at a
/// time is the same session.
#[tokio::test]
async fn the_session_is_the_same_however_the_radio_chunks_it() {
    let (mut pump, handle, _events) = Pump::new(
        SonyDriver::new(),
        Framed::with_limit(one_byte_at_a_time(), SonyFraming, 256),
        PumpConfig::default(),
    );
    let script = async {
        let answer = handle.request(Req::Status).await;
        handle.shutdown();
        answer
    };
    let (_stopped, answer) = tokio::join!(pump.run(), script);
    let Resp::Status(status) = answer.unwrap_or_else(|err| panic!("{err}")) else {
        panic!("a status request is answered by a status");
    };
    assert_eq!(status.serial, Reading::Value("ABCDE".to_string()));
    assert_eq!(status.general_settings.len(), 2);
}
