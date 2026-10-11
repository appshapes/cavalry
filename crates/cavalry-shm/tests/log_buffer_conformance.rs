//! Runs every vector in `spec/vectors/log-buffer.json` against this crate's layout
//! layer: a header vector decodes to its fields and encodes back to its bytes or is
//! rejected for the named reason; a tail vector unpacks; a position or claim vector's
//! fields are computed from its inputs; a claim-header vector's bytes are produced
//! before and after commit and abort (`spec/README.md`, "Conventions"; ADR 0013). The
//! layout layer has no `unsafe` and no mapping, so this test also runs under `just miri`.

use cavalry_shm::log::layout::{
    ClaimRefused, HEADER_LENGTH, HeaderFields, LogType, Positions, Reserved, Tail, aborted_header,
    after_reserve, before_reserve, claim_header, committed_header, encode_header, parse_header,
};
use serde_json::Value;

const VECTORS: &str = include_str!("../../../spec/vectors/log-buffer.json");

fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("vector bytes are hex"))
        .collect()
}

fn int(fields: &Value, name: &str) -> i64 {
    fields[name]
        .as_i64()
        .unwrap_or_else(|| panic!("vector fields lack {name}"))
}

fn text<'a>(fields: &'a Value, name: &str) -> &'a str {
    fields[name]
        .as_str()
        .unwrap_or_else(|| panic!("vector fields lack {name}"))
}

fn i32_of(fields: &Value, name: &str) -> i32 {
    i32::try_from(int(fields, name)).unwrap_or_else(|_| panic!("{name} fits i32"))
}

fn u32_of(fields: &Value, name: &str) -> u32 {
    u32::try_from(int(fields, name)).unwrap_or_else(|_| panic!("{name} fits u32"))
}

fn tail_of(fields: &Value, name: &str) -> Tail {
    let bytes = hex(text(fields, name));
    Tail(i64::from_le_bytes(
        bytes.try_into().expect("a tail is eight bytes"),
    ))
}

fn header_vector(name: &str, vector: &Value) {
    let bytes = hex(text(vector, "bytes"));
    let file_length = vector["file_length"]
        .as_u64()
        .expect("a header vector has file_length");
    let result = parse_header(&bytes, file_length);

    if let Some(reason) = vector.get("error") {
        let reason = reason.as_str().expect("error is a string");
        match result {
            Err(error) => assert_eq!(error.reason(), reason, "{name}: wrong reason: {error}"),
            Ok(fields) => panic!("{name}: accepted, expected {reason}: {fields:?}"),
        }
        return;
    }

    let expected = &vector["fields"];
    let parsed = result.unwrap_or_else(|error| panic!("{name}: rejected: {error}"));
    assert_eq!(
        i64::from(parsed.version()),
        int(expected, "version"),
        "{name}: version"
    );
    assert_eq!(
        i64::from(parsed.log_type() as i32),
        int(expected, "type"),
        "{name}: type"
    );
    assert_eq!(
        parsed.correlation_id(),
        int(expected, "correlation_id"),
        "{name}: correlation_id"
    );
    assert_eq!(
        i64::from(parsed.initial_term_id()),
        int(expected, "initial_term_id"),
        "{name}: initial_term_id"
    );
    assert_eq!(
        i64::from(parsed.term_length()),
        int(expected, "term_length"),
        "{name}: term_length"
    );
    assert_eq!(
        i64::from(parsed.mtu_length()),
        int(expected, "mtu_length"),
        "{name}: mtu_length"
    );
    assert_eq!(
        i64::from(parsed.publication_window_length()),
        int(expected, "publication_window_length"),
        "{name}: publication_window_length"
    );
    assert_eq!(
        i64::from(parsed.session_id()),
        int(expected, "session_id"),
        "{name}: session_id"
    );
    assert_eq!(
        i64::from(parsed.stream_id()),
        int(expected, "stream_id"),
        "{name}: stream_id"
    );
    assert_eq!(
        parsed.default_frame_header().to_vec(),
        hex(text(expected, "default_frame_header")),
        "{name}: default_frame_header"
    );
    assert_eq!(parsed.file_length(), file_length, "{name}: file_length");

    let log_type = LogType::try_from(i32_of(expected, "type")).expect("a valid vector's type");
    let built = HeaderFields::new(
        log_type,
        int(expected, "correlation_id"),
        i32_of(expected, "initial_term_id"),
        u32_of(expected, "term_length"),
        u32_of(expected, "mtu_length"),
        u32_of(expected, "publication_window_length"),
        i32_of(expected, "session_id"),
        i32_of(expected, "stream_id"),
    )
    .unwrap_or_else(|error| panic!("{name}: the fields do not build: {error}"));
    assert_eq!(
        built, parsed,
        "{name}: the fields build a different header than parsed"
    );
    assert_eq!(
        &encode_header(&built)[..],
        &bytes[..HEADER_LENGTH],
        "{name}: encode_header differs"
    );
}

