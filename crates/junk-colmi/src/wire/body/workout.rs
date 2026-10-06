//! The `0x42`, `0x44` and `0x45` workout replies: TLV summaries of the stored records,
//! then a descriptor and sample packages for one of them.
//!
//! The layouts are from the `QRing` APK ("`SportPlus`"), decoded against the complete frames
//! in the thering fixture: one record for a 61-second session of sport 7, with 54 heart
//! rate samples in one package.

use alloc::vec;
use alloc::vec::Vec;

use crate::wire::BigDataKind;
use crate::wire::WireError;
use crate::wire::body::{BodyError, arrays, byte_of};

/// A TLV's length byte and tag byte, which its length counts.
const TLV_HEADER_LEN: usize = 2;
/// A record's length byte and sport byte, which its length counts.
const RECORD_HEADER_LEN: usize = 2;
/// The most bytes a value can have and still be read as a number.
const MAX_NUMBER_LEN: usize = 8;

/// The body of a `0x42` reply: `<count:u8>` then `count` [`WorkoutRecord`]s, nothing
/// else.
///
/// The thering reply is `01` then a 48-byte record; every `QRing` reply is `00`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub struct WorkoutSummary {
    /// The records, in the order the ring sent them.
    pub records: Vec<WorkoutRecord>,
}

impl WorkoutSummary {
    /// The body's records, or why they do not decode.
    pub(super) fn from_body(body: &[u8]) -> Result<WorkoutSummary, BodyError> {
        let malformed = |what| BodyError::Malformed {
            kind: BigDataKind::WorkoutSummary,
            what,
        };
        let (&count, mut rest) = body.split_first().ok_or(malformed("record count"))?;
        let mut records = Vec::new();
        for _ in 0..count {
            let (&len, after_len) = rest.split_first().ok_or(malformed("record count"))?;
            let (record, next) = usize::from(len)
                .checked_sub(1)
                .and_then(|len| after_len.split_at_checked(len))
                .ok_or(malformed("record length"))?;
            let (&sport_type, tlvs) = record.split_first().ok_or(malformed("record length"))?;
            let fields = fields_of(tlvs).ok_or(malformed("field length"))?;
            records.push(WorkoutRecord { sport_type, fields });
            rest = next;
        }
        if !rest.is_empty() {
            return Err(malformed("trailing bytes"));
        }
        Ok(WorkoutSummary { records })
    }

    /// The body: the count then each record with its length byte.
    pub(super) fn to_body(&self) -> Result<Vec<u8>, WireError> {
        let mut body = vec![byte_of(self.records.len(), "workout records")?];
        for record in &self.records {
            let mut tlvs = Vec::new();
            for field in &record.fields {
                tlvs.push(byte_of(
                    field.value.len() + TLV_HEADER_LEN,
                    "workout field length",
                )?);
                tlvs.push(field.tag.byte());
                tlvs.extend_from_slice(&field.value);
            }
            body.push(byte_of(
                tlvs.len() + RECORD_HEADER_LEN,
                "workout record length",
            )?);
            body.push(record.sport_type);
            body.extend_from_slice(&tlvs);
        }
        Ok(body)
    }
}

/// The fields in a run of TLVs, or `None` if one's length runs past the end or below its
/// own two bytes.
fn fields_of(mut tlvs: &[u8]) -> Option<Vec<WorkoutField>> {
    let mut fields = Vec::new();
    while !tlvs.is_empty() {
        let (&[len, tag], after) = tlvs.split_first_chunk()?;
        let (value, next) =
            after.split_at_checked(usize::from(len).checked_sub(TLV_HEADER_LEN)?)?;
        fields.push(WorkoutField {
            tag: WorkoutTag::from_byte(tag),
            value: value.to_vec(),
        });
        tlvs = next;
    }
    Some(fields)
}

