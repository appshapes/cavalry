//! The allocation gate over a purge's file work (CLAUDE.md, invariant 2 and gate 2;
//! card 177): a driver whose log directory holds a stopped recording of 8,192 sparse
//! segment files, a client that purges them all with one `purge segments`, and the
//! counting allocator the bench uses over the window from the first file's removal to
//! the last's but one, with no allocation at all. The replayer lists a command's files
//! once, when it is admitted, and formats each file's name on its stack after that;
//! `recording.segments_purged` moves one per file as it goes, which is how the window
//! is placed. Alone in its binary, so that no other test's threads are counted
//! (`cargo test` runs a binary's tests in parallel).
use std::alloc::System;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cavalry_client::Cavalry;
use cavalry_driver::Driver;
use cavalry_log::catalog::CatalogWriter;
use cavalry_log::dir::LogDir;
use cavalry_log::layout::{
    IndexHeader, Recording, SEGMENT_FILE_LENGTH, index_file_name, segment_file_name,
};
use cavalry_shm::counters::CountersReader;
use cavalry_shm::counters::generated::RECORDING_SEGMENTS_PURGED;
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

/// The pile: enough files that the window between the first removal and the last's
/// but one is tens of milliseconds, so the counter is read inside it many times.
const SEGMENTS: u64 = 8_192;
/// How long any wait in this test may last before it fails with a message.
const DEADLINE: Duration = Duration::from_secs(30);

/// The driver's directory and, beside it, its log directory; both go with the test.
struct Dirs {
    dir: PathBuf,
    log_dir: PathBuf,
}

impl Dirs {
    fn new() -> Self {
        let dir =
            std::env::temp_dir().join(format!("cavalry-purge-allocations-{}", std::process::id()));
        let log_dir = dir.with_extension("log");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&log_dir);
        Self { dir, log_dir }
    }
}

impl Drop for Dirs {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
        let _ = std::fs::remove_dir_all(&self.log_dir);
    }
}

/// A stopped recording of `segments` segment files in `log_dir`, built before the
/// driver opens it, as the bench's `--purging` builds its pile: the slot added and
/// stopped at the end of the last file, the index header, and each file created at the
/// segment length and never written, so it is sparse. Returns the position past every
/// file, what `purge segments` is asked to retain from.
fn build_pile(log_dir: &Path, segments: u64) -> i64 {
    let now = 1_760_000_000_000_000_000;
    let (log, _) = LogDir::open(log_dir, now).expect("the log directory");
    drop(log);
    let mut catalog = CatalogWriter::open(log_dir, now).expect("the catalog");
    let recording_id = catalog
        .add(&Recording {
            recording_id: 0,
            start_timestamp: now,
            start_position: 0,
            initial_term_id: 0,
            term_length: 64 * 1024,
            mtu_length: 1408,
            session_id: 0,
            stream_id: 1001,
            channel: "cavalry:ipc",
            source_identity: "cavalry:ipc",
        })
        .expect("the pile's slot");
    assert_eq!(recording_id, 0);
    let segment_length = i64::from(SEGMENT_FILE_LENGTH);
    let stop = segment_length * segments as i64;
    catalog
        .stop(recording_id, stop, now)
        .expect("the pile's stop");
    drop(catalog);
    let header = IndexHeader::new(recording_id).expect("the index header");
    std::fs::write(log_dir.join(index_file_name(recording_id)), header.encode())
        .expect("the index");
    for k in 0..segments {
        let path = log_dir.join(segment_file_name(recording_id, segment_length * k as i64));
        std::fs::File::create(&path)
            .and_then(|file| file.set_len(segment_length as u64))
            .expect("a segment file of the pile");
    }
    stop
}

#[test]
fn a_purges_file_work_allocates_nothing_per_file() {
    let dirs = Dirs::new();
    let stop = build_pile(&dirs.log_dir, SEGMENTS);
    let driver = Driver::start(&dirs.dir, &dirs.log_dir).expect("the driver");
    let dir = dirs.dir.clone();
    let purger = std::thread::spawn(move || {
        let mut client = Cavalry::connect_to(&dir).expect("the purger's client");
        client.purge_segments(0, stop).expect("the purge");
        client.close().expect("the purger's close");
    });
    let reader = CountersReader::open(&dirs.dir).expect("the counters file");
    let purged = || {
        reader
            .read_acquire(RECORDING_SEGMENTS_PURGED.record())
            .expect("recording.segments_purged") as u64
    };
    // The listing of the pile's files, which allocates, is behind the first removal.
    let started = Instant::now();
    while purged() == 0 {
        assert!(
            started.elapsed() < DEADLINE,
            "no file of the pile was removed in {DEADLINE:?}"
        );
        std::hint::spin_loop();
    }
    let region = Region::new(GLOBAL);
    let first_seen = purged();
    assert!(
        first_seen < SEGMENTS - 1,
        "the window opened with {first_seen} of {SEGMENTS} files already gone, too few \
         removals left to count under; make the pile larger"
    );
    while purged() < SEGMENTS - 1 {
        assert!(
            started.elapsed() < DEADLINE,
            "{} of {SEGMENTS} files removed in {DEADLINE:?}",
            purged()
        );
        std::hint::spin_loop();
    }
    // The window closes before the last file, whose removal ends the command and sends
    // the reply through the conductor and the client, which is not the file work.
    let allocations = region.change().allocations;
    purger.join().expect("the purger thread");
    assert_eq!(purged(), SEGMENTS, "every file of the pile went");
    driver.close().expect("the driver's close");
    assert_eq!(
        allocations, 0,
        "the file work allocated {allocations} times between the first file's removal \
         and the last's but one; see .claude/rules/data-plane.md"
    );
}
