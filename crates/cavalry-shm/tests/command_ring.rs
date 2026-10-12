//! The command ring on disk: `CommandRing` creates and opens it with the spec's checks,
//! a `Producer` claims, commits and writes, a `Consumer` reads, clears, unblocks and
//! stores the heartbeat, and the file's bytes are the spec's (ADR 0014). The full case
//! comes first (`.claude/rules/data-plane.md`, "Bounds"). Every test works in a directory
//! of its own under the system's temporary directory and removes it after. Miri has no
//! file-backed mapping, so `just miri` skips these and runs the layout, cell and
//! conformance tests.

use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cavalry_shm::command_ring::layout::{
    CONSUMER_HEARTBEAT_OFFSET, CORRELATION_ID_COUNTER_OFFSET, FILE_NAME,
    HEAD_CACHE_POSITION_OFFSET, HEAD_POSITION_OFFSET, HEADER_LENGTH, ReadStop,
    TAIL_POSITION_OFFSET, TRAILER_LENGTH, encode_header,
};
use cavalry_shm::command_ring::{
    CommandRing, CommandRingError, RecordHeader, Rejection, UnblockOutcome,
};

struct Dir(PathBuf);

impl Dir {
    fn new(test: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "cavalry-shm-command-ring-{}-{test}",
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
const TIMEOUT: Duration = Duration::from_secs(10);

fn now() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    )
    .unwrap()
}