/// One stored workout: `<len:u8> <sport_type:u8> <tlv…>`, where `len` counts the whole
/// record, itself included, and each TLV is `<len:u8> <tag:u8> <value…>` with `len`
/// counting itself and the tag.
///
/// The thering record is `30 07` then eleven TLVs: 48 bytes, sport 7, start `0x691d2980`
/// (the `77 01` ack's timestamp), duration 61, distance 0, calories 0, speeds 0, rate
/// average 60, min 57, max 62, step rate 0, steps 0. Value widths seen: four bytes for
/// tags 1, 4, 19; two for 2, 3, 5, 6; one for 7, 8, 9, 13.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub struct WorkoutRecord {
    /// The sport, as the app numbers them; `7` in the fixture.
    pub sport_type: u8,
    /// The fields, in the order the ring sent them.
    pub fields: Vec<WorkoutField>,
}

impl WorkoutRecord {
    /// The first field with `tag` as a number, or `None` if there is none or its value is
    /// wider than eight bytes.
    #[must_use]
    pub fn get(&self, tag: WorkoutTag) -> Option<u64> {
        self.fields
            .iter()
            .find(|field| field.tag == tag)
            .and_then(WorkoutField::as_u64)
    }
}

/// One TLV of a [`WorkoutRecord`]: a tag and its value bytes as sent, so a width the docs
/// have not seen survives a round trip.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub struct WorkoutField {
    /// What the value is.
    pub tag: WorkoutTag,
    /// The value, little-endian, at most 253 bytes on the wire.
    pub value: Vec<u8>,
}

impl WorkoutField {
    /// The value as a little-endian number; `None` if it is wider than eight bytes. An
    /// empty value is `0`.
    #[must_use]
    pub fn as_u64(&self) -> Option<u64> {
        if self.value.len() > MAX_NUMBER_LEN {
            return None;
        }
        Some(
            self.value
                .iter()
                .rev()
                .fold(0, |number, &byte| (number << 8) | u64::from(byte)),
        )
    }
}

/// The tag of a [`WorkoutField`]: which figure of the record it carries.
///
/// The numbers are the APK's; the units are whatever the app stores (seconds for the
/// duration, bpm for the rates, and the rest unexercised by the fixtures). Every byte has a
/// value; unnamed ones are [`WorkoutTag::Other`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub enum WorkoutTag {
    /// `1`: when the workout started, on the ring's clock (`u32` in the fixture).
    StartTime,
    /// `2`: how long it lasted, seconds (`u16`).
    Duration,
    /// `3`: distance (`u16`).
    Distance,
    /// `4`: calories (`u32`).
    Calories,
    /// `5`: average speed (`u16`).
    SpeedAvg,
    /// `6`: maximum speed (`u16`).
    SpeedMax,
    /// `7`: average heart rate, bpm (`u8`).
    RateAvg,
    /// `8`: minimum heart rate, bpm (`u8`).
    RateMin,
    /// `9`: maximum heart rate, bpm (`u8`).
    RateMax,
    /// `10`: elevation.
    Elevation,
    /// `11`: climb.
    Uphill,
    /// `12`: descent.
    Downhill,
    /// `13`: step rate (`u8`).
    StepRate,
    /// `14`: a sport-specific count.
    SportCount,
    /// `19`: steps (`u32`).
    Steps,
    /// A byte with no named variant, passed through undecoded.
    Other(u8),
}

impl WorkoutTag {
    /// The tag with this byte. Total: an unnamed byte is [`WorkoutTag::Other`].
    #[must_use]
    pub const fn from_byte(byte: u8) -> WorkoutTag {
        match byte {
            1 => WorkoutTag::StartTime,
            2 => WorkoutTag::Duration,
            3 => WorkoutTag::Distance,
            4 => WorkoutTag::Calories,
            5 => WorkoutTag::SpeedAvg,
            6 => WorkoutTag::SpeedMax,
            7 => WorkoutTag::RateAvg,
            8 => WorkoutTag::RateMin,
            9 => WorkoutTag::RateMax,
            10 => WorkoutTag::Elevation,
            11 => WorkoutTag::Uphill,
            12 => WorkoutTag::Downhill,
            13 => WorkoutTag::StepRate,
            14 => WorkoutTag::SportCount,
            19 => WorkoutTag::Steps,
            other => WorkoutTag::Other(other),
        }
    }

