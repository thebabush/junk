//! What the ring notifies on V1 notify, one variant per command shape.

use junk_core::Percent;

use crate::wire::payload::{
    Body, DecodeError, Prefs, WorkoutAction, bcd, bcd_to_u8, bcd_year, frame, is_on, on_byte,
    u24_be, u24_le, u24_to_be, u24_to_le, year_of_bcd,
};
use crate::wire::{Cmd, Frame, WireError};

/// The first byte of a log packet that says the day has no data.
const NO_DATA: u8 = 0xff;
/// The first byte of an activity reply that is the header rather than a row.
const ACTIVITY_HEADER: u8 = 0xf0;
/// The feature bit of the capability bitmap that means `SpO2`.
const FEATURE_SPO2: u8 = 0x02;

/// A frame the ring notifies, as a typed value.
///
/// [`RingFrame::decode`] reads one from a [`Frame`] and is total for well-formed input;
/// [`RingFrame::encode`] builds the frame back, for the peer simulator and replay tooling.
/// Every variant's doc gives its body; unspecified trailing bytes are zero.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub enum RingFrame {
    /// `0x01`: the set-time ack, which is the ring's capability bitmap.
    SetTimeAck(Capabilities),
    /// `0x03`: the battery. Body `<percent> <charging>` (`64 00` = 100 %, not charging).
    Battery {
        /// Charge level.
        percent: Percent,
        /// `01` when on the charger.
        charging: bool,
    },
    /// `0x04`: the phone-name ack. Body `00`.
    PhoneNameAck,
    /// `0x0a`: the reply to a profile read. Body `01` then the nine [`Prefs`] bytes.
    Prefs(Prefs),
    /// `0x0a`: the profile-write ack. Body `02`.
    PrefsWriteAck,
    /// `0x15`: one packet of a day's HR log.
    HrLog(HrLogPacket),
    /// `0x16`: the automatic HR preference. Body `01 <enabled> <interval_min> <unknown>`
    /// (`01 01 05 05` = on, every 5 minutes).
    AutoHrPref {
        /// `01` when the ring measures on its own.
        enabled: bool,
        /// Minutes between measurements.
        interval_min: u8,
        /// The fourth byte, `05` in the fixture like the interval; not understood, kept
        /// so the frame re-encodes byte for byte.
        unknown: u8,
    },
    /// `0x19`: the version ack. Body `01 00 01`; the firmware and hardware strings follow
    /// as raw ASCII on the same channel, handled by [`V1Rx`].
    ///
    /// [`V1Rx`]: crate::wire::V1Rx
    VersionAck,
    /// `0x21`: the daily goals. Body `01 <steps u24> <calories u24> <distance u24>
    /// <sport_min u16> <sleep_min u16>` (`01 88 13 00 e0 93 04 b8 0b 00 00 00 00` = 5000
    /// steps, 300000, 3000, 0, 0).
    Goals {
        /// Steps per day.
        steps: u32,
        /// Calories per day, in whatever unit the app uses (300000 in the fixture).
        calories: u32,
        /// Distance per day (3000 in the fixture; metres, presumably).
        distance: u32,
        /// Minutes of sport per day.
        sport_min: u16,
        /// Minutes of sleep per day.
        sleep_min: u16,
    },
    /// `0x2c`: the automatic `SpO2` preference. Body `01 <enabled> <interval_min>`
    /// (`01 01 1e` = on, every 30 minutes).
    AutoSpo2Pref {
        /// `01` when the ring measures on its own.
        enabled: bool,
        /// Minutes between measurements.
        interval_min: u8,
    },
    /// `0x2f`: the ring's packet size, volunteered after connect. Body `<size>` (`f4` =
    /// 244).
    PacketSize(u8),
    /// `0x36`: the automatic stress preference. Body `01 <enabled>`.
    AutoStressPref {
        /// `01` when the ring measures on its own.
        enabled: bool,
    },
    /// `0x37` or `0x39`: one packet of a day's stress or HRV series.
    Series {
        /// Which series.
        cmd: SeriesCmd,
        /// The packet.
        packet: SeriesPacket,
    },
    /// `0x38`: the automatic HRV preference. Body `01 <enabled>`.
    AutoHrvPref {
        /// `01` when the ring measures on its own.
        enabled: bool,
    },
    /// `0x43`: one packet of a day's activity.
    Activity(ActivityPacket),
    /// `0x48`: today's running totals. Body `<steps u24 be> <running_steps u24 be>
    /// <cal u24 be> <distance_m u24 be> <active_min u16 be>` (R09 decompilation;
    /// `00 05 79 00 00 00 00 f7 af 00 04 34 00 29` = 1401 steps, 0, 63407 cal, 1076 m,
    /// 41 min, which matches the app's rows for that hour).
    TodayTotals {
        /// Steps so far today.
        steps: u32,
        /// Of which running.
        running_steps: u32,
        /// Calories so far today (small calories: the fixture's 63407 is 63 kcal).
        cal: u32,
        /// Metres so far today.
        distance_m: u32,
        /// Active minutes so far today.
        active_min: u16,
    },
    /// `0x73`: an unsolicited notification.
    Notify(Notification),
    /// `0x77`: the workout-control ack. Body `<action> 00 <start u32>`: a start is acked
    /// with `01 00 <ts>` where `ts` is the workout's start on the ring's own clock
    /// (`01 00 80 29 1d 69` → `0x691d2980`); pause and stop with `00`.
    WorkoutCtlAck {
        /// The action acked; [`WorkoutAction::Other`]`(0)` for pause and stop.
        action: WorkoutAction,
        /// The start timestamp, present when the action is [`WorkoutAction::Start`].
        start: Option<u32>,
    },
    /// `0x78`: one sample of the live workout stream, about once a second. Body
    /// `<sport_type> <flag> 00 <seq> <bpm>` (`07 01 00 0a 3e` = sport 7, flag 1, seq 10,
    /// 62 bpm). The sequence number repeats every ten frames or so; dedupe on it.
    WorkoutData {
        /// The sport being recorded.
        sport_type: u8,
        /// The second byte, `01` throughout the fixture.
        flag: u8,
        /// The sample's sequence number.
        seq: u8,
        /// Heart rate; `0` before the first reading.
        bpm: u8,
    },
    /// Any frame with no variant: every [`Cmd::Other`], and a host-only command seen
    /// coming from the ring (`0x50`, `0x69`, `0x6a`, `0xff`).
    Raw {
        /// The command byte.
        cmd: Cmd,
        /// The body, undecoded.
        body: [u8; Frame::BODY_LEN],
    },
}

