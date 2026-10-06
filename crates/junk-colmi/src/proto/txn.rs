//! Transactions: one typed state machine per request kind (SPEC §3.3).

use alloc::string::String;
use alloc::vec::Vec;
use core::mem;

use crate::measure::{StepBucket, bpm_from_raw};
use junk_core::{Battery, Bytes, Channel, ProtoError, Timestamp};

use crate::proto::{MINUTES_PER_DAY, Req, Resp, civil_from_days, day_minute, model, small};
use crate::wire::{
    ActivityPacket, BigData, BigDataKind, BodyError, Cmd, Frame, HostFrame, HrLogPacket, ReplyBody,
    RequestBody, RingFrame, SeriesCmd, SeriesPacket, WireError, WorkoutDescriptor,
};
use crate::{DIS_FW, DIS_HW, V1_WRITE, V2_CMD};

/// Seconds per minute: the HR log's timestamp counts seconds, a [`Timestamp`] minutes.
const SECONDS_PER_MINUTE: i64 = 60;
/// The index of the first packet of a log, after the header.
const FIRST_PACKET: u8 = 1;
/// The number of the first `bc 45` package of a workout detail.
const FIRST_PACKAGE: u8 = 1;
/// The `bc 44` status that means the detail is available.
const DETAIL_OK: u8 = 0;

/// What feeding one reply did to a transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Accepted; more is needed.
    Continue,
    /// Accepted; read this channel first, then more is needed. Its value comes back
    /// through [`Txn::feed_text`].
    Read(Channel),
    /// Finished with this answer.
    Done(Resp),
    /// Finished with this error.
    Fail(ProtoError),
    /// Not something this transaction is waiting for. Nothing changed.
    Ignored,
}

/// What appending one V2 notification to a transaction's big-data collector did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Collected {
    /// This transaction is not collecting, or the bytes cannot start a frame; they were
    /// not taken.
    NotWanted,
    /// Taken; the frame is not complete yet.
    Incomplete,
    /// The frame is complete and its CRC holds.
    Frame(BigData),
    /// The frame is complete and bad; the collector is empty again.
    Fail(ProtoError),
}

/// How a single reply becomes a [`Resp`]: which typed [`RingFrame`] a
/// [`Txn::Simple`] is waiting for, and what to make of it.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Want {
    /// [`RingFrame::SetTimeAck`] → [`Resp::Capabilities`].
    Capabilities,
    /// [`RingFrame::PhoneNameAck`], [`RingFrame::PrefsWriteAck`] or a [`RingFrame::Raw`]
    /// of the request's command → [`Resp::Ack`].
    Ack,
    /// [`RingFrame::Battery`] → [`Resp::Battery`].
    Battery,
    /// [`RingFrame::Prefs`] → [`Resp::Prefs`].
    Prefs,
    /// [`RingFrame::Goals`] → [`Resp::Goals`].
    Goals,
    /// Any of the four automatic-measurement replies → [`Resp::AutoPref`].
    AutoPref,
    /// [`RingFrame::TodayTotals`] → [`Resp::TodayTotals`].
    TodayTotals,
    /// [`RingFrame::WorkoutCtlAck`] → [`Resp::WorkoutCtl`].
    WorkoutCtl,
    /// Any reply of the request's command → [`Resp::Raw`].
    Raw,
}

impl Want {
    /// The answer `reply` gives a transaction waiting for this, or `None` if it is not
    /// the reply wanted. `frame` is `reply` as it came, for [`Want::Raw`]. The caller has
    /// already checked the command byte.
    fn answer(self, frame: &Frame, reply: &RingFrame) -> Option<Resp> {
        match (self, *reply) {
            (Want::Capabilities, RingFrame::SetTimeAck(caps)) => Some(Resp::Capabilities(caps)),
            (
                Want::Ack,
                RingFrame::PhoneNameAck | RingFrame::PrefsWriteAck | RingFrame::Raw { .. },
            ) => Some(Resp::Ack),
            (Want::Battery, RingFrame::Battery { percent, charging }) => {
                Some(Resp::Battery(Battery { percent, charging }))
            }
            (Want::Prefs, RingFrame::Prefs(prefs)) => Some(Resp::Prefs(prefs)),
            (
                Want::Goals,
                RingFrame::Goals {
                    steps,
                    calories,
                    distance,
                    sport_min,
                    sleep_min,
                },
            ) => Some(Resp::Goals {
                steps,
                calories,
                distance,
                sport_min,
                sleep_min,
            }),
            (
                Want::AutoPref,
                RingFrame::AutoHrPref {
                    enabled,
                    interval_min,
                    ..
                }
                | RingFrame::AutoSpo2Pref {
                    enabled,
                    interval_min,
                },
            ) => Some(Resp::AutoPref {
                enabled,
                interval_min: Some(interval_min),
            }),
            (
                Want::AutoPref,
                RingFrame::AutoStressPref { enabled } | RingFrame::AutoHrvPref { enabled },
            ) => Some(Resp::AutoPref {
                enabled,
                interval_min: None,
            }),
            (
                Want::TodayTotals,
                RingFrame::TodayTotals {
                    steps,
                    running_steps,
                    cal,
                    distance_m,
                    active_min,
                },
            ) => Some(Resp::TodayTotals {
                steps,
                running_steps,
                cal,
                distance_m,
                active_min,
            }),
            (Want::WorkoutCtl, RingFrame::WorkoutCtlAck { start, .. }) => {
                Some(Resp::WorkoutCtl { start })
            }
            (Want::Raw, _) => Some(Resp::Raw(*frame)),
            (
                Want::Capabilities
                | Want::Ack
                | Want::Battery
                | Want::Prefs
                | Want::Goals
                | Want::AutoPref
                | Want::TodayTotals
                | Want::WorkoutCtl,
                _,
            ) => None,
        }
    }
}

/// Which big-data replies a [`Txn::BigData`] takes, and what to make of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BigWant {
    /// The first complete frame of `kind`, undecoded → [`Resp::RawBigData`].
    Raw {
        /// The kind the reply must carry.
        kind: BigDataKind,
    },
    /// The `0x27` reply → [`Resp::Sleep`], its days counted back from `today`.
    Sleep {
        /// The request's today.
        today: Timestamp,
    },
    /// The `0x2a` reply → [`Resp::Spo2`], its days counted back from `today`.
    Spo2 {
        /// The request's today.
        today: Timestamp,
    },
    /// The `0x25` reply → [`Resp::Temperature`], its days counted back from `today`.
    Temperature {
        /// The request's today.
        today: Timestamp,
    },
    /// The `0x42` summary that answers a `0x41` request → [`Resp::Workouts`].
    WorkoutList,
    /// The `0x44` descriptor that answers a `0x43` request, then its `package_count`
    /// `0x45` packages numbered from 1 in order → [`Resp::WorkoutDetail`].
    WorkoutDetail {
        /// The descriptor, once it has come; until then nothing else is wanted.
        descriptor: Option<WorkoutDescriptor>,
        /// The packages' heart rates so far, end to end.
        heart_rates: Vec<u8>,
        /// The number the next package must carry.
        next_package: u8,
    },
}

impl BigWant {
    /// Whether a complete frame of `kind` is the reply this is waiting for now.
    #[must_use]
    pub fn wants(&self, kind: BigDataKind) -> bool {
        let wanted = match self {
            BigWant::Raw { kind } => *kind,
            BigWant::Sleep { .. } => BigDataKind::Sleep,
            BigWant::Spo2 { .. } => BigDataKind::Spo2,
            BigWant::Temperature { .. } => BigDataKind::Temperature,
            BigWant::WorkoutList => BigDataKind::WorkoutSummary,
            BigWant::WorkoutDetail {
                descriptor: None, ..
            } => BigDataKind::WorkoutDescriptor,
            BigWant::WorkoutDetail {
                descriptor: Some(_),
                ..
            } => BigDataKind::WorkoutSeries,
        };
        kind == wanted
    }
}

/// The progress of a packetised log. The `0x15` HR log and the `0x37`/`0x39` series share
/// the shape: a header with the packet count and the slot interval, then packets
/// `1..count` in index order, the last at `count − 1`, each a run of slots. They differ
/// only in what the first packet carries besides its samples, which is the caller's to
/// check.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Log {
    /// The index the next packet must carry; `0` until the header has come.
    next: u8,
    /// From the header: how many packets follow it, so the last has index `count − 1`.
    count: u8,
    /// From the header: minutes between slots.
    interval_min: u8,
    /// Every sample so far, slot by slot; `0` is no sample.
    slots: Vec<u8>,
}

/// Why a log refused a packet.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum LogError {
    /// The header's count leaves no packet to complete on: fewer than two.
    Count,
    /// The packet is not the one due next: a second header, a packet before the header,
    /// or an index other than the one expected.
    Order,
}

impl Log {
    /// Whether the header has come.
    fn started(&self) -> bool {
        self.next != 0
    }

    /// Takes the header.
    fn header(&mut self, count: u8, interval_min: u8) -> Result<(), LogError> {
        if self.started() {
            return Err(LogError::Order);
        }
        if count < 2 {
            return Err(LogError::Count);
        }
        self.count = count;
        self.interval_min = interval_min;
        self.next = FIRST_PACKET;
        Ok(())
    }

    /// Takes packet `index` with its `samples`; `Ok(true)` once it was the last.
    fn packet(&mut self, index: u8, samples: &[u8]) -> Result<bool, LogError> {
        if !self.started() || index != self.next {
            return Err(LogError::Order);
        }
        // The wire never carries an index of 255 (that byte means "no data"), so this
        // is never `None`; were it, no packet could follow and the log has gone wrong.
        let after = index.checked_add(1).ok_or(LogError::Order)?;
        self.slots.extend_from_slice(samples);
        self.next = after;
        Ok(after == self.count)
    }

    /// The slot interval and the slots, taking them out of the log.
    fn finish(&mut self) -> (u8, Vec<u8>) {
        (self.interval_min, mem::take(&mut self.slots))
    }
}

/// Where a [`Txn::Version`] is: the ack, then the two Device Information strings, each
/// read once the step before it is done.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VersionStage {
    /// Waiting for the `19 01 00 01` ack.
    Ack,
    /// The ack has come; waiting for the value of [`DIS_FW`].
    Firmware,
    /// The firmware string has come; waiting for the value of [`DIS_HW`].
    Hardware {
        /// The firmware string.
        firmware: String,
    },
}

