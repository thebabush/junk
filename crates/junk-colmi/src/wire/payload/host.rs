//! What the host writes on V1 write, one variant per command shape.

use alloc::vec::Vec;

use crate::wire::payload::{
    Body, DecodeError, Platform, Prefs, WorkoutAction, bcd, bcd_to_u8, bcd_year, frame, year_of_bcd,
};
use crate::wire::{Cmd, Frame, WireError};

/// The bucket-size byte of an activity request: 15-minute buckets.
const BUCKET_15_MIN: u8 = 0x0f;
/// The last 15-minute bucket of a day.
const LAST_BUCKET: u8 = 95;

/// A frame the host writes, as a typed value.
///
/// [`HostFrame::encode`] builds the [`Frame`]; [`HostFrame::decode`] reads one back, so a
/// recorded trace's `tx` lines can be typed and re-encoded. Every variant's doc gives its
/// body; unspecified trailing bytes are zero.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub enum HostFrame {
    /// `0x01`: set the clock. Body `YY MM DD hh mm ss <lang>`, the six time fields BCD
    /// (`26 07 02 14 16 30 01` = 2026-07-02 14:16:30). The ack is [`RingFrame::SetTimeAck`]
    /// with the ring's capabilities.
    ///
    /// [`RingFrame::SetTimeAck`]: crate::wire::RingFrame::SetTimeAck
    SetTime {
        /// `2000..=2099`; anything else does not encode.
        year: u16,
        /// Month, `1..=12` on a sane clock; encoded as two BCD digits.
        month: u8,
        /// Day of month.
        day: u8,
        /// Hour, 24-hour clock.
        hour: u8,
        /// Minute.
        minute: u8,
        /// Second.
        second: u8,
        /// The last byte; `QRing` sends `01`. A language code (R09 decompilation).
        lang: u8,
    },
    /// `0x03`: ask for the battery level. Body `01`.
    Battery,
    /// `0x04`: tell the ring about the phone. Body `<platform> <os_version> <name…>`:
    /// `01 12` is `QRing` on iOS 18 with no name, `02 0a 'G' 'B'` is Gadgetbridge on
    /// Android 10.
    PhoneName {
        /// The phone's platform.
        platform: Platform,
        /// Its OS major version.
        os_version: u8,
        /// The app's name, at most [`HostFrame::PHONE_NAME_MAX_LEN`] bytes; the body
        /// always ends in a zero.
        name: Vec<u8>,
    },
    /// `0x0a`: read the user profile. Body `01`; the reply is [`RingFrame::Prefs`].
    ///
    /// [`RingFrame::Prefs`]: crate::wire::RingFrame::Prefs
    ReadPrefs,
    /// `0x0a`: write the user profile. Body `02` then the nine [`Prefs`] bytes.
    WritePrefs(Prefs),
    /// `0x15`: the HR log of one day. Body `<day_start u32 le>`: the day's local midnight
    /// expressed as if it were UTC (`00 aa 45 6a` = 1782950400 = 2026-07-02 00:00).
    ///
    /// The docs write this as `15 00 <ts>`; the fixture bytes only give the documented
    /// timestamp when the `00` is the low byte of the u32, so that is the layout here.
    HrLog {
        /// The requested day's local midnight, as a UTC epoch second.
        day_start: u32,
    },
    /// `0x16`: read the automatic HR measurement preference. Body `01 02`, as `QRing`
    /// sends it; the `02` is not understood.
    ReadAutoHrPref,
    /// `0x19`: ask for the firmware and hardware versions. Body `01 01 01`; the ack is
    /// [`RingFrame::VersionAck`] and the strings follow as raw ASCII.
    ///
    /// [`RingFrame::VersionAck`]: crate::wire::RingFrame::VersionAck
    Version,
    /// `0x21`: read the daily goals. Body `01`.
    ReadGoals,
    /// `0x2c`: read the automatic `SpO2` preference. Body `01`.
    ReadAutoSpo2Pref,
    /// `0x36`: read the automatic stress preference. Body `01`.
    ReadAutoStressPref,
    /// `0x37`: the stress series of one day. Body `<days_ago>`.
    StressLog {
        /// `0` for today; only `0` has been seen.
        days_ago: u8,
    },
    /// `0x38`: read the automatic HRV preference. Body `01 02`, as `QRing` sends it.
    ReadAutoHrvPref,
    /// `0x39`: the HRV series of one day. Body `<days_ago>`.
    HrvLog {
        /// `0` for today; only `0` has been seen.
        days_ago: u8,
    },
    /// `0x43`: the activity of one day in 15-minute buckets. Body
    /// `<days_ago> 0f <first_bucket> <last_bucket> 01` (`00 0f 00 5f 01` = all of today).
    Activity {
        /// `0` for today; `QRing` clamps this to 29.
        days_ago: u8,
        /// First bucket wanted, `0..=95`.
        first_bucket: u8,
        /// Last bucket wanted, `0..=95`.
        last_bucket: u8,
    },
    /// `0x48`: today's running totals. Body `00`.
    TodayTotals,
    /// `0x50`: make the ring signal itself. Body `03 aa` (R09 decompilation; not seen on
    /// this ring, unverified).
    FindDevice,
    /// `0x69`: start a one-shot manual HR measurement. Body `<kind> <sub>`; `01` starts a
    /// heart-rate reading.
    ManualHrStart {
        /// What to measure.
        kind: u8,
        /// The second byte, whatever the kind takes.
        sub: u8,
    },
    /// `0x6a`: stop it. Body `<kind>`.
    ManualHrStop {
        /// What to stop measuring.
        kind: u8,
    },
    /// `0x77`: control a phone-initiated workout. Body `<action> <sport_type> 00`
    /// (`01 07 00` starts sport 7). The ring acks with [`RingFrame::WorkoutCtlAck`].
    ///
    /// [`RingFrame::WorkoutCtlAck`]: crate::wire::RingFrame::WorkoutCtlAck
    WorkoutCtl {
        /// Start, pause, continue or stop.
        action: WorkoutAction,
        /// The sport, as the app numbers them; `7` in the fixture.
        sport_type: u8,
    },
    /// Any frame with no variant: every [`Cmd::Other`], and [`Cmd::FactoryReset`], which
    /// has no variant on purpose so that wiping the ring can never be written by accident.
    /// This is the only way to build one.
    Raw {
        /// The command byte.
        cmd: Cmd,
        /// The body, undecoded.
        body: [u8; Frame::BODY_LEN],
    },
}

