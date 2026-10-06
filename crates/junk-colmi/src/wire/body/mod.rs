//! Typed bodies of the `0xbc` big-data frames: what a [`BigData`]'s body means, per kind,
//! in both directions.
//!
//! [`RequestBody`] is what the host writes on V2 cmd and [`ReplyBody`] what the ring
//! notifies on V2 notify. Each is an enum with one variant per documented body shape,
//! decoded *from* a [`BigData`] and encoded back into one. The `match` on [`BigDataKind`]
//! in each decoder is exhaustive, so a kind byte that gains a [`BigDataKind`] variant is a
//! compile error here until it is given a body (SPEC §3.2). A kind whose body is only
//! documented in the other direction, and every [`BigDataKind::Other`], decodes as `Raw`.
//! Decoding is lossless on the fixtures: every complete frame the two captures carry
//! re-encodes byte for byte, and a byte the docs do not explain is kept in its type rather
//! than dropped.
//!
//! No sequencing lives here: a body is decoded once a collector has the whole frame, and
//! pairing a reply with its request is the protocol layer's job. Nor do model types: a
//! [`SleepDay`] counts minutes from a midnight that only the protocol layer can place, with
//! the day and UTC offset the request was made under.
//!
//! Byte layouts in the docs below are the body, what follows the six-byte header;
//! multi-byte numbers are little-endian. Every reply layout was verified byte-exact against
//! the app database or the `PacketLogger` capture (`docs/colmi-protocol.md`).

mod sleep;
mod spo2;
mod temperature;
mod workout;

use alloc::vec;
use alloc::vec::Vec;
use core::fmt;
use core::ops::RangeInclusive;

use crate::wire::{BigData, BigDataKind, WireError};

pub use sleep::{SleepBody, SleepDay};
pub use spo2::{Spo2Body, Spo2Day, Spo2Hour};
pub use temperature::{TemperatureBody, TemperatureDay};
pub use workout::{
    DescriptorField, SampleTag, WorkoutDescriptor, WorkoutField, WorkoutRecord, WorkoutSeries,
    WorkoutSummary, WorkoutTag,
};

/// How long a sleep request's selector may be: one byte per the R09 decompilation, two as
/// `QRing` sends it.
const SLEEP_SELECTOR_LEN: RangeInclusive<usize> = 1..=2;

/// Why a frame with a named [`BigDataKind`] did not decode as a body.
///
/// A frame whose kind is [`BigDataKind::Other`], or whose kind has no body shape in the
/// direction being decoded, never fails: it decodes as the `Raw` variant of either enum.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub enum BodyError {
    /// The body does not fit the layout the kind has in this direction.
    Malformed {
        /// The kind byte.
        kind: BigDataKind,
        /// What did not fit: a day count, a day block, a field list, …
        what: &'static str,
    },
}

impl fmt::Display for BodyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BodyError::Malformed { kind, what } => {
                write!(
                    f,
                    "malformed {what} in {kind:?} body ({:#04x})",
                    kind.byte()
                )
            }
        }
    }
}

impl core::error::Error for BodyError {}

