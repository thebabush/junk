//! [`BleLink`]: a [`Link`] over one btleplug peripheral.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use btleplug::api::{
    Central, CentralEvent, Characteristic, DEFAULT_MTU_SIZE, Peripheral as _,
    RetrievePeripheralsOptions, ScanFilter, ValueNotification, WriteType,
};
use btleplug::platform::{Adapter, Peripheral, PeripheralId};
use futures::stream::{Stream, StreamExt};
use junk_core::{Bytes, Channel, ChannelSet, Dir, GattMap, Link, LinkError, LinkEvent, Uuid};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::scan::wanted_services;
use crate::{BleConfig, BleError, CharInfo, resolve};

/// A characteristic as btleplug's notifications identify one: service UUID, then its own.
type CharKey = (Uuid, Uuid);

/// A btleplug stream, boxed as btleplug hands them out.
type BoxStream<T> = Pin<Box<dyn Stream<Item = T> + Send>>;

/// One connection: what was resolved and where its events come from.
struct Session {
    /// Each resolved channel's characteristic, and how the map uses it.
    chars: HashMap<Channel, (Characteristic, Dir)>,
    /// What the forwarding task delivers.
    events: mpsc::UnboundedReceiver<LinkEvent>,
    /// The forwarding task.
    forwarder: JoinHandle<()>,
}

/// A [`Link`] over one btleplug peripheral. See the [crate docs](crate).
///
/// Built by [`BleLink::open`] from a [`PeripheralId`] a [`scan`](crate::scan) reported;
/// connected, used and dropped by the pump like any other link. One link may be connected
/// more than once: the pump runs again with the same link after a disconnect.
pub struct BleLink {
    adapter: Adapter,
    id: PeripheralId,
    peripheral: Peripheral,
    config: BleConfig,
    name: Option<String>,
    session: Option<Session>,
    /// Notifications on characteristics no channel resolved to, dropped.
    dropped_unmapped: Arc<AtomicUsize>,
}

impl BleLink {
    /// A link to the peripheral `id`, which a [`scan`](crate::scan) on `adapter` must have
    /// seen. Not yet connected.
    ///
    /// # Errors
    ///
    /// [`BleError::NotFound`] if the adapter does not know `id`, [`BleError::Btleplug`] if
    /// btleplug fails.
    pub async fn open(
        adapter: &Adapter,
        id: &PeripheralId,
        config: BleConfig,
    ) -> Result<BleLink, BleError> {
        let peripheral = match adapter.peripheral(id).await {
            Ok(peripheral) => peripheral,
            Err(btleplug::Error::DeviceNotFound) => return Err(BleError::NotFound(id.clone())),
            Err(err) => return Err(BleError::Btleplug(err)),
        };
        let name = peripheral
            .properties()
            .await?
            .and_then(|properties| properties.local_name);
        Ok(BleLink {
            adapter: adapter.clone(),
            id: id.clone(),
            peripheral,
            config,
            name,
            session: None,
            dropped_unmapped: Arc::new(AtomicUsize::new(0)),
        })
    }

    /// The peripheral's advertised name, as known when the link was opened.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// How many notifications arrived on characteristics no channel resolved to, and were
    /// dropped, over the life of the link.
    #[must_use]
    pub fn dropped_unmapped(&self) -> usize {
        self.dropped_unmapped.load(Ordering::Relaxed)
    }

    /// The characteristic behind `chan` in the current session, and how the map uses it.
    fn characteristic(&self, chan: Channel) -> Result<(&Characteristic, Dir), LinkError> {
        let session = self.session.as_ref().ok_or(LinkError::NotConnected)?;
        session
            .chars
            .get(&chan)
            .map(|(characteristic, dir)| (characteristic, *dir))
            .ok_or(LinkError::UnknownChannel(chan))
    }

