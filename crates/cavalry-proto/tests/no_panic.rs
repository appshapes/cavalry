//! Every byte string either parses to a header whose bytes round-trip, or is rejected
//! with one of the spec's four reasons. Never a panic (invariant 2; CLAUDE.md gate 1,
//! "frame parsers are fuzzed"; ADR 0008). `just fuzz` runs the same property under
//! cargo-fuzz for longer; this runs on every `just test`.

use cavalry_proto::frame::{DataFrameHeader, Flags, FrameType, HEADER_LENGTH};

/// Deterministic, so a failure reproduces: xorshift64.
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
}

const REASONS: [&str; 4] = ["too-short", "version", "type", "length"];

fn check(bytes: &[u8]) {
    match DataFrameHeader::parse(bytes) {
        Ok(header) => {
            let frame_length = usize::try_from(header.frame_length()).expect("validated");
            assert!(
                frame_length >= HEADER_LENGTH && frame_length <= bytes.len(),
                "{bytes:02x?}"
            );
            assert_eq!(
                &header.to_bytes()[..],
                &bytes[..HEADER_LENGTH],
                "{bytes:02x?}"
            );
            assert_eq!(
                header.body_length() + HEADER_LENGTH,
                frame_length,
                "{bytes:02x?}"
            );
            assert!(header.aligned_length() >= frame_length, "{bytes:02x?}");
            assert_eq!(header.aligned_length() % 32, 0, "{bytes:02x?}");
        }
        Err(error) => assert!(REASONS.contains(&error.reason()), "{bytes:02x?}: {error}"),
    }
}

fn valid_frame() -> [u8; 40] {
    let mut frame = [0u8; 40];
    DataFrameHeader::new(FrameType::Data, Flags::UNFRAGMENTED, 40)
        .expect("40 is a valid length")
        .with_term_offset(64)
        .with_session_id(-2)
        .with_stream_id(1001)
        .with_term_id(7)
        .with_reserved_value(-1)
        .write(&mut frame)
        .expect("40 bytes hold a header");
    frame[HEADER_LENGTH..].copy_from_slice(b"cavalry!");
    frame
}

#[test]
fn random_bytes_never_panic() {
    let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
    let mut buf = [0u8; 96];
    for _ in 0..100_000 {
        let len = (rng.next() % 97) as usize;
        for byte in &mut buf[..len] {
            *byte = rng.next() as u8;
        }
        check(&buf[..len]);
    }
}

#[test]
fn one_byte_mutations_of_a_valid_frame_never_panic() {
    let frame = valid_frame();
    check(&frame);
    for offset in 0..frame.len() {
        for value in [0x00, 0x01, 0x1F, 0x20, 0x7F, 0x80, 0xC0, 0xFE, 0xFF] {
            let mut mutated = frame;
            mutated[offset] = value;
            check(&mutated);
        }
        check(&frame[..offset]);
    }
}

#[test]
fn every_type_value_is_handled_or_rejected() {
    let mut frame = valid_frame();
    for value in 0..=u16::MAX {
        frame[6..8].copy_from_slice(&value.to_le_bytes());
        match DataFrameHeader::parse(&frame) {
            Ok(header) => assert_eq!(header.frame_type() as u16, value),
            Err(error) => assert_eq!(error.reason(), "type", "type {value}: {error}"),
        }
    }
}

#[test]
fn every_version_but_zero_is_rejected() {
    let mut frame = valid_frame();
    for value in 0..=u8::MAX {
        frame[4] = value;
        match DataFrameHeader::parse(&frame) {
            Ok(_) => assert_eq!(value, 0),
            Err(error) => assert_eq!(error.reason(), "version", "version {value}: {error}"),
        }
    }
}
