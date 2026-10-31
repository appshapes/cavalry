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

/// A stopped recording of two segments, 128 bytes in each; returns its id, descriptor
/// and stop position, with the second segment's base at `SEGMENT_FILE_LENGTH`.
fn two_segments(dir: &TestDir, test: &str) -> (i64, Descriptor, i64) {
    let _ = test;
    let (id, found) = dir.add(1001, 0);
    dir.write_frames(&found, 0, 2);
    let second = i64::from(SEGMENT_FILE_LENGTH);
    let stop = dir.write_frames(&found, second, 2);
    dir.write_index(id, &[(NOW, 0), (NOW + MS, second), (NOW + 2 * MS, stop)]);
    let mut writer = CatalogWriter::open(dir.path(), NOW).expect("the catalog opens");
    writer
        .stop(id, stop, NOW + 2 * MS)
        .expect("the stop is written");
    let found = writer.get(id).expect("reads").expect("holds");
    (id, found, stop)
}

/// `purge segments`' catalog write (ADR 0031): the header moves to version 1, the slot to
/// version 1's form with the retained position at the segment base, and a reader of the
/// other form still reads a slot nobody purged; a crash before the files went leaves the
/// leading segment for recovery to remove, counted in `files_removed`.
#[test]
fn a_purge_moves_the_retained_position_and_recovery_removes_the_leading_files() {
    let dir = TestDir::new("purge-retained");
    let (id, found, stop) = two_segments(&dir, "purge-retained");
    let second = i64::from(SEGMENT_FILE_LENGTH);
    let (live, _) = dir.add(1002, 0);
    {
        let mut writer = CatalogWriter::open(dir.path(), NOW).expect("the catalog opens");
        assert_eq!(writer.version(), 0);
        let live_error = writer.retain(live, 0).unwrap_err().reason();
        assert_eq!(live_error, "transition", "a live recording is not purged");
        assert_eq!(
            writer.retain(id, 64).unwrap_err().reason(),
            "position",
            "not a segment base"
        );
        assert_eq!(
            writer.retain(id, stop + second).unwrap_err().reason(),
            "position",
            "above the stop"
        );
        writer
            .retain(id, second)
            .expect("the retained position is written");
        assert_eq!(writer.version(), 1);
        let retained = writer.get(id).expect("reads").expect("holds");
        assert_eq!(retained.form(), cavalry_log::layout::Form::Version1);
        assert_eq!(retained.retained_position(), second);
        assert_eq!(retained.stop_position(), stop);
        // The other slot stayed in version 0's form under the version 1 header.
        let other = writer.get(1).expect("reads").expect("holds");
        assert_eq!(other.form(), cavalry_log::layout::Form::Version0);
        assert_eq!(other.retained_position(), 0);
    }
    assert!(
        dir.segment(&found, 0).exists(),
        "the purge's files are the replayer's to remove"
    );
    // A crash before the file work: recovery removes the leading segment and reads
    // the recording from its retained position, nothing to rebuild.
    let (log, recovery) = LogDir::open(dir.path(), NOW + 3 * MS).expect("the directory opens");
    assert_eq!(recovery.files_removed, 1, "{recovery:?}");
    // The one rebuild is the live recording of the transition check, which the crash
    // left with no frames: an empty recording, not this one.
    assert_eq!(recovery.recovered, 1, "{recovery:?}");
    assert!(!dir.segment(&found, 0).exists());
    assert!(dir.segment(&found, second).exists());
    let after = log.catalog().get(id).expect("reads").expect("holds");
    assert_eq!(after.retained_position(), second);
    assert_eq!(after.stop_position(), stop);
    assert_eq!(
        read(&dir, id)
            .expect("reads")
            .expect("holds")
            .retained_position(),
        second
    );
}

/// A retained group torn between the position and its checksum, by a crash inside a
/// migration or a purge: recovery rebuilds it from the files, the base of the lowest
/// segment present, counted in `recording.recovered` (ADR 0031).
#[test]
fn a_torn_retained_group_is_rebuilt_from_the_files() {
    let dir = TestDir::new("torn-retained");
    let (id, found, stop) = two_segments(&dir, "torn-retained");
    let second = i64::from(SEGMENT_FILE_LENGTH);
    {
        let mut writer = CatalogWriter::open(dir.path(), NOW).expect("the catalog opens");
        writer
            .retain(id, second)
            .expect("the retained position is written");
    }
    fs::remove_file(dir.segment(&found, 0)).expect("the purge removed the leading file");
    // The checksum torn: the group reads as another position's.
    let slot = CatalogHeader::slot_offset(id as u64);
    dir.write_catalog_at(&0xDEAD_BEEFu32.to_le_bytes(), slot + 480);
    assert_eq!(
        read(&dir, id).unwrap_err(),
        "checksum",
        "a reader reports the torn group"
    );
    let (log, recovery) = LogDir::open(dir.path(), NOW + 3 * MS).expect("the directory opens");
    assert_eq!(recovery.recovered, 1, "{recovery:?}");
    assert_eq!(recovery.invalidated, 0, "{recovery:?}");
    let after = log.catalog().get(id).expect("reads").expect("holds");
    assert_eq!(after.form(), cavalry_log::layout::Form::Version1);
    assert_eq!(
        after.retained_position(),
        second,
        "the lowest segment present"
    );
    assert_eq!(after.stop_position(), stop, "the stop was not touched");
}

