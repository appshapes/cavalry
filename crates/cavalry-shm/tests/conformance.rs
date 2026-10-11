//! Runs every vector in `spec/vectors/counters-file.json` against this crate's layout
//! layer: a valid vector decodes to its fields, those fields encode to the same bytes,
//! and an invalid vector is rejected for the named reason (`spec/README.md`,
//! "Conventions"; ADR 0012). The layout layer has no `unsafe` and no mapping, so this
//! test also runs under `just miri`. `just conformance` runs it alone; `just test` runs
//! it with the rest.

use cavalry_shm::counters::layout::{
    HEADER_LENGTH, HeaderFields, KEY_LENGTH, MAGIC, RECORD_LENGTH, RecordFields, State,
    encode_header, encode_record, parse_header, parse_record,
};
use serde_json::Value;

const VECTORS: &str = include_str!("../../../spec/vectors/counters-file.json");

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

fn header_vector(name: &str, bytes: &[u8], vector: &Value) {
    let file_length = vector["file_length"]
        .as_u64()
        .expect("a header vector has file_length");
    let result = parse_header(bytes, file_length);

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
        hex(text(expected, "magic")),
        MAGIC,
        "{name}: a valid header's magic"
    );
    assert_eq!(
        i64::from(parsed.version()),
        int(expected, "version"),
        "{name}: version"
    );
    assert_eq!(
        i64::from(parsed.capacity()),
        int(expected, "capacity"),
        "{name}: capacity"
    );
    assert_eq!(
        parsed.creator_pid(),
        int(expected, "creator_pid"),
        "{name}: creator_pid"
    );
    assert_eq!(
        parsed.start_timestamp(),
        int(expected, "start_timestamp"),
        "{name}: start_timestamp"
    );
    assert!(parsed.file_length() <= file_length, "{name}: file_length");

    let built = HeaderFields::new(
        u32::try_from(int(expected, "capacity")).expect("a valid capacity fits u32"),
        int(expected, "creator_pid"),
        int(expected, "start_timestamp"),
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

fn record_vector(name: &str, bytes: &[u8], vector: &Value) {
    let result = parse_record(bytes);

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
        i64::from(parsed.state() as i32),
        int(expected, "state"),
        "{name}: state"
    );
    assert_eq!(
        i64::from(parsed.type_id()),
        int(expected, "type_id"),
        "{name}: type_id"
    );
    assert_eq!(
        parsed.reclaim_deadline(),
        int(expected, "reclaim_deadline"),
        "{name}: reclaim_deadline"
    );
    let key = hex(text(expected, "key"));
    let mut padded = [0u8; KEY_LENGTH];
    padded[..key.len()].copy_from_slice(&key);
    assert_eq!(parsed.key(), &padded, "{name}: key");
    assert_eq!(
        i64::try_from(parsed.label_bytes().len()).unwrap(),
        int(expected, "label_length"),
        "{name}: label_length"
    );
    assert_eq!(parsed.label(), text(expected, "label"), "{name}: label");

    let state =
        State::try_from(i32::try_from(int(expected, "state")).expect("a valid state fits i32"))
            .unwrap_or_else(|error| panic!("{name}: the state does not build: {error}"));
    let built = RecordFields::new(
        state,
        i32::try_from(int(expected, "type_id")).expect("a type id fits i32"),
        int(expected, "reclaim_deadline"),
        &key,
        text(expected, "label"),
    )
    .unwrap_or_else(|error| panic!("{name}: the fields do not build: {error}"));
    assert_eq!(
        built, parsed,
        "{name}: the fields build a different record than parsed"
    );
    assert_eq!(
        &encode_record(&built)[..],
        &bytes[..RECORD_LENGTH],
        "{name}: encode_record differs"
    );
}

#[test]
fn every_vector_in_spec_vectors_counters_file_json() {
    let doc: Value = serde_json::from_str(VECTORS).expect("counters-file.json parses");
    assert_eq!(doc["version"], 0, "the vectors are for version 0");
    let vectors = doc["vectors"].as_array().expect("vectors is an array");
    assert!(
        vectors.len() >= 21,
        "counters-file.py writes at least 21 vectors"
    );

    for vector in vectors {
        let name = vector["name"].as_str().expect("name");
        let bytes = hex(vector["bytes"].as_str().expect("bytes"));
        match vector["region"].as_str().expect("region") {
            "header" => header_vector(name, &bytes, vector),
            "record" => record_vector(name, &bytes, vector),
            other => panic!("{name}: unknown region {other}"),
        }
    }
}