/// The in-flight state of one request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Txn {
    /// One 16-byte reply of the request's command, read as `want` says.
    Simple {
        /// The command byte the reply must carry.
        cmd: Cmd,
        /// Which reply, and what to make of it.
        want: Want,
    },
    /// `0x19`: the ack, then the firmware and hardware strings read from the Device
    /// Information service, one after the other.
    Version {
        /// How far it has come.
        stage: VersionStage,
    },
    /// `0x15`: the day's packets, or `15 ff` for none.
    HrLog {
        /// The day's start as the request gave it: where slot 0 is, and the zone.
        day_start: Timestamp,
        /// The day's start as the ring counts it, which the first packet must echo.
        wire_day: u32,
        /// The packets so far.
        log: Log,
    },
    /// `0x37` or `0x39`: the day's packets, or `ff` for none.
    Series {
        /// Which series, so which command byte and which [`Resp`].
        cmd: SeriesCmd,
        /// The day offset the request asked for, which the first packet must echo.
        days_ago: u8,
        /// Midnight of that day: where slot 0 is, and the zone.
        day: Timestamp,
        /// The packets so far.
        log: Log,
    },
    /// `0x43`: `43 ff` for a day with nothing, else a header and then rows `0..total` in
    /// index order.
    Activity {
        /// The zone the rows' dates are in.
        utc_offset_min: i16,
        /// The header's calorie flag, once the header has come.
        new_protocol: Option<bool>,
        /// The index the next row must carry.
        next: u8,
        /// The rows so far.
        buckets: Vec<StepBucket>,
    },
    /// Big-data replies, each collected from MTU-sized notifications and read as `want`
    /// says.
    BigData {
        /// Which replies, and what to make of them.
        want: BigWant,
        /// What has arrived of the current frame so far.
        buf: Vec<u8>,
    },
}

/// A transaction that has been started: its state machine and the write that starts it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Started {
    /// The state machine.
    pub txn: Txn,
    /// Where to write.
    pub chan: Channel,
    /// What to write.
    pub bytes: Bytes,
}

impl Txn {
    /// Begins the transaction for `req`.
    ///
    /// # Errors
    ///
    /// [`ProtoError::Unsupported`] for a request the wire cannot carry (a phone name over
    /// eleven bytes, a year outside `2000..=2099`, a second above 59, an HR log day that
    /// does not fit the ring's `u32`, a sleep selector that is not one or two bytes) and
    /// for a factory reset. [`ProtoError::Malformed`] if a request fails to encode for
    /// any other reason, which none is expected to.
    pub fn start(req: Req) -> Result<Started, ProtoError> {
        match plan(req)? {
            Write::V1(txn, frame) => Ok(Started {
                txn,
                chan: V1_WRITE,
                bytes: frame.encode().map_err(encode_error)?.to_bytes().to_vec(),
            }),
            Write::V2(want, body) => Ok(Started {
                txn: Txn::BigData {
                    want,
                    buf: Vec::new(),
                },
                chan: V2_CMD,
                bytes: body.encode().map_err(encode_error)?.to_bytes(),
            }),
        }
    }

    /// Feeds one 16-byte reply. `frame` is `reply` as it came.
    pub fn feed_frame(&mut self, frame: &Frame, reply: &RingFrame) -> Step {
        match self {
            Txn::Simple { cmd, want } => {
                if frame.cmd != *cmd {
                    return Step::Ignored;
                }
                match want.answer(frame, reply) {
                    Some(resp) => Step::Done(resp),
                    None => Step::Ignored,
                }
            }
            Txn::Version { stage } => {
                if matches!(reply, RingFrame::VersionAck) && *stage == VersionStage::Ack {
                    *stage = VersionStage::Firmware;
                    Step::Read(DIS_FW)
                } else {
                    Step::Ignored
                }
            }
            Txn::HrLog {
                day_start,
                wire_day,
                log,
            } => match reply {
                RingFrame::HrLog(packet) => feed_hr_log(*day_start, *wire_day, log, packet),
                _ => Step::Ignored,
            },
            Txn::Series {
                cmd,
                days_ago,
                day,
                log,
            } => match reply {
                RingFrame::Series { cmd: got, packet } if got == cmd => {
                    feed_series(*cmd, *days_ago, *day, log, packet)
                }
                _ => Step::Ignored,
            },
            Txn::Activity {
                utc_offset_min,
                new_protocol,
                next,
                buckets,
            } => match reply {
                RingFrame::Activity(packet) => {
                    feed_activity(*utc_offset_min, new_protocol, next, buckets, packet)
                }
                _ => Step::Ignored,
            },
            Txn::BigData { .. } => Step::Ignored,
        }
    }

    /// Feeds the value of `chan`, a Device Information channel, as text: the answer to a
    /// [`Step::Read`] this transaction asked for. A value on a channel it is not waiting
    /// for is ignored.
    pub fn feed_text(&mut self, chan: Channel, text: &str) -> Step {
        let Txn::Version { stage } = self else {
            return Step::Ignored;
        };
        match stage {
            VersionStage::Firmware if chan == DIS_FW => {
                *stage = VersionStage::Hardware {
                    firmware: String::from(text),
                };
                Step::Read(DIS_HW)
            }
            VersionStage::Hardware { firmware } if chan == DIS_HW => Step::Done(Resp::Version {
                firmware: mem::take(firmware),
                hardware: String::from(text),
            }),
            VersionStage::Ack | VersionStage::Firmware | VersionStage::Hardware { .. } => {
                Step::Ignored
            }
        }
    }

    /// Appends one V2 notification to the big-data collector, if this transaction has
    /// one, and decodes the frame once it is whole. A complete frame goes to
    /// [`Txn::feed_big`].
    ///
    /// A notification that cannot start a frame (not `0xbc`) while nothing has been
    /// collected is not taken, so a straggler from an earlier reply does not poison this
    /// one; once a frame is under way every byte is part of it.
    pub fn collect_v2(&mut self, bytes: &[u8]) -> Collected {
        let Txn::BigData { buf, .. } = self else {
            return Collected::NotWanted;
        };
        if buf.is_empty() && matches!(BigData::parse(bytes), Err(WireError::Magic { .. })) {
            return Collected::NotWanted;
        }
        buf.extend_from_slice(bytes);
        let Some(expected) = BigData::expected_len(buf) else {
            return Collected::Incomplete;
        };
        if buf.len() < expected {
            return Collected::Incomplete;
        }
        let frame = mem::take(buf);
        match BigData::parse(&frame) {
            Ok(big) => Collected::Frame(big),
            Err(WireError::Crc { .. }) => Collected::Fail(ProtoError::Checksum),
            Err(WireError::Length { .. }) => Collected::Fail(ProtoError::Malformed(
                "big data: more bytes than the header promised",
            )),
            Err(
                WireError::Magic { .. }
                | WireError::Incomplete { .. }
                | WireError::Checksum { .. }
                | WireError::PayloadTooLong { .. }
                | WireError::Value { .. }
                | WireError::Binrw,
            ) => Collected::Fail(ProtoError::Malformed("big data: frame did not decode")),
        }
    }

    /// Feeds one complete big-data frame: ignored unless its kind is the one wanted now,
    /// else decoded as the body its kind has and turned into the answer, or the next
    /// step of one.
    pub fn feed_big(&mut self, big: &BigData) -> Step {
        let Txn::BigData { want, .. } = self else {
            return Step::Ignored;
        };
        if !want.wants(big.kind) {
            return Step::Ignored;
        }
        if let BigWant::Raw { .. } = want {
            return Step::Done(Resp::RawBigData(big.clone()));
        }
        let body = match ReplyBody::decode(big) {
            Ok(body) => body,
            Err(BodyError::Malformed { what, .. }) => {
                return Step::Fail(ProtoError::Malformed(what));
            }
        };
        match (want, body) {
            (BigWant::Sleep { today }, ReplyBody::Sleep(sleep)) => {
                Step::Done(Resp::Sleep(model::sleep_sessions(*today, sleep)))
            }
            (BigWant::Spo2 { today }, ReplyBody::Spo2(spo2)) => {
                Step::Done(Resp::Spo2(model::spo2_samples(*today, &spo2)))
            }
            (BigWant::Temperature { today }, ReplyBody::Temperature(temperature)) => Step::Done(
                Resp::Temperature(model::temperature_samples(*today, &temperature)),
            ),
            (BigWant::WorkoutList, ReplyBody::WorkoutSummary(summary)) => {
                Step::Done(Resp::Workouts(summary))
            }
            (
                BigWant::WorkoutDetail {
                    descriptor: slot @ None,
                    ..
                },
                ReplyBody::WorkoutDescriptor(descriptor),
            ) => {
                if descriptor.status != DETAIL_OK {
                    return Step::Fail(ProtoError::Malformed("workout: status"));
                }
                if descriptor.package_count == 0 {
                    return Step::Done(Resp::WorkoutDetail {
                        descriptor,
                        heart_rates: Vec::new(),
                    });
                }
                *slot = Some(descriptor);
                Step::Continue
            }
            (
                BigWant::WorkoutDetail {
                    descriptor: Some(descriptor),
                    heart_rates,
                    next_package,
                },
                ReplyBody::WorkoutSeries(series),
            ) => {
                if series.package != *next_package {
                    return Step::Fail(ProtoError::Malformed("workout: package order"));
                }
                let Some(rates) = series.heart_rates(descriptor) else {
                    return Step::Fail(ProtoError::Malformed("workout: no heart rate field"));
                };
                heart_rates.extend_from_slice(&rates);
                if series.package == descriptor.package_count {
                    return Step::Done(Resp::WorkoutDetail {
                        descriptor: descriptor.clone(),
                        heart_rates: mem::take(heart_rates)
                            .into_iter()
                            .map(bpm_from_raw)
                            .collect(),
                    });
                }
                // `package < package_count`, so the next number fits.
                *next_package = series.package.saturating_add(1);
                Step::Continue
            }
            // Unreachable: a raw want returned above, `wants` admitted the kind, and a
            // kind decodes as its own body.
            (
                BigWant::Raw { .. }
                | BigWant::Sleep { .. }
                | BigWant::Spo2 { .. }
                | BigWant::Temperature { .. }
                | BigWant::WorkoutList
                | BigWant::WorkoutDetail { .. },
                _,
            ) => Step::Ignored,
        }
    }
}