    /// Makes `self.peripheral` a handle btleplug still honours.
    ///
    /// btleplug forgets a peripheral once it disconnects, so a second connection has to
    /// get it back: from the adapter if it still lists it, by identifier where the backend
    /// can retrieve one (macOS can), and otherwise by scanning for `gatt`'s services until
    /// it is seen again or [`BleConfig::scan_timeout`] passes.
    async fn refresh(&mut self, gatt: &GattMap) -> Result<(), LinkError> {
        if let Ok(peripheral) = self.adapter.peripheral(&self.id).await {
            self.peripheral = peripheral;
            return Ok(());
        }
        let by_id = RetrievePeripheralsOptions {
            identifiers: Some(vec![self.id.clone()]),
            services: None,
        };
        if let Ok(mut retrieved) = self.adapter.retrieve_peripherals(by_id).await
            && let Some(peripheral) = retrieved.drain(..).find(|p| p.id() == self.id)
        {
            self.peripheral = peripheral;
            return Ok(());
        }

        let id = self.id.clone();
        let mut events = self.adapter.events().await.io()?;
        self.adapter
            .start_scan(ScanFilter {
                services: wanted_services(gatt),
            })
            .await
            .io()?;
        let seen = tokio::time::timeout(self.config.scan_timeout, async {
            while let Some(event) = events.next().await {
                match event {
                    CentralEvent::DeviceDiscovered(who) | CentralEvent::DeviceUpdated(who)
                        if who == id =>
                    {
                        return true;
                    }
                    _ => {}
                }
            }
            false
        })
        .await
        .unwrap_or(false);
        let _ = self.adapter.stop_scan().await;
        if !seen {
            return Err(LinkError::Io(format!(
                "peripheral {} not seen within {:?}",
                self.id, self.config.scan_timeout
            )));
        }
        self.peripheral = self.adapter.peripheral(&self.id).await.io()?;
        Ok(())
    }

    /// After `connect`: discovers, resolves, subscribes to the streams and starts forwarding.
    async fn open_session(
        &self,
        gatt: &GattMap,
        adapter_events: BoxStream<CentralEvent>,
    ) -> Result<(Session, ChannelSet), LinkError> {
        self.peripheral.discover_services().await.io()?;
        let characteristics: Vec<Characteristic> =
            self.peripheral.characteristics().into_iter().collect();
        let infos: Vec<CharInfo> = characteristics.iter().map(CharInfo::from).collect();
        let resolution = resolve(gatt, &infos)?;

        let mut chars = HashMap::new();
        let mut channels = HashMap::new();
        for (chan, index) in resolution.matched {
            // `resolve` only reports channels the map declares.
            let Some(decl) = gatt.find(chan) else {
                continue;
            };
            let characteristic = characteristics[index].clone();
            channels.insert((characteristic.service_uuid, characteristic.uuid), chan);
            chars.insert(chan, (characteristic, decl.dir));
        }

        // Subscribed before anything is enabled, so no notification can slip past.
        let notifications = self.peripheral.notifications().await.io()?;
        let (tx, rx) = mpsc::unbounded_channel();
        let forwarder = tokio::spawn(forward(
            self.id.clone(),
            notifications,
            adapter_events,
            channels,
            tx,
            Arc::clone(&self.dropped_unmapped),
        ));
        let session = Session {
            chars,
            events: rx,
            forwarder,
        };
        Ok((session, resolution.resolved))
    }
}

/// What [`inspect`] found on a peripheral.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inspection {
    /// Every characteristic, as discovered.
    pub chars: Vec<CharInfo>,
    /// For each characteristic of a service in the `read` list, its service UUID, its
    /// UUID and its value; a characteristic that refused to be read has an empty value.
    pub values: Vec<(Uuid, Uuid, Vec<u8>)>,
}

/// Connects to `id` just long enough to list every characteristic it has and read those
/// of the `read` services, then disconnects: what to look at when a map does not resolve,
/// or a reply seems to arrive on a characteristic the map does not declare.
///
/// One connection does both, because btleplug forgets a peripheral once it disconnects.
///
/// # Errors
///
/// [`BleError::NotFound`] if the adapter does not know `id`, [`BleError::Btleplug`] if
/// connecting or discovering fails.
pub async fn inspect(
    adapter: &Adapter,
    id: &PeripheralId,
    read: &[Uuid],
) -> Result<Inspection, BleError> {
    let peripheral = match adapter.peripheral(id).await {
        Ok(peripheral) => peripheral,
        Err(btleplug::Error::DeviceNotFound) => return Err(BleError::NotFound(id.clone())),
        Err(err) => return Err(BleError::Btleplug(err)),
    };
    peripheral.connect().await?;
    let inspected = async {
        peripheral.discover_services().await?;
        let characteristics = peripheral.characteristics();
        let chars = characteristics.iter().map(CharInfo::from).collect();
        let mut values = Vec::new();
        for characteristic in &characteristics {
            if !read.contains(&characteristic.service_uuid) {
                continue;
            }
            let value = peripheral.read(characteristic).await.unwrap_or_default();
            values.push((characteristic.service_uuid, characteristic.uuid, value));
        }
        Ok(Inspection { chars, values })
    }
    .await;
    let _ = peripheral.disconnect().await;
    inspected
}

