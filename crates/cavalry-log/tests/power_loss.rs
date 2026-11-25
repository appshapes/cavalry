//! A recording that a crash left live, recovered after its unsynced tail was cut in
//! the ways a power loss can cut it (`spec/durable-log.md`, "Time index" and "The
//! driver"; ADR 0144; CLAUDE.md, invariant 1). The recorder writes a recording, syncs
//! part of it and writes more; the writer is dropped without a stop, as a crash leaves
//! it; the test then damages the bytes past the synced position as a power loss can:
//! a frame whose header survived and whose body did not, a page lost between pages
//! that survived, a tail lost whole, index entries that point past the data, and a
//! sync whose slot the crash tore. Each time the driver's recovery stops the recording
//! at the position the recorder had synced, zeroes what lay past it, counts the cut,
//! and every frame read back from the start to the stop is one the recorder wrote,
//! whole. The full cases come first; a crash that left nothing past the sync, and a
//! crash before the first sync, follow. Then the two cases where the unsynced tail
//! crosses a roll into the next segment file: the synced position in the middle of
//! the segment before the roll, and the synced position at the next segment's base
//! (work item 405). A segment file is 128 MiB, and those tests cross one by padding
//! frames, whose bytes the recorder writes as their 32-byte header alone, one header
//! per 64 KiB term: each test writes under 150 KiB. On a file system that keeps a file
//! sparse, as APFS and ext4 do, each header still takes a whole block of the file
//! system, 4 KiB, so a test's two segment files take about 8 MiB of the disk while
//! it runs, against the 256 MiB of their lengths.

use std::fs::{self, OpenOptions};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use cavalry_log::catalog::CatalogReader;
use cavalry_log::dir::{LogDir, Recovery};
use cavalry_log::layout::{
    Descriptor, HEADER_LENGTH, IndexHeader, Recording, SEGMENT_FILE_LENGTH, SYNCED_SLOT_OFFSETS,
    State, index_file_name, locate, segment_file_name,
};
use cavalry_log::recorder::RecordingWriter;
use cavalry_proto::frame::{DataFrameHeader, Flags, FrameType};

const NOW: i64 = 1_760_000_000_000_000_000;
const MS: i64 = 1_000_000;
const TERM: i32 = 65536;
const SESSION: i32 = 7;
const STREAM: i32 = 1001;
const FRAME: usize = 64;
/// Frames in one block the recorder writes: one 4 KiB page.
const PER_BLOCK: usize = 64;
const BLOCK: usize = FRAME * PER_BLOCK;
/// The length of a segment file, and so the base of the second.
const SEGMENT: i64 = SEGMENT_FILE_LENGTH as i64;

/// A directory of its own per test, removed when the guard drops.
struct TestDir(PathBuf);

