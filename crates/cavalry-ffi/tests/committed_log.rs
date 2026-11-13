//! The committed log through the ABI (`spec/c-abi.md`, "Functions", committed log;
//! ADR 0070): a driver that hosts no member answers the open with a null handle and
//! no error; beside a cluster member in the test's process, the C handler is handed
//! the entries a quorum committed and none past the commit position, each with its
//! position; the calls from inside the handler answer as the page's table says; a
//! subscription's header carries no position; every null is refused; and a service
//! whose replay the driver withdrew before it had read it is told where the entries it
//! was not given begin.

use std::ffi::{CStr, CString, c_void};
use std::path::{Path, PathBuf};
use std::ptr;
use std::time::{Duration, Instant};

use cavalry_driver::Driver;
use cavalry_ffi::*;
use cavalry_raft::{ConsensusModule, ElectionState, Role};

struct TestDir(PathBuf);

impl TestDir {
    fn new(test: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("cavalry-ffi-log-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
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

fn errmsg() -> String {
    // SAFETY: the message is NUL-terminated and valid until the next failing call.
    unsafe { CStr::from_ptr(cavalry_errmsg()) }
        .to_str()
        .expect("UTF-8")
        .to_string()
}

/// A client through the ABI on the driver at `dir`.
fn connect(dir: &Path) -> (*mut cavalry_context_t, *mut cavalry_t) {
    let dir_c = CString::new(dir.to_str().expect("a UTF-8 temp dir")).expect("no NUL");
    // SAFETY: the ABI's conventions, held by this test.
    unsafe {
        let mut context = ptr::null_mut();
        assert_eq!(cavalry_context_init_v(&mut context, CAVALRY_ABI_VERSION), 0);
        assert_eq!(cavalry_context_set_dir(context, dir_c.as_ptr()), 0);
        let mut client = ptr::null_mut();
        assert_eq!(cavalry_init(&mut client, context), 0, "{}", errmsg());
        (context, client)
    }
}

// A cluster's ports, from the one file that says how they are taken and why, which
// is `cavalry-raft`'s: `crates/cavalry-raft/tests/ports/mod.rs`.
#[path = "../../cavalry-raft/tests/ports/mod.rs"]
mod ports;
use ports::free_ports;

/// Three members over three drivers on the loopback, as `cavalry-raft`'s own cluster
/// test starts them. `members[0]` is the member that leads and `members[1..]` are its
/// followers, whichever member the cluster has for a leader: `start` finds it, and
/// starts the members in the order 1, 2, 0, so that no member's place is its id,
/// before the leader is put first or after (ADR 0100, change 3). `members[1]` is a
/// follower that joined the leader's log on time, whose log is one recording.
struct Cluster {
    members: Vec<(Driver, ConsensusModule, PathBuf)>,
    _dir: TestDir,
}

impl Cluster {
    fn start(test: &str) -> Self {
        let dir = TestDir::new(test);
        let ports = free_ports(15);
        let members_string = (0..3)
            .map(|member| {
                let endpoints: Vec<String> = ports[member * 5..member * 5 + 5]
                    .iter()
                    .map(|port| format!("127.0.0.1:{port}"))
                    .collect();
                format!("{member},{}", endpoints.join(","))
            })
            .collect::<Vec<_>>()
            .join("|");
        let members = [1, 2, 0]
            .into_iter()
            .map(|id| {
                let driver_dir = dir.path().join(format!("member-{id}"));
                let log_dir = dir.path().join(format!("member-{id}-log"));
                let driver = Driver::start(&driver_dir, &log_dir).expect("the driver starts");
                let module = ConsensusModule::start(&driver_dir, &log_dir, &members_string, id)
                    .expect("the member starts");
                (driver, module, driver_dir)
            })
            .collect();
        let mut cluster = Self { members, _dir: dir };
        cluster.run_until("every election closed", |cluster| {
            cluster
                .members
                .iter()
                .all(|(_, module, _)| module.election_state() == ElectionState::Closed)
        });
        let leader = cluster
            .members
            .iter()
            .position(|(_, module, _)| module.role() == Role::Leader)
            .expect("one member leads");
        cluster.members.swap(0, leader);
        assert_eq!(cluster.members[0].1.role(), Role::Leader);
        // The follower a service reads beside, `members[1]`, is one whose log is one
        // recording, from 0. A follower that joined the leader's log late holds what
        // lies below its live image in its recording of the replay it caught up by
        // (ADR 0103; ADR 0104), and a handle opened at 0 beside it replays that
        // recording and ends where it stops, after the term's first entry; the tests
        // here read past it through one handle. The leader closes its election on a
        // quorum of members that joined its log at its position, so one follower at
        // least did. Each member writes its file before it names its recording.
        cluster.run_until("every member named its recording", |cluster| {
            cluster
                .members
                .iter()
                .all(|(_, module, _)| module.log_recording_id().is_some())
        });
        let on_time = (1..3)
            .find(|&place| cluster.is_one_recording(place))
            .expect("a follower that joined the log on time");
        cluster.members.swap(1, on_time);
        cluster
    }

    /// Whether the member in `place` holds its log in one recording, from 0: its file
    /// of its log's recordings names none below the live one, which begins at 0.
    fn is_one_recording(&self, place: usize) -> bool {
        use cavalry_proto::cluster::{LOG_RECORDINGS_FILE_PREFIX, LogRecordings};
        let (_, module, dir) = &self.members[place];
        let member_id = module.member_id();
        let bytes = std::fs::read(dir.join(format!("{LOG_RECORDINGS_FILE_PREFIX}{member_id}")))
            .expect("the member's file of its log's recordings");
        let file = LogRecordings::read(&bytes, member_id).expect("the file passes its checks");
        file.is_empty() && file.live_start_position() == 0
    }

    fn work(&mut self) -> usize {
        self.members
            .iter_mut()
            .map(|(_, module, _)| module.do_work().expect("the member's duty cycle"))
            .sum()
    }

    fn run_until(&mut self, what: &str, mut done: impl FnMut(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while !done(self) {
            assert!(Instant::now() < deadline, "{what}");
            if self.work() == 0 {
                std::thread::sleep(Duration::from_micros(200));
            }
        }
    }

    /// The leader appends `count` entries of eight bytes, numbered from `from`.
    fn offer(&mut self, from: u32, count: u32) {
        for i in from..from + count {
            let payload = [i.to_le_bytes(), (!i).to_le_bytes()].concat();
            let deadline = Instant::now() + Duration::from_secs(60);
            while self.members[0].1.offer(&payload).is_err() {
                assert!(Instant::now() < deadline, "the leader takes the entry");
                self.work();
            }
        }
    }

    /// Runs the members until every one has committed up to `position` and recorded
    /// up to it. A commit needs a quorum alone, and since each member asks for its
    /// sync a quorum is quick: the leader and one follower commit an entry while the
    /// other follower has yet to record it, or to name its recording at all. A service
    /// reads through its own member's recording, and nothing runs a member here but
    /// this loop, so a test that opens a log waits for the recording it will read:
    /// without that the open answers a null handle, no log yet, or refuses a position
    /// past the recording's extent.
    fn commit(&mut self, position: i64) {
        self.run_until("every member committed and recorded", |cluster| {
            cluster.members.iter().all(|(_, module, _)| {
                module.commit_position() == position && module.append_position() == position
            })
        });
    }
}

/// What the handler is handed for one entry.
#[derive(Debug, Clone, PartialEq)]
struct Entry {
    position: i64,
    body: Vec<u8>,
    frame_length: i32,
}

/// What a poll's handler collects, and what its calls on its own log answered.
#[derive(Default)]
struct Collected {
    entries: Vec<Entry>,
    /// The log the handler belongs to, when the test wants its re-entrant calls made.
    log: Option<*mut cavalry_committed_log_t>,
    client: Option<*mut cavalry_t>,
    reentrant: Vec<(&'static str, i64, i32)>,
    /// The id of the member the log is read beside, which the handler's own call for
    /// the constants must name.
    member_id: i32,
}

unsafe extern "C" fn collect(
    clientd: *mut c_void,
    buffer: *const u8,
    length: usize,
    header: *const cavalry_header_t,
) {
    // SAFETY: the test passes a `Collected`, and the body and header are valid for the
    // call.
    unsafe {
        let collected = &mut *clientd.cast::<Collected>();
        let mut values = std::mem::zeroed::<cavalry_header_values_t>();
        assert_eq!(cavalry_header_values(header, &mut values), 0);
        collected.entries.push(Entry {
            position: cavalry_committed_log_entry_position(header),
            body: std::slice::from_raw_parts(buffer, length).to_vec(),
            frame_length: values.frame.frame_length,
        });
        let (Some(log), Some(client)) = (collected.log, collected.client) else {
            return;
        };
        if !collected.reentrant.is_empty() {
            return;
        }
        // Each call on the log from inside its own handler, with what it returned and
        // the thread's error code after it.
        let mut answers = Vec::new();
        let polled = cavalry_committed_log_poll(log, Some(collect), clientd, 1);
        answers.push(("poll", i64::from(polled), cavalry_errcode()));
        let mut fault = std::mem::zeroed::<cavalry_fault_t>();
        let faulted = cavalry_committed_log_fault(log, &mut fault);
        answers.push(("fault", i64::from(faulted), cavalry_errcode()));
        answers.push((
            "position",
            cavalry_committed_log_position(log),
            cavalry_errcode(),
        ));
        answers.push((
            "commit_position",
            cavalry_committed_log_commit_position(log),
            cavalry_errcode(),
        ));
        answers.push((
            "is_ended",
            i64::from(cavalry_committed_log_is_ended(log)),
            cavalry_errcode(),
        ));
        let mut at = 7;
        let failed = cavalry_committed_log_failed_at(log, &mut at);
        assert_eq!(at, 7, "a refused call writes nothing");
        answers.push(("failed_at", i64::from(failed), cavalry_errcode()));
        let withdrawn = cavalry_committed_log_withdrawn_at(log, &mut at);
        assert_eq!(at, 7, "a refused call writes nothing");
        answers.push(("withdrawn_at", i64::from(withdrawn), cavalry_errcode()));
        let mut kept = cavalry_fault_t {
            code: 7,
            session_id: 7,
            position: 7,
        };
        let faulted = cavalry_committed_log_faulted(log, &mut kept);
        assert_eq!(
            (kept.code, kept.session_id, kept.position),
            (7, 7, 7),
            "a refused call writes nothing"
        );
        answers.push(("faulted", i64::from(faulted), cavalry_errcode()));
        // A null pointer is refused first, as it is outside the handler.
        for (name, refused) in [
            (
                "failed_at null",
                cavalry_committed_log_failed_at(log, ptr::null_mut()),
            ),
            (
                "withdrawn_at null",
                cavalry_committed_log_withdrawn_at(log, ptr::null_mut()),
            ),
            (
                "faulted null",
                cavalry_committed_log_faulted(log, ptr::null_mut()),
            ),
        ] {
            answers.push((name, i64::from(refused), cavalry_errcode()));
        }
        answers.push((
            "close",
            i64::from(cavalry_committed_log_close(log, client)),
            cavalry_errcode(),
        ));
        let mut constants = std::mem::zeroed::<cavalry_committed_log_constants_t>();
        let filled = cavalry_committed_log_constants(log, &mut constants);
        assert_eq!(
            (constants.member_id, constants.recording_id),
            (collected.member_id, 0)
        );
        answers.push(("constants", i64::from(filled), 0));
        collected.reentrant = answers;
    }
}

/// A handler for a subscription's poll that asks its header for an entry position.
unsafe extern "C" fn ask_position(
    clientd: *mut c_void,
    _buffer: *const u8,
    _length: usize,
    header: *const cavalry_header_t,
) {
    // SAFETY: the test passes a `Vec<(i64, i32)>`, and the header is valid for the call.
    unsafe {
        let answers = &mut *clientd.cast::<Vec<(i64, i32)>>();
        let position = cavalry_committed_log_entry_position(header);
        answers.push((position, cavalry_errcode()));
    }
}

#[test]
fn a_driver_with_no_member_answers_the_open_with_a_null_handle_and_no_error() {
    let dir = TestDir::new("no-member");
    let driver_dir = dir.path().join("driver");
    let driver = Driver::start(&driver_dir, dir.path().join("log")).expect("the driver");
    let (context, client) = connect(&driver_dir);
    // SAFETY: the ABI's conventions, held by this test.
    unsafe {
        let before = cavalry_errcode();
        let mut log = ptr::dangling_mut::<cavalry_committed_log_t>();
        assert_eq!(
            cavalry_committed_log(&mut log, client, 0),
            0,
            "{}",
            errmsg()
        );
        assert!(log.is_null(), "no member: no handle");
        assert_eq!(cavalry_errcode(), before, "no error is set");

        // Every null is refused, and a refused open leaves a null handle.
        assert_eq!(cavalry_committed_log(ptr::null_mut(), client, 0), -1);
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_NULL);
        let mut log = ptr::dangling_mut::<cavalry_committed_log_t>();
        assert_eq!(cavalry_committed_log(&mut log, ptr::null_mut(), 0), -1);
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_NULL);
        assert!(log.is_null());
        assert!(errmsg().contains("docs/reference/c-abi.md#when-a-function-cannot-answer"));
        let null = ptr::null_mut::<cavalry_committed_log_t>();
        let mut clientd = Collected::default();
        let clientd = ptr::addr_of_mut!(clientd).cast::<c_void>();
        assert_eq!(
            cavalry_committed_log_poll(null, Some(collect), clientd, 1),
            -1
        );
        assert_eq!(cavalry_committed_log_position(null), -1);
        assert_eq!(cavalry_committed_log_commit_position(null), -1);
        assert!(!cavalry_committed_log_is_ended(null));
        let mut at = 7;
        assert!(!cavalry_committed_log_failed_at(null, &mut at));
        assert_eq!(at, 7);
        assert!(!cavalry_committed_log_withdrawn_at(null, &mut at));
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_NULL);
        assert_eq!(at, 7);
        let mut fault = std::mem::zeroed::<cavalry_fault_t>();
        assert_eq!(cavalry_committed_log_fault(null, &mut fault), -1);
        assert!(!cavalry_committed_log_faulted(null, &mut fault));
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_NULL);
        assert_eq!(
            (fault.code, fault.session_id, fault.position),
            (0, 0, 0),
            "a refused call writes nothing"
        );
        let mut constants = std::mem::zeroed::<cavalry_committed_log_constants_t>();
        assert_eq!(cavalry_committed_log_constants(null, &mut constants), -1);
        assert_eq!(cavalry_committed_log_close(null, client), -1);
        assert_eq!(cavalry_committed_log_entry_position(ptr::null()), -1);
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_NULL);

        assert_eq!(cavalry_close(client), 0);
        assert_eq!(cavalry_context_close(context), 0);
    }
    driver.close().expect("closes");
}