impl HostFrame {
    /// The most name bytes a [`HostFrame::PhoneName`] carries.
    pub const PHONE_NAME_MAX_LEN: usize = 11;

    /// The typed value of `frame`, read from its command byte and body.
    ///
    /// Total for a [`Cmd::Other`] command and for the ring-only commands (`0x2f`, `0x73`,
    /// `0x78`), which come back as [`HostFrame::Raw`].
    ///
    /// # Errors
    ///
    /// [`DecodeError::Malformed`] when a named command's body does not fit its layout:
    /// a sub-command byte that is not the documented one, a non-BCD time field, an
    /// activity request without the `0f` bucket size or with a bucket above 95, a phone
    /// name longer than [`HostFrame::PHONE_NAME_MAX_LEN`].
    pub fn decode(frame: &Frame) -> Result<HostFrame, DecodeError> {
        let body = &frame.body;
        let malformed = |what| DecodeError::Malformed {
            cmd: frame.cmd,
            what,
        };
        match frame.cmd {
            Cmd::SetTime => set_time(body).ok_or(malformed("bcd time")),
            Cmd::Battery if body[0] == 0x01 => Ok(HostFrame::Battery),
            Cmd::PhoneName => phone_name(body).ok_or(malformed("phone name")),
            Cmd::Prefs if body[0] == 0x01 => Ok(HostFrame::ReadPrefs),
            Cmd::Prefs if body[0] == 0x02 => Ok(HostFrame::WritePrefs(Prefs::from_body(body))),
            Cmd::HrLog => {
                let [t0, t1, t2, t3, ..] = *body;
                Ok(HostFrame::HrLog {
                    day_start: u32::from_le_bytes([t0, t1, t2, t3]),
                })
            }
            Cmd::AutoHrPref if body[0] == 0x01 => Ok(HostFrame::ReadAutoHrPref),
            Cmd::Version if body[0] == 0x01 => Ok(HostFrame::Version),
            Cmd::Goals if body[0] == 0x01 => Ok(HostFrame::ReadGoals),
            Cmd::AutoSpo2Pref if body[0] == 0x01 => Ok(HostFrame::ReadAutoSpo2Pref),
            Cmd::AutoStressPref if body[0] == 0x01 => Ok(HostFrame::ReadAutoStressPref),
            Cmd::StressLog => Ok(HostFrame::StressLog { days_ago: body[0] }),
            Cmd::AutoHrvPref if body[0] == 0x01 => Ok(HostFrame::ReadAutoHrvPref),
            Cmd::HrvLog => Ok(HostFrame::HrvLog { days_ago: body[0] }),
            Cmd::Activity if body[1] != BUCKET_15_MIN => Err(malformed("bucket size")),
            Cmd::Activity if body[2] > LAST_BUCKET || body[3] > LAST_BUCKET => {
                Err(malformed("bucket"))
            }
            Cmd::Activity => Ok(HostFrame::Activity {
                days_ago: body[0],
                first_bucket: body[2],
                last_bucket: body[3],
            }),
            Cmd::TodayTotals if body[0] == 0x00 => Ok(HostFrame::TodayTotals),
            Cmd::FindDevice if body[0] == 0x03 && body[1] == 0xaa => Ok(HostFrame::FindDevice),
            Cmd::ManualHrStart => Ok(HostFrame::ManualHrStart {
                kind: body[0],
                sub: body[1],
            }),
            Cmd::ManualHrStop => Ok(HostFrame::ManualHrStop { kind: body[0] }),
            Cmd::WorkoutCtl => Ok(HostFrame::WorkoutCtl {
                action: WorkoutAction::from_byte(body[0]),
                sport_type: body[1],
            }),
            Cmd::Battery
            | Cmd::Prefs
            | Cmd::AutoHrPref
            | Cmd::Version
            | Cmd::Goals
            | Cmd::AutoSpo2Pref
            | Cmd::AutoStressPref
            | Cmd::AutoHrvPref
            | Cmd::TodayTotals
            | Cmd::FindDevice => Err(malformed("sub-command")),
            Cmd::PacketSize
            | Cmd::Notify
            | Cmd::WorkoutData
            | Cmd::FactoryReset
            | Cmd::Other(_) => Ok(HostFrame::Raw {
                cmd: frame.cmd,
                body: *body,
            }),
        }
    }

