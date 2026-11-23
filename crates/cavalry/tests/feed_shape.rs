//! What Brigade's server runs on an embedded driver, held as a test (work item 362;
//! ADR 0138, condition 9; invariants 1 and 3). The feed is `tests/feed/mod.rs`, the
//! server's own use of Cavalry call for call; these tests run it in both of its
//! shapes, the pinned one of Brigade's server 0.9.0 and the one its 0.10.0 moved to:
//!
//! - an exclusive publication in manual control mode created at a position, by
//!   `init-term-id`, `term-id` and `term-offset`, in a term past the first;
//! - a subscription to its own feed added as a destination, where the shape needs one;
//! - a recording for each run whose positions continue from the last, and a sync
//!   before each answer;
//! - a start that stops a recording an earlier client left live;
//! - a replay of the earlier recordings onto `cavalry:ipc`, from the start and from a
//!   position inside one;
//! - a roll, and a purge of what a snapshot covers, made again when a replay held it
//!   off;
//! - a replay across recordings of two term lengths, which is the server's move from
//!   the one shape to the other on a directory it keeps;
//! - a recording that ends in a term's padding, read to its end in either order in
//!   which the driver lets the reader learn that its replay ended (Brigade's work item
//!   375).
//!
//! The stall that ends in a reconnect under an append, and the kill, need a process of
//! their own to stop: `tests/feed_held_still.rs`. A test that fails here names a call that
//! Brigade's server makes: `docs/explanation/what-brigades-server-relies-on.md` says
//! what to do before that call changes.

use std::path::{Path, PathBuf};

use cavalry::{Cavalry, NULL_LENGTH};
use cavalry_log::catalog::CatalogReader;
use cavalry_log::layout::State;

mod feed;
use feed::{
    FeedLog, FeedOptions, MOVED_TO, PINNED, REPLAY_STREAM, Recorded, Shape, as_the_replay_says,
    record, recordings, told_after_the_image_went,
};

// This test's ports, from the one file that says how they are taken and why, which is
// `cavalry-raft`'s: `crates/cavalry-raft/tests/ports/mod.rs`.
#[path = "../../cavalry-raft/tests/ports/mod.rs"]
mod ports;
use ports::free_ports;

/// A directory of its own per test, removed when the guard drops.
struct TestDir(PathBuf);

