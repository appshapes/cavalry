//! Runs every vector in `spec/vectors/durable-log.json` against this crate's layout
//! layer: a valid vector decodes to its fields and those fields encode to the same
//! bytes, an invalid vector is rejected for the named reason, and a position vector's
//! arithmetic gives the named segment file and offsets (`spec/README.md`,
//! "Conventions"; ADR 0024). No file and no `unsafe`, so this also runs under
//! `just miri`. `just conformance` runs it alone; `just test` runs it with the rest.

use cavalry_log::layout::{
    CatalogHeader, DESCRIPTOR_LENGTH, Descriptor, ENTRY_LENGTH, HEADER_LENGTH, IndexEntry,
    IndexHeader, Recording, State, locate,
};
use serde_json::Value;

const VECTORS: &str = include_str!("../../../spec/vectors/durable-log.json");

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

fn descriptor_from(fields: &Value) -> Descriptor {
    let live = Descriptor::new(&Recording {
        recording_id: int(fields, "recording_id"),
        start_timestamp: int(fields, "start_timestamp"),
        start_position: int(fields, "start_position"),
        initial_term_id: int(fields, "initial_term_id") as i32,
        term_length: int(fields, "term_length") as i32,
        mtu_length: int(fields, "mtu_length") as i32,
        session_id: int(fields, "session_id") as i32,
        stream_id: int(fields, "stream_id") as i32,
        channel: text(fields, "channel"),
        source_identity: text(fields, "source_identity"),
    })
    .expect("a vector's live descriptor is valid");
    let stopped = if int(fields, "stop_position") == -1 {
        live
    } else {
        live.stopped(int(fields, "stop_position"), int(fields, "stop_timestamp"))
            .expect("a vector's stopped descriptor is valid")
    };
    let state = State::from_i32(int(fields, "state") as i32).expect("a vector's state is valid");
    stopped.with_state(state)
}

#[test]
fn every_vector_agrees_with_the_layout_layer() {
    let doc: Value = serde_json::from_str(VECTORS).expect("the vectors are JSON");
    assert_eq!(doc["spec"], "durable-log");
    assert_eq!(doc["version"], 0);
    let mut count = 0;
    for vector in doc["vectors"].as_array().expect("a vector list") {
        let name = vector["name"].as_str().expect("a name");
        let region = vector["region"].as_str().expect("a region");
        let error = vector["error"].as_str();
        match region {
            "catalog-header" => {
                let bytes = hex(vector["bytes"].as_str().expect("bytes"));
                let file_length = vector["file_length"].as_u64().expect("a file length");
                match (CatalogHeader::parse(&bytes, file_length), error) {
                    (Ok(header), None) => {
                        assert_eq!(
                            header.created(),
                            int(&vector["fields"], "created"),
                            "{name}"
                        );
                        assert_eq!(&header.encode()[..], &bytes[..HEADER_LENGTH], "{name}");
                        assert_eq!(
                            CatalogHeader::slots(file_length),
                            vector["slots"].as_u64().expect("slots"),
                            "{name}"
                        );
                    }
                    (Err(found), Some(expected)) => assert_eq!(found.reason(), expected, "{name}"),
                    (outcome, expected) => panic!("{name}: {outcome:?} against {expected:?}"),
                }
            }
            "descriptor" => {
                let bytes = hex(vector["bytes"].as_str().expect("bytes"));
                let slot = vector["slot"].as_i64().expect("a slot");
                match (Descriptor::parse(&bytes, slot), error) {
                    (Ok(None), None) => {
                        assert_eq!(int(&vector["fields"], "state"), 0, "{name}");
                    }
                    (Ok(Some(descriptor)), None) => {
                        let expected = descriptor_from(&vector["fields"]);
                        assert_eq!(descriptor, expected, "{name}");
                        assert_eq!(
                            &descriptor.encode()[..],
                            &bytes[..DESCRIPTOR_LENGTH],
                            "{name}"
                        );
                    }
                    (Err(found), Some(expected)) => assert_eq!(found.reason(), expected, "{name}"),
                    (outcome, expected) => panic!("{name}: {outcome:?} against {expected:?}"),
                }
            }
            "index-header" => {
                let bytes = hex(vector["bytes"].as_str().expect("bytes"));
                let file_length = vector["file_length"].as_u64().expect("a file length");
                match (IndexHeader::parse(&bytes, file_length), error) {
                    (Ok(header), None) => {
                        assert_eq!(
                            header.recording_id(),
                            int(&vector["fields"], "recording_id"),
                            "{name}"
                        );
                        assert_eq!(&header.encode()[..], &bytes[..HEADER_LENGTH], "{name}");
                        assert_eq!(
                            IndexHeader::entries(file_length),
                            vector["entries"].as_u64().expect("entries"),
                            "{name}"
                        );
                    }
                    (Err(found), Some(expected)) => assert_eq!(found.reason(), expected, "{name}"),
                    (outcome, expected) => panic!("{name}: {outcome:?} against {expected:?}"),
                }
            }
            "index-entry" => {
                let bytes = hex(vector["bytes"].as_str().expect("bytes"));
                let previous = vector.get("previous").map(|p| IndexEntry {
                    timestamp: int(p, "timestamp"),
                    position: int(p, "position"),
                });
                match (IndexEntry::parse(&bytes, previous.as_ref()), error) {
                    (Ok(entry), None) => {
                        assert_eq!(
                            entry.timestamp,
                            int(&vector["fields"], "timestamp"),
                            "{name}"
                        );
                        assert_eq!(entry.position, int(&vector["fields"], "position"), "{name}");
                        assert_eq!(&entry.encode()[..], &bytes[..ENTRY_LENGTH], "{name}");
                    }
                    (Err(found), Some(expected)) => assert_eq!(found.reason(), expected, "{name}"),
                    (outcome, expected) => panic!("{name}: {outcome:?} against {expected:?}"),
                }
            }
            "position" => {
                let inputs = &vector["inputs"];
                let descriptor = descriptor_from(&inputs["descriptor"]);
                let located = locate(&descriptor, int(inputs, "position"))
                    .expect("a vector's position locates");
                let fields = &vector["fields"];
                assert_eq!(located.segment_file, text(fields, "segment_file"), "{name}");
                assert_eq!(
                    located.segment_base_position,
                    int(fields, "segment_base_position"),
                    "{name}"
                );
                assert_eq!(
                    located.offset_in_segment,
                    int(fields, "offset_in_segment"),
                    "{name}"
                );
                assert_eq!(i64::from(located.term_id), int(fields, "term_id"), "{name}");
                assert_eq!(
                    i64::from(located.term_offset),
                    int(fields, "term_offset"),
                    "{name}"
                );
            }
            other => panic!("{name}: unknown region {other}"),
        }
        count += 1;
    }
    assert!(count >= 40, "the vectors file has {count} vectors");
}
