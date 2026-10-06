//! Replaying the fixtures: the [`Adapter`] that lets [`junk_trace::replay()`] drive a
//! [`ColmiDriver`] from a trace under `fixtures/colmi-r10`.
//!
//! The harness feeds every `rx` line to the driver and asks this adapter what request each
//! `tx` line was, so the driver is made to write it again (SPEC §5, Stage A). A write is
//! read back through the wire layer, [`HostFrame`] on `v1.write` and [`RequestBody`] on
//! `v2.cmd`, and mapped to the [`Req`] whose transaction writes that frame. What the frames
//! do not carry, the zone and "today", comes from the trace's stamps: the ring is written
//! the local wall clock, so a stamp's date is the ring's date.

use alloc::vec::Vec;

use junk_core::{Channel, ChannelSet, Timestamp};
use junk_trace::{Adapter, DataLine, Trace};

use crate::proto::{ColmiDriver, MINUTES_PER_DAY, Req, days_from_civil};
use crate::wire::{BigData, BigDataKind, Frame, HostFrame, RequestBody};
use crate::{DIS_FW, DIS_HW, V1_NOTIFY, V1_WRITE, V2_CMD, V2_NOTIFY, channel_by_name};

/// The ATT MTU the fixtures were captured at (`didConnect … mtu = 247` in the `QRing` log).
const MTU: u16 = 247;
/// Seconds per minute: the HR log request counts seconds, a [`Timestamp`] minutes.
const SECONDS_PER_MINUTE: i64 = 60;

/// The [`Adapter`] for Colmi traces.
///
/// It maps the six channel names the traces use, reports a link with all six channels
/// resolved, and turns each write into its [`Req`]. The requests that place a day get
/// `today` (the date of the trace's first line at midnight) and its zone; a
/// [`HostFrame::SetTime`] or [`HostFrame::HrLog`] carries its own date and gets the zone
/// only.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct ColmiAdapter {
    /// The day the trace was captured, in the zone the ring's clock was set to. Requests
    /// that place days get it as is; the driver takes its midnight.
    today: Timestamp,
}

impl ColmiAdapter {
    /// An adapter for `trace`: `today` is midnight of the first data line's date, in the
    /// offset its stamp carries, or `fallback_offset_min` if the stamps carry none (the
    /// `QRing` log's do not). `None` if the trace has no data lines.
    #[must_use]
    pub fn for_trace(trace: &Trace, fallback_offset_min: i16) -> Option<ColmiAdapter> {
        let first = trace.data().next()?;
        let at = first.at;
        let utc_offset_min = at.utc_offset_min().unwrap_or(fallback_offset_min);
        Some(ColmiAdapter::new(Timestamp {
            local_minute: days_from_civil(i64::from(at.year()), at.month(), at.day())
                .saturating_mul(MINUTES_PER_DAY),
            utc_offset_min,
        }))
    }

    /// An adapter whose requests place days from `today` and use its zone.
    #[must_use]
    pub const fn new(today: Timestamp) -> ColmiAdapter {
        ColmiAdapter { today }
    }

    /// The day the requests count from.
    #[must_use]
    pub const fn today(&self) -> Timestamp {
        self.today
    }

    /// `minute_of_day` minutes into `year-month-day`, in the adapter's zone.
    fn local(&self, year: i64, month: u8, day: u8, minute_of_day: i64) -> Timestamp {
        Timestamp {
            local_minute: days_from_civil(year, month, day)
                .saturating_mul(MINUTES_PER_DAY)
                .saturating_add(minute_of_day),
            utc_offset_min: self.today.utc_offset_min,
        }
    }

