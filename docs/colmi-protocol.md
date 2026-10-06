# Colmi R0x protocol — facts as observed (R10_F300, fw RT03CR_1.00.02_260319)

Sources: (1) the QRing 1.2.9 app's own packet log on this Mac (the `Documents/*.log` files in the app's container under `~/Library/Containers`), a full sync session of which is `fixtures/colmi-r10/qring-sync-2026-07-02.trace`; (2) a PacketLogger capture, not published, and its generated `fixtures/colmi-r10/thering-realtime-2025-11-19.trace` (workout HR stream + record fetch); (3) Gadgetbridge `devices/colmi/ColmiR0xConstants.java` + `ColmiR0xPacketHandler.java` + `service/devices/colmi/ColmiR0xDeviceSupport.java` (AGPL — used as documentation of the protocol, not copied); (4) private protocol notes (not published), reversed from the QRing APK, unpacked locally, for the sibling R09 — used as hints, each one checked against this ring before being stated as fact; (5) the QRing SQLite (see "Ground truth"), which decoded values are compared to; (6) sessions the stack recorded itself against the ring on 2026-09-06 (`fixtures/colmi-r10/junk-sync-2026-09-06.trace`, `junk-live-2026-09-06.trace`), lossless. The QRing log renders notifications via NSData description, which elides everything between the first 16 and last 8 bytes, so its `0xbc` replies were logged header-only; the trace's big-data replies were reconstructed from the app DB (the fixture header says how), each reproducing the CRC the log recorded, and that reconstruction is how the big-data layouts below were proven byte-exact. Anything marked **?** is unverified.

## GATT

| name | UUID | role |
|---|---|---|
| service V1 | `6e40fff0-b5a3-f393-e0a9-e50e24dcca9e` | NUS-like; 16-byte command frames |
| V1 write | `6e400002-…` | phone → ring, 16-byte frames |
| V1 notify | `6e400003-…` | ring → phone, 16-byte frames |
| service V2 | `de5bf728-d711-4e47-af26-65e3012a5dc7` | "big data" |
| V2 command | `de5bf72a-…` | phone → ring, `0xbc` requests |
| V2 notify | `de5bf729-…` | ring → phone, `0xbc` responses (reassembled) |
| service Device Information | `0000180a-0000-1000-8000-00805f9b34fb` | standard DIS; every characteristic below is read by the host |
| DIS system id | `00002a23-…` | read |
| DIS serial number | `00002a25-…` | read; empty on this ring |
| DIS firmware revision | `00002a26-…` | read: `RT03CR_1.00.02_260319` (trace channel `dis.fw`) |
| DIS hardware revision | `00002a27-…` | read: `RT03CR_V1.0` (trace channel `dis.hw`) |
| service WeChat | `0000fee7-…` | present, unused |
| WeChat `fea1` | `0000fea1-…` | notify; present, unused |
| WeChat `fea2` | `0000fea2-…` | write/indicate; present, unused |
| WeChat `fec9` | `0000fec9-…` | present, unused |

As `junk gatt` lists them on the R10: V1 notify is the only notify characteristic apart from the WeChat one, so everything the ring pushes comes there or on V2 notify. MTU negotiated 247 on macOS; the ring replies to `0x2f` with its packet size (`2f f4` → 244).

## 16-byte frame

`[cmd:u8][body:14][checksum:u8]`, checksum = (sum of first 15 bytes) & 0xff. Same in both directions. Dates are **BCD** (2026-07-02 → `26 07 02`); multi-byte counters are little-endian.

## Command catalogue (what this ring actually exchanged; counts from all logged sessions)