/// A truncate torn by a crash after the stop was reopened and the segment cut, before
/// the new stop was written: the slot reads "live, or being cut", and recovery rebuilds
/// the stop from the files as cut, dropping the index entries past it (ADR 0031).
#[test]
fn a_cut_torn_before_its_stop_is_rebuilt_from_the_files_as_cut() {
    let dir = TestDir::new("torn-cut");
    let (id, found) = dir.add(1001, 0);
    let stop = dir.write_frames(&found, 0, 4);
    assert_eq!(stop, 256);
    dir.write_index(id, &[(NOW, 0), (NOW + MS, 128), (NOW + 2 * MS, 256)]);
    {
        let mut writer = CatalogWriter::open(dir.path(), NOW).expect("the catalog opens");
        writer
            .stop(id, stop, NOW + 2 * MS)
            .expect("the stop is written");
        writer.open_cut(id).expect("the cut opens");
        let cutting = writer.get(id).expect("reads").expect("holds");
        assert!(cutting.is_live(), "the stop reads -1 while the cut runs");
        assert_eq!(
            writer.open_cut(id).unwrap_err().reason(),
            "transition",
            "a cut is not reopened"
        );
    }
    // The replayer's file work, done to the position 128: the segment cut and extended.
    let segment = dir.segment(&found, 0);
    let file = OpenOptions::new()
        .write(true)
        .open(&segment)
        .expect("the segment opens");
    file.set_len(128).expect("cut");
    file.set_len(SEGMENT_FILE_LENGTH as u64).expect("extended");
    drop(file);
    // The crash: the index and the stop were not written.
    let (log, recovery) = LogDir::open(dir.path(), NOW + 3 * MS).expect("the directory opens");
    assert_eq!(recovery.recovered, 1, "{recovery:?}");
    assert_eq!(
        recovery.index_rewritten, 1,
        "the entries past the cut dropped: {recovery:?}"
    );
    let after = log.catalog().get(id).expect("reads").expect("holds");
    assert_eq!(after.stop_position(), 128);
    assert_eq!(after.stop_timestamp(), NOW + MS, "the last entry left");
    assert_eq!(dir.index_entries(id), vec![(NOW, 0), (NOW + MS, 128)]);
}

/// `purge recording`'s catalog write: state 3 on a stopped recording, refused on a live
/// one and on a deleted one; recovery removes the files it finds (ADR 0024, ADR 0031).
#[test]
fn a_delete_writes_state_3_once_and_recovery_removes_the_files() {
    let dir = TestDir::new("delete");
    let (id, found, _stop) = two_segments(&dir, "delete");
    let (live, _) = dir.add(1002, 0);
    {
        let mut writer = CatalogWriter::open(dir.path(), NOW).expect("the catalog opens");
        assert_eq!(writer.delete(live).unwrap_err().reason(), "transition");
        writer.delete(id).expect("deleted");
        assert_eq!(
            writer.delete(id).unwrap_err().reason(),
            "transition",
            "deleted once"
        );
        assert_eq!(
            writer.get(id).expect("reads").expect("holds").state(),
            State::Deleted
        );
    }
    let (_log, recovery) = LogDir::open(dir.path(), NOW + 3 * MS).expect("the directory opens");
    assert_eq!(
        recovery.files_removed, 3,
        "two segments and the index: {recovery:?}"
    );
    assert!(!dir.segment(&found, 0).exists());
    assert!(!dir.index(id).exists());
}

/// The time index's cut (ADR 0031): entries past the position go, the last entry left
/// gives the stop timestamp, and a cut below every entry keeps the first entry alone.
#[test]
fn the_index_is_cut_at_a_position() {
    let dir = TestDir::new("index-cut");
    let (id, _found) = dir.add(1001, 0);
    dir.write_index(id, &[(NOW, 0), (NOW + MS, 128), (NOW + 2 * MS, 256)]);
    assert_eq!(
        cavalry_log::index::truncate_at(dir.path(), id, 128).expect("cut"),
        Some(NOW + MS)
    );
    assert_eq!(dir.index_entries(id), vec![(NOW, 0), (NOW + MS, 128)]);
    assert_eq!(
        cavalry_log::index::truncate_at(dir.path(), id, 64).expect("cut"),
        Some(NOW)
    );
    assert_eq!(dir.index_entries(id), vec![(NOW, 0)]);
    let (id2, _found) = dir.add(1002, 4096);
    dir.write_index(id2, &[(NOW, 4096), (NOW + MS, 8192)]);
    assert_eq!(
        cavalry_log::index::truncate_at(dir.path(), id2, 0).expect("cut"),
        None,
        "below every entry"
    );
    assert_eq!(
        dir.index_entries(id2),
        vec![(NOW, 4096)],
        "the first entry stays"
    );
}