    /// The request that writes `host`, which came as `frame`.
    fn v1_request(&self, frame: &Frame, host: HostFrame) -> Req {
        match host {
            HostFrame::SetTime {
                year,
                month,
                day,
                hour,
                minute,
                second,
                lang,
            } => Req::SetTime {
                at: self.local(
                    i64::from(year),
                    month,
                    day,
                    i64::from(hour) * 60 + i64::from(minute),
                ),
                second,
                lang,
            },
            HostFrame::Battery => Req::Battery,
            HostFrame::PhoneName {
                platform,
                os_version,
                name,
            } => Req::PhoneName {
                platform,
                os_version,
                name,
            },
            HostFrame::ReadPrefs => Req::ReadPrefs,
            HostFrame::WritePrefs(prefs) => Req::WritePrefs(prefs),
            HostFrame::HrLog { day_start } => Req::HrLog {
                day_start: Timestamp {
                    local_minute: i64::from(day_start) / SECONDS_PER_MINUTE,
                    utc_offset_min: self.today.utc_offset_min,
                },
            },
            HostFrame::ReadAutoHrPref => Req::ReadAutoHrPref,
            HostFrame::Version => Req::Version,
            HostFrame::ReadGoals => Req::ReadGoals,
            HostFrame::ReadAutoSpo2Pref => Req::ReadAutoSpo2Pref,
            HostFrame::ReadAutoStressPref => Req::ReadAutoStressPref,
            HostFrame::StressLog { days_ago } => Req::Stress {
                days_ago,
                today: self.today,
            },
            HostFrame::ReadAutoHrvPref => Req::ReadAutoHrvPref,
            HostFrame::HrvLog { days_ago } => Req::Hrv {
                days_ago,
                today: self.today,
            },
            HostFrame::Activity {
                days_ago,
                first_bucket,
                last_bucket,
            } => Req::Activity {
                days_ago,
                today: self.today,
                first_bucket,
                last_bucket,
            },
            HostFrame::TodayTotals => Req::TodayTotals,
            HostFrame::WorkoutCtl { action, sport_type } => Req::WorkoutCtl { action, sport_type },
            HostFrame::ManualHrStart { kind, sub } => Req::ManualHrStart { kind, sub },
            HostFrame::ManualHrStop { kind } => Req::ManualHrStop { kind },
            // No request asks for these by name; sent as they came.
            HostFrame::FindDevice | HostFrame::Raw { .. } => Req::Raw {
                cmd: frame.cmd,
                body: frame.body,
            },
        }
    }

    /// The request that writes `body`.
    fn v2_request(&self, body: RequestBody) -> Req {
        match body {
            RequestBody::Sleep(selector) => Req::Sleep {
                today: self.today,
                selector,
            },
            RequestBody::Spo2(selector) => Req::Spo2 {
                today: self.today,
                selector,
            },
            RequestBody::Temperature(selector) => Req::Temperature {
                today: self.today,
                selector,
            },
            RequestBody::FileList => Req::RawBigData {
                kind: BigDataKind::FileList,
                body: Vec::new(),
            },
            RequestBody::WorkoutList { since } => Req::WorkoutList { since },
            RequestBody::WorkoutDetail { sport_type, start } => {
                Req::WorkoutDetail { sport_type, start }
            }
            RequestBody::Raw { kind, body } => Req::RawBigData { kind, body },
        }
    }
}

impl Adapter for ColmiAdapter {
    type Driver = ColmiDriver;

    fn channel(&self, name: &str) -> Option<Channel> {
        channel_by_name(name)
    }

    fn connected(&self) -> (ChannelSet, u16) {
        (
            [V1_WRITE, V1_NOTIFY, V2_CMD, V2_NOTIFY, DIS_FW, DIS_HW]
                .into_iter()
                .collect(),
            MTU,
        )
    }