/// A big-data request the host writes on V2 cmd, as a typed value.
///
/// [`RequestBody::encode`] builds the [`BigData`]; [`RequestBody::decode`] reads one back,
/// so a recorded trace's `tx` lines can be typed and re-encoded. Every variant's doc gives
/// its body. The reply to each is the [`ReplyBody`] variant of the same kind.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub enum RequestBody {
    /// `0x27`: ask for sleep. Body one or two bytes: `QRing` sends `06 01` on the first
    /// sync of a session and `01 01` afterwards; the R09 decompilation reads a one-byte
    /// body as `00` = today, `ff` = all history. What the two bytes mean is not settled,
    /// so they are kept as sent.
    Sleep(Vec<u8>),
    /// `0x2a`: ask for `SpO2`. Body `<selector>`: `ff` on the first sync of a session (all
    /// history, per the R09 decompilation) and `02` afterwards.
    Spo2(u8),
    /// `0x25`: ask for temperature. Body `<selector>`: `00` = today.
    Temperature(u8),
    /// `0x30`: ask for the list of files on the ring. Empty body.
    FileList,
    /// `0x41`: ask for the workout records since a timestamp. Body `<since u32>`
    /// (`00 00 00 00` from thering, `4e 74 21 69` from `QRing`).
    WorkoutList {
        /// The cursor: records from this ring-clock timestamp on.
        since: u32,
    },
    /// `0x43`: ask for the detail of one workout. Body `<sport_type> <start u32>`
    /// (`07 80 29 1d 69` = sport 7, the start the summary record gave).
    WorkoutDetail {
        /// The record's sport, as its summary gave it.
        sport_type: u8,
        /// The record's start, as its summary gave it.
        start: u32,
    },
    /// Any frame with no request shape: every [`BigDataKind::Other`], a reply-only kind
    /// (`0x42`, `0x44`, `0x45`), and `0x28`, whose request no fixture carries.
    Raw {
        /// The kind byte.
        kind: BigDataKind,
        /// The body, undecoded.
        body: Vec<u8>,
    },
}

impl RequestBody {
    /// The typed value of `big`, read from its kind and body.
    ///
    /// Total for a [`BigDataKind::Other`] kind and for the kinds with no request shape,
    /// which come back as [`RequestBody::Raw`].
    ///
    /// # Errors
    ///
    /// [`BodyError::Malformed`] when a named kind's body is not the length its request
    /// takes.
    pub fn decode(big: &BigData) -> Result<RequestBody, BodyError> {
        let body = big.body();
        let malformed = |what| BodyError::Malformed {
            kind: big.kind,
            what,
        };
        match big.kind {
            BigDataKind::Temperature => match *body {
                [selector] => Ok(RequestBody::Temperature(selector)),
                _ => Err(malformed("selector")),
            },
            BigDataKind::Sleep if SLEEP_SELECTOR_LEN.contains(&body.len()) => {
                Ok(RequestBody::Sleep(body.to_vec()))
            }
            BigDataKind::Sleep => Err(malformed("selector")),
            BigDataKind::Spo2 => match *body {
                [selector] => Ok(RequestBody::Spo2(selector)),
                _ => Err(malformed("selector")),
            },
            BigDataKind::FileList if body.is_empty() => Ok(RequestBody::FileList),
            BigDataKind::FileList => Err(malformed("body")),
            BigDataKind::WorkoutList => match *body {
                [t0, t1, t2, t3] => Ok(RequestBody::WorkoutList {
                    since: u32::from_le_bytes([t0, t1, t2, t3]),
                }),
                _ => Err(malformed("since")),
            },
            BigDataKind::WorkoutDetail => match *body {
                [sport_type, t0, t1, t2, t3] => Ok(RequestBody::WorkoutDetail {
                    sport_type,
                    start: u32::from_le_bytes([t0, t1, t2, t3]),
                }),
                _ => Err(malformed("sport and start")),
            },
            BigDataKind::ManualHr
            | BigDataKind::WorkoutSummary
            | BigDataKind::WorkoutDescriptor
            | BigDataKind::WorkoutSeries
            | BigDataKind::Other(_) => Ok(RequestBody::Raw {
                kind: big.kind,
                body: body.to_vec(),
            }),
        }
    }