impl RingFrame {
    /// The typed value of `frame`, read from its command byte and body.
    ///
    /// Total for a [`Cmd::Other`] command and for the host-only commands, which come back
    /// as [`RingFrame::Raw`].
    ///
    /// # Errors
    ///
    /// [`DecodeError::Malformed`] when a named command's body does not fit its layout: a
    /// sub-command byte that is not the documented one, a battery percentage above 100,
    /// an activity row whose date is not BCD.
    pub fn decode(frame: &Frame) -> Result<RingFrame, DecodeError> {
        let body = &frame.body;
        let malformed = |what| DecodeError::Malformed {
            cmd: frame.cmd,
            what,
        };
        match frame.cmd {
            Cmd::SetTime => Ok(RingFrame::SetTimeAck(Capabilities::from_body(body))),
            Cmd::Battery => Percent::new(body[0])
                .map(|percent| RingFrame::Battery {
                    percent,
                    charging: is_on(body[1]),
                })
                .ok_or(malformed("battery percent")),
            Cmd::PhoneName if body[0] == 0x00 => Ok(RingFrame::PhoneNameAck),
            Cmd::Prefs if body[0] == 0x01 => Ok(RingFrame::Prefs(Prefs::from_body(body))),
            Cmd::Prefs if body[0] == 0x02 => Ok(RingFrame::PrefsWriteAck),
            Cmd::HrLog => Ok(RingFrame::HrLog(HrLogPacket::from_body(body))),
            Cmd::AutoHrPref if body[0] == 0x01 => Ok(RingFrame::AutoHrPref {
                enabled: is_on(body[1]),
                interval_min: body[2],
                unknown: body[3],
            }),
            Cmd::Version if body[0] == 0x01 => Ok(RingFrame::VersionAck),
            Cmd::Goals if body[0] == 0x01 => Ok(goals(body)),
            Cmd::AutoSpo2Pref if body[0] == 0x01 => Ok(RingFrame::AutoSpo2Pref {
                enabled: is_on(body[1]),
                interval_min: body[2],
            }),
            Cmd::PacketSize => Ok(RingFrame::PacketSize(body[0])),
            Cmd::AutoStressPref if body[0] == 0x01 => Ok(RingFrame::AutoStressPref {
                enabled: is_on(body[1]),
            }),
            Cmd::StressLog => Ok(RingFrame::Series {
                cmd: SeriesCmd::Stress,
                packet: SeriesPacket::from_body(body),
            }),
            Cmd::AutoHrvPref if body[0] == 0x01 => Ok(RingFrame::AutoHrvPref {
                enabled: is_on(body[1]),
            }),
            Cmd::HrvLog => Ok(RingFrame::Series {
                cmd: SeriesCmd::Hrv,
                packet: SeriesPacket::from_body(body),
            }),
            Cmd::Activity => ActivityPacket::from_body(body)
                .map(RingFrame::Activity)
                .ok_or(malformed("bcd date")),
            Cmd::TodayTotals => Ok(today_totals(body)),
            Cmd::Notify => Notification::from_body(body)
                .map(RingFrame::Notify)
                .ok_or(malformed("battery percent")),
            Cmd::WorkoutCtl => {
                let [action, _, t0, t1, t2, t3, ..] = *body;
                Ok(RingFrame::WorkoutCtlAck {
                    action: WorkoutAction::from_byte(action),
                    start: (action == WorkoutAction::Start.byte())
                        .then(|| u32::from_le_bytes([t0, t1, t2, t3])),
                })
            }
            Cmd::WorkoutData => Ok(RingFrame::WorkoutData {
                sport_type: body[0],
                flag: body[1],
                seq: body[3],
                bpm: body[4],
            }),
            Cmd::PhoneName
            | Cmd::Prefs
            | Cmd::AutoHrPref
            | Cmd::Version
            | Cmd::Goals
            | Cmd::AutoSpo2Pref
            | Cmd::AutoStressPref
            | Cmd::AutoHrvPref => Err(malformed("sub-command")),
            Cmd::FindDevice
            | Cmd::ManualHrStart
            | Cmd::ManualHrStop
            | Cmd::FactoryReset
            | Cmd::Other(_) => Ok(RingFrame::Raw {
                cmd: frame.cmd,
                body: *body,
            }),
        }
    }