    /// A `v1.write` line through [`Frame::parse`] and [`HostFrame::decode`], a `v2.cmd`
    /// line through [`BigData::parse`] and [`RequestBody::decode`]; `None` for a line on
    /// any other channel or one that does not decode.
    fn request(&self, line: &DataLine) -> Option<Req> {
        match channel_by_name(&line.chan)? {
            V1_WRITE => {
                let frame = Frame::parse(&line.bytes).ok()?;
                let host = HostFrame::decode(&frame).ok()?;
                Some(self.v1_request(&frame, host))
            }
            V2_CMD => {
                let big = BigData::parse(&line.bytes).ok()?;
                let body = RequestBody::decode(&big).ok()?;
                Some(self.v2_request(body))
            }
            Channel(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    use junk_trace::{Direction, Stamp};

    use crate::proto::Txn;
    use crate::wire::Cmd;

    /// 2026-07-02 00:00 in UTC-4: the `QRing` fixture day.
    const FIXTURE_DAY: Timestamp = Timestamp {
        local_minute: 29_715_840,
        utc_offset_min: -240,
    };

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap_or_else(|err| panic!("{err}")))
            .collect()
    }

    fn line(chan: &str, text: &str) -> DataLine {
        DataLine {
            at: Stamp::parse("2026-07-02T14:16:30.474").unwrap_or_else(|err| panic!("{err}")),
            dir: Direction::Tx,
            chan: chan.into(),
            bytes: hex(text),
        }
    }

    fn adapter() -> ColmiAdapter {
        ColmiAdapter::new(FIXTURE_DAY)
    }

    /// The request for `text` on `chan`, and the write its transaction makes.
    fn round_trip(chan: &str, text: &str) -> (Req, Channel, Vec<u8>) {
        let req = adapter()
            .request(&line(chan, text))
            .unwrap_or_else(|| panic!("{chan} {text} has no request"));
        let started = Txn::start(req.clone()).unwrap_or_else(|err| panic!("{err}"));
        (req, started.chan, started.bytes)
    }

    #[test]
    fn for_trace_takes_the_first_lines_date_and_offset() {
        let no_offset = Trace::parse(
            "# header\n! 2026-07-01T23:59:59.999 earlier event\n2026-07-02T14:16:30.474 tx v1.write 0401\n",
        )
        .unwrap_or_else(|err| panic!("{err}"));
        let adapter = ColmiAdapter::for_trace(&no_offset, -240).unwrap_or_else(|| panic!("none"));
        assert_eq!(adapter.today(), FIXTURE_DAY);

        let with_offset = Trace::parse("2025-11-19T10:20:45.341-05:00 tx v1.write 7701\n")
            .unwrap_or_else(|err| panic!("{err}"));
        let adapter = ColmiAdapter::for_trace(&with_offset, 600).unwrap_or_else(|| panic!("none"));
        assert_eq!(
            adapter.today(),
            Timestamp {
                local_minute: days_from_civil(2025, 11, 19) * MINUTES_PER_DAY,
                utc_offset_min: -300,
            }
        );

        let empty = Trace::parse("# only a header\n").unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(ColmiAdapter::for_trace(&empty, 0), None);
    }

    #[test]
    fn connected_has_every_channel() {
        let (resolved, mtu) = adapter().connected();
        assert_eq!(resolved.len(), 6);
        assert_eq!(resolved, crate::GATT.chars().map(|c| c.id).collect());
        assert!(crate::GATT.satisfied_by(&resolved));
        assert_eq!(mtu, 247);
        assert_eq!(adapter().channel("v2.notify"), Some(V2_NOTIFY));
        assert_eq!(adapter().channel("dis.fw"), Some(DIS_FW));
        assert_eq!(adapter().channel("dis.hw"), Some(DIS_HW));
        assert_eq!(adapter().channel("v3.notify"), None);
    }

    #[test]
    fn set_time_and_hr_log_carry_their_own_date() {
        let (req, chan, bytes) = round_trip("v1.write", "0126070214163001000000000000008b");
        assert_eq!(
            req,
            Req::SetTime {
                at: Timestamp {
                    local_minute: FIXTURE_DAY.local_minute + 14 * 60 + 16,
                    utc_offset_min: -240,
                },
                second: 30,
                lang: 1,
            }
        );
        assert_eq!(chan, V1_WRITE);
        assert_eq!(bytes, hex("0126070214163001000000000000008b"));

        let (req, _, bytes) = round_trip("v1.write", "1500aa456a000000000000000000006e");
        assert_eq!(
            req,
            Req::HrLog {
                day_start: FIXTURE_DAY
            }
        );
        assert_eq!(bytes, hex("1500aa456a000000000000000000006e"));
    }

    #[test]
    fn the_day_requests_get_today() {
        let (req, ..) = round_trip("v1.write", "37000000000000000000000000000037");
        assert_eq!(
            req,
            Req::Stress {
                days_ago: 0,
                today: FIXTURE_DAY
            }
        );
        let (req, ..) = round_trip("v1.write", "39000000000000000000000000000039");
        assert_eq!(
            req,
            Req::Hrv {
                days_ago: 0,
                today: FIXTURE_DAY
            }
        );
        let (req, ..) = round_trip("v1.write", "431d0f015f01000000000000000000d0");
        assert_eq!(
            req,
            Req::Activity {
                days_ago: 29,
                today: FIXTURE_DAY,
                first_bucket: 1,
                last_bucket: 95,
            }
        );
        let (req, chan, bytes) = round_trip("v2.cmd", "bc270200c3d00601");
        assert_eq!(
            req,
            Req::Sleep {
                today: FIXTURE_DAY,
                selector: vec![0x06, 0x01],
            }
        );
        assert_eq!(chan, V2_CMD);
        assert_eq!(bytes, hex("bc270200c3d00601"));
        let (req, ..) = round_trip("v2.cmd", "bc2a0100ff00ff");
        assert_eq!(
            req,
            Req::Spo2 {
                today: FIXTURE_DAY,
                selector: 0xff,
            }
        );
        let (req, ..) = round_trip("v2.cmd", "bc250100bf4000");
        assert_eq!(
            req,
            Req::Temperature {
                today: FIXTURE_DAY,
                selector: 0,
            }
        );
    }

    #[test]
    fn unnamed_writes_are_raw() {
        let (req, _, bytes) = round_trip("v1.write", "3c00000000000000000000000000003c");
        assert_eq!(
            req,
            Req::Raw {
                cmd: Cmd::Other(0x3c),
                body: [0; 14],
            }
        );
        assert_eq!(bytes, hex("3c00000000000000000000000000003c"));

        let (req, _, bytes) = round_trip("v1.write", "5003aa000000000000000000000000fd");
        assert_eq!(
            req,
            Req::Raw {
                cmd: Cmd::FindDevice,
                body: [0x03, 0xaa, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            }
        );
        assert_eq!(bytes, hex("5003aa000000000000000000000000fd"));

        let (req, _, bytes) = round_trip("v2.cmd", "bc300000ffff");
        assert_eq!(
            req,
            Req::RawBigData {
                kind: BigDataKind::FileList,
                body: vec![],
            }
        );
        assert_eq!(bytes, hex("bc300000ffff"));

        let (req, _, bytes) = round_trip("v2.cmd", "bc4104008f684e742169");
        assert_eq!(req, Req::WorkoutList { since: 0x6921_744e });
        assert_eq!(bytes, hex("bc4104008f684e742169"));
    }

    #[test]
    fn what_does_not_decode_has_no_request() {
        let adapter = adapter();
        // A bad checksum, a short frame, a bad CRC, a reply-only channel.
        assert_eq!(
            adapter.request(&line("v1.write", "0401120000000000000000000000ff")),
            None
        );
        assert_eq!(adapter.request(&line("v1.write", "0401")), None);
        assert_eq!(adapter.request(&line("v2.cmd", "bc300000ff00")), None);
        assert_eq!(
            adapter.request(&line("v1.notify", "04000000000000000000000000000004")),
            None
        );
        assert_eq!(adapter.request(&line("v2.notify", "bc300100bf4000")), None);
        assert_eq!(adapter.request(&line("dis.fw", "5254")), None);
        assert_eq!(
            adapter.request(&line("elsewhere", "04011200000000000000000000000017")),
            None
        );
        // A named command whose sub-command is not the documented one.
        assert_eq!(
            adapter.request(&line("v1.write", "03020000000000000000000000000005")),
            None
        );
    }
}
