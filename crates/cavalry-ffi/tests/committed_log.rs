//! The committed log through the ABI (`spec/c-abi.md`, "Functions", committed log;
//! ADR 0070): a driver that hosts no member answers the open with a null handle and
//! no error; beside a cluster member in the test's process, the C handler is handed
//! the entries a quorum committed and none past the commit position, each with its
//! position; the calls from inside the handler answer as the page's table says; a
//! subscription's header carries no position; every null is refused; and a service
//! whose replay the driver withdrew before it had read it is told where the entries it
//! was not given begin.

use std::ffi::{CStr, CString, c_void};
use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicU32, Ordering};
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

/// Loopback ports for a cluster's endpoints, from 20000 to 31999, as `cavalry-raft`'s
/// `tests/cluster.rs` takes its own and for its reason: below the range the operating
/// system gives a socket bound to port 0, which starts at 32768 on Linux and at 49152
/// on macOS. A follower binds its `log` endpoint only when it joins the log, and
/// meanwhile every publication of the drivers in this process has a send socket bound
/// to port 0, which could be given a port that was chosen by binding port 0 and
/// releasing it: the member's bind then fails with `Address already in use`, and it
/// never joins. Each port is free when it is chosen, and each is chosen once in this
/// process, so that tests that run at once never share one. The process id picks
/// where the choice starts, so that two test processes on one host start apart.
fn free_ports(count: usize) -> Vec<u16> {
    const FIRST: u32 = 20_000;
    const SPAN: u32 = 12_000;
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let start = (std::process::id() % 60) * 200;
    let mut sockets = Vec::with_capacity(count);
    for _ in 0..SPAN {
        if sockets.len() == count {
            break;
        }
        let offset = (start + NEXT.fetch_add(1, Ordering::Relaxed)) % SPAN;
        let port = u16::try_from(FIRST + offset).expect("below 32000");
        if let Ok(socket) = UdpSocket::bind(("127.0.0.1", port)) {
            sockets.push(socket);
        }
    }
    assert_eq!(
        sockets.len(),
        count,
        "free loopback ports from {FIRST} to {}",
        FIRST + SPAN - 1
    );
    sockets
        .iter()
        .map(|socket| socket.local_addr().expect("bound").port())
        .collect()
}

/// Three members over three drivers on the loopback, as `cavalry-raft`'s own cluster
/// test starts them; member 0 is the appointed leader.
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
        let members = (0..3)
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
        assert_eq!(cluster.members[0].1.role(), Role::Leader);
        cluster
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

    /// Runs the members until every one has committed up to `position`.
    fn commit(&mut self, position: i64) {
        self.run_until("every member committed", |cluster| {
            cluster
                .members
                .iter()
                .all(|(_, module, _)| module.commit_position() == position)
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
        answers.push((
            "close",
            i64::from(cavalry_committed_log_close(log, client)),
            cavalry_errcode(),
        ));
        let mut constants = std::mem::zeroed::<cavalry_committed_log_constants_t>();
        let filled = cavalry_committed_log_constants(log, &mut constants);
        assert_eq!((constants.member_id, constants.recording_id), (1, 0));
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

/// A service through the ABI beside a follower, member 1: a hundred entries committed
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
        assert!(!log.is_null(), "member 1 has a recording of the log");
        let mut constants = std::mem::zeroed::<cavalry_committed_log_constants_t>();
        assert_eq!(cavalry_committed_log_constants(log, &mut constants), 0);
        assert_eq!((constants.member_id, constants.recording_id), (1, 0));
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
    let (context, client) = connect(&cluster.members[2].2);
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

        // Follower 2's consensus module closes. Its recording stops, the replay ends,
        // and the service does not read: a poll with a fragment limit of 0 applies the
        // events the subscription has and reads no frame, so the handle is ended only
        // when the driver has withdrawn the image, the linger after the replay's end.
        let (driver, module, _) = cluster.members.remove(2);
        module.close().expect("the member closes");
        let closed = Instant::now();
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
