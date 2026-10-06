# junk — a typed, sans-I/O wearable protocol stack

Status: design spec, 2026-09-03, since updated as the code grew. The name is not final ("junk" = the stuff you strap to yourself).
Working crate prefix: `junk-*`. First target: Colmi R10 ring (the one on your finger).

## 1. Why this exists

Gadgetbridge has the protocol knowledge but couples parsing, protocol state, Android GATT
callbacks and database writes inside one class per device. Nothing reusable falls out.
The single-device Rust projects (OpenSCQ30, watchmate/infinitime) show the shape that
works: a pure protocol crate, a thin transport, thin platform shells.

junk is that shape, generalised just enough to host more than one device family without
pretending to be Gadgetbridge.

## 2. Goals / non-goals

Goals
- **Strong contract** between the three concerns: *wire* (bytes ↔ typed frames),
  *protocol* (state machine: requests, multi-packet transactions, timeouts),
  *I/O* (BLE, timers, clock). Each is a separate crate with a separate trait boundary.
- **Declarative** wherever a byte layout or a device description can be data instead of code:
  packet layouts via `binrw`, GATT maps and capability tables as `const` data, command
  catalogues as enums with `#[repr(u8)]`.
- **Strong types**: no `Vec<u8>` crossing a layer boundary except inside `Frame` newtypes;
  no `u8` command constants outside the wire crate; exhaustive `match` everywhere.
- **Multi-device in principle**: the core traits are device-agnostic; a device family is a
  crate that implements them. Colmi is the first. Nothing in core may mention Colmi.