impl Link for BleLink {
    async fn connect(&mut self, gatt: &GattMap) -> Result<(ChannelSet, u16), LinkError> {
        // Whatever an earlier session left behind is over.
        self.disconnect().await;
        self.refresh(gatt).await?;
        // Subscribed before connecting, so a disconnect during or right after it is seen.
        let adapter_events = self.adapter.events().await.io()?;
        self.peripheral.connect().await.io()?;
        let (session, resolved) = match self.open_session(gatt, adapter_events).await {
            Ok(opened) => opened,
            Err(err) => {
                let _ = self.peripheral.disconnect().await;
                return Err(err);
            }
        };
        self.session = Some(session);
        let mtu = report_mtu(self.peripheral.mtu(), self.config.assumed_mtu);
        Ok((resolved, mtu))
    }

    async fn write(&mut self, chan: Channel, bytes: &[u8]) -> Result<(), LinkError> {
        let (characteristic, dir) = self.characteristic(chan)?;
        let write_type = match dir {
            Dir::Write => WriteType::WithResponse,
            Dir::WriteNoResponse => WriteType::WithoutResponse,
            // The map says the host does not write here.
            Dir::Notify | Dir::Indicate | Dir::Read => {
                return Err(LinkError::UnknownChannel(chan));
            }
            // Unreachable: a stream channel never resolves (`CharInfo::supports`), so the
            // lookup above already failed. The answer is the same either way.
            Dir::Stream => return Err(LinkError::UnknownChannel(chan)),
        };
        self.peripheral
            .write(characteristic, bytes, write_type)
            .await
            .io()
    }

    async fn subscribe(&mut self, chan: Channel) -> Result<(), LinkError> {
        let (characteristic, dir) = self.characteristic(chan)?;
        match dir {
            Dir::Notify | Dir::Indicate => {}
            // The map says the device does not push here.
            Dir::Write | Dir::WriteNoResponse | Dir::Read => {
                return Err(LinkError::UnknownChannel(chan));
            }
            // Unreachable, as in `write`: a stream channel never resolves over GATT. A
            // stream link subscribes to one with `Ok(())`; this is not one.
            Dir::Stream => return Err(LinkError::UnknownChannel(chan)),
        }
        self.peripheral.subscribe(characteristic).await.io()
    }

    async fn read(&mut self, chan: Channel) -> Result<Bytes, LinkError> {
        let (characteristic, dir) = self.characteristic(chan)?;
        match dir {
            Dir::Read => {}
            // The map says the host does not read here.
            Dir::Write | Dir::WriteNoResponse | Dir::Notify | Dir::Indicate => {
                return Err(LinkError::UnknownChannel(chan));
            }
            // Unreachable, as in `write`: a stream channel never resolves over GATT. It
            // has nothing to read on any transport either.
            Dir::Stream => return Err(LinkError::UnknownChannel(chan)),
        }
        self.peripheral.read(characteristic).await.io()
    }

    async fn disconnect(&mut self) {
        if let Some(session) = self.session.take() {
            let _ = self.peripheral.disconnect().await;
            session.forwarder.abort();
        }
    }

    // Cancel-safe: `recv` on an mpsc receiver loses nothing when dropped early, and the
    // forwarding task keeps the btleplug streams drained meanwhile.
    async fn next(&mut self) -> LinkEvent {
        let Some(session) = &mut self.session else {
            return LinkEvent::Disconnected;
        };
        match session.events.recv().await {
            Some(LinkEvent::Rx { chan, bytes }) => LinkEvent::Rx { chan, bytes },
            // The task ends after saying so; a channel closed without it is the same news.
            Some(LinkEvent::Disconnected) | None => {
                if let Some(session) = self.session.take() {
                    session.forwarder.abort();
                }
                LinkEvent::Disconnected
            }
        }
    }
}

