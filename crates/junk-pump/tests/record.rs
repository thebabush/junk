//! `RecordingLink` over a `ChannelLink`: what it writes down, rendered and read back.

use junk_core::{Channel, ChannelSet, Link, LinkError, LinkEvent};
use junk_fake::{CMD, EVT, EXTRA, GATT};
use junk_pump::channel_link::ChannelLink;
use junk_pump::record::RecordingLink;
use junk_trace::{Line, Stamp, Trace};

/// The fake family's names for its channels; `EXTRA` deliberately has none.
fn channel_name(chan: Channel) -> Option<&'static str> {
    match chan {
        CMD => Some("cmd"),
        EVT => Some("evt"),
        Channel(_) => None,
    }
}

/// A clock that advances one millisecond per stamp from a fixed start, offset `-04:00`.
fn clock() -> impl FnMut() -> Stamp {
    let start = Stamp::parse("2026-09-06T12:00:00.000-04:00")
        .expect("valid")
        .naive_epoch_millis();
    let mut ticks = 0;
    move || {
        let at = Stamp::from_naive_epoch_millis(start + ticks, Some(-240)).expect("in range");
        ticks += 1;
        at
    }
}

fn trace(lines: &[Line]) -> Trace {
    Trace {
        lines: lines.to_vec(),
    }
}

#[tokio::test]
async fn a_session_renders_to_the_expected_trace_and_reads_back() {
    let (link, peer) = ChannelLink::pair();
    let mut link = RecordingLink::new(link, clock(), channel_name);

    link.connect(&GATT).await.expect("connects");
    link.subscribe(EVT).await.expect("subscribes");
    link.write(CMD, &[0x01, 0x02, 0xff]).await.expect("writes");
    assert!(peer.notify(EVT, vec![0xaa, 0xbb]));
    assert!(peer.notify(EXTRA, vec![0x00]));
    assert_eq!(
        link.next().await,
        LinkEvent::Rx {
            chan: EVT,
            bytes: vec![0xaa, 0xbb],
        }
    );
    assert_eq!(
        link.next().await,
        LinkEvent::Rx {
            chan: EXTRA,
            bytes: vec![0x00],
        }
    );
    peer.disconnect();
    assert_eq!(link.next().await, LinkEvent::Disconnected);
    link.disconnect().await;

    let expected = "\
! 2026-09-06T12:00:00.000-04:00 connected mtu=247 channels=cmd,evt,unknown.2
! 2026-09-06T12:00:00.001-04:00 subscribe evt
2026-09-06T12:00:00.002-04:00 tx cmd 0102ff
2026-09-06T12:00:00.003-04:00 rx evt aabb
2026-09-06T12:00:00.004-04:00 rx unknown.2 00
! 2026-09-06T12:00:00.005-04:00 disconnected
! 2026-09-06T12:00:00.006-04:00 disconnect requested
";
    let recorded = trace(link.lines());
    assert_eq!(recorded.render(), expected);
    assert_eq!(Trace::parse(expected), Ok(recorded));
}

#[tokio::test]
async fn failures_are_recorded_where_they_happen() {
    let (link, mut peer) = ChannelLink::pair();
    let missing: ChannelSet = [EVT].into_iter().collect();
    peer.set_connect_error(Some(LinkError::MissingRequired(missing)));
    let mut link = RecordingLink::new(link, clock(), channel_name);

    assert!(link.connect(&GATT).await.is_err());
    // Nothing is connected: these fail, and a failed operation leaves no line.
    assert_eq!(link.write(CMD, &[1]).await, Err(LinkError::NotConnected));
    assert_eq!(link.subscribe(EVT).await, Err(LinkError::NotConnected));

    peer.set_connect_error(None);
    peer.set_resolved([CMD, EVT].into_iter().collect());
    peer.set_mtu(23);
    link.connect(&GATT).await.expect("connects");
    assert_eq!(
        link.write(EXTRA, &[1]).await,
        Err(LinkError::UnknownChannel(EXTRA))
    );
    assert_eq!(
        link.subscribe(Channel(9)).await,
        Err(LinkError::UnknownChannel(Channel(9)))
    );

    let expected = "\
! 2026-09-06T12:00:00.000-04:00 connect failed: device lacks required channels {Channel(1)}
! 2026-09-06T12:00:00.001-04:00 connected mtu=23 channels=cmd,evt
";
    let recorded = trace(link.lines());
    assert_eq!(recorded.render(), expected);
    assert_eq!(Trace::parse(expected), Ok(recorded));
}

#[tokio::test]
async fn empty_payloads_become_events() {
    let (link, peer) = ChannelLink::pair();
    let mut link = RecordingLink::new(link, clock(), channel_name);
    link.connect(&GATT).await.expect("connects");
    link.write(CMD, &[]).await.expect("writes");
    assert!(peer.notify(EVT, vec![]));
    assert_eq!(
        link.next().await,
        LinkEvent::Rx {
            chan: EVT,
            bytes: vec![],
        }
    );

    let expected = "\
! 2026-09-06T12:00:00.000-04:00 connected mtu=247 channels=cmd,evt,unknown.2
! 2026-09-06T12:00:00.001-04:00 empty tx cmd
! 2026-09-06T12:00:00.002-04:00 empty rx evt
";
    let recorded = trace(link.lines());
    assert_eq!(recorded.render(), expected);
    assert_eq!(Trace::parse(expected), Ok(recorded));
}

#[tokio::test]
async fn reads_are_recorded_as_rx_lines() {
    let (link, mut peer) = ChannelLink::pair();
    let mut link = RecordingLink::new(link, clock(), channel_name);
    link.connect(&GATT).await.expect("connects");
    peer.set_value(EXTRA, vec![0x52, 0x54]);
    assert_eq!(link.read(EXTRA).await, Ok(vec![0x52, 0x54]));
    // A failed read leaves no line; an empty value becomes an event, as a notification's
    // would.
    assert_eq!(link.read(CMD).await, Err(LinkError::Io("no value".into())));
    peer.set_value(CMD, vec![]);
    assert_eq!(link.read(CMD).await, Ok(vec![]));

    let expected = "\
! 2026-09-06T12:00:00.000-04:00 connected mtu=247 channels=cmd,evt,unknown.2
2026-09-06T12:00:00.001-04:00 rx unknown.2 5254
! 2026-09-06T12:00:00.002-04:00 empty rx cmd
";
    let recorded = trace(link.lines());
    assert_eq!(recorded.render(), expected);
    assert_eq!(Trace::parse(expected), Ok(recorded));
}

#[tokio::test]
async fn lines_can_be_taken_and_the_link_taken_back() {
    let (link, peer) = ChannelLink::pair();
    let mut link = RecordingLink::new(link, clock(), channel_name);
    link.connect(&GATT).await.expect("connects");
    link.write(CMD, &[7]).await.expect("writes");
    assert_eq!(link.lines().len(), 2);

    let taken = link.take_lines();
    assert_eq!(taken.len(), 2);
    assert_eq!(link.lines(), []);
    assert!(matches!(&taken[1], Line::Data(data) if data.bytes == [7]));

    // The clock keeps counting: what follows continues the session.
    link.subscribe(EVT).await.expect("subscribes");
    let (mut inner, rest) = link.into_inner();
    assert_eq!(
        trace(&rest).render(),
        "! 2026-09-06T12:00:00.002-04:00 subscribe evt\n"
    );
    assert!(peer.is_connected());
    inner.write(CMD, &[8]).await.expect("still connected");
    inner.disconnect().await;
    assert!(!peer.is_connected());
}
