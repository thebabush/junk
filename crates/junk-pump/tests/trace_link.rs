//! `TraceLink` over a session this stack recorded against a real ring: what it serves, in
//! what order, and how it fails when a script departs from the recorded one.

use std::fs;

use junk_colmi::{DIS_FW, DIS_HW, GATT, V1_NOTIFY, V1_WRITE, V2_CMD, V2_NOTIFY, channel_by_name};
use junk_core::{Channel, ChannelSet, Link, LinkError, LinkEvent};
use junk_pump::trace_link::TraceLink;
use junk_trace::Trace;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/colmi-r10");
const SYNC: &str = "junk-sync-2026-09-06.trace";

/// The first write of the sync fixture: `04 01 12`, the phone name.
const FIRST_WRITE: [u8; 16] = [0x04, 0x01, 0x12, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x17];
/// Its one reply: the `04` ack.
const FIRST_REPLY: [u8; 16] = [0x04, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x04];
/// The `tx` lines the fixture has.
const WRITES: usize = 41;

fn trace(name: &str) -> Trace {
    let text = fs::read_to_string(format!("{FIXTURES}/{name}")).expect("the fixture is readable");
    Trace::parse(&text).unwrap_or_else(|err| panic!("{err}"))
}

fn link() -> TraceLink {
    TraceLink::new(&trace(SYNC), channel_by_name)
}

#[tokio::test]
async fn connect_resolves_what_the_connected_event_line_says() {
    let mut link = link();
    assert_eq!(
        link.unknown_channels(),
        0,
        "the fixture names every channel"
    );
    let all: ChannelSet = [V1_WRITE, V1_NOTIFY, V2_CMD, V2_NOTIFY, DIS_FW, DIS_HW]
        .into_iter()
        .collect();
    assert_eq!(link.connect(&GATT).await, Ok((all, 247)));
}

#[tokio::test]
async fn a_write_is_answered_by_the_lines_that_followed_it() {
    let mut link = link();
    link.connect(&GATT).await.expect("connects");
    assert_eq!(link.unserved(), WRITES);

    link.write(V1_WRITE, &FIRST_WRITE).await.expect("recorded");
    assert_eq!(link.writes(), [(V1_WRITE, FIRST_WRITE.to_vec())]);
    assert_eq!(link.unserved(), WRITES - 1);
    assert_eq!(
        link.next().await,
        LinkEvent::Rx {
            chan: V1_NOTIFY,
            bytes: FIRST_REPLY.to_vec(),
        }
    );

    // The second write's replies come in the order the ring sent them: the `2f` packet
    // size the ring volunteered, then the `01` capability ack.
    let set_time = hex("0126090602102401000000000000006d");
    link.write(V1_WRITE, &set_time).await.expect("recorded");
    let replies = [link.next().await, link.next().await];
    assert_eq!(
        replies,
        [
            LinkEvent::Rx {
                chan: V1_NOTIFY,
                bytes: hex("2ff40000000000000000000000000023"),
            },
            LinkEvent::Rx {
                chan: V1_NOTIFY,
                bytes: hex("01010000020000000001002000003055"),
            },
        ]
    );
    assert_eq!(link.unserved(), WRITES - 2);
    assert_eq!(link.writes().len(), 2);
}

#[tokio::test]
async fn a_write_the_trace_does_not_have_fails_rather_than_waits() {
    let mut link = link();
    link.connect(&GATT).await.expect("connects");
    let err = link.write(V1_WRITE, &[0x03, 0x99]).await.unwrap_err();
    assert_eq!(
        err,
        LinkError::Io("the trace has no such write: 0399".to_owned())
    );
    // Nothing was served, so the trace is still whole.
    assert_eq!(link.unserved(), WRITES);
    assert_eq!(link.writes(), []);
}

