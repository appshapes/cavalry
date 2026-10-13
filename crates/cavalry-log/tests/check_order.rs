//! Bytes with two faults each are rejected for the one the page checks first
//! (`spec/durable-log.md`, "What a reader rejects", "Check order"; ADR 0024), so that
//! every implementation reports the same reason for the same bytes. The vectors carry
//! one fault each; these pin the order between faults.

use cavalry_log::layout::{
    CatalogHeader, DESCRIPTOR_LENGTH, Descriptor, HEADER_LENGTH, IndexEntry, IndexHeader,
    MAX_SLOTS, Recording, crc32,
};

fn slot_bytes() -> [u8; DESCRIPTOR_LENGTH] {
    Descriptor::new(&Recording {
        recording_id: 0,
        start_timestamp: 1_760_000_000_000_000_000,
        start_position: 0,
        initial_term_id: 7,
        term_length: 64 * 1024 * 1024,
        mtu_length: 1408,
        session_id: 1,
        stream_id: 1001,
        channel: "cavalry:ipc",
        source_identity: "cavalry:ipc",
    })
    .expect("a valid recording")
    .encode()
}

fn put_i32(bytes: &mut [u8], offset: usize, value: i32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_i64(bytes: &mut [u8], offset: usize, value: i64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

/// Signs both checksums again, so that the faults are in the fields and not in them.
fn sign(bytes: &mut [u8; DESCRIPTOR_LENGTH]) {
    let creation = crc32(&bytes[28..]);
    bytes[24..28].copy_from_slice(&creation.to_le_bytes());
    let stop = crc32(&bytes[8..24]);
    bytes[4..8].copy_from_slice(&stop.to_le_bytes());
}

fn descriptor_reason(edit: impl FnOnce(&mut [u8; DESCRIPTOR_LENGTH]), signed: bool) -> String {
    let mut bytes = slot_bytes();
    edit(&mut bytes);
    if signed {
        sign(&mut bytes);
    }
    match Descriptor::parse(&bytes, 0) {
        Err(error) => error.reason().to_string(),
        Ok(outcome) => panic!("two faults parsed: {outcome:?}"),
    }
}

#[test]
fn a_descriptor_reports_its_first_fault_in_the_page_order() {
    // state before checksum
    let reason = descriptor_reason(|b| put_i32(b, 0, 4), false);
    assert_eq!(reason, "state");
    // invalid before checksum
    let reason = descriptor_reason(|b| put_i32(b, 0, 2), false);
    assert_eq!(reason, "invalid");
    // checksum before every field check
    let reason = descriptor_reason(|b| put_i64(b, 32, 5), false);
    assert_eq!(reason, "checksum");
    // timestamp before recording-id
    let reason = descriptor_reason(
        |b| {
            put_i64(b, 40, -1);
            put_i64(b, 32, 5);
        },
        true,
    );
    assert_eq!(reason, "timestamp");
    // recording-id before string
    let reason = descriptor_reason(
        |b| {
            put_i32(b, 80, 300);
            put_i64(b, 32, 5);
        },
        true,
    );
    assert_eq!(reason, "recording-id");
    // position before term-length
    let reason = descriptor_reason(
        |b| {
            put_i64(b, 48, 16);
            put_i32(b, 64, 3 * 1024 * 1024);
        },
        true,
    );
    assert_eq!(reason, "position");
    // term-length before mtu
    let reason = descriptor_reason(
        |b| {
            put_i32(b, 64, 3 * 1024 * 1024);
            put_i32(b, 68, 1400);
        },
        true,
    );
    assert_eq!(reason, "term-length");
    // segment-length before mtu
    let reason = descriptor_reason(
        |b| {
            put_i32(b, 60, 192 * 1024 * 1024);
            put_i32(b, 68, 1400);
        },
        true,
    );
    assert_eq!(reason, "segment-length");
    // mtu before string
    let reason = descriptor_reason(
        |b| {
            put_i32(b, 68, 1400);
            put_i32(b, 80, 300);
        },
        true,
    );
    assert_eq!(reason, "mtu");
    // string is last: a length over its field, then bytes that are not UTF-8
    let reason = descriptor_reason(|b| b[84] = 0xFF, true);
    assert_eq!(reason, "string");
}

fn catalog_reason(edit: impl FnOnce(&mut [u8; HEADER_LENGTH]), file_length: u64) -> String {
    let mut bytes = CatalogHeader::new(1_760_000_000_000_000_000)
        .expect("a valid header")
        .encode();
    edit(&mut bytes);
    match CatalogHeader::parse(&bytes, file_length) {
        Err(error) => error.reason().to_string(),
        Ok(header) => panic!("two faults parsed: {header:?}"),
    }
}

#[test]
fn a_catalog_header_reports_its_first_fault_in_the_page_order() {
    let header = HEADER_LENGTH as u64;
    let over_bound = header + (MAX_SLOTS + 1) * DESCRIPTOR_LENGTH as u64;
    let reason = catalog_reason(|b| b[..8].copy_from_slice(&[0; 8]), 64);
    assert_eq!(reason, "too-short");
    let reason = catalog_reason(
        |b| {
            b[..8].copy_from_slice(&[0; 8]);
            put_i32(b, 8, 1);
        },
        header,
    );
    assert_eq!(reason, "not-ready");
    let reason = catalog_reason(
        |b| {
            b[..8].copy_from_slice(b"CVLYTIDX");
            put_i32(b, 8, 1);
        },
        header,
    );
    assert_eq!(reason, "magic");
    let reason = catalog_reason(
        |b| {
            put_i32(b, 8, 1);
            put_i32(b, 12, 512);
        },
        header,
    );
    assert_eq!(reason, "version");
    let reason = catalog_reason(
        |b| {
            put_i32(b, 12, 512);
            put_i32(b, 20, 1);
        },
        header,
    );
    assert_eq!(reason, "descriptor-length");
    let reason = catalog_reason(
        |b| {
            put_i32(b, 20, 1);
            put_i64(b, 24, -1);
        },
        header,
    );
    assert_eq!(reason, "checksum-kind");
    let reason = catalog_reason(|b| put_i64(b, 24, -1), over_bound);
    assert_eq!(reason, "timestamp");
}

fn index_reason(edit: impl FnOnce(&mut [u8; HEADER_LENGTH])) -> String {
    let mut bytes = IndexHeader::new(1).expect("a valid header").encode();
    edit(&mut bytes);
    match IndexHeader::parse(&bytes, HEADER_LENGTH as u64) {
        Err(error) => error.reason().to_string(),
        Ok(header) => panic!("two faults parsed: {header:?}"),
    }
}

#[test]
fn an_index_reports_its_first_fault_in_the_page_order() {
    let reason = index_reason(|b| {
        b[..8].copy_from_slice(&[0; 8]);
        put_i32(b, 8, 1);
    });
    assert_eq!(reason, "not-ready");
    let reason = index_reason(|b| {
        b[..8].copy_from_slice(b"CVLYCTLG");
        put_i64(b, 16, -1);
    });
    assert_eq!(reason, "magic");
    let reason = index_reason(|b| {
        put_i32(b, 8, 1);
        put_i64(b, 16, -1);
    });
    assert_eq!(reason, "version");

    let previous = IndexEntry {
        timestamp: 1_000,
        position: 8_192,
    };
    let entry = |timestamp: i64, position: i64| {
        let bytes = IndexEntry {
            timestamp,
            position,
        }
        .encode();
        IndexEntry::parse(&bytes, Some(&previous))
            .expect_err("two faults")
            .reason()
    };
    // timestamp before position
    assert_eq!(entry(-1, 4_100), "timestamp");
    // position before order
    assert_eq!(entry(999, 4_100), "position");
}