/// Feeds one `0x15` packet to an HR log.
fn feed_hr_log(day_start: Timestamp, wire_day: u32, log: &mut Log, packet: &HrLogPacket) -> Step {
    let result = match *packet {
        HrLogPacket::NoData if !log.started() => return Step::Done(Resp::HrLog(Vec::new())),
        HrLogPacket::NoData => Err(LogError::Order),
        HrLogPacket::Header {
            count,
            interval_min,
        } => log.header(count, interval_min).map(|()| false),
        HrLogPacket::First {
            day_start: got,
            samples,
        } => match log.packet(FIRST_PACKET, &samples) {
            Ok(_) if got != wire_day => {
                return Step::Fail(ProtoError::Malformed("hr log: day mismatch"));
            }
            result => result,
        },
        HrLogPacket::More { index, samples } => log.packet(index, &samples),
    };
    match result {
        Ok(true) => {
            let (interval_min, slots) = log.finish();
            Step::Done(Resp::HrLog(model::hr_samples(
                day_start,
                interval_min,
                &slots,
            )))
        }
        Ok(false) => Step::Continue,
        Err(LogError::Count) => Step::Fail(ProtoError::Malformed("hr log: count")),
        Err(LogError::Order) => Step::Fail(ProtoError::Malformed("hr log: out of order")),
    }
}

/// Feeds one `0x37`/`0x39` packet to a series.
fn feed_series(
    cmd: SeriesCmd,
    days_ago: u8,
    day: Timestamp,
    log: &mut Log,
    packet: &SeriesPacket,
) -> Step {
    let empty = || match cmd {
        SeriesCmd::Stress => Resp::Stress(Vec::new()),
        SeriesCmd::Hrv => Resp::Hrv(Vec::new()),
    };
    let result = match *packet {
        SeriesPacket::NoData if !log.started() => return Step::Done(empty()),
        SeriesPacket::NoData => Err(LogError::Order),
        SeriesPacket::Header {
            count,
            interval_min,
        } => log.header(count, interval_min).map(|()| false),
        SeriesPacket::First {
            days_ago: got,
            samples,
        } => match log.packet(FIRST_PACKET, &samples) {
            Ok(_) if got != days_ago => {
                return Step::Fail(ProtoError::Malformed("series: day mismatch"));
            }
            result => result,
        },
        SeriesPacket::More { index, samples } => log.packet(index, &samples),
    };
    match result {
        Ok(true) => {
            let (interval_min, slots) = log.finish();
            Step::Done(match cmd {
                SeriesCmd::Stress => Resp::Stress(model::stress_samples(day, interval_min, &slots)),
                SeriesCmd::Hrv => Resp::Hrv(model::hrv_samples(day, interval_min, &slots)),
            })
        }
        Ok(false) => Step::Continue,
        Err(LogError::Count) => Step::Fail(ProtoError::Malformed("series: count")),
        Err(LogError::Order) => Step::Fail(ProtoError::Malformed("series: out of order")),
    }
}

/// Feeds one `0x43` packet to an activity transaction.
fn feed_activity(
    utc_offset_min: i16,
    new_protocol: &mut Option<bool>,
    next: &mut u8,
    buckets: &mut Vec<StepBucket>,
    packet: &ActivityPacket,
) -> Step {
    let out_of_order = || Step::Fail(ProtoError::Malformed("activity: out of order"));
    let (index, total) = match *packet {
        ActivityPacket::NoData { .. } if new_protocol.is_none() => {
            return Step::Done(Resp::Activity(Vec::new()));
        }
        ActivityPacket::NoData { .. } => return out_of_order(),
        ActivityPacket::Header { .. } if new_protocol.is_some() => return out_of_order(),
        ActivityPacket::Header {
            new_protocol: flag, ..
        } => {
            *new_protocol = Some(flag);
            return Step::Continue;
        }
        ActivityPacket::Row { index, total, .. } => (index, total),
    };
    let Some(flag) = *new_protocol else {
        return Step::Fail(ProtoError::Malformed("activity: row before header"));
    };
    if index != *next {
        return out_of_order();
    }
    // A row always makes a bucket; the header and the empty marker returned above.
    buckets.extend(model::step_bucket(packet, flag, utc_offset_min));
    if Some(index) == total.checked_sub(1) {
        return Step::Done(Resp::Activity(mem::take(buckets)));
    }
    *next = index.saturating_add(1);
    Step::Continue
}

/// What starting a request writes, with the state machine that waits for the answer.
enum Write {
    /// A 16-byte frame on V1 write.
    V1(Txn, HostFrame),
    /// A big-data frame on V2 cmd, whose replies are read as the [`BigWant`] says.
    V2(BigWant, RequestBody),
}

/// The write for `req`, one arm per request kind.
///
/// # Errors
///
/// As [`Txn::start`], for what is known before encoding: a factory reset, an HR log day
/// out of range.
fn plan(req: Req) -> Result<Write, ProtoError> {
    Ok(match req {
        Req::SetTime { at, second, lang } => simple(
            Cmd::SetTime,
            Want::Capabilities,
            set_time(at, second, lang)?,
        ),
        Req::Battery => simple(Cmd::Battery, Want::Battery, HostFrame::Battery),
        Req::PhoneName {
            platform,
            os_version,
            name,
        } => simple(
            Cmd::PhoneName,
            Want::Ack,
            HostFrame::PhoneName {
                platform,
                os_version,
                name,
            },
        ),
        Req::ReadPrefs => simple(Cmd::Prefs, Want::Prefs, HostFrame::ReadPrefs),
        Req::WritePrefs(prefs) => simple(Cmd::Prefs, Want::Ack, HostFrame::WritePrefs(prefs)),
        Req::Version => Write::V1(
            Txn::Version {
                stage: VersionStage::Ack,
            },
            HostFrame::Version,
        ),
        Req::ReadGoals => simple(Cmd::Goals, Want::Goals, HostFrame::ReadGoals),
        Req::ReadAutoHrPref => simple(Cmd::AutoHrPref, Want::AutoPref, HostFrame::ReadAutoHrPref),
        Req::ReadAutoSpo2Pref => simple(
            Cmd::AutoSpo2Pref,
            Want::AutoPref,
            HostFrame::ReadAutoSpo2Pref,
        ),
        Req::ReadAutoStressPref => simple(
            Cmd::AutoStressPref,
            Want::AutoPref,
            HostFrame::ReadAutoStressPref,
        ),
        Req::ReadAutoHrvPref => {
            simple(Cmd::AutoHrvPref, Want::AutoPref, HostFrame::ReadAutoHrvPref)
        }
        Req::TodayTotals => simple(Cmd::TodayTotals, Want::TodayTotals, HostFrame::TodayTotals),
        Req::WorkoutCtl { action, sport_type } => simple(
            Cmd::WorkoutCtl,
            Want::WorkoutCtl,
            HostFrame::WorkoutCtl { action, sport_type },
        ),
        Req::ManualHrStart { kind, sub } => simple(
            Cmd::ManualHrStart,
            Want::Ack,
            HostFrame::ManualHrStart { kind, sub },
        ),
        Req::ManualHrStop { kind } => simple(
            Cmd::ManualHrStop,
            Want::Ack,
            HostFrame::ManualHrStop { kind },
        ),
        Req::Raw {
            cmd: Cmd::FactoryReset,
            ..
        } => return Err(ProtoError::Unsupported("factory reset is never sent")),
        Req::Raw { cmd, body } => simple(cmd, Want::Raw, HostFrame::Raw { cmd, body }),
        Req::RawBigData { kind, body } => {
            Write::V2(BigWant::Raw { kind }, RequestBody::Raw { kind, body })
        }
        Req::HrLog { day_start } => hr_log(day_start)?,
        Req::Stress { days_ago, today } => series(SeriesCmd::Stress, days_ago, today),
        Req::Hrv { days_ago, today } => series(SeriesCmd::Hrv, days_ago, today),
        Req::Activity {
            days_ago,
            today,
            first_bucket,
            last_bucket,
        } => activity(days_ago, today, first_bucket, last_bucket),
        Req::Sleep { today, selector } => {
            Write::V2(BigWant::Sleep { today }, RequestBody::Sleep(selector))
        }
        Req::Spo2 { today, selector } => {
            Write::V2(BigWant::Spo2 { today }, RequestBody::Spo2(selector))
        }
        Req::Temperature { today, selector } => Write::V2(
            BigWant::Temperature { today },
            RequestBody::Temperature(selector),
        ),
        Req::WorkoutList { since } => {
            Write::V2(BigWant::WorkoutList, RequestBody::WorkoutList { since })
        }
        Req::WorkoutDetail { sport_type, start } => Write::V2(
            BigWant::WorkoutDetail {
                descriptor: None,
                heart_rates: Vec::new(),
                next_package: FIRST_PACKAGE,
            },
            RequestBody::WorkoutDetail { sport_type, start },
        ),
    })
}

/// A [`Txn::Simple`] on `cmd` that writes `frame`.
fn simple(cmd: Cmd, want: Want, frame: HostFrame) -> Write {
    Write::V1(Txn::Simple { cmd, want }, frame)
}

/// A [`Txn::HrLog`] for the day starting at `day_start`.
fn hr_log(day_start: Timestamp) -> Result<Write, ProtoError> {
    let wire_day = hr_log_day(day_start)?;
    Ok(Write::V1(
        Txn::HrLog {
            day_start,
            wire_day,
            log: Log::default(),
        },
        HostFrame::HrLog {
            day_start: wire_day,
        },
    ))
}

/// A [`Txn::Series`] for `cmd`'s series of the day `days_ago` days before `today`'s.
fn series(cmd: SeriesCmd, days_ago: u8, today: Timestamp) -> Write {
    let frame = match cmd {
        SeriesCmd::Stress => HostFrame::StressLog { days_ago },
        SeriesCmd::Hrv => HostFrame::HrvLog { days_ago },
    };
    Write::V1(
        Txn::Series {
            cmd,
            days_ago,
            day: day_minute(today, days_ago),
            log: Log::default(),
        },
        frame,
    )
}