#[tokio::test]
async fn the_version_strings_come_back_as_reads() {
    let mut link = link();
    link.connect(&GATT).await.expect("connects");
    // The `19 01` ack and the two Device Information values are the lines that follow the
    // version write; the driver takes the two values with `read`.
    for write in [
        "04011200000000000000000000000017",
        "0126090602102401000000000000006d",
        "1901010100000000000000000000001c",
    ] {
        link.write(V1_WRITE, &hex(write)).await.expect("recorded");
    }
    assert_eq!(
        link.read(DIS_FW).await.map(text),
        Ok("RT03CR_1.00.02_260319".to_owned())
    );
    assert_eq!(
        link.read(DIS_HW).await.map(text),
        Ok("RT03CR_V1.0".to_owned())
    );
    assert_eq!(
        link.read(DIS_FW).await,
        Err(LinkError::Io(
            "the trace has no read left on dis.fw".to_owned()
        ))
    );

    // What a read took is not notified as well: of the version write's three lines only
    // the `19 01` ack is left queued, after the two earlier writes' replies.
    let mut queued = Vec::new();
    for _ in 0..4 {
        queued.push(link.next().await);
    }
    assert_eq!(
        queued.last(),
        Some(&LinkEvent::Rx {
            chan: V1_NOTIFY,
            bytes: hex("1901000100000000000000000000001b"),
        })
    );
    assert!(
        queued.iter().all(|event| !matches!(
            event,
            LinkEvent::Rx { chan, .. } if *chan == DIS_FW || *chan == DIS_HW
        )),
        "{queued:?}"
    );
}

#[tokio::test]
async fn a_second_connect_serves_the_trace_again() {
    let mut link = link();
    link.connect(&GATT).await.expect("connects");
    link.write(V1_WRITE, &FIRST_WRITE).await.expect("recorded");
    assert_eq!(link.unserved(), WRITES - 1);
    link.disconnect().await;
    assert_eq!(
        link.write(V1_WRITE, &FIRST_WRITE).await,
        Err(LinkError::NotConnected)
    );
    assert_eq!(link.read(DIS_FW).await, Err(LinkError::NotConnected));

    link.connect(&GATT).await.expect("connects again");
    assert_eq!(link.unserved(), WRITES, "the whole trace, from the top");
    link.write(V1_WRITE, &FIRST_WRITE).await.expect("recorded");
    assert_eq!(
        link.next().await,
        LinkEvent::Rx {
            chan: V1_NOTIFY,
            bytes: FIRST_REPLY.to_vec(),
        },
        "and the queue with it"
    );
    assert_eq!(link.writes().len(), 2, "every write served, both passes");
}

#[tokio::test]
async fn a_trace_the_family_does_not_know_resolves_nothing() {
    let text = "\
! 2026-09-06T02:10:27.290-04:00 connected mtu=185 channels=v1.write,v1.notify,nowhere
2026-09-06T02:10:29.518-04:00 tx v1.write 0401
2026-09-06T02:10:29.639-04:00 rx elsewhere 0400
";
    let trace = Trace::parse(text).unwrap_or_else(|err| panic!("{err}"));
    let mut link = TraceLink::new(&trace, channel_by_name);
    assert_eq!(link.unknown_channels(), 1, "the `elsewhere` line");
    let v1: ChannelSet = [V1_WRITE, V1_NOTIFY].into_iter().collect();
    assert_eq!(
        link.connect(&GATT).await,
        Ok((v1, 185)),
        "`nowhere` is ignored, the MTU is the event line's"
    );

    // Without the required V1 pair, connecting fails as a real link would.
    let trace = Trace::parse("2026-09-06T02:10:29.639-04:00 rx v1.notify 0400\n")
        .unwrap_or_else(|err| panic!("{err}"));
    let mut link = TraceLink::new(&trace, channel_by_name);
    assert_eq!(
        link.connect(&GATT).await,
        Err(LinkError::MissingRequired([V1_WRITE].into_iter().collect())),
        "a trace with no event line resolves the channels its data lines use"
    );
    assert_eq!(
        link.read(Channel(9)).await,
        Err(LinkError::NotConnected),
        "a failed connect leaves the link down"
    );
}

/// `text` as bytes, as a trace writes them.
fn hex(text: &str) -> Vec<u8> {
    (0..text.len() / 2)
        .map(|index| {
            u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).expect("two hex digits")
        })
        .collect()
}

/// `bytes` as the ASCII string a Device Information value is.
fn text(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).expect("ascii")
}
