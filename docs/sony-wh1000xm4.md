# Sony WH-1000XM4 protocol notes

**One real WH-1000XM4 (firmware 2.7.1) has been read, read-only, with `junk sony status`.** That run
confirmed the link, the init sequence, the protocol version and the status decoders listed under
"Verified and not". Everything else below (other firmware versions, the v2 tables, every command that
writes to the headset) is read out of Sony's own app or out of community write-ups and has not been on a
wire here; "Verified and not" says exactly what that leaves.

Sources, in order of trust:

1. An analysis, for interoperability, of the Sony Sound Connect Android app 12.4.2 (`com.sony.songpal.mdr`). Sony's protocol
   library is `com.sony.songpal.tandemfamily` ("Tandem"; "MDR" is the headphone table). Package, enum and constant names are readable in the app, and every command id is
   bound to its payload class by a named enum, so ids, enum values and byte layouts are
   read, not guessed. Fields without a Sony name, and anything the headset reports at runtime, are
   not in the app and are unknown.
2. Three community write-ups (the Gadgetbridge Sony support, a desktop-client collection, an
   Android-app write-up). Where they disagree with the app, the app wins; "Details checked in the app"
   below lists the points that were checked.

## Transport

Bluetooth Classic RFCOMM, secure socket, channel found through SDP.

- v1 service UUID `96CC203E-5068-46AD-B32D-E316F5E069BA`; v2 service UUID
  `956C7B26-D49A-4BA8-B03F-B17D393CB6E2`. The app lists `[v2, v1]` and connects with the first the
  device advertises, and **picks its command table by that UUID**, not by the init reply (v1 UUID:
  the v1 tables; anything else: the v2 tables). The XM4 is expected to be a v1 device; that is
  inferred from the community's four-byte init reply and working v1 command ids. **Unknown** whether
  it also advertises the v2 UUID, in which case Sony's app would talk v2 to it.
- `junk-sony` declares the v1 UUID only, as one `Dir::Stream` channel (`rfcomm`), because
  `junk-rfcomm` resolves exactly one stream channel. `SonyFraming` cuts the stream into frames for
  `junk-pump`'s `Framed`.
- Sony's app writes a frame longer than 2048 bytes in 2048-byte pieces and reads 1024 bytes at a
  time; neither concerns a stream that is framed by its markers.

## Frame

```text
3E | type(1) | seq(1) | length(4, BE, payload bytes) | payload | checksum(1) | 3C
   \______________________ escaped ______________________________/
```

- Checksum: byte sum (mod 256) of type, seq, the four length bytes and the payload, **before**
  escaping.
- Escape, applied to everything between the markers, the checksum included: `3C` becomes `3D 2C`, `3D`
  becomes `3D 2D`, `3E` becomes `3D 2E`. Unescape: after `3D`, the next byte with `0x10` set.
- The reader skips to `3E` and takes bytes up to the first `3C`; the length field is deliberately not
  used to find the end of a frame.
- A frame that fails its checksum, length or escape check is dropped **without an ACK**, so the
  headset retransmits.
- Data types that matter on a v1 link: `0C` `DATA_MDR` (table one), `0E` `DATA_MDR_NO2` (table two:
  peripheral and voice guidance), `01` `ACK`. `1C`/`1E` are the same two tables, never acknowledged.
  Everything else asks for an ACK and is dropped by the app.
- Initial request: `3E 0C 00 00 00 00 02 00 00 0E 3C` (payload `00 00`, seq 0).

## Link layer

All from Sony's app (its link layer).

- The transmit sequence number starts at 0. The device ACKs a frame sent with seq `s` using seq
  `1 - s`. On an ACK whose seq differs from the current transmit seq the app adopts the ACK's seq and
  releases the waiter; an ACK whose seq equals it is "invalid ack, ignore" and the app keeps waiting.
- One command is in flight at a time. No ACK within **750 ms**: the identical frame, same seq, is
  written again, up to **10 resends** (11 transmissions); then the app closes the connection.