    /// The frame for this value.
    ///
    /// # Errors
    ///
    /// [`WireError::Value`] for a field the layout cannot hold: a number above what three
    /// bytes carry in [`RingFrame::Goals`], [`RingFrame::TodayTotals`] or
    /// [`Notification::LiveActivity`], an [`ActivityPacket::Row`] date that is not two BCD
    /// digits per field, a `More` packet index below 2 or of `0xff`.
    pub fn encode(&self) -> Result<Frame, WireError> {
        match self {
            RingFrame::SetTimeAck(caps) => Ok(Frame {
                cmd: Cmd::SetTime,
                body: caps.to_body(),
            }),
            RingFrame::Battery { percent, charging } => {
                frame(Cmd::Battery, &[&[percent.get(), on_byte(*charging)]])
            }
            RingFrame::PhoneNameAck => frame(Cmd::PhoneName, &[&[0x00]]),
            RingFrame::Prefs(prefs) => frame(Cmd::Prefs, &[&[0x01], &prefs.to_bytes()]),
            RingFrame::PrefsWriteAck => frame(Cmd::Prefs, &[&[0x02]]),
            RingFrame::HrLog(packet) => packet.encode(),
            RingFrame::AutoHrPref {
                enabled,
                interval_min,
                unknown,
            } => frame(
                Cmd::AutoHrPref,
                &[&[0x01, on_byte(*enabled), *interval_min, *unknown]],
            ),
            RingFrame::VersionAck => frame(Cmd::Version, &[&[0x01, 0x00, 0x01]]),
            RingFrame::Goals {
                steps,
                calories,
                distance,
                sport_min,
                sleep_min,
            } => frame(
                Cmd::Goals,
                &[
                    &[0x01],
                    &u24_to_le(*steps, "steps")?,
                    &u24_to_le(*calories, "calories")?,
                    &u24_to_le(*distance, "distance")?,
                    &sport_min.to_le_bytes(),
                    &sleep_min.to_le_bytes(),
                ],
            ),
            RingFrame::AutoSpo2Pref {
                enabled,
                interval_min,
            } => frame(
                Cmd::AutoSpo2Pref,
                &[&[0x01, on_byte(*enabled), *interval_min]],
            ),
            RingFrame::PacketSize(size) => frame(Cmd::PacketSize, &[&[*size]]),
            RingFrame::AutoStressPref { enabled } => {
                frame(Cmd::AutoStressPref, &[&[0x01, on_byte(*enabled)]])
            }
            RingFrame::Series { cmd, packet } => packet.encode(*cmd),
            RingFrame::AutoHrvPref { enabled } => {
                frame(Cmd::AutoHrvPref, &[&[0x01, on_byte(*enabled)]])
            }
            RingFrame::Activity(packet) => packet.encode(),
            RingFrame::TodayTotals {
                steps,
                running_steps,
                cal,
                distance_m,
                active_min,
            } => frame(
                Cmd::TodayTotals,
                &[
                    &u24_to_be(*steps, "steps")?,
                    &u24_to_be(*running_steps, "running steps")?,
                    &u24_to_be(*cal, "cal")?,
                    &u24_to_be(*distance_m, "distance")?,
                    &active_min.to_be_bytes(),
                ],
            ),
            RingFrame::Notify(notification) => notification.encode(),
            RingFrame::WorkoutCtlAck { action, start } => frame(
                Cmd::WorkoutCtl,
                &[&[action.byte(), 0x00], &start.unwrap_or(0).to_le_bytes()],
            ),
            RingFrame::WorkoutData {
                sport_type,
                flag,
                seq,
                bpm,
            } => frame(Cmd::WorkoutData, &[&[*sport_type, *flag, 0x00, *seq, *bpm]]),
            RingFrame::Raw { cmd, body } => Ok(Frame {
                cmd: *cmd,
                body: *body,
            }),
        }
    }
}

/// `01 <steps u24> <calories u24> <distance u24> <sport_min u16> <sleep_min u16>`.
fn goals(body: &Body) -> RingFrame {
    let [_, s0, s1, s2, c0, c1, c2, d0, d1, d2, m0, m1, z0, z1] = *body;
    RingFrame::Goals {
        steps: u24_le([s0, s1, s2]),
        calories: u24_le([c0, c1, c2]),
        distance: u24_le([d0, d1, d2]),
        sport_min: u16::from_le_bytes([m0, m1]),
        sleep_min: u16::from_le_bytes([z0, z1]),
    }
}

/// `<steps u24 be> <running_steps u24 be> <cal u24 be> <distance_m u24 be> <active_min u16 be>`.
fn today_totals(body: &Body) -> RingFrame {
    let [s0, s1, s2, r0, r1, r2, c0, c1, c2, d0, d1, d2, a0, a1] = *body;
    RingFrame::TodayTotals {
        steps: u24_be([s0, s1, s2]),
        running_steps: u24_be([r0, r1, r2]),
        cal: u24_be([c0, c1, c2]),
        distance_m: u24_be([d0, d1, d2]),
        active_min: u16::from_be_bytes([a0, a1]),
    }
}

/// What the ring says it can do: the body of the `0x01` set-time ack.
///
/// Layout (R09 decompilation, consistent with this ring):
///
/// ```text
/// <temperature> <watch_faces> <menstruation> <features> <screen_w u16> <screen_h u16>
/// <new_sleep_protocol> 00 <?> 00 00 <?>
/// ```
///
/// The fixture ack `01 00 00 02 00 00 00 00 01 00 20 00 00 30` has temperature, `SpO2`
/// (feature bit 1) and the new sleep protocol (sleep comes as `bc 27`); its bytes 10
/// (`20`) and 13 (`30`) are not decoded and survive in [`Capabilities::raw`].
#[expect(
    clippy::struct_excessive_bools,
    reason = "the ring reports each capability as its own byte; packing them into flags would invent a structure the wire does not have"
)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub struct Capabilities {
    /// Body byte 0: skin temperature is measured (`bc 25`).
    pub temperature: bool,
    /// Body byte 1: watch faces can be changed.
    pub watch_faces: bool,
    /// Body byte 2: menstruation tracking.
    pub menstruation: bool,
    /// Body byte 3: feature bits; bit 1 is `SpO2`, see [`Capabilities::spo2`].
    pub features: u8,
    /// Body bytes 4–5: screen width, `0` on a ring.
    pub screen_w: u16,
    /// Body bytes 6–7: screen height, `0` on a ring.
    pub screen_h: u16,
    /// Body byte 8: sleep comes through big data (`bc 27`) rather than a `0x44` frame.
    pub new_sleep_protocol: bool,
    /// The whole body as received, so the bytes above do not decode are not lost.
    /// [`RingFrame::encode`] starts from it and writes the decoded fields over their
    /// positions.
    pub raw: [u8; Frame::BODY_LEN],
}

impl Capabilities {
    /// Whether the feature bits say `SpO2` is measured.
    #[must_use]
    pub const fn spo2(&self) -> bool {
        self.features & FEATURE_SPO2 != 0
    }