/// A service through the ABI beside a follower: a hundred entries committed
/// by a quorum are handed to the C handler in order, each at the position the log
/// gives it, the term's first entry at 0 and the hundred after it 96 bytes apart; the
/// handle's position and bound are the end of the last; and nothing arrives while
/// nothing more is committed. Then the calls from inside the handler, each refused
/// but the constants; and a close with the client, after which a new handle opens at
/// the position the first reached and reads only what was committed since.
#[test]
fn a_service_reads_the_committed_log_through_the_c_handler_with_each_entrys_position() {
    let mut cluster = Cluster::start("service");
    const ENTRIES: u32 = 100;
    cluster.offer(0, ENTRIES);
    let end = 96 + i64::from(ENTRIES) * 96;
    cluster.commit(end);

    // The service is beside a follower, whichever member that is.
    let member_id = cluster.members[1].1.member_id();
    let (context, client) = connect(&cluster.members[1].2);
    // SAFETY: the ABI's conventions, held by this test.
    unsafe {
        let mut log = ptr::null_mut();
        assert_eq!(
            cavalry_committed_log(&mut log, client, 0),
            0,
            "{}",
            errmsg()
        );
        assert!(!log.is_null(), "the follower has a recording of the log");
        let mut constants = std::mem::zeroed::<cavalry_committed_log_constants_t>();
        assert_eq!(cavalry_committed_log_constants(log, &mut constants), 0);
        // The follower's log is one recording (`Cluster::start`), so a service at
        // position 0 reads the live one.
        assert_eq!(
            (constants.member_id, Some(constants.recording_id)),
            (member_id, cluster.members[1].1.log_recording_id())
        );
        assert_eq!(cavalry_committed_log_position(log), 0);
        assert_eq!(cavalry_committed_log_commit_position(log), 0);

        let mut collected = Collected::default();
        let clientd = ptr::addr_of_mut!(collected).cast::<c_void>();
        let deadline = Instant::now() + Duration::from_secs(60);
        while collected.entries.len() < ENTRIES as usize + 1 {
            assert!(Instant::now() < deadline, "the service read the log");
            let delivered = cavalry_committed_log_poll(log, Some(collect), clientd, 16);
            assert!(delivered >= 0, "{}", errmsg());
            assert!(delivered <= 16);
            cluster.work();
        }
        assert_eq!(collected.entries.len(), ENTRIES as usize + 1);
        for (index, entry) in collected.entries.iter().enumerate() {
            assert_eq!(entry.position, 96 * index as i64, "entry {index}");
            if index > 0 {
                let i = index as u32 - 1;
                assert!(
                    entry
                        .body
                        .ends_with(&[i.to_le_bytes(), (!i).to_le_bytes()].concat()),
                    "entry {index} carries the payload the leader appended"
                );
            }
            // The position after an entry is its own plus its frame, rounded up to 32.
            let after = entry.position + ((i64::from(entry.frame_length) + 31) & !31);
            assert_eq!(after, 96 * (index as i64 + 1));
        }
        assert_eq!(cavalry_committed_log_position(log), end);
        assert_eq!(cavalry_committed_log_commit_position(log), end);
        assert!(!cavalry_committed_log_is_ended(log));
        let mut at = 7;
        assert!(!cavalry_committed_log_failed_at(log, &mut at));
        assert_eq!(at, 7, "a live log writes nothing");
        assert!(!cavalry_committed_log_withdrawn_at(log, &mut at));
        assert_eq!(at, 7, "a live log writes nothing");
        // A null pointer for the position is refused as any required pointer is.
        assert!(!cavalry_committed_log_failed_at(log, ptr::null_mut()));
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_NULL);
        assert!(!cavalry_committed_log_withdrawn_at(log, ptr::null_mut()));
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_NULL);
        let mut fault = std::mem::zeroed::<cavalry_fault_t>();
        assert_eq!(cavalry_committed_log_fault(log, &mut fault), 0);
        assert_eq!(
            (fault.code, fault.session_id, fault.position),
            (0, 0, CAVALRY_NULL_POSITION)
        );
        // Nothing more is committed, so nothing more arrives.
        for _ in 0..50 {
            assert_eq!(
                cavalry_committed_log_poll(log, Some(collect), clientd, 16),
                0
            );
            cluster.work();
        }

        // One more entry, read by a handler that calls its own log.
        cluster.offer(ENTRIES, 1);
        cluster.commit(end + 96);
        let mut reentrant = Collected {
            log: Some(log),
            client: Some(client),
            member_id,
            ..Collected::default()
        };
        let clientd = ptr::addr_of_mut!(reentrant).cast::<c_void>();
        let deadline = Instant::now() + Duration::from_secs(60);
        while reentrant.entries.is_empty() {
            assert!(Instant::now() < deadline, "the service read the entry");
            assert!(cavalry_committed_log_poll(log, Some(collect), clientd, 16) >= 0);
            cluster.work();
        }
        assert_eq!(
            reentrant.entries.len(),
            1,
            "the refused poll delivered nothing"
        );
        assert_eq!(reentrant.entries[0].position, end);
        assert_eq!(
            reentrant.reentrant,
            vec![
                ("poll", -1, CAVALRY_ERR_POLLING),
                ("fault", -1, CAVALRY_ERR_POLLING),
                ("position", -1, CAVALRY_ERR_POLLING),
                ("commit_position", -1, CAVALRY_ERR_POLLING),
                ("is_ended", 0, CAVALRY_ERR_POLLING),
                ("failed_at", 0, CAVALRY_ERR_POLLING),
                ("withdrawn_at", 0, CAVALRY_ERR_POLLING),
                ("faulted", 0, CAVALRY_ERR_POLLING),
                ("failed_at null", 0, CAVALRY_ERR_NULL),
                ("withdrawn_at null", 0, CAVALRY_ERR_NULL),
                ("faulted null", 0, CAVALRY_ERR_NULL),
                ("close", -1, CAVALRY_ERR_POLLING),
                ("constants", 0, 0),
            ]
        );
        // The refused close freed nothing: the handle still answers.
        assert_eq!(cavalry_committed_log_position(log), end + 96);

        // A subscription's header carries no position.
        let channel = CString::new("cavalry:ipc").unwrap();
        let mut subscription = ptr::null_mut();
        assert_eq!(
            cavalry_add_subscription(&mut subscription, client, channel.as_ptr(), 1001),
            0,
            "{}",
            errmsg()
        );
        let mut publication = ptr::null_mut();
        assert_eq!(
            cavalry_add_publication(&mut publication, client, channel.as_ptr(), 1001),
            0,
            "{}",
            errmsg()
        );
        let mut answers: Vec<(i64, i32)> = Vec::new();
        let answers_d = ptr::addr_of_mut!(answers).cast::<c_void>();
        let message = [1u8; 8];
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut offered = false;
        while answers.is_empty() {
            assert!(
                Instant::now() < deadline,
                "the subscription read the message"
            );
            if !offered {
                offered =
                    cavalry_publication_offer(publication, message.as_ptr(), message.len()) > 0;
            }
            assert!(cavalry_subscription_poll(subscription, Some(ask_position), answers_d, 1) >= 0);
        }
        assert_eq!(answers, vec![(-1, CAVALRY_ERR_NO_POSITION)]);
        assert!(
            errmsg().contains("docs/reference/c-abi.md#a-frames-position"),
            "{}",
            errmsg()
        );
        assert_eq!(cavalry_publication_close(publication), 0);
        assert_eq!(cavalry_subscription_close(subscription), 0);

        // The close stops the replay; a new handle opens where the first one stopped
        // and reads what is committed after it and nothing before.
        assert_eq!(cavalry_committed_log_close(log, client), 0, "{}", errmsg());
        cluster.offer(ENTRIES + 1, 3);
        cluster.commit(end + 4 * 96);
        let mut again = ptr::null_mut();
        assert_eq!(
            cavalry_committed_log(&mut again, client, end + 96),
            0,
            "{}",
            errmsg()
        );
        assert!(!again.is_null());
        assert_eq!(cavalry_committed_log_position(again), end + 96);
        let mut rest = Collected::default();
        let clientd = ptr::addr_of_mut!(rest).cast::<c_void>();
        let deadline = Instant::now() + Duration::from_secs(60);
        while rest.entries.len() < 3 {
            assert!(Instant::now() < deadline, "the second handle read the rest");
            assert!(cavalry_committed_log_poll(again, Some(collect), clientd, 16) >= 0);
            cluster.work();
        }
        assert_eq!(
            rest.entries.iter().map(|e| e.position).collect::<Vec<_>>(),
            vec![end + 96, end + 2 * 96, end + 3 * 96]
        );
        // A log that outlives its client: after `cavalry_close` its poll answers the
        // closed fault, and a null client is how it is closed, since the client handle
        // is freed. The subscription closes, and the replay is the driver's to end.
        assert_eq!(cavalry_close(client), 0);
        assert_eq!(
            cavalry_committed_log_poll(again, Some(collect), clientd, 16),
            -1
        );
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_FAULT_CLOSED);
        let mut fault = std::mem::zeroed::<cavalry_fault_t>();
        assert_eq!(cavalry_committed_log_fault(again, &mut fault), 0);
        assert_eq!(fault.code, CAVALRY_ERR_FAULT_CLOSED);
        assert_eq!(cavalry_committed_log_close(again, ptr::null_mut()), 0);
        assert_eq!(cavalry_context_close(context), 0);
    }
}

