//! Runs every vector in `spec/vectors/command-ring.json` against this crate's layout
//! layer: a header vector decodes to its fields and encodes back to its bytes or is
//! rejected for the named reason; a record vector decodes and is bounded; a claim,
//! unblock or read vector's fields are computed from its inputs (`spec/README.md`,
//! "Conventions"; ADR 0014). The layout layer has no `unsafe` and no mapping, so this
//! test also runs under `just miri`. `just conformance` runs it alone; `just test` runs
//! it with the rest.

use cavalry_shm::command_ring::layout::{
    ClaimAction, ClaimOutcome, ClaimResult, HEADER_LENGTH, HeaderFields, PADDING_TYPE,
    PaddingRecord, ReadStop, RecordHeader, RecordState, encode_header, parse_header, plan_claim,
    plan_claim_action, plan_read, plan_unblock,
};
use serde_json::Value;

const VECTORS: &str = include_str!("../../../spec/vectors/command-ring.json");

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

fn i32_of(fields: &Value, name: &str) -> i32 {
    i32::try_from(int(fields, name)).unwrap_or_else(|_| panic!("{name} fits i32"))
}

fn padding_of(value: &Value) -> Option<PaddingRecord> {
    if value.is_null() {
        None
    } else {
        Some(PaddingRecord {
            index: u32_of(value, "index"),
            length: u32_of(value, "length"),
        })
    }
}

fn expect_error<T: std::fmt::Debug>(
    name: &str,
    vector: &Value,
    result: &Result<T, cavalry_shm::command_ring::CommandRingError>,
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
    assert_eq!(hex(text(expected, "magic")), *b"CVLYCMDR", "{name}: magic");
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
        RecordState::Unclaimed => "unclaimed",
        RecordState::Claimed => "claimed",
        RecordState::Committed => "committed",
        RecordState::Padding => "padding",
    };
    assert_eq!(state, text(expected, "state"), "{name}: state");
    if let Some(aligned) = expected.get("aligned_length") {
        assert_eq!(
            i64::from(header.aligned_length()),
            aligned.as_i64().unwrap(),
            "{name}: aligned_length"
        );
    }
    if let Some(message) = expected.get("message_length") {
        assert_eq!(
            i64::from(header.message_length()),
            message.as_i64().unwrap(),
            "{name}: message_length"
        );
    }
}

fn claim_vector(name: &str, vector: &Value) {
    let inputs = &vector["inputs"];
    let capacity = u32_of(inputs, "capacity");
    let head = int(inputs, "head");
    let head_cache = int(inputs, "head_cache");
    let message_length = usize::try_from(int(inputs, "message_length")).unwrap();
    let result = plan_claim(
        capacity,
        int(inputs, "tail"),
        head_cache,
        message_length,
        || head,
    );
    if expect_error(name, vector, &result) {
        return;
    }
    let plan = result.unwrap();
    let expected = &vector["fields"];
    let (result_name, index) = match plan.result {
        ClaimResult::Claimed { index } => ("claimed", Some(index)),
        ClaimResult::Full => ("full", None),
    };
    assert_eq!(result_name, text(expected, "result"), "{name}: result");
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
        plan.tail_after,
        int(expected, "tail_after"),
        "{name}: tail_after"
    );
    assert_eq!(
        plan.head_cache_after.unwrap_or(head_cache),
        int(expected, "head_cache_after"),
        "{name}: head_cache_after"
    );
    assert_eq!(
        plan.padding,
        padding_of(&expected["padding"]),
        "{name}: padding"
    );
    if let Some(index) = index {
        assert_eq!(i64::from(index), int(expected, "index"), "{name}: index");
    }
}

fn unblock_vector(name: &str, vector: &Value) {
    let inputs = &vector["inputs"];
    let capacity = u32_of(inputs, "capacity");
    let head = int(inputs, "head");
    let lengths: Vec<i32> = inputs["lengths"]
        .as_array()
        .expect("lengths")
        .iter()
        .map(|l| i32::try_from(l.as_i64().unwrap()).unwrap())
        .collect();
    let consumer = (head.max(0) as u64 & u64::from(capacity - 1)) as u32;
    let result = plan_unblock(capacity, head, int(inputs, "tail"), |index| {
        let step = index.saturating_sub(consumer) as usize / 8;
        let length = lengths.get(step).copied().unwrap_or(0);
        RecordHeader::new(length, if length == 0 { 0 } else { PADDING_TYPE }).word()
    });
    if expect_error(name, vector, &result) {
        return;
    }
    let plan = result.unwrap();
    let expected = &vector["fields"];
    assert_eq!(
        plan.is_some(),
        expected["unblocked"].as_bool().unwrap(),
        "{name}: unblocked"
    );
    let record = &expected["record"];
    match plan {
        None => assert!(record.is_null(), "{name}: no record"),
        Some(padding) => {
            assert_eq!(
                i64::from(padding.index),
                int(record, "index"),
                "{name}: index"
            );
            assert_eq!(
                i64::from(padding.length),
                int(record, "length"),
                "{name}: length"
            );
            assert_eq!(int(record, "type"), i64::from(PADDING_TYPE), "{name}: type");
        }
    }
}

