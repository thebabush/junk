# junk

A pure-Rust protocol stack for wearables and Bluetooth gadgets. Each device family is a
sans-I/O `Driver`: a state machine that is fed bytes, timer ticks and requests and answers
with bytes to send, timers to set and typed results. It never touches a radio or a clock.
A `Link` carries the bytes (BLE GATT or Bluetooth Classic RFCOMM), `junk-pump` is the one
loop that joins the two, and the shells on top (a command line tool and an example iOS app)
stay thin. Because the drivers are pure, every protocol is tested by replaying recorded
sessions, with no hardware.

The reasoning and the layering are in [`SPEC.md`](SPEC.md), the design document. Protocol
facts, with what was verified and what was not, are in [`docs/`](docs/).

## Supported devices

| Device | Transport | Status |
|---|---|---|
| Colmi R10 smart ring | BLE GATT | Verified on hardware: scan, sync (heart rate, steps, SpO2, HRV, stress, temperature, sleep, workouts) and a live workout with heart rate, through `junk scan`, `junk sync` and `junk live`. The rest of the R0x family speaks related protocols and is expected to need small changes; it is unverified. |
| Soundcore Motion 300 speaker | Bluetooth Classic RFCOMM | Read-only driver (`junk-soundcore`), verified on one speaker. No CLI command. |
| Sony WH-1000XM4 headphones | Bluetooth Classic RFCOMM | Read-only status (link, init, battery, noise cancelling, equalizer, paired devices and more), verified on one unit, firmware 2.7.1. `junk sony status` works on macOS only. |

Other firmware versions and other devices of the same makers may behave differently.

## Quick start without hardware

Everything below runs from a clean checkout, with no device and no Bluetooth.

```sh
# Run the whole test suite: every driver is replayed against the recorded traces under fixtures/.
cargo test --workspace

# Replay a Colmi sync session through the driver. It checks that the driver writes exactly
# what the app wrote and prints the answers it decoded.
cargo run -q -p junk-cli -- replay fixtures/colmi-r10/qring-sync-2026-07-02.trace

# Replay a Sony WH-1000XM4 session and print the status report. This trace is synthetic.
cargo run -q -p junk-cli -- sony replay fixtures/sony-wh1000xm4/synthetic-init-status.trace
```

The first replay ends with `writes: 118 expected, 118 written, match` and `answers: 118, ok 118,
err 0`; the second prints a `Device` and a `Settings` block (model, firmware, battery, noise
cancelling, equalizer and so on). Add `--csv <dir>` to the Colmi replay to write the decoded
samples as CSV.

## With hardware

```sh
cargo run -q -p junk-cli -- scan                       # rings in range: name, id, RSSI
cargo run -q -p junk-cli -- sync --days 7 --csv out/   # sync a ring into CSV files under out/
cargo run -q -p junk-cli -- live                       # start a workout, print heart rate until Ctrl-C
cargo run -q -p junk-cli -- sony status --address <AA:BB:CC:DD:EE:FF>   # macOS only; headphones paired and on
```

Or `cargo install --path crates/junk-cli` and run `junk`. Answers go to stdout; what the
device says on its own and every failure go to stderr.

- `junk scan [--timeout <secs>]` lists the rings in range: name, id, RSSI.
- `junk sync [--device <name-or-id>] [--days <n>] [--csv <dir>] [--record <file>]` connects,
  runs the app's session (`docs/colmi-protocol.md`, "Session anatomy"), writes one CSV per
  sample kind under `--csv` (`hr`, `steps`, `spo2`, `hrv`, `stress`, `temperature`, `sleep`,
  `workouts`), merged with what is already there so that syncing twice writes the same
  files, and with `--record` keeps a lossless trace of the session in the `fixtures/` format.
- `junk live [--device <name-or-id>] [--sport <type>] [--seconds <n>] [--record <file>]`
  starts a workout on the ring, prints its heart rate as `<seconds since start> <bpm>` until
  the time is up or Ctrl-C, stops it and fetches the stored record and its detail.
- `junk gatt [--device <name-or-id>]` connects just long enough to list every characteristic
  the ring has and read its Device Information service.
- `junk replay <trace> [--tz <offset-minutes>] [--csv <dir>]` drives the driver from a
  recorded trace with no hardware, says whether it wrote what the app wrote, and writes the
  same CSVs `sync` does; exit status 1 on a mismatch or an error answer.
- `junk sony status --address <AA:BB:CC:DD:EE:FF> [--json] [--raw] [--record <file>] [--timeout <secs>]`
  connects to a paired, switched-on WH-1000XM4 over Bluetooth Classic, runs Sony's init, reads
  every setting it lists (read-only) and prints them; `--record` keeps the session as a trace.
  `junk sony replay <trace> [--json] [--raw]` prints the same report offline
  (`docs/sony-wh1000xm4.md`).