/// A service that does not read for longer than the publication linger, 5 s, after its
/// member's consensus module closes, with entries below its bound unread
/// (`spec/c-abi.md`, "Functions", committed log; `spec/client-protocol.md`, "Events").
/// The member's recording stops, the replay ends at the stop, and the driver withdraws
/// the replay's image once the linger has passed, read or not. The handle is ended
/// short of its bound, and it says so through the ABI as the Rust handle does:
/// `cavalry_committed_log_withdrawn_at` writes where the entries it was not given
/// begin, which is where the service had read to, and `cavalry_committed_log_failed_at`
/// stays `false`, so that a service in C is never told it read what it did not.
#[test]
fn a_service_whose_replay_the_driver_withdrew_unread_is_told_where_its_entries_stop() {
    let mut cluster = Cluster::start("service-withdrawn");
    let (context, client) = connect(&cluster.members[1].2);
    // SAFETY: the ABI's conventions, held by this test.
    unsafe {
        // The service opens its log before any entry is offered.
        let mut log: *mut cavalry_committed_log_t = ptr::null_mut();
        let deadline = Instant::now() + Duration::from_secs(60);
        while log.is_null() {
            assert!(Instant::now() < deadline, "the service opened its log");
            assert_eq!(
                cavalry_committed_log(&mut log, client, 0),
                0,
                "{}",
                errmsg()
            );
            cluster.work();
        }

        // A thousand entries, committed and recorded on every member before the
        // service reads any: the follower's recording holds them all, and flow control
        // lets the replay run a window ahead of a service that has read one entry.
        const ENTRIES: u32 = 1000;
        cluster.offer(0, ENTRIES);
        let end = 96 + i64::from(ENTRIES) * 96;
        cluster.run_until(
            "every member committed and appended every entry",
            |cluster| {
                cluster.members.iter().all(|(_, module, _)| {
                    module.commit_position() == end && module.append_position() == end
                })
            },
        );

        // The service takes the member's commit position as its bound and reads at
        // most one entry under it.
        let mut collected = Collected::default();
        let clientd = ptr::addr_of_mut!(collected).cast::<c_void>();
        let deadline = Instant::now() + Duration::from_secs(30);
        while cavalry_committed_log_commit_position(log) < end {
            assert!(Instant::now() < deadline, "the service took its bound");
            let delivered = cavalry_committed_log_poll(log, Some(collect), clientd, 1);
            assert!((0..=1).contains(&delivered), "{}", errmsg());
        }
        assert!(cavalry_committed_log_position(log) <= 96);
        assert!(!cavalry_committed_log_is_ended(log));

        // The follower's consensus module closes. Its recording stops, the replay ends,
        // and the service does not read: a poll with a fragment limit of 0 applies the
        // events the subscription has and reads no frame, so the handle is ended only
        // when the driver has withdrawn the image, the linger after the replay's end.
        //
        // The linger runs from the replay's end, which is inside the member's close.
        // The time below is taken before the close begins, so that it is at or before
        // the replay's end and the five seconds are a lower bound: taken after the
        // close returned, it read 4.993 s on a run of `master`.
        let (driver, module, _) = cluster.members.remove(1);
        let closed = Instant::now();
        module.close().expect("the member closes");
        let deadline = Instant::now() + Duration::from_secs(30);
        while !cavalry_committed_log_is_ended(log) {
            assert!(Instant::now() < deadline, "the service's log ended");
            assert_eq!(
                cavalry_committed_log_poll(log, Some(collect), clientd, 0),
                0,
                "{}",
                errmsg()
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            closed.elapsed() >= Duration::from_secs(5),
            "the image went at the linger's end, not at its end of stream: {:?}",
            closed.elapsed()
        );

        // Ended short of its bound, and the handle says where.
        let position = cavalry_committed_log_position(log);
        assert!(position < end, "{position} of {end}");
        assert_eq!(cavalry_committed_log_commit_position(log), end);
        let mut at = -7;
        assert!(
            cavalry_committed_log_withdrawn_at(log, &mut at),
            "an ended handle short of its bound says so: {}",
            errmsg()
        );
        assert_eq!(at, position);
        let mut failed = -7;
        assert!(!cavalry_committed_log_failed_at(log, &mut failed));
        assert_eq!(failed, -7, "a replay that did not fail writes nothing");
        // Nothing more is delivered, and the answer stays.
        assert_eq!(
            cavalry_committed_log_poll(log, Some(collect), clientd, 16),
            0
        );
        assert!(cavalry_committed_log_withdrawn_at(log, &mut at));
        assert_eq!(at, position);
        // Every entry below the position was delivered, in order, and none past it.
        assert_eq!(
            i64::try_from(collected.entries.len()).expect("a count") * 96,
            position
        );
        for (index, entry) in collected.entries.iter().enumerate() {
            assert_eq!(entry.position, 96 * i64::try_from(index).expect("an index"));
        }

        assert_eq!(cavalry_committed_log_close(log, client), 0, "{}", errmsg());
        assert_eq!(cavalry_close(client), 0);
        assert_eq!(cavalry_context_close(context), 0);
        driver.close().expect("the driver closes");
    }
}

/// A log opened at a position where no entry begins (`spec/c-abi.md`, "Functions",
/// committed log). The driver's replay finds no frame there and fails at once, at that
/// position, with nothing played. The handle is ended with nothing delivered, and it
/// says why through the ABI: `cavalry_committed_log_failed_at` writes the position,
/// and `cavalry_committed_log_withdrawn_at` stays `false`, since nothing that was
/// played went unread. Before ADR 0078 the first poll found the client closed as
/// `corrupt`: the driver had freed the replay's image before the client's conductor
/// read its announcement.
#[test]
fn a_log_opened_where_no_entry_begins_fails_there_and_says_so() {
    let mut cluster = Cluster::start("service-failed");
    const ENTRIES: u32 = 10;
    cluster.offer(0, ENTRIES);
    let end = 96 + i64::from(ENTRIES) * 96;
    // Every member has recorded the entries, so the position asked for is below the
    // extent of the follower's recording and the driver takes the replay.
    cluster.commit(end);

    let (context, client) = connect(&cluster.members[1].2);
    // SAFETY: the ABI's conventions, held by this test.
    unsafe {
        // Position 32 is inside the term's first entry, which is 96 bytes from 0.
        let mut log: *mut cavalry_committed_log_t = ptr::null_mut();
        assert_eq!(
            cavalry_committed_log(&mut log, client, 32),
            0,
            "{}",
            errmsg()
        );
        assert!(!log.is_null(), "the follower has a recording of the log");
        let mut collected = Collected::default();
        let clientd = ptr::addr_of_mut!(collected).cast::<c_void>();
        let deadline = Instant::now() + Duration::from_secs(30);
        while !cavalry_committed_log_is_ended(log) {
            assert!(Instant::now() < deadline, "the log ended");
            let delivered = cavalry_committed_log_poll(log, Some(collect), clientd, 16);
            assert_eq!(
                delivered,
                0,
                "{}; the client closed with {}",
                errmsg(),
                cavalry_closed(client)
            );
            cluster.work();
        }
        assert!(collected.entries.is_empty(), "nothing was delivered");
        assert_eq!(cavalry_committed_log_position(log), 32);
        let mut at = -7;
        assert!(
            cavalry_committed_log_failed_at(log, &mut at),
            "a replay that failed says where: {}",
            errmsg()
        );
        assert_eq!(at, 32);
        let mut withdrawn = -7;
        assert!(!cavalry_committed_log_withdrawn_at(log, &mut withdrawn));
        assert_eq!(withdrawn, -7, "nothing played went unread");
        assert_eq!(cavalry_committed_log_close(log, client), 0, "{}", errmsg());
        assert_eq!(cavalry_close(client), 0);
        assert_eq!(cavalry_context_close(context), 0);
    }
}

/// The log buffers under a driver directory's `publications/`, by name.
fn publication_logs(dir: &Path) -> Vec<PathBuf> {
    let mut logs: Vec<PathBuf> = std::fs::read_dir(dir.join("publications"))
        .expect("the driver's publications")
        .map(|entry| entry.expect("an entry").path())
        .collect();
    logs.sort();
    logs
}

/// A service whose read of the replay's image stops on a fault, through the ABI: the
/// poll that meets it returns −1 with `CAVALRY_ERR_FAULT_IMAGE`, once;
/// `cavalry_committed_log_fault` answers it until the next poll; and
/// `cavalry_committed_log_faulted` answers it from that poll on, for as long as the
/// handle is held, with the same code, session id and position, and sets no error
/// (`spec/c-abi.md`, version 3; ADR 0080). Before the fault it is `false` and writes
/// nothing.
///
/// The test makes the fault as `cavalry-raft`'s own test of the Rust handle does: the
/// service reads five of eleven committed entries, and the test changes the version
/// byte of the sixth in the replay's log buffer, the one file that appears under the
/// member's `publications/` when the log is opened.
#[test]
fn a_service_whose_image_faulted_reads_the_fault_for_the_life_of_its_handle() {
    use std::os::unix::fs::FileExt;

    let mut cluster = Cluster::start("service-faulted");
    const ENTRIES: u32 = 10;
    const READ_FIRST: usize = 5;
    cluster.offer(0, ENTRIES);
    let end = 96 + i64::from(ENTRIES) * 96;
    let stopped = 96 * i64::try_from(READ_FIRST).expect("a count");
    cluster.commit(end);

    let member_dir = cluster.members[1].2.clone();
    let (context, client) = connect(&member_dir);
    let before = publication_logs(&member_dir);
    // SAFETY: the ABI's conventions, held by this test.
    unsafe {
        let mut log: *mut cavalry_committed_log_t = ptr::null_mut();
        assert_eq!(
            cavalry_committed_log(&mut log, client, 0),
            0,
            "{}",
            errmsg()
        );
        assert!(!log.is_null(), "the follower has a recording of the log");
        let replayed: Vec<PathBuf> = publication_logs(&member_dir)
            .into_iter()
            .filter(|path| !before.contains(path))
            .collect();
        assert_eq!(replayed.len(), 1, "the replay's one log: {replayed:?}");
        let replay_log = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&replayed[0])
            .expect("the replay's log opens");

        // Five entries, under a bound that is the end of the eleven.
        let mut collected = Collected::default();
        let clientd = ptr::addr_of_mut!(collected).cast::<c_void>();
        let deadline = Instant::now() + Duration::from_secs(30);
        while collected.entries.len() < READ_FIRST
            || cavalry_committed_log_commit_position(log) < end
        {
            assert!(Instant::now() < deadline, "the service read five entries");
            let limit = READ_FIRST - collected.entries.len();
            let delivered = cavalry_committed_log_poll(log, Some(collect), clientd, limit);
            assert!(delivered >= 0, "{}", errmsg());
            cluster.work();
        }
        assert_eq!(cavalry_committed_log_position(log), stopped);

        // No fault yet: `false`, nothing written, and no error set by the answer.
        let untouched = cavalry_fault_t {
            code: 7,
            session_id: 7,
            position: 7,
        };
        let mut kept = untouched;
        assert_eq!(cavalry_committed_log_entry_position(ptr::null()), -1);
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_NULL);
        assert!(!cavalry_committed_log_faulted(log, &mut kept));
        assert_eq!((kept.code, kept.session_id, kept.position), (7, 7, 7));
        assert_eq!(
            cavalry_errcode(),
            CAVALRY_ERR_NULL,
            "an answer sets no error: the code is the one the call before left"
        );
        assert!(!cavalry_committed_log_faulted(log, ptr::null_mut()));
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_NULL);
        assert!(
            errmsg().contains("cavalry_committed_log_faulted"),
            "{}",
            errmsg()
        );

        // The sixth entry's frame, once the replay has written it, with a version no
        // reader accepts.
        let at = u64::try_from(stopped).expect("a position");
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let mut length = [0u8; 4];
            replay_log
                .read_exact_at(&mut length, at)
                .expect("the frame's length");
            if i32::from_le_bytes(length) >= 32 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the replay wrote the sixth entry"
            );
            cluster.work();
        }
        replay_log
            .write_all_at(&[9], at + 4)
            .expect("the version byte");

        // The poll that meets it: −1 with the fault as the error and in `_fault`.
        assert_eq!(
            cavalry_committed_log_poll(log, Some(collect), clientd, 16),
            -1,
            "the poll read a frame that fails its check"
        );
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_FAULT_IMAGE, "{}", errmsg());
        assert_eq!(
            collected.entries.len(),
            READ_FIRST,
            "nothing more was delivered"
        );
        let mut met = std::mem::zeroed::<cavalry_fault_t>();
        assert_eq!(cavalry_committed_log_fault(log, &mut met), 0);
        assert_eq!((met.code, met.position), (CAVALRY_ERR_FAULT_IMAGE, stopped));
        assert_ne!(met.session_id, 0, "the replay's session");

        // The lasting read: the same fault, now and after any number of polls, each
        // of which returns 0 and clears what `_fault` answers.
        for polls in 0..50 {
            let mut kept = untouched;
            assert_eq!(cavalry_committed_log_entry_position(ptr::null()), -1);
            assert!(
                cavalry_committed_log_faulted(log, &mut kept),
                "after {polls} polls"
            );
            assert_eq!(
                (kept.code, kept.session_id, kept.position),
                (CAVALRY_ERR_FAULT_IMAGE, met.session_id, stopped)
            );
            assert_eq!(
                cavalry_errcode(),
                CAVALRY_ERR_NULL,
                "an answer sets no error: the code is the one the call before left"
            );
            assert_eq!(
                cavalry_committed_log_poll(log, Some(collect), clientd, 16),
                0
            );
            let mut last = std::mem::zeroed::<cavalry_fault_t>();
            assert_eq!(cavalry_committed_log_fault(log, &mut last), 0);
            assert_eq!(last.code, 0, "the next poll cleared the poll's own report");
        }
        assert!(cavalry_committed_log_is_ended(log));
        let mut position = -7;
        assert!(!cavalry_committed_log_failed_at(log, &mut position));
        assert!(!cavalry_committed_log_withdrawn_at(log, &mut position));
        assert_eq!(position, -7);
        assert_eq!(cavalry_committed_log_position(log), stopped);
        assert_eq!(cavalry_closed(client), 0, "the client is open");

        assert_eq!(cavalry_committed_log_close(log, client), 0, "{}", errmsg());
        assert_eq!(cavalry_close(client), 0);
        assert_eq!(cavalry_context_close(context), 0);
    }
}