impl TestDir {
    fn new(test: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("cavalry-power-loss-{}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        TestDir(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn segment(&self, found: &Descriptor, position: i64) -> (PathBuf, u64) {
        let located = locate(found, position).expect("a position of the recording");
        (
            self.0.join(&located.segment_file),
            located.offset_in_segment as u64,
        )
    }

    /// Writes `bytes` at `position` of the recording's segment, as the disk kept them.
    fn put(&self, found: &Descriptor, position: i64, bytes: &[u8]) {
        let (path, offset) = self.segment(found, position);
        OpenOptions::new()
            .write(true)
            .open(path)
            .expect("the segment opens")
            .write_all_at(bytes, offset)
            .expect("the bytes are written");
    }

    fn read(&self, found: &Descriptor, position: i64, length: usize) -> Vec<u8> {
        let (path, offset) = self.segment(found, position);
        let mut bytes = vec![0u8; length];
        fs::File::open(path)
            .expect("the segment opens")
            .read_exact_at(&mut bytes, offset)
            .expect("the bytes are read");
        bytes
    }

    fn index(&self, id: i64) -> PathBuf {
        self.0.join(index_file_name(id))
    }

    fn descriptor(&self, id: i64) -> Descriptor {
        CatalogReader::open(self.path())
            .expect("the catalog opens")
            .get(id)
            .expect("the slot reads")
            .expect("it holds the recording")
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The body byte of the frame at `position`: each frame's own, so a frame read back
/// says which one it is.
fn fill(position: i64) -> u8 {
    ((position / FRAME as i64) % 251) as u8 + 1
}

/// The frame the recorder writes at `position`: a data frame of the recording's
/// session and stream, its body every byte `fill(position)`.
fn frame(found: &Descriptor, position: i64) -> [u8; FRAME] {
    let located = locate(found, position).expect("a position of the recording");
    let header = DataFrameHeader::new(FrameType::Data, Flags::UNFRAGMENTED, FRAME)
        .expect("a header")
        .with_term_offset(located.term_offset)
        .with_term_id(located.term_id)
        .with_session_id(found.session_id())
        .with_stream_id(found.stream_id())
        .to_bytes();
    let mut bytes = [fill(position); FRAME];
    bytes[..32].copy_from_slice(&header);
    bytes
}

/// Recording 0 started at position 0 in the directory `log` holds.
fn started(log: &mut LogDir) -> RecordingWriter {
    log.start(
        &Recording {
            recording_id: 0,
            start_timestamp: NOW,
            start_position: 0,
            initial_term_id: 3,
            term_length: TERM,
            mtu_length: 1408,
            session_id: SESSION,
            stream_id: STREAM,
            channel: "cavalry:ipc",
            source_identity: "cavalry:ipc",
        },
        NOW,
    )
    .expect("the recording starts")
}

/// A recording started at position 0, written `synced` blocks and synced, then
/// written `unsynced` blocks more, and dropped without a stop, as a crash leaves it:
/// the directory, the descriptor, the synced position and the position written to.
fn crashed(test: &str, synced: usize, unsynced: usize) -> (TestDir, Descriptor, i64, i64) {
    let dir = TestDir::new(test);
    let (mut log, _) = LogDir::open(dir.path(), NOW).expect("the directory opens");
    let mut writer = started(&mut log);
    let found = writer.descriptor().clone();
    let mut now = NOW;
    write_blocks(&mut writer, &found, synced, &mut now);
    if synced > 0 {
        assert!(writer.sync_now(now).expect("the sync"), "there was a sync");
    }
    let synced_position = writer.synced_position();
    write_blocks(&mut writer, &found, unsynced, &mut now);
    let written = writer.position();
    assert_eq!(
        writer.synced_position(),
        synced_position,
        "nothing synced since"
    );
    // The crash: no stop, the slot reads live.
    drop(writer);
    drop(log);
    (dir, found, synced_position, written)
}

/// Writes `blocks` blocks of frames, each 2 ms after the one before: more than the
/// index interval, so the recorder appends an index entry for each.
fn write_blocks(writer: &mut RecordingWriter, found: &Descriptor, blocks: usize, now: &mut i64) {
    for _ in 0..blocks {
        *now += 2 * MS;
        writer
            .write(&block(found, writer.position()), BLOCK, *now)
            .expect("the block is written");
    }
}

/// The block of frames the recorder writes at `position`: one page of them.
fn block(found: &Descriptor, mut position: i64) -> Vec<u8> {
    let mut block = Vec::with_capacity(BLOCK);
    for _ in 0..PER_BLOCK {
        block.extend_from_slice(&frame(found, position));
        position += FRAME as i64;
    }
    block
}

/// The position of the last entry in the recording's time index.
fn last_indexed(dir: &TestDir, found: &Descriptor) -> i64 {
    let entries = fs::read(dir.index(found.recording_id())).expect("the index reads");
    let last = entries[HEADER_LENGTH..]
        .as_chunks::<16>()
        .0
        .last()
        .expect("the index holds an entry");
    i64::from_le_bytes(last[8..16].try_into().expect("8"))
}

/// Writes padding frames from the writer's position to `to`, each to the end of its
/// term at most, as the recorder writes a padding block: the 32-byte header alone,
/// the position moving by the frame's length. The time does not move, so no index
/// entry is appended for them.
fn padded_to(writer: &mut RecordingWriter, found: &Descriptor, to: i64, now: i64) {
    assert!(
        to >= writer.position() && to % 32 == 0,
        "padding goes forward to a frame's alignment"
    );
    while writer.position() < to {
        let position = writer.position();
        let left_in_term = i64::from(TERM) - position % i64::from(TERM);
        let length = left_in_term.min(to - position) as usize;
        writer
            .write(&pad(found, position, length), length, now)
            .expect("the padding is written");
    }
    assert_eq!(writer.position(), to);
}

/// The header of a padding frame of `length` bytes at `position`.
fn pad(found: &Descriptor, position: i64, length: usize) -> [u8; 32] {
    let located = locate(found, position).expect("a position of the recording");
    DataFrameHeader::new(FrameType::Pad, Flags::UNFRAGMENTED, length)
        .expect("a header")
        .with_term_offset(located.term_offset)
        .with_term_id(located.term_id)
        .with_session_id(found.session_id())
        .with_stream_id(found.stream_id())
        .to_bytes()
}

/// Holds that the whole-term padding frames from position 0 to `to`, a term's start,
/// are as the recorder wrote them: recovery left the segment below the cut alone.
fn padding_kept(dir: &TestDir, found: &Descriptor, to: i64) {
    let mut position = 0;
    while position < to {
        assert_eq!(
            dir.read(found, position, 32),
            pad(found, position, TERM as usize),
            "the padding frame at {position} is kept"
        );
        position += i64::from(TERM);
    }
}

/// Opens the directory again, as the next driver's start does, and holds what a
/// reader then finds: the recording stopped at `stop`, every frame from the start
/// to it the one the recorder wrote there, and only zeros past it in its segment.
fn recovered(dir: &TestDir, found: &Descriptor, stop: i64) -> Recovery {
    recovered_from(dir, found, 0, stop)
}

/// [`recovered`], with the frames the recorder wrote whole checked from `from`: below
/// it lie padding frames, whose bodies the recorder never writes.
fn recovered_from(dir: &TestDir, found: &Descriptor, from: i64, stop: i64) -> Recovery {
    let (_log, recovery) = LogDir::open(dir.path(), NOW + 10_000 * MS).expect("it opens again");
    let stopped = dir.descriptor(found.recording_id());
    assert_eq!(stopped.state(), State::Valid, "the recording is no damage");
    assert_eq!(
        stopped.stop_position(),
        stop,
        "it stops where it had synced"
    );
    let mut position = from;
    while position < stop {
        assert_eq!(
            dir.read(found, position, FRAME),
            frame(found, position),
            "the frame at {position} is whole"
        );
        position += FRAME as i64;
    }
    // A segment that begins at the stop is removed: nothing is past the stop then.
    if dir.segment(found, stop).0.exists() {
        let past = dir.read(found, stop, 4 * BLOCK);
        assert!(
            past.iter().all(|&byte| byte == 0),
            "nothing past the stop: a later scan finds no frame there"
        );
    }
    recovery
}

/// The recovery a cut makes: one recording recovered and cut, its index's entries
/// past the stop dropped.
const CUT: Recovery = Recovery {
    truncated: false,
    dropped_empty_slot: false,
    recovered: 1,
    invalidated: 0,
    index_rewritten: 1,
    files_removed: 0,
    cut_to_synced: 1,
};

/// A frame whose header survived and whose body did not: the case no check of the
/// frame can see, since the header carries every field the `frame` check reads.
#[test]
fn a_frame_whose_body_a_power_loss_took_past_the_sync_is_cut() {
    let (dir, found, synced, written) = crashed("body-lost", 4, 3);
    assert!(written > synced);
    dir.put(&found, synced + 32, &[0u8; 32]);
    assert_eq!(recovered(&dir, &found, synced), CUT);
}

/// A page lost between pages that survived, which writeback in any order leaves:
/// the frames after the hole are whole and still past the sync, and go with it.
#[test]
fn a_page_lost_between_pages_that_survived_is_cut_with_every_page_after_it() {
    let (dir, found, synced, written) = crashed("page-lost", 4, 3);
    assert_eq!(written, synced + 3 * BLOCK as i64);
    dir.put(&found, synced + BLOCK as i64, &[0u8; BLOCK]);
    assert_eq!(recovered(&dir, &found, synced), CUT);
}

/// The unsynced tail whole, as a crash of the driver's process alone leaves it: no
/// start can tell it from a power loss, so it is cut the same way.
#[test]
fn a_whole_tail_past_the_sync_is_cut_since_no_start_tells_the_crashes_apart() {
    let (dir, found, synced, _) = crashed("tail-whole", 4, 3);
    assert_eq!(recovered(&dir, &found, synced), CUT);
}

/// The tail lost whole, and index entries that point past what the segment kept,
/// since the two files are synced apart: the entries say the recorder wrote past the
/// sync, so the cut is counted, and the entries past the stop go.
#[test]
fn a_tail_lost_whole_is_counted_as_cut_by_the_index_entries_past_the_sync() {
    let (dir, found, synced, written) = crashed("tail-lost", 4, 3);
    dir.put(&found, synced, &vec![0u8; (written - synced) as usize]);
    assert_eq!(recovered(&dir, &found, synced), CUT);
}

/// The first page past the sync lost and the pages after it kept, whole frames among
/// them: the scan stops at the sync, and every byte past it is zeroed all the same, so
/// that no later scan finds those frames; the index entries past the sync count the
/// cut.
#[test]
fn the_first_page_past_the_sync_lost_and_later_ones_kept_are_all_zeroed() {
    let (dir, found, synced, written) = crashed("first-page-lost", 4, 3);
    assert_eq!(written, synced + 3 * BLOCK as i64);
    dir.put(&found, synced, &[0u8; BLOCK]);
    assert_eq!(
        dir.read(&found, synced + BLOCK as i64, FRAME),
        frame(&found, synced + BLOCK as i64),
        "a whole frame lies past the lost page"
    );
    assert_eq!(recovered(&dir, &found, synced), CUT);
}

/// Nothing says the recorder wrote past the sync: the tail and the index entries past
/// it are both lost. The stop is the synced position, and no cut is counted, since
/// none can be told from the files.
#[test]
fn a_tail_lost_with_its_index_entries_stops_at_the_sync_and_counts_no_cut() {
    let (dir, found, synced, written) = crashed("tail-and-index-lost", 4, 3);
    dir.put(&found, synced, &vec![0u8; (written - synced) as usize]);
    let index = dir.index(found.recording_id());
    let entries = fs::read(&index).expect("the index reads");
    let kept = entries[HEADER_LENGTH..]
        .as_chunks::<16>()
        .0
        .iter()
        .take_while(|entry| i64::from_le_bytes(entry[8..16].try_into().expect("8")) <= synced)
        .count();
    OpenOptions::new()
        .write(true)
        .open(&index)
        .expect("the index opens")
        .set_len((HEADER_LENGTH + kept * 16) as u64)
        .expect("the entries past the sync are lost");
    assert_eq!(
        recovered(&dir, &found, synced),
        Recovery {
            recovered: 1,
            ..Recovery::default()
        }
    );
}

/// A synced slot whose checksum matches and whose position no writer writes, 16 bytes
/// off a frame's alignment: damage, which no crash leaves. The recording gets state 2,
/// as for every damage the files cannot repair, and the index is left as it is.
#[test]
fn a_synced_slot_that_fails_its_check_is_damage() {
    let (dir, found, synced, _) = crashed("slot-damaged", 2, 1);
    let index = dir.index(found.recording_id());
    let before = fs::read(&index).expect("the index reads");
    let damaged = IndexHeader::synced_slot(synced + 16);
    OpenOptions::new()
        .write(true)
        .open(&index)
        .expect("the index opens")
        .write_all_at(&damaged, SYNCED_SLOT_OFFSETS[0] as u64)
        .expect("the slot is written");
    let (_log, recovery) = LogDir::open(dir.path(), NOW + 10_000 * MS).expect("it opens again");
    assert_eq!(
        recovery,
        Recovery {
            invalidated: 1,
            ..Recovery::default()
        }
    );
    assert_eq!(
        CatalogReader::open(dir.path())
            .expect("the catalog opens")
            .get(found.recording_id())
            .map(|_| ())
            .map_err(|error| error.reason()),
        Err("invalid")
    );
    let after = fs::read(&index).expect("the index reads");
    assert_eq!(after.len(), before.len(), "the index is left as it is");
}

/// A sync whose slot the crash tore reads as the sync before it: the recording is cut
/// to the earlier synced position, below what the torn write would have named.
#[test]
fn a_sync_whose_slot_a_crash_tore_cuts_to_the_sync_before_it() {
    let (dir, found, first, _) = crashed("slot-torn", 2, 0);
    // A second sync, written and torn: slot 0 holds the position, its checksum does
    // not match, and slot 1 holds the first sync's.
    let index = OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.index(found.recording_id()))
        .expect("the index opens");
    let mut torn = IndexHeader::synced_slot(first + 2 * BLOCK as i64);
    torn[8] ^= 0xFF;
    index
        .write_all_at(&torn, SYNCED_SLOT_OFFSETS[0] as u64)
        .expect("the torn slot");
    let mut header = [0u8; HEADER_LENGTH];
    index.read_exact_at(&mut header, 0).expect("the header");
    let length = index.metadata().expect("stat").len();
    assert_eq!(
        IndexHeader::parse(&header, length)
            .expect("a header")
            .synced_position(),
        Some(first),
        "the torn slot is not read"
    );
    drop(index);
    // What the second sync covered, written and whole.
    let mut position = first;
    for _ in 0..2 * PER_BLOCK {
        dir.put(&found, position, &frame(&found, position));
        position += FRAME as i64;
    }
    // The index holds no entry past the first sync: none is dropped.
    assert_eq!(
        recovered(&dir, &found, first),
        Recovery {
            index_rewritten: 0,
            ..CUT
        }
    );
}

/// A crash right after a sync, nothing written past it: the stop is the synced
/// position and nothing is cut.
#[test]
fn a_crash_with_nothing_past_the_sync_stops_there_and_cuts_nothing() {
    let (dir, found, synced, written) = crashed("nothing-past", 4, 0);
    assert_eq!(written, synced);
    assert_eq!(
        recovered(&dir, &found, synced),
        Recovery {
            recovered: 1,
            ..Recovery::default()
        }
    );
}

/// A crash before the first sync: the synced position is the start position, the
/// recording is empty, and a frame at the start whose body was lost is no damage, as
/// it would be under a synced position past it.
#[test]
fn a_crash_before_the_first_sync_leaves_an_empty_recording_not_damage() {
    let (dir, found, synced, written) = crashed("no-sync", 0, 3);
    assert_eq!((synced, written), (0, 3 * BLOCK as i64));
    dir.put(&found, 32, &[0u8; 32]);
    let recovery = recovered(&dir, &found, 0);
    assert_eq!(
        recovery,
        Recovery {
            files_removed: 1,
            ..CUT
        },
        "the segment begins at the stop and goes"
    );
    assert_eq!(
        dir.descriptor(found.recording_id()).stop_timestamp(),
        NOW,
        "an empty recording's stop timestamp is its start timestamp"
    );
}

/// The synced position in the middle of a segment, and the recorder written on to the
/// end of it, rolled into the next, and written there before the crash: the roll
/// synced the full segment's data and not the synced position, which stays where the
/// last sync left it. The bytes past it in its own segment are zeroed, all the way to
/// the segment's end, and the next segment, which lies wholly past the stop, is
/// removed.
#[test]
fn a_tail_that_rolled_into_the_next_segment_is_cut_in_its_segment_and_the_next_is_removed() {
    let dir = TestDir::new("rolled-past-the-sync");
    let (mut log, _) = LogDir::open(dir.path(), NOW).expect("the directory opens");
    let mut writer = started(&mut log);
    let found = writer.descriptor().clone();
    let mut now = NOW;
    let middle = SEGMENT / 2;
    padded_to(&mut writer, &found, middle, now);
    write_blocks(&mut writer, &found, 4, &mut now);
    assert!(writer.sync_now(now).expect("the sync"), "there was a sync");
    let synced = writer.synced_position();
    assert_eq!(synced, middle + 4 * BLOCK as i64);
    write_blocks(&mut writer, &found, 3, &mut now);
    padded_to(&mut writer, &found, SEGMENT, now);
    write_blocks(&mut writer, &found, 3, &mut now);
    assert_eq!(writer.position(), SEGMENT + 3 * BLOCK as i64);
    assert_eq!(
        writer.synced_position(),
        synced,
        "the roll moved no synced position"
    );
    let next = dir
        .path()
        .join(segment_file_name(found.recording_id(), SEGMENT));
    assert!(next.exists(), "the recorder rolled into the next segment");
    let last_term = SEGMENT - i64::from(TERM);
    assert_ne!(
        dir.read(&found, last_term, FRAME),
        [0u8; FRAME],
        "a padding frame begins the segment's last term"
    );
    // The crash: no stop, the slot reads live.
    drop(writer);
    drop(log);
    assert_eq!(
        recovered_from(&dir, &found, middle, synced),
        Recovery {
            files_removed: 1,
            ..CUT
        }
    );
    padding_kept(&dir, &found, middle);
    // Every padding frame past the stop, at each term's start to the segment's end.
    let mut position = middle + i64::from(TERM);
    while position < SEGMENT {
        assert_eq!(
            dir.read(&found, position, 32),
            [0u8; 32],
            "the bytes past the stop are zeroed to the segment's end, at {position}"
        );
        position += i64::from(TERM);
    }
    assert!(
        !next.exists(),
        "the next segment lay past the stop and is gone"
    );
}

/// The synced position at the next segment's base: the recorder filled the segment,
/// rolled, synced, and wrote into the next before the crash. Nothing in the full
/// segment lies past the stop, so it is left whole, and the next segment, which
/// begins at the stop, is removed. The tail is written within the millisecond after
/// the sync, so the recorder appended no index entry for it, and the frame at the
/// next segment's base alone says the recorder wrote past the sync: it counts the
/// cut, and with the next segment gone no file holds it afterwards.
#[test]
fn a_tail_past_a_sync_at_the_next_segments_base_removes_that_segment_and_keeps_the_full_one() {
    let dir = TestDir::new("synced-at-the-roll");
    let (mut log, _) = LogDir::open(dir.path(), NOW).expect("the directory opens");
    let mut writer = started(&mut log);
    let found = writer.descriptor().clone();
    let mut now = NOW;
    let last_term = SEGMENT - i64::from(TERM);
    padded_to(&mut writer, &found, last_term, now);
    write_blocks(&mut writer, &found, TERM as usize / BLOCK, &mut now);
    assert_eq!(writer.position(), SEGMENT, "the segment is full");
    let next = dir
        .path()
        .join(segment_file_name(found.recording_id(), SEGMENT));
    assert!(next.exists(), "the recorder rolled into the next segment");
    assert!(writer.sync_now(now).expect("the sync"), "there was a sync");
    assert_eq!(
        writer.synced_position(),
        SEGMENT,
        "synced at the next segment's base"
    );
    let within_the_interval = now + MS / 2;
    for _ in 0..3 {
        writer
            .write(
                &block(&found, writer.position()),
                BLOCK,
                within_the_interval,
            )
            .expect("the block is written");
    }
    assert_eq!(writer.position(), SEGMENT + 3 * BLOCK as i64);
    // The crash: no stop, the slot reads live.
    drop(writer);
    drop(log);
    assert_eq!(
        last_indexed(&dir, &found),
        SEGMENT,
        "no index entry lies past the sync"
    );
    assert_eq!(
        recovered_from(&dir, &found, last_term, SEGMENT),
        Recovery {
            files_removed: 1,
            index_rewritten: 0,
            ..CUT
        }
    );
    padding_kept(&dir, &found, last_term);
    assert!(
        !next.exists(),
        "the next segment began at the stop and is gone"
    );
}

/// The recorder's side of the synced position: the start position in slot 0 at the
/// creation, then each sync in the other slot, the higher of the two read back.
#[test]
fn each_sync_writes_the_other_slot_and_the_higher_is_the_synced_position() {
    let dir = TestDir::new("slots");
    let (mut log, _) = LogDir::open(dir.path(), NOW).expect("the directory opens");
    let mut writer = log
        .start(
            &Recording {
                recording_id: 0,
                start_timestamp: NOW,
                start_position: 4096,
                initial_term_id: 3,
                term_length: TERM,
                mtu_length: 1408,
                session_id: SESSION,
                stream_id: STREAM,
                channel: "cavalry:ipc",
                source_identity: "cavalry:ipc",
            },
            NOW,
        )
        .expect("the recording starts");
    let found = writer.descriptor().clone();
    let slots = |dir: &TestDir| {
        let bytes = fs::read(dir.index(found.recording_id())).expect("the index reads");
        let slot = |offset: usize| {
            let position = i64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("8"));
            let checks = bytes[offset..offset + 12] == IndexHeader::synced_slot(position);
            checks.then_some(position)
        };
        let synced = IndexHeader::parse(&bytes, bytes.len() as u64)
            .expect("a header")
            .synced_position();
        (
            slot(SYNCED_SLOT_OFFSETS[0]),
            slot(SYNCED_SLOT_OFFSETS[1]),
            synced,
        )
    };
    assert_eq!(
        slots(&dir),
        (Some(4096), None, Some(4096)),
        "at the creation"
    );
    let mut now = NOW;
    write_blocks(&mut writer, &found, 1, &mut now);
    writer.sync_now(now).expect("the first sync");
    let first = writer.position();
    assert_eq!(slots(&dir), (Some(4096), Some(first), Some(first)));
    write_blocks(&mut writer, &found, 1, &mut now);
    writer.sync_now(now).expect("the second sync");
    let second = writer.position();
    assert_eq!(slots(&dir), (Some(second), Some(first), Some(second)));
    let stopped = log.stop(writer, now + MS).expect("the stop");
    assert_eq!(stopped.stop_position(), second);
    assert_eq!(slots(&dir).2, Some(second), "the stop syncs nothing more");
    assert!(
        SEGMENT_FILE_LENGTH as i64 > second,
        "the recording stays in its first segment"
    );
}
