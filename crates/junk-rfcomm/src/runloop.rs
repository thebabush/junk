//! The one thread that talks to `IOBluetooth`, and the delegate it installs.
//!
//! `IOBluetooth` is a `CFRunLoop` framework: it delivers everything through the run loop of
//! the thread that created the object, and several of its calls block. So exactly one
//! thread here touches it. That thread finds the device, runs the SDP query, opens the
//! RFCOMM channel, installs [`Delegate`] on it and then does nothing but turn the run loop,
//! taking [`Command`]s off a [`std::sync::mpsc`] between turns and pushing [`Report`]s into
//! a [`tokio::sync::mpsc`]. Nothing on the async side ever calls into the framework.
//!
//! The run loop is turned in [`TICK`] slices rather than being woken by a `CFRunLoopSource`
//! of our own: a turn returns as soon as the device says anything, so the only thing the
//! slice bounds is how long a write waits for a quiet channel to notice it.
//!
//! # Which run loop delivers, and why this thread's is not enough
//!
//! Turning this thread's run loop is necessary but, on the macOS in front of me, not
//! sufficient. Disassembling `IOBluetooth.framework` (26.0, arm64e) shows
//! `-[IOBluetoothRFCOMMChannel setupRFCOMMChannelForDevice]` scheduling the channel's input
//! and output `NSStream`s on `[NSRunLoop mainRunLoop]`, and both
//! `rfcommChannelOpenComplete:status:` and `rfcommChannelData:data:length:` being sent from
//! blocks dispatched out of `-[IOBluetoothRFCOMMChannel stream:handleEvent:]`. So the
//! channel's events reach a process only while its **main** thread turns its run loop, and
//! the delegate is then called on whatever thread the `dispatch_async` lands on — not
//! necessarily this one. That is why [`Status`] and `closed` are atomics rather than
//! `Cell`s, and why a host puts tokio on a second thread and leaves the main thread in
//! [`turn_main_loop`](crate::turn_main_loop).
//!
//! This was first read out of the framework and has since been measured. Against a
//! powered-on Motion 300 on 2026-09-15: a plain `#[tokio::main]`, with nobody turning the
//! main run loop, saw no callback at all and `connect` gave up after 25 s; with the main
//! thread turning the main run loop, the same `connect` returned in 122 ms and the channel
//! delivered.

use std::ffi::{c_int, c_uint, c_void};
use std::slice;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use junk_core::{Bytes, Uuid};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_foundation::{NSDate, NSDefaultRunLoopMode, NSRunLoop, NSString};
use objc2_io_bluetooth::{
    BluetoothRFCOMMChannelID, IOBluetoothDevice, IOBluetoothRFCOMMChannel,
    IOBluetoothRFCOMMChannelDelegate, IOBluetoothSDPUUID,
};
use tokio::sync::{mpsc, oneshot};

use crate::RfcommError;

/// `kIOReturnSuccess`. Every other `IOReturn` is a failure of some kind.
const SUCCESS: c_int = 0;

/// The length of a 128-bit SDP UUID, in bytes.
const UUID_BYTES: c_uint = 16;

/// How long one turn of the run loop waits when the device has nothing to say.
const TICK: Duration = Duration::from_millis(10);

/// How long the SDP query is given before the cached service records are used instead.
const SDP_TIMEOUT: Duration = Duration::from_secs(20);

/// How long the RFCOMM channel is given to finish opening.
const OPEN_TIMEOUT: Duration = Duration::from_secs(20);

/// What the async side asks the thread to do.
pub enum Command {
    /// Send these bytes, and say on `done` whether they went.
    Write {
        /// The bytes, owned so that the thread can hand `IOBluetooth` a pointer to them.
        bytes: Bytes,
        /// Where the outcome goes; dropped if the caller gave up waiting.
        done: oneshot::Sender<Result<(), String>>,
    },
    /// Close the channel and end the thread.
    Close,
}

