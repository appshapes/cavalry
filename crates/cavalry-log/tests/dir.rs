//! The log directory on real files: the lock that keeps a second driver out, and the
//! recovery pass of `spec/durable-log.md`, "The driver", case by case: a torn catalog,
//! an empty slot before the last, a creation checksum that fails, a recording live at a
//! crash, a torn stop, a missing index, an empty recording, damage, a deleted
//! recording's files, and segments past the stop (ADR 0024, ADR 0025; CLAUDE.md,
//! invariants 1 and 3). The full case comes first.

use std::fs::{self, OpenOptions};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use cavalry_log::catalog::{CatalogReader, CatalogWriter};
use cavalry_log::dir::{DirError, LogDir, Recovery};
use cavalry_log::layout::{
    CatalogHeader, Descriptor, ENTRY_LENGTH, HEADER_LENGTH, IndexEntry, IndexHeader, LayoutError,
    Recording, SEGMENT_FILE_LENGTH, State, index_file_name, locate, segment_file_name,
};
use cavalry_proto::frame::{DataFrameHeader, Flags, FrameType};

const NOW: i64 = 1_760_000_000_000_000_000;
const MS: i64 = 1_000_000;
const TERM: i32 = 65536;
const SESSION: i32 = 7;
const INITIAL_TERM_ID: i32 = 3;

/// A directory of its own per test, removed when the guard drops.
struct TestDir(PathBuf);