    /// The frame for this value.
    ///
    /// # Errors
    ///
    /// [`WireError::Value`] for a [`RequestBody::Sleep`] selector that is not one or two
    /// bytes; [`WireError::PayloadTooLong`] for a [`RequestBody::Raw`] body longer than
    /// [`BigData::MAX_BODY_LEN`].
    pub fn encode(&self) -> Result<BigData, WireError> {
        match self {
            RequestBody::Sleep(selector) => {
                if !SLEEP_SELECTOR_LEN.contains(&selector.len()) {
                    return Err(WireError::Value {
                        what: "sleep selector",
                    });
                }
                BigData::new(BigDataKind::Sleep, selector.clone())
            }
            RequestBody::Spo2(selector) => BigData::new(BigDataKind::Spo2, vec![*selector]),
            RequestBody::Temperature(selector) => {
                BigData::new(BigDataKind::Temperature, vec![*selector])
            }
            RequestBody::FileList => BigData::new(BigDataKind::FileList, Vec::new()),
            RequestBody::WorkoutList { since } => {
                BigData::new(BigDataKind::WorkoutList, since.to_le_bytes().to_vec())
            }
            RequestBody::WorkoutDetail { sport_type, start } => {
                let mut body = vec![*sport_type];
                body.extend_from_slice(&start.to_le_bytes());
                BigData::new(BigDataKind::WorkoutDetail, body)
            }
            RequestBody::Raw { kind, body } => BigData::new(*kind, body.clone()),
        }
    }
}

/// A big-data reply the ring notifies on V2 notify, as a typed value.
///
/// [`ReplyBody::decode`] reads one from a complete [`BigData`] and is total for
/// well-formed input; [`ReplyBody::encode`] builds the frame back, for the peer simulator
/// and replay tooling. Every variant's doc, or its type's, gives the body.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub enum ReplyBody {
    /// `0x27`: sleep sessions, one block per day.
    Sleep(SleepBody),
    /// `0x2a`: hourly `SpO2`, one block per day.
    Spo2(Spo2Body),
    /// `0x25`: half-hourly skin temperature, one block per day.
    Temperature(TemperatureBody),
    /// `0x30`: the list of files on the ring, undecoded. The app reads a leading
    /// count/status byte then length-prefixed names; the fixture body is `00`, nothing
    /// stored, which is all this ring has shown.
    FileList(Vec<u8>),
    /// `0x42`: the workout records since the requested timestamp, as TLV summaries.
    WorkoutSummary(WorkoutSummary),
    /// `0x44`: how one workout's detail samples are laid out.
    WorkoutDescriptor(WorkoutDescriptor),
    /// `0x45`: one package of detail samples.
    WorkoutSeries(WorkoutSeries),
    /// Any frame with no reply shape: every [`BigDataKind::Other`], a request-only kind
    /// (`0x41`, `0x43`), and `0x28`, whose reply no fixture carries.
    Raw {
        /// The kind byte.
        kind: BigDataKind,
        /// The body, undecoded.
        body: Vec<u8>,
    },
}

impl ReplyBody {
    /// The typed value of `big`, read from its kind and body.
    ///
    /// Total for a [`BigDataKind::Other`] kind and for the kinds with no reply shape, which
    /// come back as [`ReplyBody::Raw`].
    ///
    /// # Errors
    ///
    /// [`BodyError::Malformed`] when a named kind's body does not fit its layout: a day
    /// block whose length byte disagrees with what follows, an odd number of stage bytes, a
    /// day count that does not match the blocks, a body that does not divide into day
    /// blocks, a record or field whose length runs past its container, a descriptor whose
    /// field list is not whole pairs.
    pub fn decode(big: &BigData) -> Result<ReplyBody, BodyError> {
        let body = big.body();
        match big.kind {
            BigDataKind::Temperature => {
                TemperatureBody::from_body(body).map(ReplyBody::Temperature)
            }
            BigDataKind::Sleep => SleepBody::from_body(body).map(ReplyBody::Sleep),
            BigDataKind::Spo2 => Spo2Body::from_body(body).map(ReplyBody::Spo2),
            BigDataKind::FileList => Ok(ReplyBody::FileList(body.to_vec())),
            BigDataKind::WorkoutSummary => {
                WorkoutSummary::from_body(body).map(ReplyBody::WorkoutSummary)
            }
            BigDataKind::WorkoutDescriptor => {
                WorkoutDescriptor::from_body(body).map(ReplyBody::WorkoutDescriptor)
            }
            BigDataKind::WorkoutSeries => {
                WorkoutSeries::from_body(body).map(ReplyBody::WorkoutSeries)
            }
            BigDataKind::ManualHr
            | BigDataKind::WorkoutList
            | BigDataKind::WorkoutDetail
            | BigDataKind::Other(_) => Ok(ReplyBody::Raw {
                kind: big.kind,
                body: body.to_vec(),
            }),
        }
    }

