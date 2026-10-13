//! The catalog's writer and reader on a real file: the bound, the ids that are not
//! there, a creation and a header torn by a crash, a stop torn by a crash, a delete,
//! and the checks [`CatalogWriter::rewrite`] makes before it writes
//! (`spec/durable-log.md`, "Catalog header", "Descriptor" and "The driver"; ADR 0024;
//! CLAUDE.md, invariants 1 and 3).

use std::fs::OpenOptions;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cavalry_log::catalog::{CatalogError, CatalogReader, CatalogWriter};
use cavalry_log::layout::{
    CATALOG_FILE_NAME, CATALOG_MAGIC, CatalogHeader, DESCRIPTOR_LENGTH, Descriptor, HEADER_LENGTH,
    MAX_SLOTS, Recording, State,
};

const NOW: i64 = 1_760_000_000_000_000_000;

/// A directory of its own per test, removed when the guard drops.
struct TestDir(PathBuf);

impl TestDir {
    fn new(test: &str) -> Self {
        let path = std::env::temp_dir().join(format!("cavalry-log-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("a test directory");
        TestDir(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn catalog(&self) -> PathBuf {
        self.0.join(CATALOG_FILE_NAME)
    }

    fn catalog_length(&self) -> u64 {
        std::fs::metadata(self.catalog())
            .expect("the catalog exists")
            .len()
    }

    fn write_at(&self, bytes: &[u8], offset: u64) {
        OpenOptions::new()
            .write(true)
            .open(self.catalog())
            .expect("the catalog opens")
            .write_all_at(bytes, offset)
            .expect("the bytes are written");
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn recording(stream_id: i32) -> Recording<'static> {
    Recording {
        recording_id: 0,
        start_timestamp: NOW,
        start_position: 0,
        initial_term_id: 7,
        term_length: 64 * 1024 * 1024,
        mtu_length: 1408,
        session_id: 1,
        stream_id,
        channel: "cavalry:ipc",
        source_identity: "cavalry:ipc",
    }
}

fn reason<T>(outcome: Result<T, CatalogError>) -> &'static str {
    match outcome {
        Ok(_) => panic!("accepted, and a refusal was expected"),
        Err(error) => error.reason(),
    }
}

#[test]
fn add_at_the_bound_is_full_and_writes_nothing() {
    let dir = TestDir::new("full");
    drop(CatalogWriter::open(dir.path(), NOW).expect("a new catalog"));
    // A sparse file of exactly the bound's whole slots: no disk is used for them.
    let at_bound = CatalogHeader::slot_offset(MAX_SLOTS);
    OpenOptions::new()
        .write(true)
        .open(dir.catalog())
        .expect("the catalog opens")
        .set_len(at_bound)
        .expect("the catalog grows");
    // A last slot that is not empty, so that open counts it.
    let last = Descriptor::new(&Recording {
        recording_id: (MAX_SLOTS - 1) as i64,
        ..recording(1)
    })
    .expect("a valid recording");
    dir.write_at(&last.encode(), CatalogHeader::slot_offset(MAX_SLOTS - 1));
    let mut writer = CatalogWriter::open(dir.path(), NOW).expect("a catalog at its bound opens");
    assert_eq!(writer.slots(), MAX_SLOTS);
    assert!(matches!(writer.add(&recording(1)), Err(CatalogError::Full)));
    assert_eq!(dir.catalog_length(), at_bound, "nothing is written");
    let message = writer.add(&recording(1)).expect_err("full").to_string();
    assert!(
        message.contains("move or archive the log directory"),
        "{message}"
    );
}

#[test]
fn an_id_out_of_range_or_negative_is_unknown() {
    let dir = TestDir::new("unknown");
    let mut writer = CatalogWriter::open(dir.path(), NOW).expect("a new catalog");
    let id = writer.add(&recording(1)).expect("added");
    assert_eq!(id, 0);
    let reader = CatalogReader::open(dir.path()).expect("the catalog opens");
    for missing in [-1, 1, i64::MIN, i64::MAX] {
        assert_eq!(reason(writer.get(missing)), "unknown", "{missing}");
        assert_eq!(reason(reader.get(missing)), "unknown", "{missing}");
        assert_eq!(
            reason(writer.stop(missing, 4096, NOW)),
            "unknown",
            "{missing}"
        );
        assert_eq!(reason(writer.delete(missing)), "unknown", "{missing}");
        assert_eq!(reason(writer.invalidate(missing)), "unknown", "{missing}");
    }
}

#[test]
fn a_stop_is_read_back_by_the_reader() {
    let dir = TestDir::new("stop");
    let mut writer = CatalogWriter::open(dir.path(), NOW).expect("a new catalog");
    let id = writer.add(&recording(1001)).expect("added");
    let reader = CatalogReader::open(dir.path()).expect("the catalog opens");
    let live = reader.get(id).expect("read").expect("a recording");
    assert!(live.is_live());
    assert_eq!(live.stop_timestamp(), -1);
    writer.stop(id, 4096, NOW + 1_000).expect("stopped");
    let stopped = reader.get(id).expect("read").expect("a recording");
    assert!(!stopped.is_live());
    assert_eq!(stopped.stop_position(), 4096);
    assert_eq!(stopped.stop_timestamp(), NOW + 1_000);
    assert_eq!(stopped.stream_id(), 1001);
    assert_eq!(writer.get(id).expect("read"), Some(stopped));
}

#[test]
fn a_delete_keeps_both_checksums_valid() {
    let dir = TestDir::new("delete");
    let mut writer = CatalogWriter::open(dir.path(), NOW).expect("a new catalog");
    let id = writer.add(&recording(1)).expect("added");
    writer.stop(id, 4096, NOW + 1).expect("stopped");
    writer.delete(id).expect("deleted");
    let reader = CatalogReader::open(dir.path()).expect("the catalog opens");
    let deleted = reader
        .get(id)
        .expect("both checksums hold")
        .expect("a slot");
    assert_eq!(deleted.state(), State::Deleted);
    assert_eq!(deleted.stop_position(), 4096);
}

#[test]
fn a_torn_creation_reads_as_empty_and_the_next_add_takes_its_id() {
    let dir = TestDir::new("torn-creation");
    let mut writer = CatalogWriter::open(dir.path(), NOW).expect("a new catalog");
    assert_eq!(writer.add(&recording(1)).expect("added"), 0);
    drop(writer);
    // A crash between the creation fields and the state: every byte but the state.
    let mut torn = Descriptor::new(&Recording {
        recording_id: 1,
        ..recording(2)
    })
    .expect("a valid recording")
    .encode();
    torn[..4].copy_from_slice(&0i32.to_le_bytes());
    dir.write_at(&torn, CatalogHeader::slot_offset(1));
    let reader = CatalogReader::open(dir.path()).expect("the catalog opens");
    assert_eq!(reader.slots().expect("slots"), 2);
    assert_eq!(reader.get(1).expect("an empty slot reads"), None);
    let mut writer = CatalogWriter::open(dir.path(), NOW).expect("the catalog opens");
    assert_eq!(writer.slots(), 1, "the empty last slot is not counted");
    assert_eq!(writer.add(&recording(3)).expect("added"), 1);
    let added = reader.get(1).expect("read").expect("a recording");
    assert_eq!(added.stream_id(), 3);
}

#[test]
fn a_stop_checksum_that_fails_is_retried_then_reported() {
    let dir = TestDir::new("stop-checksum");
    let mut writer = CatalogWriter::open(dir.path(), NOW).expect("a new catalog");
    let id = writer.add(&recording(1)).expect("added");
    writer.stop(id, 4096, NOW + 1).expect("stopped");
    dir.write_at(&[0xDE, 0xAD, 0xBE, 0xEF], CatalogHeader::slot_offset(0) + 4);
    let reader = CatalogReader::open(dir.path()).expect("the catalog opens");
    let started = Instant::now();
    let error = reader.get(id).expect_err("a torn stop");
    assert_eq!(error.reason(), "checksum");
    assert!(
        started.elapsed() >= Duration::from_millis(100),
        "retried for {:?}",
        started.elapsed()
    );
    assert_eq!(reason(writer.get(id)), "checksum");
}

#[test]
fn a_catalog_shorter_than_its_header_is_written_again() {
    let dir = TestDir::new("torn-short");
    std::fs::write(dir.catalog(), [0x5A; 50]).expect("a torn catalog");
    assert_eq!(reason(CatalogReader::open(dir.path())), "too-short");
    let writer = CatalogWriter::open(dir.path(), NOW).expect("the header is written again");
    assert_eq!(writer.slots(), 0);
    assert_eq!(dir.catalog_length(), HEADER_LENGTH as u64);
    let bytes = std::fs::read(dir.catalog()).expect("the catalog reads");
    assert_eq!(bytes[..8], CATALOG_MAGIC);
    let header = CatalogHeader::parse(&bytes, bytes.len() as u64).expect("a whole header");
    assert_eq!(header.created(), NOW);
    CatalogReader::open(dir.path()).expect("the catalog opens");
}

#[test]
fn a_header_not_ready_with_no_slot_is_written_again() {
    let dir = TestDir::new("torn-magic");
    let mut header = CatalogHeader::new(NOW - 5).expect("a header").encode();
    header[..8].copy_from_slice(&[0; 8]);
    let mut bytes = header.to_vec();
    bytes.extend_from_slice(&[0; 100]); // part of a slot, never a whole one
    std::fs::write(dir.catalog(), &bytes).expect("a torn catalog");
    assert_eq!(reason(CatalogReader::open(dir.path())), "not-ready");
    let mut writer = CatalogWriter::open(dir.path(), NOW).expect("the header is written again");
    assert_eq!(dir.catalog_length(), HEADER_LENGTH as u64);
    assert_eq!(writer.add(&recording(1)).expect("added"), 0);
    CatalogReader::open(dir.path()).expect("the catalog opens");
}

#[test]
fn a_header_not_ready_over_whole_slots_is_refused() {
    let dir = TestDir::new("not-ready-slots");
    let mut writer = CatalogWriter::open(dir.path(), NOW).expect("a new catalog");
    writer.add(&recording(1)).expect("added");
    drop(writer);
    dir.write_at(&[0; 8], 0);
    assert_eq!(reason(CatalogWriter::open(dir.path(), NOW)), "not-ready");
    assert_eq!(
        dir.catalog_length(),
        CatalogHeader::slot_offset(1),
        "nothing is rewritten"
    );
}

#[test]
fn rewrite_checks_the_slot_before_it_writes() {
    let dir = TestDir::new("rewrite");
    let mut writer = CatalogWriter::open(dir.path(), NOW).expect("a new catalog");
    let first = writer.add(&recording(1)).expect("added");
    let second = writer.add(&recording(2)).expect("added");
    let stopped_first = writer
        .get(first)
        .expect("read")
        .expect("a recording")
        .stopped(4096, NOW + 1)
        .expect("a valid stop");
    let length = dir.catalog_length();

    // Past the last slot: nothing is written, and the file does not grow.
    assert_eq!(reason(writer.rewrite(2, &stopped_first)), "unknown");
    assert_eq!(reason(writer.rewrite(u64::MAX, &stopped_first)), "unknown");
    assert_eq!(dir.catalog_length(), length);
    // Another slot's descriptor.
    assert_eq!(reason(writer.rewrite(1, &stopped_first)), "recording-id");
    // The right id, other creation fields.
    let other = Descriptor::new(&Recording {
        recording_id: second,
        ..recording(9)
    })
    .expect("a valid recording");
    assert_eq!(reason(writer.rewrite(1, &other)), "mismatch");
    // A slot the driver found damaged.
    let stopped_second = writer
        .get(second)
        .expect("read")
        .expect("a recording")
        .stopped(4096, NOW + 1)
        .expect("a valid stop");
    writer.invalidate(second).expect("invalidated");
    assert_eq!(reason(writer.rewrite(1, &stopped_second)), "invalid");
    // A creation checksum that fails is never re-signed.
    let mut slot = [0u8; DESCRIPTOR_LENGTH];
    slot.copy_from_slice(&std::fs::read(dir.catalog()).expect("read")[128..128 + 1024]);
    dir.write_at(&[0xFF], CatalogHeader::slot_offset(0) + 100);
    assert_eq!(reason(writer.rewrite(0, &stopped_first)), "checksum");
    dir.write_at(&slot, CatalogHeader::slot_offset(0));

    // A torn stop is what rewrite repairs.
    dir.write_at(&[0xDE, 0xAD, 0xBE, 0xEF], CatalogHeader::slot_offset(0) + 4);
    assert_eq!(reason(writer.get(first)), "checksum");
    writer
        .rewrite(0, &stopped_first)
        .expect("the stop is rebuilt");
    let reader = CatalogReader::open(dir.path()).expect("the catalog opens");
    assert_eq!(reader.get(first).expect("read"), Some(stopped_first));
}

/// The bytes of slot `slot`, for asserting that a refused write wrote nothing.
fn slot_bytes(dir: &TestDir, slot: u64) -> Vec<u8> {
    let offset = CatalogHeader::slot_offset(slot) as usize;
    std::fs::read(dir.catalog()).expect("read")[offset..offset + DESCRIPTOR_LENGTH].to_vec()
}

#[test]
fn a_stop_is_written_once_and_refused_after() {
    let dir = TestDir::new("stop-once");
    let mut writer = CatalogWriter::open(dir.path(), NOW).expect("a new catalog");
    let id = writer.add(&recording(1)).expect("added");
    writer.stop(id, 8192, NOW + 1).expect("stopped");
    let before = slot_bytes(&dir, 0);
    assert_eq!(reason(writer.stop(id, 4096, NOW + 2)), "transition");
    assert_eq!(slot_bytes(&dir, 0), before, "a refused stop writes nothing");
    writer.delete(id).expect("deleted");
    let before = slot_bytes(&dir, 0);
    assert_eq!(reason(writer.stop(id, 4096, NOW + 3)), "transition");
    assert_eq!(
        slot_bytes(&dir, 0),
        before,
        "a stop after a delete writes nothing"
    );
}

#[test]
fn a_delete_needs_a_stopped_recording_and_happens_once() {
    let dir = TestDir::new("delete-once");
    let mut writer = CatalogWriter::open(dir.path(), NOW).expect("a new catalog");
    let id = writer.add(&recording(1)).expect("added");
    let before = slot_bytes(&dir, 0);
    assert_eq!(reason(writer.delete(id)), "transition", "a live recording");
    assert_eq!(slot_bytes(&dir, 0), before);
    writer.stop(id, 4096, NOW + 1).expect("stopped");
    writer.delete(id).expect("deleted");
    let before = slot_bytes(&dir, 0);
    assert_eq!(reason(writer.delete(id)), "transition", "already deleted");
    assert_eq!(slot_bytes(&dir, 0), before);
    writer
        .invalidate(id)
        .expect("a deleted recording can still be marked damaged");
}

#[test]
fn an_invalidation_marks_damage_once_and_never_an_empty_slot() {
    let dir = TestDir::new("invalidate");
    let mut writer = CatalogWriter::open(dir.path(), NOW).expect("a new catalog");
    let id = writer.add(&recording(1)).expect("added");
    // Damage the creation fields: get reports checksum, and invalidate still marks it.
    dir.write_at(&[0xFF], CatalogHeader::slot_offset(0) + 100);
    assert_eq!(reason(writer.get(id)), "checksum");
    writer
        .invalidate(id)
        .expect("damage is marked whatever the checksums say");
    assert_eq!(reason(writer.get(id)), "invalid");
    let before = slot_bytes(&dir, 0);
    assert_eq!(
        reason(writer.invalidate(id)),
        "transition",
        "already invalid"
    );
    assert_eq!(slot_bytes(&dir, 0), before);
    assert_eq!(
        reason(writer.invalidate(id + 1)),
        "unknown",
        "past the last slot"
    );
}

#[test]
fn open_truncates_bytes_past_the_last_whole_slot() {
    let dir = TestDir::new("truncate");
    let mut writer = CatalogWriter::open(dir.path(), NOW).expect("a new catalog");
    writer.add(&recording(1)).expect("added");
    drop(writer);
    let whole = std::fs::metadata(dir.catalog()).expect("stat").len();
    dir.write_at(&[7u8; 100], whole);
    assert_eq!(
        std::fs::metadata(dir.catalog()).expect("stat").len(),
        whole + 100
    );
    let writer = CatalogWriter::open(dir.path(), NOW).expect("opens");
    assert_eq!(writer.slots(), 1);
    assert_eq!(
        std::fs::metadata(dir.catalog()).expect("stat").len(),
        whole,
        "truncated to whole slots"
    );
}
