//! The recorder on real files: a block the writer refuses with nothing moved, blocks
//! written and rolled across a segment boundary and read back by position, the time
//! index's cadence, the sync cadence behind the two positions, the stop, and a stopped
//! recording that a later open finds nothing to repair (`spec/durable-log.md`, "Segment
//! files", "Time index", "The driver"; ADR 0025; CLAUDE.md, invariants 1 and 3). The
//! full case comes first.

use std::fs;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use cavalry_log::catalog::{CatalogError, CatalogReader};
use cavalry_log::dir::{LogDir, Recovery};
use cavalry_log::layout::{
    Descriptor, ENTRY_LENGTH, HEADER_LENGTH, IndexEntry, Recording, SEGMENT_FILE_LENGTH, State,
    index_file_name, locate, segment_file_name,
};
use cavalry_log::recorder::{INDEX_INTERVAL, RecorderError, RecordingWriter, SYNC_INTERVAL};
use cavalry_proto::frame::{DataFrameHeader, Flags, FrameType};

const NOW: i64 = 1_760_000_000_000_000_000;
const MS: i64 = 1_000_000;
const TERM: i32 = 65536;
const SEGMENT: i64 = SEGMENT_FILE_LENGTH as i64;

struct TestDir(PathBuf);

impl TestDir {
    fn new(test: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("cavalry-recorder-{}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        TestDir(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn index_entries(&self, id: i64) -> Vec<(i64, i64)> {
        let bytes = fs::read(self.0.join(index_file_name(id))).expect("the index reads");
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

    fn segment(&self, id: i64, base: i64) -> PathBuf {
        self.0.join(segment_file_name(id, base))
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn recording(stream_id: i32, start: i64) -> Recording<'static> {
    Recording {
        recording_id: 0,
        start_timestamp: NOW,
        start_position: start,
        initial_term_id: 3,
        term_length: TERM,
        mtu_length: 1408,
        session_id: 7,
        stream_id,
        channel: "cavalry:ipc",
        source_identity: "cavalry:ipc",
    }
}

/// A block of one data frame of 64 bytes at `position`, as the scan hands it.
fn data_block(found: &Descriptor, position: i64, fill: u8) -> [u8; 64] {
    let located = locate(found, position).expect("a position in the recording");
    let header = DataFrameHeader::new(FrameType::Data, Flags::UNFRAGMENTED, 64)
        .expect("a header")
        .with_term_offset(located.term_offset)
        .with_term_id(located.term_id)
        .with_session_id(found.session_id())
        .with_stream_id(found.stream_id())
        .to_bytes();
    let mut block = [fill; 64];
    block[..32].copy_from_slice(&header);
    block
}

/// A padding block at `position` filling the rest of its term: the header alone, with
/// the padding's aligned length as the block's length.
fn padding_block(found: &Descriptor, position: i64) -> ([u8; 32], usize) {
    let located = locate(found, position).expect("a position in the recording");
    let length = (found.term_length() - located.term_offset) as usize;
    let header = DataFrameHeader::new(FrameType::Pad, Flags::NONE, length)
        .expect("a header")
        .with_term_offset(located.term_offset)
        .with_term_id(located.term_id)
        .with_session_id(found.session_id())
        .with_stream_id(found.stream_id())
        .to_bytes();
    (header, length)
}

fn read_at(path: &Path, offset: i64, length: usize) -> Vec<u8> {
    let mut bytes = vec![0u8; length];
    fs::File::open(path)
        .expect("the file opens")
        .read_exact_at(&mut bytes, offset as u64)
        .expect("the bytes read");
    bytes
}

#[test]
fn a_block_the_writer_cannot_take_is_refused_and_nothing_moves() {
    let dir = TestDir::new("refused");
    let (mut log, _) = LogDir::open(dir.path(), NOW).expect("opens");
    let mut writer = log.start(&recording(1001, 0), NOW).expect("starts");
    let found = writer.descriptor().clone();
    let block = data_block(&found, 0, 0xAA);
    let before = read_at(&dir.segment(0, 0), 0, 64);
    let cases: [(&[u8], usize, &str); 5] = [
        (&block, 0, "is empty"),
        (&block, 65, "is not a multiple of 32"),
        (&block[..33], 64, "is not a multiple of 32"),
        (&block, 32, "has more bytes than its length"),
        (
            &block[..32],
            SEGMENT as usize + 32,
            "would cross the end of its segment file",
        ),
    ];
    for (bytes, length, reason) in cases {
        let error = writer.write(bytes, length, NOW).unwrap_err();
        assert!(
            matches!(&error, RecorderError::Block { reason: r, position: 0, .. } if *r == reason),
            "{error}"
        );
        assert_eq!(error.reason(), "block");
        assert!(error.to_string().contains(reason), "{error}");
    }
    assert_eq!(writer.position(), 0, "nothing moved");
    assert_eq!(
        read_at(&dir.segment(0, 0), 0, 64),
        before,
        "nothing written"
    );
    assert_eq!(dir.index_entries(0), vec![(NOW, 0)]);
    // A recording whose channel is past its field is refused before any slot exists.
    let long = "c".repeat(257);
    let error = log
        .start(
            &Recording {
                channel: &long,
                ..recording(1002, 0)
            },
            NOW,
        )
        .unwrap_err();
    assert_eq!(error.reason(), "string", "{error}");
    assert_eq!(log.catalog().slots(), 1, "nothing was written for it");
    let _ = writer;
}

#[test]
fn blocks_roll_across_the_segment_end_and_read_back_by_position() {
    let dir = TestDir::new("roll");
    let (mut log, _) = LogDir::open(dir.path(), NOW).expect("opens");
    let mut writer = log.start(&recording(1001, 0), NOW).expect("starts");
    let found = writer.descriptor().clone();
    assert_eq!(
        fs::metadata(dir.segment(0, 0)).expect("stat").len(),
        SEGMENT as u64,
        "the first segment is created at its full length"
    );
    // One data frame, then padding to the end of every term up to the segment's end.
    let first = data_block(&found, 0, 0x11);
    writer.write(&first, 64, NOW).expect("writes");
    let mut now = NOW;
    while writer.position() < SEGMENT {
        let (header, length) = padding_block(&found, writer.position());
        now += MS;
        writer.write(&header, length, now).expect("writes padding");
    }
    assert_eq!(writer.position(), SEGMENT);
    assert!(
        dir.segment(0, SEGMENT).exists(),
        "rolled into the next segment at the boundary"
    );
    let second = data_block(&found, SEGMENT, 0x22);
    writer
        .write(&second, 64, now + MS)
        .expect("writes in the next segment");
    assert_eq!(writer.position(), SEGMENT + 64);
    assert_eq!(writer.synced_position(), 0, "nothing synced yet");
    let stopped = log.stop(writer, now + 2 * MS).expect("stops");
    assert_eq!(stopped.state(), State::Valid);
    assert_eq!(stopped.stop_position(), SEGMENT + 64);
    assert_eq!(stopped.stop_timestamp(), now + 2 * MS);

    let at_zero = locate(&stopped, 0).expect("locates");
    assert_eq!(
        read_at(
            &dir.path().join(&at_zero.segment_file),
            at_zero.offset_in_segment,
            64
        ),
        first
    );
    let at_second = locate(&stopped, SEGMENT).expect("locates");
    assert_eq!(
        (at_second.segment_base_position, at_second.offset_in_segment),
        (SEGMENT, 0)
    );
    assert_eq!(
        read_at(&dir.path().join(&at_second.segment_file), 0, 64),
        second
    );
    let padding_at = locate(&stopped, 64).expect("locates");
    let header = read_at(&dir.segment(0, 0), padding_at.offset_in_segment, 32);
    assert_eq!(
        &header[..4],
        &(TERM - 64).to_le_bytes(),
        "a padding frame's length"
    );
    assert_eq!(
        dir.segment(0, 0).metadata().expect("stat").len(),
        SEGMENT as u64
    );
    let entries = dir.index_entries(0);
    assert_eq!(entries.first(), Some(&(NOW, 0)));
    assert_eq!(
        entries.last(),
        Some(&(now + 2 * MS, SEGMENT + 64)),
        "one entry at the stop"
    );
    assert_eq!(
        CatalogReader::open(dir.path())
            .expect("opens")
            .get(0)
            .expect("reads")
            .expect("holds"),
        stopped
    );
}

#[test]
fn the_index_gets_one_entry_per_millisecond_and_never_a_timestamp_before_the_last() {
    let dir = TestDir::new("index-cadence");
    let (mut log, _) = LogDir::open(dir.path(), NOW).expect("opens");
    let mut writer = log.start(&recording(1001, 0), NOW).expect("starts");
    let found = writer.descriptor().clone();
    let mut expected = vec![(NOW, 0)];
    let mut last = NOW;
    for n in 0..8 {
        let now = NOW + (n + 1) * 300_000; // 0.3 ms apart
        let block = data_block(&found, writer.position(), n as u8);
        writer.write(&block, 64, now).expect("writes");
        if now - last >= INDEX_INTERVAL {
            last = now;
            expected.push((now, writer.position()));
        }
    }
    assert_eq!(
        dir.index_entries(0),
        expected,
        "entries at most once per millisecond"
    );
    assert_eq!(expected.len(), 3, "the start entry and two more in 2.4 ms");
    // The clock steps back: the next entry carries the last timestamp, never an earlier one.
    let block = data_block(&found, writer.position(), 9);
    writer.write(&block, 64, NOW - 5 * MS).expect("writes");
    let stopped = log.stop(writer, NOW - 5 * MS).expect("stops");
    let entries = dir.index_entries(0);
    assert_eq!(entries.last(), Some(&(last, stopped.stop_position())));
    assert_eq!(
        stopped.stop_timestamp(),
        last,
        "the stop carries the stop entry's timestamp, never one before the last entry's"
    );
}

#[test]
fn sync_runs_once_per_second_while_bytes_are_unsynced_and_the_synced_position_follows() {
    let dir = TestDir::new("sync-cadence");
    let (mut log, _) = LogDir::open(dir.path(), NOW).expect("opens");
    let mut writer = log.start(&recording(1001, 0), NOW).expect("starts");
    let found = writer.descriptor().clone();
    assert!(
        !writer.sync(NOW + SYNC_INTERVAL).expect("syncs"),
        "nothing unsynced"
    );
    writer
        .write(&data_block(&found, 0, 1), 64, NOW)
        .expect("writes");
    assert!(
        !writer.sync(NOW + SYNC_INTERVAL - 1).expect("syncs"),
        "not a second yet"
    );
    assert_eq!(writer.synced_position(), 0);
    assert!(
        writer.sync(NOW + SYNC_INTERVAL).expect("syncs"),
        "a second passed"
    );
    assert_eq!(writer.synced_position(), 64);
    assert!(
        !writer.sync(NOW + 2 * SYNC_INTERVAL).expect("syncs"),
        "nothing unsynced"
    );
    writer
        .write(&data_block(&found, 64, 2), 64, NOW + SYNC_INTERVAL)
        .expect("writes");
    assert!(
        !writer.sync(NOW + 2 * SYNC_INTERVAL - 1).expect("syncs"),
        "a second since the last sync, not since the write"
    );
    assert!(writer.sync(NOW + 2 * SYNC_INTERVAL).expect("syncs"));
    assert_eq!((writer.position(), writer.synced_position()), (128, 128));
    let _ = log.stop(writer, NOW + 3 * SYNC_INTERVAL).expect("stops");
}

#[test]
fn a_stop_at_a_segment_boundary_removes_the_empty_next_segment() {
    let dir = TestDir::new("stop-at-boundary");
    let (mut log, _) = LogDir::open(dir.path(), NOW).expect("opens");
    let mut writer = log.start(&recording(1001, 0), NOW).expect("starts");
    let found = writer.descriptor().clone();
    let mut now = NOW;
    while writer.position() < SEGMENT {
        let (header, length) = padding_block(&found, writer.position());
        now += MS;
        writer.write(&header, length, now).expect("writes padding");
    }
    assert!(dir.segment(0, SEGMENT).exists());
    let stopped = log.stop(writer, now + MS).expect("stops");
    assert_eq!(stopped.stop_position(), SEGMENT);
    assert!(
        dir.segment(0, 0).exists(),
        "the segment holding the recording stays"
    );
    assert!(
        !dir.segment(0, SEGMENT).exists(),
        "a segment wholly past the stop is removed"
    );
}

#[test]
fn a_stopped_recording_needs_no_repair_at_the_next_open_and_the_next_id_follows() {
    let dir = TestDir::new("reopen");
    {
        let (mut log, _) = LogDir::open(dir.path(), NOW).expect("opens");
        let mut writer = log.start(&recording(1001, 128), NOW).expect("starts");
        let found = writer.descriptor().clone();
        writer
            .write(&data_block(&found, 128, 5), 64, NOW)
            .expect("writes");
        let stopped = log.stop(writer, NOW + MS).expect("stops");
        assert_eq!(stopped.stop_position(), 192);
    }
    let (mut log, recovery) = LogDir::open(dir.path(), NOW + 2 * MS).expect("opens again");
    assert_eq!(
        recovery,
        Recovery::default(),
        "a cleanly stopped recording is left alone"
    );
    let writer = log
        .start(&recording(1002, 0), NOW + 2 * MS)
        .expect("starts");
    assert_eq!(writer.recording_id(), 1);
    assert!(dir.segment(1, 0).exists());
    let _: &RecordingWriter = &writer;
    let _ = log.stop(writer, NOW + 3 * MS).expect("stops");
    assert_eq!(
        CatalogReader::open(dir.path())
            .expect("opens")
            .slots()
            .expect("slots"),
        2
    );
}

#[test]
fn every_error_names_the_page_that_explains_it() {
    let errors = [
        RecorderError::Block {
            position: 64,
            length: 65,
            bytes: 64,
            reason: "is not a multiple of 32",
        },
        RecorderError::Io {
            what: "write a block of",
            path: PathBuf::from("/tmp/log/0-0.rec"),
            source: std::io::Error::other("refused"),
        },
        RecorderError::Catalog(CatalogError::Full),
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