    /// The fields at their byte positions, and the body itself as `raw`.
    fn from_body(body: &Body) -> Capabilities {
        let [
            temperature,
            watch_faces,
            menstruation,
            features,
            w0,
            w1,
            h0,
            h1,
            new_sleep,
            ..,
        ] = *body;
        Capabilities {
            temperature: is_on(temperature),
            watch_faces: is_on(watch_faces),
            menstruation: is_on(menstruation),
            features,
            screen_w: u16::from_le_bytes([w0, w1]),
            screen_h: u16::from_le_bytes([h0, h1]),
            new_sleep_protocol: is_on(new_sleep),
            raw: *body,
        }
    }

    /// `raw` with the decoded fields written over their positions.
    fn to_body(self) -> Body {
        let mut body = self.raw;
        let [w0, w1] = self.screen_w.to_le_bytes();
        let [h0, h1] = self.screen_h.to_le_bytes();
        body[0] = on_byte(self.temperature);
        body[1] = on_byte(self.watch_faces);
        body[2] = on_byte(self.menstruation);
        body[3] = self.features;
        body[4] = w0;
        body[5] = w1;
        body[6] = h0;
        body[7] = h1;
        body[8] = on_byte(self.new_sleep_protocol);
        body
    }
}

/// Whether a `More` packet index is one the wire can carry: `0x00` and `0x01` are the
/// header and first packet, `0xff` is "no data".
fn more_index(index: u8) -> Result<u8, WireError> {
    if index < 2 || index == NO_DATA {
        return Err(WireError::Value {
            what: "packet index",
        });
    }
    Ok(index)
}

/// One packet of the `0x15` HR log, keyed on its first byte.
///
/// A day's log is a header, a first packet with the day's start and nine samples, then
/// packets of thirteen samples up to index `count - 1`. Sample `i` of packet `n ≥ 2` is
/// at `(9 + (n - 2) * 13 + i) * interval_min` minutes after the day's start; `0` means no
/// sample.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub enum HrLogPacket {
    /// `ff`: no data for that day.
    NoData,
    /// `00 <count> <interval_min>`: how many packets follow and the minutes between
    /// samples (`00 18 05` = 24 packets, 5-minute samples).
    Header {
        /// Packets in the log, the header not included.
        count: u8,
        /// Minutes between samples.
        interval_min: u8,
    },
    /// `01 <day_start u32> <9 samples>`.
    First {
        /// The day's local midnight as if UTC, the same number the request carried.
        day_start: u32,
        /// The first nine samples, bpm.
        samples: [u8; 9],
    },
    /// `<index> <13 samples>` for an index of 2 or more.
    More {
        /// The packet's index, `2..count`.
        index: u8,
        /// Thirteen samples, bpm.
        samples: [u8; 13],
    },
}

impl HrLogPacket {
    /// The packet the body carries; total.
    fn from_body(body: &Body) -> HrLogPacket {
        match *body {
            [NO_DATA, ..] => HrLogPacket::NoData,
            [0x00, count, interval_min, ..] => HrLogPacket::Header {
                count,
                interval_min,
            },
            [0x01, t0, t1, t2, t3, samples @ ..] => HrLogPacket::First {
                day_start: u32::from_le_bytes([t0, t1, t2, t3]),
                samples,
            },
            [index, samples @ ..] => HrLogPacket::More { index, samples },
        }
    }

    /// The `0x15` frame for this packet.
    fn encode(&self) -> Result<Frame, WireError> {
        match self {
            HrLogPacket::NoData => frame(Cmd::HrLog, &[&[NO_DATA]]),
            HrLogPacket::Header {
                count,
                interval_min,
            } => frame(Cmd::HrLog, &[&[0x00, *count, *interval_min]]),
            HrLogPacket::First { day_start, samples } => {
                frame(Cmd::HrLog, &[&[0x01], &day_start.to_le_bytes(), samples])
            }
            HrLogPacket::More { index, samples } => {
                frame(Cmd::HrLog, &[&[more_index(*index)?], samples])
            }
        }
    }
}

/// Which of the two series that share the [`SeriesPacket`] layout a frame belongs to.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub enum SeriesCmd {
    /// `0x37`: stress.
    Stress,
    /// `0x39`: heart-rate variability.
    Hrv,
}

impl SeriesCmd {
    /// The command byte of the series.
    #[must_use]
    pub const fn cmd(self) -> Cmd {
        match self {
            SeriesCmd::Stress => Cmd::StressLog,
            SeriesCmd::Hrv => Cmd::HrvLog,
        }
    }
}

/// One packet of a `0x37` stress or `0x39` HRV series, keyed on its first byte.
///
/// The packetisation of [`HrLogPacket`] with a day offset in place of the timestamp:
/// slot `i` of the day is `i * interval_min` minutes after midnight of `days_ago`; the
/// first packet holds slots 0–11, packet `n ≥ 2` slots `12 + (n - 2) * 13` onward. With
/// 30-minute slots, 12 + 4 × 13 = 64 ≥ 48, and the tail is zero.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub enum SeriesPacket {
    /// `ff`: no data for that day.
    NoData,
    /// `00 <count> <interval_min>` (`00 05 1e` = 5 packets, 30-minute slots).
    Header {
        /// Packets in the series, the header not included.
        count: u8,
        /// Minutes per slot.
        interval_min: u8,
    },
    /// `01 <days_ago> <12 samples>`.
    First {
        /// The day, as the request asked for it.
        days_ago: u8,
        /// Slots 0–11.
        samples: [u8; 12],
    },
    /// `<index> <13 samples>` for an index of 2 or more.
    More {
        /// The packet's index, `2..count`.
        index: u8,
        /// Thirteen slots.
        samples: [u8; 13],
    },
}

impl SeriesPacket {
    /// The packet the body carries; total.
    fn from_body(body: &Body) -> SeriesPacket {
        match *body {
            [NO_DATA, ..] => SeriesPacket::NoData,
            [0x00, count, interval_min, ..] => SeriesPacket::Header {
                count,
                interval_min,
            },
            [0x01, days_ago, samples @ ..] => SeriesPacket::First { days_ago, samples },
            [index, samples @ ..] => SeriesPacket::More { index, samples },
        }
    }

