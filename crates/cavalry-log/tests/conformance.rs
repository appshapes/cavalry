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
use cavalry_log::recordings::{
    self, ListedRecording, RecordingsHeader, SLOT_LENGTH as RECORDINGS_SLOT_LENGTH, SlotFields,
};
use cavalry_log::terms::{
    self, ENTRY_LENGTH as TERMS_ENTRY_LENGTH, HEADER_LENGTH as TERMS_HEADER_LENGTH, TermsEntry,
    TermsHeader,
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
    // Version 1's form carries its retained position; version 0's has none
    // (`spec/durable-log.md`, "Descriptor"; ADR 0031).
    let formed = if fields.get("form").and_then(Value::as_i64) == Some(1) {
        stopped
            .retained(int(fields, "retained_position"))
            .expect("a vector's retained position is valid")
    } else {
        stopped
    };
    let state = State::from_i32(int(fields, "state") as i32).expect("a vector's state is valid");
    formed.with_state(state)
}

/// Holds a slot of the log recordings file to a vector's `fields`: its sequence, its
/// floor position and its entries, each a recording id and a start position.
fn assert_listed(name: &str, fields: &Value, slot: SlotFields, entries: &[ListedRecording]) {
    assert_eq!(slot.sequence, int(fields, "sequence"), "{name}");
    assert_eq!(slot.floor_position, int(fields, "floor_position"), "{name}");
    let expected: Vec<ListedRecording> = fields["entries"]
        .as_array()
        .expect("a list of entries")
        .iter()
        .map(|entry| ListedRecording {
            recording_id: entry[0].as_i64().expect("a recording id"),
            start_position: entry[1].as_i64().expect("a start position"),
        })
        .collect();
    assert_eq!(entries, &expected[..], "{name}");
}

#[test]
fn every_vector_agrees_with_the_layout_layer() {
    let doc: Value = serde_json::from_str(VECTORS).expect("the vectors are JSON");
    assert_eq!(doc["spec"], "durable-log");
    assert_eq!(
        doc["version"], 3,
        "the vectors are for the page's version 3"
    );
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
                        assert_eq!(
                            i64::from(header.version()),
                            int(&vector["fields"], "version"),
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
            "terms-header" => {
                let bytes = hex(vector["bytes"].as_str().expect("bytes"));
                let file_length = vector["file_length"].as_u64().expect("a file length");
                match (TermsHeader::parse(&bytes, file_length), error) {
                    (Ok(header), None) => {
                        let fields = &vector["fields"];
                        let slots = fields["candidate_terms"].as_array().expect("two slots");
                        assert_eq!(slots.len(), header.candidate_slots.len(), "{name}");
                        for (slot, expected) in header.candidate_slots.iter().zip(slots) {
                            // `null` is a slot whose checksum does not match.
                            assert_eq!(*slot, expected.as_i64(), "{name}");
                        }
                        assert_eq!(
                            header.candidate_term(),
                            Some(int(fields, "candidate_term")),
                            "{name}"
                        );
                        // A torn slot's bytes are whatever the crash left, so only a
                        // header with both slots whole encodes back to its bytes.
                        if header.candidate_slots.iter().all(Option::is_some) {
                            assert_eq!(
                                &header.encode()[..],
                                &bytes[..TERMS_HEADER_LENGTH],
                                "{name}"
                            );
                        }
                    }
                    (Err(found), Some(expected)) => assert_eq!(found.reason(), expected, "{name}"),
                    (outcome, expected) => panic!("{name}: {outcome:?} against {expected:?}"),
                }
            }
            "terms-entry" => {
                let bytes = hex(vector["bytes"].as_str().expect("bytes"));
                let previous = vector.get("previous").and_then(|p| {
                    p.as_object().map(|_| TermsEntry {
                        state: i32::try_from(int(p, "state")).expect("a state"),
                        leadership_term_id: int(p, "leadership_term_id"),
                        term_base_log_position: int(p, "term_base_log_position"),
                        recording_id: int(p, "recording_id"),
                    })
                });
                match (TermsEntry::parse(&bytes, previous.as_ref()), error) {
                    (Ok(entry), None) => {
                        let fields = &vector["fields"];
                        assert_eq!(i64::from(entry.state), int(fields, "state"), "{name}");
                        if entry.state != terms::EMPTY {
                            assert_eq!(
                                entry.leadership_term_id,
                                int(fields, "leadership_term_id"),
                                "{name}"
                            );
                            assert_eq!(
                                entry.term_base_log_position,
                                int(fields, "term_base_log_position"),
                                "{name}"
                            );
                            assert_eq!(entry.recording_id, int(fields, "recording_id"), "{name}");
                            assert_eq!(&entry.encode()[..], &bytes[..TERMS_ENTRY_LENGTH], "{name}");
                        }
                    }
                    (Err(found), Some(expected)) => assert_eq!(found.reason(), expected, "{name}"),
                    (outcome, expected) => panic!("{name}: {outcome:?} against {expected:?}"),
                }
            }
            "recordings-header" => {
                let bytes = hex(vector["bytes"].as_str().expect("bytes"));
                let file_length = vector["file_length"].as_u64().expect("a file length");
                match (RecordingsHeader::parse(&bytes, file_length), error) {
                    (Ok(header), None) => {
                        assert_eq!(
                            i64::from(header.member_id),
                            int(&vector["fields"], "member_id"),
                            "{name}"
                        );
                        assert_eq!(&header.encode()[..], &bytes[..], "{name}");
                    }
                    (Err(found), Some(expected)) => assert_eq!(found.reason(), expected, "{name}"),
                    (outcome, expected) => panic!("{name}: {outcome:?} against {expected:?}"),
                }
            }
            "recordings-slot" => {
                let bytes = hex(vector["bytes"].as_str().expect("bytes"));
                let mut entries = Vec::new();
                match (recordings::parse_slot(&bytes, &mut entries), error) {
                    (Ok(fields), None) => {
                        assert_listed(name, &vector["fields"], fields, &entries);
                        // The fields and the entries encode to the vector's bytes.
                        let mut slot = Box::new([0u8; RECORDINGS_SLOT_LENGTH]);
                        let length =
                            recordings::encode_slot(fields, entries.iter().copied(), &mut slot)
                                .expect("a slot a reader takes is one a writer writes");
                        assert_eq!(&slot[..length], &bytes[..], "{name}");
                    }
                    (Err(found), Some(expected)) => assert_eq!(found.reason(), expected, "{name}"),
                    (outcome, expected) => panic!("{name}: {outcome:?} against {expected:?}"),
                }
            }
            "recordings-file" => {
                let slots = vector["slots"].as_array().expect("two slots");
                assert_eq!(slots.len(), 2, "{name}");
                let mut lists = [Vec::new(), Vec::new()];
                let [list_0, list_1] = &mut lists;
                let slot_0 = recordings::parse_slot(&hex(slots[0].as_str().expect("hex")), list_0);
                let slot_1 = recordings::parse_slot(&hex(slots[1].as_str().expect("hex")), list_1);
                let read = [slot_0.as_ref().ok().copied(), slot_1.as_ref().ok().copied()];
                match (recordings::choose(slot_0, slot_1), error) {
                    (Ok(slot), None) => {
                        let fields = &vector["fields"];
                        assert_eq!(
                            i64::try_from(slot).expect("0 or 1"),
                            int(fields, "slot"),
                            "{name}"
                        );
                        let chosen = read[slot].expect("the slot chosen passed its checks");
                        assert_listed(name, fields, chosen, &lists[slot]);
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