    /// The frame for this value.
    ///
    /// # Errors
    ///
    /// [`WireError::Value`] for a field the layout cannot hold: a [`HostFrame::SetTime`]
    /// year outside `2000..=2099` or another time field above 99, an
    /// [`HostFrame::Activity`] bucket above 95. [`WireError::PayloadTooLong`] for a
    /// [`HostFrame::PhoneName`] name longer than [`HostFrame::PHONE_NAME_MAX_LEN`].
    pub fn encode(&self) -> Result<Frame, WireError> {
        match self {
            HostFrame::SetTime {
                year,
                month,
                day,
                hour,
                minute,
                second,
                lang,
            } => frame(
                Cmd::SetTime,
                &[&[
                    bcd_year(*year)?,
                    bcd(*month, "month")?,
                    bcd(*day, "day")?,
                    bcd(*hour, "hour")?,
                    bcd(*minute, "minute")?,
                    bcd(*second, "second")?,
                    *lang,
                ]],
            ),
            HostFrame::Battery => frame(Cmd::Battery, &[&[0x01]]),
            HostFrame::PhoneName {
                platform,
                os_version,
                name,
            } => {
                if name.len() > Self::PHONE_NAME_MAX_LEN {
                    return Err(WireError::PayloadTooLong {
                        max: Self::PHONE_NAME_MAX_LEN,
                        got: name.len(),
                    });
                }
                frame(Cmd::PhoneName, &[&[platform.byte(), *os_version], name])
            }
            HostFrame::ReadPrefs => frame(Cmd::Prefs, &[&[0x01]]),
            HostFrame::WritePrefs(prefs) => frame(Cmd::Prefs, &[&[0x02], &prefs.to_bytes()]),
            HostFrame::HrLog { day_start } => frame(Cmd::HrLog, &[&day_start.to_le_bytes()]),
            HostFrame::ReadAutoHrPref => frame(Cmd::AutoHrPref, &[&[0x01, 0x02]]),
            HostFrame::Version => frame(Cmd::Version, &[&[0x01, 0x01, 0x01]]),
            HostFrame::ReadGoals => frame(Cmd::Goals, &[&[0x01]]),
            HostFrame::ReadAutoSpo2Pref => frame(Cmd::AutoSpo2Pref, &[&[0x01]]),
            HostFrame::ReadAutoStressPref => frame(Cmd::AutoStressPref, &[&[0x01]]),
            HostFrame::StressLog { days_ago } => frame(Cmd::StressLog, &[&[*days_ago]]),
            HostFrame::ReadAutoHrvPref => frame(Cmd::AutoHrvPref, &[&[0x01, 0x02]]),
            HostFrame::HrvLog { days_ago } => frame(Cmd::HrvLog, &[&[*days_ago]]),
            HostFrame::Activity {
                days_ago,
                first_bucket,
                last_bucket,
            } => {
                if *first_bucket > LAST_BUCKET || *last_bucket > LAST_BUCKET {
                    return Err(WireError::Value { what: "bucket" });
                }
                frame(
                    Cmd::Activity,
                    &[&[*days_ago, BUCKET_15_MIN, *first_bucket, *last_bucket, 0x01]],
                )
            }
            HostFrame::TodayTotals => frame(Cmd::TodayTotals, &[&[0x00]]),
            HostFrame::FindDevice => frame(Cmd::FindDevice, &[&[0x03, 0xaa]]),
            HostFrame::ManualHrStart { kind, sub } => frame(Cmd::ManualHrStart, &[&[*kind, *sub]]),
            HostFrame::ManualHrStop { kind } => frame(Cmd::ManualHrStop, &[&[*kind]]),
            HostFrame::WorkoutCtl { action, sport_type } => {
                frame(Cmd::WorkoutCtl, &[&[action.byte(), *sport_type, 0x00]])
            }
            HostFrame::Raw { cmd, body } => Ok(Frame {
                cmd: *cmd,
                body: *body,
            }),
        }
    }
}