    /// The frame for this packet on `cmd`'s series.
    fn encode(&self, cmd: SeriesCmd) -> Result<Frame, WireError> {
        let cmd = cmd.cmd();
        match self {
            SeriesPacket::NoData => frame(cmd, &[&[NO_DATA]]),
            SeriesPacket::Header {
                count,
                interval_min,
            } => frame(cmd, &[&[0x00, *count, *interval_min]]),
            SeriesPacket::First { days_ago, samples } => frame(cmd, &[&[0x01, *days_ago], samples]),
            SeriesPacket::More { index, samples } => frame(cmd, &[&[more_index(*index)?], samples]),
        }
    }
}

/// One packet of a `0x43` activity reply, keyed on its first byte.
///
/// A day with activity is a header then one row per bucket that has any; this ring sends
/// whole hours only (`bucket` 4 = 01:00, 44 = 11:00, …).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub enum ActivityPacket {
    /// `ff 00 <unknown>`: no data for that day.
    NoData {
        /// The third byte: `01` on the reply for yesterday, `00` for older days in the
        /// fixture. The header carries its new-protocol flag at this offset, so it may be
        /// the same bit; kept so the frame re-encodes byte for byte.
        unknown: u8,
    },
    /// `f0 <count> <new_protocol>` (`f0 05 01` before five rows).
    Header {
        /// The second byte: `05` before five rows, presumably how many follow.
        count: u8,
        /// `01`: the calorie field of each row is in units of 10 calories (R09
        /// decompilation; this is why the app's calorie column is raw × 10).
        new_protocol: bool,
    },
    /// `YY MM DD <bucket> <index> <total> <cal_raw u16> <steps u16> <distance_m u16>`, the
    /// date BCD (`26 07 02 04 00 05 70 00 1c 00 13 00` = 2026-07-02 01:00, row 0 of 5,
    /// 112 → 1120 cal, 28 steps, 19 m).
    Row {
        /// Year, `2000..=2099`.
        year: u16,
        /// Month.
        month: u8,
        /// Day of month.
        day: u8,
        /// The 15-minute bucket the row covers, `0..=95`.
        bucket: u8,
        /// The row's index among the day's rows.
        index: u8,
        /// How many rows the day has.
        total: u8,
        /// Calories as sent; × 10 under the new protocol.
        cal_raw: u16,
        /// Steps in the bucket.
        steps: u16,
        /// Metres in the bucket.
        distance_m: u16,
    },
}

impl ActivityPacket {
    /// The packet the body carries; `None` if a row's date is not BCD.
    fn from_body(body: &Body) -> Option<ActivityPacket> {
        match *body {
            [NO_DATA, _, unknown, ..] => Some(ActivityPacket::NoData { unknown }),
            [ACTIVITY_HEADER, count, new_protocol, ..] => Some(ActivityPacket::Header {
                count,
                new_protocol: is_on(new_protocol),
            }),
            [yy, mm, dd, bucket, index, total, c0, c1, s0, s1, d0, d1, ..] => {
                Some(ActivityPacket::Row {
                    year: year_of_bcd(yy)?,
                    month: bcd_to_u8(mm)?,
                    day: bcd_to_u8(dd)?,
                    bucket,
                    index,
                    total,
                    cal_raw: u16::from_le_bytes([c0, c1]),
                    steps: u16::from_le_bytes([s0, s1]),
                    distance_m: u16::from_le_bytes([d0, d1]),
                })
            }
        }
    }

    /// The `0x43` frame for this packet.
    fn encode(&self) -> Result<Frame, WireError> {
        match self {
            ActivityPacket::NoData { unknown } => {
                frame(Cmd::Activity, &[&[NO_DATA, 0x00, *unknown]])
            }
            ActivityPacket::Header {
                count,
                new_protocol,
            } => frame(
                Cmd::Activity,
                &[&[ACTIVITY_HEADER, *count, on_byte(*new_protocol)]],
            ),
            ActivityPacket::Row {
                year,
                month,
                day,
                bucket,
                index,
                total,
                cal_raw,
                steps,
                distance_m,
            } => frame(
                Cmd::Activity,
                &[
                    &[
                        bcd_year(*year)?,
                        bcd(*month, "month")?,
                        bcd(*day, "day")?,
                        *bucket,
                        *index,
                        *total,
                    ],
                    &cal_raw.to_le_bytes(),
                    &steps.to_le_bytes(),
                    &distance_m.to_le_bytes(),
                ],
            ),
        }
    }
}

/// What a `0x73` notification announces, keyed on its first byte.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub enum Notification {
    /// `01`: a new periodic HR sample is in the log.
    NewHr,
    /// `03`: a new `SpO2` sample.
    NewSpo2,
    /// `04`: new steps.
    NewSteps,
    /// `07`: a workout record is stored, after a `77 04` stop.
    WorkoutStored,
    /// `0c <percent>`: the battery level (`0c 5e` = 94 %).
    Battery(Percent),
    /// `12 <steps u24 be> <cal u24 be> <distance_m u24 be>`: today's totals so far
    /// (`00 05 85 00 f9 8f 00 04 3c` = 1413 steps, 63887 cal, 1084 m).
    LiveActivity {
        /// Steps so far today.
        steps: u32,
        /// Calories so far today, small calories.
        cal: u32,
        /// Metres so far today.
        distance_m: u32,
    },
    /// Any other sub-type, with the thirteen bytes after it (`27 a6` and `2c 23` were each
    /// seen once, meaning unknown).
    Other {
        /// The sub-type byte.
        sub: u8,
        /// The rest of the body, undecoded.
        body: [u8; 13],
    },
}

impl Notification {
    /// The notification the body carries; `None` for a battery percentage above 100.
    fn from_body(body: &Body) -> Option<Notification> {
        match *body {
            [0x01, ..] => Some(Notification::NewHr),
            [0x03, ..] => Some(Notification::NewSpo2),
            [0x04, ..] => Some(Notification::NewSteps),
            [0x07, ..] => Some(Notification::WorkoutStored),
            [0x0c, percent, ..] => Percent::new(percent).map(Notification::Battery),
            [0x12, s0, s1, s2, c0, c1, c2, d0, d1, d2, ..] => Some(Notification::LiveActivity {
                steps: u24_be([s0, s1, s2]),
                cal: u24_be([c0, c1, c2]),
                distance_m: u24_be([d0, d1, d2]),
            }),
            [sub, body @ ..] => Some(Notification::Other { sub, body }),
        }
    }