/// What the thread tells the async side.
pub enum Report {
    /// The channel is open. Sent exactly once, before any [`Report::Data`].
    Opened {
        /// The channel's maximum transfer unit.
        mtu: u16,
        /// The device's name, as the Mac knows it.
        name: Option<String>,
    },
    /// The channel never opened. Sent instead of [`Report::Opened`], and last.
    Failed(RfcommError),
    /// One chunk of the byte stream, in whatever size the radio delivered it.
    Data(Bytes),
    /// The channel is closed and the thread is over. Always the last report.
    Closed,
}

/// One `IOReturn` the delegate records once and the thread waits for.
///
/// Atomic rather than a `Cell` because the two are not reliably the same thread: see the
/// note on which run loop delivers, in the [module docs](self).
#[derive(Default)]
struct Status {
    /// Whether `value` has been written.
    known: AtomicBool,
    /// The status, meaningless until `known`.
    value: AtomicI32,
}

impl Status {
    /// Records `value`, if nothing was recorded before.
    fn set(&self, value: c_int) {
        self.value.store(value, Ordering::Relaxed);
        self.known.store(true, Ordering::Release);
    }

    /// The status, once there is one.
    fn get(&self) -> Option<c_int> {
        self.known
            .load(Ordering::Acquire)
            .then(|| self.value.load(Ordering::Relaxed))
    }
}

/// What the delegate keeps: where to put data, and what the thread is waiting for.
struct DelegateIvars {
    /// Where data goes. Cloned from the thread's own sender.
    reports: mpsc::UnboundedSender<Report>,
    /// The `IOReturn` of the SDP query, once it has finished.
    sdp_status: Status,
    /// The `IOReturn` of the channel open, once it has finished.
    open_status: Status,
    /// Whether the device closed the channel.
    closed: AtomicBool,
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - `Delegate` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[ivars = DelegateIvars]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    // SAFETY: every method below has the signature IOBluetooth calls it with, taken from
    // the protocol declaration in `objc2-io-bluetooth`.
    unsafe impl IOBluetoothRFCOMMChannelDelegate for Delegate {
        #[unsafe(method(rfcommChannelData:data:length:))]
        fn data(
            &self,
            _channel: Option<&IOBluetoothRFCOMMChannel>,
            data: *mut c_void,
            length: usize,
        ) {
            if data.is_null() || length == 0 {
                return;
            }
            // SAFETY: IOBluetooth documents `data` as `length` readable bytes belonging to
            // the channel's receive buffer and valid for the duration of this callback.
            // They are copied here and the pointer is not kept.
            let bytes = unsafe { slice::from_raw_parts(data.cast::<u8>(), length) }.to_vec();
            let _ = self.ivars().reports.send(Report::Data(bytes));
        }

        #[unsafe(method(rfcommChannelOpenComplete:status:))]
        fn open_complete(&self, _channel: Option<&IOBluetoothRFCOMMChannel>, status: c_int) {
            self.ivars().open_status.set(status);
        }

        #[unsafe(method(rfcommChannelClosed:))]
        fn closed(&self, _channel: Option<&IOBluetoothRFCOMMChannel>) {
            self.ivars().closed.store(true, Ordering::Release);
        }
    }

    impl Delegate {
        /// `sdpQueryComplete:status:` is an informal protocol — a selector `IOBluetooth`
        /// sends to whatever object `performSDPQuery:` was given — so it is declared here
        /// rather than under a protocol.
        ///
        // SAFETY: the signature is the one in `IOBluetoothDevice.h`.
        #[unsafe(method(sdpQueryComplete:status:))]
        fn sdp_query_complete(&self, _device: Option<&IOBluetoothDevice>, status: c_int) {
            self.ivars().sdp_status.set(status);
        }
    }
);