/// `YY MM DD hh mm ss <lang>`, the six time fields BCD; `None` if one is not.
fn set_time(body: &Body) -> Option<HostFrame> {
    let [yy, mm, dd, hh, min, ss, lang, ..] = *body;
    Some(HostFrame::SetTime {
        year: year_of_bcd(yy)?,
        month: bcd_to_u8(mm)?,
        day: bcd_to_u8(dd)?,
        hour: bcd_to_u8(hh)?,
        minute: bcd_to_u8(min)?,
        second: bcd_to_u8(ss)?,
        lang,
    })
}

/// `<platform> <os_version> <name…>`, the name up to its first zero byte; `None` if the
/// name runs past [`HostFrame::PHONE_NAME_MAX_LEN`].
fn phone_name(body: &Body) -> Option<HostFrame> {
    let [platform, os_version, name @ ..] = *body;
    let len = name.iter().position(|&b| b == 0).unwrap_or(name.len());
    if len > HostFrame::PHONE_NAME_MAX_LEN {
        return None;
    }
    Some(HostFrame::PhoneName {
        platform: Platform::from_byte(platform),
        os_version,
        name: name.get(..len)?.to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// A frame for `cmd` with `payload`, zero-padded.
    fn raw(cmd: u8, payload: &[u8]) -> Frame {
        Frame::new(Cmd::from_byte(cmd), payload).unwrap_or_else(|err| panic!("{err}"))
    }

    /// `payload` after command byte `cmd` decodes to `value`, and `value` encodes back to
    /// exactly that frame.
    fn check(cmd: u8, payload: &[u8], value: HostFrame) {
        let frame = raw(cmd, payload);
        assert_eq!(value.encode(), Ok(frame), "{value:?}");
        assert_eq!(HostFrame::decode(&frame), Ok(value), "{payload:02x?}");
    }

    #[test]
    fn set_time_is_bcd() {
        check(
            0x01,
            &[0x26, 0x07, 0x02, 0x14, 0x16, 0x30, 0x01],
            HostFrame::SetTime {
                year: 2026,
                month: 7,
                day: 2,
                hour: 14,
                minute: 16,
                second: 30,
                lang: 1,
            },
        );
        check(
            0x01,
            &[0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00],
            HostFrame::SetTime {
                year: 2000,
                month: 1,
                day: 1,
                hour: 0,
                minute: 0,
                second: 0,
                lang: 0,
            },
        );
        let value = |year| HostFrame::SetTime {
            year,
            month: 7,
            day: 2,
            hour: 14,
            minute: 16,
            second: 30,
            lang: 1,
        };
        assert_eq!(value(1999).encode(), Err(WireError::Value { what: "year" }));
        assert_eq!(value(2100).encode(), Err(WireError::Value { what: "year" }));
        assert!(value(2099).encode().is_ok());
        let bad_minute = HostFrame::SetTime {
            year: 2026,
            month: 7,
            day: 2,
            hour: 14,
            minute: 100,
            second: 30,
            lang: 1,
        };
        assert_eq!(
            bad_minute.encode(),
            Err(WireError::Value { what: "minute" })
        );
        assert_eq!(
            HostFrame::decode(&raw(0x01, &[0x2a, 0x07, 0x02, 0x14, 0x16, 0x30, 0x01])),
            Err(DecodeError::Malformed {
                cmd: Cmd::SetTime,
                what: "bcd time"
            })
        );
    }

    #[test]
    fn one_byte_requests() {
        check(0x03, &[0x01], HostFrame::Battery);
        check(0x0a, &[0x01], HostFrame::ReadPrefs);
        check(0x16, &[0x01, 0x02], HostFrame::ReadAutoHrPref);
        check(0x19, &[0x01, 0x01, 0x01], HostFrame::Version);
        check(0x21, &[0x01], HostFrame::ReadGoals);
        check(0x2c, &[0x01], HostFrame::ReadAutoSpo2Pref);
        check(0x36, &[0x01], HostFrame::ReadAutoStressPref);
        check(0x38, &[0x01, 0x02], HostFrame::ReadAutoHrvPref);
        check(0x48, &[0x00], HostFrame::TodayTotals);
        check(0x50, &[0x03, 0xaa], HostFrame::FindDevice);
        check(0x37, &[0x00], HostFrame::StressLog { days_ago: 0 });
        check(0x39, &[0x00], HostFrame::HrvLog { days_ago: 0 });
        check(0x37, &[0x03], HostFrame::StressLog { days_ago: 3 });
        check(
            0x69,
            &[0x01, 0x00],
            HostFrame::ManualHrStart { kind: 1, sub: 0 },
        );
        check(0x6a, &[0x01], HostFrame::ManualHrStop { kind: 1 });
    }

    #[test]
    fn a_wrong_sub_command_is_malformed() {
        let malformed = |cmd, what| {
            Err(DecodeError::Malformed {
                cmd: Cmd::from_byte(cmd),
                what,
            })
        };
        assert_eq!(
            HostFrame::decode(&raw(0x03, &[0x02])),
            malformed(0x03, "sub-command")
        );
        assert_eq!(
            HostFrame::decode(&raw(0x0a, &[0x03])),
            malformed(0x0a, "sub-command")
        );
        assert_eq!(
            HostFrame::decode(&raw(0x48, &[0x01])),
            malformed(0x48, "sub-command")
        );
        assert_eq!(
            HostFrame::decode(&raw(0x50, &[0x03])),
            malformed(0x50, "sub-command")
        );
        assert_eq!(
            HostFrame::decode(&raw(0x21, &[0x02, 0x88, 0x13])),
            malformed(0x21, "sub-command")
        );
    }

    #[test]
    fn phone_name_holds_at_most_eleven_bytes() {
        check(
            0x04,
            &[0x01, 0x12],
            HostFrame::PhoneName {
                platform: Platform::Ios,
                os_version: 18,
                name: vec![],
            },
        );
        check(
            0x04,
            &[0x02, 0x0a, b'G', b'B'],
            HostFrame::PhoneName {
                platform: Platform::Android,
                os_version: 10,
                name: b"GB".to_vec(),
            },
        );
        check(
            0x04,
            b"\x05\x01abcdefghijk",
            HostFrame::PhoneName {
                platform: Platform::Other(5),
                os_version: 1,
                name: b"abcdefghijk".to_vec(),
            },
        );
        let long = HostFrame::PhoneName {
            platform: Platform::Ios,
            os_version: 18,
            name: b"abcdefghijkl".to_vec(),
        };
        assert_eq!(
            long.encode(),
            Err(WireError::PayloadTooLong { max: 11, got: 12 })
        );
        assert_eq!(
            HostFrame::decode(&raw(0x04, b"\x01\x12abcdefghijkl")),
            Err(DecodeError::Malformed {
                cmd: Cmd::PhoneName,
                what: "phone name"
            })
        );
    }

    #[test]
    fn write_prefs_is_the_fixture_profile() {
        check(
            0x0a,
            &[0x02, 0x00, 0x00, 0x00, 0x1e, 0xaf, 0x46, 0x78, 0x50, 0x96],
            HostFrame::WritePrefs(Prefs {
                hour12: false,
                imperial: false,
                sex: 0,
                age: 30,
                height_cm: 175,
                weight_kg: 70,
                sbp: 120,
                dbp: 80,
                hr_warn: 150,
            }),
        );
    }

    #[test]
    fn hr_log_carries_the_day_start_from_body_zero() {
        // 2026-07-02 00:00 as if UTC, as QRing sent it: the low byte is the `00`.
        check(
            0x15,
            &[0x00, 0xaa, 0x45, 0x6a],
            HostFrame::HrLog {
                day_start: 1_782_950_400,
            },
        );
        check(
            0x15,
            &[0x80, 0xfb, 0x46, 0x6a],
            HostFrame::HrLog {
                day_start: 1_783_036_800,
            },
        );
    }

    #[test]
    fn activity_wants_fifteen_minute_buckets_in_range() {
        check(
            0x43,
            &[0x00, 0x0f, 0x00, 0x5f, 0x01],
            HostFrame::Activity {
                days_ago: 0,
                first_bucket: 0,
                last_bucket: 95,
            },
        );
        check(
            0x43,
            &[0x1d, 0x0f, 0x01, 0x5f, 0x01],
            HostFrame::Activity {
                days_ago: 29,
                first_bucket: 1,
                last_bucket: 95,
            },
        );
        let bucket_96 = HostFrame::Activity {
            days_ago: 0,
            first_bucket: 0,
            last_bucket: 96,
        };
        assert_eq!(bucket_96.encode(), Err(WireError::Value { what: "bucket" }));
        assert_eq!(
            HostFrame::decode(&raw(0x43, &[0x00, 0x0f, 0x60, 0x5f, 0x01])),
            Err(DecodeError::Malformed {
                cmd: Cmd::Activity,
                what: "bucket"
            })
        );
        assert_eq!(
            HostFrame::decode(&raw(0x43, &[0x00, 0x00, 0x00, 0x5f, 0x01])),
            Err(DecodeError::Malformed {
                cmd: Cmd::Activity,
                what: "bucket size"
            })
        );
    }

    #[test]
    fn workout_ctl_from_the_thering_fixture() {
        check(
            0x77,
            &[0x01, 0x07, 0x00],
            HostFrame::WorkoutCtl {
                action: WorkoutAction::Start,
                sport_type: 7,
            },
        );
        check(
            0x77,
            &[0x02, 0x07, 0x00],
            HostFrame::WorkoutCtl {
                action: WorkoutAction::Pause,
                sport_type: 7,
            },
        );
        check(
            0x77,
            &[0x04, 0x07, 0x00],
            HostFrame::WorkoutCtl {
                action: WorkoutAction::Stop,
                sport_type: 7,
            },
        );
    }

    #[test]
    fn raw_for_unnamed_ring_only_and_factory_reset() {
        let other = raw(0x3c, &[0x00]);
        check(
            0x3c,
            &[0x00],
            HostFrame::Raw {
                cmd: Cmd::Other(0x3c),
                body: other.body,
            },
        );
        for cmd in [0x2f, 0x73, 0x78, 0xff] {
            let frame = raw(cmd, &[0x01, 0x02]);
            assert_eq!(
                HostFrame::decode(&frame),
                Ok(HostFrame::Raw {
                    cmd: Cmd::from_byte(cmd),
                    body: frame.body,
                })
            );
        }
        // Factory reset has no variant: `Raw` is the only spelling.
        let reset = HostFrame::Raw {
            cmd: Cmd::FactoryReset,
            body: [0; 14],
        };
        assert_eq!(reset.encode().map(|f| f.cmd), Ok(Cmd::FactoryReset));
    }
}