- Every received data frame whose type asks for it is ACKed at once with seq `1 - rxSeq`
  (a seq other than 0 or 1 gets none). A frame whose seq equals the last one received is a
  retransmission: ACKed, not processed again. Unknown command ids and unparseable payloads are ACKed and
  dropped.

## Init

Sony's init (the app's init sequence): each step sends one request and waits for its reply.
The app has no per-step timeout and gives up after 30 s in all. `junk-sony` runs it on `Connected`,
without being asked, in this order:

| step | request | reply |
|---|---|---|
| 1 | `00 00` | `01 <inq> <ver_hi> <ver_lo>`: protocol version, big-endian, bytes past `[3]` ignored |
| 2 | `02 00` | `03 <inq> <capabilityCounter> <len> <uniqueId>` |
| 3 | `04 01`, `04 02`, `04 03`, and `04 04` from version `0x5000` | `05 01 <len> <model>`, `05 02 <len> <fw>`, `05 03 <series> <color>`, `05 04 <n> <category x n>` |
| 4 | `06 00` | `07 <inq> <n> <FunctionType x n>`: **the capability model** |
| 5 | one capability GET per listed function (below) | `*_RET_CAPABILITY` |

Step 5 order is that of the app's init code (checked against the code, not only against the
app's table of capability GETs, which does not say its reading order): VPT, sound position, preset EQ, non-customizable
EQ, Extra Bass, noise cancelling, NC+ASM, ambient sound, NC optimizer, playback controller, connection
mode, upscaling, the six SYSTEM functions, training mode, general settings (in the order the function list
names them), BLE set-up, pairing management, voice guidance.

| FunctionType | request (type `0C` unless noted) | kept as |
|---|---|---|
| `41` VPT / `42` SOUND_POSITION | `40 01 <lang>` / `40 02 <lang>` | raw |
| `51` PRESET_EQ / `53` non-customizable | `50 01 <lang>` / `50 03 <lang>` | typed (`51`, else `53`) |
| `52` EBB | `50 02 <lang>` | typed |
| `61` / `62` / `63` NC / NCASM / ASM | `60 01` / `60 02` / `60 03` | typed (`62`, else `61`, else `63`) |
| `81` NC_OPTIMIZER, `A1` PLAYBACK_CONTROLLER | `80 01`, `A0 01` | raw |
| `E1` CONNECTION_MODE / `E2` UPSCALING | `E0 01` / `E0 02` | raw |
| `F1`..`F6` SYSTEM | `F0 01`..`F0 06` | `F4` typed (accepted auto-power-off ids), the rest raw |
| `D1`/`D2`/`D3` GENERAL_SETTINGn | `D0 D1\|D2\|D3 <lang>` | typed |
| `38` PAIRING_DEVICE_MANAGEMENT | type `0E`: `30 01` | typed (`PairingCapability`) |
| `39` VOICE_GUIDANCE | type `0E`: `40 01`, then `46 01 05` | `40 01` typed, `46 01 05` raw |

`<lang>` is an `MdrLanguage` byte; the driver sends `01` (English, `DEFAULT_LANGUAGE`) and
`SonyDriver::with_language` changes it. The headset localises the names it returns in it.

**Not asked**, although the app asks them: `14` BLE_SETUP (`1C 01`, `1C 00`), `30` FW_UPDATE (`36 07`..) and
`B1` TRAINING_MODE (`B0 01`). They describe BLE set-up, firmware update and training mode, none of which is a
setting this driver reads. **Never sent:** `C4 01 00` (the action log). It switches on a stream of large JSON
documents (`C9`) that the app forwards to its analytics and nothing else; the driver ACKs and ignores any `C9`
that arrives.

