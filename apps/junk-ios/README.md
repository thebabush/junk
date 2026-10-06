# junk-ios

An example iPhone app over `junk-ffi`: scan for a ring, sync it, run a workout on it. Three
screens over the three functions the Rust exports, and nothing else.

**Every byte of Bluetooth and of the ring's protocol happens in Rust.** `junk-ble` runs
btleplug's CoreBluetooth backend on the phone exactly as it runs on the desktop, so the app
runs the same driver, pump and link `junk` does (SPEC §4); what crosses into Swift is
`FoundRing`, `Progress`, `SyncResult` and `LiveResult` — numbers and strings. There is no
`CBCentralManager` here, no frame and no byte.

## Building

```sh
./bootstrap.sh
```

That is the whole of it: it builds `junk-ffi` for `aarch64-apple-ios` and
`aarch64-apple-ios-sim`, generates the Swift bindings from the library with uniffi,
assembles `Generated/JunkFFI.xcframework`, and runs `xcodegen`. Run it again after any
change to the Rust and the app picks the change up; it is safe to run repeatedly.
`--debug` builds the Rust unoptimised, `--release` (the default) with optimisations.
Everything it writes is gitignored: `Generated/` and `JunkIOS.xcodeproj`.

Then:

```sh
open JunkIOS.xcodeproj
```

and Run (Cmd-R). Headlessly, for the simulator, which needs no signing:

```sh
xcodebuild -project JunkIOS.xcodeproj -scheme JunkIOS \
    -sdk iphonesimulator -destination 'generic/platform=iOS Simulator' \
    -derivedDataPath build CODE_SIGNING_ALLOWED=NO build
```

## Running it

**On the simulator there is no Bluetooth.** A scan there fails, and the app says so in a
sentence rather than showing an empty list; nothing else can work either. The simulator is
good for the layout and for proving the whole thing links, and for nothing else.

**On a device**, a scan finds the ring, tapping it selects it, and Sync and Live talk to
the one selected — or, with none selected, to whichever single ring is in range.

- **Scan** lists what is nearby: name, id, RSSI. The id is the `device` argument.
- **Sync** sets the ring's clock from the phone, reads 1 to 7 days of logs and everything
  else the ring keeps, and shows the counts per sample kind, the firmware and hardware
  strings, the battery and what the ring says it can do. A request the ring *refuses* is
  not a failed sync: dialects differ, the session carries on, and the refusals are listed
  on their own.
- **Live** starts a workout (sport type 7, the one `junk live` starts), shows the heart
  rate as the ring streams it, and when the seconds are up shows the record the ring
  stored. **Stop** ends the *stream*, not the workout: the ring is still told to stop and
  the record is still fetched, so a stopped session returns a result like any other.

## Signing

There is no team id anywhere in this repo, and there is not going to be one.

A simulator build needs no signing at all — that is what `CODE_SIGNING_ALLOWED=NO` above
says out loud. A device build needs a team, and it goes in `apps/junk-ios/Local.xcconfig`,
which is gitignored:

```
DEVELOPMENT_TEAM = YOURTEAMID
```

Then run `./bootstrap.sh` again. xcodegen refuses to generate against an xcconfig that is
not there, so `project.yml` pulls the reference in — through `Signing.yml` — only when
`bootstrap.sh` has seen the file. Without it the project generates unsigned, which is all a
simulator build ever wants. Picking a team in Xcode's own Signing & Capabilities pane works
too, but the next `bootstrap.sh` regenerates the project over it.

## What is where

- `bootstrap.sh` — Rust, bindings, xcframework, project. One command, idempotent.
- `project.yml` — the xcodegen spec. `Signing.yml` is the optional half of it.
- `Sources/JunkIOS/` — the app: `RingModel` holds the state and makes the calls,
  `ScanView`, `SyncView` and `LiveView` show them.
- `Generated/` — everything `bootstrap.sh` writes: the bindings, the headers with their
  `module.modulemap`, `JunkFFI.xcframework` and `Info.plist`. Not tracked.

## Limits

btleplug's central manager does no background execution and no CoreBluetooth state
restoration, so a suspended app rescans rather than resuming (SPEC §4). Fine for an
example, wrong for a product; the way out is a Swift `Link` handing bytes to a driver
object, which the `Link` trait already allows and this API would sit on unchanged.

The simulator slice of `JunkFFI.xcframework` is `arm64` only, because those are the Rust
targets installed. An Intel Mac would need `x86_64-apple-ios` added to `bootstrap.sh` and
the `EXCLUDED_ARCHS` line dropped from `project.yml`.