/// What the open answers through the ABI for the member's file of its log's recordings
/// (`spec/c-abi.md`, the committed log's row; ADR 0103). A file that names another
/// live recording than the member's counter is a member between two recordings: 0
/// with a null handle and no error set, as for a driver with no member. A file that
/// fails a check is −1 with `CAVALRY_ERR_IO`, a null handle and a message that names
/// the file and the check. The member's own file opens the log.
#[test]
fn the_open_answers_for_the_members_file_of_its_logs_recordings() {
    use cavalry_proto::cluster::{
        LOG_RECORDINGS_FILE_PREFIX, LogRecording, MAX_LOG_RECORDINGS_LENGTH, write_log_recordings,
    };

    let mut cluster = Cluster::start("recordings-file");
    cluster.offer(0, 10);
    cluster.commit(96 + 10 * 96);
    let member_id = cluster.members[1].1.member_id();
    let live = cluster.members[1]
        .1
        .log_recording_id()
        .expect("the follower names its recording");
    let file = cluster.members[1]
        .2
        .join(format!("{LOG_RECORDINGS_FILE_PREFIX}{member_id}"));
    let own = std::fs::read(&file).expect("the member wrote its file");
    let (context, client) = connect(&cluster.members[1].2);
    // SAFETY: the ABI's conventions, held by this test.
    unsafe {
        // Between two recordings: the file names a live recording the counter does not.
        let mut bytes = vec![0; MAX_LOG_RECORDINGS_LENGTH];
        let length = write_log_recordings(
            &mut bytes,
            member_id,
            live + 1_000,
            0,
            std::iter::empty::<LogRecording>(),
        )
        .expect("the file is written");
        std::fs::write(&file, &bytes[..length]).expect("the file is stored");
        let before = cavalry_errcode();
        let mut log = ptr::null_mut();
        assert_eq!(
            cavalry_committed_log(&mut log, client, 0),
            0,
            "{}",
            errmsg()
        );
        assert!(log.is_null(), "no log between two recordings");
        assert_eq!(cavalry_errcode(), before, "no error is set");

        // A file that fails a check: its magic.
        std::fs::write(&file, vec![0u8; 64]).expect("the file is stored");
        let mut log = ptr::null_mut();
        assert_eq!(cavalry_committed_log(&mut log, client, 0), -1);
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_IO);
        assert_eq!(CAVALRY_ERR_IO, 113);
        assert!(log.is_null(), "no handle for a file that failed");
        let message = errmsg();
        assert!(
            message.contains(&format!("{LOG_RECORDINGS_FILE_PREFIX}{member_id}"))
                && message.contains("recordings-magic"),
            "{message}"
        );

        // The member's own file again: the log opens.
        std::fs::write(&file, &own).expect("the file is stored");
        let mut log = ptr::null_mut();
        assert_eq!(
            cavalry_committed_log(&mut log, client, 0),
            0,
            "{}",
            errmsg()
        );
        assert!(!log.is_null(), "the follower has a recording of the log");
        assert_eq!(cavalry_committed_log_close(log, client), 0, "{}", errmsg());
        assert_eq!(cavalry_close(client), 0);
        assert_eq!(cavalry_context_close(context), 0);
    }
}