/// A btleplug result as a link result: any failure is [`LinkError::Io`] with btleplug's
/// own words.
trait IoExt<T> {
    fn io(self) -> Result<T, LinkError>;
}

impl<T> IoExt<T> for btleplug::Result<T> {
    fn io(self) -> Result<T, LinkError> {
        self.map_err(|err| LinkError::Io(err.to_string()))
    }
}

/// The MTU `connect` reports: what btleplug learned if it learned anything, else `assumed`.
///
/// btleplug reports the BLE default of 23 until a backend has learned the negotiated value,
/// so 23 is read as "nothing learned"; that loses nothing, since a device that really
/// negotiated 23 supports nothing more than the assumed value promises anyway.
fn report_mtu(learned: u16, assumed: u16) -> u16 {
    if learned > DEFAULT_MTU_SIZE {
        learned
    } else {
        assumed
    }
}

/// Forwards what btleplug says about the peripheral `id` into `events` until it
/// disconnects, then says so and ends.
///
/// Notifications on characteristics not in `channels` are dropped and counted in
/// `dropped_unmapped`; with the environment variable `JUNK_BLE_DEBUG` set, each is also
/// printed on stderr with its UUIDs and bytes, which is how a characteristic the map does
/// not declare yet gets found. Notifications are taken before adapter events, so data that
/// arrived before a disconnect is delivered before it. Either stream ending is read as the
/// peripheral being gone: nothing more could arrive on it.
async fn forward(
    id: PeripheralId,
    mut notifications: BoxStream<ValueNotification>,
    mut adapter_events: BoxStream<CentralEvent>,
    channels: HashMap<CharKey, Channel>,
    events: mpsc::UnboundedSender<LinkEvent>,
    dropped_unmapped: Arc<AtomicUsize>,
) {
    let debug = std::env::var_os("JUNK_BLE_DEBUG").is_some();
    loop {
        let event = tokio::select! {
            biased;
            notification = notifications.next() => match notification {
                Some(ValueNotification { uuid, service_uuid, value }) => {
                    let Some(&chan) = channels.get(&(service_uuid, uuid)) else {
                        dropped_unmapped.fetch_add(1, Ordering::Relaxed);
                        if debug {
                            let hex = value.iter().fold(String::new(), |mut hex, b| {
                                let _ = write!(hex, "{b:02x}");
                                hex
                            });
                            eprintln!("junk-ble: unmapped notification on {service_uuid}/{uuid}: {hex}");
                        }
                        continue;
                    };
                    LinkEvent::Rx { chan, bytes: value }
                }
                None => LinkEvent::Disconnected,
            },
            event = adapter_events.next() => match event {
                Some(CentralEvent::DeviceDisconnected(who)) if who == id => {
                    LinkEvent::Disconnected
                }
                Some(_) => continue,
                None => LinkEvent::Disconnected,
            },
        };
        let over = event == LinkEvent::Disconnected;
        if events.send(event).is_err() || over {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;

    #[test]
    fn learned_mtu_wins_over_assumed_unless_it_is_the_default() {
        assert_eq!(report_mtu(247, 23), 247);
        assert_eq!(report_mtu(185, 247), 185);
        assert_eq!(report_mtu(23, 23), 23);
        assert_eq!(report_mtu(23, 247), 247);
        assert_eq!(report_mtu(0, 247), 247);
    }

    // The forwarding task needs a `PeripheralId`, which only the Apple backend lets a test
    // build (from a `Uuid`); the task itself is platform-neutral.
    #[cfg(target_vendor = "apple")]
    mod forwarding {
        use super::*;

        const SVC: Uuid = Uuid::from_u128(0x10);
        const CH_A: Uuid = Uuid::from_u128(0x11);
        const CH_B: Uuid = Uuid::from_u128(0x12);
        const A: Channel = Channel(4);

        fn id(n: u128) -> PeripheralId {
            PeripheralId::from(Uuid::from_u128(n))
        }

        fn notification(service: Uuid, uuid: Uuid, value: &[u8]) -> ValueNotification {
            ValueNotification {
                uuid,
                service_uuid: service,
                value: value.to_vec(),
            }
        }

        struct Run {
            events: mpsc::UnboundedReceiver<LinkEvent>,
            dropped: Arc<AtomicUsize>,
            task: JoinHandle<()>,
        }

        fn run(
            notifications: Vec<ValueNotification>,
            end_notifications: bool,
            adapter_events: Vec<CentralEvent>,
            end_adapter_events: bool,
        ) -> Run {
            let notifications: BoxStream<ValueNotification> = if end_notifications {
                Box::pin(stream::iter(notifications))
            } else {
                Box::pin(stream::iter(notifications).chain(stream::pending()))
            };
            let adapter_events: BoxStream<CentralEvent> = if end_adapter_events {
                Box::pin(stream::iter(adapter_events))
            } else {
                Box::pin(stream::iter(adapter_events).chain(stream::pending()))
            };
            let channels = HashMap::from([((SVC, CH_A), A)]);
            let (tx, rx) = mpsc::unbounded_channel();
            let dropped = Arc::new(AtomicUsize::new(0));
            let task = tokio::spawn(forward(
                id(1),
                notifications,
                adapter_events,
                channels,
                tx,
                Arc::clone(&dropped),
            ));
            Run {
                events: rx,
                dropped,
                task,
            }
        }

        #[tokio::test]
        async fn maps_notifications_and_counts_unmapped_ones() {
            let mut run = run(
                vec![
                    notification(SVC, CH_A, &[1, 2]),
                    notification(SVC, CH_B, &[3]),
                    notification(Uuid::from_u128(0x99), CH_A, &[4]),
                    notification(SVC, CH_A, &[5]),
                ],
                false,
                vec![],
                false,
            );
            assert_eq!(
                run.events.recv().await,
                Some(LinkEvent::Rx {
                    chan: A,
                    bytes: vec![1, 2]
                })
            );
            assert_eq!(
                run.events.recv().await,
                Some(LinkEvent::Rx {
                    chan: A,
                    bytes: vec![5]
                })
            );
            assert_eq!(run.dropped.load(Ordering::Relaxed), 2);
            assert!(!run.task.is_finished());
            run.task.abort();
        }

        #[tokio::test]
        async fn our_disconnect_ends_the_task_after_the_data() {
            let mut run = run(
                vec![notification(SVC, CH_A, &[7])],
                false,
                vec![
                    CentralEvent::DeviceDisconnected(id(2)),
                    CentralEvent::DeviceConnected(id(1)),
                    CentralEvent::DeviceDisconnected(id(1)),
                ],
                false,
            );
            assert_eq!(
                run.events.recv().await,
                Some(LinkEvent::Rx {
                    chan: A,
                    bytes: vec![7]
                })
            );
            assert_eq!(run.events.recv().await, Some(LinkEvent::Disconnected));
            assert_eq!(run.events.recv().await, None);
            run.task.await.expect("ended on its own");
        }

        #[tokio::test]
        async fn another_peripherals_disconnect_is_ignored() {
            let mut run = run(
                vec![],
                false,
                vec![CentralEvent::DeviceDisconnected(id(2))],
                false,
            );
            tokio::task::yield_now().await;
            assert!(run.events.try_recv().is_err());
            assert!(!run.task.is_finished());
            run.task.abort();
        }

        #[tokio::test]
        async fn a_stream_ending_is_a_disconnect() {
            let mut run = run(vec![], true, vec![], false);
            assert_eq!(run.events.recv().await, Some(LinkEvent::Disconnected));
            run.task.await.expect("ended on its own");

            let mut run = run_adapter_gone();
            assert_eq!(run.events.recv().await, Some(LinkEvent::Disconnected));
            run.task.await.expect("ended on its own");
        }

        fn run_adapter_gone() -> Run {
            run(vec![], false, vec![], true)
        }

        #[tokio::test]
        async fn a_dropped_receiver_ends_the_task() {
            let run = run(
                vec![notification(SVC, CH_A, &[1]), notification(SVC, CH_A, &[2])],
                false,
                vec![],
                false,
            );
            drop(run.events);
            run.task.await.expect("ended on its own");
        }
    }
}