    /// The byte on the wire.
    #[must_use]
    pub const fn byte(self) -> u8 {
        match self {
            WorkoutTag::StartTime => 1,
            WorkoutTag::Duration => 2,
            WorkoutTag::Distance => 3,
            WorkoutTag::Calories => 4,
            WorkoutTag::SpeedAvg => 5,
            WorkoutTag::SpeedMax => 6,
            WorkoutTag::RateAvg => 7,
            WorkoutTag::RateMin => 8,
            WorkoutTag::RateMax => 9,
            WorkoutTag::Elevation => 10,
            WorkoutTag::Uphill => 11,
            WorkoutTag::Downhill => 12,
            WorkoutTag::StepRate => 13,
            WorkoutTag::SportCount => 14,
            WorkoutTag::Steps => 19,
            WorkoutTag::Other(byte) => byte,
        }
    }
}

/// The body of a `0x44` reply: how one workout's detail samples are laid out.
///
/// Layout: `<status:u8> <package_count:u8> <byte2:u8> <sample_second:u8>` then repeated
/// `<len:u8> <tag:u8>`, one per field of a sample record. The thering reply is
/// `00 01 00 01 01 11`: ok, one package, one second between samples, one one-byte field
/// with tag 17, the realtime heart rate (the only tag the app decodes; 15 and 16 are
/// referenced but not decoded).
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub struct WorkoutDescriptor {
    /// `0` = ok.
    pub status: u8,
    /// How many `0x45` packages follow.
    pub package_count: u8,
    /// The third byte, `00` in the fixture; not understood, kept so the frame re-encodes
    /// byte for byte.
    pub byte2: u8,
    /// Seconds between samples.
    pub sample_second: u8,
    /// The fields of one sample record, in order.
    pub fields: Vec<DescriptorField>,
}

impl WorkoutDescriptor {
    /// The descriptor, or why it does not decode.
    pub(super) fn from_body(body: &[u8]) -> Result<WorkoutDescriptor, BodyError> {
        let malformed = |what| BodyError::Malformed {
            kind: BigDataKind::WorkoutDescriptor,
            what,
        };
        let (&[status, package_count, byte2, sample_second], list) =
            body.split_first_chunk().ok_or(malformed("header"))?;
        if list.len() % 2 != 0 {
            return Err(malformed("field list"));
        }
        Ok(WorkoutDescriptor {
            status,
            package_count,
            byte2,
            sample_second,
            fields: arrays(list)
                .map(|[len, tag]| DescriptorField {
                    len,
                    tag: SampleTag::from_byte(tag),
                })
                .collect(),
        })
    }

    /// The body: the four header bytes then the field list.
    pub(super) fn to_body(&self) -> Vec<u8> {
        let mut body = vec![
            self.status,
            self.package_count,
            self.byte2,
            self.sample_second,
        ];
        for field in &self.fields {
            body.extend_from_slice(&[field.len, field.tag.byte()]);
        }
        body
    }

    /// How many bytes one sample record takes: the fields' lengths added up.
    #[must_use]
    pub fn record_len(&self) -> usize {
        self.fields.iter().map(|field| usize::from(field.len)).sum()
    }

    /// Where the first field with `tag` sits in a sample record: its byte offset and its
    /// length. `None` if no field has that tag.
    #[must_use]
    pub fn offset_of(&self, tag: SampleTag) -> Option<(usize, usize)> {
        let mut offset = 0;
        for field in &self.fields {
            let len = usize::from(field.len);
            if field.tag == tag {
                return Some((offset, len));
            }
            offset += len;
        }
        None
    }
}

/// One field of a sample record, as the [`WorkoutDescriptor`] declares it.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub struct DescriptorField {
    /// How many bytes the field takes in each record.
    pub len: u8,
    /// What it is; [`SampleTag::HeartRate`] is the one the app reads.
    pub tag: SampleTag,
}

/// The tag of a sample-record field, as a [`WorkoutDescriptor`] declares it.
///
/// Only `17` is decoded by the app; `15` and `16` are referenced there but not read.
/// Every byte has a value; unnamed ones are [`SampleTag::Other`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub enum SampleTag {
    /// `17`: the realtime heart rate, one byte per sample.
    HeartRate,
    /// A byte with no named variant, passed through undecoded.
    Other(u8),
}