fn tail_vector(name: &str, vector: &Value) {
    let tail = tail_of(vector, "bytes");
    let term_length = u32_of(vector, "term_length");
    let expected = &vector["fields"];
    assert_eq!(
        i64::from(tail.term_id()),
        int(expected, "term_id"),
        "{name}: term_id"
    );
    assert_eq!(
        i64::from(tail.term_offset(term_length)),
        int(expected, "term_offset"),
        "{name}: term_offset"
    );
    let repacked = Tail::pack(tail.term_id(), tail.raw_term_offset());
    assert_eq!(repacked, tail, "{name}: pack differs");
}

fn position_vector(name: &str, vector: &Value) {
    let inputs = &vector["inputs"];
    let expected = &vector["fields"];
    let positions = Positions::new(
        i32_of(inputs, "initial_term_id"),
        u32_of(inputs, "term_length"),
    );
    let position = if inputs.get("position").is_some() {
        int(inputs, "position")
    } else {
        positions.position(i32_of(inputs, "term_id"), u32_of(inputs, "term_offset"))
    };
    assert_eq!(position, int(expected, "position"), "{name}: position");
    assert_eq!(
        positions.term_count_of(position),
        int(expected, "term_count"),
        "{name}: term_count"
    );
    assert_eq!(
        i64::from(positions.term_id_of(position)),
        int(expected, "term_id"),
        "{name}: term_id"
    );
    assert_eq!(
        i64::from(positions.term_offset_of(position)),
        int(expected, "term_offset"),
        "{name}: term_offset"
    );
    assert_eq!(
        positions.partition_of(position) as i64,
        int(expected, "partition"),
        "{name}: partition"
    );
    assert_eq!(
        i64::from(positions.term_count(positions.term_id_of(position))),
        int(expected, "term_count"),
        "{name}: term_count from term_id"
    );
}

