# Soundcore Motion 300 protocol notes

Sources so far:

- Soundcore Android APK: `com.oceanwing.soundcore.apk` downloaded with `apkeep` and unpacked locally (not published).
- Gadgetbridge `SoundcoreMotion300DeviceSupport` / `SoundcoreMotion300Protocol`.
- **A real exchange with the speaker, 2026-09-15** — see "Verified on hardware" below. Everything not marked as confirmed there is still Gadgetbridge-only and has never been on a wire here.

## Transport

Gadgetbridge treats Motion 300 as Bluetooth Classic/RFCOMM, not BLE GATT. **Confirmed.**

- Device name: `soundcore Motion 300` — confirmed
- Product label in APK resources: `A3135` / `Motion 300` — Gadgetbridge/APK only
- RFCOMM service UUID: `0cf12d31-fac3-4553-bd80-d6832e7b3135` — confirmed
- RFCOMM channel number: **11** — observed (SDP)
- Negotiated MTU: **668** — observed

## Packet frame

All multi-byte integers are little-endian. **Confirmed**, header included.

Host to speaker:

```text
08 ee 00 00 00 <cmd:u16> <len:u16> <payload...> <sum:u8>
```

Speaker to host:

```text
09 ff 00 00 01 <cmd:u16> <len:u16> <payload...> <sum:u8>
```

`len` is the full frame length, including the checksum. The checksum is the wrapping byte sum of all previous bytes. Gadgetbridge computes this on send; its decoder currently checks header/direction/length but does not validate the checksum. junk does validate it, and the speaker's own reply passes.

### The start-of-packet byte order

Gadgetbridge declares `START_OF_PACKET_HOST = (short)0xee08` and `START_OF_PACKET_DEVICE = (short)0xff09` and writes both through a **little-endian** `ByteBuffer`. Those constants are therefore not the wire order: the two bytes that leave the host are `08 ee`, and the ones that come back are `09 ff`. This document and `crates/junk-soundcore` had them reversed until the probe of 2026-09-15 read the constants as if they were wire order, sent the corrected form, and was answered.

## Motion 300 commands from Gadgetbridge

| Command | Direction/use | Meaning |
| --- | --- | --- |
| `0x0101` | request/reply | device info |
| `0x0301` | notify | battery info, one byte in fifths (`level * 20`) |
| `0x0401` | notify | charging info, `0` normal else charging |
| `0x0901` | notify | volume info |
| `0x2101` | notify | playback info |
| `0x7f01` | request/reply | LDAC mode |
| `0x8901` | request | power off |
| `0x9001` | set | voice prompts boolean |
| `0x8601` | set | auto power-off `[enabled, duration]` |
| `0x9310` | request/reply | button brightness |
| `0x9210` | set | button brightness byte |
| `0xff01` | set | LDAC mode boolean |
| `0x8c02` | request | current direction/orientation |
| `0x8a02` | set/reply | adaptive direction boolean; empty reply |
| `0x8902` | request/reply | equalizer state |
| `0x8b02` | set | equalizer preset byte |
| `0x8d02` | set | custom equalizer |
| `0x8e02` | notify | bass mode |

## Known payloads

### `0x0101` device info reply

Gadgetbridge expects exactly 29 payload bytes:

```text
volume:u8
battery_level:u8        # in fifths, percentage = value * 20
battery_charging:u8     # 0 normal, nonzero charging
currently_playing:u8
voice_prompts:u8
auto_power_off_enabled:u8
auto_power_off_duration:u8
firmware_ascii[5]
serial_ascii[17]
```

After this reply Gadgetbridge queries `0x7f01`, then `0x9310`, then `0x8902`.

### `0x8902` equalizer reply

Gadgetbridge expects 57 payload bytes:

```text
adaptive_direction:u8
current_direction:u8
equalizer_preset:u8
custom_eq_standing[18]
custom_eq_lying_left_or_other[18]
custom_eq_lying_right_or_other[18]
```

Each custom EQ block is nine pairs:

```text
value:u8, freq:u8
```

The on-wire value is offset by 60: app preference `0..120` maps to wire `60..180`, with 120 neutral on wire.

### `0x8d02` custom equalizer set

Payload length 21:

