//! Runs every vector in `spec/vectors/frame.json` against this crate: a valid vector
//! decodes to its fields, those fields encode to the same bytes, and an invalid vector
//! is rejected for the named reason (`spec/README.md`, "Conventions"; ADR 0008).
//! `just conformance` runs this test alone; `just test` runs it with the rest.

use cavalry_proto::frame::{DataFrameHeader, Flags, FrameType, HEADER_LENGTH};
use serde_json::Value;

const VECTORS: &str = include_str!("../../../spec/vectors/frame.json");

fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("vector bytes are hex"))
        .collect()
}

fn field(header: &Value, name: &str) -> i64 {
    header[name]
        .as_i64()
        .unwrap_or_else(|| panic!("vector header lacks {name}"))
}

fn build(header: &Value) -> DataFrameHeader {
    let frame_type =
        FrameType::try_from(u16::try_from(field(header, "type")).expect("type fits u16"))
            .expect("a valid vector's type is one this crate handles");
    let flags = Flags::from_bits(u8::try_from(field(header, "flags")).expect("flags fit a byte"));
    let frame_length = usize::try_from(field(header, "frame_length")).expect("length is positive");
    DataFrameHeader::new(frame_type, flags, frame_length)
        .expect("a valid vector's length is at least the header")
        .with_term_offset(field(header, "term_offset") as i32)
        .with_session_id(field(header, "session_id") as i32)
        .with_stream_id(field(header, "stream_id") as i32)
        .with_term_id(field(header, "term_id") as i32)
        .with_reserved_value(field(header, "reserved_value"))
}

#[test]
fn every_vector_in_spec_vectors_frame_json() {
    let doc: Value = serde_json::from_str(VECTORS).expect("frame.json parses");
    assert_eq!(doc["version"], 0, "the vectors are for version 0");
    let vectors = doc["vectors"].as_array().expect("vectors is an array");
    assert!(vectors.len() >= 18, "frame.py writes at least 18 vectors");

    for vector in vectors {
        let name = vector["name"].as_str().expect("name");
        let bytes = hex(vector["bytes"].as_str().expect("bytes"));
        let result = DataFrameHeader::parse(&bytes);

        if let Some(reason) = vector.get("error") {
            let reason = reason.as_str().expect("error is a string");
            match result {
                Err(error) => assert_eq!(error.reason(), reason, "{name}: wrong reason: {error}"),
                Ok(header) => panic!("{name}: accepted, expected {reason}: {header:?}"),
            }
            continue;
        }

        let expected = &vector["header"];
        let parsed = result.unwrap_or_else(|error| panic!("{name}: rejected: {error}"));
        assert_eq!(
            i64::from(parsed.frame_length()),
            field(expected, "frame_length"),
            "{name}: frame_length"
        );
        assert_eq!(
            i64::from(parsed.version()),
            field(expected, "version"),
            "{name}: version"
        );
        assert_eq!(
            i64::from(parsed.flags().bits()),
            field(expected, "flags"),
            "{name}: flags"
        );
        assert_eq!(
            i64::from(parsed.frame_type() as u16),
            field(expected, "type"),
            "{name}: type"
        );
        assert_eq!(
            i64::from(parsed.term_offset()),
            field(expected, "term_offset"),
            "{name}: term_offset"
        );
        assert_eq!(
            i64::from(parsed.session_id()),
            field(expected, "session_id"),
            "{name}: session_id"
        );
        assert_eq!(
            i64::from(parsed.stream_id()),
            field(expected, "stream_id"),
            "{name}: stream_id"
        );
        assert_eq!(
            i64::from(parsed.term_id()),
            field(expected, "term_id"),
            "{name}: term_id"
        );
        assert_eq!(
            parsed.reserved_value(),
            field(expected, "reserved_value"),
            "{name}: reserved_value"
        );

        let frame_length = usize::try_from(parsed.frame_length()).expect("validated");
        assert!(
            frame_length <= bytes.len(),
            "{name}: frame longer than the bytes"
        );
        assert_eq!(
            parsed.body_length(),
            frame_length - HEADER_LENGTH,
            "{name}: body_length"
        );
        assert!(
            parsed.aligned_length() >= frame_length,
            "{name}: aligned_length"
        );
        assert_eq!(
            parsed.aligned_length() % 32,
            0,
            "{name}: aligned_length alignment"
        );

        let built = build(expected);
        assert_eq!(
            built, parsed,
            "{name}: the fields build a different header than parsed"
        );
        assert_eq!(
            &built.to_bytes()[..],
            &bytes[..HEADER_LENGTH],
            "{name}: to_bytes differs"
        );
        let mut written = vec![0u8; bytes.len()];
        built.write(&mut written).expect("buffer holds a header");
        written[HEADER_LENGTH..].copy_from_slice(&bytes[HEADER_LENGTH..]);
        assert_eq!(
            &written[..frame_length],
            &bytes[..frame_length],
            "{name}: write differs"
        );
    }
}