impl SampleTag {
    /// The tag with this byte. Total: an unnamed byte is [`SampleTag::Other`].
    #[must_use]
    pub const fn from_byte(byte: u8) -> SampleTag {
        match byte {
            17 => SampleTag::HeartRate,
            other => SampleTag::Other(other),
        }
    }

    /// The byte on the wire.
    #[must_use]
    pub const fn byte(self) -> u8 {
        match self {
            SampleTag::HeartRate => 17,
            SampleTag::Other(byte) => byte,
        }
    }
}

/// The body of a `0x45` reply: `<package:u8> <byte1:u8>` then sample records laid out
/// per the [`WorkoutDescriptor`].
///
/// The thering reply is `01 00` then 54 bytes, one heart rate each for the 61-second
/// session, all within the summary's rate min and max of 57 and 62.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub struct WorkoutSeries {
    /// Which package this is, `1..=package_count`.
    pub package: u8,
    /// The second byte, `00` in the fixture; not understood, kept so the frame re-encodes
    /// byte for byte.
    pub byte1: u8,
    /// The sample records, as sent.
    pub data: Vec<u8>,
}

impl WorkoutSeries {
    /// The series, or why it does not decode.
    pub(super) fn from_body(body: &[u8]) -> Result<WorkoutSeries, BodyError> {
        let (&[package, byte1], data) = body.split_first_chunk().ok_or(BodyError::Malformed {
            kind: BigDataKind::WorkoutSeries,
            what: "header",
        })?;
        Ok(WorkoutSeries {
            package,
            byte1,
            data: data.to_vec(),
        })
    }

    /// The body: the two header bytes then the data.
    pub(super) fn to_body(&self) -> Vec<u8> {
        let mut body = vec![self.package, self.byte1];
        body.extend_from_slice(&self.data);
        body
    }

