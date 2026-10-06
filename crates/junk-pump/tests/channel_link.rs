//! `ChannelLink` on its own: what each end sees, without a pump.

use junk_core::{Channel, ChannelSet, Link, LinkError, LinkEvent};
use junk_fake::{CMD, EVT, EXTRA, GATT};
use junk_pump::channel_link::{ChannelLink, DEFAULT_MTU};

fn channels(chans: &[Channel]) -> ChannelSet {
    chans.iter().copied().collect()
}

#[tokio::test]
async fn nothing_works_before_connect() {
    let (mut link, peer) = ChannelLink::pair();
    assert!(!peer.is_connected());
    assert_eq!(link.write(CMD, &[1]).await, Err(LinkError::NotConnected));
    assert_eq!(link.subscribe(EVT).await, Err(LinkError::NotConnected));
    assert_eq!(link.read(EXTRA).await, Err(LinkError::NotConnected));
    assert!(!peer.notify(EVT, vec![1]));
    peer.disconnect();
    link.disconnect().await;
    assert!(!peer.is_connected());
    assert!(peer.subscriptions().is_empty());
    assert!(peer.reads().is_empty());
}

#[tokio::test]
async fn reads_return_what_the_peer_set() {
    let (mut link, mut peer) = ChannelLink::pair();
    link.connect(&GATT).await.unwrap();
    // No value yet; an undeclared channel is unknown, and is not counted as a read.
    assert_eq!(
        link.read(EXTRA).await,
        Err(LinkError::Io("no value".into()))
    );
    assert_eq!(
        link.read(Channel(9)).await,
        Err(LinkError::UnknownChannel(Channel(9)))
    );
    peer.set_value(EXTRA, vec![1, 2]);
    assert_eq!(link.read(EXTRA).await, Ok(vec![1, 2]));
    assert_eq!(link.read(EXTRA).await, Ok(vec![1, 2]));
    peer.set_value(EXTRA, vec![3]);
    assert_eq!(link.read(EXTRA).await, Ok(vec![3]));
    assert_eq!(peer.reads(), vec![EXTRA, EXTRA, EXTRA, EXTRA]);

    // A value set for a channel the connection lacks is unreachable on it.
    peer.set_value(EVT, vec![4]);
    peer.set_resolved([CMD, EXTRA].into_iter().collect());
    link.disconnect().await;
    assert_eq!(link.read(EXTRA).await, Err(LinkError::NotConnected));
    link.connect(&GATT).await.unwrap();
    assert_eq!(link.read(EVT).await, Err(LinkError::UnknownChannel(EVT)));
    assert_eq!(link.read(EXTRA).await, Ok(vec![3]));
    assert_eq!(peer.reads().len(), 5);
}

#[tokio::test]
async fn connect_resolves_what_is_declared_and_present() {
    let (mut link, mut peer) = ChannelLink::pair();
    assert_eq!(
        link.connect(&GATT).await,
        Ok((channels(&[CMD, EVT, EXTRA]), DEFAULT_MTU))
    );
    assert!(peer.is_connected());
    link.disconnect().await;
    assert!(!peer.is_connected());
    assert_eq!(link.write(CMD, &[1]).await, Err(LinkError::NotConnected));

    peer.set_resolved(channels(&[CMD, EVT, Channel(9)]));
    peer.set_mtu(23);
    assert_eq!(link.connect(&GATT).await, Ok((channels(&[CMD, EVT]), 23)));
    assert_eq!(link.write(CMD, &[1, 2]).await, Ok(()));
    assert_eq!(
        link.write(EXTRA, &[1]).await,
        Err(LinkError::UnknownChannel(EXTRA))
    );
    assert_eq!(
        link.subscribe(Channel(9)).await,
        Err(LinkError::UnknownChannel(Channel(9)))
    );
    assert_eq!(link.subscribe(EVT).await, Ok(()));
    assert_eq!(link.subscribe(CMD).await, Ok(()));
    assert_eq!(peer.subscriptions(), vec![EVT, CMD]);
    assert_eq!(peer.next_write().await, Some((CMD, vec![1, 2])));
}

#[tokio::test]
async fn connect_fails_with_the_configured_error() {
    let (mut link, mut peer) = ChannelLink::pair();
    let missing = LinkError::MissingRequired(channels(&[EVT]));
    peer.set_connect_error(Some(missing.clone()));
    assert_eq!(link.connect(&GATT).await, Err(missing));
    assert!(!peer.is_connected());
    peer.set_connect_error(None);
    assert!(link.connect(&GATT).await.is_ok());
    assert!(peer.is_connected());
}

#[tokio::test]
async fn notifications_and_a_disconnect_arrive_in_order() {
    let (mut link, peer) = ChannelLink::pair();
    link.connect(&GATT).await.unwrap();
    assert!(peer.notify(EVT, vec![1]));
    assert!(peer.notify(EXTRA, vec![2, 3]));
    peer.disconnect();
    assert!(!peer.is_connected());
    assert!(!peer.notify(EVT, vec![4]));
    peer.disconnect();

    assert_eq!(
        link.next().await,
        LinkEvent::Rx {
            chan: EVT,
            bytes: vec![1],
        }
    );
    assert_eq!(
        link.next().await,
        LinkEvent::Rx {
            chan: EXTRA,
            bytes: vec![2, 3],
        }
    );
    assert_eq!(link.next().await, LinkEvent::Disconnected);
    assert_eq!(link.write(CMD, &[1]).await, Err(LinkError::NotConnected));

    // The second `disconnect` was a no-op: nothing else is queued, and a reconnect is clean.
    link.connect(&GATT).await.unwrap();
    assert!(peer.notify(EVT, vec![5]));
    assert_eq!(
        link.next().await,
        LinkEvent::Rx {
            chan: EVT,
            bytes: vec![5],
        }
    );
}

#[tokio::test]
async fn a_reconnect_forgets_what_the_device_said_before() {
    let (mut link, peer) = ChannelLink::pair();
    link.connect(&GATT).await.unwrap();
    assert!(peer.notify(EVT, vec![1]));
    link.disconnect().await;
    link.connect(&GATT).await.unwrap();
    assert!(peer.notify(EVT, vec![2]));
    assert_eq!(
        link.next().await,
        LinkEvent::Rx {
            chan: EVT,
            bytes: vec![2],
        }
    );
}

#[tokio::test]
async fn dropping_the_peer_ends_the_link() {
    let (mut link, peer) = ChannelLink::pair();
    link.connect(&GATT).await.unwrap();
    drop(peer);
    assert_eq!(link.next().await, LinkEvent::Disconnected);
    assert_eq!(link.next().await, LinkEvent::Disconnected);
    assert_eq!(link.write(CMD, &[1]).await, Err(LinkError::NotConnected));
    assert!(matches!(link.connect(&GATT).await, Err(LinkError::Io(_))));
}

#[tokio::test]
async fn dropping_the_link_ends_the_peer() {
    let (mut link, mut peer) = ChannelLink::pair();
    link.connect(&GATT).await.unwrap();
    link.write(CMD, &[7]).await.unwrap();
    drop(link);
    assert_eq!(peer.next_write().await, Some((CMD, vec![7])));
    assert_eq!(peer.next_write().await, None);
    assert!(!peer.notify(EVT, vec![1]));
}