/// A [`Txn::Activity`] for buckets `first_bucket..=last_bucket` of the day `days_ago`
/// days before `today`'s, in `today`'s zone.
fn activity(days_ago: u8, today: Timestamp, first_bucket: u8, last_bucket: u8) -> Write {
    Write::V1(
        Txn::Activity {
            utc_offset_min: today.utc_offset_min,
            new_protocol: None,
            next: 0,
            buckets: Vec::new(),
        },
        HostFrame::Activity {
            days_ago,
            first_bucket,
            last_bucket,
        },
    )
}

/// The `0x01` frame for `at`: the local wall clock, as the ring expects it.
fn set_time(at: Timestamp, second: u8, lang: u8) -> Result<HostFrame, ProtoError> {
    if second > 59 {
        return Err(ProtoError::Unsupported("set time: second above 59"));
    }
    let (year, month, day) = civil_from_days(at.local_minute.div_euclid(MINUTES_PER_DAY));
    let minute_of_day = at.local_minute.rem_euclid(MINUTES_PER_DAY);
    let year = u16::try_from(year).map_err(|_| ProtoError::Unsupported("year"))?;
    Ok(HostFrame::SetTime {
        year,
        month,
        day,
        hour: small(minute_of_day / 60),
        minute: small(minute_of_day % 60),
        second,
        lang,
    })
}

/// The `0x15` timestamp for `day_start`: its minutes as seconds, the local clock as if it
/// were UTC, or unsupported if the ring's `u32` cannot hold it.
fn hr_log_day(day_start: Timestamp) -> Result<u32, ProtoError> {
    day_start
        .local_minute
        .checked_mul(SECONDS_PER_MINUTE)
        .and_then(|seconds| u32::try_from(seconds).ok())
        .ok_or(ProtoError::Unsupported("hr log: day out of range"))
}

