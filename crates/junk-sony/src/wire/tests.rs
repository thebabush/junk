use super::*;
use alloc::vec;
use core::num::NonZeroUsize;
use junk_core::{FrameLen, Framing};

use crate::SonyFraming;

fn frame(data_type: DataType, seq: u8, payload: &[u8]) -> Frame {
    Frame::new(data_type, seq, payload.to_vec())
}

fn encoded(f: &Frame) -> Vec<u8> {
    f.encode().expect("small payload")
}

/// INIT request: type `0C`, seq 0, payload `00 00`, checksum `0C + 2 = 0E`.
const INIT: [u8; 11] = [
    0x3e, 0x0c, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x0e, 0x3c,
];

/// NC set: payload `68 02 11 02 02 01 00 00`, checksum `0C + 8 + 68 + 02 + 11 + 02 + 02 +
/// 01 = 94`.
const NC_SET: [u8; 17] = [
    0x3e, 0x0c, 0x00, 0x00, 0x00, 0x00, 0x08, 0x68, 0x02, 0x11, 0x02, 0x02, 0x01, 0x00, 0x00, 0x94,
    0x3c,
];

const ACK_SEQ0: [u8; 9] = [0x3e, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x3c];
const ACK_SEQ1: [u8; 9] = [0x3e, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x02, 0x3c];

#[test]
fn the_init_request_encodes_and_decodes() {
    let init = frame(DataType::DataMdr, 0, &[0x00, 0x00]);
    assert_eq!(encoded(&init), INIT);
    assert_eq!(Frame::decode(&INIT), Ok(init));
}

#[test]
fn the_nc_set_request_encodes_and_decodes() {
    let nc = frame(
        DataType::DataMdr,
        0,
        &[0x68, 0x02, 0x11, 0x02, 0x02, 0x01, 0x00, 0x00],
    );
    assert_eq!(encoded(&nc), NC_SET);
    assert_eq!(Frame::decode(&NC_SET), Ok(nc));
}

#[test]
fn the_three_markers_escape_to_their_known_pairs() {
    assert_eq!(
        escape(&[0x3e, 0x3c, 0x3d]),
        [0x3d, 0x2e, 0x3d, 0x2c, 0x3d, 0x2d]
    );
    assert_eq!(
        unescape(&[0x3d, 0x2e, 0x3d, 0x2c, 0x3d, 0x2d]),
        Ok(vec![0x3e, 0x3c, 0x3d])
    );
    assert_eq!(
        escape(&[0x00, 0x2c, 0x3b, 0x3f, 0xff]),
        [0x00, 0x2c, 0x3b, 0x3f, 0xff]
    );
}

#[test]
fn a_trailing_escape_byte_is_an_error() {
    assert_eq!(unescape(&[0x01, 0x3d]), Err(FrameError::BadEscape));
    assert_eq!(unescape(&[0x3d]), Err(FrameError::BadEscape));
    assert_eq!(unescape(&[]), Ok(vec![]));
    // `3D 3D` is an escaped `3D | 10 = 3D`, not an error: the app does the same.
    assert_eq!(unescape(&[0x3d, 0x3d]), Ok(vec![0x3d]));
}

/// Payload `3C 3D`, type `0C`, seq 0, length 2.
///
/// Checksum, hand-added on the unescaped bytes: `0C + 00 + 00 + 00 + 00 + 02 = 0E`, then
/// `0E + 3C = 4A`, then `4A + 3D = 87`. The body on the wire is the escaped
/// `0C 00 00 00 00 02 3D 2C 3D 2D 87`.
#[test]
fn a_payload_with_markers_is_escaped_and_checksummed_unescaped() {
    let wire = [
        0x3e, 0x0c, 0x00, 0x00, 0x00, 0x00, 0x02, 0x3d, 0x2c, 0x3d, 0x2d, 0x87, 0x3c,
    ];
    let f = frame(DataType::DataMdr, 0, &[0x3c, 0x3d]);
    assert_eq!(encoded(&f), wire);
    assert_eq!(Frame::decode(&wire), Ok(f));
}

/// The bug in an existing client: summing the bytes as they arrive, escapes included.
/// The frame above sums to something else that way, so a decoder that did it would answer
/// `Checksum` where this one answers `Ok`.
#[test]
fn the_checksum_is_over_unescaped_bytes_not_the_wire_bytes() {
    let wire = encoded(&frame(DataType::DataMdr, 0, &[0x3c, 0x3d]));
    let body = &wire[1..wire.len() - 1];
    let escaped_sum = checksum(&body[..body.len() - 1]);
    assert_eq!(escaped_sum, 0x0e_u8.wrapping_add(0x3d + 0x2c + 0x3d + 0x2d));
    assert_ne!(escaped_sum, body[body.len() - 1]);
    assert!(Frame::decode(&wire).is_ok());
}