A step whose reply does not come within the reply timeout (3 s, from the ACK; Sony has no per-step number)
is recorded as no reply and the session goes on: reads are best-effort. If the protocol version is not
`0x1000`, `0x2000`, `0x3000`, `0x4000`, `0x4010`, `0x5000`, `0x6000`, `0x7000` or `0x7010`, init stops as the
app's does (`UNAVAILABLE_PROTOCOL_VERSION`): the version is reported in `Ev::ProtocolVersion`, and every
request is answered with `ProtoError::Unsupported`. The driver leaves the link open; closing it is the
shell's decision.

## Details checked in the app

Two details were checked in the app, because the community write-ups and the contract of the
driver disagreed on them:

- **The version whitelist is hexadecimal.** The version list is often written `1000, 2000, 3000, 4000, 4010, 5000, 6000,
  7000, 7010`. The app's whitelist is `0x1000, 0x2000, 0x3000,
  0x4000, 0x4010, 0x5000, 0x6000, 0x7000, 0x7010`; its constants are decimal in the code (4096, 8192, ..., 28688). Read as decimal, the community's XM3 reply
  `01 00 40 10` (`0x4010`) would be refused by the app it works with. The `04 04` gate ("version >= 5000")
  is `>= 0x5000` (20480 in the app's constants). The driver uses the hexadecimal values.
- **The order of step 5** is the order above; the app's table is two columns and does not say
  whether it reads down or across.

The serial number (`36 06`) is not part of the app's init (the app asks the UPDT group only when `30` is
listed, and then for other types); the app reads it, and when is not known. The driver asks it in
`Req::Status`, gated on function `30` being listed, which is a guess.

## Capability model

The headset describes itself; the app has no per-model feature table (its only XM4 string is an alert
special case). The `07` function list gates everything: a function that is not listed is never asked,
and its field in `Status` says `NotSupported`. Each listed function's capability reply carries what the
reads and any later writes depend on, and the driver keeps it:

- `NcAsmCapability`: the setting types and, **per ambient mode, `asmStep`**, the highest ambient level.
  Never assume 20: the app sends `1..asmStep` for ambient and takes `asmStep` from the `61` reply.
- `EqCapability`: band count, level steps (values run `0..levelSteps-1`), and the presets with their
  localised names. `EbbCapability`: the signed range of Extra Bass.
- The accepted auto-power-off ids (`F1 04`).
- `GsCapability` per general-setting slot: title, description, boolean or list, and the list's choices.
  Touch panel and multipoint are not fixed commands: they are slots whose `ENUM_NAME` title is
  `TOUCH_PANEL_SETTING` or `MULTIPOINT_SETTING` (or one of the other `GsTitleTitle` constants), and the
  driver decodes the title with `GsString::title()`. It is not known how an `ENUM_NAME` constant
  is spelled inside the string; the decoder assumes the constant's own name and falls back to
  `GsTitle::Unknown(text)`, losing nothing.
- The voice-guidance capability (table two, exact lengths validated).
- `PairingCapability` (`31 01 <max paired> <max connected> <00 file transfer possible | 01 impossible>`): how
  many devices the headset pairs with and keeps connected at once. A real XM4 sent `31 01 08 02 01`.

Every reply init consumed, decoded or not, is also in `Status::raw_replies` (data type and payload, command
id first), so a shell can dump it and a capture can become a fixture.

## What `Req::Status` reads

After init, in this order, each only if its function is listed (Sony's app):

| feature | function | request | reply | `Status` field |
|---|---|---|---|---|
| battery | `11` / `15` / `18` | `10 00` / `10 01` / `10 02` | `11 00 <lvl> <chg>`; `11 01 <L lvl> <L chg> <R lvl> <R chg>`; `11 02 <lvl> <chg>` | `battery`, `battery_left_right`, `battery_cradle` |
| codec | `13` | `18 00` | `19 <inq> <AudioCodec>` | `codec` |
| DSEE active | `12` | `14 00` | `15 <inq> <type> <status>` | `upscaling_indicator` |
| bud connection | `17` | `24 01` | `25 01 <L> <R>` | `connection_status` |
| NC / ambient | `62`, `61` or `63` | `66 <type>` | `67 02 <effect> <ncAsmSettingType> <ncValue> <asmSettingType> <asmId> <level>`; types `01`, `03` | `nc_asm` |
| equalizer | `51` or `53` | `56 <type>`, `5A <type>` | `57 <type> <preset> <n> <v x n>`; `5B <type> <n> {<kind> <u16>} x n` | `eq`, `eq_bands` |
| Extra Bass | `52` | `56 02` | `57 02 <level s8>` | `ebb` |
| DSEE | `E2` | `E6 02` | `E7 02 <00> <00 off\|01 auto>` | `dsee` |
| connection priority | `E1` | `E6 01` | `E7 01 <00> <00 sound\|01 connection>` | `connection_mode` |
| voice guidance | `39` (table two) | `46 01 01`, `46 01 02` | `47 01 01 <v>`, `47 01 02 <MdrLanguage>`, exact length | `voice_guidance`, `voice_guidance_language` |
| pairing mode | `38` (table two) | `32 01` | `33 01 <00 normal \| 01 inquiry scan> <CommonStatus>` | `pairing_mode` |
| paired devices | `38` (table two) | `36 01` | `37 01 <n> {<17 ASCII address> <order> <nameLen> <name>} x n [<playback order>]` | `paired_devices` |
| pause when taken off | `F3` | `F6 03` | `F7 03 00 <v>` | `pause_when_taken_off` |
| auto power off | `F4` | `F6 04` | `F7 04 01 <active id> <timer id>` | `auto_power_off` |
| Speak-to-Chat | `F5` | `F6 05`, `FA 05` | `F7 05 00 <v>`; `FB 05 00 <sens> <focus> <timeout>` | `speak_to_chat`, `speak_to_chat_config` |
| assignable settings | `F6` | `F6 06` | `F7 06 <n> <preset x n>` | `assignable_settings` |
| general settings | `D1`..`D3` | `D6 <slot>` | `D7 <slot> <GsSettingType> <value>` | `general_settings[i].value` |
| NC optimizer | `81` | `86 01` | `87 01 <personal type> <personal> <baro type> <pressure>` | `nc_optimizer` |
| serial | `30` (guess) | `36 06` | `37 06 <len> <ascii>` | `serial` |

Rules that decide the decoding:

- **The paired-device read is read-only.** The layouts of `32` and `36` come from the app and
  were then seen on one real WH-1000XM4 (firmware 2.7.1), which listed three devices. The commands of the
  same group that change the headset (`34` enter pairing mode, `3C` connect, disconnect or unpair a device)
  are never sent. In the list, the connection-order byte is `00` for a device that is paired and not
  connected and the n-th connection otherwise (`ConnectionOrder`). **The final byte is not an index into the
  list: it is the connection order of the device that holds the playback right** (`PlaybackHolder::Order`;
  Sony's app compares it with each device's order, and the real headset sent `01` with its one connected
  device at order 1). It may be absent, which reads as `Unknown`, not as an error; an order no listed device
  has names nobody. The 17 address bytes are kept as text whatever they are, and any length shortfall is a
  typed `Malformed`.

- `CommonStatus` is **`00` = ENABLE, `01` = DISABLE, `FF` = OUT_OF_RANGE**.
- `NcAsmEffect` is `00` off, `01` on, `10` adjustment in progress, `11` adjustment complete. `11` is not
  "on". `NcDualSingleValue` is `00` off, `01` single ("wind noise reduction"), `02` dual.
- The EQ byte after the preset id is a **count**. Clear Bass has no fixed index: it is the `5B` entry of
  kind `10` (`SPECIFIC_INFORMATION`) and value `0001`; `EqBands::clear_bass_index` finds it.
- Speak-to-Chat's reply is `F7 05 00 <v>` while a notification is `F9 05 <01|02> <v>` (`01` mode on/off,
  `02` preview mode on/off).
- The app ignores bytes past a layout, so the decoders do. A **short** payload is a typed
  `ProtoError::Malformed`, reported as `Ev::Unparsed` and recorded as `Reading::Malformed`. Voice guidance
  is the one exception: the app validates its exact lengths and so does the driver.
- Every enum keeps the byte it does not know (`Unknown(u8)`; `Other(u8)` where Sony has its own `OTHER`/
  `UNKNOWN` value), so a new value from a new firmware is read, not refused.

`Status` has one `Reading<T>` per feature rather than an `Option`:

| `Reading` | means |
|---|---|
| `NotSupported` | the headset did not list the function; nothing was asked |
| `NoReply` | asked (or not asked because the function list itself never came), no reply in time |
| `Malformed` | a reply came, was acknowledged, and could not be decoded |
| `Value(T)` | the decoded reply |

## Events

Anything the headset says that is not the reply a step is waiting for: battery (`13`), noise cancelling
(`69`), codec (`1B`), DSEE indicator (`17`), bud connection (`27`), equalizer and Extra Bass (`59`), DSEE
and connection priority (`E9`), pause/auto power off/Speak-to-Chat/assignable (`F9`), Speak-to-Chat config
(`FD`), general settings (`D9`), NC optimizer (`89`), voice guidance (`49`, table two), the pairing mode (`35`) and
paired-device list (`39`, both table two; another device connecting changes them) and alerts (`99`) are
decoded into `Ev::Report(Report)` by the same decoders as the replies. The driver does not answer an alert;
Sony's app answers `98 01 <type> <00|01>`, and for the XM4 answers alert `08` positively on its own.
Everything else is `Ev::Unparsed { bytes }`. A frame that fails to decode is `Ev::Dropped { error, bytes }`
and gets no ACK.

## `Req` and `Resp`

| `Req` | `Resp` |
|---|---|
| `Status` | `Status(Box<Status>)`: everything above |
| `Raw { data_type, payload }` | `Raw(Vec<u8>)`: the payload of the next frame of that data type, after the command was acknowledged |

`Req::Raw` is the one way to write to the headset, and it is unchecked: it sends what it is given. The SET
commands (`48`, `58`, `68`, `E8`, `F8`, `D8`, power off `22 00 01`) change what the owner chose and the
headset keeps it. A data type that is not acknowledged (`ACK`, the `SHOT_*` types) is refused, because the
link layer waits for an ACK that such a frame never gets. A notification of the same data type that arrives
first answers it.

## junk implementation

`crates/junk-sony`:

- `gatt`, `framing`, `wire`: the stream map, `SonyFraming`, and `Frame`/`DataType`/`FrameError` with
  escaping and checksum.
- `payload`: the byte enums, the decoders (`decode_report`) and `Report`.
- `proto`: `SonyDriver` (link layer, init, one step in flight, a queue of requests), `Req`/`Resp`/`Ev`, and
  `Status`/`Reading`. Two timers: `ACK_TIMER` (id 0, 750 ms, resends the identical frame) and `REPLY_TIMER`
  (id 1, 3 s, starts at the ACK). `Input::Disconnected` resets everything and fails every request with
  `ProtoError::Disconnected`; giving up after 10 resends fails the request in flight with
  `ProtoError::Timeout` and emits `Output::Disconnect`.

A reply that arrives before the ACK of its command is held until the ACK comes, so the next command
never goes out under a sequence number the headset has not released.

## The CLI

`junk sony status --address AA:BB:CC:DD:EE:FF` (macOS only, since it needs `IOBluetooth`) opens the
headphones' RFCOMM channel, runs init, sends one `Req::Status` and prints what the headset reports.
Nothing else is ever sent: the CLI has no way to reach `Req::Raw`.

- Find the address with `system_profiler SPBluetoothDataType` (the `Address:` line of the headphones)
  or in System Settings, Bluetooth, the (i) button beside them. There is no lookup by name.
- The headphones must be paired with the Mac and switched on, and should preferably not be connected to
  a phone or another app for control.
- Text by default, grouped as Device (model, firmware, series and colour, protocol version, unique id
  and capability counter, function list) and Settings (battery, codec, noise cancelling and ambient
  with the level and its steps, equalizer with band values and Clear Bass, DSEE, connection mode, voice
  guidance, pause when taken off, auto power off, Speak-to-Chat, assignable buttons, general settings
  as `title -> value`, NC optimizer, left/right connection, serial) and Paired devices (the capability as
  `up to 8 paired, 2 connected`, the pairing mode, then one row per device: name, address, `connected (n)` or
  `not connected`, and `<- playback` on the one that holds the playback right). A feature the headset does not
  list reads `not supported`, one that got no answer `no reply`, one that could not be decoded
  `malformed reply`. `--raw` appends the raw init and capability replies as hex lines (data type
  byte, then payload with the command id first). `--json` prints one JSON document of the same data,
  raw replies included, in serde's default shape (`{"Value": ...}` or `"NotSupported"`).
- The headset's own events (protocol version, notifications, frames dropped or not understood) go to
  stderr, one line each, while it runs; stdout is the report only.
- `--record <file>` writes the whole session as a junk trace v1 headed as a real capture of a
  WH-1000XM4: that is how a real session becomes a fixture to replace the synthetic one.
- `--timeout <secs>` (default 40: init is up to about twenty steps with a 3 s reply timeout each) is
  a hard limit on the whole session. Ctrl-C ends it early.
- Exit status 0 means the session ran and a status came back, even if some features read `no reply`.
  Non-zero, with a one-line message on stderr, means the link could not be opened (including on any
  platform but macOS), init failed (an unsupported protocol version is printed), the headset closed
  the connection, or the timeout or Ctrl-C ended the session. A trace is still written with `--record`.
- `junk sony replay <trace> [--json] [--raw]` runs the same session offline from a trace and prints
  the same report; the CLI's tests do this over the synthetic fixture.

`sony status` keeps the main thread turning the main run loop, which `IOBluetooth` needs, and runs the
session on a second thread (see `junk-rfcomm`).

The CLI has been run against one real WH-1000XM4 (firmware 2.7.1, 2026-10-06): see "Verified and not". `--json`,
Ctrl-C during a session and a non-macOS host are not exercised on hardware.

## Tests

- `crates/junk-sony/src/payload/tests.rs`: every decoder against the exact hex layouts of Sony's app,
  with unknown enum bytes, short payloads, ignored tails, and a deterministic sweep that nothing panics.
- `crates/junk-sony/tests/proto.rs`: the driver against a scripted fake headset: init order and bytes,
  `Status`, ACK adoption and the invalid ACK, resends and the give-up, duplicate rx seq, bad checksum,
  unsolicited frames mid-step, a reply before its ACK, a step with no reply, an unsupported version, reset
  on `Disconnected`, `Req::Raw`, unparseable frames, a sweep that nothing the headset could send panics and
  every request is answered once, and a session against 300 randomly mangled headsets.
- `crates/junk-sony/tests/recorded.rs`: the same session through `junk-pump`, a `Framed` and a `TraceLink`,
  over `fixtures/sony-wh1000xm4/synthetic-init-status.trace`, whole and one byte at a time.

**All of those payloads are synthetic.** They are written out from the layouts of Sony's app and made up to exercise the
driver; the function list, capabilities and values are not an XM4's. The trace under `fixtures/` is not a
capture: it is the fake headset's side of a session, generated by `tests/proto.rs`
(`JUNK_SONY_REGEN=1 cargo test -p junk-sony --test proto synthetic` rewrites it), and its header says so.
Replace it with a real capture as soon as there is one.

## Verified and not

**One session against a real WH-1000XM4 has been run** (firmware 2.7.1, `junk sony status`, 2026-10-06), and it
confirmed the following. Everything else below is still from the analysis of the app and the community write-ups.

- The link works as implemented: the RFCOMM channel opens through the **v1** service UUID, init completes, every
  step is acknowledged and answered, and the whole status read takes a few seconds.
- The init reply is `01 00 70 00`: **protocol version `0x7000`**, which is on the version whitelist.
- The function list (`07`) has 23 entries: AutoNcAsm, PairingDeviceManagementClassicBt, NoiseCancellingAndAmbientSoundMode,
  SmartTalkingMode, NcOptimizer, PresetEq, PlaybackController, ConnectionMode, Upscaling, GeneralSetting1 and 2,
  AssignableSettings, AutoPowerOff, ControlByWearing, VoiceGuidance, UpscalingIndicator, CodecIndicator, BatteryLevel,
  FwUpdate, ActionLogNotifier, BleSetup, ConciergeData and PowerOff. It does not list left/right battery or connection
  status, cradle battery, Volume, VPT or sound position.
- Ambient sound has **20 steps for both Normal and Voice** (`asmStep` 20); the noise-cancelling capability is
  `DualSingleOff` with `ncStep` 0. The EQ has 6 bands and 21 levels (0 to 20), with Clear Bass first.
- Battery, codec, DSEE, connection mode, wear detection, auto power off, Speak-to-Chat, voice guidance, the NC
  optimizer and the serial number all decoded, and the battery level agreed with the system's own reading.
- General settings: slot 1 is `TOUCH_PANEL_SETTING` (an `ENUM_NAME` title with an **empty description**, length 0,
  and one byte after the setting type), slot 2 is `MULTIPOINT_SETTING` (a `RAW_NAME` title, with a description).
  The decoder had required a description of at least one byte and rejected the touch panel until this run; it now
  accepts an empty description (the capture is a regression test).
- The pairing-management capability (`30 01` on table two) reads `31 01 08 02 01`: eight paired devices, two
  connected at once. It is decoded into `PairingCapability`.
- The pairing mode (`32 01` / `33 01 <mode> <status>`) and the paired-device list (`36 01` / `37 01 ...`) were
  read from the same headset when function `38` is listed: pairing mode `Normal` and enabled, three paired
  devices with this Mac connected (order 1), exactly as the layout says. The final byte of the list is the
  connection order of the device that holds playback, which the first decoder read wrongly as an index; the
  doc above and the decoder now say what the app does. Both are reads.

- Frame layout, escaping, checksum, ACK rule, resend rule: read from the app (high confidence),
  unit-tested here against the bytes the community write-ups give. The initial request
  and an NC set frame match the community's bytes.
- Command ids, enum values, reply layouts: read from the app, from named enums (high confidence).
  The decoders are tested against hex written out from those layouts, not against a device.
- The version whitelist and the `04 04` gate: checked in the app's init code; the community
  write-ups print them in decimal.
- The 3 s reply timeout, `DEFAULT_LANGUAGE`, the gating of `36 06` on function `30`, the reading of
  `ENUM_NAME` strings as constant names: this crate's own choices, named as such.

**Still unknown** (what the app leaves to the headset, plus this driver's):

- Whether this headset also advertises the v2 service UUID (the app would then prefer it). The v1 channel
  opened, and the driver speaks only the v1 tables, so this has not mattered.
- The meaning of the byte after `GsSettingType` in a general-setting capability (it was `00` on both slots).
- The assignable-settings key/action tree (`F1 06`, kept raw) and the auto-power-off ids from `F1 04`
  beyond the ones the status read reports.
- The meaning of the optimizer capability bytes, of the `NcSettingType`, `PersonalMeasureType`,
  `BarometricMeasureType`, `ModelColor` and `GuidanceCategory` values, and of the trailing Speak-to-Chat
  capability bytes: raw bytes in `Status`.
- Whether the headset ACKs before it answers, and how fast: the reply timeout assumes the ACK comes first.
- Whether a 750 ms ACK timeout is right for a headset on a loaded link.