```text
direction_bitmask:u8    # 1 << selected direction
0x01
0xff
eq_block[18]           # nine value/frequency pairs
```

`crates/junk-soundcore` exposes this as `CustomEqualizerSet::to_payload()`.

### `0x8601` auto power-off set

Gadgetbridge preference values:

- `0`: disabled -> `[0x00, disabled_duration]`
- `1`: 10 min -> `[0x01, 0x00]`
- `2`: 20 min -> `[0x01, 0x01]`
- `3`: 30 min -> `[0x01, 0x02]`
- `4`: 60 min -> `[0x01, 0x03]`

## Verified on hardware

2026-09-15, a paired Soundcore Motion 300, powered on, over `cargo run -p junk-rfcomm --example probe`. The whole exchange is kept as `fixtures/soundcore-motion-300/probe-2026-09-15.trace` and replayed by `crates/junk-soundcore/tests/recorded.rs`, so nothing below depends on the speaker being in the room again.

Transport: Bluetooth Classic RFCOMM on service `0cf12d31-fac3-4553-bd80-d6832e7b3135`, **channel 11**, negotiated **MTU 668**, device name `soundcore Motion 300`.

Sent, ten bytes — command `0x0101` with no payload, a pure read:

```text
08 ee 00 00 00 01 01 0a 00 02
```

Received, 39 bytes, as **one** RFCOMM chunk, 47.2 ms later:

```text
09 ff 00 00 01 01 01 27 00 19 04 00 00 00 01 02 33 2e 30 2e 34 41 43 43 4c 58 58
30 30 30 30 30 30 30 30 30 30 78 60
```

Which decodes exactly as this document said it would, once the header was read the right way round: command `0x0101`, `len` = `0x0027` = 39 = the whole frame including the checksum, a 29-byte payload, checksum `0x60` (verified, not merely present).

| Field | Wire | Decoded |
| --- | --- | --- |
| volume | `19` | 25 |
| battery | `04` | 4 fifths = 80 % |
| charging | `00` | no |
| currently playing | `00` | no |
| voice prompts | `00` | off |
| auto power-off | `01` | on |
| auto power-off duration | `02` | index 2 (30 min by the table above) |
| firmware | `33 2e 30 2e 34` | `3.0.4` |
| serial | `41 … 78` | `ACCLXX0000000000x` |

What this confirms: the transport, the service UUID, the header byte order in both directions, the little-endian command and length fields, `len` counting the whole frame, the checksum rule, and the whole `0x0101` device-info payload layout including the battery's fifths.

What it does **not** confirm: every other row of the command table, the `0x8902` equalizer reply, the `0x8d02` custom-equalizer payload, the `0x8601` auto power-off values, and the mapping from duration index to minutes. Those are still Gadgetbridge-only.

The main-run-loop constraint was measured at the same time: from a plain `#[tokio::main]`, with nobody turning the main run loop, `connect` timed out after 25 s; with the main thread turning the main run loop and tokio on a second thread, it returned in 122 ms. See the `junk-rfcomm` crate docs and `junk_rfcomm::turn_main_loop`.

## junk implementation

`crates/junk-soundcore` is a device family in the shape SPEC §3.2–§3.4 describes:

- `gatt`: the one-service, one-`Dir::Stream` `GATT` map and the `rfcomm` channel name traces use.
- `wire`: `Packet::encode_into` / `Packet::decode`, the checksum, the command catalogue and the Gadgetbridge payload structs.
- `proto`: `SoundcoreDriver` (one transaction in flight, a queue, one timeout timer), `Req::DeviceInfo` → `Resp::DeviceInfo(DeviceInfo)`, `Req::Raw` for anything undecoded, and `Ev` for what the speaker says unasked.
- `SoundcoreFraming`: the `junk_core::Framing` rule that cuts the RFCOMM byte stream back into packets.

The driver deliberately has no request that changes the speaker. `0x8901` powers it off, and `0x8b02` / `0x8d02` / `0x9001` / `0x8601` / `0xff01` rewrite the owner's settings; the only way to send any of them is `Req::Raw`, whose docs say so.

Next steps are the rest of the typed payloads (equalizer first) and a shell that can drive the RFCOMM path end to end — see `junk-cli` in SPEC §4.
