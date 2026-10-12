//! The broadcast buffer on disk: `BroadcastBuffer` creates and opens it with the spec's
//! checks, a `Transmitter` writes records without waiting, a `Receiver` copies them out
//! and is told when it was lapped, and the file's bytes are the spec's (ADR 0014). The
//! lapped case comes first (`.claude/rules/data-plane.md`, "Bounds"). Every test works in
//! a directory of its own under the system's temporary directory and removes it after.
//! Miri has no file-backed mapping, so `just miri` skips these and runs the layout, cell
//! and conformance tests.

use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use cavalry_shm::broadcast::layout::{
    FILE_NAME, HEADER_LENGTH, LATEST_RECORD_OFFSET, TAIL_INTENT_OFFSET, TAIL_POSITION_OFFSET,
    TRAILER_LENGTH, encode_header,
};
use cavalry_shm::broadcast::{BroadcastBuffer, BroadcastError, Positions};

struct Dir(PathBuf);

impl Dir {
    fn new(test: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "cavalry-shm-broadcast-{}-{test}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("a temporary directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn file(&self) -> PathBuf {
        self.0.join(FILE_NAME)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const C: u32 = 4096;

fn le_i64(bytes: &[u8], offset: usize) -> i64 {
    i64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

fn patch(dir: &Dir, offset: u64, bytes: &[u8]) {
    let mut file = fs::OpenOptions::new().write(true).open(dir.file()).unwrap();
    file.seek(SeekFrom::Start(offset)).unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
}

/// Every record the receiver has, as (type, message), until none is available.
fn drain(receiver: &mut cavalry_shm::broadcast::Receiver) -> Vec<(i32, Vec<u8>)> {
    let mut out = Vec::new();
    while receiver
        .receive(|type_id, message| out.push((type_id, message.to_vec())))
        .unwrap()
    {}
    out
}

#[test]
#[cfg_attr(miri, ignore)]
fn the_lapped_receiver_before_anything_else() {
    let dir = Dir::new("lapped");
    let buffer = BroadcastBuffer::create(dir.path(), C).unwrap();
    let mut transmitter = buffer.transmitter().unwrap();
    let client = BroadcastBuffer::open(dir.path(), false).unwrap();
    let mut receiver = client.receiver().unwrap();
    assert!(
        !receiver.receive(|_, _| unreachable!()).unwrap(),
        "nothing yet"
    );

    // Nine records of 512 bytes: eight fill the ring, the ninth overwrites the first,
    // and the transmitter never waited for the receiver.
    for n in 0..9u8 {
        transmitter.transmit(i32::from(n) + 1, &[n; 504]).unwrap();
    }
    assert_eq!(
        buffer.positions(),
        Positions {
            tail: 4608,
            tail_intent: 4608,
            latest: 4096
        }
    );
    let lapped = receiver.receive(|_, _| unreachable!()).unwrap_err();
    assert_eq!(lapped.reason(), "lapped");
    assert!(
        matches!(
            lapped,
            BroadcastError::Lapped {
                cursor: 0,
                tail_intent: 4608,
                latest: 4096
            }
        ),
        "{lapped}"
    );
    assert_eq!((receiver.cursor(), receiver.next_record()), (4096, 4096));

    // It continues from the latest record and keeps up from there.
    assert_eq!(drain(&mut receiver), vec![(9, vec![8; 504])]);
    transmitter.transmit(10, b"after").unwrap();
    assert_eq!(drain(&mut receiver), vec![(10, b"after".to_vec())]);
    assert_eq!(receiver.next_record(), 4624);
}

#[test]
#[cfg_attr(miri, ignore)]
fn create_then_open_round_trips_and_the_bytes_are_the_specs() {
    let dir = Dir::new("round-trip");
    let buffer = BroadcastBuffer::create(dir.path(), C).unwrap();
    let header = buffer.header();
    assert_eq!(header.capacity(), C);
    assert_eq!(header.creator_pid(), i64::from(std::process::id()));
    assert_eq!(buffer.max_message_length(), 512);

    let bytes = fs::read(dir.file()).unwrap();
    assert_eq!(bytes.len() as u64, header.file_length());
    let trailer = &bytes[C as usize..];
    assert_eq!(trailer.len(), TRAILER_LENGTH);
    assert_eq!(&trailer[..HEADER_LENGTH], &encode_header(&header)[..]);
    assert_eq!(&trailer[..8], b"CVLYBCST");
    assert_eq!(le_i64(trailer, TAIL_INTENT_OFFSET), 0);
    assert_eq!(le_i64(trailer, TAIL_POSITION_OFFSET), 0);
    assert_eq!(le_i64(trailer, LATEST_RECORD_OFFSET), 0);

    let mut transmitter = buffer.transmitter().unwrap();
    transmitter.transmit(1, b"x").unwrap();
    let bytes = fs::read(dir.file()).unwrap();
    assert_eq!(&bytes[..4], &9i32.to_le_bytes(), "record length 8 + 1");
    assert_eq!(&bytes[4..8], &1i32.to_le_bytes(), "type");
    assert_eq!(bytes[8], b'x');
    let trailer = &bytes[C as usize..];
    assert_eq!(le_i64(trailer, TAIL_INTENT_OFFSET), 16);
    assert_eq!(le_i64(trailer, TAIL_POSITION_OFFSET), 16);
    assert_eq!(le_i64(trailer, LATEST_RECORD_OFFSET), 0);

    let doctor = BroadcastBuffer::open(dir.path(), false).unwrap();
    assert_eq!(doctor.header(), header);
    assert_eq!(
        doctor.positions(),
        Positions {
            tail: 16,
            tail_intent: 16,
            latest: 0
        }
    );
    let refused = doctor.transmitter().unwrap_err();
    assert_eq!(refused.reason(), "io");
    assert!(refused.to_string().contains("read-only"), "{refused}");

    // One transmitter per buffer: not a second one while the first lives, and none from a
    // buffer that was opened, even writable; dropping the first lets the creator hand out
    // another.
    let second = buffer.transmitter().unwrap_err();
    assert_eq!(second.reason(), "io");
    assert!(
        second.to_string().contains("already has a transmitter"),
        "{second}"
    );
    let opened = BroadcastBuffer::open(dir.path(), true).unwrap();
    let refused = opened.transmitter().unwrap_err();
    assert_eq!(refused.reason(), "io");
    assert!(
        refused.to_string().contains("only the driver that created"),
        "{refused}"
    );
    assert!(
        refused
            .to_string()
            .contains("docs/reference/broadcast-buffer.md#"),
        "{refused}"
    );
    drop(transmitter);
    let mut transmitter = buffer.transmitter().unwrap();
    transmitter.transmit(1, b"y").unwrap();

    let again = BroadcastBuffer::create(dir.path(), C).unwrap_err();
    assert_eq!(
        again.reason(),
        "io",
        "a buffer that exists is not recreated: {again}"
    );
    assert!(
        again
            .to_string()
            .contains("docs/reference/broadcast-buffer.md#file")
    );
}

#[test]
#[cfg_attr(miri, ignore)]
fn records_are_received_in_order_and_padding_is_skipped() {
    let dir = Dir::new("order");
    let buffer = BroadcastBuffer::create(dir.path(), C).unwrap();
    let mut transmitter = buffer.transmitter().unwrap();
    let mut receiver = BroadcastBuffer::open(dir.path(), false)
        .unwrap()
        .receiver()
        .unwrap();

    // Eight 512-byte records fill the ring exactly, no padding, and are received in
    // order; a receiver exactly a ring behind counts as lapped, so it drains before the
    // eighth. A short one then lands at 0.
    let mut sent = Vec::new();
    for n in 0..7u8 {
        transmitter.transmit(1, &[n; 504]).unwrap();
        sent.push((1, vec![n; 504]));
    }
    assert_eq!(drain(&mut receiver), sent);
    transmitter.transmit(1, &[7; 504]).unwrap();
    assert_eq!(drain(&mut receiver), vec![(1, vec![7; 504])]);
    transmitter.transmit(2, b"short").unwrap(); // 16 bytes at 0
    assert_eq!(drain(&mut receiver), vec![(2, b"short".to_vec())]);
    assert_eq!(receiver.next_record(), 4096 + 16);

    // Seven more 512-byte records reach index 3600; the next does not fit the 496 to the
    // end: a padding record fills them and the record begins at 0, which the receiver
    // skips over without delivering anything.
    let mut sent = Vec::new();
    for n in 0..7u8 {
        transmitter.transmit(3, &[n; 504]).unwrap();
        sent.push((3, vec![n; 504]));
    }
    assert_eq!(drain(&mut receiver), sent);
    transmitter.transmit(3, &[7; 504]).unwrap();
    assert_eq!(
        buffer.positions(),
        Positions {
            tail: 2 * 4096 + 512,
            tail_intent: 2 * 4096 + 512,
            latest: 2 * 4096
        }
    );
    assert_eq!(drain(&mut receiver), vec![(3, vec![7; 504])]);
    assert_eq!(
        (receiver.cursor(), receiver.next_record()),
        (2 * 4096, 2 * 4096 + 512)
    );

    // A zero-length message is a record too.
    transmitter.transmit(4, &[]).unwrap();
    assert_eq!(drain(&mut receiver), vec![(4, vec![])]);
}

#[test]
#[cfg_attr(miri, ignore)]
fn a_receiver_joins_at_the_latest_record() {
    let dir = Dir::new("join");
    let buffer = BroadcastBuffer::create(dir.path(), C).unwrap();
    let mut transmitter = buffer.transmitter().unwrap();
    for n in 1..=3 {
        transmitter.transmit(n, &[n as u8; 8]).unwrap();
    }
    let mut late = buffer.receiver().unwrap();
    assert_eq!((late.cursor(), late.next_record()), (32, 32));
    assert_eq!(drain(&mut late), vec![(3, vec![3; 8])]);
}

#[test]
#[cfg_attr(miri, ignore)]
fn message_and_type_are_refused_before_anything_is_written() {
    let dir = Dir::new("refused");
    let buffer = BroadcastBuffer::create(dir.path(), C).unwrap();
    let mut transmitter = buffer.transmitter().unwrap();
    assert_eq!(transmitter.transmit(0, b"x").unwrap_err().reason(), "type");
    assert_eq!(transmitter.transmit(-1, b"x").unwrap_err().reason(), "type");
    let error = transmitter.transmit(1, &[0; 513]).unwrap_err();
    assert_eq!(error.reason(), "message");
    assert!(
        matches!(
            error,
            BroadcastError::Message {
                found: 513,
                limit: 512
            }
        ),
        "{error}"
    );
    assert_eq!(
        buffer.positions(),
        Positions {
            tail: 0,
            tail_intent: 0,
            latest: 0
        }
    );
    transmitter.transmit(1, &[0; 512]).unwrap();
    assert_eq!(buffer.positions().tail, 520);
}

#[test]
#[cfg_attr(miri, ignore)]
fn corrupt_positions_and_records_are_rejected_by_name() {
    let dir = Dir::new("corrupt");
    let buffer = BroadcastBuffer::create(dir.path(), C).unwrap();
    let mut transmitter = buffer.transmitter().unwrap();
    let mut receiver = BroadcastBuffer::open(dir.path(), false)
        .unwrap()
        .receiver()
        .unwrap();
    transmitter.transmit(1, b"fine").unwrap();
    transmitter.transmit(2, b"bad!").unwrap();

    // The second record's type becomes 0: the writer is the driver, so the file is
    // corrupt, and the receiver that was not lapped says so.
    patch(&dir, 16 + 4, &0i32.to_le_bytes());
    assert!(
        receiver
            .receive(|type_id, _| assert_eq!(type_id, 1))
            .unwrap()
    );
    let error = receiver.receive(|_, _| unreachable!()).unwrap_err();
    assert_eq!(error.reason(), "record");
    assert!(
        matches!(
            error,
            BroadcastError::Record {
                index: 16,
                length: 12,
                type_id: 0
            }
        ),
        "{error}"
    );

    // A latest record at the tail is a transmit in progress, not a fault, so the corrupt
    // record is still what the receiver reports; a negative latest record is `tail`.
    patch(
        &dir,
        u64::from(C) + LATEST_RECORD_OFFSET as u64,
        &32i64.to_le_bytes(),
    );
    assert_eq!(
        receiver
            .receive(|_, _| unreachable!())
            .unwrap_err()
            .reason(),
        "record"
    );
    patch(
        &dir,
        u64::from(C) + LATEST_RECORD_OFFSET as u64,
        &(-8i64).to_le_bytes(),
    );
    let error = receiver.receive(|_, _| unreachable!()).unwrap_err();
    assert_eq!(error.reason(), "tail");
    assert!(
        matches!(
            error,
            BroadcastError::Tail {
                tail: 32,
                tail_intent: 32,
                latest: -8,
                next_record: 16
            }
        ),
        "{error}"
    );
}

/// A valid file whose trailer header is then changed by `change`, for the header checks.
fn buffer_with_header(dir: &Dir, change: impl FnOnce(&mut [u8])) {
    drop(BroadcastBuffer::create(dir.path(), C).unwrap());
    let mut bytes = fs::read(dir.file()).unwrap();
    let trailer = bytes.len() - TRAILER_LENGTH;
    change(&mut bytes[trailer..trailer + HEADER_LENGTH]);
    fs::write(dir.file(), bytes).unwrap();
}

#[test]
#[cfg_attr(miri, ignore)]
fn a_reader_rejects_a_bad_file_by_name() {
    let dir = Dir::new("rejects");
    let open = |dir: &Dir| BroadcastBuffer::open(dir.path(), false).unwrap_err();

    fs::write(dir.file(), [0u8; 100]).unwrap();
    assert_eq!(open(&dir).reason(), "too-short");
    fs::remove_file(dir.file()).unwrap();

    buffer_with_header(&dir, |h| h[..8].copy_from_slice(&[0; 8]));
    assert_eq!(open(&dir).reason(), "not-ready");
    fs::remove_file(dir.file()).unwrap();

    buffer_with_header(&dir, |h| h[..8].copy_from_slice(b"CVLYCMDR"));
    let error = open(&dir);
    assert_eq!(error.reason(), "magic");
    assert!(error.to_string().contains("43564c59434d4452"), "{error}");
    fs::remove_file(dir.file()).unwrap();

    buffer_with_header(&dir, |h| h[8] = 1);
    assert_eq!(open(&dir).reason(), "version");
    fs::remove_file(dir.file()).unwrap();

    buffer_with_header(&dir, |h| h[12..16].copy_from_slice(&2048i32.to_le_bytes()));
    assert_eq!(open(&dir).reason(), "capacity");
    fs::remove_file(dir.file()).unwrap();

    buffer_with_header(&dir, |h| h[12..16].copy_from_slice(&8192i32.to_le_bytes()));
    let error = open(&dir);
    assert_eq!(error.reason(), "length");
    assert!(
        matches!(
            error,
            BroadcastError::Length {
                found: 4608,
                needed: 8704
            }
        ),
        "{error}"
    );
    fs::remove_file(dir.file()).unwrap();

    drop(BroadcastBuffer::create(dir.path(), C).unwrap());
    fs::OpenOptions::new()
        .write(true)
        .open(dir.file())
        .unwrap()
        .set_len(u64::from(C) + TRAILER_LENGTH as u64 - 8)
        .unwrap();
    assert_eq!(
        open(&dir).reason(),
        "not-ready",
        "truncated, the last 512 bytes are not the trailer"
    );
}

#[test]
#[cfg_attr(miri, ignore)]
fn every_error_names_the_page_that_explains_it() {
    let errors = [
        BroadcastError::TooShort { given: 1 },
        BroadcastError::NotReady,
        BroadcastError::Magic {
            found: *b"CVLYCNTR",
        },
        BroadcastError::Version { found: 1 },
        BroadcastError::Capacity { found: 100 },
        BroadcastError::Length {
            found: 1,
            needed: 2,
        },
        BroadcastError::Changed,
        BroadcastError::Tail {
            tail: 0,
            tail_intent: 0,
            latest: 8,
            next_record: 0,
        },
        BroadcastError::Lapped {
            cursor: 0,
            tail_intent: 8192,
            latest: 8168,
        },
        BroadcastError::Record {
            index: 0,
            length: 4,
            type_id: 1,
        },
        BroadcastError::Message {
            found: 513,
            limit: 512,
        },
        BroadcastError::Type { found: 0 },
        BroadcastError::Io(std::io::Error::other("no")),
    ];
    for error in errors {
        let text = error.to_string();
        assert!(
            text.contains("docs/reference/broadcast-buffer.md#"),
            "{text}"
        );
        assert!(text.contains(&format!("({})", error.reason())), "{text}");
    }
}