    /// The `0x73` frame for this notification.
    fn encode(&self) -> Result<Frame, WireError> {
        match self {
            Notification::NewHr => frame(Cmd::Notify, &[&[0x01]]),
            Notification::NewSpo2 => frame(Cmd::Notify, &[&[0x03]]),
            Notification::NewSteps => frame(Cmd::Notify, &[&[0x04]]),
            Notification::WorkoutStored => frame(Cmd::Notify, &[&[0x07]]),
            Notification::Battery(percent) => frame(Cmd::Notify, &[&[0x0c, percent.get()]]),
            Notification::LiveActivity {
                steps,
                cal,
                distance_m,
            } => frame(
                Cmd::Notify,
                &[
                    &[0x12],
                    &u24_to_be(*steps, "steps")?,
                    &u24_to_be(*cal, "cal")?,
                    &u24_to_be(*distance_m, "distance")?,
                ],
            ),
            Notification::Other { sub, body } => frame(Cmd::Notify, &[&[*sub], body]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame for `cmd` with `payload`, zero-padded.
    fn raw(cmd: u8, payload: &[u8]) -> Frame {
        Frame::new(Cmd::from_byte(cmd), payload).unwrap_or_else(|err| panic!("{err}"))
    }

    /// `payload` after command byte `cmd` decodes to `value`, and `value` encodes back to
    /// exactly that frame.
    fn check(cmd: u8, payload: &[u8], value: RingFrame) {
        let frame = raw(cmd, payload);
        assert_eq!(RingFrame::decode(&frame), Ok(value), "{payload:02x?}");
        assert_eq!(value.encode(), Ok(frame), "{value:?}");
    }

    fn percent(value: u8) -> Percent {
        Percent::new(value).unwrap_or_else(|| panic!("{value}"))
    }

    /// The `QRing` fixture's `0x01` ack body.
    const CAPS: [u8; 14] = [
        0x01, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x20, 0x00, 0x00, 0x30,
    ];

    #[test]
    fn set_time_ack_is_the_capability_bitmap() {
        let caps = Capabilities {
            temperature: true,
            watch_faces: false,
            menstruation: false,
            features: 0x02,
            screen_w: 0,
            screen_h: 0,
            new_sleep_protocol: true,
            raw: CAPS,
        };
        check(0x01, &CAPS, RingFrame::SetTimeAck(caps));
        assert!(caps.spo2());
        assert!(
            !Capabilities {
                features: 0x01,
                ..caps
            }
            .spo2()
        );

        // Edited fields win over `raw`; undecoded bytes come from `raw`.
        let edited = Capabilities {
            watch_faces: true,
            screen_w: 0x0140,
            screen_h: 0x01c2,
            ..caps
        };
        let body = RingFrame::SetTimeAck(edited)
            .encode()
            .unwrap_or_else(|err| panic!("{err}"))
            .body;
        assert_eq!(
            body,
            [
                0x01, 0x01, 0x00, 0x02, 0x40, 0x01, 0xc2, 0x01, 0x01, 0x00, 0x20, 0x00, 0x00, 0x30
            ]
        );
        let built = Capabilities {
            raw: [0; 14],
            ..caps
        };
        let body = RingFrame::SetTimeAck(built)
            .encode()
            .unwrap_or_else(|err| panic!("{err}"))
            .body;
        assert_eq!(
            body,
            [
                0x01, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00
            ]
        );
    }

    #[test]
    fn battery_is_percent_and_charging() {
        check(
            0x03,
            &[0x64, 0x00],
            RingFrame::Battery {
                percent: Percent::FULL,
                charging: false,
            },
        );
        check(
            0x03,
            &[0x5e, 0x01],
            RingFrame::Battery {
                percent: percent(94),
                charging: true,
            },
        );
        assert_eq!(
            RingFrame::decode(&raw(0x03, &[101, 0x00])),
            Err(DecodeError::Malformed {
                cmd: Cmd::Battery,
                what: "battery percent"
            })
        );
    }

    #[test]
    fn acks_and_prefs() {
        check(0x04, &[0x00], RingFrame::PhoneNameAck);
        check(0x0a, &[0x02], RingFrame::PrefsWriteAck);
        check(0x19, &[0x01, 0x00, 0x01], RingFrame::VersionAck);
        check(
            0x0a,
            &[0x01, 0x00, 0x00, 0x00, 0x1e, 0xaf, 0x46],
            RingFrame::Prefs(Prefs {
                hour12: false,
                imperial: false,
                sex: 0,
                age: 30,
                height_cm: 175,
                weight_kg: 70,
                sbp: 0,
                dbp: 0,
                hr_warn: 0,
            }),
        );
        let malformed = |cmd| {
            Err(DecodeError::Malformed {
                cmd: Cmd::from_byte(cmd),
                what: "sub-command",
            })
        };
        assert_eq!(RingFrame::decode(&raw(0x04, &[0x01])), malformed(0x04));
        assert_eq!(RingFrame::decode(&raw(0x0a, &[0x03])), malformed(0x0a));
        assert_eq!(RingFrame::decode(&raw(0x19, &[0x02])), malformed(0x19));
        assert_eq!(RingFrame::decode(&raw(0x21, &[0x02])), malformed(0x21));
        assert_eq!(RingFrame::decode(&raw(0x16, &[0x02])), malformed(0x16));
    }

    #[test]
    fn hr_log_packets_by_first_byte() {
        check(0x15, &[0xff], RingFrame::HrLog(HrLogPacket::NoData));
        check(
            0x15,
            &[0x00, 0x18, 0x05],
            RingFrame::HrLog(HrLogPacket::Header {
                count: 24,
                interval_min: 5,
            }),
        );
        check(
            0x15,
            &[
                0x01, 0x00, 0xaa, 0x45, 0x6a, 0x3f, 0x50, 0x4a, 0x47, 0x3d, 0x3c, 0x3a, 0x3f, 0x75,
            ],
            RingFrame::HrLog(HrLogPacket::First {
                day_start: 1_782_950_400,
                samples: [0x3f, 0x50, 0x4a, 0x47, 0x3d, 0x3c, 0x3a, 0x3f, 0x75],
            }),
        );
        check(
            0x15,
            &[
                0x02, 0x3d, 0x3b, 0x3c, 0x3b, 0x3b, 0x38, 0x50, 0x52, 0x4e, 0x4f, 0x49, 0x58, 0x3c,
            ],
            RingFrame::HrLog(HrLogPacket::More {
                index: 2,
                samples: [
                    0x3d, 0x3b, 0x3c, 0x3b, 0x3b, 0x38, 0x50, 0x52, 0x4e, 0x4f, 0x49, 0x58, 0x3c,
                ],
            }),
        );
        check(
            0x15,
            &[0x17],
            RingFrame::HrLog(HrLogPacket::More {
                index: 23,
                samples: [0; 13],
            }),
        );
        for index in [0, 1, 0xff] {
            let bad = RingFrame::HrLog(HrLogPacket::More {
                index,
                samples: [0; 13],
            });
            assert_eq!(
                bad.encode(),
                Err(WireError::Value {
                    what: "packet index"
                })
            );
        }
    }

    #[test]
    fn preference_replies() {
        check(
            0x16,
            &[0x01, 0x01, 0x05, 0x05],
            RingFrame::AutoHrPref {
                enabled: true,
                interval_min: 5,
                unknown: 5,
            },
        );
        check(
            0x2c,
            &[0x01, 0x01, 0x1e],
            RingFrame::AutoSpo2Pref {
                enabled: true,
                interval_min: 30,
            },
        );
        check(
            0x36,
            &[0x01, 0x01],
            RingFrame::AutoStressPref { enabled: true },
        );
        check(
            0x38,
            &[0x01, 0x01],
            RingFrame::AutoHrvPref { enabled: true },
        );
        check(
            0x38,
            &[0x01, 0x00],
            RingFrame::AutoHrvPref { enabled: false },
        );
    }

    #[test]
    fn goals_are_three_u24_and_two_u16() {
        check(
            0x21,
            &[
                0x01, 0x88, 0x13, 0x00, 0xe0, 0x93, 0x04, 0xb8, 0x0b, 0x00, 0x00, 0x00, 0x00,
            ],
            RingFrame::Goals {
                steps: 5000,
                calories: 300_000,
                distance: 3000,
                sport_min: 0,
                sleep_min: 0,
            },
        );
        check(
            0x21,
            &[
                0x01, 0xff, 0xff, 0xff, 0x01, 0x00, 0x00, 0x02, 0x00, 0x00, 0x34, 0x12, 0x78, 0x56,
            ],
            RingFrame::Goals {
                steps: 0x00ff_ffff,
                calories: 1,
                distance: 2,
                sport_min: 0x1234,
                sleep_min: 0x5678,
            },
        );
        let too_big = RingFrame::Goals {
            steps: 0x0100_0000,
            calories: 0,
            distance: 0,
            sport_min: 0,
            sleep_min: 0,
        };
        assert_eq!(too_big.encode(), Err(WireError::Value { what: "steps" }));
    }

    #[test]
    fn packet_size() {
        check(0x2f, &[0xf4], RingFrame::PacketSize(244));
    }

    #[test]
    fn series_packets_for_stress_and_hrv() {
        check(
            0x37,
            &[0x00, 0x05, 0x1e],
            RingFrame::Series {
                cmd: SeriesCmd::Stress,
                packet: SeriesPacket::Header {
                    count: 5,
                    interval_min: 30,
                },
            },
        );
        check(
            0x37,
            &[
                0x01, 0x00, 0x2b, 0x27, 0x24, 0x22, 0x2f, 0x2e, 0x2a, 0x23, 0x22, 0x27, 0x24, 0x21,
            ],
            RingFrame::Series {
                cmd: SeriesCmd::Stress,
                packet: SeriesPacket::First {
                    days_ago: 0,
                    samples: [
                        0x2b, 0x27, 0x24, 0x22, 0x2f, 0x2e, 0x2a, 0x23, 0x22, 0x27, 0x24, 0x21,
                    ],
                },
            },
        );
        check(
            0x39,
            &[
                0x02, 0x32, 0x00, 0x20, 0x00, 0x24, 0x00, 0x2c, 0x00, 0x24, 0x00, 0x25, 0x00, 0x26,
            ],
            RingFrame::Series {
                cmd: SeriesCmd::Hrv,
                packet: SeriesPacket::More {
                    index: 2,
                    samples: [50, 0, 32, 0, 36, 0, 44, 0, 36, 0, 37, 0, 38],
                },
            },
        );
        check(
            0x39,
            &[0xff],
            RingFrame::Series {
                cmd: SeriesCmd::Hrv,
                packet: SeriesPacket::NoData,
            },
        );
        assert_eq!(SeriesCmd::Stress.cmd(), Cmd::StressLog);
        assert_eq!(SeriesCmd::Hrv.cmd(), Cmd::HrvLog);
        let bad = RingFrame::Series {
            cmd: SeriesCmd::Hrv,
            packet: SeriesPacket::More {
                index: 1,
                samples: [0; 13],
            },
        };
        assert_eq!(
            bad.encode(),
            Err(WireError::Value {
                what: "packet index"
            })
        );
    }

    #[test]
    fn activity_packets_by_first_byte() {
        check(
            0x43,
            &[0xff, 0x00, 0x00],
            RingFrame::Activity(ActivityPacket::NoData { unknown: 0 }),
        );
        check(
            0x43,
            &[0xff, 0x00, 0x01],
            RingFrame::Activity(ActivityPacket::NoData { unknown: 1 }),
        );
        check(
            0x43,
            &[0xf0, 0x05, 0x01],
            RingFrame::Activity(ActivityPacket::Header {
                count: 5,
                new_protocol: true,
            }),
        );
        check(
            0x43,
            &[
                0x26, 0x07, 0x02, 0x04, 0x00, 0x05, 0x70, 0x00, 0x1c, 0x00, 0x13, 0x00,
            ],
            RingFrame::Activity(ActivityPacket::Row {
                year: 2026,
                month: 7,
                day: 2,
                bucket: 4,
                index: 0,
                total: 5,
                cal_raw: 112,
                steps: 28,
                distance_m: 19,
            }),
        );
        // 43 26 07 02 30 02 05 50 0c dc 02 18 02: 12:00, 3152 → 31520 cal, 732 steps, 536 m.
        check(
            0x43,
            &[
                0x26, 0x07, 0x02, 0x30, 0x02, 0x05, 0x50, 0x0c, 0xdc, 0x02, 0x18, 0x02,
            ],
            RingFrame::Activity(ActivityPacket::Row {
                year: 2026,
                month: 7,
                day: 2,
                bucket: 48,
                index: 2,
                total: 5,
                cal_raw: 3152,
                steps: 732,
                distance_m: 536,
            }),
        );
        assert_eq!(
            RingFrame::decode(&raw(0x43, &[0x2a, 0x07, 0x02, 0x04, 0x00, 0x05])),
            Err(DecodeError::Malformed {
                cmd: Cmd::Activity,
                what: "bcd date"
            })
        );
        let bad_year = RingFrame::Activity(ActivityPacket::Row {
            year: 1999,
            month: 7,
            day: 2,
            bucket: 4,
            index: 0,
            total: 5,
            cal_raw: 0,
            steps: 0,
            distance_m: 0,
        });
        assert_eq!(bad_year.encode(), Err(WireError::Value { what: "year" }));
    }

    #[test]
    fn today_totals_are_big_endian_from_body_zero() {
        check(
            0x48,
            &[
                0x00, 0x05, 0x79, 0x00, 0x00, 0x00, 0x00, 0xf7, 0xaf, 0x00, 0x04, 0x34, 0x00, 0x29,
            ],
            RingFrame::TodayTotals {
                steps: 1401,
                running_steps: 0,
                cal: 63407,
                distance_m: 1076,
                active_min: 41,
            },
        );
        let too_big = RingFrame::TodayTotals {
            steps: 0,
            running_steps: 0,
            cal: 0x0100_0000,
            distance_m: 0,
            active_min: 0,
        };
        assert_eq!(too_big.encode(), Err(WireError::Value { what: "cal" }));
    }

    #[test]
    fn notifications_by_sub_type() {
        check(0x73, &[0x01], RingFrame::Notify(Notification::NewHr));
        check(0x73, &[0x03], RingFrame::Notify(Notification::NewSpo2));
        check(0x73, &[0x04], RingFrame::Notify(Notification::NewSteps));
        check(
            0x73,
            &[0x07],
            RingFrame::Notify(Notification::WorkoutStored),
        );
        check(
            0x73,
            &[0x0c, 0x5e],
            RingFrame::Notify(Notification::Battery(percent(94))),
        );
        check(
            0x73,
            &[0x12, 0x00, 0x05, 0x85, 0x00, 0xf9, 0x8f, 0x00, 0x04, 0x3c],
            RingFrame::Notify(Notification::LiveActivity {
                steps: 1413,
                cal: 63887,
                distance_m: 1084,
            }),
        );
        check(
            0x73,
            &[0x27, 0xa6],
            RingFrame::Notify(Notification::Other {
                sub: 0x27,
                body: [0xa6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            }),
        );
        assert_eq!(
            RingFrame::decode(&raw(0x73, &[0x0c, 0x65])),
            Err(DecodeError::Malformed {
                cmd: Cmd::Notify,
                what: "battery percent"
            })
        );
        let too_big = RingFrame::Notify(Notification::LiveActivity {
            steps: 0,
            cal: 0,
            distance_m: 0x0100_0000,
        });
        assert_eq!(too_big.encode(), Err(WireError::Value { what: "distance" }));
    }

    #[test]
    fn workout_ack_and_stream_from_the_thering_fixture() {
        check(
            0x77,
            &[0x01, 0x00, 0x80, 0x29, 0x1d, 0x69],
            RingFrame::WorkoutCtlAck {
                action: WorkoutAction::Start,
                start: Some(0x691d_2980),
            },
        );
        check(
            0x77,
            &[0x00],
            RingFrame::WorkoutCtlAck {
                action: WorkoutAction::Other(0),
                start: None,
            },
        );
        check(
            0x78,
            &[0x07, 0x01, 0x00, 0x0a, 0x3e],
            RingFrame::WorkoutData {
                sport_type: 7,
                flag: 1,
                seq: 10,
                bpm: 62,
            },
        );
    }

    #[test]
    fn raw_for_unnamed_and_host_only_commands() {
        let other = raw(0x3c, &[0x00, 0xac, 0x27]);
        check(
            0x3c,
            &[0x00, 0xac, 0x27],
            RingFrame::Raw {
                cmd: Cmd::Other(0x3c),
                body: other.body,
            },
        );
        for cmd in [0x50, 0x69, 0x6a, 0xff] {
            let frame = raw(cmd, &[0x01]);
            assert_eq!(
                RingFrame::decode(&frame),
                Ok(RingFrame::Raw {
                    cmd: Cmd::from_byte(cmd),
                    body: frame.body,
                })
            );
        }
    }

    // Invariant 5 at this layer: no body makes either decoder panic, and whatever decodes
    // encodes again.
    #[test]
    fn every_command_byte_decodes_or_errs_without_panicking() {
        for cmd in 0..=u8::MAX {
            for fill in [0x00, 0x01, 0x02, 0x0f, 0x12, 0x26, 0x65, 0xf0, 0xff] {
                let frame = Frame {
                    cmd: Cmd::from_byte(cmd),
                    body: [fill; 14],
                };
                if let Ok(value) = RingFrame::decode(&frame) {
                    let _ = value.encode();
                }
                if let Ok(value) = crate::wire::HostFrame::decode(&frame) {
                    let _ = value.encode();
                }
            }
        }
    }
}