impl Delegate {
    /// A delegate that reports into `reports` and has seen nothing yet.
    fn new(reports: mpsc::UnboundedSender<Report>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(DelegateIvars {
            reports,
            sdp_status: Status::default(),
            open_status: Status::default(),
            closed: AtomicBool::new(false),
        });
        // SAFETY: `init` on a freshly allocated NSObject subclass, which is what
        // `set_ivars` produced.
        unsafe { msg_send![super(this), init] }
    }
}

/// An open channel and what was learned opening it.
struct Opened {
    /// The device, kept alive for as long as its channel is.
    _device: Retained<IOBluetoothDevice>,
    /// The channel itself.
    channel: Retained<IOBluetoothRFCOMMChannel>,
    /// Its maximum transfer unit, which is what `connect` reports as the MTU.
    mtu: u16,
    /// The device's name, if the Mac has one for it.
    name: Option<String>,
}

/// The thread body: open the channel, serve `commands` until it closes, say so and stop.
///
/// Every failure is one final [`Report`], so the async side is never left waiting.
pub fn run(
    address: &str,
    service: Uuid,
    commands: &Receiver<Command>,
    reports: &mpsc::UnboundedSender<Report>,
) {
    let delegate = Delegate::new(reports.clone());
    let opened = match open(address, service, &delegate) {
        Ok(opened) => opened,
        Err(why) => {
            let _ = reports.send(Report::Failed(why));
            return;
        }
    };
    let _ = reports.send(Report::Opened {
        mtu: opened.mtu,
        name: opened.name.clone(),
    });
    pump(&opened, &delegate, commands);
    close(&opened.channel);
    let _ = reports.send(Report::Closed);
}

/// Finds the device, queries SDP for `service`, and opens its RFCOMM channel.
fn open(address: &str, service: Uuid, delegate: &Delegate) -> Result<Opened, RfcommError> {
    let io = |why: String| RfcommError::Io { why };
    let device = device(address)?;
    let name = name_of(&device, address);
    // Deref coercion: `Delegate` is an `NSObject` is an `AnyObject`.
    let target: &AnyObject = delegate;

    let bytes = service.as_bytes();
    // SAFETY: `bytes` is exactly `UUID_BYTES` readable bytes, and IOBluetooth copies them
    // into the UUID object it returns rather than keeping the pointer.
    let sdp_uuid =
        unsafe { IOBluetoothSDPUUID::uuidWithBytes_length(bytes.as_ptr().cast(), UUID_BYTES) };
    let sdp_uuid =
        sdp_uuid.ok_or_else(|| io(format!("IOBluetooth would not take the UUID {service}")))?;

    // The query is asynchronous and answers on the delegate. A device the Mac has queried
    // before also has cached records, so failing to start one is not on its own fatal:
    // what matters is whether a record for `service` can be had afterwards.
    // SAFETY: `target` is a live object that implements `sdpQueryComplete:status:`, which
    // is the whole of what IOBluetooth asks of it.
    let started = unsafe { device.performSDPQuery(Some(target)) };
    trace(|| format!("performSDPQuery started (IOReturn {started:#x})"));
    if started == SUCCESS {
        wait("sdp query", delegate, SDP_TIMEOUT, |ivars| {
            ivars.sdp_status.get()
        });
    }
    // SAFETY: `sdp_uuid` is a live IOBluetoothSDPUUID; the search only reads it.
    let record = unsafe { device.getServiceRecordForUUID(Some(&sdp_uuid)) };
    let record = record.ok_or(RfcommError::NoService { uuid: service })?;
    trace(|| format!("service record for {service} found"));

    let mut channel_id: BluetoothRFCOMMChannelID = 0;
    // SAFETY: the pointer is to a live local of exactly the type asked for.
    let status = unsafe { record.getRFCOMMChannelID(&raw mut channel_id) };
    if status != SUCCESS {
        return Err(io(format!(
            "service {service} declares no RFCOMM channel number (IOReturn {status:#x})"
        )));
    }

    let mut channel: Option<Retained<IOBluetoothRFCOMMChannel>> = None;
    // SAFETY: the out-parameter is a live local `Option<Retained<_>>`, which is the layout
    // this expects; `target` implements the channel delegate protocol and, as the thread
    // outlives the channel, stays alive for as long as the channel can call it.
    let status = unsafe {
        device.openRFCOMMChannelAsync_withChannelID_delegate(
            Some(&mut channel),
            channel_id,
            Some(target),
        )
    };
    if status != SUCCESS {
        return Err(io(format!(
            "RFCOMM channel {channel_id} would not open (IOReturn {status:#x})"
        )));
    }
    let channel =
        channel.ok_or_else(|| io(format!("RFCOMM channel {channel_id} opened as nothing")))?;
    trace(|| {
        // SAFETY: reading properties of a live channel and its device.
        let (open, connected) = unsafe { (channel.isOpen(), device.isConnected()) };
        format!(
            "openRFCOMMChannelAsync on channel {channel_id} returned; isOpen {open}, device connected {connected}"
        )
    });

    let opened = wait("channel open", delegate, OPEN_TIMEOUT, |ivars| {
        ivars.open_status.get()
    });
    let Some(SUCCESS) = opened else {
        close(&channel);
        return Err(io(match opened {
            Some(status) => {
                format!("RFCOMM channel {channel_id} refused to open (IOReturn {status:#x})")
            }
            None => format!(
                "RFCOMM channel {channel_id} did not open within {OPEN_TIMEOUT:?}: the device \
                 may be off, or nothing in this process is turning the main run loop, which \
                 is where IOBluetooth schedules the channel"
            ),
        }));
    };
    // SAFETY: reading a property of a channel that has just finished opening.
    let mtu = unsafe { channel.getMTU() };
    Ok(Opened {
        _device: device,
        channel,
        mtu,
        name,
    })
}