/// A frame of type `0C`, seq 0 and a one-byte payload `p` has checksum `0D + p`, so `p` of
/// `2F`, `30` or `31` makes the checksum `3C`, `3D` or `3E`. The checksum byte is escaped
/// like any other, and the frame ends in the escape pair and then the real end marker.
#[test]
fn a_checksum_that_is_a_marker_is_escaped_and_unescaped() {
    let cases = [
        (0x2f, [0x3d, 0x2c]),
        (0x30, [0x3d, 0x2d]),
        (0x31, [0x3d, 0x2e]),
    ];
    for (payload, escaped_sum) in cases {
        let f = frame(DataType::DataMdr, 0, &[payload]);
        let mut wire = vec![0x3e, 0x0c, 0x00, 0x00, 0x00, 0x00, 0x01, payload];
        wire.extend_from_slice(&escaped_sum);
        wire.push(0x3c);
        assert_eq!(encoded(&f), wire, "payload {payload:02x}");
        assert_eq!(Frame::decode(&wire), Ok(f), "payload {payload:02x}");
        assert_eq!(
            SonyFraming.frame_len(&wire),
            FrameLen::Len(NonZeroUsize::new(wire.len()).expect("not empty"))
        );
    }
}

#[test]
fn the_ack_frames_are_the_known_good_bytes() {
    // Received seq 1 is answered with seq 0, and the other way round.
    assert_eq!(encoded(&Frame::ack_for(1).expect("valid")), ACK_SEQ0);
    assert_eq!(encoded(&Frame::ack_for(0).expect("valid")), ACK_SEQ1);
    assert_eq!(Frame::decode(&ACK_SEQ0), Ok(frame(DataType::Ack, 0, &[])));
    assert_eq!(Frame::decode(&ACK_SEQ1), Ok(frame(DataType::Ack, 1, &[])));
    for seq in 2..=u8::MAX {
        assert_eq!(Frame::ack_for(seq), None, "seq {seq}");
    }
}

#[test]
fn the_data_type_table_round_trips_every_byte() {
    for raw in 0..=u8::MAX {
        assert_eq!(DataType::from_raw(raw).raw(), raw);
    }
    assert_eq!(DataType::from_raw(0x0c), DataType::DataMdr);
    assert_eq!(DataType::from_raw(0x2d), DataType::LargeDataCommon);
    assert_eq!(DataType::from_raw(0x99), DataType::Other(0x99));
    // A named variant is never also reachable as `Other`.
    for raw in 0..=u8::MAX {
        if let DataType::Other(inner) = DataType::from_raw(raw) {
            assert_eq!(inner, raw);
        }
    }
}

#[test]
fn only_ack_and_the_shots_go_unacknowledged() {
    let no_ack = [0x01, 0x10, 0x12, 0x19, 0x1a, 0x1c, 0x1d, 0x1e];
    let ack = [0x00, 0x02, 0x07, 0x09, 0x0a, 0x0c, 0x0d, 0x0e, 0x27, 0x2d];
    for raw in no_ack {
        assert!(!DataType::from_raw(raw).needs_ack(), "{raw:02x}");
    }
    for raw in ack {
        assert!(DataType::from_raw(raw).needs_ack(), "{raw:02x}");
    }
    assert!(!DataType::Other(0x99).needs_ack());
}

#[test]
fn an_unknown_type_byte_decodes_and_round_trips() {
    let f = frame(DataType::Other(0x99), 1, &[0x01]);
    let wire = encoded(&f);
    assert_eq!(wire[1], 0x99);
    assert_eq!(Frame::decode(&wire), Ok(f));
}

#[test]
fn a_length_that_does_not_match_is_an_error_either_way() {
    // Declares three payload bytes, carries two: checksum fixed up so only the length is wrong.
    let mut raw = vec![0x0c, 0x00, 0, 0, 0, 3, 0xaa, 0xbb];
    raw.push(checksum(&raw));
    let mut wire = vec![START];
    wire.extend_from_slice(&escape(&raw));
    wire.push(END);
    assert_eq!(
        Frame::decode(&wire),
        Err(FrameError::Length {
            declared: 3,
            got: 2
        })
    );

    // Declares one, carries two: the app would truncate; this does not.
    let mut raw = vec![0x0c, 0x00, 0, 0, 0, 1, 0xaa, 0xbb];
    raw.push(checksum(&raw));
    let mut wire = vec![START];
    wire.extend_from_slice(&escape(&raw));
    wire.push(END);
    assert_eq!(
        Frame::decode(&wire),
        Err(FrameError::Length {
            declared: 1,
            got: 2
        })
    );

    // A declared length that cannot fit is refused without allocating for it.
    let mut raw = vec![0x0c, 0x00, 0xff, 0xff, 0xff, 0xff];
    raw.push(checksum(&raw));
    let mut wire = vec![START];
    wire.extend_from_slice(&escape(&raw));
    wire.push(END);
    assert_eq!(
        Frame::decode(&wire),
        Err(FrameError::Length {
            declared: u32::MAX,
            got: 0
        })
    );
}

