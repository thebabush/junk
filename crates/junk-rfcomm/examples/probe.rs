//! Opens an RFCOMM channel to a Soundcore speaker, asks it who it is, and prints whatever
//! comes back.
//!
//! ```text
//! cargo run -p junk-rfcomm --example probe -- [address] [service-uuid] [--reversed]
//! ```
//!
//! Both arguments default to the Motion 300 this was written against and its vendor
//! control service.
//!
//! # The question this settled
//!
//! Gadgetbridge declares `START_OF_PACKET_HOST = (short)0xee08` and writes it through a
//! **little-endian** `ByteBuffer`, so the bytes on the wire are `08 ee`, not `ee 08`;
//! likewise the device's `0xff09` arrives as `09 ff`. `docs/soundcore-motion-300.md` and
//! `crates/junk-soundcore` both had these reversed. This probe sends the corrected form
//! and prints the reply byte for byte; `--reversed` sends the other one instead, so the
//! two can be compared against the same speaker.
//!
//! Run against a Motion 300 on 2026-09-15, the corrected form was answered: the reply
//! began `09 ff 00 00 01` and decoded exactly as documented. Both crates and the doc were
//! corrected to match, and the exchange is kept as
//! `fixtures/soundcore-motion-300/probe-2026-09-15.trace`.
//!
//! The only thing it ever sends is command `0x0101`, device info, with an empty payload.
//! That is a pure read: nothing here changes a setting or powers anything off.
//!
//! # Why the shape of `main`
//!
//! `IOBluetooth` schedules an RFCOMM channel on the **main** run loop (see the `junk-rfcomm`
//! crate docs), so the main thread stays turning it with `turn_main_loop` and the async
//! work goes to a second thread. That is the shape any host of this link has to take, and
//! it is measured rather than assumed: without it this `connect` times out after 25 s, with
//! it the speaker answered in 122 ms.

use std::error::Error;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

use junk_core::{Channel, CharDecl, Dir, GattMap, Link, LinkEvent, ServiceDecl, Uuid};
use junk_rfcomm::{RfcommLink, turn_main_loop};

/// Any error, in a form that can cross the thread the probe runs on.
type Boxed = Box<dyn Error + Send + Sync>;

/// The speaker this was written against: a Soundcore Motion 300, paired to this Mac.
const ADDRESS: &str = "F4:2B:7D:A1:B2:C3";

/// Its vendor control service, from Gadgetbridge's `SoundcoreMotion300DeviceSupport`.
const SERVICE: &str = "0cf12d31-fac3-4553-bd80-d6832e7b3135";

/// The channel the map below names the stream by.
const STREAM: Channel = Channel(0);

/// How long to listen after the write before giving up.
const LISTEN: Duration = Duration::from_secs(5);

/// Start of packet, host to device, as a little-endian `ByteBuffer` writes `0xee08`.
const CORRECTED: [u8; 2] = [0x08, 0xee];

/// The same two bytes the doc and `junk-soundcore` had before this probe: kept so the
/// experiment can be repeated, not because anything sends them.
const REVERSED: [u8; 2] = [0xee, 0x08];

fn main() -> Result<(), Boxed> {
    let mut positional = Vec::new();
    let mut reversed = false;
    for arg in std::env::args().skip(1) {
        if arg == "--reversed" {
            reversed = true;
        } else {
            positional.push(arg);
        }
    }
    let address = positional
        .first()
        .map_or(ADDRESS, String::as_str)
        .to_owned();
    let service: Uuid = positional.get(1).map_or(SERVICE, String::as_str).parse()?;

    // The probe runs on a second thread so that this one can keep the main run loop going,
    // which is the only thing IOBluetooth delivers an RFCOMM channel through.
    let (done, outcome) = std::sync::mpsc::channel();
    let probe = std::thread::Builder::new()
        .name("probe".to_owned())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a tokio runtime");
            let _ = done.send(runtime.block_on(probe(&address, service, reversed)));
        })?;
    turn_main_loop(|| probe.is_finished());
    probe.join().map_err(|_| "the probe thread panicked")?;
    outcome.recv()?
}

/// Connects, asks the speaker who it is, and prints what comes back.
async fn probe(address: &str, service: Uuid, reversed: bool) -> Result<(), Boxed> {
    let mut link = RfcommLink::open(address)?;
    println!("address    {}", link.address());
    println!("service    {service}");
    let (resolved, mtu) = link.connect(&map(service)).await?;
    println!(
        "connected  name {:?}, channels {resolved:?}, mtu {mtu}",
        link.name()
    );

    let frame = device_info_request(if reversed { REVERSED } else { CORRECTED });
    println!(
        "sending    {}   ({} start of packet, command 0x0101, no payload, len 10, checksum {:#04x})",
        hex(&frame),
        if reversed { "reversed" } else { "corrected" },
        frame[9]
    );

    let sent = Instant::now();
    link.write(STREAM, &frame).await?;
    let deadline = sent + LISTEN;
    let mut chunks = 0usize;
    let mut total = 0usize;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        match tokio::time::timeout(left, link.next()).await {
            Ok(LinkEvent::Rx { bytes, .. }) => {
                chunks += 1;
                total += bytes.len();
                println!(
                    "  +{:>8.1} ms  {}",
                    sent.elapsed().as_secs_f64() * 1000.0,
                    hex(&bytes)
                );
            }
            Ok(LinkEvent::Disconnected) => {
                println!("  the speaker dropped the channel");
                break;
            }
            Err(_) => break,
        }
    }
    println!("received   {chunks} chunk(s), {total} bytes, within {LISTEN:?}");
    link.disconnect().await;
    Ok(())
}

/// The one-stream map an RFCOMM device family declares, for `service`.
///
/// Leaked rather than `const` because the UUID is an argument here; a real family writes
/// this as a constant, with the stream's own UUID set equal to its service's.
fn map(service: Uuid) -> GattMap {
    let chars: &'static [CharDecl] = Box::leak(Box::new([CharDecl {
        id: STREAM,
        uuid: service,
        dir: Dir::Stream,
        required: true,
    }]));
    let services: &'static [ServiceDecl] = Box::leak(Box::new([ServiceDecl {
        uuid: service,
        chars,
        required: true,
    }]));
    GattMap { services }
}

/// The device-info request, `0x0101` with an empty payload, built from the framing in
/// `docs/soundcore-motion-300.md` and nothing else.
///
/// Ten bytes: two of start-of-packet, two reserved zeroes, one direction byte (`0` for
/// host to device), the command little-endian, the length little-endian — `10`, the whole
/// frame including the checksum — no payload, and the checksum, which is the wrapping byte
/// sum of the first nine. That sum is `0x102`, so the checksum is `0x02`, whichever way
/// round the start-of-packet bytes go.
fn device_info_request(start: [u8; 2]) -> [u8; 10] {
    let mut frame = [0u8; 10];
    frame[0] = start[0];
    frame[1] = start[1];
    frame[5] = 0x01;
    frame[6] = 0x01;
    frame[7] = 0x0a;
    frame[9] = frame[..9].iter().copied().fold(0u8, u8::wrapping_add);
    frame
}

/// `bytes` as lower-case hex pairs, separated by spaces.
fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 3);
    for byte in bytes {
        let _ = write!(out, "{byte:02x} ");
    }
    out.truncate(out.trim_end().len());
    out
}
