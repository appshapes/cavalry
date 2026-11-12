//! A service's read of a member's log across the member's recordings, through the
//! client API against a real driver in this process (`spec/cluster.md`, "The service"
//! and "The log's recordings"; ADR 0103). The member is played by a client of the same
//! driver: two recordings it makes, the two counters a service finds it by, and the
//! file of its log's recordings. The consensus module's own side, which writes that
//! file before its counter names a recording, is `cavalry-raft`'s to test, over three
//! drivers.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cavalry_client::{Cavalry, ClientError, CommittedLog, Counter, Publication, Recording};
use cavalry_driver::Driver;
use cavalry_proto::cluster::{
    LOG_RECORDINGS_FILE_PREFIX, LogRecording, MAX_LOG_RECORDINGS, MAX_LOG_RECORDINGS_LENGTH,
    write_log_recordings,
};
use cavalry_shm::counters::generated::{
    CLUSTER_COMMIT_POSITION, CLUSTER_LOG_RECORDING_ID, REPLAY_STARTED,
};

/// A directory of its own per test, removed when the guard drops.
struct TestDir(PathBuf);

impl TestDir {
    fn new(test: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "cavalry-client-committed-log-{}-{test}",
            std::process::id()
        ));
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

/// Waits up to ten seconds for `done`.
fn wait(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// The member's id.
const MEMBER: i32 = 3;
/// The length on the stream of each entry the member appends: a frame header and 32
/// bytes.
const ENTRY: i64 = 64;

/// One recording the member makes of a stream of its own, with its publication.
struct Recorded {
    publication: Publication,
    recording: Recording,
    /// The position after the last entry appended.
    end: i64,
}

impl Recorded {
    /// Appends `count` entries, each numbered from where the stream is, and waits for
    /// the recording to have synced them.
    fn append(&mut self, count: i64) {
        for _ in 0..count {
            let mut entry = [0u8; 32];
            entry[..8].copy_from_slice(&(self.end / ENTRY).to_le_bytes());
            entry[8..12].copy_from_slice(&self.publication.stream_id().to_le_bytes());
            let mut after = self.end;
            wait("the offer goes through", || {
                match self.publication.offer(&entry) {
                    Ok(position) => {
                        after = position;
                        true
                    }
                    Err(_) => false,
                }
            });
            self.end = after;
        }
        wait("the recording is synced", || {
            self.recording.synced_position() == self.end
        });
    }
}

/// A cluster member as a service sees one: a driver, the member's client, its two
/// counters and its driver directory.
struct Member {
    client: Cavalry,
    log_recording_id: Counter,
    commit_position: Counter,
    dir: PathBuf,
    driver: Driver,
    _test: TestDir,
}

impl Member {
    /// A driver and a member on it that names no recording yet: its recording's id
    /// counter added first and holding −1, then its commit position.
    fn start(test: &str) -> Self {
        let test = TestDir::new(test);
        let dir = test.path().join("driver");
        let driver = Driver::start(&dir, test.path().join("log")).expect("the driver starts");
        let mut client = Cavalry::connect_to(&dir).expect("the member's client connects");
        let key = MEMBER.to_le_bytes();
        let log_recording_id = client
            .add_counter(
                CLUSTER_LOG_RECORDING_ID.0,
                &key,
                &format!("cluster.log_recording_id {MEMBER}"),
            )
            .expect("the recording's id counter");
        log_recording_id.store_release(-1).expect("stores");
        let commit_position = client
            .add_counter(
                CLUSTER_COMMIT_POSITION.0,
                &key,
                &format!("cluster.commit_position {MEMBER}"),
            )
            .expect("the commit position counter");
        Member {
            client,
            log_recording_id,
            commit_position,
            dir,
            driver,
            _test: test,
        }
    }

    /// A recording of `stream`, a stream of the member's own, holding `count` entries.
    fn record(&mut self, stream: i32, count: i64) -> Recorded {
        let publication = self
            .client
            .add_exclusive_publication("cavalry:ipc", stream)
            .expect("a publication");
        let recording = self
            .client
            .start_recording("cavalry:ipc", stream)
            .expect("a recording");
        let mut recorded = Recorded {
            publication,
            recording,
            end: 0,
        };
        recorded.append(count);
        recorded
    }

    /// Stops `recorded` and returns its id and the position it stopped at.
    fn stop(&mut self, recorded: Recorded) -> (i64, i64) {
        let id = recorded.recording.recording_id();
        self.client.stop_recording(id).expect("the recording stops");
        recorded
            .publication
            .close()
            .expect("the publication closes");
        (id, recorded.end)
    }

    /// The path of the member's file of its log's recordings.
    fn file(&self) -> PathBuf {
        self.dir
            .join(format!("{LOG_RECORDINGS_FILE_PREFIX}{MEMBER}"))
    }

    /// Writes the member's file: the live recording and where it begins, and the
    /// recordings below it, each as its id, its start and its stop.
    fn write_file(&self, live: i64, live_start: i64, below: &[(i64, i64, i64)]) {
        let mut bytes = vec![0; MAX_LOG_RECORDINGS_LENGTH];
        let length = write_log_recordings(
            &mut bytes,
            MEMBER,
            live,
            live_start,
            below.iter().map(|&(id, start, stop)| LogRecording {
                recording_id: id,
                start_position: start,
                stop_position: stop,
            }),
        )
        .expect("the file is written");
        std::fs::write(self.file(), &bytes[..length]).expect("the file is stored");
    }

    /// Names `live` as the member's live recording, after its file, and stores
    /// `commit` as its commit position.
    fn name(&self, live: i64, live_start: i64, below: &[(i64, i64, i64)], commit: i64) {
        self.write_file(live, live_start, below);
        self.log_recording_id.store_release(live).expect("stores");
        self.commit_position.store_release(commit).expect("stores");
    }

    fn close(self) {
        self.client.close().expect("the member's client closes");
        self.driver.close().expect("the driver closes");
    }
}

/// What a service read: each entry's position and its number on its stream.
type Read = Vec<(i64, i64, i32)>;

/// One poll of `log` into `read`.
fn poll(log: &mut CommittedLog, read: &mut Read) {
    log.poll(
        |body, _, position| {
            let number = i64::from_le_bytes(body[..8].try_into().expect("eight bytes"));
            let stream = i32::from_le_bytes(body[8..12].try_into().expect("four bytes"));
            read.push((position, number, stream));
        },
        64,
    )
    .expect("the poll");
}

/// The log opened at `position`, which the test expects to be there.
fn open(service: &mut Cavalry, position: i64) -> CommittedLog {
    service
        .committed_log(position)
        .expect("the driver answers")
        .expect("the member has its log")
}

/// A member whose log is two recordings, the first stopped at 640 and the live one
/// from there, and a service that starts at 0. It reads the first recording no
/// further than the member's commit position, though the recording holds all of
/// it; reads on when the commit position covers the rest; is ended at the
/// recording's stop with nothing reported; and opens the log again there, on the
/// live recording. Every entry once, in order.
#[test]
fn a_service_reads_a_recording_below_the_live_one_to_its_stop_and_then_the_live_one() {
    let mut member = Member::start("across");
    let first = member.record(71, 10);
    let (first_id, first_end) = member.stop(first);
    assert_eq!(first_end, 10 * ENTRY);
    // The live recording is a stream of its own that began at 0; the member's log
    // takes it from 640, where an entry of it begins.
    let live = member.record(72, 12);
    let live_id = live.recording.recording_id();
    let committed = 4 * ENTRY;
    member.name(live_id, first_end, &[(first_id, 0, first_end)], committed);

    let mut service = Cavalry::connect_to(&member.dir).expect("the service's client connects");
    let mut log = open(&mut service, 0);
    assert_eq!(log.recording_id(), first_id);
    assert_eq!(log.member_id(), MEMBER);
    let mut read = Read::new();
    wait("the service read to the commit position", || {
        poll(&mut log, &mut read);
        log.position() == committed
    });
    // The replay plays on to the recording's stop; the service is given no more.
    let until = Instant::now() + Duration::from_millis(200);
    while Instant::now() < until {
        poll(&mut log, &mut read);
    }
    assert_eq!(read.len(), 4);
    assert_eq!(log.commit_position(), committed);
    assert!(!log.is_ended(), "entries of the recording are still owed");

    // The commit position passes the recording's stop, one entry into the live one.
    member
        .commit_position
        .store_release(first_end + ENTRY)
        .expect("stores");
    wait("the service read the recording to its stop", || {
        poll(&mut log, &mut read);
        log.is_ended()
    });
    assert_eq!(log.position(), first_end);
    assert_eq!(
        (log.failed_at(), log.withdrawn_at(), log.faulted()),
        (None, None, None)
    );
    assert_eq!(read.len(), 10);
    let ended_at = log.position();
    log.close(&mut service).expect("the handle closes");

    let mut log = open(&mut service, ended_at);
    assert_eq!(log.recording_id(), live_id);
    wait("the service read on in the live recording", || {
        poll(&mut log, &mut read);
        log.position() == first_end + ENTRY
    });
    let expected: Read = (0..10)
        .map(|number| (number * ENTRY, number, 71))
        .chain([(first_end, 10, 72)])
        .collect();
    assert_eq!(read, expected);
    assert!(!log.is_ended(), "the live recording is followed");
    log.close(&mut service).expect("the handle closes");
    service.close().expect("the service's client closes");
    member.close();
}

/// The bound of a recording below the live one is the commit position the member
/// stored while it named that live recording, and none it stores after. The member
/// names another recording with a commit position past the first recording's stop,
/// as a member does that cut its log and went on: the handle is ended where its
/// bound was, and delivers nothing of what the replay had played past it.
#[test]
fn a_service_takes_no_commit_position_stored_after_its_member_named_another_recording() {
    let mut member = Member::start("bound");
    let first = member.record(71, 10);
    let (first_id, first_end) = member.stop(first);
    let live = member.record(72, 12);
    let live_id = live.recording.recording_id();
    let committed = 4 * ENTRY;
    member.name(live_id, first_end, &[(first_id, 0, first_end)], committed);

    let mut service = Cavalry::connect_to(&member.dir).expect("the service's client connects");
    let mut log = open(&mut service, 0);
    let mut read = Read::new();
    wait("the service read to the commit position", || {
        poll(&mut log, &mut read);
        log.position() == committed
    });

    // The member leaves its live recording, and names another, with more committed.
    member.log_recording_id.store_release(-1).expect("stores");
    let next = member.record(73, 12);
    member.name(
        next.recording.recording_id(),
        committed,
        &[(first_id, 0, committed)],
        first_end + ENTRY,
    );
    wait("the handle is ended", || {
        poll(&mut log, &mut read);
        log.is_ended()
    });
    assert_eq!(log.position(), committed);
    assert_eq!(log.commit_position(), committed, "the bound it took, kept");
    assert_eq!(read.len(), 4, "nothing past the bound was delivered");
    log.close(&mut service).expect("the handle closes");

    // The log opened again is read by the member's file as it is now.
    let log = open(&mut service, committed);
    assert_eq!(log.recording_id(), next.recording.recording_id());
    log.close(&mut service).expect("the handle closes");
    service.close().expect("the service's client closes");
    member.close();
}

/// A follower in a catch-up: its recording of the replay is still being made and is
/// listed with what it had synced, 640, below a live recording that begins at 320,
/// inside it. A service at 0 follows the replay's recording past the listed stop to
/// where it stops, 768, and opens the log again there, on the live recording.
#[test]
fn a_service_follows_a_recording_still_being_made_to_where_it_stops() {
    let mut member = Member::start("catch-up");
    let mut replayed = member.record(71, 10);
    let replayed_id = replayed.recording.recording_id();
    let live = member.record(72, 14);
    let live_id = live.recording.recording_id();
    member.name(
        live_id,
        5 * ENTRY,
        &[(replayed_id, 0, replayed.end)],
        14 * ENTRY,
    );

    let mut service = Cavalry::connect_to(&member.dir).expect("the service's client connects");
    // A position the two share is read from the earlier one.
    let shared = open(&mut service, 6 * ENTRY);
    assert_eq!(shared.recording_id(), replayed_id);
    shared.close(&mut service).expect("the handle closes");

    let mut log = open(&mut service, 0);
    assert_eq!(log.recording_id(), replayed_id);
    let mut read = Read::new();
    wait("the service read what the recording held", || {
        poll(&mut log, &mut read);
        log.position() == 10 * ENTRY
    });
    assert!(!log.is_ended(), "the recording is still being made");
    replayed.append(2);
    wait("the service read what the recording gained", || {
        poll(&mut log, &mut read);
        log.position() == 12 * ENTRY
    });
    let (_, stopped_at) = member.stop(replayed);
    assert_eq!(stopped_at, 12 * ENTRY);
    wait("the handle is ended at the recording's stop", || {
        poll(&mut log, &mut read);
        log.is_ended()
    });
    assert_eq!(
        (log.failed_at(), log.withdrawn_at(), log.faulted()),
        (None, None, None)
    );
    assert_eq!(read.len(), 12);
    assert!(read.iter().all(|&(_, _, stream)| stream == 71));
    let ended_at = log.position();
    log.close(&mut service).expect("the handle closes");

    let mut log = open(&mut service, ended_at);
    assert_eq!(log.recording_id(), live_id);
    wait("the service read on in the live recording", || {
        poll(&mut log, &mut read);
        log.position() == 14 * ENTRY
    });
    assert_eq!(read[12..], [(12 * ENTRY, 12, 72), (13 * ENTRY, 13, 72)]);
    log.close(&mut service).expect("the handle closes");
    service.close().expect("the service's client closes");
    member.close();
}

/// A service whose member's commit position stays below the stop of a recording the
/// member has stopped: the driver withdraws the replay's image the publication
/// linger, 5 s, after the replay ended, the handle is ended with `withdrawn_at` at
/// the service's position, and the log opened again there is the same recording,
/// which gives the rest once it is committed.
#[test]
fn a_service_held_below_a_stopped_recordings_end_opens_that_recording_again() {
    let mut member = Member::start("held");
    let first = member.record(71, 10);
    let (first_id, first_end) = member.stop(first);
    let live = member.record(72, 12);
    let committed = 4 * ENTRY;
    member.name(
        live.recording.recording_id(),
        first_end,
        &[(first_id, 0, first_end)],
        committed,
    );

    let mut service = Cavalry::connect_to(&member.dir).expect("the service's client connects");
    let mut log = open(&mut service, 0);
    let mut read = Read::new();
    wait("the image was withdrawn with entries unread", || {
        poll(&mut log, &mut read);
        log.is_ended()
    });
    assert_eq!(log.position(), committed);
    assert_eq!(log.withdrawn_at(), Some(committed));
    assert_eq!((log.failed_at(), log.faulted()), (None, None));
    assert_eq!(read.len(), 4);
    log.close(&mut service).expect("the handle closes");

    let mut log = open(&mut service, committed);
    assert_eq!(log.recording_id(), first_id);
    member
        .commit_position
        .store_release(first_end)
        .expect("stores");
    wait("the service read the rest of the recording", || {
        poll(&mut log, &mut read);
        log.is_ended()
    });
    assert_eq!(log.withdrawn_at(), None);
    let expected: Read = (0..10).map(|number| (number * ENTRY, number, 71)).collect();
    assert_eq!(read, expected);
    log.close(&mut service).expect("the handle closes");
    service.close().expect("the service's client closes");
    member.close();
}

/// What the call answers for each state of the member's file. No file: a member of a
/// version that writes none, and the live recording is replayed. A file of another
/// live recording than the counter names: the member has moved on, and the call
/// answers that there is no log now. A file that fails a check: an error that names
/// the file and the check, and no replay is started.
#[test]
fn the_members_file_is_read_as_missing_as_anothers_and_as_damaged() {
    let mut member = Member::start("file");
    let live = member.record(72, 4);
    let live_id = live.recording.recording_id();
    member
        .log_recording_id
        .store_release(live_id)
        .expect("stores");
    member
        .commit_position
        .store_release(4 * ENTRY)
        .expect("stores");
    let mut service = Cavalry::connect_to(&member.dir).expect("the service's client connects");

    assert!(!member.file().exists());
    let log = open(&mut service, 0);
    assert_eq!(log.recording_id(), live_id);
    log.close(&mut service).expect("the handle closes");

    member.write_file(live_id + 1, 0, &[]);
    assert!(
        service
            .committed_log(0)
            .expect("the driver answers")
            .is_none(),
        "the file is of a recording the member does not name"
    );

    let replays_started = |service: &Cavalry| {
        service
            .counters()
            .read(REPLAY_STARTED.record())
            .expect("the counter")
    };
    let replays_before = replays_started(&service);
    std::fs::write(
        member.file(),
        b"CVLYTERM and then some more bytes, forty and over",
    )
    .expect("the file is stored");
    let damaged = service
        .committed_log(0)
        .expect_err("a file that is not a file of recordings");
    match &damaged {
        ClientError::Io { path, source, .. } => {
            assert_eq!(*path, member.file());
            assert_eq!(source.kind(), std::io::ErrorKind::InvalidData);
        }
        other => panic!("{other}"),
    }
    let text = damaged.to_string();
    assert!(text.contains("recordings-magic"), "{text}");
    assert!(
        text.contains(&format!("{LOG_RECORDINGS_FILE_PREFIX}{MEMBER}")),
        "{text}"
    );
    // A long file of zeros fails the first check it fails, its magic.
    std::fs::write(member.file(), vec![0u8; 4 * MAX_LOG_RECORDINGS_LENGTH])
        .expect("the file is stored");
    let zeros = service.committed_log(0).expect_err("a file of zeros");
    assert!(zeros.to_string().contains("recordings-magic"), "{zeros}");
    // A file longer than the longest is read to one byte past the longest and no
    // further, and refused by its length: the most entries a file holds, every
    // other check passing, with bytes after them. The message says how many bytes
    // were read, so it fails if the read's bound goes.
    let most: Vec<(i64, i64, i64)> = (0..i64::try_from(MAX_LOG_RECORDINGS).expect("a count"))
        .map(|index| (1_000 + index, index * 32, (index + 1) * 32))
        .collect();
    let end = i64::try_from(MAX_LOG_RECORDINGS).expect("a count") * 32;
    member.write_file(live_id, end, &most);
    let mut bytes = std::fs::read(member.file()).expect("the file is read");
    assert_eq!(bytes.len(), MAX_LOG_RECORDINGS_LENGTH, "the longest file");
    bytes.resize(4 * MAX_LOG_RECORDINGS_LENGTH, 0);
    std::fs::write(member.file(), &bytes).expect("the file is stored");
    let too_long = service.committed_log(0).expect_err("a file too long");
    let text = too_long.to_string();
    assert!(
        text.contains("recordings-length")
            && text.contains(&format!("in {} bytes", MAX_LOG_RECORDINGS_LENGTH + 1)),
        "{text}"
    );
    // A file the operating system will not let the service read: a directory in
    // its place. The error names the file and says what to do.
    std::fs::remove_file(member.file()).expect("the file is removed");
    std::fs::create_dir(member.file()).expect("a directory in the file's place");
    let unreadable = service
        .committed_log(0)
        .expect_err("a file that cannot be read");
    assert!(
        matches!(&unreadable, ClientError::Io { path, .. } if *path == member.file()),
        "{unreadable}"
    );
    let text = unreadable.to_string();
    assert!(
        text.contains("Give the service's user the right to read")
            && text.contains("docs/how-to/read-the-committed-log.md#when-it-does-not-work"),
        "{text}"
    );
    std::fs::remove_dir(member.file()).expect("the directory is removed");
    assert_eq!(
        replays_started(&service),
        replays_before,
        "no replay was started for a file that failed"
    );

    member.write_file(live_id, 0, &[]);
    let log = open(&mut service, 0);
    assert_eq!(log.recording_id(), live_id);
    log.close(&mut service).expect("the handle closes");
    service.close().expect("the service's client closes");
    member.close();
}