fn le_i64(bytes: &[u8], offset: usize) -> i64 {
    i64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

/// Writes `bytes` into the ring's file at `offset`, as a process that breaks the spec
/// would; the mappings share the page cache and see it.
fn patch(dir: &Dir, offset: u64, bytes: &[u8]) {
    let mut file = fs::OpenOptions::new().write(true).open(dir.file()).unwrap();
    file.seek(SeekFrom::Start(offset)).unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
}

#[test]
#[cfg_attr(miri, ignore)]
fn the_full_ring_before_anything_else() {
    let dir = Dir::new("full");
    let ring = CommandRing::create(dir.path(), C).unwrap();
    let mut consumer = ring.consumer(TIMEOUT).unwrap();
    let client = CommandRing::open(dir.path(), true).unwrap();
    let mut producer = client.producer(TIMEOUT).unwrap();

    // 256 records of 16 bytes fill the ring exactly; the 257th finds no room and nothing
    // is written, not even padding.
    for n in 0..=255u8 {
        producer.write(1, &[n; 8], now()).unwrap();
    }
    assert_eq!(ring.producer_position(), 4096);
    let full = producer.write(1, &[0; 8], now()).unwrap_err();
    assert_eq!(full.reason(), "full");
    assert!(
        matches!(full, CommandRingError::Full { needed: 16 }),
        "{full}"
    );
    assert_eq!(ring.producer_position(), 4096, "nothing was claimed");
    assert_eq!(ring.size(), 4096);

    // The driver reads all of them, in order, and the ring is empty again.
    let mut seen = Vec::new();
    let read = consumer
        .read(usize::MAX, |type_id, message| {
            seen.push((type_id, message.to_vec()))
        })
        .unwrap();
    assert_eq!(
        (read.messages, read.bytes, read.stop),
        (256, 4096, ReadStop::EndOfRing)
    );
    assert!(
        seen.iter()
            .enumerate()
            .all(|(n, (type_id, message))| *type_id == 1 && message == &[n as u8; 8]),
        "every record in order"
    );
    assert_eq!(ring.consumer_position(), 4096);
    assert_eq!(ring.size(), 0);

    // A record that does not fit before the end of the ring, while the head blocks the
    // start of it, claims only the padding to the end: `full`, and the tail at the end.
    producer.write(2, &[0; 8], now()).unwrap(); // 16 bytes at index 0
    for n in 0..169u8 {
        producer.write(3, &[n; 16], now()).unwrap(); // 24 bytes each, to index 4072
    }
    producer.write(4, &[], now()).unwrap(); // 8 bytes, to index 4080
    assert_eq!(ring.producer_position(), 4096 + 4080);
    let read = consumer
        .read(1, |type_id, _| assert_eq!(type_id, 2))
        .unwrap();
    assert_eq!(
        (read.messages, read.bytes, read.stop),
        (1, 16, ReadStop::Limit)
    );
    let full = producer.write(5, &[0; 16], now()).unwrap_err();
    assert_eq!(full.reason(), "full");
    assert_eq!(
        ring.producer_position(),
        2 * 4096,
        "the padding to the end was claimed"
    );
    let mut types = Vec::new();
    let read = consumer
        .read(usize::MAX, |type_id, _| types.push(type_id))
        .unwrap();
    assert_eq!(
        (read.messages, read.bytes, read.stop),
        (170, 4080, ReadStop::EndOfRing)
    );
    assert_eq!(types[..169], [3; 169]);
    assert_eq!(types[169], 4);
    assert_eq!(ring.consumer_position(), 2 * 4096);

    // Free again: the claim that was refused now lands at index 0.
    let claim = producer.try_claim(5, 16, now()).unwrap();
    assert_eq!(claim.index(), 0);
    claim.commit().unwrap();
    let read = consumer
        .read(16, |type_id, _| assert_eq!(type_id, 5))
        .unwrap();
    assert_eq!((read.messages, read.stop), (1, ReadStop::Unclaimed));
}

#[test]
#[cfg_attr(miri, ignore)]
fn create_then_open_round_trips_and_the_bytes_are_the_specs() {
    let dir = Dir::new("round-trip");
    let ring = CommandRing::create(dir.path(), C).unwrap();
    let header = ring.header();
    assert_eq!(header.capacity(), C);
    assert_eq!(header.creator_pid(), i64::from(std::process::id()));
    assert!(header.start_timestamp() > 0);
    assert_eq!(ring.max_message_length(), 512);

    let bytes = fs::read(dir.file()).unwrap();
    assert_eq!(bytes.len() as u64, header.file_length());
    let trailer = &bytes[C as usize..];
    assert_eq!(trailer.len(), TRAILER_LENGTH);
    assert_eq!(&trailer[..HEADER_LENGTH], &encode_header(&header)[..]);
    assert_eq!(&trailer[..8], b"CVLYCMDR");
    assert_eq!(le_i64(trailer, TAIL_POSITION_OFFSET), 0);
    assert_eq!(le_i64(trailer, HEAD_CACHE_POSITION_OFFSET), 0);
    assert_eq!(le_i64(trailer, HEAD_POSITION_OFFSET), 0);
    assert_eq!(le_i64(trailer, CORRELATION_ID_COUNTER_OFFSET), 0);
    assert_eq!(
        le_i64(trailer, CONSUMER_HEARTBEAT_OFFSET),
        header.start_timestamp(),
        "the heartbeat is stored before the magic"
    );
    assert!(
        bytes[..C as usize].iter().all(|b| *b == 0),
        "the ring starts zero"
    );

    let doctor = CommandRing::open(dir.path(), false).unwrap();
    assert_eq!(doctor.header(), header);
    assert_eq!(doctor.consumer_heartbeat(), header.start_timestamp());
    assert_eq!(
        (
            doctor.producer_position(),
            doctor.consumer_position(),
            doctor.size()
        ),
        (0, 0, 0)
    );
    let refused = doctor.producer(TIMEOUT).unwrap_err();
    assert_eq!(refused.reason(), "io");
    assert!(refused.to_string().contains("read-only"), "{refused}");
    assert_eq!(doctor.consumer(TIMEOUT).unwrap_err().reason(), "io");

    let again = CommandRing::create(dir.path(), C).unwrap_err();
    assert_eq!(
        again.reason(),
        "io",
        "a ring that exists is not recreated: {again}"
    );
    assert!(
        again
            .to_string()
            .contains("docs/reference/command-ring.md#file")
    );
}

#[test]
#[cfg_attr(miri, ignore)]
fn a_claim_commits_aborts_or_is_aborted_on_drop_and_the_consumer_sees_it() {
    let dir = Dir::new("claim");
    let ring = CommandRing::create(dir.path(), C).unwrap();
    let mut consumer = ring.consumer(TIMEOUT).unwrap();
    let mut producer = ring.producer(TIMEOUT).unwrap();

    let mut claim = producer.try_claim(7, 5, now()).unwrap();
    assert_eq!(claim.body().unwrap().len(), 5);
    claim.body().unwrap().copy_from_slice(b"hello");
    claim.commit().unwrap();
    producer.try_claim(8, 3, now()).unwrap().abort().unwrap();
    drop(producer.try_claim(9, 3, now()).unwrap());
    producer.write(10, b"bye", now()).unwrap();
    assert_eq!(ring.producer_position(), 16 + 16 + 16 + 16);

    let mut seen = Vec::new();
    let read = consumer
        .read(usize::MAX, |type_id, message| {
            seen.push((type_id, message.to_vec()))
        })
        .unwrap();
    assert_eq!(seen, vec![(7, b"hello".to_vec()), (10, b"bye".to_vec())]);
    assert_eq!(
        (read.messages, read.bytes, read.stop),
        (2, 64, ReadStop::Unclaimed)
    );
    let bytes = fs::read(dir.file()).unwrap();
    assert!(
        bytes[..64].iter().all(|b| *b == 0),
        "the records read are cleared"
    );
    assert_eq!(ring.consumer_position(), 64);
}

#[test]
#[cfg_attr(miri, ignore)]
fn correlation_ids_count_up_across_producers() {
    let dir = Dir::new("correlation");
    let ring = CommandRing::create(dir.path(), C).unwrap();
    let one = ring.producer(TIMEOUT).unwrap();
    let two = CommandRing::open(dir.path(), true)
        .unwrap()
        .producer(TIMEOUT)
        .unwrap();
    assert_eq!(one.next_correlation_id(), 0);
    assert_eq!(two.next_correlation_id(), 1);
    assert_eq!(one.next_correlation_id(), 2);
}

#[test]
#[cfg_attr(miri, ignore)]
fn the_heartbeat_tells_a_closed_driver_from_a_stopped_one() {
    let dir = Dir::new("heartbeat");
    let ring = CommandRing::create(dir.path(), C).unwrap();
    let consumer = ring.consumer(TIMEOUT).unwrap();
    let mut producer = ring.producer(Duration::from_secs(1)).unwrap();
    let start = ring.header().start_timestamp();

    assert!(producer.try_claim(1, 0, start + 500_000_000).is_ok());
    let stale = producer.try_claim(1, 0, start + 2_000_000_000).unwrap_err();
    assert_eq!(stale.reason(), "driver-timeout");
    assert!(
        matches!(
            stale,
            CommandRingError::DriverTimeout {
                age: 2_000_000_000,
                timeout: 1_000_000_000
            }
        ),
        "{stale}"
    );
    consumer.store_heartbeat(start + 2_000_000_000);
    assert_eq!(producer.consumer_heartbeat(), start + 2_000_000_000);
    assert!(producer.try_claim(1, 0, start + 2_500_000_000).is_ok());
    consumer.close();
    let closed = producer.try_claim(1, 0, start + 2_500_000_000).unwrap_err();
    assert_eq!(closed.reason(), "driver-closed");
    assert_eq!(ring.consumer_heartbeat(), -1);
    assert_eq!(
        ring.producer_position(),
        16,
        "the refused claims wrote nothing"
    );
}

#[test]
#[cfg_attr(miri, ignore)]
fn the_driver_unblocks_a_claim_a_dead_client_left() {
    let dir = Dir::new("unblock");
    let ring = CommandRing::create(dir.path(), C).unwrap();
    let mut consumer = ring.consumer(TIMEOUT).unwrap();
    let client = CommandRing::open(dir.path(), true).unwrap();
    let mut producer = client.producer(TIMEOUT).unwrap();
    let start = now();
    let timeout = TIMEOUT.as_nanos() as i64;

    assert_eq!(
        consumer.unblock(start).unwrap(),
        UnblockOutcome::NoAction,
        "an empty ring"
    );
    producer.write(1, b"first", now()).unwrap();
    assert_eq!(
        consumer.unblock(start).unwrap(),
        UnblockOutcome::NoAction,
        "the head starts its clock"
    );
    assert_eq!(
        consumer.unblock(start + timeout).unwrap(),
        UnblockOutcome::NoAction,
        "a committed record waits"
    );
    consumer.read(16, |_, _| {}).unwrap();

    // A client claims and dies: the claim is never committed or aborted.
    let dead = producer.try_claim(2, 16, now()).unwrap();
    std::mem::forget(dead);
    let mut later = CommandRing::open(dir.path(), true)
        .unwrap()
        .producer(TIMEOUT)
        .unwrap();
    later.write(3, b"after", now()).unwrap();
    let read = consumer
        .read(16, |_, _| panic!("nothing is committed at the head"))
        .unwrap();
    assert_eq!((read.messages, read.stop), (0, ReadStop::Claimed));

    // The head moved, so the clock starts again here; an unblock before the head has
    // stood still for the timeout leaves the claim alone, since its client may be alive.
    let moved = start + timeout;
    assert_eq!(
        consumer.unblock(moved).unwrap(),
        UnblockOutcome::NoAction,
        "the head moved: its clock starts"
    );
    assert_eq!(
        consumer.unblock(moved + timeout - 1).unwrap(),
        UnblockOutcome::NoAction,
        "one nanosecond short of the timeout"
    );
    let read = consumer
        .read(16, |_, _| panic!("the claim is still not committed"))
        .unwrap();
    assert_eq!((read.messages, read.stop), (0, ReadStop::Claimed));
    assert_eq!(
        consumer.unblock(moved + timeout).unwrap(),
        UnblockOutcome::Unblocked {
            index: 16,
            length: 24
        }
    );
    let mut seen = Vec::new();
    let read = consumer.read(16, |type_id, _| seen.push(type_id)).unwrap();
    assert_eq!(
        seen,
        vec![3],
        "the padding is skipped and the next record delivered"
    );
    assert_eq!(read.bytes, 24 + 16);
    assert_eq!(
        consumer.unblock(moved + 2 * timeout).unwrap(),
        UnblockOutcome::NoAction
    );
    assert_eq!(ring.size(), 0);
}

#[test]
#[cfg_attr(miri, ignore)]
fn a_live_claim_is_never_unblocked_before_the_timeout() {
    let dir = Dir::new("unblock-live");
    let ring = CommandRing::create(dir.path(), C).unwrap();
    let mut consumer = ring.consumer(TIMEOUT).unwrap();
    let mut producer = ring.producer(TIMEOUT).unwrap();
    let start = now();
    let timeout = TIMEOUT.as_nanos() as i64;

    // A client holds a claim and writes it while the driver unblocks on every duty
    // cycle; until the timeout nothing pads the claim or clears its bytes.
    let mut claim = producer.try_claim(4, 8, now()).unwrap();
    for step in [0, 1, timeout / 2, timeout - 1] {
        assert_eq!(
            consumer.unblock(start + step).unwrap(),
            UnblockOutcome::NoAction,
            "{step} ns after the head stood still"
        );
        claim.body().unwrap().copy_from_slice(&step.to_le_bytes());
    }
    claim.commit().unwrap();
    let mut seen = Vec::new();
    consumer
        .read(16, |type_id, message| {
            seen.push((type_id, message.to_vec()))
        })
        .unwrap();
    assert_eq!(seen, vec![(4, (timeout - 1).to_le_bytes().to_vec())]);
    assert_eq!(
        consumer.unblock(start + 2 * timeout).unwrap(),
        UnblockOutcome::NoAction,
        "an empty ring"
    );
}

#[test]
#[cfg_attr(miri, ignore)]
fn a_ring_has_one_consumer_and_only_its_creator_hands_it_out() {
    let dir = Dir::new("one-consumer");
    let ring = CommandRing::create(dir.path(), C).unwrap();
    let consumer = ring.consumer(TIMEOUT).unwrap();

    let second = ring.consumer(TIMEOUT).unwrap_err();
    assert_eq!(second.reason(), "io");
    assert!(
        second.to_string().contains("already has a consumer"),
        "{second}"
    );

    let opened = CommandRing::open(dir.path(), true).unwrap();
    let refused = opened.consumer(TIMEOUT).unwrap_err();
    assert_eq!(refused.reason(), "io");
    assert!(
        refused.to_string().contains("only the driver that created"),
        "{refused}"
    );
    assert!(
        refused
            .to_string()
            .contains("docs/reference/command-ring.md#"),
        "{refused}"
    );
    assert!(opened.producer(TIMEOUT).is_ok(), "an opened ring produces");

    // Dropping the consumer lets the creator hand out another; still one at a time.
    drop(consumer);
    let _again = ring.consumer(TIMEOUT).unwrap();
    assert_eq!(ring.consumer(TIMEOUT).unwrap_err().reason(), "io");
}

#[test]
#[cfg_attr(miri, ignore)]
fn a_corrupt_record_stops_the_ring_by_name() {
    let dir = Dir::new("corrupt");
    let ring = CommandRing::create(dir.path(), C).unwrap();
    let mut consumer = ring.consumer(TIMEOUT).unwrap();
    let mut producer = ring.producer(TIMEOUT).unwrap();
    producer.write(1, b"good", now()).unwrap();
    producer.write(2, b"bad!", now()).unwrap();
    producer.write(3, b"next", now()).unwrap();
    patch(&dir, 16 + 4, &0i32.to_le_bytes()); // the second record's type becomes 0

    let mut seen = Vec::new();
    let error = consumer
        .read(16, |type_id, _| seen.push(type_id))
        .unwrap_err();
    assert_eq!(seen, vec![1], "the records before it are delivered");
    assert_eq!(error.reason(), "record");
    assert!(
        matches!(
            error,
            CommandRingError::Record {
                index: 16,
                length: 12,
                type_id: 0
            }
        ),
        "{error}"
    );
    assert_eq!(
        consumer.rejected(),
        Some(Rejection::Record {
            index: 16,
            header: RecordHeader::new(12, 0)
        })
    );
    assert_eq!(
        ring.consumer_position(),
        16,
        "nothing from the record on is cleared"
    );
    assert_eq!(
        consumer
            .read(16, |_, _| panic!("read nothing more"))
            .unwrap_err()
            .reason(),
        "record"
    );
    assert_eq!(consumer.unblock(now()).unwrap_err().reason(), "record");

    // A corrupt tail is the same for the unblocker.
    let dir = Dir::new("corrupt-tail");
    let ring = CommandRing::create(dir.path(), C).unwrap();
    let mut consumer = ring.consumer(TIMEOUT).unwrap();
    patch(
        &dir,
        u64::from(C) + TAIL_POSITION_OFFSET as u64,
        &12i64.to_le_bytes(),
    );
    // The unblock runs its checks once the head has stood still for the timeout.
    let start = now();
    assert_eq!(consumer.unblock(start).unwrap(), UnblockOutcome::NoAction);
    let error = consumer
        .unblock(start + TIMEOUT.as_nanos() as i64)
        .unwrap_err();
    assert_eq!(error.reason(), "tail");
    assert!(
        matches!(error, CommandRingError::Tail { tail: 12, head: 0 }),
        "{error}"
    );
    assert_eq!(
        consumer.rejected(),
        Some(Rejection::Tail { tail: 12, head: 0 })
    );
    assert_eq!(
        consumer
            .read(16, |_, _| panic!("read nothing more"))
            .unwrap_err()
            .reason(),
        "tail",
        "a bad tail stops the ring as a bad record does"
    );
    let mut producer = ring.producer(TIMEOUT).unwrap();
    assert_eq!(
        producer.try_claim(1, 0, now()).unwrap_err().reason(),
        "tail"
    );
}

#[test]
#[cfg_attr(miri, ignore)]
fn message_and_type_are_refused_before_anything_is_written() {
    let dir = Dir::new("refused");
    let ring = CommandRing::create(dir.path(), C).unwrap();
    let mut producer = ring.producer(TIMEOUT).unwrap();
    let error = producer.try_claim(0, 8, now()).unwrap_err();
    assert_eq!(error.reason(), "type");
    let error = producer.try_claim(-1, 8, now()).unwrap_err();
    assert_eq!(error.reason(), "type");
    let error = producer.write(1, &[0; 513], now()).unwrap_err();
    assert_eq!(error.reason(), "message");
    assert!(
        matches!(
            error,
            CommandRingError::Message {
                found: 513,
                limit: 512
            }
        ),
        "{error}"
    );
    producer.write(1, &[0; 512], now()).unwrap();
    assert_eq!(ring.producer_position(), 520);
}

/// A valid file whose trailer header is then changed by `change`, for the header checks.
fn ring_with_header(dir: &Dir, change: impl FnOnce(&mut [u8])) {
    drop(CommandRing::create(dir.path(), C).unwrap());
    let mut bytes = fs::read(dir.file()).unwrap();
    let trailer = bytes.len() - TRAILER_LENGTH;
    change(&mut bytes[trailer..trailer + HEADER_LENGTH]);
    fs::write(dir.file(), bytes).unwrap();
}

#[test]
#[cfg_attr(miri, ignore)]
fn a_reader_rejects_a_bad_file_by_name() {
    let dir = Dir::new("rejects");
    let open = |dir: &Dir| CommandRing::open(dir.path(), false).unwrap_err();

    fs::write(dir.file(), [0u8; 100]).unwrap();
    assert_eq!(open(&dir).reason(), "too-short");
    fs::remove_file(dir.file()).unwrap();

    ring_with_header(&dir, |h| h[..8].copy_from_slice(&[0; 8]));
    assert_eq!(open(&dir).reason(), "not-ready");
    fs::remove_file(dir.file()).unwrap();

    ring_with_header(&dir, |h| h[..8].copy_from_slice(b"CVLYBCST"));
    let error = open(&dir);
    assert_eq!(error.reason(), "magic");
    assert!(error.to_string().contains("43564c5942435354"), "{error}");
    fs::remove_file(dir.file()).unwrap();

    ring_with_header(&dir, |h| h[8] = 0);
    assert_eq!(
        open(&dir).reason(),
        "version",
        "version 0 is refused since ADR 0018"
    );
    fs::remove_file(dir.file()).unwrap();

    ring_with_header(&dir, |h| h[12..16].copy_from_slice(&4097i32.to_le_bytes()));
    assert_eq!(open(&dir).reason(), "capacity");
    fs::remove_file(dir.file()).unwrap();

    // The trailer is the file's last 768 bytes, so a header whose capacity disagrees
    // with the file's length is `length`: here a header for 8 KiB in a 4 KiB file.
    ring_with_header(&dir, |h| h[12..16].copy_from_slice(&8192i32.to_le_bytes()));
    let error = open(&dir);
    assert_eq!(error.reason(), "length");
    assert!(
        matches!(
            error,
            CommandRingError::Length {
                found: 4864,
                needed: 8960
            }
        ),
        "{error}"
    );
    fs::remove_file(dir.file()).unwrap();

    // A file padded or truncated after creation no longer ends with its header: the
    // last 768 bytes then begin in a slot or in the ring, which are zero.
    drop(CommandRing::create(dir.path(), C).unwrap());
    let resize = |length: u64| {
        fs::OpenOptions::new()
            .write(true)
            .open(dir.file())
            .unwrap()
            .set_len(length)
            .unwrap();
    };
    resize(u64::from(C) + TRAILER_LENGTH as u64 + 128);
    assert_eq!(open(&dir).reason(), "not-ready");
    resize(u64::from(C) + TRAILER_LENGTH as u64 - 8);
    assert_eq!(open(&dir).reason(), "not-ready");
}

/// A client claims, stalls past the unblock timeout, and acts after the driver has
/// padded, read and cleared its claim (ADR 0018, GitHub issue 8): the message write and
/// the commit are refused as `unblocked`, nothing lands behind the head, the next lap
/// delivers only what was committed, and the ring stays sound for every client.
#[test]
#[cfg_attr(miri, ignore)]
fn a_late_commit_after_an_unblock_is_refused_and_the_ring_stays_sound() {
    let dir = Dir::new("late-commit");
    let ring = CommandRing::create(dir.path(), C).unwrap();
    let mut consumer = ring.consumer(TIMEOUT).unwrap();
    let client = CommandRing::open(dir.path(), true).unwrap();
    let mut stalled = client.producer(TIMEOUT).unwrap();
    let mut other = CommandRing::open(dir.path(), true)
        .unwrap()
        .producer(TIMEOUT)
        .unwrap();
    let start = now();
    let timeout = TIMEOUT.as_nanos() as i64;

    // The stalled client claims at index 0 and does nothing; the driver's clock runs out.
    let mut claim = stalled.try_claim(2, 8, now()).unwrap();
    assert_eq!(claim.index(), 0);
    consumer.unblock(start).unwrap();
    assert_eq!(
        consumer.unblock(start + timeout).unwrap(),
        UnblockOutcome::Unblocked {
            index: 0,
            length: 16
        }
    );
    let read = consumer
        .read(16, |_, _| panic!("padding delivers nothing"))
        .unwrap();
    assert_eq!((read.messages, read.bytes), (0, 16));
    assert_eq!(ring.consumer_position(), 16);

    // The client wakes: its message write and its commit are refused, and the slot stays
    // clear. A second claim it makes with the same handle works, so the client recovers.
    assert!(matches!(
        claim.body(),
        Err(CommandRingError::Unblocked { index: 0 })
    ));
    assert!(matches!(
        claim.commit(),
        Err(CommandRingError::Unblocked { index: 0 })
    ));
    let bytes = fs::read(dir.file()).unwrap();
    assert!(
        bytes[..16].iter().all(|b| *b == 0),
        "nothing landed behind the head"
    );

    // Another client fills the rest of the lap and the stalled one commits late on the
    // next lap's slot: the commit of a claim whose index another record now holds fails
    // too, and the record there is delivered intact.
    for _ in 0..255 {
        other.write(3, b"12345678", now()).unwrap();
    }
    let mut seen = Vec::new();
    let read = consumer
        .read(usize::MAX, |type_id, _| seen.push(type_id))
        .unwrap();
    assert_eq!((read.messages, read.bytes), (255, 255 * 16));
    assert_eq!(ring.consumer_position(), C as i64);
    let mut lapped = other.try_claim(4, 8, now()).unwrap();
    assert_eq!(lapped.index(), 0, "the next lap begins at index 0");
    lapped.body().unwrap().copy_from_slice(b"intact!!");
    let mut stale = stalled.try_claim(5, 8, now()).unwrap();
    assert_eq!(stale.index(), 16);
    stale.body().unwrap().copy_from_slice(b"second!!");
    stale.commit().unwrap();
    lapped.commit().unwrap();
    let mut delivered = Vec::new();
    let read = consumer
        .read(usize::MAX, |type_id, message| {
            delivered.push((type_id, message.to_vec()))
        })
        .unwrap();
    assert_eq!(read.messages, 2);
    assert_eq!(
        delivered,
        vec![(4, b"intact!!".to_vec()), (5, b"second!!".to_vec())]
    );
    assert_eq!(ring.size(), 0);
    assert_eq!(ring.producer_position(), ring.consumer_position());
}

#[test]
#[cfg_attr(miri, ignore)]
fn every_error_names_the_page_that_explains_it() {
    let errors = [
        CommandRingError::TooShort { given: 1 },
        CommandRingError::NotReady,
        CommandRingError::Magic {
            found: *b"CVLYCNTR",
        },
        CommandRingError::Version { found: 0 },
        CommandRingError::Capacity { found: 100 },
        CommandRingError::Length {
            found: 1,
            needed: 2,
        },
        CommandRingError::Changed,
        CommandRingError::Tail { tail: 1, head: 0 },
        CommandRingError::Record {
            index: 0,
            length: 4,
            type_id: 1,
        },
        CommandRingError::Full { needed: 24 },
        CommandRingError::Message {
            found: 513,
            limit: 512,
        },
        CommandRingError::Type { found: 0 },
        CommandRingError::Unblocked { index: 16 },
        CommandRingError::DriverClosed,
        CommandRingError::DriverTimeout { age: 2, timeout: 1 },
        CommandRingError::Io(std::io::Error::other("no")),
    ];
    for error in errors {
        let text = error.to_string();
        assert!(text.contains("docs/reference/command-ring.md#"), "{text}");
        assert!(text.contains(&format!("({})", error.reason())), "{text}");
    }
}