impl TestDir {
    fn new(test: &str) -> Self {
        let path = std::env::temp_dir().join(format!("cavalry-dir-{}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("a test directory");
        TestDir(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn write_catalog_at(&self, bytes: &[u8], offset: u64) {
        OpenOptions::new()
            .write(true)
            .open(self.0.join("catalog"))
            .expect("the catalog opens")
            .write_all_at(bytes, offset)
            .expect("the bytes are written");
    }

    /// Adds a live recording on `stream_id` from `start`, as the driver would, and
    /// returns its id and descriptor. The writer is dropped, so nothing holds the lock.
    fn add(&self, stream_id: i32, start: i64) -> (i64, Descriptor) {
        let mut writer = CatalogWriter::open(self.path(), NOW).expect("the catalog opens");
        let id = writer
            .add(&Recording {
                recording_id: 0,
                start_timestamp: NOW,
                start_position: start,
                initial_term_id: INITIAL_TERM_ID,
                term_length: TERM,
                mtu_length: 1408,
                session_id: SESSION,
                stream_id,
                channel: "cavalry:ipc",
                source_identity: "cavalry:ipc",
            })
            .expect("a slot");
        let descriptor = writer
            .get(id)
            .expect("the slot reads")
            .expect("it holds the recording");
        (id, descriptor)
    }

    /// Writes `count` frames of 64 bytes from `from` into the recording's segment,
    /// creating the segment at its full length first; returns the position after them.
    fn write_frames(&self, found: &Descriptor, from: i64, count: usize) -> i64 {
        let mut position = from;
        for _ in 0..count {
            let located = locate(found, position).expect("a position in the recording");
            let path = self.0.join(&located.segment_file);
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&path)
                .expect("the segment opens");
            if file.metadata().expect("stat").len() == 0 {
                file.set_len(SEGMENT_FILE_LENGTH as u64)
                    .expect("the segment extends");
            }
            let header = DataFrameHeader::new(FrameType::Data, Flags::UNFRAGMENTED, 64)
                .expect("a header")
                .with_term_offset(located.term_offset)
                .with_term_id(located.term_id)
                .with_session_id(found.session_id())
                .with_stream_id(found.stream_id())
                .to_bytes();
            let mut frame = [0xABu8; 64];
            frame[..32].copy_from_slice(&header);
            file.write_all_at(&frame, located.offset_in_segment as u64)
                .expect("the frame is written");
            position += 64;
        }
        position
    }

    fn segment(&self, found: &Descriptor, base: i64) -> PathBuf {
        self.0.join(segment_file_name(found.recording_id(), base))
    }

    fn index(&self, id: i64) -> PathBuf {
        self.0.join(index_file_name(id))
    }

    fn write_index(&self, id: i64, entries: &[(i64, i64)]) {
        let mut bytes = IndexHeader::new(id).expect("a header").encode().to_vec();
        for &(timestamp, position) in entries {
            bytes.extend_from_slice(
                &IndexEntry {
                    timestamp,
                    position,
                }
                .encode(),
            );
        }
        fs::write(self.index(id), bytes).expect("the index is written");
    }

    fn index_entries(&self, id: i64) -> Vec<(i64, i64)> {
        let bytes = fs::read(self.index(id)).expect("the index reads");
        bytes[HEADER_LENGTH..]
            .as_chunks::<ENTRY_LENGTH>()
            .0
            .iter()
            .map(|chunk| {
                let entry = IndexEntry::parse(chunk, None).expect("an entry");
                (entry.timestamp, entry.position)
            })
            .collect()
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn read(dir: &TestDir, id: i64) -> Result<Option<Descriptor>, &'static str> {
    CatalogReader::open(dir.path())
        .expect("the reader opens")
        .get(id)
        .map_err(|error| error.reason())
}

#[test]
fn a_second_opener_is_refused_until_the_first_drops() {
    let dir = TestDir::new("lock");
    let (first, recovery) = LogDir::open(dir.path(), NOW).expect("the first opener holds it");
    assert_eq!(recovery, Recovery::default());
    let refused = LogDir::open(dir.path(), NOW).unwrap_err();
    assert_eq!(refused.reason(), "locked");
    assert!(
        matches!(&refused, DirError::Locked { dir: d } if d == dir.path()),
        "{refused}"
    );
    let message = refused.to_string();
    assert!(message.contains("--log-dir"), "{message}");
    assert!(
        message.contains("docs/reference/durable-log.md#the-driver"),
        "{message}"
    );
    assert_eq!(first.catalog().slots(), 0);
    drop(first);
    LogDir::open(dir.path(), NOW).expect("the lock went with its holder");
}

#[test]
fn an_empty_or_missing_directory_gets_a_catalog_and_nothing_to_recover() {
    let dir = TestDir::new("fresh");
    let nested = dir.path().join("deeper").join("log");
    let (log, recovery) = LogDir::open(&nested, NOW).expect("the directory is created");
    assert_eq!(recovery, Recovery::default());
    assert_eq!(log.dir(), nested.as_path());
    assert!(nested.join("catalog").exists());
    assert_eq!(
        CatalogReader::open(&nested)
            .expect("opens")
            .slots()
            .expect("reads"),
        0
    );
}

#[test]
fn a_torn_catalog_is_truncated_and_an_empty_last_slot_is_dropped() {
    let dir = TestDir::new("torn-catalog");
    let (id, _) = dir.add(1001, 0);
    CatalogWriter::open(dir.path(), NOW)
        .expect("opens")
        .stop(id, 64, NOW + MS)
        .expect("stops");
    // A whole zeroed slot after the last, then part of another: a creation torn twice over.
    dir.write_catalog_at(&[0u8; 1024 + 100], CatalogHeader::slot_offset(1));
    let (log, recovery) = LogDir::open(dir.path(), NOW).expect("opens");
    assert_eq!(
        recovery,
        Recovery {
            truncated: true,
            dropped_empty_slot: true,
            ..Recovery::default()
        }
    );
    assert_eq!(
        log.catalog().slots(),
        1,
        "the empty slot is the next recording's"
    );
    assert_eq!(
        fs::metadata(dir.path().join("catalog"))
            .expect("stat")
            .len(),
        CatalogHeader::slot_offset(1),
        "whole slots only, the empty last slot dropped from the file"
    );
    drop(log);
    let (_log, recovery) = LogDir::open(dir.path(), NOW).expect("opens again");
    assert_eq!(
        recovery,
        Recovery::default(),
        "a drop happens once; a later open finds nothing to drop"
    );
}

#[test]
fn an_empty_slot_before_the_last_and_a_bad_creation_checksum_are_invalidated() {
    let dir = TestDir::new("damaged-slots");
    for stream in [1, 2, 3] {
        let (id, _) = dir.add(stream, 0);
        CatalogWriter::open(dir.path(), NOW)
            .expect("opens")
            .stop(id, 0, NOW)
            .expect("stops");
    }
    dir.write_catalog_at(&[0u8; 4], CatalogHeader::slot_offset(0));
    let offset = CatalogHeader::slot_offset(1) + 24;
    let mut checksum = [0u8; 4];
    OpenOptions::new()
        .read(true)
        .open(dir.path().join("catalog"))
        .expect("opens")
        .read_exact_at(&mut checksum, offset)
        .expect("reads");
    checksum[0] ^= 1;
    dir.write_catalog_at(&checksum, offset);
    let (_log, recovery) = LogDir::open(dir.path(), NOW).expect("opens");
    assert_eq!(
        recovery,
        Recovery {
            invalidated: 2,
            ..Recovery::default()
        }
    );
    assert_eq!(read(&dir, 0), Err("invalid"));
    assert_eq!(read(&dir, 1), Err("invalid"));
    assert_eq!(
        read(&dir, 2).expect("reads").expect("holds").state(),
        State::Valid
    );
    // Opened again, nothing is left to do: damage is marked once.
    drop(_log);
    let (_log, recovery) = LogDir::open(dir.path(), NOW).expect("opens again");
    assert_eq!(recovery, Recovery::default());
}

#[test]
fn a_recording_live_at_a_crash_stops_where_its_frames_end_and_its_index_is_cut() {
    let dir = TestDir::new("live-at-crash");
    let (id, found) = dir.add(1001, 0);
    let end = dir.write_frames(&found, 0, 3);
    assert_eq!(end, 192);
    // The last entry points past what the segment kept, as a power loss leaves it.
    dir.write_index(id, &[(NOW, 0), (NOW + MS, 128), (NOW + 2 * MS, 256)]);
    let (_log, recovery) = LogDir::open(dir.path(), NOW).expect("opens");
    assert_eq!(
        recovery,
        Recovery {
            recovered: 1,
            index_rewritten: 1,
            ..Recovery::default()
        }
    );
    let stopped = read(&dir, id).expect("reads").expect("holds");
    assert_eq!(stopped.state(), State::Valid);
    assert_eq!(stopped.stop_position(), 192);
    assert_eq!(stopped.stop_timestamp(), NOW + MS, "the last entry left");
    assert_eq!(dir.index_entries(id), vec![(NOW, 0), (NOW + MS, 128)]);
    assert!(
        dir.segment(&found, 0).exists(),
        "the segment holding the stop stays"
    );
}

#[test]
fn a_torn_stop_is_rebuilt_and_a_missing_or_torn_index_is_written_again() {
    let dir = TestDir::new("torn-stop");
    let (id, found) = dir.add(1001, 0);
    dir.write_frames(&found, 0, 2);
    // The stop fields written, the checksum not: the first write of a stop.
    let offset = CatalogHeader::slot_offset(0);
    dir.write_catalog_at(&(NOW + 5 * MS).to_le_bytes(), offset + 8);
    dir.write_catalog_at(&4096i64.to_le_bytes(), offset + 16);
    let (other, other_found) = dir.add(1002, 0);
    dir.write_frames(&other_found, 0, 1);
    fs::write(dir.index(other), [0u8; 20]).expect("a torn index");
    let (_log, recovery) = LogDir::open(dir.path(), NOW).expect("opens");
    assert_eq!(
        recovery,
        Recovery {
            recovered: 2,
            index_rewritten: 2,
            ..Recovery::default()
        }
    );
    let stopped = read(&dir, id).expect("reads").expect("holds");
    assert_eq!(
        (stopped.stop_position(), stopped.stop_timestamp()),
        (128, NOW),
        "derived from the files, not from the torn fields"
    );
    assert_eq!(dir.index_entries(id), vec![(NOW, 0)]);
    let stopped = read(&dir, other).expect("reads").expect("holds");
    assert_eq!(stopped.stop_position(), 64);
    assert_eq!(dir.index_entries(other), vec![(NOW, 0)]);
}

#[test]
fn a_recording_with_no_files_or_a_zero_first_frame_stops_at_its_start() {
    let dir = TestDir::new("empty-recording");
    let (none, _) = dir.add(1001, 64);
    let (zero, zero_found) = dir.add(1002, 64);
    let segment = dir.segment(&zero_found, 0);
    fs::File::create(&segment)
        .expect("creates")
        .set_len(SEGMENT_FILE_LENGTH as u64)
        .expect("extends");
    let (_log, recovery) = LogDir::open(dir.path(), NOW).expect("opens");
    assert_eq!(
        recovery,
        Recovery {
            recovered: 2,
            index_rewritten: 2,
            ..Recovery::default()
        }
    );
    for id in [none, zero] {
        let stopped = read(&dir, id).expect("reads").expect("holds");
        assert_eq!(stopped.state(), State::Valid);
        assert_eq!(
            (stopped.stop_position(), stopped.stop_timestamp()),
            (64, NOW)
        );
        assert_eq!(dir.index_entries(id), vec![(NOW, 64)]);
    }
    assert!(
        segment.exists(),
        "the segment holding the start position is not wholly past the stop"
    );
}

#[test]
fn damage_is_invalidated_and_the_creation_fields_are_left_alone() {
    let dir = TestDir::new("damage");
    // The start segment is missing while a later one exists.
    let (missing_start, found) = dir.add(1001, 0);
    fs::File::create(dir.segment(&found, i64::from(SEGMENT_FILE_LENGTH))).expect("creates");
    // A segment between the start and the last is missing.
    let (missing_middle, found) = dir.add(1002, 0);
    dir.write_frames(&found, 0, 1);
    fs::File::create(dir.segment(&found, 2 * i64::from(SEGMENT_FILE_LENGTH))).expect("creates");
    // The frame at the start position has a length and fails the check.
    let (bad_frame, found) = dir.add(1003, 0);
    dir.write_frames(&found, 0, 1);
    let wrong = DataFrameHeader::new(FrameType::Data, Flags::UNFRAGMENTED, 64)
        .expect("a header")
        .with_term_id(INITIAL_TERM_ID)
        .with_session_id(SESSION)
        .with_stream_id(9999)
        .to_bytes();
    OpenOptions::new()
        .write(true)
        .open(dir.segment(&found, 0))
        .expect("opens")
        .write_all_at(&wrong, 0)
        .expect("writes");
    let (_log, recovery) = LogDir::open(dir.path(), NOW).expect("opens");
    assert_eq!(
        recovery,
        Recovery {
            invalidated: 3,
            index_rewritten: 3,
            ..Recovery::default()
        }
    );
    for id in [missing_start, missing_middle, bad_frame] {
        assert_eq!(read(&dir, id), Err("invalid"));
        let mut bytes = [0u8; 1024];
        OpenOptions::new()
            .read(true)
            .open(dir.path().join("catalog"))
            .expect("opens")
            .read_exact_at(&mut bytes, CatalogHeader::slot_offset(id as u64))
            .expect("reads");
        let creation = Descriptor::parse_for_recovery(
            &{
                let mut valid = bytes;
                valid[..4].copy_from_slice(&1i32.to_le_bytes());
                valid
            },
            id,
        );
        assert!(
            matches!(creation, Ok(Some((ref d, _))) if d.stream_id() == 1001 + id as i32),
            "the creation fields are untouched: {creation:?}"
        );
    }
    assert!(matches!(Descriptor::parse(&[0u8; 1024], 0), Ok(None)));
    assert!(matches!(LayoutError::Invalid.reason(), "invalid"));
}

#[test]
fn a_deleted_recording_loses_its_files_again_and_a_stopped_one_is_left_alone() {
    let dir = TestDir::new("deleted");
    let (deleted, deleted_found) = dir.add(1001, 0);
    let (stopped, stopped_found) = dir.add(1002, 0);
    dir.write_frames(&deleted_found, 0, 1);
    dir.write_index(deleted, &[(NOW, 0)]);
    dir.write_frames(&stopped_found, 0, 1);
    dir.write_index(stopped, &[(NOW, 0), (NOW + MS, 64)]);
    {
        let mut writer = CatalogWriter::open(dir.path(), NOW).expect("opens");
        writer.stop(deleted, 64, NOW + MS).expect("stops");
        // State 3 as card 95's purge will write it, with the files still present.
        dir.write_catalog_at(
            &(State::Deleted as i32).to_le_bytes(),
            CatalogHeader::slot_offset(deleted as u64),
        );
        writer.stop(stopped, 64, NOW + MS).expect("stops");
    }
    let (_log, recovery) = LogDir::open(dir.path(), NOW).expect("opens");
    assert_eq!(
        recovery,
        Recovery {
            files_removed: 2,
            ..Recovery::default()
        }
    );
    assert!(!dir.segment(&deleted_found, 0).exists());
    assert!(!dir.index(deleted).exists());
    assert!(dir.segment(&stopped_found, 0).exists());
    assert_eq!(dir.index_entries(stopped), vec![(NOW, 0), (NOW + MS, 64)]);
    assert_eq!(
        read(&dir, deleted).expect("reads").expect("holds").state(),
        State::Deleted
    );
}

#[test]
fn segments_wholly_past_the_stop_are_removed_and_the_scan_walks_the_index_back() {
    let dir = TestDir::new("past-stop");
    let (id, found) = dir.add(1001, 0);
    let end = dir.write_frames(&found, 0, 3);
    fs::File::create(dir.segment(&found, i64::from(SEGMENT_FILE_LENGTH)))
        .expect("the next segment, pre-extended before the crash")
        .set_len(SEGMENT_FILE_LENGTH as u64)
        .expect("extends");
    // Every entry but the first points past the data: the walk back lands on the start.
    dir.write_index(id, &[(NOW, 0), (NOW + MS, 256), (NOW + 2 * MS, 320)]);
    let (_log, recovery) = LogDir::open(dir.path(), NOW).expect("opens");
    assert_eq!(
        recovery,
        Recovery {
            recovered: 1,
            index_rewritten: 1,
            files_removed: 1,
            ..Recovery::default()
        }
    );
    let stopped = read(&dir, id).expect("reads").expect("holds");
    assert_eq!(
        (stopped.stop_position(), stopped.stop_timestamp()),
        (end, NOW)
    );
    assert!(dir.segment(&found, 0).exists());
    assert!(!dir.segment(&found, i64::from(SEGMENT_FILE_LENGTH)).exists());
    assert_eq!(dir.index_entries(id), vec![(NOW, 0)]);
}

#[test]
fn every_error_names_the_page_that_explains_it() {
    let errors = [
        DirError::Locked {
            dir: PathBuf::from("/tmp/log"),
        },
        DirError::Io {
            what: "lock",
            path: PathBuf::from("/tmp/log/catalog"),
            source: std::io::Error::other("refused"),
        },
    ];
    for error in errors {
        let message = error.to_string();
        assert!(
            message.contains("docs/reference/durable-log.md#"),
            "{message}"
        );
        assert!(!error.reason().is_empty());
    }
}

#[test]
fn a_huge_sparse_index_is_cut_in_fixed_chunks_without_being_held() {
    // An index whose length is far past its whole entries, as a damaged or sparse file
    // leaves it: the repair reads forward in fixed chunks, stops at the first entry out
    // of order (the zeros after the real entries) and truncates there; it never holds
    // the file, so a gigabyte of index costs one chunk of memory.
    let dir = TestDir::new("huge-index");
    let (id, found) = dir.add(1001, 0);
    dir.write_frames(&found, 0, 2);
    dir.write_index(id, &[(NOW, 0), (NOW + MS, 64)]);
    OpenOptions::new()
        .write(true)
        .open(dir.index(id))
        .expect("opens")
        .set_len(HEADER_LENGTH as u64 + (1u64 << 30))
        .expect("a sparse gigabyte");
    let (_log, recovery) = LogDir::open(dir.path(), NOW).expect("opens");
    assert_eq!(
        recovery,
        Recovery {
            recovered: 1,
            index_rewritten: 1,
            ..Recovery::default()
        }
    );
    assert_eq!(dir.index_entries(id), vec![(NOW, 0), (NOW + MS, 64)]);
    let stopped = read(&dir, id).expect("reads").expect("holds");
    assert_eq!(
        (stopped.stop_position(), stopped.stop_timestamp()),
        (128, NOW + MS)
    );
}

#[test]
fn an_index_with_many_entries_past_the_stop_is_cut_by_a_search() {
    // 100,000 whole entries in order, spanning many read chunks, of which all but three
    // point past the data: the walk back finds the third, the scan stops at the end of
    // the frames, and the cut finds the first entry past the stop by a search.
    let dir = TestDir::new("many-entries");
    let (id, found) = dir.add(1001, 0);
    let end = dir.write_frames(&found, 0, 3);
    let mut entries: Vec<(i64, i64)> = vec![(NOW, 0), (NOW + MS, 64), (NOW + 2 * MS, 128)];
    for j in 0..100_000i64 {
        entries.push((NOW + (3 + j) * MS, 256 + 64 * j));
    }
    dir.write_index(id, &entries);
    let (_log, recovery) = LogDir::open(dir.path(), NOW).expect("opens");
    assert_eq!(
        recovery,
        Recovery {
            recovered: 1,
            index_rewritten: 1,
            ..Recovery::default()
        }
    );
    assert_eq!(dir.index_entries(id), entries[..3]);
    let stopped = read(&dir, id).expect("reads").expect("holds");
    assert_eq!(
        (stopped.stop_position(), stopped.stop_timestamp()),
        (end, NOW + 2 * MS)
    );
}
