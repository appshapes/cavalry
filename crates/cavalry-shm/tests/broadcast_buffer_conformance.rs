//! Runs every vector in `spec/vectors/broadcast-buffer.json` against this crate's
//! layout layer: a header vector decodes to its fields and encodes back to its bytes or
//! is rejected for the named reason; a record vector decodes and is bounded; a transmit
//! or receive vector's fields are computed from its inputs (`spec/README.md`,
//! "Conventions"; ADR 0014). The layout layer has no `unsafe` and no mapping, so this
//! test also runs under `just miri`. `just conformance` runs it alone; `just test` runs
//! it with the rest.

use cavalry_shm::broadcast::layout::{
    HEADER_LENGTH, HeaderFields, PaddingRecord, Positions, ReceivePlan, RecordHeader, RecordState,
    encode_header, is_lapped, parse_header, plan_receive, plan_transmit,
};
use serde_json::Value;

const VECTORS: &str = include_str!("../../../spec/vectors/broadcast-buffer.json");

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

fn u32_of(fields: &Value, name: &str) -> u32 {
    u32::try_from(int(fields, name)).unwrap_or_else(|_| panic!("{name} fits u32"))
}

fn expect_error<T: std::fmt::Debug>(
    name: &str,
    vector: &Value,
    result: &Result<T, cavalry_shm::broadcast::BroadcastError>,
) -> bool {
    let Some(reason) = vector.get("error") else {
        return false;
    };
    let reason = reason.as_str().expect("error is a string");
    match result {
        Err(error) => assert_eq!(error.reason(), reason, "{name}: wrong reason: {error}"),
        Ok(fields) => panic!("{name}: accepted, expected {reason}: {fields:?}"),
    }
    true
}

fn header_vector(name: &str, vector: &Value) {
    let bytes = hex(text(vector, "bytes"));
    let file_length = vector["file_length"]
        .as_u64()
        .expect("a header vector has file_length");
    let result = parse_header(&bytes, file_length);
    if expect_error(name, vector, &result) {
        return;
    }
    let expected = &vector["fields"];
    let parsed = result.unwrap_or_else(|error| panic!("{name}: rejected: {error}"));
    assert_eq!(hex(text(expected, "magic")), *b"CVLYBCST", "{name}: magic");
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
    assert_eq!(parsed.file_length(), file_length, "{name}: file_length");
    let built = HeaderFields::new(
        u32_of(expected, "capacity"),
        int(expected, "creator_pid"),
        int(expected, "start_timestamp"),
    )
    .unwrap_or_else(|error| panic!("{name}: the fields do not build: {error}"));
    assert_eq!(built, parsed, "{name}: the fields build a different header");
    assert_eq!(
        &encode_header(&built)[..],
        &bytes[..HEADER_LENGTH],
        "{name}: encode_header differs"
    );
}

fn record_vector(name: &str, vector: &Value) {
    let bytes = hex(text(vector, "bytes"));
    let capacity = u32_of(vector, "capacity");
    let index = u32_of(vector, "index");
    let mut raw = [0u8; 8];
    raw.copy_from_slice(&bytes[..8]);
    let header = RecordHeader::decode(&raw);
    assert_eq!(header.encode(), raw, "{name}: encode differs");
    let result = header.check(capacity, index);
    if expect_error(name, vector, &result) {
        return;
    }
    let expected = &vector["fields"];
    assert_eq!(
        i64::from(header.length()),
        int(expected, "length"),
        "{name}: length"
    );
    assert_eq!(
        i64::from(header.type_id()),
        int(expected, "type"),
        "{name}: type"
    );
    let state = match header.state() {
        RecordState::Message => "message",
        RecordState::Padding => "padding",
    };
    assert_eq!(state, text(expected, "state"), "{name}: state");
    assert_eq!(
        i64::from(header.aligned_length()),
        int(expected, "aligned_length"),
        "{name}: aligned_length"
    );
    if let Some(message) = expected.get("message_length") {
        assert_eq!(
            i64::from(header.message_length()),
            message.as_i64().unwrap(),
            "{name}: message_length"
        );
    }
}

