//! Every byte string either parses, and then encodes to bytes that parse to the same
//! fields (reserved bytes are written as 0 and never read, so the bytes themselves may
//! differ), or is rejected with one of the spec's named reasons. Never a panic
//! (CLAUDE.md gate 1; ADR 0024).
//! Deterministic, so a failure reproduces: xorshift64 over lengths around each layout's.

use cavalry_log::layout::{
    CatalogHeader, DESCRIPTOR_LENGTH, Descriptor, ENTRY_LENGTH, HEADER_LENGTH, IndexEntry,
    IndexHeader, Recording, crc32, locate,
};
use cavalry_log::recordings::{
    self, FILE_LENGTH as RECORDINGS_FILE_LENGTH, HEADER_LENGTH as RECORDINGS_HEADER_LENGTH,
    RecordingsHeader, SLOT_HEADER_LENGTH, SLOT_LENGTH,
};
use cavalry_log::terms::{
    self, ENTRY_LENGTH as TERMS_ENTRY_LENGTH, HEADER_LENGTH as TERMS_HEADER_LENGTH, MAX_ENTRIES,
    TermsEntry, TermsHeader,
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

const REASONS: [&str; 28] = [
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
    "entry-length",
    "candidate",
    "term",
    "recording",
    "terms-full",
    "slot-length",
    "member",
    "sequence",
    "slots",
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

#[test]
fn terms_headers_and_entries_never_panic() {
    let mut random = XorShift(0xBF58_476D_1CE4_E5B9);
    let bound = TERMS_HEADER_LENGTH as u64 + MAX_ENTRIES * TERMS_ENTRY_LENGTH as u64;
    for round in 0..20_000 {
        let length = if round % 4 == 0 {
            (random.next() % 100) as usize
        } else {
            TERMS_HEADER_LENGTH
        };
        let mut bytes = random.bytes(length);
        if round % 3 == 0 && bytes.len() >= 16 {
            bytes[..8].copy_from_slice(b"CVLYTERM");
            bytes[8..12].copy_from_slice(&0i32.to_le_bytes());
            bytes[12..16].copy_from_slice(&(TERMS_ENTRY_LENGTH as i32).to_le_bytes());
        }
        // The candidate slots: both checksums right, one right, or neither.
        if round % 2 == 0 && bytes.len() >= 28 {
            let crc = crc32(&bytes[16..24]);
            bytes[24..28].copy_from_slice(&crc.to_le_bytes());
        }
        if round % 3 != 1 && bytes.len() >= 44 {
            let crc = crc32(&bytes[32..40]);
            bytes[40..44].copy_from_slice(&crc.to_le_bytes());
        }
        // Lengths around the bound as well as well inside it.
        let file_length = if round % 5 == 0 {
            bound - 64 + random.next() % 128
        } else {
            random.next() % (TERMS_HEADER_LENGTH as u64 * 100)
        };
        match TermsHeader::parse(&bytes, file_length) {
            Ok(header) => {
                let again =
                    TermsHeader::parse(&header.encode(), file_length).expect("encoded bytes parse");
                assert_eq!(again, header);
            }
            Err(error) => named(error.reason()),
        }
        let entry_length = if round % 7 == 0 {
            (random.next() % 40) as usize
        } else {
            TERMS_ENTRY_LENGTH
        };
        let mut entry_bytes = random.bytes(entry_length);
        if round % 2 == 0 && entry_bytes.len() >= 4 {
            let state = [terms::EMPTY, terms::VALID, terms::DELETED][(random.next() % 3) as usize];
            entry_bytes[..4].copy_from_slice(&state.to_le_bytes());
        }
        if round % 3 == 0 && entry_bytes.len() == TERMS_ENTRY_LENGTH {
            let crc = crc32(&entry_bytes[8..]);
            entry_bytes[4..8].copy_from_slice(&crc.to_le_bytes());
        }
        let previous = (round % 4 != 0).then(|| TermsEntry {
            state: terms::VALID,
            leadership_term_id: (random.next() % 1_000) as i64,
            term_base_log_position: ((random.next() % 1_000) as i64) * 32,
            recording_id: (random.next() % 1_000) as i64 - 1,
        });
        match TermsEntry::parse(&entry_bytes, previous.as_ref()) {
            Ok(entry) if entry.state == terms::EMPTY => {
                // An empty entry's other bytes are not read, so its encoding is zeros
                // that parse to the same empty entry.
                let again = TermsEntry::parse(&entry.encode(), previous.as_ref())
                    .expect("encoded bytes parse");
                assert_eq!(again, entry);
            }
            Ok(entry) => assert_eq!(
                &entry.encode()[..],
                &entry_bytes[..TERMS_ENTRY_LENGTH],
                "a valid or deleted entry has no reserved bytes"
            ),
            Err(error) => named(error.reason()),
        }
    }
}

/// A slot of the log recordings file for [`recordings_headers_and_slots_never_panic`]:
/// random bytes, or fields a reader takes with now and then one that it does not,
/// under a checksum that matches two times in three.
fn recordings_slot(random: &mut XorShift, round: u64) -> Vec<u8> {
    let count = (random.next() % 6) as usize;
    let whole = SLOT_HEADER_LENGTH + count * 16;
    let length = if round.is_multiple_of(7) {
        (random.next() % 200) as usize
    } else {
        whole
    };
    let mut slot = random.bytes(length);
    if round.is_multiple_of(2) && slot.len() >= 8 {
        slot[4..8].copy_from_slice(&(count as i32).to_le_bytes());
    }
    if !round.is_multiple_of(4) && slot.len() >= whole {
        slot[4..8].copy_from_slice(&(count as i32).to_le_bytes());
        let sequence = (random.next() % 1_000) as i64;
        let mut position = ((random.next() % 16) as i64) * 32;
        slot[8..16].copy_from_slice(&sequence.to_le_bytes());
        slot[16..24].copy_from_slice(&position.to_le_bytes());
        for entry in 0..count {
            let at = SLOT_HEADER_LENGTH + entry * 16;
            let recording_id = (random.next() % 1_000) as i64;
            slot[at..at + 8].copy_from_slice(&recording_id.to_le_bytes());
            slot[at + 8..at + 16].copy_from_slice(&position.to_le_bytes());
            position += ((random.next() % 4) as i64) * 32;
        }
        // One field in five rounds is whatever the generator gives: a sequence, a
        // floor, a recording id or a start position a reader may refuse.
        if round.is_multiple_of(5) {
            let fields = 2 + 2 * count;
            let at = 8 + 8 * (random.next() as usize % fields);
            slot[at..at + 8].copy_from_slice(&(random.next() as i64 >> 40).to_le_bytes());
        }
    }
    if !round.is_multiple_of(3) && slot.len() >= whole && slot.len() >= SLOT_HEADER_LENGTH {
        let count = i32::from_le_bytes([slot[4], slot[5], slot[6], slot[7]]);
        if let Ok(count) = usize::try_from(count)
            && slot.len() >= SLOT_HEADER_LENGTH + count * 16
        {
            let crc = crc32(&slot[4..SLOT_HEADER_LENGTH + count * 16]);
            slot[..4].copy_from_slice(&crc.to_le_bytes());
        }
    }
    slot
}

#[test]
fn recordings_headers_and_slots_never_panic() {
    let mut random = XorShift(0x94D0_49BB_1331_11EB);
    let mut taken = 0;
    for round in 0..20_000 {
        let length = if round % 4 == 0 {
            (random.next() % 100) as usize
        } else {
            RECORDINGS_HEADER_LENGTH
        };
        let mut bytes = random.bytes(length);
        if round % 3 == 0 && bytes.len() >= 16 {
            bytes[..8].copy_from_slice(b"CVLYLOGR");
            bytes[8..12].copy_from_slice(&0i32.to_le_bytes());
            bytes[12..16].copy_from_slice(&(SLOT_LENGTH as i32).to_le_bytes());
        }
        if round % 2 == 0 && bytes.len() >= 20 {
            let member_id = (random.next() % 8) as i32 - 1;
            bytes[16..20].copy_from_slice(&member_id.to_le_bytes());
        }
        // Lengths around the file's own as well as its own.
        let file_length = if round % 5 == 0 {
            RECORDINGS_FILE_LENGTH as u64 - 2 + random.next() % 5
        } else {
            RECORDINGS_FILE_LENGTH as u64
        };
        match RecordingsHeader::parse(&bytes, file_length) {
            Ok(header) => {
                let again = RecordingsHeader::parse(&header.encode(), file_length)
                    .expect("encoded bytes parse");
                assert_eq!(again, header);
            }
            Err(error) => named(error.reason()),
        }

        let slots = [
            recordings_slot(&mut random, round),
            recordings_slot(&mut random, round / 2),
        ];
        let mut lists = [Vec::new(), Vec::new()];
        let [list_0, list_1] = &mut lists;
        let read = [
            recordings::parse_slot(&slots[0], list_0),
            recordings::parse_slot(&slots[1], list_1),
        ];
        for ((slot, list), bytes) in read.iter().zip(&lists).zip(&slots) {
            match slot {
                Ok(fields) => {
                    // A slot a reader takes is one a writer writes, byte for byte.
                    let mut out = Box::new([0u8; SLOT_LENGTH]);
                    let length = recordings::encode_slot(*fields, list.iter().copied(), &mut out)
                        .expect("the fields and entries a reader took encode");
                    assert_eq!(&out[..length], &bytes[..length]);
                    taken += 1;
                }
                Err(error) => named(error.reason()),
            }
        }
        let [slot_0, slot_1] = read;
        match recordings::choose(slot_0, slot_1) {
            Ok(slot) => assert!(slot < 2),
            Err(error) => named(error.reason()),
        }
    }
    assert!(
        taken > 5_000,
        "{taken} slots were taken: the generator is off"
    );
}