    /// The sample records, `data` cut into [`WorkoutDescriptor::record_len`]-byte pieces.
    /// A trailing piece shorter than a record is left out; a descriptor with no bytes per
    /// record gives no records.
    pub fn records<'a>(
        &'a self,
        descriptor: &WorkoutDescriptor,
    ) -> impl Iterator<Item = &'a [u8]> + use<'a> {
        let len = descriptor.record_len();
        let data: &[u8] = if len == 0 { &[] } else { &self.data };
        data.chunks_exact(len.max(1))
    }

    /// The heart rate of each record: the first byte of the field tagged
    /// [`SampleTag::HeartRate`]. `None` if the descriptor has no such field or it is empty.
    #[must_use]
    pub fn heart_rates(&self, descriptor: &WorkoutDescriptor) -> Option<Vec<u8>> {
        let (offset, len) = descriptor.offset_of(SampleTag::HeartRate)?;
        if len == 0 {
            return None;
        }
        // Every record is `record_len` bytes and the field starts inside it, so `get`
        // finds a byte in each and nothing is dropped.
        Some(
            self.records(descriptor)
                .filter_map(|record| record.get(offset).copied())
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The thering summary reply body: `01`, then a 48-byte record for sport 7.
    const SUMMARY: [u8; 49] = [
        0x01, 0x30, 0x07, 0x06, 0x01, 0x80, 0x29, 0x1d, 0x69, 0x04, 0x02, 0x3d, 0x00, 0x04, 0x03,
        0x00, 0x00, 0x06, 0x04, 0x00, 0x00, 0x00, 0x00, 0x04, 0x05, 0x00, 0x00, 0x04, 0x06, 0x00,
        0x00, 0x03, 0x07, 0x3c, 0x03, 0x08, 0x39, 0x03, 0x09, 0x3e, 0x03, 0x0d, 0x00, 0x06, 0x13,
        0x00, 0x00, 0x00, 0x00,
    ];

    /// The thering descriptor reply body.
    const DESCRIPTOR: [u8; 6] = [0x00, 0x01, 0x00, 0x01, 0x01, 0x11];

    /// The 54 heart rates of the thering series reply, as runs: the body after `01 00` is
    /// `3e×7 3d×8 3c×4 3d×4 3b×11 3d×2 3c×13 39×2 3a×3`.
    const HEART_RATE_RUNS: [(u8, usize); 9] = [
        (0x3e, 7),
        (0x3d, 8),
        (0x3c, 4),
        (0x3d, 4),
        (0x3b, 11),
        (0x3d, 2),
        (0x3c, 13),
        (0x39, 2),
        (0x3a, 3),
    ];

    fn heart_rates() -> Vec<u8> {
        HEART_RATE_RUNS
            .iter()
            .flat_map(|&(bpm, count)| core::iter::repeat_n(bpm, count))
            .collect()
    }

    fn series_body() -> Vec<u8> {
        let mut body = vec![0x01, 0x00];
        body.extend(heart_rates());
        body
    }

    fn field(tag: u8, value: &[u8]) -> WorkoutField {
        WorkoutField {
            tag: WorkoutTag::from_byte(tag),
            value: value.to_vec(),
        }
    }

    fn malformed(kind: BigDataKind, what: &'static str) -> BodyError {
        BodyError::Malformed { kind, what }
    }

    #[test]
    fn tag_bytes_round_trip_over_every_value() {
        let mut others = 0;
        for byte in 0..=u8::MAX {
            let tag = WorkoutTag::from_byte(byte);
            assert_eq!(tag.byte(), byte);
            if let WorkoutTag::Other(inner) = tag {
                assert_eq!(inner, byte);
                others += 1;
            }
        }
        assert_eq!(others, 256 - 15);
        assert_eq!(WorkoutTag::from_byte(1), WorkoutTag::StartTime);
        assert_eq!(WorkoutTag::from_byte(19), WorkoutTag::Steps);
        assert_eq!(WorkoutTag::from_byte(15), WorkoutTag::Other(15));
    }

    #[test]
    fn fixture_summary_is_the_documented_record() {
        let summary = WorkoutSummary::from_body(&SUMMARY).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(summary.records.len(), 1);
        let record = &summary.records[0];
        assert_eq!(record.sport_type, 7);
        assert_eq!(record.fields.len(), 11);
        assert_eq!(record.fields[0], field(1, &[0x80, 0x29, 0x1d, 0x69]));
        assert_eq!(record.fields[1], field(2, &[0x3d, 0x00]));
        assert_eq!(record.fields[10], field(19, &[0; 4]));
        assert_eq!(record.get(WorkoutTag::StartTime), Some(0x691d_2980));
        assert_eq!(record.get(WorkoutTag::Duration), Some(61));
        assert_eq!(record.get(WorkoutTag::Distance), Some(0));
        assert_eq!(record.get(WorkoutTag::Calories), Some(0));
        assert_eq!(record.get(WorkoutTag::SpeedAvg), Some(0));
        assert_eq!(record.get(WorkoutTag::SpeedMax), Some(0));
        assert_eq!(record.get(WorkoutTag::RateAvg), Some(60));
        assert_eq!(record.get(WorkoutTag::RateMin), Some(57));
        assert_eq!(record.get(WorkoutTag::RateMax), Some(62));
        assert_eq!(record.get(WorkoutTag::StepRate), Some(0));
        assert_eq!(record.get(WorkoutTag::Steps), Some(0));
        assert_eq!(record.get(WorkoutTag::Elevation), None);
        assert_eq!(record.get(WorkoutTag::Other(15)), None);
        assert_eq!(summary.to_body(), Ok(SUMMARY.to_vec()));

        let none = WorkoutSummary::from_body(&[0x00]).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(none.records, Vec::new());
        assert_eq!(none.to_body(), Ok(vec![0x00]));
    }

    #[test]
    fn records_and_fields_of_every_shape_round_trip() {
        let summary = WorkoutSummary {
            records: vec![
                WorkoutRecord {
                    sport_type: 1,
                    fields: Vec::new(),
                },
                WorkoutRecord {
                    sport_type: 2,
                    fields: vec![field(15, &[]), field(1, &[1, 2, 3, 4, 5, 6, 7, 8, 9])],
                },
            ],
        };
        let body = summary.to_body().unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(
            body,
            [
                0x02, 0x02, 0x01, 0x0f, 0x02, 0x02, 0x0f, 0x0b, 0x01, 1, 2, 3, 4, 5, 6, 7, 8, 9
            ]
        );
        assert_eq!(WorkoutSummary::from_body(&body), Ok(summary.clone()));
        assert_eq!(summary.records[1].fields[0].as_u64(), Some(0));
        assert_eq!(summary.records[1].get(WorkoutTag::Other(15)), Some(0));
        assert_eq!(summary.records[1].get(WorkoutTag::StartTime), None);
        assert_eq!(
            field(1, &[1, 2, 3, 4, 5, 6, 7, 8]).as_u64(),
            Some(0x0807_0605_0403_0201)
        );
        assert_eq!(field(1, &[0xff]).as_u64(), Some(255));
    }

    #[test]
    fn a_field_that_runs_past_its_record_is_malformed() {
        let kind = BigDataKind::WorkoutSummary;
        // Record of 4: sport 7, then a TLV claiming 6 bytes with 2 left.
        assert_eq!(
            WorkoutSummary::from_body(&[0x01, 0x04, 0x07, 0x06, 0x01]),
            Err(malformed(kind, "field length"))
        );
        // A TLV length below its own two bytes.
        assert_eq!(
            WorkoutSummary::from_body(&[0x01, 0x04, 0x07, 0x01, 0x01]),
            Err(malformed(kind, "field length"))
        );
        // A lone length byte where a TLV header should be.
        assert_eq!(
            WorkoutSummary::from_body(&[0x01, 0x03, 0x07, 0x02]),
            Err(malformed(kind, "field length"))
        );
    }

    #[test]
    fn a_record_that_runs_past_the_body_is_malformed() {
        let kind = BigDataKind::WorkoutSummary;
        assert_eq!(
            WorkoutSummary::from_body(&SUMMARY[..10]),
            Err(malformed(kind, "record length"))
        );
        for len in [0x00, 0x01] {
            assert_eq!(
                WorkoutSummary::from_body(&[0x01, len, 0x07]),
                Err(malformed(kind, "record length"))
            );
        }
        assert_eq!(
            WorkoutSummary::from_body(&[]),
            Err(malformed(kind, "record count"))
        );
        assert_eq!(
            WorkoutSummary::from_body(&[0x02, 0x02, 0x07]),
            Err(malformed(kind, "record count"))
        );
        assert_eq!(
            WorkoutSummary::from_body(&[0x01, 0x02, 0x07, 0x00]),
            Err(malformed(kind, "trailing bytes"))
        );
    }

    #[test]
    fn lengths_the_layout_writes_as_a_byte_do_not_encode_past_it() {
        let record = |fields| WorkoutRecord {
            sport_type: 7,
            fields,
        };
        let too_many = WorkoutSummary {
            records: vec![record(Vec::new()); 256],
        };
        assert_eq!(
            too_many.to_body(),
            Err(WireError::Value {
                what: "workout records"
            })
        );
        let wide_field = WorkoutSummary {
            records: vec![record(vec![field(1, &[0; 254])])],
        };
        assert_eq!(
            wide_field.to_body(),
            Err(WireError::Value {
                what: "workout field length"
            })
        );
        let widest_field = WorkoutSummary {
            records: vec![record(vec![field(1, &[0; 253])])],
        };
        assert_eq!(
            widest_field.to_body(),
            Err(WireError::Value {
                what: "workout record length"
            })
        );
        let widest_record = WorkoutSummary {
            records: vec![record(vec![field(1, &[0; 251])])],
        };
        let body = widest_record
            .to_body()
            .unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(body[1], 0xff);
        assert_eq!(WorkoutSummary::from_body(&body), Ok(widest_record));
    }

    #[test]
    fn fixture_descriptor_is_one_heart_rate_byte_per_second() {
        let descriptor =
            WorkoutDescriptor::from_body(&DESCRIPTOR).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(
            descriptor,
            WorkoutDescriptor {
                status: 0,
                package_count: 1,
                byte2: 0,
                sample_second: 1,
                fields: vec![DescriptorField {
                    len: 1,
                    tag: SampleTag::HeartRate,
                }],
            }
        );
        assert_eq!(descriptor.record_len(), 1);
        assert_eq!(descriptor.offset_of(SampleTag::HeartRate), Some((0, 1)));
        assert_eq!(descriptor.offset_of(SampleTag::Other(15)), None);
        assert_eq!(descriptor.to_body(), DESCRIPTOR);

        let bare =
            WorkoutDescriptor::from_body(&DESCRIPTOR[..4]).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(bare.fields, Vec::new());
        assert_eq!(bare.record_len(), 0);
        assert_eq!(bare.to_body(), DESCRIPTOR[..4]);
    }

    #[test]
    fn a_descriptor_with_a_broken_field_list_is_malformed() {
        let kind = BigDataKind::WorkoutDescriptor;
        assert_eq!(
            WorkoutDescriptor::from_body(&DESCRIPTOR[..5]),
            Err(malformed(kind, "field list"))
        );
        assert_eq!(
            WorkoutDescriptor::from_body(&DESCRIPTOR[..3]),
            Err(malformed(kind, "header"))
        );
        assert_eq!(
            WorkoutDescriptor::from_body(&[]),
            Err(malformed(kind, "header"))
        );
    }

    #[test]
    fn fixture_series_is_fifty_four_heart_rates() {
        let body = series_body();
        assert_eq!(body.len(), 56);
        let series = WorkoutSeries::from_body(&body).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(series.package, 1);
        assert_eq!(series.byte1, 0);
        assert_eq!(series.data.len(), 54);
        assert_eq!(series.to_body(), body);

        let descriptor =
            WorkoutDescriptor::from_body(&DESCRIPTOR).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(series.records(&descriptor).count(), 54);
        let rates = series
            .heart_rates(&descriptor)
            .unwrap_or_else(|| panic!("no heart rate field"));
        assert_eq!(rates, heart_rates());
        assert!(rates.iter().all(|bpm| (57..=62).contains(bpm)));
        assert_eq!(rates.iter().min(), Some(&57));
        assert_eq!(rates.iter().max(), Some(&62));

        assert_eq!(
            WorkoutSeries::from_body(&[0x01]),
            Err(malformed(BigDataKind::WorkoutSeries, "header"))
        );
        assert_eq!(
            WorkoutSeries::from_body(&[0x01, 0x00]),
            Ok(WorkoutSeries {
                package: 1,
                byte1: 0,
                data: Vec::new(),
            })
        );
    }

    #[test]
    fn records_follow_the_descriptor_they_are_given() {
        let series = WorkoutSeries {
            package: 1,
            byte1: 0,
            data: vec![10, 11, 60, 20, 21, 61, 30],
        };
        let two_fields = WorkoutDescriptor {
            status: 0,
            package_count: 1,
            byte2: 0,
            sample_second: 1,
            fields: vec![
                DescriptorField {
                    len: 2,
                    tag: SampleTag::Other(15),
                },
                DescriptorField {
                    len: 1,
                    tag: SampleTag::HeartRate,
                },
            ],
        };
        assert_eq!(two_fields.record_len(), 3);
        assert_eq!(two_fields.offset_of(SampleTag::HeartRate), Some((2, 1)));
        assert_eq!(two_fields.offset_of(SampleTag::Other(15)), Some((0, 2)));
        let records: Vec<&[u8]> = series.records(&two_fields).collect();
        assert_eq!(records, [&[10, 11, 60][..], &[20, 21, 61][..]]);
        assert_eq!(series.heart_rates(&two_fields), Some(vec![60, 61]));

        let no_fields = WorkoutDescriptor {
            fields: Vec::new(),
            ..two_fields.clone()
        };
        assert_eq!(series.records(&no_fields).count(), 0);
        assert_eq!(series.heart_rates(&no_fields), None);

        let empty_rate = WorkoutDescriptor {
            fields: vec![
                DescriptorField {
                    len: 0,
                    tag: SampleTag::HeartRate,
                },
                DescriptorField {
                    len: 1,
                    tag: SampleTag::Other(15),
                },
            ],
            ..two_fields.clone()
        };
        assert_eq!(empty_rate.offset_of(SampleTag::HeartRate), Some((0, 0)));
        assert_eq!(series.records(&empty_rate).count(), 7);
        assert_eq!(series.heart_rates(&empty_rate), None);

        let wide_rate = WorkoutDescriptor {
            fields: vec![DescriptorField {
                len: 2,
                tag: SampleTag::HeartRate,
            }],
            ..two_fields
        };
        assert_eq!(series.heart_rates(&wide_rate), Some(vec![10, 60, 21]));
    }
}