    /// The frame for this value.
    ///
    /// # Errors
    ///
    /// [`WireError::Value`] for a count the layout writes as one byte and cannot: more
    /// than 255 sleep days or workout records, a sleep day of more than 125 stages, a
    /// workout record or field longer than 255 bytes. [`WireError::PayloadTooLong`] for a
    /// body longer than [`BigData::MAX_BODY_LEN`].
    pub fn encode(&self) -> Result<BigData, WireError> {
        match self {
            ReplyBody::Sleep(sleep) => BigData::new(BigDataKind::Sleep, sleep.to_body()?),
            ReplyBody::Spo2(spo2) => BigData::new(BigDataKind::Spo2, spo2.to_body()),
            ReplyBody::Temperature(temperature) => {
                BigData::new(BigDataKind::Temperature, temperature.to_body())
            }
            ReplyBody::FileList(body) => BigData::new(BigDataKind::FileList, body.clone()),
            ReplyBody::WorkoutSummary(summary) => {
                BigData::new(BigDataKind::WorkoutSummary, summary.to_body()?)
            }
            ReplyBody::WorkoutDescriptor(descriptor) => {
                BigData::new(BigDataKind::WorkoutDescriptor, descriptor.to_body())
            }
            ReplyBody::WorkoutSeries(series) => {
                BigData::new(BigDataKind::WorkoutSeries, series.to_body())
            }
            ReplyBody::Raw { kind, body } => BigData::new(*kind, body.clone()),
        }
    }
}

/// `bytes` as whole `N`-byte arrays, in order. A tail shorter than `N` is left out, so a
/// caller that needs the whole input checks its length first.
fn arrays<const N: usize>(bytes: &[u8]) -> impl Iterator<Item = [u8; N]> + '_ {
    bytes.as_chunks::<N>().0.iter().copied()
}