/// The paired device at `address`.
fn device(address: &str) -> Result<Retained<IOBluetoothDevice>, RfcommError> {
    let written = NSString::from_str(address);
    // SAFETY: `written` is a live NSString in the `XX:XX:XX:XX:XX:XX` form this documents,
    // which `crate::address::normalise` guaranteed.
    let device = unsafe { IOBluetoothDevice::deviceWithAddressString(Some(&written)) };
    let device = device.ok_or_else(|| RfcommError::BadAddress {
        got: address.to_owned(),
    })?;
    // SAFETY: reading properties of a live device.
    trace(|| unsafe {
        format!(
            "device {address}: paired {}, connected {}",
            device.isPaired(),
            device.isConnected()
        )
    });
    // SAFETY: reading a property of a live device. Bluetooth Classic has no
    // scan-then-connect: an unpaired device has no baseband link to build an SDP query on.
    if unsafe { device.isPaired() } {
        Ok(device)
    } else {
        Err(RfcommError::NotPaired {
            address: address.to_owned(),
        })
    }
}

/// The device's name, or `None` when the Mac has never learned one and answers with the
/// address instead.
fn name_of(device: &IOBluetoothDevice, address: &str) -> Option<String> {
    // SAFETY: reading a property of a live device. `nameOrAddress` is used rather than
    // `name` because the latter is bound as non-null and really can be nil.
    let name = unsafe { device.nameOrAddress() }?.to_string();
    let dashed = address.replace(':', "-");
    (name != dashed && name != address).then_some(name)
}

/// Serves `commands` and turns the run loop until the channel closes or the link says stop.
fn pump(opened: &Opened, delegate: &Delegate, commands: &Receiver<Command>) {
    let run_loop = NSRunLoop::currentRunLoop();
    loop {
        if delegate.ivars().closed.load(Ordering::Acquire) {
            return;
        }
        match commands.try_recv() {
            Ok(Command::Write { mut bytes, done }) => {
                let _ = done.send(write(&opened.channel, &mut bytes, opened.mtu));
                // There may be more waiting; the run loop can have its turn after them.
                continue;
            }
            // A dropped sender is the link being dropped, which is also "stop".
            Ok(Command::Close) | Err(TryRecvError::Disconnected) => return,
            Err(TryRecvError::Empty) => {}
        }
        turn(&run_loop);
    }
}