fn claim_vector(name: &str, vector: &Value) {
    let inputs = &vector["inputs"];
    let expected = &vector["fields"];
    let positions = Positions::new(
        i32_of(inputs, "initial_term_id"),
        u32_of(inputs, "term_length"),
    );
    let active = i32_of(inputs, "active_term_count");
    let tail = tail_of(inputs, "tail");
    let frame_length = u32_of(inputs, "frame_length");
    let aligned = cavalry_shm::log::layout::aligned_length(frame_length);
    let limit = int(inputs, "limit");
    let connected = int(inputs, "is_connected") == 1;
    let result = text(expected, "result");

    let before = before_reserve(&positions, active, tail, aligned, limit, connected);
    let refused = |refused: ClaimRefused| assert_eq!(refused.reason(), result, "{name}: refusal");
    match before {
        Err(ClaimRefused::AdminAction) => {
            assert_eq!(
                text(expected, "reason"),
                "tail",
                "{name}: a refusal before reserving is the tail check"
            );
            refused(ClaimRefused::AdminAction);
            return;
        }
        Err(other) => {
            assert_eq!(
                aligned as i64,
                int(expected, "aligned_length"),
                "{name}: aligned_length"
            );
            refused(other);
            return;
        }
        Ok(position) => {
            assert_eq!(
                position,
                int(expected, "position_before"),
                "{name}: position_before"
            );
            assert_eq!(
                aligned as i64,
                int(expected, "aligned_length"),
                "{name}: aligned_length"
            );
        }
    }
    let tail_after = Tail(tail.0 + i64::from(aligned));
    assert_eq!(
        tail_after,
        tail_of(expected, "tail_after"),
        "{name}: tail_after"
    );
    match after_reserve(&positions, active, tail, aligned) {
        Reserved::Fits {
            term_id,
            term_offset,
            position_after,
        } => {
            assert_eq!(result, "claimed", "{name}: fits");
            assert_eq!(
                i64::from(term_id),
                int(expected, "term_id"),
                "{name}: term_id"
            );
            assert_eq!(
                i64::from(term_offset),
                int(expected, "term_offset"),
                "{name}: term_offset"
            );
            assert_eq!(
                position_after,
                int(expected, "position_after"),
                "{name}: position_after"
            );
        }
        Reserved::EndOfTerm {
            term_id,
            padding,
            rotation,
        } => {
            assert_eq!(result, "admin-action", "{name}: end of term");
            assert_eq!(text(expected, "reason"), "end-of-term", "{name}: reason");
            check_padding(name, padding, expected);
            let expected_rotation = &expected["rotation"];
            assert_eq!(
                rotation.next_partition as i64,
                int(expected_rotation, "partition"),
                "{name}: next partition"
            );
            assert_eq!(
                i64::from(rotation.next_term_count),
                int(expected_rotation, "active_term_count"),
                "{name}: next term count"
            );
            let next_tail = tail_of(inputs, "next_tail");
            let rotated = if next_tail.term_id() == rotation.expected_term_id {
                rotation.next_tail
            } else {
                next_tail
            };
            assert_eq!(
                rotated,
                tail_of(expected_rotation, "tail"),
                "{name}: the tail after rotation"
            );
            assert_eq!(
                rotation.next_tail.term_id(),
                term_id.wrapping_add(1),
                "{name}: next term id"
            );
        }
        Reserved::MaxPositionExceeded { padding, .. } => {
            assert_eq!(result, "max-position-exceeded", "{name}: max position");
            check_padding(name, padding, expected);
        }
    }
}

fn check_padding(name: &str, padding: Option<cavalry_shm::log::layout::Padding>, expected: &Value) {
    match (&expected["padding"], padding) {
        (Value::Null, None) => {}
        (fields, Some(padding)) if !fields.is_null() => {
            assert_eq!(
                i64::from(padding.term_offset),
                int(fields, "term_offset"),
                "{name}: padding offset"
            );
            assert_eq!(
                i64::from(padding.frame_length),
                int(fields, "frame_length"),
                "{name}: padding length"
            );
        }
        (fields, padding) => panic!("{name}: padding {padding:?} against {fields}"),
    }
}

fn claim_header_vector(name: &str, vector: &Value) {
    let inputs = &vector["inputs"];
    let default: [u8; 32] = hex(text(inputs, "default_frame_header"))
        .try_into()
        .expect("32 bytes");
    let claimed = claim_header(
        &default,
        u32_of(inputs, "term_offset"),
        u32_of(inputs, "frame_length"),
        i32_of(inputs, "term_id"),
        int(inputs, "reserved_value"),
    );
    assert_eq!(
        claimed.to_vec(),
        hex(text(vector, "bytes")),
        "{name}: the claim header"
    );
    let expected = &vector["fields"];
    assert_eq!(
        committed_header(&claimed).to_vec(),
        hex(text(expected, "committed")),
        "{name}: committed"
    );
    assert_eq!(
        aborted_header(&claimed).to_vec(),
        hex(text(expected, "aborted")),
        "{name}: aborted"
    );
}

#[test]
fn every_vector_in_spec_vectors_log_buffer_json() {
    let doc: Value = serde_json::from_str(VECTORS).expect("log-buffer.json parses");
    assert_eq!(doc["version"], 0, "the vectors are for version 0");
    let vectors = doc["vectors"].as_array().expect("vectors is an array");
    assert!(
        vectors.len() >= 51,
        "log-buffer.py writes at least 51 vectors"
    );

    for vector in vectors {
        let name = vector["name"].as_str().expect("name");
        match vector["region"].as_str().expect("region") {
            "header" => header_vector(name, vector),
            "tail" => tail_vector(name, vector),
            "position" => position_vector(name, vector),
            "claim" => claim_vector(name, vector),
            "claim-header" => claim_header_vector(name, vector),
            other => panic!("{name}: unknown region {other}"),
        }
    }
}