#[test]
fn a_bad_checksum_is_its_own_error() {
    let mut wire = INIT;
    wire[9] = 0x0f;
    assert_eq!(
        Frame::decode(&wire),
        Err(FrameError::Checksum {
            expected: 0x0e,
            got: 0x0f
        })
    );
    assert_eq!(
        ProtoError::from(FrameError::Checksum {
            expected: 0,
            got: 1
        }),
        ProtoError::Checksum
    );
    assert!(matches!(
        ProtoError::from(FrameError::MissingEnd),
        ProtoError::Malformed(_)
    ));
}

#[test]
fn malformed_frames_are_refused_with_a_reason() {
    assert_eq!(Frame::decode(&[]), Err(FrameError::TooShort { got: 0 }));
    assert_eq!(Frame::decode(&[0x3e]), Err(FrameError::TooShort { got: 1 }));
    assert_eq!(Frame::decode(&[0x00, 0x3c]), Err(FrameError::MissingStart));
    assert_eq!(
        Frame::decode(&INIT[..INIT.len() - 1]),
        Err(FrameError::MissingEnd)
    );
    // Empty body, and a body that is only a header.
    assert_eq!(
        Frame::decode(&[0x3e, 0x3c]),
        Err(FrameError::TooShort { got: 0 })
    );
    assert_eq!(
        Frame::decode(&[0x3e, 0x0c, 0, 0, 0, 0, 0, 0x3c]),
        Err(FrameError::TooShort { got: 6 })
    );
    // An unescaped marker inside, and two frames glued together.
    let mut bad = INIT;
    bad[7] = 0x3e;
    assert_eq!(Frame::decode(&bad), Err(FrameError::StrayMarker { at: 7 }));
    let mut two = INIT.to_vec();
    two.extend_from_slice(&ACK_SEQ0);
    assert!(matches!(
        Frame::decode(&two),
        Err(FrameError::StrayMarker { .. })
    ));
    // The body ends in an escape byte.
    assert_eq!(
        Frame::decode(&[0x3e, 0x0c, 0, 0, 0, 0, 0, 0x3d, 0x3c]),
        Err(FrameError::BadEscape)
    );
}

/// A small deterministic generator; no `rand` dependency.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u8 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        u8::try_from((self.0 >> 33) & 0xff).expect("masked")
    }
}

/// Bytes biased towards the ones that matter: markers, escapes, zeros.
fn pick(rng: &mut Lcg) -> u8 {
    match rng.next() % 8 {
        0 => 0x3c,
        1 => 0x3d,
        2 => 0x3e,
        3 => 0x00,
        _ => rng.next(),
    }
}

#[test]
fn no_input_panics_decode_or_the_framing() {
    let check = |bytes: &[u8]| {
        let _ = Frame::decode(bytes);
        let _ = unescape(bytes);
        let _ = SonyFraming.frame_len(bytes);
    };
    check(&[]);
    for a in 0..=u8::MAX {
        check(&[a]);
        for b in 0..=u8::MAX {
            check(&[a, b]);
        }
    }
    let mut rng = Lcg(0x5eed);
    for round in 0..20_000_usize {
        let len = 3 + round % 40;
        let mut bytes: Vec<u8> = (0..len).map(|_| pick(&mut rng)).collect();
        check(&bytes);
        // The same, dressed as a frame so the inner checks are reached.
        bytes.insert(0, START);
        bytes.push(END);
        check(&bytes);
    }
}

#[test]
fn random_frames_round_trip_and_are_cut_at_their_own_end() {
    let mut rng = Lcg(42);
    for round in 0..2_000_usize {
        let payload: Vec<u8> = (0..round % 50).map(|_| pick(&mut rng)).collect();
        let f = Frame::new(DataType::from_raw(rng.next()), rng.next() & 1, payload);
        let wire = encoded(&f);
        assert!(
            wire[1..wire.len() - 1]
                .iter()
                .all(|&b| b != START && b != END)
        );
        assert_eq!(Frame::decode(&wire), Ok(f));
        assert_eq!(
            SonyFraming.frame_len(&wire),
            FrameLen::Len(NonZeroUsize::new(wire.len()).expect("not empty"))
        );
    }
}