fn transmit_vector(name: &str, vector: &Value) {
    let inputs = &vector["inputs"];
    let result = plan_transmit(
        u32_of(inputs, "capacity"),
        int(inputs, "tail"),
        usize::try_from(int(inputs, "message_length")).unwrap(),
    );
    if expect_error(name, vector, &result) {
        return;
    }
    let plan = result.unwrap();
    let expected = &vector["fields"];
    assert_eq!(
        i64::from(plan.record_length),
        int(expected, "record_length"),
        "{name}: record_length"
    );
    assert_eq!(
        i64::from(plan.aligned_length),
        int(expected, "aligned_length"),
        "{name}: aligned_length"
    );
    assert_eq!(
        plan.tail_intent,
        int(expected, "tail_intent"),
        "{name}: tail_intent"
    );
    let padding = &expected["padding"];
    let expected_padding = if padding.is_null() {
        None
    } else {
        Some(PaddingRecord {
            index: u32_of(padding, "index"),
            length: u32_of(padding, "length"),
        })
    };
    assert_eq!(plan.padding, expected_padding, "{name}: padding");
    assert_eq!(
        i64::from(plan.index),
        int(expected, "index"),
        "{name}: index"
    );
    assert_eq!(plan.latest, int(expected, "latest"), "{name}: latest");
    assert_eq!(
        plan.tail_after,
        int(expected, "tail_after"),
        "{name}: tail_after"
    );
}

fn record_word(value: &Value) -> Option<u64> {
    if value.is_null() {
        None
    } else {
        let pair = value.as_array().expect("a record is [length, type]");
        Some(
            RecordHeader::new(
                i32::try_from(pair[0].as_i64().unwrap()).unwrap(),
                i32::try_from(pair[1].as_i64().unwrap()).unwrap(),
            )
            .word(),
        )
    }
}

fn receive_vector(name: &str, vector: &Value) {
    let inputs = &vector["inputs"];
    let capacity = u32_of(inputs, "capacity");
    let next_record = int(inputs, "next_record");
    let positions = Positions {
        tail: int(inputs, "tail"),
        tail_intent: int(inputs, "tail_intent"),
        latest: int(inputs, "latest"),
    };
    let record = record_word(&inputs["record"]);
    let record_at_zero = record_word(&inputs["record_at_zero"]);
    let tail_intent_after = inputs["tail_intent_after"]
        .as_i64()
        .unwrap_or(positions.tail_intent);
    let mut loads = 0;
    let result = plan_receive(
        capacity,
        next_record,
        positions,
        |_| {
            loads += 1;
            let word = if loads == 1 { record } else { record_at_zero };
            word.unwrap_or_else(|| panic!("{name}: a record was loaded that the vector lacks"))
        },
        || tail_intent_after,
    );
    if expect_error(name, vector, &result) {
        return;
    }
    let expected = &vector["fields"];
    let lapped = ("lapped", positions.latest, positions.latest);
    let (result_name, cursor, following) = match result.unwrap() {
        ReceivePlan::None => ("none", next_record, next_record),
        ReceivePlan::Lapped => lapped,
        ReceivePlan::Record {
            cursor,
            index,
            next_record,
            header,
        } => {
            if is_lapped(cursor, capacity, tail_intent_after) {
                lapped
            } else {
                assert_eq!(i64::from(index), int(expected, "index"), "{name}: index");
                assert_eq!(
                    i64::from(header.type_id()),
                    int(expected, "type"),
                    "{name}: type"
                );
                assert_eq!(
                    i64::from(header.message_length()),
                    int(expected, "message_length"),
                    "{name}: message_length"
                );
                ("message", cursor, next_record)
            }
        }
    };
    assert_eq!(result_name, text(expected, "result"), "{name}: result");
    assert_eq!(cursor, int(expected, "cursor"), "{name}: cursor");
    assert_eq!(
        following,
        int(expected, "next_record"),
        "{name}: next_record"
    );
}

#[test]
fn every_vector_in_spec_vectors_broadcast_buffer_json() {
    let doc: Value = serde_json::from_str(VECTORS).expect("broadcast-buffer.json parses");
    assert_eq!(doc["version"], 0, "the vectors are for version 0");
    let vectors = doc["vectors"].as_array().expect("vectors is an array");
    assert!(
        vectors.len() >= 52,
        "broadcast-buffer.py writes at least 52 vectors"
    );
    for vector in vectors {
        let name = vector["name"].as_str().expect("name");
        match vector["region"].as_str().expect("region") {
            "header" => header_vector(name, vector),
            "record" => record_vector(name, vector),
            "transmit" => transmit_vector(name, vector),
            "receive" => receive_vector(name, vector),
            other => panic!("{name}: unknown region {other}"),
        }
    }
}