fn commit_vector(name: &str, vector: &Value) {
    let inputs = &vector["inputs"];
    let header = |value: &Value| RecordHeader::new(i32_of(value, "length"), i32_of(value, "type"));
    let action = match text(inputs, "operation") {
        "claim" => ClaimAction::Claim,
        "write" => ClaimAction::Write,
        "commit" => ClaimAction::Commit,
        "abort" => ClaimAction::Abort,
        other => panic!("{name}: unknown operation {other}"),
    };
    let outcome = plan_claim_action(header(&inputs["claim"]), header(&inputs["found"]), action);
    let expected = &vector["fields"];
    let (result, written) = match outcome {
        ClaimOutcome::Claimed(word) => ("claimed", Some(word)),
        ClaimOutcome::Writable => ("writable", None),
        ClaimOutcome::Committed(word) => ("committed", Some(word)),
        ClaimOutcome::Aborted(word) => ("aborted", Some(word)),
        ClaimOutcome::Unblocked => ("unblocked", None),
    };
    assert_eq!(result, text(expected, "result"), "{name}: result");
    let expected_written = expected["written"]
        .as_object()
        .map(|_| header(&expected["written"]));
    assert_eq!(written, expected_written, "{name}: written");
}

fn read_vector(name: &str, vector: &Value) {
    let inputs = &vector["inputs"];
    let capacity = u32_of(inputs, "capacity");
    let head = int(inputs, "head");
    let limit = usize::try_from(int(inputs, "limit")).unwrap();
    let ring = hex(text(inputs, "ring"));
    assert_eq!(
        ring.len(),
        capacity as usize,
        "{name}: the ring image is the capacity"
    );
    let mut messages = Vec::new();
    let plan = plan_read(capacity, head, limit, &ring[..], |index, header| {
        let start = index as usize + 8;
        let end = index as usize + header.length() as usize;
        messages.push((header.type_id(), ring[start..end].to_vec()));
    });
    let expected = &vector["fields"];
    let stop = match plan.stop {
        ReadStop::EndOfRing => "end-of-ring",
        ReadStop::Unclaimed => "unclaimed",
        ReadStop::Claimed => "claimed",
        ReadStop::Limit => "limit",
        ReadStop::Record => "record",
    };
    assert_eq!(stop, text(expected, "result"), "{name}: result");
    let expected_messages: Vec<(i32, Vec<u8>)> = expected["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| (i32_of(m, "type"), hex(text(m, "message"))))
        .collect();
    assert_eq!(messages, expected_messages, "{name}: messages");
    assert_eq!(
        i64::from(plan.bytes_read),
        int(expected, "bytes_read"),
        "{name}: bytes_read"
    );
    assert_eq!(
        plan.messages,
        expected_messages.len(),
        "{name}: message count"
    );
    assert_eq!(
        head + i64::from(plan.bytes_read),
        int(expected, "head_after"),
        "{name}: head_after"
    );
    let zeroed = &expected["zeroed"];
    assert_eq!(
        i64::from((head as u64 & u64::from(capacity - 1)) as u32),
        int(zeroed, "index"),
        "{name}: zeroed index"
    );
    assert_eq!(
        i64::from(plan.bytes_read),
        int(zeroed, "length"),
        "{name}: zeroed length"
    );
    if plan.stop == ReadStop::Record {
        assert!(
            plan.rejected.is_some(),
            "{name}: the rejected record is named"
        );
    }
}

#[test]
fn every_vector_in_spec_vectors_command_ring_json() {
    let doc: Value = serde_json::from_str(VECTORS).expect("command-ring.json parses");
    assert_eq!(doc["version"], 1, "the vectors are for version 1");
    let vectors = doc["vectors"].as_array().expect("vectors is an array");
    assert!(
        vectors.len() >= 56,
        "command-ring.py writes at least 56 vectors"
    );
    for vector in vectors {
        let name = vector["name"].as_str().expect("name");
        match vector["region"].as_str().expect("region") {
            "header" => header_vector(name, vector),
            "record" => record_vector(name, vector),
            "claim" => claim_vector(name, vector),
            "unblock" => unblock_vector(name, vector),
            "read" => read_vector(name, vector),
            "commit" => commit_vector(name, vector),
            other => panic!("{name}: unknown region {other}"),
        }
    }
}