| cmd | name (Gadgetbridge) | direction seen | notes from the trace |
|---|---|---|---|
| 0x01 | SET_DATE_TIME | tx 40 / rx 39 | `01 YY MM DD hh mm ss 01`. The ack `01 01 00 00 02 00 00 00 00 01 00 20 00 00 30` is a **capability bitmap** (layout from the R09 decompilation, consistent with this ring): frame[1]=1 temperature, frame[2]=0 watch faces, frame[3]=0 menstruation, frame[4]=0x02 feature bits (bit1 SpO2), frame[5..8]=0 screen size, frame[9]=1 "new sleep protocol" (sleep via `bc 27`, not `0x44`), frame[11]=0x20 and frame[14]=0x30 undecoded. App logged "时间 设置失败" once when the ack was slow — treat first ack as best-effort |
| 0x03 | BATTERY | tx 355 / rx 351 | `03 01` → `03 <pct> <charging?>`; polled constantly by the app |
| 0x04 | PHONE_NAME | tx 33 / rx 32 | `04 01 12 …` then ring `04 00 …`. Gadgetbridge sends `04 02 0a 'G' 'B'` |
| 0x0a | PREFERENCES | tx/rx 68 | `0a 01` read, `0a 02 <payload>` write |
| 0x15 | SYNC_HEART_RATE | tx 94 / **rx 1543** | see "HR log" |
| 0x16 | AUTO_HR_PREF | 35 | `16 01 02` read → `16 01 01 05 05` (enabled, 5-min interval) |
| 0x19 | (version handshake) | 34 | `19 01 01 01` → `19 01 00 01`, the ack and nothing else. The version strings are **not** notified: right after the ack QRing reads the standard DIS (`180a`: `2a26` firmware `RT03CR_1.00.02_260319`, `2a27` hardware `RT03CR_V1.0`), and its log prints those reads the way it prints notifications, which is how they were first filed as raw ASCII replies on V1 notify (the sync fixture now carries them on `dis.fw`/`dis.hw`). The R09 decompilation labels 0x19 a units/°C toggle, so what the frame itself does is firmware-specific |
| 0x21 | GOALS | 34 | `21 01` → `21 01 88 13 00 e0 93 04 b8 0b` (5000 steps, 300000 (?), 3000 (?)) |
| 0x2c | AUTO_SPO2_PREF | 36 | `2c 01` → `2c 01 01 1e` (on, 30 min) |
| 0x2f | PACKET_SIZE | rx 39 | unsolicited after connect: `2f f4` |
| 0x36 | AUTO_STRESS_PREF | 35 | `36 01` → `36 01 01` |
| 0x37 | SYNC_STRESS | tx 47 / rx 179 | `37 <daysAgo>`; see "Stress and HRV series" |
| 0x38 | AUTO_HRV_PREF | 35 | `38 01 02` → `38 01 01` |
| 0x39 | SYNC_HRV | tx 44 / rx 164 | `39 <daysAgo>`; same layout as 0x37, see "Stress and HRV series" |
| 0x3a | ? | 34 | `3a 03 01` → `3a 03 01 01` |
| 0x3b | ? | 34 | `3b 01 01` → `3b 01 01 00 01` |
| 0x3c | ? | 39 | `3c 00` → `3c 00 ac 27` (0x27ac = 10156 — a size? a serial?) |
| 0x43 | SYNC_ACTIVITY | tx 142 / rx 503 | see "Activity" |
| 0x48 | (today's totals) | 26 | `48 00` → `48 <steps u24be> <runSteps u24be> <cal u24be> <dist_m u24be> <sportMin u16be>` (R09 decompilation layout, big-endian). Fixture reply `48 00 05 79 00 00 00 00 f7 af 00 04 34 00 29` at 14:16 = 1401 steps, 63407 cal, 1076 m, 41 min — consistent with the DB's hourly rows up to that hour (1134 steps and 846 m through 13:59, with the 14:00 hour in progress) |
| 0x50 | FIND_DEVICE | – | not seen |
| 0x69 / 0x6a | MANUAL_HEART_RATE start/stop | 2 / 2 | one-shot measurement, `69 01` … `6a` |
| 0x73 | NOTIFICATION (ring→phone) | rx 155 | sub-type byte: 0x01 new HR, 0x03 new SpO2, 0x04 new steps, 0x07 workout record stored (after `77 04`), 0x0c battery (`73 0c 5e` = 94 %), 0x12 live activity (`73 12 <steps u24be> <cal u24be> <dist_m u24be>`, e.g. 1413 steps, 63887 cal, 1084 m), 0x27 and 0x2c seen once each with a one-byte payload **?** |
| 0x77 / 0x78 | workout ctl / data ("phone sport" in the QRing APK) | (thering fixture) | **Not in Gadgetbridge.** A phone-initiated workout, not a generic live-HR channel: `77 <action> <sportType> 00` with action 1 start, 2 pause, 4 stop (3 = continue per the R09 decompilation, unseen); sportType 7 in the fixture. Ring acks start with `77 01 00 <ts u32le>` — the decompiled app reads payload[2..5] as a u32 timestamp; it is the workout's `startTime` (identical to tag 1 in the later `bc 42` record) from the ring's own clock (8 h behind local in the thering capture, which never sets the time; local-as-UTC once `0x01` has been sent). Pause/stop are acked with `77 00`; `73 07` (record stored) follows stop, and can even precede the stop's ack. Observed live: a 20 s workout is not stored at all, a 75 s one is. Data `78 <sportType> 01 00 <seq> <bpm>` about once a second; seq repeats every ~10 frames, dedupe on it. The stored record is then fetched via `bc 41`–`bc 45` (see "Big data"). `77 02 02 00` seen in another session = pause of sportType 2 **?** |
| 0xbc | BIG_DATA_V2 | tx 126 / rx 62 | see "Big data" |
| 0xff | FACTORY_RESET | – | never send |

## Session anatomy (what QRing does on connect — replicate this order in `junk sync`)

1. `04` phone name, `01` set time (retry once) → ring volunteers `2f` packet size and `01` ack
2. `3c 00`, `0a 01` read prefs, `0a 02` write prefs, `19 01 01 01` → ack, then GATT reads of the DIS firmware (`2a26`) and hardware (`2a27`) revision strings
3. `03 01` battery, `bc 30` file list query (empty body; ring answers body `00` = no files)
4. preference reads: `16`, `2c`, `36`, `38`, `21`, `3b`, `3a`
5. `43` activity: `43 <daysAgo> 0f <firstIdx> <lastIdx> 01` (15-min bucket indices, see "Activity"); the app reads day 29 first (`43 1d 0f 01 5f 01`, the oldest day it keeps) then days 0..6 with `00 5f`
6. `15` HR log per day: `15 <ts:u32le>` with ts = local midnight of that day as if UTC
7. `bc 2a` SpO2, `bc 27` sleep, `bc 25` temperature, `bc 41 <since u32le>` workout records — QRing sends the constant `4e 74 21 69` (2025-11-22 08:29:02, a pairing-time cursor? **?**) and the ring answers `bc 42` with count 0
8. `37` stress, `39` HRV per day
9. `03 01` battery polling every few seconds while connected

## HR log (0x15)

Request: `15 <ts u32le>` where ts = **the requested day's local midnight expressed as a UTC epoch** (verified: `00 aa 45 6a`→1782950400 = 2026-07-02 00:00 UTC — an earlier reading of this as `15 00 <ts>` mistook the timestamp's low byte for a sub-command; midnights whose epoch is a multiple of 256 happen to start with `00`, sent from a UTC-4 zone on 2026-07-02 for that day; the decompiled app subtracts the phone's UTC offset from every ring u32 before storing, i.e. the ring speaks local wall-clock as if it were UTC throughout). Replies (each a 16-byte frame, `cmd=0x15`):

| pkt[1] | layout |
|---|---|
| `0xff` | no data for that day |
| `0x00` | header: `15 00 <count> <interval_min> …` (`15 00 18 05` → 24 packets, 5-min samples) |
| `0x01` | `15 01 <ts u32le> <9 samples>` |
| `n ≥ 2` | `15 n <13 samples>` |

Sample time = day start + `(9 + (n−2)·13 + i)·interval` minutes; `0x00` = no sample. **Verified against the app DB:** packet 1's 9 samples and packet 2's 13 samples for 2026-07-02 equal `SchedualHeartRate` slots 0-21 byte-for-byte. The last packet index is `count − 1`. Note the app stores the day as 288 slots of 5 min; 24 packets × 13 − 4 = 288 ✓.

## Activity (0x43)

Request `43 <daysAgo> 0f <firstIdx> <lastIdx> 01`: `0f` = 15-minute buckets, indices 0..95 (`00 5f` = the whole day; the app clamps daysAgo ≤ 29). Replies: `43 ff …` = empty; `43 f0 05 01` = header, whose byte 3 = 1 means "new protocol: kcal field is ×10" (R09 decompilation — this is why the DB calorie column is raw × 10); rows:
`43 YY MM DD <bucket> <idx> <total> <kcal u16> <steps u16> <dist_m u16>` where `bucket` is the 15-min index. **Verified against the app DB (2026-07-02):** this ring only sends whole hours (`04`→01:00, `2c`→11:00 …), `steps` and `dist_m` match `step.count`/`step.distance` exactly, and `step.calorie` = raw × 10 (raw `0x0070`=112 ↔ DB 1120.0). The DB unit is the small calorie (28 steps ↔ 1120 cal = 1.12 kcal), so the raw field is in units of 10 cal; `0x48` and `73 12` report plain calories. Only hours with activity are sent (5 rows for that day).

## Stress and HRV series (0x37, 0x39)

Request `37 <daysAgo>` / `39 <daysAgo>` (only `00` = today seen). Replies use the 0x15 packetisation with a day offset instead of a timestamp:

| pkt[1] | layout |
|---|---|
| `0xff` | no data |
| `0x00` | header: `<cmd> 00 <count> <interval_min>` (`05 1e` → 5 packets, 30-min slots) |
| `0x01` | `<cmd> 01 <daysAgo> <12 samples>` |
| `n ≥ 2` | `<cmd> n <13 samples>` |

Samples are u8, slot i = midnight of `daysAgo` + i·interval; 12 + 4×13 = 64 ≥ 48 slots, the tail is zero. **Verified against the app DB (2026-07-02):** stress packet 1 = `SchedualPressure` slots 0–11 and HRV packets 1–2 = `SchedualHRV` slots 0–24, byte for byte. HRV is reported hourly with the intervening half-hour slot zero, and the app stores the zeros.

## Big data (0xbc)

Frame: `bc <kind> <len u16le> <crc u16le> <body[len]>`, delivered on V2 notify in MTU-sized pieces; reassemble until `6 + len` bytes. `crc` is **CRC-16/MODBUS** over `body` (poly 0x8005 reflected = 0xA001, init 0xFFFF, no final xor), little-endian; an empty body gives `ff ff`, the init value. Verified on every distinct frame in both fixtures, both directions.

Requests carry a short body: `bc 2a` with `ff` on the first sync of a session and `02` afterwards, `bc 27` with `06 01` then `01 01`, `bc 25` with `00`, `bc 30` with nothing, `bc 41` with `<since u32le>`. The R09 decompilation reads a one-byte body as `00` = today, `ff` = all history; the two-byte sleep body and the `02` are day counts? **?**

The QRing log elides reply bodies (first 16 and last 8 bytes survive). The layouts below were first pinned by matching those bytes to the app DB for 2026-07-02, then proven byte-exact: bodies rebuilt from the DB rows reproduce the logged CRC-16 of every reply, including the two SpO2 and two temperature variants that differ by the 14:30 sample. Those rebuilt frames are what the sync fixture now carries. The workout frames come from the PacketLogger fixture and are complete as captured.

**Sleep (kind 0x27) — verified.** Body: `<days:u8>` then per day `<daysAgo:u8> <len−2:u8> <start:u16le> <end:u16le> <(stage:u8, minutes:u8)…>`, start/end in minutes after midnight. Fixture reply (49 bytes): `01 00 2e bb 00 c5 02 | 02 18 … 03 1b 02 19 04 0f 02 31` = 1 day, today, 46 bytes, start 187 (03:07), end 709 (11:49), 21 pairs beginning (light, 24) and ending (deep, 27) (light, 25) (REM, 15) (light, 49) — exactly the `sleepV3` row (`effective_minutes` 522 = end − start). Stage codes: 2 light, 3 deep, 4 REM, 5 awake (REM is absent from the R09 decompilation but present here). Previous-evening rule: the app treats start ≥ 1080 (18:00) as the day before; equivalent to "start > end" for real nights.

**SpO2 (kind 0x2a) — verified.** 49-byte day blocks: `<daysAgo:u8> <24 × (min:u8, max:u8)>`, one pair per hour, 0 = no data. Fixture prefix `00 62 62 60 60 61 61 63 63 60` = today, hours 0–4 = 98, 96, 97, 99, 96 — the `BloodOxygen` rows at 00:00–04:00 exactly, with min = max on every row (the ring reports one value per hour). The app stores only hours with data (20 rows that day); the earlier "48 half-hours" reading of that table was wrong. (min, max) order is from the decompilation; indistinguishable here.

**Temperature (kind 0x25) — verified.** 50-byte day block: `<daysAgo:u8> <interval_min:u8 = 0x1e> <48 × u8>`, °C = (v + 200) / 10, 0 = no sample. Fixture prefix `00 1e a7 a8 a8 a8 a8 a8 a1 a8` = 36.7, 36.8, 36.8, 36.8, 36.8, 36.8, 36.1, 36.8 — the `Temperatures` rows 00:00–03:30 exactly.

**Workout records (kinds 0x41–0x45) — verified: layout from the QRing APK ("SportPlus"), decoded against the complete frames in the thering fixture.**

- `bc 41 <since u32le>`: summary query with a timestamp cursor (thering sends 0). The ring lists records **strictly newer** than the cursor: asking with the just-acked start returns nothing, one second earlier returns the record; QRing's constant cursor is its newest known start, so it gets only new ones. Reply `bc 42 <count:u8>` then `count` length-prefixed records: `<len:u8> <sportType:u8> <tlv…>`, each TLV `<len:u8 incl. itself> <tag:u8> <value LE>`. Tags: 1 startTime u32, 2 duration s, 3 distance, 4 calories, 5 speedAvg, 6 speedMax, 7 rateAvg, 8 rateMin, 9 rateMax, 10 elevation, 11 uphill, 12 downhill, 13 stepRate, 14 sportCount, 19 steps. Fixture record: len 48, sportType 7, startTime = the `77 01` ack timestamp, duration 61, rateAvg 60, rateMin 57, rateMax 62, the rest 0; the tags consume the 48 bytes exactly. Value widths seen: u32 for tags 1, 4, 19; u16 for 2, 3, 5, 6; u8 for 7, 8, 9, 13.
- `bc 43 <sportType:u8> <startTime u32le>`: detail request. Reply `bc 44`: `[0]` status (0 = ok), `[1]` packageCount, `[2]` ?, `[3]` sampleSecond, then from `[4]` repeated `<fieldLen:u8> <fieldTag:u8>` describing one sample record. Fixture: `00 01 00 01 01 11` = 1 package, 1 s sampling, one 1-byte field with tag 17 = realtime heart rate (the only tag the app decodes; 15 and 16 are referenced but not decoded **?**).
- `bc 45 <packageId:u8> <?:u8> <records…>`: detail packets, records from offset 2 laid out per the descriptor. Fixture: package 1, 54 one-byte HR samples for the 61 s session, spanning 57–62 = the summary's rateMin/rateMax.
- Target table: `ExerciseData(startTime, lastSeconds, exerciseType, steps, meters, calories, heartRates)`.

**File list (kind 0x30).** Empty request; reply body is a large payload the app parses as repeated length-prefixed strings (file names), with a leading count/status byte (the decompiled parser starts reading lengths at byte 1). Fixture reply body `00` = nothing stored. Sent right after the version strings.

Other big-data kinds known from the APK but unexercised here: `bc 28` manual HR history (`<daysAgo> {<minute u16le> <bpm>}`), `bc 3a` custom watch face (read `01`; write `02` then 8-byte elements `<type> <x u16le> <y u16le> <r> <g> <b>`), `bc 47` blood sugar (49-byte day blocks like SpO2). Small-channel `0x3a`, `0x3b`, `0x3c` are unrelated to these big-data kinds and remain undecoded.

## Ground truth for tests

`qifit_default.db` in the QRing app's container under `~/Library/Containers` (`Data/Documents/`) (copy it; it is live).
- `SchedualHeartRate(date, heartRates="v0,v1,…v287", secondInterval=300)`
- `step_day`, `step` (one row per active hour on this ring), `sleepV3(date,start,end,data_types,data_minutes)` — start/end in minutes after midnight, one row per night
- `BloodOxygen(TimeInterval, soa2, max_soa2, min_soa2)` — one row per hour that has data (20 for the fixture day), max = min
- `SchedualHRV(date, HRV)` / `SchedualPressure(date, prerssures)` — 48 comma-separated 30-min slots, `secondInterval=1800`; HRV has zeros in every other slot
- `Temperatures(date, temperature)` — one row per 30 min, °C
- `ExerciseData(startTime, lastSeconds, exerciseType, steps, meters, calories, heartRates)` — workouts (`bc 42`/`bc 45`); empty in this DB, the workout fixture came from a different app
- `ManualData(type=0)` = manual HR spot checks
The fixture day 2026-07-02 has rows in all of these except `ExerciseData`.