## Requirements

- **Rust 1.88 or newer** (the dependency tree needs it).
- **BLE** goes through [btleplug](https://github.com/deviceplug/btleplug) and works on macOS,
  Linux and Windows. On Linux it needs BlueZ at run time and `libdbus-1-dev` and `pkg-config` to build
  (`sudo apt-get install libdbus-1-dev pkg-config`).
- **macOS** is required for `junk-rfcomm` (Bluetooth Classic through IOBluetooth), for
  `junk sony status`, and for the iOS app. Elsewhere `junk-rfcomm` builds but connects to nothing.
- **iOS app** (`apps/junk-ios`): Xcode, [xcodegen](https://github.com/yonaskolb/XcodeGen) and the two iOS
  Rust targets (`rustup target add aarch64-apple-ios aarch64-apple-ios-sim`).
- **Python tools** (`tools/pklg2trace.py` and its tests) need [uv](https://docs.astral.sh/uv/):
  `uv run --with pytest pytest tools`.

The `no_std + alloc` crates are checked on a bare-metal target, which proves they need no operating system:

```sh
rustup target add thumbv7em-none-eabihf
cargo build -p junk-core -p junk-fake -p junk-trace -p junk-colmi -p junk-soundcore -p junk-sony --target thumbv7em-none-eabihf
```

Checks before sending a change are in [`CONTRIBUTING.md`](CONTRIBUTING.md).

## Crates

- `junk-core`: the contract: `Driver` and `Link` traits, `Input`/`Output`, channels, GATT maps, generic primitives (`no_std`).
- `junk-colmi`: the Colmi ring: wire frames, typed payloads, transactions, dialects, the measurement model (`no_std`).
- `junk-soundcore`: the Soundcore Motion 300 speaker driver (`no_std`).
- `junk-sony`: the Sony WH-1000XM4 driver: framing, ACK/resend link layer, init, read-only status (`no_std`).
- `junk-fake`: a toy device family that exists only to prove the core is generic (`no_std`).
- `junk-trace`: the trace format and the generic replay harness (`no_std`).
- `junk-pump`: the one loop joining a `Driver` and a `Link`; recording and replay links; stream framing (tokio).
- `junk-ble`: a `Link` over btleplug (BLE GATT).
- `junk-rfcomm`: a `Link` over Bluetooth Classic RFCOMM, macOS only; the one crate allowed `unsafe`.
- `junk-app`: what a shell asks of a ring: the sync and live scripts and the samples they collect.
- `junk-cli`: the `junk` command line tool.
- `junk-ffi`: uniffi bindings (scan, sync, live) for the iOS app.
- `apps/junk-ios`: an example SwiftUI app over `junk-ffi`.

## On the phone

`apps/junk-ios/` is an example SwiftUI app over `junk-ffi`: the same three things the CLI
does (scan, sync, live) on an iPhone, with the ring's heart rate arriving as it streams.
One command builds it, from the Rust up:

```sh
apps/junk-ios/bootstrap.sh   # cargo, uniffi, xcframework, xcodegen
open apps/junk-ios/JunkIOS.xcodeproj
```

Rust keeps the Bluetooth: btleplug's CoreBluetooth backend is the same code on the phone as
on the desktop, so the app runs the identical driver, pump and link, and Swift never sees a
frame, a channel or a byte. The simulator has no Bluetooth, so only a real device can find
a ring; the app says so rather than showing an empty list. See `apps/junk-ios/README.md`
for running it on a device and where the signing team goes.

## Fixtures and privacy

`fixtures/` holds the recorded sessions the replay tests run on; [`fixtures/README.md`](fixtures/README.md)
lists each file, where it came from and what was changed.

- Device identifiers in the traces are made up; Bluetooth addresses and serial numbers are not kept.
- The Sony trace is **synthetic**: it was generated by a test, never seen on a wire, and says so in its header.
- Raw captures (`.pklg`) are not published. The `.trace` files are the record.

## Disclaimer

This is an independent community interoperability project. It is not affiliated with,
endorsed by or sponsored by Colmi, Sony Group Corporation, Anker/Soundcore, or the makers
of the QRing, Sound Connect or Soundcore apps. Product and company names are trademarks of
their owners and are used only to identify compatible devices.

The protocol knowledge comes from observing the devices' own Bluetooth traffic, from public
community documentation, and from analysis of the vendors' apps for interoperability. No
vendor code or assets are included.

The software is provided as is, without warranty. It is not a medical device: heart rate,
SpO2, sleep, temperature and stress values are not for diagnosis or treatment. Sending raw
commands (`Req::Raw`) can change a device's settings or state, and doing so is at your own risk.

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md). Security reports: [`SECURITY.md`](SECURITY.md).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option. Unless you state otherwise, any contribution
you submit for inclusion in this work is dual-licensed as above, without any additional terms
or conditions.