/// Sends `bytes` on `channel`, a channel MTU at a time.
///
/// `writeSync:length:` blocks until the data is out, which is why it is only ever called
/// here, on the thread that owns the run loop.
fn write(channel: &IOBluetoothRFCOMMChannel, bytes: &mut [u8], mtu: u16) -> Result<(), String> {
    // A zero MTU would be nonsense from the framework; one byte at a time still works.
    let most = usize::from(mtu).max(1);
    for chunk in bytes.chunks_mut(most) {
        let len = u16::try_from(chunk.len()).map_err(|_| "chunk larger than a u16".to_owned())?;
        // SAFETY: `chunk` is a live, uniquely borrowed slice of exactly `len` bytes that
        // outlives this synchronous call, and IOBluetooth only reads from it.
        let status = unsafe { channel.writeSync_length(chunk.as_mut_ptr().cast(), len) };
        if status != SUCCESS {
            return Err(format!("writing {len} bytes failed (IOReturn {status:#x})"));
        }
    }
    Ok(())
}

/// Closes `channel` and takes the delegate off it.
///
/// Since Mac OS X 10.6 a channel does not retain its delegate, so leaving ours installed
/// past the thread's life would leave `IOBluetooth` holding a dangling pointer.
fn close(channel: &IOBluetoothRFCOMMChannel) {
    // SAFETY: closing a live channel; an already closed one answers kIOReturnNotOpen,
    // which is nothing to do about.
    unsafe { channel.closeChannel() };
    // SAFETY: nil is how a delegate is unregistered.
    unsafe { channel.setDelegate(None) };
}

/// Turns the run loop until `ready` answers or `timeout` passes.
fn wait<T>(
    what: &str,
    delegate: &Delegate,
    timeout: Duration,
    ready: impl Fn(&DelegateIvars) -> Option<T>,
) -> Option<T> {
    let run_loop = NSRunLoop::currentRunLoop();
    let started = Instant::now();
    let deadline = started + timeout;
    let mut turns = 0usize;
    let mut served = 0usize;
    loop {
        if let Some(answer) = ready(delegate.ivars()) {
            trace(|| {
                let took = started.elapsed();
                format!("{what}: answered after {took:?}, {served} of {turns} turns served")
            });
            return Some(answer);
        }
        if Instant::now() >= deadline {
            trace(|| format!("{what}: nothing in {timeout:?}, {served} of {turns} turns served"));
            return None;
        }
        turns += 1;
        served += usize::from(turn(&run_loop));
    }
}

/// One turn of the run loop: at most [`TICK`], returning as soon as a source has fired.
///
/// Answers whether the run loop had anything at all to run.
fn turn(run_loop: &NSRunLoop) -> bool {
    // SAFETY: a Foundation constant, initialised before any Rust code runs.
    let mode = unsafe { NSDefaultRunLoopMode };
    let until = NSDate::dateWithTimeIntervalSinceNow(TICK.as_secs_f64());
    if run_loop.runMode_beforeDate(mode, &until) {
        true
    } else {
        // The run loop had no input source at all and so returned at once. Sleep the slice
        // rather than spin; IOBluetooth attaches its own source as soon as it has one.
        std::thread::sleep(TICK);
        false
    }
}

/// Writes one line to stderr when `JUNK_RFCOMM_DEBUG` is set.
///
/// Everything the framework does happens inside one `connect`, on a thread with no other
/// way of saying what it saw; this is how a channel that will not open gets looked at.
fn trace(line: impl FnOnce() -> String) {
    if std::env::var_os("JUNK_RFCOMM_DEBUG").is_some() {
        eprintln!("junk-rfcomm: {}", line());
    }
}