- **Testable without hardware**: recorded traces replay through the protocol crate and must
  reproduce known-good decoded values (ground truth = the QRing app's own SQLite).
- **Portable**: `junk-core`, `junk-colmi`, `junk-soundcore`, `junk-sony` and `junk-trace` are `no_std + alloc`. Desktop CLI via
  `btleplug`; iOS via `uniffi` bindings around the same crates, over the same `btleplug`
  (its CoreBluetooth backend is shared between macOS and iOS).

Non-goals (for now)
- Notification forwarding, firmware update, watch-face anything.
- A GUI. The current shell is a CLI; experimental uniffi bindings are retained, but no app is included.
- Supporting devices the author does not own. "In principle" means the seams exist and
  are proven by real hardware where there is any: `junk-colmi` (a Colmi R10 ring, BLE GATT)
  and `junk-soundcore` (a Soundcore Motion 300 speaker, Bluetooth Classic RFCOMM) are two
  device families of two quite different shapes, over two transports, both checked on the
  author's own devices; `junk-sony` (a Sony WH-1000XM4, RFCOMM, read-only) is a third, checked
  on one unit. `junk-fake` still proves the seams in tests without any radio, but it is no
  longer the only thing holding the "multi-device in principle" claim up: real families over
  a second real transport is the stronger proof of the same claim, and it is
  what turned up the places where the core was quietly assuming GATT (`Dir::Stream`, and
  framing a byte stream above the link).

## 3. Layering and the contract

```
┌───────────────────────────────────────────────────────────────────┐
│ shells      junk-cli (tokio + btleplug)   junk-ffi (uniffi→Swift)  │
├───────────────────────────────────────────────────────────────────┤
│ pump        junk-pump: owns Driver + Link + timers; the only loop  │
├──────────────────────────────┬────────────────────────────────────┤
│ protocol    junk-colmi        │ junk-<other>   (impl core::Driver)  │
│ (no_std)    dialect select,   │                                    │
│             transactions      │                                    │
├──────────────────────────────┴────────────────────────────────────┤
│ wire        junk-colmi::wire  — binrw frames, command enums        │
│ (no_std)                                                           │
├───────────────────────────────────────────────────────────────────┤
│ core        junk-core: Driver/Link traits, Input/Output, Channel,  │
│ (no_std)    GattMap, Timestamp/Percent/Battery                     │
└───────────────────────────────────────────────────────────────────┘
```

### 3.1 `junk-core` — the contract

```rust
/// Opaque handle for "a characteristic the device family declared". The protocol layer
/// speaks in channels; only the Link knows UUIDs.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct Channel(pub u8);

pub struct GattMap { pub services: &'static [ServiceDecl] }
pub struct ServiceDecl { pub uuid: Uuid, pub chars: &'static [CharDecl], pub required: bool }
pub struct CharDecl { pub id: Channel, pub uuid: Uuid, pub dir: Dir, pub required: bool }
pub enum Dir { Write, WriteNoResponse, Notify, Indicate, Read, Stream }

/// What the world tells the driver. Nothing else exists.
pub enum Input<Req> {
    Connected { resolved: ChannelSet, mtu: u16 },   // which declared channels were found
    Disconnected,
    Rx { chan: Channel, bytes: Bytes },              // one notification, or the value of a Read, unmodified
    Tick { now: Instant },                          // clock only ever arrives this way
    Timer(TimerId),                                 // a timer the driver asked for fired
    Request { id: ReqId, req: Req, now: Instant },  // user intent
}

/// What the driver asks the world to do. Nothing else exists.
pub enum Output<Resp, Ev> {
    Tx { chan: Channel, bytes: Bytes },
    Subscribe(Channel),
    Read(Channel),                                   // read this channel's value; it comes back as Input::Rx { chan, bytes }
    SetTimer { id: TimerId, after: Duration },
    CancelTimer(TimerId),
    Event(Ev),                                       // unsolicited data (battery, live HR, …)
    Done { id: ReqId, result: Result<Resp, ProtoError> },
    Disconnect,
}

pub trait Driver {
    type Req; type Resp; type Ev;
    const GATT: &'static GattMap;
    fn handle(&mut self, input: Input<Self::Req>, out: &mut Outputs<Self::Resp, Self::Ev>);
}

/// The transport. Deliberately stupid.
pub trait Link {
    async fn connect(&mut self, gatt: &GattMap) -> Result<(ChannelSet, u16), LinkError>;
    async fn write(&mut self, chan: Channel, bytes: &[u8]) -> Result<(), LinkError>;
    async fn subscribe(&mut self, chan: Channel) -> Result<(), LinkError>;
    async fn read(&mut self, chan: Channel) -> Result<Bytes, LinkError>;   // the value of chan now, for a Dir::Read channel
    async fn next(&mut self) -> LinkEvent;   // Rx{chan,bytes} | Disconnected
}
```

Invariants (these are the spec; tests enforce them):

1. **Driver is pure.** `handle` is deterministic in its inputs. It never reads a clock, never
   sleeps, never allocates outside `alloc`, never touches I/O. `no_std` is the compiler-
   enforced version of this rule: if `junk-colmi` needs `std`, the design has leaked.
2. **Link is dumb.** Bytes and channels only. A Link that knows what `0x15` means is a bug.
3. **One loop.** `junk-pump` is the only place that calls `Driver::handle` and the only
   owner of the Link. It `select!`s over Link events, timers, and a request queue, feeds
   the driver one `Input` at a time, and applies every `Output` in order before the next
   input. No other task may talk to the Link.
4. **Every `Request` produces exactly one `Done`.** Timeouts are the driver's job: it emits
   `SetTimer`, gets `Timer`, emits `Done(Err(Timeout))`. Separately, the pump bounds transport
   operations and terminates a session on stalled I/O; it does not synthesize protocol replies.
   Shutdown interrupts active I/O, with a separate bounded disconnect attempt. Unfed queued
   requests fail at session end rather than carrying over to a reconnect.
5. **Malformed bytes never panic.** `Rx` of garbage yields `Event(Ev::Unparsed{..})` or
   `Done(Err(..))`. Malformed-input tests cover known cases; a fuzz harness is not yet implemented.
6. **No hidden state across connections.** `Disconnected` resets every transaction; the
   driver must be reusable for a reconnect without being rebuilt.

A serial-style transport (Bluetooth Classic RFCOMM, say) is not an exception to any of
this: it declares one `Dir::Stream` channel — the host writes and the device pushes on the
same endpoint, nothing to subscribe to and nothing to read — and `junk-pump`'s `Framed`
sits over its Link with the device family's framing rule, buffering the chunks the
transport delivers and handing the driver whole frames, so invariant 2 still holds and the
Link never learns what a packet is.

The measurement model lives in `junk-colmi::measure` for now, because the Colmi ring is
the only device that emits it; it moves to core once a second device does and the types
can be designed from two real examples. Core keeps only the generic primitives
(`Timestamp`, `Percent`, `Duration`, `Battery`).

```rust
pub struct HrSample { pub at: Timestamp, pub bpm: Bpm /* Valid(u8) | Invalid: the ring sends 0 for "no reading" */, pub source: Source /* Periodic | Manual | Live */ }
pub struct StepBucket { pub start: Timestamp, pub span: Duration, pub steps: u16, pub cal: u32 /* small calories */, pub distance_m: u16 }
pub struct SleepSession { pub start: Timestamp, pub end: Timestamp, pub stages: Vec<SleepStage> }
pub struct SleepStage { pub kind: SleepKind /* Light|Deep|Rem|Awake|Unknown(u8) */, pub minutes: u8 }
```

`Timestamp` is a local wall-clock minute with an explicit UTC offset; rings don't know
timezones, the app that set the time does. The driver is handed the offset in `Request`s
that need it and never computes "today" itself.

### 3.2 Wire layer — declarative frames

`binrw` for everything that is a fixed layout. The 16-byte Colmi frame:

```rust
#[binrw]
#[brw(little)]
pub struct Frame {
    pub cmd: Cmd,                                     // #[repr(u8)] enum, unknown → Cmd::Other(u8)
    pub body: [u8; 14],
    #[br(assert(checksum == sum8(cmd, &body), ProtoError::Checksum))]
    #[bw(calc = sum8(*cmd, body))]
    checksum: u8,
}

#[binrw] #[brw(magic = 0xbcu8, little)]
pub struct BigData {
    pub kind: BigDataKind,                            // 0x27 sleep, 0x2a spo2, 0x25 temp, 0x30 file list, 0x41–0x45 workout
    pub len: u16,
    pub crc: u16,                                     // CRC-16/MODBUS over body; verified after reassembly, not here
    #[br(count = len)] pub body: Vec<u8>,
}
```

Command-specific payloads are enums decoded *from* `Frame.body` by an exhaustive `match` on
`Cmd` (`HostFrame` for what the host writes, `RingFrame` for what the ring notifies; the
same for big-data bodies as `RequestBody`/`ReplyBody` keyed on `BigDataKind`), so adding a
command is adding a variant, and forgetting to handle it is a compile error in the decoder
and at the `match` in the protocol layer. Every decoder is lossless on the fixtures: bytes
the docs do not explain stay in the variant. The wire layer has zero knowledge of sequencing.

### 3.3 Protocol layer — transactions, not callbacks

Multi-packet exchanges are the whole difficulty of these devices. Each is a typed,
single-purpose state machine:

```rust
pub enum Txn {
    HrLog(HrLogTxn),          // 0x15: header(count, interval) → pkt1(ts + 9) → pktN(13 each)
    Activity(ActivityTxn),    // 0x43: 0xff empty | 0xf0 header | rows(idx/total)
    BigData(BigDataTxn),      // 0xbc: len-prefixed, reassembled across notifications, CRC
    Stress(SeriesTxn), Hrv(SeriesTxn), Simple(SimpleTxn /* one reply */)
}
impl Txn { fn feed(&mut self, frame: &Frame) -> Step }   // Step::{Continue, Done(Samples), Fail(ProtoError)}
```

Driver state = `{ dialect: Dialect, phase: Phase, active: Option<(ReqId, Txn, TimerId)>, queue: VecDeque<(ReqId, Req)> }`.
One transaction in flight at a time (that is how the rings behave); the queue serialises
user requests. Unsolicited frames (battery, live HR, "new data" notifications 0x73) are
dispatched to `Event` without touching the active transaction.

No pairing typestate is implemented: Colmi needs no pairing handshake. If a future device
requires one, consider an `Unpaired → Paired` state boundary based on that device's actual
protocol rather than adding generic pairing machinery now.

### 3.4 Dialects — "Colmi has a bunch of slightly different protos"

They do, and it is data, not code paths:

```rust
pub struct Dialect {
    pub transport: Transport,        // V1 (NUS-like 6e40fff0) | V2 (de5bf728) | Both
    pub big_data: bool,              // 0xbc available (sleep/spo2/… on V2 notify)
    pub live_hr: LiveHr,             // Manual69 (0x69/0x6a one-shot) | Workout77 (0x77/0x78 phone-sport stream + bc41–45 record, R10)
    pub hr_log: HrLogLayout,         // Header0 { first: 9, rest: 13 } — the only one seen so far
    pub sleep: SleepSource,          // BigData27 | Legacy
    pub fw_prefix: &'static [&'static str],   // e.g. ["RT03CR"] — matched against 0x19 reply
}
```

The ring also self-describes: the `0x01` set-time ack is a capability bitmap (temperature,
SpO2, "new sleep protocol", …; see docs). Selection at `Connected`: which channels resolved
(V1/V2/both) narrows the candidates, the `0x01` ack sets the capability flags (`big_data`,
`sleep`, temperature), and the firmware string from `0x19` picks the row for whatever the
bitmap does not cover. Unknown firmware → most conservative dialect +
`Event::UnknownFirmware`, never a refusal. Every capability flag is exercised
by at least one fixture so a new ring can be added by adding a row and a trace.

## 4. Crates

```
junk/
  Cargo.toml                 workspace, resolver 3; `unsafe_code = "forbid"` workspace-wide,
                             `junk-rfcomm` the one documented exception
  crates/
    junk-core/               traits, Input/Output, Channel/GattMap, Timestamp/Percent/Battery, ProtoError. no_std+alloc
    junk-colmi/              wire/ (binrw frames, Cmd, BigDataKind), proto/ (Driver impl, Txn, Dialect),
                             measure (HrSample, SleepSession, … until a second device emits them)
    junk-soundcore/          the Soundcore speakers (Motion 300 first): gatt (one Dir::Stream
                             channel), wire/ (packets, command catalogue), proto/ (Driver impl,
                             Txn), and SoundcoreFraming. no_std+alloc
    junk-sony/               the Sony WH-1000XM4: gatt (one Dir::Stream channel), wire/ (frames,
                             escaping, checksum, DataType), payload/ (byte enums with Sony's
                             names, typed decoders, Report), proto/ (Driver impl: ACK/resend link
                             layer, init, read-only Status; Req::Raw the only writer) and
                             SonyFraming. Reads only (`junk sony status`). no_std+alloc
    junk-fake/               a toy device family that exists only to prove core is generic
    junk-trace/              trace format reader/writer + generic replay harness (no_std)
    junk-pump/               the loop: generic over Driver + Link; tokio. Also RecordingLink
                             (a session → a trace), TraceLink (a trace → a session) and Framed
                             (a byte stream → whole frames, given the family's Framing)
    junk-ble/                Link impl over btleplug (macOS/Linux/Windows/iOS)
    junk-rfcomm/             Link impl over Bluetooth Classic RFCOMM, through IOBluetooth
                             (macOS only; the same API over nothing elsewhere). The one crate
                             allowed `unsafe`, and the one whose host must turn a run loop
    junk-app/                what a shell asks of a ring: the sync and live scripts, generic
                             over Link, the samples they collect, and the words they report
    junk-cli/                `junk scan | sync | live | replay <trace> | gatt | sony status | sony replay <trace>`
    junk-ffi/                uniffi: scan/sync/live as an app-level API; owns the pump and the Link
  fixtures/colmi-r10/        captured sessions: one QRing sync and one PacketLogger workout from
                             the apps, and one sync and one workout this stack recorded itself
                             — .trace files come from .pklg via tools/pklg2trace.py, or from
                             `junk sync --record` / `junk live --record`
  fixtures/soundcore-motion-300/  the RFCOMM probe that settled the packet header, captured by
                             `cargo run -p junk-rfcomm --example probe`
  fixtures/sony-wh1000xm4/   one synthetic session (never on a wire), generated by a test of
                             junk-sony; fixtures/README.md gives the provenance of every trace
  docs/colmi-protocol.md     protocol facts (observed + Gadgetbridge), kept honest about unknowns
  docs/soundcore-motion-300.md  the same for the speaker, marking what the hardware confirmed
  docs/sony-wh1000xm4.md     the same for the headphones: layouts from analysis of the vendor
                             app, and what one real unit confirmed
  tools/                     pklg2trace.py (PacketLogger .pklg to trace) and its tests; needs uv
```

Dependencies, pinned at spec time: binrw 0.15, btleplug 0.13, uniffi 0.32, tokio, uuid.
`junk-cli` adds clap, chrono, anyhow and serde_json; `junk-ffi` adds thiserror; `junk-rfcomm` adds the
objc2 family, target-gated to macOS. Only the shells and the transports are allowed
dependencies beyond those: `junk-core`, `junk-colmi`, `junk-soundcore`, `junk-sony` and `junk-trace` have
none but `binrw`, `serde`, `uuid` and each other.
`deku` was considered; `binrw`'s `#[bw(calc)]` / `#[br(import)]` fit these frames better.

### The transports

Two, and they are not the same shape:

- **BLE GATT** (`junk-ble`, over btleplug): a family declares several characteristics, and
  one notification is one frame. Nothing above the link has to reassemble anything.
- **Bluetooth Classic RFCOMM** (`junk-rfcomm`, over `IOBluetooth`): one bidirectional byte
  stream, declared as a single `Dir::Stream` channel whose UUID repeats its service's,
  because a stream has no characteristic. Bytes arrive in whatever chunks the radio hands
  over, so the family also supplies a `junk_core::Framing` and the pump runs over a
  `Framed` link. The link stays as ignorant of packets as the BLE one (invariant 2).

**A host of `junk-rfcomm` must turn the main run loop on its main thread.** `IOBluetooth`
schedules an RFCOMM channel's `NSStream`s on `[NSRunLoop mainRunLoop]` and sends the
delegate its open, data and close messages from blocks dispatched out of those stream
events, so a process whose main thread is running a tokio runtime never hears from the
channel. Measured against a Motion 300 on 2026-09-15: a plain `#[tokio::main]` timed out in
`connect` after 25 s; with the main thread turning the main run loop and tokio on a second
thread, `connect` returned in 122 ms. `junk_rfcomm::turn_main_loop` and
`run_main_loop_forever` are that loop, and `crates/junk-rfcomm/examples/probe.rs` is the
smallest whole example of the shape. This is a real constraint on the shells: `junk-cli`'s
`sony status` takes that shape (a plain `main` that turns the main run loop while a second
thread runs the session; every other command still runs on a multi-threaded tokio runtime),
and `junk-ffi`'s runtime-owning API needs restructuring before it can drive an RFCOMM device.

The FFI shape: **Rust keeps the Bluetooth**. `btleplug`'s CoreBluetooth backend is the same
code on macOS and iOS, and `junk-ble` cross-compiles to `aarch64-apple-ios` unchanged, so
the phone runs the identical driver, pump and Link the CLI runs — the portability claim is
then true by construction rather than by a second Link written in Swift. `junk-ffi` exposes
what an app actually asks for (scan, sync with progress, live with samples), not the driver;
Swift never sees a frame, a channel or a byte.

The cost is what btleplug's own central manager does not do: no background execution and no
CoreBluetooth state restoration, so a suspended app rescans instead of resuming. That is
acceptable for an experiment, not a background-sync product; a possible future path is a Swift `Link` handing
bytes to a `JunkDriver` object, which the `Link` trait already allows and this API would sit
on unchanged. The `thering` project, which does scan cadence, reconnect backoff and state
restoration well, is where that would go.

## 5. Smoke test — Colmi R10

Ground truth exists: the QRing app's SQLite (`qifit_default.db`, see docs) holds the
decoded values for the same days the trace covers. The smoke test is "decode the trace,
match the app".

Stage A — replay, no hardware
1. `junk replay fixtures/colmi-r10/qring-sync-2026-07-02.trace` feeds every `rx` line to
   the driver as `Input::Rx` and asserts every `tx` line the driver *would* emit for the
   same requests matches byte-for-byte (checksum, BCD time, request timestamps, MODBUS CRC).
2. Emitted samples must equal the app's rows for 2026-07-02: `SchedualHeartRate`
   (5-min HR), `step_day`, `SchedualPressure` and `SchedualHRV` (30-min series), `sleepV3`,
   `BloodOxygen` (hourly), `Temperatures` (half-hourly), battery 100 %.
3. The big-data replies in that trace (sleep, SpO2, temperature) are reconstructed: the
   QRing log elided their bodies, so they were rebuilt from the app DB rows for that day
   with the documented layouts, and each frame's CRC-16/MODBUS equals the CRC the log
   recorded for it (the fixture header has the details). Their part of A2 therefore holds
   by construction; the complete `bc 42`/`bc 45` frames in
   `fixtures/colmi-r10/thering-realtime-2025-11-19.trace` cover the workout flow.
4. Planned, not implemented: fuzz `Frame::read` and `Txn::feed` with `cargo-fuzz`; invariant 5.

Stage B — live, Mac CLI (passed 2026-09-06; see the status note under § 6)
1. `junk scan` finds `COLMI R10_F300` by the V1 service UUID (or as an already-connected
   peripheral offering it: macOS keeps a link to the ring, and a connected peripheral does
   not advertise).
2. `junk sync --days 7 --csv out/` runs the same request sequence QRing does (§ docs,
   "session anatomy"), writes HR/steps/sleep/SpO2/HRV/stress/temperature CSV.
3. Re-run immediately: identical output, no duplicate rows (idempotent sync keyed on
   timestamp).
4. `junk sync --record fixtures/colmi-r10/junk-sync-<date>.trace` writes a lossless trace
   of the session (the pump logs every `Rx`/`Tx`, nothing is elided). Promote it to a
   fixture and run the A2 comparison against the app DB for the same day: a genuinely
   captured big-data fixture, and days the current one lacks. PacketLogger is not needed.

Stage C — workout stream (passed 2026-09-06)
1. `junk live` drives the `Workout77` path from the thering fixture: `77 01 07 00` start →
   ring acks with its clock; `78 07 01 00 <seq> <bpm>` about once a second, seq repeats
   every ~10 frames so dedupe on seq; `77 02 07 00` pause, `77 04 07 00` stop → `73 07`
   (which may arrive before the stop's own ack); then `bc 41` with a cursor one second
   before the acked start (the ring lists records strictly newer than the cursor) and
   `bc 43` fetch the stored record and HR series (`bc 42`/`bc 44`/`bc 45`). A workout
   under about a minute is not stored: 20 s produced no record, 75 s did.

Exit criteria for "smoke test passed": A2 exact match on HR, steps, stress, HRV, sleep
(start/end and stage sequence), SpO2 and temperature; B3 idempotent; B4 a recorded session
matches the app DB the same way.

## 6. Milestones

| # | Deliverable | Proves |
|---|---|---|
| 0 | workspace + `junk-core` traits + `junk-fake` driver + pump running against a fake Link | the contract compiles and is generic |
| 1 | `junk-colmi::wire`: Frame, Cmd, BigData; round-trip tests from the trace | declarative wire layer |
| 2 | `junk-trace` + replay harness; Stage A1 | sans-I/O replay works |
| 3 | Txns: HrLog, Activity, BigData(sleep, spo2), Simple(battery, time, fw); Stage A2 | protocol layer complete for R10 |
| 4 | `junk-ble` + `junk-cli sync --record`; Stage B incl. the recorded big-data fixture | real transport, real ring |
| 5 | `Workout77` stream + record fetch; Stage C | dialect mechanism carries a real variant |

Milestones 0–5 are complete. An iOS example previously exercised the `junk-ffi` build and
simulator path, but never a ring on a real iPhone. That example has been removed; the
experimental bindings remain, not a supported app. The experiment led to radio-state errors
in `junk-ble`, displayable errors in `junk-ffi`, and graceful sync cancellation shared by
the CLI and FFI.

Milestones 0–5, 2026-09-06: Stage A passes on the two captured fixtures
and on the two the pump recorded itself (`fixtures/colmi-r10/junk-sync-2026-09-06.trace`,
seven days, every answer Ok; `junk-live-2026-09-06.trace`, a 75 s workout with its record).
Stage B: scan, a seven-day sync with CSVs, an idempotent re-run, and the recorded fixture;
Stage C: the live stream, the stored record and its 65 heart rates. The live run found
one protocol error in the docs (the version strings are Device Information reads, not V1
notifications) and made the stack grow a `Read` primitive; nothing else needed changing.

The former iOS experiment is not a current milestone; real-device iOS support remains unverified.

## 7. Open questions

- **Name.** junk / junk-rs / something else; the name is not final. Crate prefix follows.
- **Unknown commands** still open after the 2026-09-03 pass: small-channel `0x3a`, `0x3b`,
  `0x3c` (not in the APK pass either), the constant `bc 41` cursor QRing sends, and
  workout detail field tags 15/16. Leave as `Cmd::Other` / `BigDataKind::Other` with
  fixtures proving they pass through harmlessly.

Resolved (details in `docs/colmi-protocol.md`):
- **Timestamp policy.** Ring u32 timestamps are local wall-clock expressed as if UTC; the
  QRing app (by analysis) subtracts the local offset before storing. `Timestamp` freezes on
  "local minute + explicit offset supplied by the request".
- **Big-data CRC** is CRC-16/MODBUS over the body, `ff ff` for an empty body. Verified on
  every frame in both fixtures; the type says so.
- `bc 25` is temperature (half-hourly, °C = (v + 200) / 10), checked against the app DB.
- `bc 41`–`bc 45` are the workout ("SportPlus") record list / detail flow: TLV summary
  tags and the `bc 44` sample descriptor come from the APK and decode the workout fixture
  exactly. `bc 30` is the file-list query.
- `0x77`/`0x78` is the small-channel phone-sport control and live stream; the archive is
  the `bc 41`–`45` flow above.
- **Pump on iOS.** Rust, with `btleplug` under it: its CoreBluetooth backend is shared with
  macOS and `junk-ble` cross-compiles to `aarch64-apple-ios` as it stands, so no pump and no
  `Link` is written twice. A Swift `Link` remains the answer if background sync is ever
  wanted (see § 4).
