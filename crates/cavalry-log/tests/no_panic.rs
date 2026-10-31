//! Every byte string either parses, and then encodes to bytes that parse to the same
//! fields (reserved bytes are written as 0 and never read, so the bytes themselves may
//! differ), or is rejected with one of the spec's named reasons. Never a panic
//! (CLAUDE.md gate 1; ADR 0024).
//! Deterministic, so a failure reproduces: xorshift64 over lengths around each layout's.

use cavalry_log::layout::{
    CatalogHeader, DESCRIPTOR_LENGTH, Descriptor, ENTRY_LENGTH, HEADER_LENGTH, IndexEntry,
    IndexHeader, Recording, crc32, locate,
};

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

    fn bytes(&mut self, length: usize) -> Vec<u8> {
        (0..length).map(|_| self.next() as u8).collect()
    }
}

const REASONS: [&str; 19] = [
    "too-short",
    "not-ready",
    "magic",
    "version",
    "descriptor-length",
    "checksum-kind",
    "timestamp",
    "length",
    "state",
    "invalid",
    "checksum",
    "recording-id",
    "position",
    "term-length",
    "segment-length",
    "mtu",
    "string",
    "order",
    "frame",
];

fn named(reason: &str) {
    assert!(
        REASONS.contains(&reason),
        "{reason} is not a reason the spec names"
    );
}

#[test]
fn catalog_headers_never_panic() {
    let mut random = XorShift(0x9E37_79B9_7F4A_7C15);
    for round in 0..20_000 {
        let length = if round % 4 == 0 {
            (random.next() % 200) as usize
        } else {
            HEADER_LENGTH
        };
        let mut bytes = random.bytes(length);
        if round % 3 == 0 && bytes.len() >= 8 {
            bytes[..8].copy_from_slice(b"CVLYCTLG");
        }
        if round % 5 == 0 && bytes.len() >= 24 {
            bytes[8..12].copy_from_slice(&0i32.to_le_bytes());
            bytes[12..16].copy_from_slice(&1024i32.to_le_bytes());
            bytes[20..24].copy_from_slice(&0i32.to_le_bytes());
        }
        let file_length = random.next() % (HEADER_LENGTH as u64 * 100);
        match CatalogHeader::parse(&bytes, file_length) {
            Ok(header) => {
                let again = CatalogHeader::parse(&header.encode(), file_length)
                    .expect("encoded bytes parse");
                assert_eq!(again, header);
            }
            Err(error) => named(error.reason()),
        }
    }
}

#[test]
fn descriptors_never_panic() {
    let mut random = XorShift(0xD1B5_4A32_D192_ED03);
    for round in 0..20_000 {
        let length = if round % 4 == 0 {
            (random.next() % 1100) as usize
        } else {
            DESCRIPTOR_LENGTH
        };
        let mut bytes = random.bytes(length);
        if round % 2 == 0 && bytes.len() >= 4 {
            bytes[..4].copy_from_slice(&1i32.to_le_bytes());
        }
        if round % 3 == 0 && bytes.len() == DESCRIPTOR_LENGTH {
            let crc = cavalry_log::layout::crc32(&bytes[8..]);
            bytes[4..8].copy_from_slice(&crc.to_le_bytes());
        }
        let slot = (random.next() % 4) as i64;
        match Descriptor::parse(&bytes, slot) {
            Ok(Some(descriptor)) => {
                let again =
                    Descriptor::parse(&descriptor.encode(), slot).expect("encoded bytes parse");
                assert_eq!(again, Some(descriptor.clone()));
                for position in [
                    descriptor.start_position(),
                    descriptor.start_position() + 32,
                ] {
                    let _ = locate(&descriptor, position);
                }
            }
            Ok(None) => {}
            Err(error) => named(error.reason()),
        }
    }
}

/// A retained position at the minimum, against a start position in a later term, is
/// `position`, never an overflow in the arithmetic that places it at a segment base
/// (`spec/durable-log.md`, "What a reader rejects"; ADR 0031): through the builder,
/// and through the parser on a slot in version 1's form whose checksums hold.
#[test]
fn a_retained_position_at_the_minimum_is_position_not_an_overflow() {
    let later_term = Descriptor::new(&Recording {
        recording_id: 0,
        start_timestamp: 1_760_000_000_000_000_000,
        start_position: 64 * 1024 * 1024 + 4096,
        initial_term_id: 7,
        term_length: 64 * 1024 * 1024,
        mtu_length: 1408,
        session_id: 1,
        stream_id: 1001,
        channel: "cavalry:ipc",
        source_identity: "cavalry:ipc",
    })
    .expect("a valid recording");
    let refused = later_term
        .retained(i64::MIN)
        .expect_err("below the start position");
    assert_eq!(refused.reason(), "position");
    let mut bytes = later_term.in_version_1().encode();
    bytes[472..480].copy_from_slice(&i64::MIN.to_le_bytes());
    let retained = crc32(&bytes[472..480]);
    bytes[480..484].copy_from_slice(&retained.to_le_bytes());
    let refused = Descriptor::parse(&bytes, 0).expect_err("below the start position");
    assert_eq!(refused.reason(), "position");
}

#[test]
fn index_headers_and_entries_never_panic() {
    let mut random = XorShift(0x2545_F491_4F6C_DD1D);
    for round in 0..20_000 {
        let length = if round % 4 == 0 {
            (random.next() % 200) as usize
        } else {
            HEADER_LENGTH
        };
        let mut bytes = random.bytes(length);
        if round % 3 == 0 && bytes.len() >= 12 {
            bytes[..8].copy_from_slice(b"CVLYTIDX");
            bytes[8..12].copy_from_slice(&0i32.to_le_bytes());
        }
        let file_length = random.next() % (HEADER_LENGTH as u64 * 100);
        match IndexHeader::parse(&bytes, file_length) {
            Ok(header) => {
                let again =
                    IndexHeader::parse(&header.encode(), file_length).expect("encoded bytes parse");
                assert_eq!(again, header);
            }
            Err(error) => named(error.reason()),
        }
        let entry_bytes = random.bytes(if round % 7 == 0 { 7 } else { ENTRY_LENGTH });
        let previous = IndexEntry {
            timestamp: (random.next() % 1_000) as i64,
            position: ((random.next() % 1_000) as i64) * 32,
        };
        match IndexEntry::parse(&entry_bytes, Some(&previous)) {
            Ok(entry) => assert_eq!(
                &entry.encode()[..],
                &entry_bytes[..ENTRY_LENGTH],
                "an entry has no reserved bytes"
            ),
            Err(error) => named(error.reason()),
        }
    }
}