impl TestDir {
    fn new(test: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("cavalry-feed-shape-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("the test directory");
        TestDir(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// What a feed of `shape` under `dir` is opened with: two loopback ports of its own,
/// the same at every start, as the server's addresses are.
fn options(dir: &TestDir, shape: Shape, roll_bytes: u64) -> FeedOptions {
    let ports = free_ports(2);
    FeedOptions {
        driver_dir: dir.path().join("driver"),
        log_dir: dir.path().join("log"),
        control: format!("127.0.0.1:{}", ports[0]),
        own_endpoint: format!("127.0.0.1:{}", ports[1]),
        roll_bytes,
        shape,
    }
}

/// What the test appended, in order: each record and the position after it.
type Appended = Vec<(Vec<u8>, i64)>;

/// Appends records of `lengths` as one batch, numbered on from what `appended` holds,
/// and adds each with the position the feed answered for it.
fn append(log: &mut FeedLog, appended: &mut Appended, lengths: &[usize]) {
    let first = appended.len() as u64;
    let records: Vec<Vec<u8>> = lengths
        .iter()
        .enumerate()
        .map(|(i, length)| record(first + i as u64, *length))
        .collect();
    let before = log.end();
    let positions = log.append(&records).expect("the feed takes the batch");
    assert_eq!(
        positions.len(),
        records.len(),
        "a position for every record"
    );
    let mut last = before;
    for position in &positions {
        assert!(
            *position > last && position % 32 == 0,
            "the position after a record is past the one before it and a multiple of 32: \
             {position} after {last}"
        );
        last = *position;
    }
    assert_eq!(
        log.end(),
        last,
        "the feed ends after the batch's last record"
    );
    appended.extend(records.into_iter().zip(positions));
}

/// Appends batches until the feed's end is past `position`.
fn append_past(log: &mut FeedLog, appended: &mut Appended, position: i64) {
    let length = log.max_record_bytes().min(1 << 18);
    while log.end() <= position {
        append(log, appended, &[length; 8]);
    }
}

/// Reads the feed back from `from` to where this run began.
fn read(log: &mut FeedLog, from: i64) -> Appended {
    let mut found = Appended::new();
    let begin = log
        .read_from(from, &mut |record, after| {
            found.push((record.to_vec(), after))
        })
        .expect("the feed reads back");
    assert_eq!(begin, log.begin());
    found
}

/// Fails unless `found` is `expected`, and says where the two part without printing a
/// megabyte of record.
fn same(found: &Appended, expected: &[(Vec<u8>, i64)], what: &str) {
    for (number, (found, expected)) in found.iter().zip(expected).enumerate() {
        assert!(
            found.1 == expected.1 && found.0 == expected.0,
            "{what}: record {number} read back is {} bytes ending at position {}, and the one \
             appended there was {} bytes ending at position {}{}",
            found.0.len(),
            found.1,
            expected.0.len(),
            expected.1,
            if found.0 == expected.0 {
                ""
            } else {
                ", with other bytes"
            }
        );
    }
    assert_eq!(
        found.len(),
        expected.len(),
        "{what}: as many records read back as were appended there"
    );
}

/// The lengths of a first batch: a byte, the three lengths around a frame's alignment,
/// one frame's worth, a record of several frames, and the longest the feed takes.
fn first_batch(log: &FeedLog) -> Vec<usize> {
    vec![1, 31, 32, 33, 1_000, 5_000, 70_000, log.max_record_bytes()]
}

/// The full case: three runs of the feed on one pair of directories are one line of
/// positions, each run a recording that begins where the last stopped, the second in a
/// term past the first, and a later run reads the earlier recordings back in order,
/// from the start, from a recording's start and from a position inside one.
fn runs_are_one_line_of_positions(shape: Shape, name: &str) {
    let dir = TestDir::new(name);
    let options = options(&dir, shape, u64::MAX);
    let term = i64::try_from(shape.term_length).expect("a term length");
    let mut appended = Appended::new();

    // The first run: a new directory, so the feed begins at position 0.
    let mut log = FeedLog::open(&options).expect("the first run opens");
    assert_eq!((log.begin(), log.end()), (0, 0));
    assert_eq!(
        log.channel(),
        Some(
            format!(
                "cavalry:udp?control={}|control-mode=manual{}|init-term-id=0|term-id=0|term-offset=0",
                options.control, shape.channel_keys
            )
            .as_str()
        )
    );
    assert_eq!(
        recordings(&options.log_dir).expect("the catalog"),
        [Recorded {
            id: 0,
            start: 0,
            stop: None,
            term_length: shape.term_length,
        }],
        "the run's recording is live in the catalog"
    );
    let lengths = first_batch(&log);
    append(&mut log, &mut appended, &lengths);
    assert_eq!(appended[0].1, 64, "a record of one byte is one frame of 64");
    append_past(&mut log, &mut appended, term);
    let first_stop = log.end();
    let first_run = appended.len();
    drop(log);
    assert_eq!(
        recordings(&options.log_dir).expect("the catalog"),
        [Recorded {
            id: 0,
            start: 0,
            stop: Some(first_stop),
            term_length: shape.term_length,
        }],
        "the recording stopped at the last record answered"
    );

    // The second run begins where the first stopped, in the second term.
    let mut log = FeedLog::open(&options).expect("the second run opens");
    assert_eq!((log.begin(), log.end()), (first_stop, first_stop));
    assert!(
        first_stop / term >= 1,
        "the first run ended past its first term"
    );
    assert_eq!(
        log.channel(),
        Some(
            format!(
                "cavalry:udp?control={}|control-mode=manual{}|init-term-id=0|term-id={}|term-offset={}",
                options.control,
                shape.channel_keys,
                first_stop / term,
                first_stop % term
            )
            .as_str()
        )
    );
    assert_eq!(log.recording_id(), Some(1));
    same(
        &read(&mut log, 0),
        &appended,
        "the second run, from the start",
    );
    append(&mut log, &mut appended, &[7, 4_096, 100_000]);
    assert!(
        appended[first_run].1 > first_stop,
        "the second run's positions go on from the first's"
    );
    let second_stop = log.end();
    drop(log);
    assert_eq!(
        recordings(&options.log_dir).expect("the catalog"),
        [
            Recorded {
                id: 0,
                start: 0,
                stop: Some(first_stop),
                term_length: shape.term_length,
            },
            Recorded {
                id: 1,
                start: first_stop,
                stop: Some(second_stop),
                term_length: shape.term_length,
            }
        ]
    );

    // The third run reads both recordings back as one feed.
    let mut log = FeedLog::open(&options).expect("the third run opens");
    assert_eq!(log.begin(), second_stop);
    same(
        &read(&mut log, 0),
        &appended,
        "the third run, from the start",
    );
    same(
        &read(&mut log, first_stop),
        &appended[first_run..],
        "the third run, from the second recording's start",
    );
    // From the position after a record inside the first recording, which is where a
    // snapshot stands: the rest of that recording, then the next.
    let inside = appended[2].1;
    same(
        &read(&mut log, inside),
        &appended[3..],
        "the third run, from a position inside the first recording",
    );
    same(
        &read(&mut log, second_stop),
        &[],
        "the third run, from its own start",
    );
    let past = log.read_from(second_stop + 32, &mut |_, _| {});
    assert!(
        past.as_ref()
            .is_err_and(|error| error.contains("past the feed's end")),
        "a position past the feed is refused: {past:?}"
    );
}

#[test]
fn runs_of_the_pinned_feed_are_one_line_of_positions() {
    runs_are_one_line_of_positions(PINNED, "line-pinned");
}

#[test]
fn runs_of_the_feed_the_server_moves_to_are_one_line_of_positions() {
    runs_are_one_line_of_positions(MOVED_TO, "line-moved-to");
}

/// A start after an earlier client went without stopping its recording begins the
/// feed where that recording stops, and its own recording is of its own publication.
/// The earlier client has gone from the driver as a client that the driver timed out
/// has: the server's reconnect after a stall, without the stall. The start finds the
/// earlier recording live and stops it, or finds that the recorder stopped it at the
/// publication's end of stream a moment before; the test holds what is the same in
/// both.
fn a_start_stops_a_recording_an_earlier_client_left_live(shape: Shape, name: &str) {
    let dir = TestDir::new(name);
    let options = options(&dir, shape, u64::MAX);
    let mut appended = Appended::new();

    let mut log = FeedLog::open(&options).expect("the feed opens");
    let lengths = first_batch(&log);
    append(&mut log, &mut appended, &lengths);
    let left_at = log.end();

    let (left, stopped_at) = log
        .start_again_after_the_client_went()
        .expect("a start after the client went");
    assert_eq!(
        left,
        [Recorded {
            id: 0,
            start: 0,
            stop: None,
            term_length: shape.term_length,
        }],
        "the earlier client leaves its recording live in the catalog"
    );
    assert_eq!(
        stopped_at, left_at,
        "the earlier recording stopped at the last record answered: every append waited \
         for the recorder"
    );
    assert_eq!(
        recordings(&options.log_dir).expect("the catalog"),
        [
            Recorded {
                id: 0,
                start: 0,
                stop: Some(left_at),
                term_length: shape.term_length,
            },
            Recorded {
                id: 1,
                start: left_at,
                stop: None,
                term_length: shape.term_length,
            }
        ],
        "the earlier recording is stopped, and the feed's new one begins at its stop"
    );
    let (publication, recorded) = log.sessions().expect("the feed is open");
    assert_eq!(
        publication, recorded,
        "the new recording is of the new client's publication"
    );
    let before = appended.len();
    append(&mut log, &mut appended, &[3, 2_000, 50_000]);
    assert!(appended[before].1 > left_at);
    let end = log.end();
    drop(log);

    let mut log = FeedLog::open(&options).expect("the next run opens");
    assert_eq!(log.begin(), end);
    same(
        &read(&mut log, 0),
        &appended,
        "after the start that stopped the earlier recording",
    );
}

#[test]
fn a_start_of_the_pinned_feed_stops_a_recording_an_earlier_client_left_live() {
    a_start_stops_a_recording_an_earlier_client_left_live(PINNED, "left-live-pinned");
}

#[test]
fn a_start_of_the_feed_the_server_moves_to_stops_a_recording_an_earlier_client_left_live() {
    a_start_stops_a_recording_an_earlier_client_left_live(MOVED_TO, "left-live-moved-to");
}

/// What does not hold, held as the rule it follows from, so that a change to the rule
/// is made knowingly (work item 372). `start recording` takes the live publication
/// with the lowest registration id that is not yet being recorded, whoever sends the
/// command (`spec/client-protocol.md`, "Commands"). So a start beside an earlier
/// client that the driver still holds, one that closed on its own side and has not
/// been timed out yet, stops that client's recording and then records that client's
/// publication again, not its own: the feed's own publication takes records that no
/// recorder reaches, and the server's next append fails after its 15 s.
///
/// The pinned shape alone: there the feed connects through its own subscription, so
/// the start returns and the two sessions can be read. In the other shape the same
/// rule leaves the feed unconnected, and the start itself waits out its 15 s.
///
/// When this test fails, the rule has changed: turn the assertion around, add the
/// other shape, and take the case out of "What does not hold" in
/// `docs/explanation/what-brigades-server-relies-on.md`.
#[test]
fn a_start_beside_an_earlier_client_the_driver_still_holds_records_that_clients_publication() {
    let dir = TestDir::new("beside-pinned");
    let options = options(&dir, PINNED, u64::MAX);
    let mut appended = Appended::new();

    let mut log = FeedLog::open(&options).expect("the feed opens");
    append(&mut log, &mut appended, &[1, 2_000]);
    let left_at = log.end();
    let (earlier, _) = log.sessions().expect("the feed is open");

    let stopped_at = log
        .start_again_beside_the_earlier_client()
        .expect("a start beside the earlier client");
    assert_eq!(
        stopped_at, left_at,
        "the earlier recording is stopped where it was"
    );
    let (publication, recorded) = log.sessions().expect("the feed is open");
    assert_ne!(
        publication, earlier,
        "the new client's publication is a session of its own"
    );
    assert_eq!(
        recorded, earlier,
        "the rule of spec/client-protocol.md, \"start recording\", as it stands: the lowest \
         registration id not yet being recorded is the earlier client's publication. If the \
         recording is now of the new client's own publication, work item 372's change is in: \
         see this test's comment"
    );
}

/// The state of catalog slot `id` in `log_dir`.
fn slot_state(log_dir: &Path, id: i64) -> Option<State> {
    let catalog = CatalogReader::open(log_dir).expect("the catalog opens");
    catalog
        .get(id)
        .expect("the slot reads")
        .map(|descriptor| descriptor.state())
}

/// The files of recording `id` in `log_dir`, by name: its segment files,
/// `<id>-<base>.rec`, and its time index, `<id>.idx` (`spec/durable-log.md`).
fn files_of(log_dir: &Path, id: i64) -> Vec<String> {
    let (prefix, index) = (format!("{id}-"), format!("{id}.idx"));
    let mut names: Vec<String> = std::fs::read_dir(log_dir)
        .expect("the log directory lists")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(&prefix) || *name == index)
        .collect();
    names.sort();
    names
}

/// A release rolls the feed with no restart and purges the stopped recordings that a
/// snapshot covers, but for the newest, which the next start reads its position from.
/// What is purged is gone, and a read from before it says so.
fn a_release_rolls_the_feed_and_purges_what_a_snapshot_covers(shape: Shape, name: &str) {
    let dir = TestDir::new(name);
    // A roll at every release that has anything to roll, as the server's own test sets.
    let options = options(&dir, shape, 1);
    let mut appended = Appended::new();
    let mut log = FeedLog::open(&options).expect("the feed opens");

    // A snapshot at `a`. The feed rolls there; recording 0 is the newest stopped one,
    // so nothing is purged.
    let lengths = first_batch(&log);
    append(&mut log, &mut appended, &lengths);
    let a = log.end();
    log.release(a).expect("the first release");
    assert_eq!(
        log.notes(),
        [format!("the feed's recording was rolled at position {a}")]
    );
    assert_eq!(
        recordings(&options.log_dir).expect("the catalog"),
        [
            Recorded {
                id: 0,
                start: 0,
                stop: Some(a),
                term_length: shape.term_length,
            },
            Recorded {
                id: 1,
                start: a,
                stop: None,
                term_length: shape.term_length,
            }
        ]
    );
    assert_eq!(
        (log.begin(), log.end()),
        (a, a),
        "the feed goes on where it rolled"
    );
    assert!(
        !files_of(&options.log_dir, 0).is_empty(),
        "recording 0 has its files"
    );

    // More records, and the snapshot still at `a`: the feed rolls at `b`, and
    // recording 0, which the snapshot covers, is purged.
    append(&mut log, &mut appended, &[9, 3_000, 80_000]);
    let b = log.end();
    let at_b = appended.len();
    log.release(a).expect("the second release");
    assert_eq!(
        log.notes(),
        [
            format!("the feed's recording was rolled at position {b}"),
            format!(
                "let go of 1 recordings of the feed, which the snapshots cover up to position {a}"
            )
        ]
    );
    assert_eq!(
        recordings(&options.log_dir).expect("the catalog"),
        [
            Recorded {
                id: 1,
                start: a,
                stop: Some(b),
                term_length: shape.term_length,
            },
            Recorded {
                id: 2,
                start: b,
                stop: None,
                term_length: shape.term_length,
            }
        ],
        "recording 0 is no recording of the feed any more"
    );
    assert_eq!(
        slot_state(&options.log_dir, 0),
        Some(State::Deleted),
        "its slot stays, deleted, so that its id is never used again"
    );

    // More records and no later snapshot: the feed rolls at `c`, and recording 1,
    // which ends past the snapshot, is kept.
    append(&mut log, &mut appended, &[11, 60_000]);
    let c = log.end();
    log.release(a).expect("the third release");
    assert_eq!(
        log.notes(),
        [format!("the feed's recording was rolled at position {c}")]
    );
    assert_eq!(slot_state(&options.log_dir, 1), Some(State::Valid));

    // A snapshot at `c` and nothing appended since: nothing to roll, and recording 1
    // is purged. Recording 2, the newest stopped, stays.
    log.release(c).expect("the fourth release");
    assert_eq!(
        log.notes(),
        [format!(
            "let go of 1 recordings of the feed, which the snapshots cover up to position {c}"
        )]
    );
    assert_eq!(
        recordings(&options.log_dir).expect("the catalog"),
        [
            Recorded {
                id: 2,
                start: b,
                stop: Some(c),
                term_length: shape.term_length,
            },
            Recorded {
                id: 3,
                start: c,
                stop: None,
                term_length: shape.term_length,
            }
        ]
    );
    append(&mut log, &mut appended, &[13]);
    let end = log.end();
    drop(log);

    // The next start reads its position from what is left, and reads back from the
    // snapshot. What was purged is gone, and a read from before it is refused.
    let mut log = FeedLog::open(&options).expect("the next run opens");
    assert_eq!(log.begin(), end);
    same(
        &read(&mut log, c),
        &appended[at_b + 2..],
        "from the snapshot at c",
    );
    same(
        &read(&mut log, b),
        &appended[at_b..],
        "from the oldest recording kept",
    );
    for gone in [0, a] {
        let refused = log.read_from(gone, &mut |_, _| {});
        assert_eq!(
            refused,
            Err(format!(
                "the feed no longer holds position {gone}: its earliest recording after it \
                 begins at {b}"
            ))
        );
    }
    // The purged recordings' files are removed one per duty cycle: gone by now.
    for id in [0, 1] {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !files_of(&options.log_dir, id).is_empty() {
            assert!(
                std::time::Instant::now() < deadline,
                "the files of purged recording {id} are removed: {:?}",
                files_of(&options.log_dir, id)
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
}

#[test]
fn a_release_rolls_the_pinned_feed_and_purges_what_a_snapshot_covers() {
    a_release_rolls_the_feed_and_purges_what_a_snapshot_covers(PINNED, "release-pinned");
}

#[test]
fn a_release_rolls_the_feed_the_server_moves_to_and_purges_what_a_snapshot_covers() {
    a_release_rolls_the_feed_and_purges_what_a_snapshot_covers(MOVED_TO, "release-moved-to");
}

/// A purge is refused while a replay reads the recording, and the server sends it
/// again at its next release. Another client of the same driver holds the replay. Its
/// subscriber reads nothing, and the recording is longer than a term, which is further
/// than a replay's publication goes ahead of its slowest subscriber: the replay stands
/// short of the recording's end until it is stopped, however long the release takes.
fn a_purge_refused_under_a_replay_is_made_at_the_next_release(shape: Shape, name: &str) {
    let dir = TestDir::new(name);
    let options = options(&dir, shape, 1);
    let term = i64::try_from(shape.term_length).expect("a term length");
    let mut appended = Appended::new();
    let mut log = FeedLog::open(&options).expect("the feed opens");

    append(&mut log, &mut appended, &[1, 2_000]);
    append_past(&mut log, &mut appended, term);
    let a = log.end();
    log.release(a).expect("the first release");
    append(&mut log, &mut appended, &[2, 3_000]);
    let b = log.end();
    let _ = log.notes();

    let mut reader = Cavalry::connect_to(&options.driver_dir).expect("a second client");
    let unread = reader
        .add_subscription("cavalry:ipc", REPLAY_STREAM + 1)
        .expect("a subscription that reads nothing");
    let replay = reader
        .start_replay(0, 0, NULL_LENGTH, REPLAY_STREAM + 1, "cavalry:ipc")
        .expect("a replay of recording 0");
    log.release(a).expect("the release under the replay");
    let notes = log.notes();
    assert_eq!(notes.len(), 2, "a roll and a refusal: {notes:?}");
    assert_eq!(
        notes[0],
        format!("the feed's recording was rolled at position {b}")
    );
    assert!(
        notes[1].starts_with("recording 0 of the feed was not let go of: "),
        "the purge of a recording that a replay reads is refused: {notes:?}"
    );
    assert_eq!(slot_state(&options.log_dir, 0), Some(State::Valid));
    assert_eq!(
        replay.ended(),
        None,
        "the replay is live: its subscriber has read nothing"
    );

    reader
        .stop_replay(replay.replay_session_id())
        .expect("the replay stops");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while replay.ended().is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "the replay ends once it is stopped"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    log.release(a).expect("the release after the replay");
    assert_eq!(
        log.notes(),
        [format!(
            "let go of 1 recordings of the feed, which the snapshots cover up to position {a}"
        )]
    );
    assert_eq!(slot_state(&options.log_dir, 0), Some(State::Deleted));
    unread.close().expect("the subscription closes");
    reader.close().expect("the second client closes");
}

#[test]
fn a_purge_of_the_pinned_feed_refused_under_a_replay_is_made_at_the_next_release() {
    a_purge_refused_under_a_replay_is_made_at_the_next_release(PINNED, "busy-pinned");
}

#[test]
fn a_purge_of_the_feed_the_server_moves_to_refused_under_a_replay_is_made_at_the_next_release() {
    a_purge_refused_under_a_replay_is_made_at_the_next_release(MOVED_TO, "busy-moved-to");
}

/// The server's move from the pinned shape to the other, on the directories it keeps:
/// a run in terms of 16 MiB with its own subscription, then runs in terms of 1 MiB
/// connected by the recording. The feed is one line of positions across the move, and
/// one read crosses recordings of the two term lengths.
#[test]
fn a_replay_crosses_recordings_of_two_term_lengths() {
    let dir = TestDir::new("two-term-lengths");
    let pinned = options(&dir, PINNED, u64::MAX);
    let moved_to = FeedOptions {
        shape: MOVED_TO,
        ..pinned.clone()
    };
    let short = i64::try_from(MOVED_TO.term_length).expect("a term length");
    let mut appended = Appended::new();

    // In terms of 16 MiB, to a position past the first megabyte, so that the next
    // run's publication is created in a term past its first.
    let mut log = FeedLog::open(&pinned).expect("the pinned run opens");
    assert_eq!(log.longest_term(), 0, "a new directory holds no recording");
    let lengths = first_batch(&log);
    append(&mut log, &mut appended, &lengths);
    append_past(&mut log, &mut appended, short + short / 2);
    let moved_at = log.end();
    let before_the_move = appended.len();
    drop(log);

    // The same directories, in terms of 1 MiB.
    let mut log = FeedLog::open(&moved_to).expect("the first run after the move opens");
    assert_eq!(log.begin(), moved_at);
    assert_eq!(
        log.channel(),
        Some(
            format!(
                "cavalry:udp?control={}|control-mode=manual|ssc=true|term-length=1m|init-term-id=0|term-id={}|term-offset={}",
                moved_to.control,
                moved_at / short,
                moved_at % short
            )
            .as_str()
        )
    );
    assert!(moved_at / short >= 1);
    same(
        &read(&mut log, 0),
        &appended,
        "the run after the move, from the start",
    );
    // Past the end of a term of the new length, so that the recording holds the
    // padding at a term's end.
    append(&mut log, &mut appended, &[5, 9_000]);
    let next_term = (log.end() / short + 1) * short;
    append_past(&mut log, &mut appended, next_term);
    let end = log.end();
    drop(log);

    let mut log = FeedLog::open(&moved_to).expect("the second run after the move opens");
    assert_eq!(log.begin(), end);
    assert_eq!(
        log.longest_term(),
        PINNED.term_length,
        "the catalog, read before the driver started, holds a recording of 16 MiB terms"
    );
    assert_eq!(
        recordings(&moved_to.log_dir)
            .expect("the catalog")
            .iter()
            .map(|recorded| recorded.term_length)
            .collect::<Vec<_>>(),
        [
            PINNED.term_length,
            MOVED_TO.term_length,
            MOVED_TO.term_length
        ],
        "the catalog says each recording's term length, from before the move and after it"
    );
    same(
        &read(&mut log, 0),
        &appended,
        "across the two term lengths, from the start",
    );
    let inside = appended[before_the_move - 2].1;
    same(
        &read(&mut log, inside),
        &appended[before_the_move - 1..],
        "across the two term lengths, from inside the recording of 16 MiB terms",
    );
}

/// A recording that a stop left ending in a term's padding is read to its end,
/// whichever of the driver's two messages reaches the reader first (Brigade's work
/// item 375). The driver withdraws a replay's image once it has been read to the end
/// of stream, and `replay stopped` can reach the client after that. The padding
/// reaches no handler, so only the image's position says that the end was read: the
/// reader keeps the furthest position the image stood at. Brigade's reader before its
/// fix failed 9 of 12 starts on such a recording asked as the server asks, as the
/// driver's threads fell, and fails every read told in the second order. The test asks
/// in that order first, then as the server asks.
fn a_recording_that_ends_in_padding_is_read_to_its_end(shape: Shape, name: &str) {
    let dir = TestDir::new(name);
    let options = options(&dir, shape, u64::MAX);
    let term = i64::try_from(shape.term_length).expect("a term length");

    let mut log = FeedLog::open(&options).expect("the feed opens");
    let appended = log
        .append_up_to_a_terms_padding(0)
        .expect("the feed writes a term's padding");
    let last = appended.last().expect("records before the padding").1;
    assert!(
        last < term,
        "the last record ends before the term's end, at {last}"
    );
    // The feed's clean stop, in the moment after the padding.
    drop(log);
    assert_eq!(
        recordings(&options.log_dir).expect("the catalog"),
        [Recorded {
            id: 0,
            start: 0,
            stop: Some(term),
            term_length: shape.term_length,
        }],
        "a stop after the padding leaves the recording stopped at the term's end"
    );

    let mut log = FeedLog::open(&options).expect("the feed opens again");
    assert_eq!(log.begin(), term);
    log.learn_a_replays_end_by(told_after_the_image_went);
    same(
        &read(&mut log, 0),
        &appended,
        "told that the replay ended after its image went",
    );
    log.learn_a_replays_end_by(as_the_replay_says);
    same(&read(&mut log, 0), &appended, "told as the server is told");
}

#[test]
fn a_recording_of_the_pinned_feed_that_ends_in_padding_is_read_to_its_end() {
    a_recording_that_ends_in_padding_is_read_to_its_end(PINNED, "padding-pinned");
}

#[test]
fn a_recording_of_the_feed_the_server_moves_to_that_ends_in_padding_is_read_to_its_end() {
    a_recording_that_ends_in_padding_is_read_to_its_end(MOVED_TO, "padding-moved-to");
}