/// Why a request could not be encoded, as the user sees it: a value the layout cannot
/// hold is unsupported by this ring, anything else is a bug in the frame builder.
fn encode_error(err: WireError) -> ProtoError {
    match err {
        WireError::Value { what } => ProtoError::Unsupported(what),
        WireError::PayloadTooLong { .. } => ProtoError::Unsupported("payload too long"),
        WireError::Length { .. }
        | WireError::Checksum { .. }
        | WireError::Magic { .. }
        | WireError::Incomplete { .. }
        | WireError::Crc { .. }
        | WireError::Binrw => ProtoError::Malformed("request did not encode"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    use crate::measure::{Bpm, HrSample, HrvSample, Source, StressSample};
    use junk_core::Percent;

    use crate::wire::{
        DescriptorField, Platform, Prefs, SampleTag, WorkoutAction, WorkoutSeries, WorkoutTag,
    };

    /// 2026-07-02 14:16 in UTC-4, the `QRing` fixture's set-time request.
    const FIXTURE_TIME: Timestamp = Timestamp {
        local_minute: 29_716_696,
        utc_offset_min: -240,
    };

    /// 2026-07-02 00:00 in UTC-4: the fixture day, epoch 1782950400 = 29715840 minutes.
    const FIXTURE_DAY: Timestamp = Timestamp {
        local_minute: 29_715_840,
        utc_offset_min: -240,
    };

    /// The fixture's HR log request body: `00 aa 45 6a` = 1782950400.
    const HR_DAY: u32 = 1_782_950_400;

    /// The first three data packets of the fixture's HR log, after the command byte.
    const HR_PACKETS: [&[u8]; 3] = [
        &[
            0x01, 0x00, 0xaa, 0x45, 0x6a, 0x3f, 0x50, 0x4a, 0x47, 0x3d, 0x3c, 0x3a, 0x3f, 0x75,
        ],
        &[
            0x02, 0x3d, 0x3b, 0x3c, 0x3b, 0x3b, 0x38, 0x50, 0x52, 0x4e, 0x4f, 0x49, 0x58, 0x3c,
        ],
        &[
            0x03, 0x3c, 0x47, 0x39, 0x51, 0x3e, 0x3a, 0x72, 0x53, 0x39, 0x40, 0x52, 0x4b, 0x3d,
        ],
    ];

    /// The fixture's `bc 44` descriptor reply and `bc 45` series reply from the thering
    /// trace.
    const DESCRIPTOR: &str = "bc440600ad87000100010111";
    const SERIES: &str = "bc453800326501003e3e3e3e3e3e3e3d3d3d3d3d3d3d3d3c3c3c3c3d3d3d3d3b3b3b3b3b3b3b3b3b3b3b3d3d3c3c3c3c3c3c3c3c3c3c3c3c3c39393a3a3a";
    /// The fixture's `bc 42` summary reply from the thering trace.
    const SUMMARY: &str = "bc423100e62e013007060180291d6904023d0004030000060400000000040500000406000003073c03083903093e030d00061300000000";
    /// The `QRing` fixture's `bc 27` sleep reply.
    const SLEEP: &str = "bc27310039ec01002ebb00c50202180320021804100225030b02240413021d0312020b041b02160309021e0412022b031b0219040f0231";

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap_or_else(|err| panic!("{err}")))
            .collect()
    }

    fn started(req: Req) -> Started {
        Txn::start(req).unwrap_or_else(|err| panic!("{err}"))
    }

    fn frame(cmd: u8, payload: &[u8]) -> Frame {
        Frame::new(Cmd::from_byte(cmd), payload).unwrap_or_else(|err| panic!("{err}"))
    }

    fn feed(txn: &mut Txn, cmd: u8, payload: &[u8]) -> Step {
        let frame = frame(cmd, payload);
        let reply = RingFrame::decode(&frame).unwrap_or_else(|err| panic!("{err}"));
        txn.feed_frame(&frame, &reply)
    }

    fn big(kind: BigDataKind, body: &[u8]) -> BigData {
        BigData::new(kind, body.to_vec()).unwrap_or_else(|err| panic!("{err}"))
    }

    fn parsed(text: &str) -> BigData {
        BigData::parse(&hex(text)).unwrap_or_else(|err| panic!("{err}"))
    }

    fn malformed(what: &'static str) -> Step {
        Step::Fail(ProtoError::Malformed(what))
    }

    fn at(minute_of_day: i64) -> Timestamp {
        Timestamp {
            local_minute: FIXTURE_DAY.local_minute + minute_of_day,
            ..FIXTURE_DAY
        }
    }

    fn hr_log() -> Txn {
        started(Req::HrLog {
            day_start: FIXTURE_DAY,
        })
        .txn
    }

    fn stress() -> Txn {
        started(Req::Stress {
            days_ago: 0,
            today: FIXTURE_TIME,
        })
        .txn
    }

    fn activity() -> Txn {
        started(Req::activity_day(0, FIXTURE_TIME)).txn
    }

    fn detail() -> Txn {
        started(Req::WorkoutDetail {
            sport_type: 7,
            start: 0x691d_2980,
        })
        .txn
    }

    #[test]
    fn start_writes_the_fixture_frames() {
        let phone = started(Req::PhoneName {
            platform: Platform::Ios,
            os_version: 18,
            name: vec![],
        });
        assert_eq!(phone.chan, V1_WRITE);
        assert_eq!(phone.bytes, frame(0x04, &[0x01, 0x12]).to_bytes());
        assert_eq!(
            phone.txn,
            Txn::Simple {
                cmd: Cmd::PhoneName,
                want: Want::Ack
            }
        );

        let time = started(Req::SetTime {
            at: FIXTURE_TIME,
            second: 30,
            lang: 1,
        });
        assert_eq!(
            time.bytes,
            frame(0x01, &[0x26, 0x07, 0x02, 0x14, 0x16, 0x30, 0x01]).to_bytes()
        );
        assert_eq!(
            time.txn,
            Txn::Simple {
                cmd: Cmd::SetTime,
                want: Want::Capabilities
            }
        );

        assert_eq!(started(Req::Battery).bytes, frame(0x03, &[0x01]).to_bytes());
        let version = started(Req::Version);
        assert_eq!(version.bytes, frame(0x19, &[0x01, 0x01, 0x01]).to_bytes());
        assert_eq!(
            version.txn,
            Txn::Version {
                stage: VersionStage::Ack
            }
        );
        assert_eq!(
            started(Req::Raw {
                cmd: Cmd::Other(0x3c),
                body: [0; 14]
            })
            .bytes,
            frame(0x3c, &[]).to_bytes()
        );

        let list = started(Req::RawBigData {
            kind: BigDataKind::FileList,
            body: vec![],
        });
        assert_eq!(list.chan, V2_CMD);
        assert_eq!(list.bytes, [0xbc, 0x30, 0x00, 0x00, 0xff, 0xff]);
        assert_eq!(
            list.txn,
            Txn::BigData {
                want: BigWant::Raw {
                    kind: BigDataKind::FileList
                },
                buf: vec![]
            }
        );
    }

    #[test]
    fn multi_packet_requests_write_the_fixture_frames() {
        let hr = started(Req::HrLog {
            day_start: FIXTURE_DAY,
        });
        assert_eq!(hr.chan, V1_WRITE);
        assert_eq!(hr.bytes, hex("1500aa456a000000000000000000006e"));
        assert_eq!(
            hr.txn,
            Txn::HrLog {
                day_start: FIXTURE_DAY,
                wire_day: HR_DAY,
                log: Log::default(),
            }
        );

        let stress = started(Req::Stress {
            days_ago: 0,
            today: FIXTURE_TIME,
        });
        assert_eq!(stress.bytes, frame(0x37, &[0x00]).to_bytes());
        assert_eq!(
            stress.txn,
            Txn::Series {
                cmd: SeriesCmd::Stress,
                days_ago: 0,
                day: FIXTURE_DAY,
                log: Log::default(),
            }
        );
        let hrv = started(Req::Hrv {
            days_ago: 3,
            today: FIXTURE_TIME,
        });
        assert_eq!(hrv.bytes, frame(0x39, &[0x03]).to_bytes());
        assert_eq!(
            hrv.txn,
            Txn::Series {
                cmd: SeriesCmd::Hrv,
                days_ago: 3,
                day: at(-3 * MINUTES_PER_DAY),
                log: Log::default(),
            }
        );

        let activity = started(Req::activity_day(0, FIXTURE_TIME));
        assert_eq!(activity.bytes, hex("43000f005f01000000000000000000b2"));
        assert_eq!(
            activity.txn,
            Txn::Activity {
                utc_offset_min: -240,
                new_protocol: None,
                next: 0,
                buckets: vec![],
            }
        );
        // QRing asks the oldest day it keeps from bucket 1: `43 1d 0f 01 5f 01`.
        let oldest = started(Req::Activity {
            days_ago: 29,
            today: FIXTURE_TIME,
            first_bucket: 1,
            last_bucket: Req::LAST_BUCKET,
        });
        assert_eq!(oldest.bytes, hex("431d0f015f01000000000000000000d0"));
    }

    #[test]
    fn big_data_requests_write_the_fixture_frames() {
        let sleep = started(Req::Sleep {
            today: FIXTURE_TIME,
            selector: vec![0x06, 0x01],
        });
        assert_eq!(sleep.chan, V2_CMD);
        assert_eq!(sleep.bytes, hex("bc270200c3d00601"));
        assert_eq!(
            sleep.txn,
            Txn::BigData {
                want: BigWant::Sleep {
                    today: FIXTURE_TIME
                },
                buf: vec![],
            }
        );
        assert_eq!(
            started(Req::Spo2 {
                today: FIXTURE_TIME,
                selector: 0xff,
            })
            .bytes,
            hex("bc2a0100ff00ff")
        );
        assert_eq!(
            started(Req::Temperature {
                today: FIXTURE_TIME,
                selector: 0,
            })
            .bytes,
            hex("bc250100bf4000")
        );
        assert_eq!(
            started(Req::WorkoutList { since: 0 }).bytes,
            hex("bc410400002400000000")
        );
        let detail = started(Req::WorkoutDetail {
            sport_type: 7,
            start: 0x691d_2980,
        });
        assert_eq!(detail.bytes, hex("bc430500a0b60780291d69"));
        assert_eq!(
            detail.txn,
            Txn::BigData {
                want: BigWant::WorkoutDetail {
                    descriptor: None,
                    heart_rates: vec![],
                    next_package: 1,
                },
                buf: vec![],
            }
        );
    }

    #[test]
    fn set_time_handles_midnight_and_the_offset() {
        // The offset is not applied: the ring gets the local clock.
        let midnight = Timestamp {
            local_minute: 20_636 * MINUTES_PER_DAY,
            utc_offset_min: 600,
        };
        let out = started(Req::SetTime {
            at: midnight,
            second: 0,
            lang: 0,
        });
        assert_eq!(
            out.bytes,
            frame(0x01, &[0x26, 0x07, 0x02, 0x00, 0x00, 0x00, 0x00]).to_bytes()
        );
        let last_minute = Timestamp {
            local_minute: 20_636 * MINUTES_PER_DAY + MINUTES_PER_DAY - 1,
            utc_offset_min: 0,
        };
        let out = started(Req::SetTime {
            at: last_minute,
            second: 59,
            lang: 1,
        });
        assert_eq!(
            out.bytes,
            frame(0x01, &[0x26, 0x07, 0x02, 0x23, 0x59, 0x59, 0x01]).to_bytes()
        );
    }

    #[test]
    fn start_refuses_what_the_wire_cannot_carry() {
        let refused = |req| Txn::start(req).map(|s| s.bytes);
        let set_time = |local_minute, second| Req::SetTime {
            at: Timestamp {
                local_minute,
                utc_offset_min: 0,
            },
            second,
            lang: 1,
        };
        assert_eq!(
            refused(set_time(0, 0)),
            Err(ProtoError::Unsupported("year"))
        );
        assert_eq!(
            refused(set_time(47_482 * MINUTES_PER_DAY, 0)),
            Err(ProtoError::Unsupported("year"))
        );
        assert_eq!(
            refused(set_time(FIXTURE_TIME.local_minute, 60)),
            Err(ProtoError::Unsupported("set time: second above 59"))
        );
        assert_eq!(
            refused(set_time(i64::MIN, 0)),
            Err(ProtoError::Unsupported("year"))
        );
        assert_eq!(
            refused(Req::PhoneName {
                platform: Platform::Ios,
                os_version: 18,
                name: vec![b'x'; 12],
            }),
            Err(ProtoError::Unsupported("payload too long"))
        );
        assert_eq!(
            refused(Req::Raw {
                cmd: Cmd::FactoryReset,
                body: [0; 14]
            }),
            Err(ProtoError::Unsupported("factory reset is never sent"))
        );
        assert_eq!(
            refused(Req::RawBigData {
                kind: BigDataKind::FileList,
                body: vec![0; BigData::MAX_BODY_LEN + 1],
            }),
            Err(ProtoError::Unsupported("payload too long"))
        );

        // An HR log day must fit the ring's u32 of seconds.
        let hr_log = |local_minute| Req::HrLog {
            day_start: Timestamp {
                local_minute,
                utc_offset_min: 0,
            },
        };
        let out_of_range = Err(ProtoError::Unsupported("hr log: day out of range"));
        assert_eq!(refused(hr_log(-MINUTES_PER_DAY)), out_of_range);
        assert_eq!(refused(hr_log(i64::MIN)), out_of_range);
        assert_eq!(refused(hr_log(i64::MAX)), out_of_range);
        assert_eq!(
            refused(hr_log(i64::from(u32::MAX) / SECONDS_PER_MINUTE + 1)),
            out_of_range
        );
        assert!(refused(hr_log(i64::from(u32::MAX) / SECONDS_PER_MINUTE)).is_ok());
        assert!(refused(hr_log(0)).is_ok());

        // A sleep selector is one or two bytes.
        for selector in [vec![], vec![1, 1, 1]] {
            assert_eq!(
                refused(Req::Sleep {
                    today: FIXTURE_TIME,
                    selector,
                }),
                Err(ProtoError::Unsupported("sleep selector"))
            );
        }
    }

    #[test]
    fn simple_wants_one_reply_of_its_command() {
        let mut txn = started(Req::Battery).txn;
        assert_eq!(feed(&mut txn, 0x04, &[0x00]), Step::Ignored);
        assert_eq!(feed(&mut txn, 0x2f, &[0xf4]), Step::Ignored);
        assert_eq!(
            feed(&mut txn, 0x03, &[0x64, 0x00]),
            Step::Done(Resp::Battery(Battery {
                percent: Percent::FULL,
                charging: false
            }))
        );

        let mut txn = started(Req::SetTime {
            at: FIXTURE_TIME,
            second: 0,
            lang: 1,
        })
        .txn;
        let caps = [
            0x01, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x20, 0x00, 0x00, 0x30,
        ];
        let Step::Done(Resp::Capabilities(got)) = feed(&mut txn, 0x01, &caps) else {
            panic!("{txn:?}");
        };
        assert!(got.temperature && got.new_sleep_protocol);

        // A read reply is not the write ack, though it shares the command byte.
        let mut txn = started(Req::WritePrefs(fixture_prefs())).txn;
        assert_eq!(
            feed(&mut txn, 0x0a, &[0x01, 0x00, 0x00, 0x00, 0x1e]),
            Step::Ignored
        );
        assert_eq!(feed(&mut txn, 0x0a, &[0x02]), Step::Done(Resp::Ack));
        let mut txn = started(Req::ReadPrefs).txn;
        assert_eq!(feed(&mut txn, 0x0a, &[0x02]), Step::Ignored);
        assert!(matches!(
            feed(&mut txn, 0x0a, &[0x01, 0x00, 0x00, 0x00, 0x1e]),
            Step::Done(Resp::Prefs(Prefs { age: 30, .. }))
        ));

        // The manual-HR acks are undecoded frames of the same command.
        let mut txn = started(Req::ManualHrStart { kind: 1, sub: 0 }).txn;
        assert_eq!(feed(&mut txn, 0x6a, &[0x01]), Step::Ignored);
        assert_eq!(feed(&mut txn, 0x69, &[0x01, 0x3c]), Step::Done(Resp::Ack));
        let mut txn = started(Req::ManualHrStop { kind: 1 }).txn;
        assert_eq!(feed(&mut txn, 0x6a, &[0x01]), Step::Done(Resp::Ack));

        // Raw takes the first reply of its command, typed or not.
        let mut txn = started(Req::Raw {
            cmd: Cmd::Other(0x3c),
            body: [0; 14],
        })
        .txn;
        assert_eq!(
            feed(&mut txn, 0x3b, &[0x01, 0x01, 0x00, 0x01]),
            Step::Ignored
        );
        assert_eq!(
            feed(&mut txn, 0x3c, &[0x00, 0xac, 0x27]),
            Step::Done(Resp::Raw(frame(0x3c, &[0x00, 0xac, 0x27])))
        );
        let mut txn = started(Req::Raw {
            cmd: Cmd::Battery,
            body: [1; 14],
        })
        .txn;
        assert_eq!(
            feed(&mut txn, 0x03, &[0x64, 0x00]),
            Step::Done(Resp::Raw(frame(0x03, &[0x64, 0x00])))
        );
    }

    #[test]
    fn the_typed_replies_become_their_resp() {
        let mut txn = started(Req::ReadGoals).txn;
        assert_eq!(
            feed(
                &mut txn,
                0x21,
                &[
                    0x01, 0x88, 0x13, 0x00, 0xe0, 0x93, 0x04, 0xb8, 0x0b, 0x00, 0x00, 0x00, 0x00
                ]
            ),
            Step::Done(Resp::Goals {
                steps: 5000,
                calories: 300_000,
                distance: 3000,
                sport_min: 0,
                sleep_min: 0,
            })
        );

        let mut txn = started(Req::ReadAutoHrPref).txn;
        // The SpO2 preference shares the shape but not the command.
        assert_eq!(feed(&mut txn, 0x2c, &[0x01, 0x01, 0x1e]), Step::Ignored);
        assert_eq!(
            feed(&mut txn, 0x16, &[0x01, 0x01, 0x05, 0x05]),
            Step::Done(Resp::AutoPref {
                enabled: true,
                interval_min: Some(5)
            })
        );
        let mut txn = started(Req::ReadAutoSpo2Pref).txn;
        assert_eq!(
            feed(&mut txn, 0x2c, &[0x01, 0x01, 0x1e]),
            Step::Done(Resp::AutoPref {
                enabled: true,
                interval_min: Some(30)
            })
        );
        let mut txn = started(Req::ReadAutoStressPref).txn;
        assert_eq!(
            feed(&mut txn, 0x36, &[0x01, 0x01]),
            Step::Done(Resp::AutoPref {
                enabled: true,
                interval_min: None
            })
        );
        let mut txn = started(Req::ReadAutoHrvPref).txn;
        assert_eq!(
            feed(&mut txn, 0x38, &[0x01, 0x00]),
            Step::Done(Resp::AutoPref {
                enabled: false,
                interval_min: None
            })
        );

        let mut txn = started(Req::TodayTotals).txn;
        assert_eq!(
            feed(
                &mut txn,
                0x48,
                &[
                    0x00, 0x05, 0x79, 0x00, 0x00, 0x00, 0x00, 0xf7, 0xaf, 0x00, 0x04, 0x34, 0x00,
                    0x29
                ]
            ),
            Step::Done(Resp::TodayTotals {
                steps: 1401,
                running_steps: 0,
                cal: 63407,
                distance_m: 1076,
                active_min: 41,
            })
        );

        let mut txn = started(Req::WorkoutCtl {
            action: WorkoutAction::Start,
            sport_type: 7,
        })
        .txn;
        assert_eq!(
            feed(&mut txn, 0x77, &[0x01, 0x00, 0x80, 0x29, 0x1d, 0x69]),
            Step::Done(Resp::WorkoutCtl {
                start: Some(0x691d_2980)
            })
        );
        let mut txn = started(Req::WorkoutCtl {
            action: WorkoutAction::Stop,
            sport_type: 7,
        })
        .txn;
        assert_eq!(
            feed(&mut txn, 0x77, &[0x00]),
            Step::Done(Resp::WorkoutCtl { start: None })
        );
    }

    #[test]
    fn version_is_the_ack_then_two_reads() {
        let mut txn = started(Req::Version).txn;
        assert_eq!(feed(&mut txn, 0x03, &[0x64, 0x00]), Step::Ignored);
        // A value before the ack is not the transaction's.
        assert_eq!(
            txn.feed_text(DIS_FW, "RT03CR_1.00.02_260319"),
            Step::Ignored
        );
        assert_eq!(
            feed(&mut txn, 0x19, &[0x01, 0x00, 0x01]),
            Step::Read(DIS_FW)
        );
        assert_eq!(
            txn,
            Txn::Version {
                stage: VersionStage::Firmware
            }
        );
        // A second ack, and the value of the other channel, are not what is waited for.
        assert_eq!(feed(&mut txn, 0x19, &[0x01, 0x00, 0x01]), Step::Ignored);
        assert_eq!(txn.feed_text(DIS_HW, "RT03CR_V1.0"), Step::Ignored);
        assert_eq!(
            txn.feed_text(DIS_FW, "RT03CR_1.00.02_260319"),
            Step::Read(DIS_HW)
        );
        assert_eq!(
            txn,
            Txn::Version {
                stage: VersionStage::Hardware {
                    firmware: String::from("RT03CR_1.00.02_260319")
                }
            }
        );
        assert_eq!(txn.feed_text(DIS_FW, "again"), Step::Ignored);
        assert_eq!(
            txn.feed_text(DIS_HW, "RT03CR_V1.0"),
            Step::Done(Resp::Version {
                firmware: String::from("RT03CR_1.00.02_260319"),
                hardware: String::from("RT03CR_V1.0"),
            })
        );

        // Text means nothing to the other transactions, nor does a battery reply to the
        // ones that are not waiting for one.
        let mut txn = started(Req::Battery).txn;
        assert_eq!(txn.feed_text(DIS_FW, "RT03CR_V1.0"), Step::Ignored);
        for mut txn in [file_list_txn(), hr_log(), stress(), activity()] {
            assert_eq!(
                txn.feed_text(DIS_HW, "RT03CR_V1.0"),
                Step::Ignored,
                "{txn:?}"
            );
            assert_eq!(
                feed(&mut txn, 0x03, &[0x64, 0x00]),
                Step::Ignored,
                "{txn:?}"
            );
        }
    }

    #[test]
    fn hr_log_collects_the_packets_in_order() {
        let mut txn = hr_log();
        // Frames of other commands, and a series packet, are not its business.
        assert_eq!(feed(&mut txn, 0x37, &[0x00, 0x05, 0x1e]), Step::Ignored);
        assert_eq!(feed(&mut txn, 0x15, &[0x00, 0x18, 0x05]), Step::Continue);
        for packet in HR_PACKETS {
            assert_eq!(feed(&mut txn, 0x15, packet), Step::Continue);
        }
        for index in 4..=0x16 {
            assert_eq!(feed(&mut txn, 0x15, &[index]), Step::Continue);
        }
        let Step::Done(Resp::HrLog(samples)) = feed(&mut txn, 0x15, &[0x17]) else {
            panic!("{txn:?}");
        };
        assert_eq!(samples.len(), 35);
        assert_eq!(
            samples[0],
            HrSample {
                at: at(0),
                bpm: Bpm::Valid(63),
                source: Source::Periodic
            }
        );
        assert_eq!(
            samples[29],
            HrSample {
                at: at(29 * 5),
                bpm: Bpm::Valid(83),
                source: Source::Periodic
            }
        );
        assert!(samples.iter().all(|s| s.at.utc_offset_min == -240));

        // The shortest log: a count of 2 completes on the first packet.
        let mut txn = hr_log();
        assert_eq!(feed(&mut txn, 0x15, &[0x00, 0x02, 0x05]), Step::Continue);
        let Step::Done(Resp::HrLog(samples)) = feed(&mut txn, 0x15, HR_PACKETS[0]) else {
            panic!("{txn:?}");
        };
        assert_eq!(samples.len(), 9);

        // No data for the day.
        let mut txn = hr_log();
        assert_eq!(
            feed(&mut txn, 0x15, &[0xff]),
            Step::Done(Resp::HrLog(Vec::new()))
        );
    }

    #[test]
    fn hr_log_refuses_a_wrong_day_a_bad_count_and_disorder() {
        let mut txn = hr_log();
        assert_eq!(feed(&mut txn, 0x15, &[0x00, 0x18, 0x05]), Step::Continue);
        let mut wrong_day = HR_PACKETS[0].to_vec();
        wrong_day[1] ^= 0x01;
        assert_eq!(
            feed(&mut txn, 0x15, &wrong_day),
            malformed("hr log: day mismatch")
        );

        for count in [0x00, 0x01] {
            let mut txn = hr_log();
            assert_eq!(
                feed(&mut txn, 0x15, &[0x00, count, 0x05]),
                malformed("hr log: count")
            );
        }

        let disorder = |frames: &[&[u8]]| {
            let mut txn = hr_log();
            let mut last = Step::Ignored;
            for payload in frames {
                last = feed(&mut txn, 0x15, payload);
            }
            last
        };
        let header: &[u8] = &[0x00, 0x18, 0x05];
        // A packet before the header, of either shape.
        assert_eq!(
            disorder(&[HR_PACKETS[0]]),
            malformed("hr log: out of order")
        );
        assert_eq!(
            disorder(&[HR_PACKETS[1]]),
            malformed("hr log: out of order")
        );
        // A second header.
        assert_eq!(
            disorder(&[header, header]),
            malformed("hr log: out of order")
        );
        // Skipping, repeating, and going back.
        assert_eq!(
            disorder(&[header, HR_PACKETS[1]]),
            malformed("hr log: out of order")
        );
        assert_eq!(
            disorder(&[header, HR_PACKETS[0], HR_PACKETS[2]]),
            malformed("hr log: out of order")
        );
        assert_eq!(
            disorder(&[header, HR_PACKETS[0], HR_PACKETS[0]]),
            malformed("hr log: out of order")
        );
        assert_eq!(
            disorder(&[header, HR_PACKETS[0], HR_PACKETS[1], HR_PACKETS[0]]),
            malformed("hr log: out of order")
        );
        // "No data" after the header is not an answer.
        assert_eq!(
            disorder(&[header, &[0xff]]),
            malformed("hr log: out of order")
        );
    }

    #[test]
    fn series_collect_the_packets_in_order() {
        let mut txn = stress();
        // An HRV packet has the same shape and another command.
        assert_eq!(feed(&mut txn, 0x39, &[0x00, 0x05, 0x1e]), Step::Ignored);
        assert_eq!(feed(&mut txn, 0x15, &[0x00, 0x18, 0x05]), Step::Ignored);
        assert_eq!(feed(&mut txn, 0x37, &[0x00, 0x05, 0x1e]), Step::Continue);
        assert_eq!(
            feed(
                &mut txn,
                0x37,
                &[
                    0x01, 0x00, 0x2b, 0x27, 0x24, 0x22, 0x2f, 0x2e, 0x2a, 0x23, 0x22, 0x27, 0x24,
                    0x21
                ]
            ),
            Step::Continue
        );
        assert_eq!(
            feed(
                &mut txn,
                0x37,
                &[
                    0x02, 0x2d, 0x31, 0x2c, 0x20, 0x31, 0x2f, 0x2f, 0x28, 0x23, 0x27, 0x2e, 0x28,
                    0x2e
                ]
            ),
            Step::Continue
        );
        assert_eq!(
            feed(&mut txn, 0x37, &[0x03, 0x31, 0x2d, 0x29, 0x2e]),
            Step::Continue
        );
        let Step::Done(Resp::Stress(samples)) = feed(&mut txn, 0x37, &[0x04]) else {
            panic!("{txn:?}");
        };
        assert_eq!(samples.len(), 29);
        assert_eq!(
            samples[0],
            StressSample {
                at: at(0),
                value: 43
            }
        );
        assert_eq!(
            samples[28],
            StressSample {
                at: at(28 * 30),
                value: 46
            }
        );

        let mut txn = started(Req::Hrv {
            days_ago: 1,
            today: FIXTURE_TIME,
        })
        .txn;
        assert_eq!(feed(&mut txn, 0x39, &[0x00, 0x02, 0x1e]), Step::Continue);
        let Step::Done(Resp::Hrv(samples)) = feed(
            &mut txn,
            0x39,
            &[
                0x01, 0x01, 0x1e, 0x00, 0x2b, 0x00, 0x31, 0x00, 0x2d, 0x00, 0x2a, 0x00, 0x1f, 0x00,
            ],
        ) else {
            panic!("{txn:?}");
        };
        assert_eq!(samples.len(), 6);
        assert_eq!(
            samples[0],
            HrvSample {
                at: at(-MINUTES_PER_DAY),
                value: 30
            }
        );
        assert_eq!(
            samples[1],
            HrvSample {
                at: at(-MINUTES_PER_DAY + 60),
                value: 43
            }
        );

        for (mut txn, empty) in [
            (stress(), Resp::Stress(Vec::new())),
            (
                started(Req::Hrv {
                    days_ago: 0,
                    today: FIXTURE_TIME,
                })
                .txn,
                Resp::Hrv(Vec::new()),
            ),
        ] {
            let cmd = match empty {
                Resp::Stress(_) => 0x37,
                _ => 0x39,
            };
            assert_eq!(feed(&mut txn, cmd, &[0xff]), Step::Done(empty));
        }
    }

    #[test]
    fn series_refuse_a_wrong_day_a_bad_count_and_disorder() {
        let header: &[u8] = &[0x00, 0x05, 0x1e];
        let first: &[u8] = &[0x01, 0x00, 0x2b];
        let second: &[u8] = &[0x02, 0x2d];
        let run = |frames: &[&[u8]]| {
            let mut txn = stress();
            let mut last = Step::Ignored;
            for payload in frames {
                last = feed(&mut txn, 0x37, payload);
            }
            last
        };
        assert_eq!(
            run(&[header, &[0x01, 0x01, 0x2b]]),
            malformed("series: day mismatch")
        );
        assert_eq!(run(&[&[0x00, 0x01, 0x1e]]), malformed("series: count"));
        assert_eq!(run(&[&[0x00, 0x00, 0x1e]]), malformed("series: count"));
        assert_eq!(run(&[first]), malformed("series: out of order"));
        assert_eq!(run(&[header, header]), malformed("series: out of order"));
        assert_eq!(run(&[header, second]), malformed("series: out of order"));
        assert_eq!(
            run(&[header, first, first]),
            malformed("series: out of order")
        );
        assert_eq!(
            run(&[header, first, &[0x03]]),
            malformed("series: out of order")
        );
        assert_eq!(run(&[header, &[0xff]]), malformed("series: out of order"));
    }

    /// The fixture's five activity rows, after the command byte.
    const ROWS: [&[u8]; 5] = [
        &[
            0x26, 0x07, 0x02, 0x04, 0x00, 0x05, 0x70, 0x00, 0x1c, 0x00, 0x13, 0x00,
        ],
        &[
            0x26, 0x07, 0x02, 0x2c, 0x01, 0x05, 0x0c, 0x00, 0x03, 0x00, 0x02, 0x00,
        ],
        &[
            0x26, 0x07, 0x02, 0x30, 0x02, 0x05, 0x50, 0x0c, 0xdc, 0x02, 0x18, 0x02,
        ],
        &[
            0x26, 0x07, 0x02, 0x34, 0x03, 0x05, 0xa8, 0x06, 0x73, 0x01, 0x21, 0x01,
        ],
        &[
            0x26, 0x07, 0x02, 0x38, 0x04, 0x05, 0x50, 0x05, 0x0b, 0x01, 0xe6, 0x00,
        ],
    ];

    #[test]
    fn activity_collects_the_rows_in_order() {
        let mut txn = activity();
        assert_eq!(feed(&mut txn, 0x48, &[0x00]), Step::Ignored);
        assert_eq!(feed(&mut txn, 0x43, &[0xf0, 0x05, 0x01]), Step::Continue);
        for row in &ROWS[..4] {
            assert_eq!(feed(&mut txn, 0x43, row), Step::Continue);
        }
        let Step::Done(Resp::Activity(buckets)) = feed(&mut txn, 0x43, ROWS[4]) else {
            panic!("{txn:?}");
        };
        assert_eq!(buckets.len(), 5);
        assert_eq!(
            buckets[0],
            StepBucket {
                start: at(60),
                span: junk_core::Duration::from_secs(3600),
                steps: 28,
                cal: 1120,
                distance_m: 19,
            }
        );
        let hours: Vec<i64> = buckets
            .iter()
            .map(|b| (b.start.local_minute - FIXTURE_DAY.local_minute) / 60)
            .collect();
        assert_eq!(hours, [1, 11, 12, 13, 14]);
        assert!(buckets.iter().all(|b| b.start.utc_offset_min == -240));

        // Without the new protocol the calories are as sent.
        let mut txn = activity();
        assert_eq!(feed(&mut txn, 0x43, &[0xf0, 0x01, 0x00]), Step::Continue);
        let mut only = ROWS[0].to_vec();
        only[5] = 0x01;
        let Step::Done(Resp::Activity(buckets)) = feed(&mut txn, 0x43, &only) else {
            panic!("{txn:?}");
        };
        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets[0].cal, 112);

        // No data for the day, with either third byte.
        for unknown in [0x00, 0x01] {
            let mut txn = activity();
            assert_eq!(
                feed(&mut txn, 0x43, &[0xff, 0x00, unknown]),
                Step::Done(Resp::Activity(Vec::new()))
            );
        }
    }

    #[test]
    fn activity_refuses_rows_before_the_header_and_out_of_order() {
        let header: &[u8] = &[0xf0, 0x05, 0x01];
        let run = |frames: &[&[u8]]| {
            let mut txn = activity();
            let mut last = Step::Ignored;
            for payload in frames {
                last = feed(&mut txn, 0x43, payload);
            }
            last
        };
        assert_eq!(run(&[ROWS[0]]), malformed("activity: row before header"));
        assert_eq!(run(&[header, ROWS[1]]), malformed("activity: out of order"));
        assert_eq!(
            run(&[header, ROWS[0], ROWS[0]]),
            malformed("activity: out of order")
        );
        assert_eq!(
            run(&[header, ROWS[0], ROWS[2]]),
            malformed("activity: out of order")
        );
        assert_eq!(run(&[header, header]), malformed("activity: out of order"));
        assert_eq!(
            run(&[header, &[0xff, 0x00, 0x00]]),
            malformed("activity: out of order")
        );
        // A total of zero never completes on a row; the timer will end it.
        let mut zero = ROWS[0].to_vec();
        zero[5] = 0x00;
        assert_eq!(run(&[header, &zero]), Step::Continue);
    }

    /// The file-list reply of the `QRing` fixture.
    const FILE_LIST: [u8; 7] = [0xbc, 0x30, 0x01, 0x00, 0xbf, 0x40, 0x00];

    fn file_list_txn() -> Txn {
        started(Req::RawBigData {
            kind: BigDataKind::FileList,
            body: vec![],
        })
        .txn
    }

    #[test]
    fn big_data_is_collected_across_notifications() {
        let mut txn = file_list_txn();
        assert_eq!(
            txn.collect_v2(&FILE_LIST),
            Collected::Frame(big(BigDataKind::FileList, &[0x00]))
        );

        let mut txn = file_list_txn();
        assert_eq!(txn.collect_v2(&FILE_LIST[..4]), Collected::Incomplete);
        assert_eq!(txn.collect_v2(&[]), Collected::Incomplete);
        assert_eq!(
            txn.collect_v2(&FILE_LIST[4..]),
            Collected::Frame(big(BigDataKind::FileList, &[0x00]))
        );
        // The collector is empty again.
        assert_eq!(txn, file_list_txn());

        let mut txn = file_list_txn();
        assert_eq!(txn.collect_v2(&FILE_LIST[..1]), Collected::Incomplete);
        assert_eq!(txn.collect_v2(&FILE_LIST[1..3]), Collected::Incomplete);
        assert_eq!(txn.collect_v2(&FILE_LIST[3..6]), Collected::Incomplete);
        assert_eq!(
            txn.collect_v2(&FILE_LIST[6..]),
            Collected::Frame(big(BigDataKind::FileList, &[0x00]))
        );

        // Every big-data transaction collects the same way.
        let mut txn = detail();
        let descriptor = hex(DESCRIPTOR);
        assert_eq!(txn.collect_v2(&descriptor[..5]), Collected::Incomplete);
        assert_eq!(
            txn.collect_v2(&descriptor[5..]),
            Collected::Frame(parsed(DESCRIPTOR))
        );
    }

    #[test]
    fn big_data_that_cannot_be_a_frame_is_rejected_or_not_taken() {
        let mut txn = file_list_txn();
        let mut bad_crc = FILE_LIST;
        bad_crc[4] ^= 0x01;
        assert_eq!(
            txn.collect_v2(&bad_crc),
            Collected::Fail(ProtoError::Checksum)
        );
        assert_eq!(txn, file_list_txn());

        let mut over = FILE_LIST.to_vec();
        over.push(0x00);
        assert_eq!(
            txn.collect_v2(&over),
            Collected::Fail(ProtoError::Malformed(
                "big data: more bytes than the header promised"
            ))
        );
        assert_eq!(txn, file_list_txn());

        // Not the start of a frame while nothing is collected: not ours.
        assert_eq!(txn.collect_v2(&[0x00, 0x01]), Collected::NotWanted);
        assert_eq!(txn.collect_v2(&[0xbf, 0x40, 0x00]), Collected::NotWanted);
        assert_eq!(txn, file_list_txn());
        // But once under way, whatever comes is part of the frame.
        assert_eq!(txn.collect_v2(&FILE_LIST[..4]), Collected::Incomplete);
        assert_eq!(
            txn.collect_v2(&[0x00, 0x00, 0x00]),
            Collected::Fail(ProtoError::Checksum)
        );

        for mut txn in [
            started(Req::Battery).txn,
            started(Req::Version).txn,
            hr_log(),
            stress(),
            activity(),
        ] {
            assert_eq!(txn.collect_v2(&FILE_LIST), Collected::NotWanted, "{txn:?}");
        }
    }

    #[test]
    fn a_complete_frame_must_be_the_kind_asked_for() {
        let mut txn = file_list_txn();
        let spo2 = big(BigDataKind::Spo2, &[0x00]);
        assert_eq!(txn.feed_big(&spo2), Step::Ignored);
        let list = big(BigDataKind::FileList, &[0x00]);
        assert_eq!(
            txn.feed_big(&list),
            Step::Done(Resp::RawBigData(list.clone()))
        );
        for mut txn in [
            started(Req::Battery).txn,
            started(Req::Version).txn,
            hr_log(),
            stress(),
            activity(),
        ] {
            assert_eq!(txn.feed_big(&list), Step::Ignored, "{txn:?}");
        }

        // A raw request takes its kind whatever the body; a typed one decodes it.
        let mut raw = started(Req::RawBigData {
            kind: BigDataKind::Spo2,
            body: vec![0xff],
        })
        .txn;
        assert_eq!(
            raw.feed_big(&spo2),
            Step::Done(Resp::RawBigData(spo2.clone()))
        );
        let mut typed = started(Req::Spo2 {
            today: FIXTURE_TIME,
            selector: 0xff,
        })
        .txn;
        assert_eq!(typed.feed_big(&list), Step::Ignored);
        assert_eq!(typed.feed_big(&spo2), malformed("day blocks"));
    }

    #[test]
    fn big_wants_name_their_kinds() {
        let raw = BigWant::Raw {
            kind: BigDataKind::Other(0x47),
        };
        assert!(raw.wants(BigDataKind::Other(0x47)));
        assert!(!raw.wants(BigDataKind::Other(0x48)));
        assert!(!raw.wants(BigDataKind::Sleep));
        let today = FIXTURE_TIME;
        assert!(BigWant::Sleep { today }.wants(BigDataKind::Sleep));
        assert!(!BigWant::Sleep { today }.wants(BigDataKind::Spo2));
        assert!(BigWant::Spo2 { today }.wants(BigDataKind::Spo2));
        assert!(BigWant::Temperature { today }.wants(BigDataKind::Temperature));
        assert!(!BigWant::Temperature { today }.wants(BigDataKind::Sleep));
        assert!(BigWant::WorkoutList.wants(BigDataKind::WorkoutSummary));
        assert!(!BigWant::WorkoutList.wants(BigDataKind::WorkoutList));
        let Txn::BigData { want, .. } = detail() else {
            panic!("not big data");
        };
        assert!(want.wants(BigDataKind::WorkoutDescriptor));
        assert!(!want.wants(BigDataKind::WorkoutSeries));
        assert!(!want.wants(BigDataKind::WorkoutDetail));
    }

    #[test]
    fn typed_big_data_becomes_samples() {
        let mut txn = started(Req::Sleep {
            today: FIXTURE_TIME,
            selector: vec![0x06, 0x01],
        })
        .txn;
        let Step::Done(Resp::Sleep(sessions)) = txn.feed_big(&parsed(SLEEP)) else {
            panic!("{txn:?}");
        };
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].start, at(3 * 60 + 7));
        assert_eq!(sessions[0].end, at(11 * 60 + 49));
        assert_eq!(sessions[0].stages.len(), 21);

        let mut txn = started(Req::Spo2 {
            today: FIXTURE_TIME,
            selector: 0x02,
        })
        .txn;
        let mut block = vec![0x00, 0x62, 0x62];
        block.resize(49, 0);
        let Step::Done(Resp::Spo2(samples)) = txn.feed_big(&big(BigDataKind::Spo2, &block)) else {
            panic!("{txn:?}");
        };
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].at, at(0));
        assert_eq!(samples[0].percent.get(), 98);

        let mut txn = started(Req::Temperature {
            today: FIXTURE_TIME,
            selector: 0x00,
        })
        .txn;
        let mut block = vec![0x01, 0x1e, 0x00, 0xa7];
        block.resize(50, 0);
        let Step::Done(Resp::Temperature(samples)) =
            txn.feed_big(&big(BigDataKind::Temperature, &block))
        else {
            panic!("{txn:?}");
        };
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].at, at(-MINUTES_PER_DAY + 30));
        assert_eq!(samples[0].deci_celsius, 367);

        // A body that does not fit its kind fails with the body layer's words.
        let mut txn = started(Req::Sleep {
            today: FIXTURE_TIME,
            selector: vec![0x01, 0x01],
        })
        .txn;
        assert_eq!(
            txn.feed_big(&big(BigDataKind::Sleep, &[0x02, 0x00])),
            malformed("day block")
        );
    }

    #[test]
    fn workout_list_is_the_summary() {
        let mut txn = started(Req::WorkoutList { since: 0 }).txn;
        assert_eq!(txn.feed_big(&parsed(DESCRIPTOR)), Step::Ignored);
        let Step::Done(Resp::Workouts(summary)) = txn.feed_big(&parsed(SUMMARY)) else {
            panic!("{txn:?}");
        };
        assert_eq!(summary.records.len(), 1);
        assert_eq!(summary.records[0].sport_type, 7);
        assert_eq!(
            summary.records[0].get(WorkoutTag::StartTime),
            Some(0x691d_2980)
        );
        assert_eq!(summary.records[0].get(WorkoutTag::Duration), Some(61));
        assert_eq!(
            txn.feed_big(&big(BigDataKind::WorkoutSummary, &[])),
            malformed("record count")
        );
    }

    #[test]
    fn workout_detail_is_the_descriptor_then_its_packages() {
        let mut txn = detail();
        // The series before the descriptor is not wanted, and changes nothing.
        assert_eq!(txn.feed_big(&parsed(SERIES)), Step::Ignored);
        assert_eq!(txn, detail());
        assert_eq!(txn.feed_big(&parsed(DESCRIPTOR)), Step::Continue);
        // Now the descriptor is not wanted, and neither is the summary.
        assert_eq!(txn.feed_big(&parsed(DESCRIPTOR)), Step::Ignored);
        assert_eq!(txn.feed_big(&parsed(SUMMARY)), Step::Ignored);
        let Step::Done(Resp::WorkoutDetail {
            descriptor,
            heart_rates,
        }) = txn.feed_big(&parsed(SERIES))
        else {
            panic!("{txn:?}");
        };
        assert_eq!(descriptor.package_count, 1);
        assert_eq!(descriptor.sample_second, 1);
        assert_eq!(heart_rates.len(), 54);
        assert!(
            heart_rates
                .iter()
                .all(|bpm| matches!(bpm, Bpm::Valid(57..=62)))
        );
    }

    /// A descriptor like the fixture's with `package_count` packages and `status`.
    fn descriptor(status: u8, package_count: u8) -> BigData {
        ReplyBody::WorkoutDescriptor(WorkoutDescriptor {
            status,
            package_count,
            byte2: 0,
            sample_second: 1,
            fields: vec![DescriptorField {
                len: 1,
                tag: SampleTag::HeartRate,
            }],
        })
        .encode()
        .unwrap_or_else(|err| panic!("{err}"))
    }

    fn package(number: u8, data: &[u8]) -> BigData {
        ReplyBody::WorkoutSeries(WorkoutSeries {
            package: number,
            byte1: 0,
            data: data.to_vec(),
        })
        .encode()
        .unwrap_or_else(|err| panic!("{err}"))
    }

    #[test]
    fn workout_packages_run_from_one_in_order() {
        let mut txn = detail();
        assert_eq!(txn.feed_big(&descriptor(0, 3)), Step::Continue);
        assert_eq!(txn.feed_big(&package(1, &[60, 61])), Step::Continue);
        assert_eq!(
            txn.feed_big(&package(3, &[62])),
            malformed("workout: package order")
        );

        let mut txn = detail();
        assert_eq!(txn.feed_big(&descriptor(0, 3)), Step::Continue);
        assert_eq!(
            txn.feed_big(&package(2, &[62])),
            malformed("workout: package order")
        );

        let mut txn = detail();
        assert_eq!(txn.feed_big(&descriptor(0, 3)), Step::Continue);
        assert_eq!(txn.feed_big(&package(1, &[60, 61])), Step::Continue);
        assert_eq!(txn.feed_big(&package(2, &[])), Step::Continue);
        let Step::Done(Resp::WorkoutDetail { heart_rates, .. }) =
            txn.feed_big(&package(3, &[62, 63, 64]))
        else {
            panic!("{txn:?}");
        };
        assert_eq!(heart_rates, [60, 61, 62, 63, 64].map(Bpm::Valid));

        // No packages: done on the descriptor, with nothing.
        let mut txn = detail();
        let Step::Done(Resp::WorkoutDetail {
            descriptor: bare,
            heart_rates,
        }) = txn.feed_big(&descriptor(0, 0))
        else {
            panic!("{txn:?}");
        };
        assert_eq!(bare.package_count, 0);
        assert!(heart_rates.is_empty());

        // The most packages a descriptor can announce.
        let mut txn = detail();
        assert_eq!(txn.feed_big(&descriptor(0, u8::MAX)), Step::Continue);
        for number in 1..u8::MAX {
            assert_eq!(txn.feed_big(&package(number, &[number])), Step::Continue);
        }
        let Step::Done(Resp::WorkoutDetail { heart_rates, .. }) =
            txn.feed_big(&package(u8::MAX, &[u8::MAX]))
        else {
            panic!("{txn:?}");
        };
        assert_eq!(heart_rates.len(), 255);
    }

    #[test]
    fn workout_detail_refuses_a_bad_status_and_a_descriptor_without_heart_rate() {
        let mut txn = detail();
        assert_eq!(
            txn.feed_big(&descriptor(1, 1)),
            malformed("workout: status")
        );

        let mut txn = detail();
        let no_rate = ReplyBody::WorkoutDescriptor(WorkoutDescriptor {
            status: 0,
            package_count: 1,
            byte2: 0,
            sample_second: 1,
            fields: vec![DescriptorField {
                len: 2,
                tag: SampleTag::Other(15),
            }],
        })
        .encode()
        .unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(txn.feed_big(&no_rate), Step::Continue);
        assert_eq!(
            txn.feed_big(&package(1, &[1, 2])),
            malformed("workout: no heart rate field")
        );

        let mut txn = detail();
        assert_eq!(
            txn.feed_big(&big(BigDataKind::WorkoutDescriptor, &[0x00, 0x01])),
            malformed("header")
        );
    }

    /// The profile the `QRing` fixture writes.
    fn fixture_prefs() -> Prefs {
        Prefs {
            hour12: false,
            imperial: false,
            sex: 0,
            age: 30,
            height_cm: 175,
            weight_kg: 70,
            sbp: 120,
            dbp: 80,
            hr_warn: 150,
        }
    }
}