/// `value` as one byte, or [`WireError::Value`] naming `what` if it needs more.
fn byte_of(value: usize, what: &'static str) -> Result<u8, WireError> {
    u8::try_from(value).map_err(|_| WireError::Value { what })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    /// A frame of `kind` carrying `body`.
    fn big(kind: BigDataKind, body: &[u8]) -> BigData {
        BigData::new(kind, body.to_vec()).unwrap_or_else(|err| panic!("{err}"))
    }

    /// `body` under `kind` decodes to `value` as a request, and `value` encodes back to
    /// exactly that frame.
    fn check_request(kind: BigDataKind, body: &[u8], value: RequestBody) {
        let frame = big(kind, body);
        assert_eq!(value.encode(), Ok(frame.clone()), "{value:?}");
        assert_eq!(RequestBody::decode(&frame), Ok(value), "{body:02x?}");
    }

    #[test]
    fn body_error_names_the_kind_and_the_field() {
        let text = BodyError::Malformed {
            kind: BigDataKind::Sleep,
            what: "day count",
        }
        .to_string();
        assert!(text.contains("Sleep"), "{text}");
        assert!(text.contains("0x27"), "{text}");
        assert!(text.contains("day count"), "{text}");
        let other = BodyError::Malformed {
            kind: BigDataKind::Other(0x47),
            what: "day blocks",
        }
        .to_string();
        assert!(other.contains("0x47"), "{other}");
    }

    #[test]
    fn fixture_requests_round_trip() {
        check_request(
            BigDataKind::Sleep,
            &[0x06, 0x01],
            RequestBody::Sleep(vec![6, 1]),
        );
        check_request(
            BigDataKind::Sleep,
            &[0x01, 0x01],
            RequestBody::Sleep(vec![1, 1]),
        );
        check_request(BigDataKind::Sleep, &[0xff], RequestBody::Sleep(vec![0xff]));
        check_request(BigDataKind::Spo2, &[0xff], RequestBody::Spo2(0xff));
        check_request(BigDataKind::Spo2, &[0x02], RequestBody::Spo2(2));
        check_request(
            BigDataKind::Temperature,
            &[0x00],
            RequestBody::Temperature(0),
        );
        check_request(BigDataKind::FileList, &[], RequestBody::FileList);
        check_request(
            BigDataKind::WorkoutList,
            &[0x4e, 0x74, 0x21, 0x69],
            RequestBody::WorkoutList { since: 0x6921_744e },
        );
        check_request(
            BigDataKind::WorkoutList,
            &[0, 0, 0, 0],
            RequestBody::WorkoutList { since: 0 },
        );
        check_request(
            BigDataKind::WorkoutDetail,
            &[0x07, 0x80, 0x29, 0x1d, 0x69],
            RequestBody::WorkoutDetail {
                sport_type: 7,
                start: 0x691d_2980,
            },
        );
    }

    #[test]
    fn a_request_of_the_wrong_length_is_malformed() {
        let malformed = |kind, what| {
            Err(BodyError::Malformed {
                kind: BigDataKind::from_byte(kind),
                what,
            })
        };
        let decode =
            |kind, body: &[u8]| RequestBody::decode(&big(BigDataKind::from_byte(kind), body));
        assert_eq!(decode(0x25, &[]), malformed(0x25, "selector"));
        assert_eq!(decode(0x25, &[0, 0]), malformed(0x25, "selector"));
        assert_eq!(decode(0x27, &[]), malformed(0x27, "selector"));
        assert_eq!(decode(0x27, &[1, 1, 1]), malformed(0x27, "selector"));
        assert_eq!(decode(0x2a, &[2, 2]), malformed(0x2a, "selector"));
        assert_eq!(decode(0x30, &[0]), malformed(0x30, "body"));
        assert_eq!(decode(0x41, &[0, 0, 0]), malformed(0x41, "since"));
        assert_eq!(decode(0x41, &[0; 5]), malformed(0x41, "since"));
        assert_eq!(decode(0x43, &[7; 4]), malformed(0x43, "sport and start"));
        assert_eq!(decode(0x43, &[7; 6]), malformed(0x43, "sport and start"));
    }

    #[test]
    fn a_sleep_selector_encodes_only_at_one_or_two_bytes() {
        for selector in [Vec::new(), vec![1, 1, 1]] {
            assert_eq!(
                RequestBody::Sleep(selector).encode(),
                Err(WireError::Value {
                    what: "sleep selector"
                })
            );
        }
    }

    #[test]
    fn kinds_without_a_shape_in_a_direction_are_raw() {
        for byte in [0x28, 0x42, 0x44, 0x45, 0x00, 0x3a, 0x47, 0xff] {
            let frame = big(BigDataKind::from_byte(byte), &[1, 2, 3]);
            assert_eq!(
                RequestBody::decode(&frame),
                Ok(RequestBody::Raw {
                    kind: frame.kind,
                    body: vec![1, 2, 3],
                }),
                "{byte:#04x}"
            );
            let raw = RequestBody::decode(&frame).unwrap_or_else(|err| panic!("{err}"));
            assert_eq!(raw.encode(), Ok(frame));
        }
        for byte in [0x28, 0x41, 0x43, 0x00, 0x3a, 0x47, 0xff] {
            let frame = big(BigDataKind::from_byte(byte), &[1, 2, 3]);
            assert_eq!(
                ReplyBody::decode(&frame),
                Ok(ReplyBody::Raw {
                    kind: frame.kind,
                    body: vec![1, 2, 3],
                }),
                "{byte:#04x}"
            );
            let raw = ReplyBody::decode(&frame).unwrap_or_else(|err| panic!("{err}"));
            assert_eq!(raw.encode(), Ok(frame));
        }
    }

    #[test]
    fn the_file_list_reply_is_kept_raw() {
        let frame = big(BigDataKind::FileList, &[0x00]);
        let list = ReplyBody::decode(&frame);
        assert_eq!(list, Ok(ReplyBody::FileList(vec![0x00])));
        assert_eq!(ReplyBody::FileList(vec![0x00]).encode(), Ok(frame));
        assert_eq!(
            ReplyBody::decode(&big(BigDataKind::FileList, &[])),
            Ok(ReplyBody::FileList(Vec::new()))
        );
    }

    #[test]
    fn arrays_yields_whole_chunks_only() {
        let bytes = [1, 2, 3, 4, 5];
        assert_eq!(arrays::<2>(&bytes).collect::<Vec<_>>(), [[1, 2], [3, 4]]);
        assert_eq!(arrays::<5>(&bytes).collect::<Vec<_>>(), [[1, 2, 3, 4, 5]]);
        assert_eq!(arrays::<6>(&bytes).count(), 0);
        assert_eq!(arrays::<1>(&[]).count(), 0);
        assert_eq!(byte_of(255, "count"), Ok(255));
        assert_eq!(
            byte_of(256, "count"),
            Err(WireError::Value { what: "count" })
        );
    }

    /// Xorshift64: enough randomness to walk the decoders' branches, no dev-dependency.
    struct XorShift(u64);

    impl XorShift {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }

        fn byte(&mut self) -> u8 {
            // Truncation is the point: one byte of the state.
            (self.next() & 0xff) as u8
        }
    }

    /// Every kind byte the decoders name, plus one they do not.
    const KINDS: [BigDataKind; 11] = [
        BigDataKind::Temperature,
        BigDataKind::Sleep,
        BigDataKind::ManualHr,
        BigDataKind::Spo2,
        BigDataKind::FileList,
        BigDataKind::WorkoutList,
        BigDataKind::WorkoutSummary,
        BigDataKind::WorkoutDetail,
        BigDataKind::WorkoutDescriptor,
        BigDataKind::WorkoutSeries,
        BigDataKind::Other(0x47),
    ];

    // Invariant 5, at the body layer: no body of length 0..=120 under any kind makes
    // either decoder panic, and whatever does decode re-encodes to the frame it came from.
    #[test]
    fn decoders_never_panic_on_random_bodies_and_are_lossless() {
        let mut rng = XorShift(0x9e37_79b9_7f4a_7c15);
        let mut replies = 0;
        let mut spo2 = 0;
        for _ in 0..20_000 {
            let len = usize::try_from(rng.next() % 121).unwrap_or(0);
            let body: Vec<u8> = (0..len).map(|_| rng.byte()).collect();
            for kind in KINDS {
                let frame = big(kind, &body);
                if let Ok(request) = RequestBody::decode(&frame) {
                    assert_eq!(request.encode(), Ok(frame.clone()), "{request:?}");
                }
                if let Ok(reply) = ReplyBody::decode(&frame) {
                    assert_eq!(reply.encode(), Ok(frame.clone()), "{reply:?}");
                    if !matches!(reply, ReplyBody::Raw { .. } | ReplyBody::FileList(_)) {
                        replies += 1;
                    }
                    if matches!(reply, ReplyBody::Spo2(_)) {
                        spo2 += 1;
                    }
                }
            }
        }
        // Lengths 0, 49 and 98 all decode as SpO2, so success paths were walked too.
        assert!(spo2 >= 100, "{spo2} SpO2 bodies");
        assert!(replies > spo2, "{replies} typed replies");
    }
}
