//! A cluster service's snapshot and its start from one through the C ABI, end to end
//! (`spec/c-abi.md`, "Functions", cluster service, "Result codes", "A service's
//! snapshot and its start"; ADR 0146): three members over three drivers on the
//! loopback, a service through the ABI beside each, and a client with a session
//! through the ABI on a fourth driver. The members are `cavalry-raft`'s and run in
//! the test's thread; the leader's own process asks for the snapshot, with
//! `ConsensusModule::snapshot`, as a program that embeds a member does.
//!
//! The services' program is the same beside every member. Its state is how many
//! session messages it has taken and every byte of them, in order, so that a state
//! of three messages of 600 bytes is 1,808 bytes and its snapshot holds the state in
//! two messages of the recording, 1,024 bytes and 784. The program replies to no
//! session: what is under test is what the log's entries make of its state.
//!
//! A member's own snapshot and its services' are taken in order, the members' first:
//! a service's call waits for its driver for up to a second at each step, and with
//! the members' duty cycles on the same thread a member would wait behind it (ADR
//! 0131, Evidence). Outside a test a service's call is on its program's thread.

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
        let path = std::env::temp_dir().join(format!(
            "cavalry-ffi-snapshot-{}-{test}",
            std::process::id()
        ));
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
    // SAFETY: the thread's own buffer, NUL-terminated, valid until the next failing
    // call on this thread.
    unsafe { CStr::from_ptr(cavalry_errmsg()) }
        .to_str()
        .expect("UTF-8")
        .to_owned()
}

fn connect(dir: &Path) -> (*mut cavalry_context_t, *mut cavalry_t) {
    let dir_c = CString::new(dir.to_str().expect("a UTF-8 path")).expect("no NUL");
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

/// How long any wait of this test may last.
const DEADLINE: Duration = Duration::from_secs(60);

/// What the three calls of a snapshot answered from inside the handler, at the
/// leader's request for one: each call's return and the thread's error code after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Inside {
    due: (bool, i32),
    snapshot: (i32, i32),
    restore: (i32, i32),
    /// Whether the refused restore wrote a null state and a length of 0 over the
    /// values the handler put there first.
    restore_cleared: bool,
}

/// What a service's handler is given as its `clientd`: its handle and its client, the
/// program's state, and what it was handed.
struct Served {
    service: *mut cavalry_cluster_service_t,
    client: *mut cavalry_t,
    /// The program's state: the session messages it took, and their bytes in order.
    messages: u64,
    bytes: Vec<u8>,
    /// The type and the position of every entry the handler took.
    handed: Vec<(i32, i64)>,
    /// Whether the last entry taken was the leader's request for a snapshot: the
    /// test polls the service no more until it has given its state.
    at_snapshot: bool,
    inside: Option<Inside>,
}

impl Served {
    fn new(service: *mut cavalry_cluster_service_t, client: *mut cavalry_t) -> Self {
        Self {
            service,
            client,
            messages: 0,
            bytes: Vec::new(),
            handed: Vec::new(),
            at_snapshot: false,
            inside: None,
        }
    }

    /// The program's state as bytes: the count, little-endian, then the messages.
    fn state(&self) -> Vec<u8> {
        let mut state = self.messages.to_le_bytes().to_vec();
        state.extend_from_slice(&(self.bytes.len() as u64).to_le_bytes());
        state.extend_from_slice(&self.bytes);
        state
    }

    /// A program that starts from `state`, as its snapshot held it.
    fn take(&mut self, state: &[u8]) {
        let (count, rest) = state.split_at(8);
        let (length, bytes) = rest.split_at(8);
        self.messages = u64::from_le_bytes(count.try_into().expect("eight bytes"));
        assert_eq!(
            u64::from_le_bytes(length.try_into().expect("eight bytes")),
            bytes.len() as u64
        );
        self.bytes = bytes.to_vec();
    }

    /// How many of the session's messages `payload` the handler took.
    fn took(&self, payload: &[u8]) -> usize {
        self.bytes
            .chunks(payload.len())
            .filter(|chunk| *chunk == payload)
            .count()
    }
}

/// The services' C handler: it applies a session's message to the program's state,
/// and at the leader's request for a snapshot it records what the three calls of a
/// snapshot answer from inside it.
unsafe extern "C" fn serve(
    clientd: *mut c_void,
    buffer: *const u8,
    length: usize,
    header: *const cavalry_header_t,
) {
    // SAFETY: `clientd` is the `Served` its poll was given, which nothing else touches
    // while the poll runs; the buffer and the header are valid for the call.
    unsafe {
        let served = &mut *clientd.cast::<Served>();
        let mut values = std::mem::zeroed::<cavalry_cluster_entry_values_t>();
        assert_eq!(
            cavalry_cluster_entry_values(header, &mut values),
            0,
            "{}",
            errmsg()
        );
        served.handed.push((values.r#type, values.position));
        served.at_snapshot = values.r#type == CAVALRY_CLUSTER_ENTRY_SNAPSHOT;
        if values.r#type == CAVALRY_CLUSTER_ENTRY_SESSION_MESSAGE && values.session_id >= 0 {
            served.messages += 1;
            served
                .bytes
                .extend_from_slice(std::slice::from_raw_parts(buffer, length));
        }
        if values.r#type == CAVALRY_CLUSTER_ENTRY_SNAPSHOT {
            assert_eq!(
                (values.session_id, values.leader_member_id, length),
                (-1, -1, 0),
                "the request carries no session, no leader and no payload"
            );
            let mut position = -7;
            let due = cavalry_cluster_service_snapshot_due(served.service, &mut position);
            let due = (due, cavalry_errcode());
            assert_eq!(position, -7, "a refused read writes nothing");
            let state = served.state();
            let snapshot = cavalry_cluster_service_snapshot(
                served.service,
                served.client,
                state.as_ptr(),
                state.len(),
            );
            let snapshot = (snapshot, cavalry_errcode());
            let mut restored: *const u8 = state.as_ptr();
            let mut restored_length = state.len();
            let restore = cavalry_cluster_service_restore(
                served.service,
                served.client,
                &mut restored,
                &mut restored_length,
            );
            let restore = (restore, cavalry_errcode());
            served.inside = Some(Inside {
                due,
                snapshot,
                restore,
                restore_cleared: restored.is_null() && restored_length == 0,
            });
        }
    }
}

/// One member, its driver, and the service through the ABI beside it.
struct Member {
    id: i32,
    driver: Driver,
    /// `None` once the test has stopped the member while its driver runs.
    module: Option<ConsensusModule>,
    context: *mut cavalry_context_t,
    client: *mut cavalry_t,
    served: Box<Served>,
}

impl Member {
    /// One poll of the member's service: the entries its handler took.
    fn poll(&mut self) -> usize {
        let clientd: *mut Served = &mut *self.served;
        // SAFETY: the ABI's conventions, held by this test; the service and the client
        // are the member's own, and `clientd` is the member's `Served`.
        let taken = unsafe {
            cavalry_cluster_service_poll(
                self.served.service,
                self.client,
                Some(serve),
                clientd.cast(),
                16,
            )
        };
        assert!(
            taken >= 0,
            "member {}'s service polls: {}",
            self.id,
            errmsg()
        );
        taken as usize
    }

    fn module(&self) -> &ConsensusModule {
        self.module.as_ref().expect("the member runs")
    }

    /// The service's answer to `cavalry_cluster_service_snapshot_due`.
    fn due(&self) -> Option<i64> {
        let mut position = -7;
        // SAFETY: the ABI's conventions, held by this test.
        let due =
            unsafe { cavalry_cluster_service_snapshot_due(self.served.service, &mut position) };
        if due {
            Some(position)
        } else {
            assert_eq!(position, -7, "no snapshot due writes nothing");
            None
        }
    }

    /// Gives the program's state to the service's snapshot: what the call returned,
    /// and the thread's error code and message after it.
    fn snapshot(&self) -> (i32, i32, String) {
        let state = self.served.state();
        // SAFETY: the ABI's conventions, held by this test.
        let given = unsafe {
            cavalry_cluster_service_snapshot(
                self.served.service,
                self.client,
                state.as_ptr(),
                state.len(),
            )
        };
        (given, cavalry_errcode(), errmsg())
    }

    /// Asks the service where it starts until the driver hosts a member with a log:
    /// the restored code, and the state's bytes, copied, for `_STATE`.
    fn restore(&self) -> (i32, Option<Vec<u8>>) {
        let deadline = Instant::now() + DEADLINE;
        loop {
            let mut state: *const u8 = ptr::null();
            let mut length = 0;
            // SAFETY: the ABI's conventions, held by this test.
            let restored = unsafe {
                cavalry_cluster_service_restore(
                    self.served.service,
                    self.client,
                    &mut state,
                    &mut length,
                )
            };
            assert!(restored > 0, "the service restores: {}", errmsg());
            if restored == CAVALRY_CLUSTER_RESTORED_NO_MEMBER {
                assert!(Instant::now() < deadline, "a member with a log");
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            if restored != CAVALRY_CLUSTER_RESTORED_STATE {
                assert!(state.is_null() && length == 0);
                return (restored, None);
            }
            assert!(!state.is_null() && length > 0, "a state of some bytes");
            // SAFETY: the handle's bytes, valid until the service's next poll or close.
            let bytes = unsafe { std::slice::from_raw_parts(state, length) }.to_vec();
            return (restored, Some(bytes));
        }
    }
}

/// Three members over three drivers on the loopback, a service beside each, and a
/// session through the ABI on a fourth driver.
struct Cluster {
    members: Vec<Member>,
    members_string: String,
    client_driver: Option<Driver>,
    client_dir: PathBuf,
    response_channel: String,
    /// The session, once it is open: each cycle polls it, so that it sends its
    /// keepalive and the leader does not close it for the session timeout.
    session: Option<Session>,
    _dir: TestDir,
}

impl Cluster {
    fn start(test: &str) -> Self {
        let dir = TestDir::new(test);
        let ports = free_ports(16);
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
                let (context, client) = connect(&driver_dir);
                Member {
                    id,
                    driver,
                    module: Some(module),
                    context,
                    client,
                    served: Box::new(Served::new(new_service(client), client)),
                }
            })
            .collect();
        let client_dir = dir.path().join("client");
        let client_driver =
            Driver::start(&client_dir, dir.path().join("client-log")).expect("the client's driver");
        let mut cluster = Self {
            members,
            members_string,
            client_driver: Some(client_driver),
            client_dir,
            response_channel: format!("cavalry:udp?endpoint=127.0.0.1:{}", ports[15]),
            session: None,
            _dir: dir,
        };
        // No service polls before the test has asked it where it starts: a poll that
        // opens the member's log begins the service.
        cluster.run_polling(
            false,
            "every election closed behind one leader",
            |cluster| {
                cluster
                    .members
                    .iter()
                    .all(|member| member.module().election_state() == ElectionState::Closed)
                    && cluster
                        .members
                        .iter()
                        .filter(|member| member.module().role() == Role::Leader)
                        .count()
                        == 1
            },
        );
        let leader = cluster
            .members
            .iter()
            .position(|member| member.module().role() == Role::Leader)
            .expect("one member leads");
        cluster.members.swap(0, leader);
        cluster
    }

    /// Every running member's duty cycle, then the poll of every service that has no
    /// snapshot to give, when `poll` says so.
    fn cycle(&mut self, poll: bool) -> usize {
        let mut work = 0;
        for member in self.members.iter_mut() {
            if let Some(module) = member.module.as_mut() {
                work += module.do_work().expect("the member's duty cycle");
            }
        }
        if let Some(session) = &self.session {
            session.poll();
        }
        if poll {
            for member in self
                .members
                .iter_mut()
                .filter(|member| member.module.is_some() && !member.served.at_snapshot)
            {
                work += member.poll();
            }
        }
        work
    }

    fn work(&mut self) -> usize {
        self.cycle(true)
    }

    fn run_until(&mut self, what: &str, done: impl FnMut(&Self) -> bool) {
        self.run_polling(true, what, done);
    }

    fn run_polling(&mut self, poll: bool, what: &str, mut done: impl FnMut(&Self) -> bool) {
        let deadline = Instant::now() + DEADLINE;
        while !done(self) {
            assert!(Instant::now() < deadline, "{what}");
            if self.cycle(poll) == 0 {
                std::thread::sleep(Duration::from_micros(200));
            }
        }
    }

    /// Opens the session through the ABI on the fourth driver. The connect blocks until
    /// the leader has opened the session, so a thread of its own makes it while this
    /// one runs the members; the handles cross back as addresses, since a handle may
    /// move between threads between calls (`spec/c-abi.md`, "Threads").
    fn connect_session(&mut self) {
        let dir = self.client_dir.clone();
        let members = CString::new(self.members_string.clone()).expect("no NUL");
        let channel = CString::new(self.response_channel.clone()).expect("no NUL");
        let opening = std::thread::spawn(move || {
            let (context, client) = connect(&dir);
            let mut session = ptr::null_mut();
            // SAFETY: the ABI's conventions, held by this test.
            let opened = unsafe {
                cavalry_connect_cluster(&mut session, client, members.as_ptr(), channel.as_ptr())
            };
            assert_eq!(opened, 0, "{}", errmsg());
            (context as usize, client as usize, session as usize)
        });
        let deadline = Instant::now() + DEADLINE;
        while !opening.is_finished() {
            assert!(Instant::now() < deadline, "the session opened");
            if self.work() == 0 {
                std::thread::sleep(Duration::from_micros(200));
            }
        }
        let (context, client, session) = opening.join().expect("the opening thread ended");
        self.session = Some(Session {
            context: context as *mut cavalry_context_t,
            client: client as *mut cavalry_t,
            session: session as *mut cavalry_cluster_session_t,
        });
    }

    /// Offers `message` on the session, running the members until it is taken.
    fn offer(&mut self, message: &[u8]) {
        let deadline = Instant::now() + DEADLINE;
        loop {
            let session = self.session.as_ref().expect("the session is open").session;
            // SAFETY: the ABI's conventions, held by this test.
            let offered =
                unsafe { cavalry_cluster_session_offer(session, message.as_ptr(), message.len()) };
            if offered >= 0 {
                return;
            }
            assert_ne!(offered, CAVALRY_PUBLICATION_ERROR, "{}", errmsg());
            assert_ne!(offered, CAVALRY_PUBLICATION_CLOSED);
            assert!(Instant::now() < deadline, "the session takes the message");
            self.work();
        }
    }

    /// Closes every service with its client, every client, member and driver.
    fn stop(mut self) {
        if let Some(session) = self.session.take() {
            session.close();
        }
        for member in self.members.drain(..) {
            // SAFETY: the ABI's conventions, held by this test: each handle is live and
            // closed once.
            unsafe {
                assert_eq!(
                    cavalry_cluster_service_close(member.served.service, member.client),
                    0,
                    "{}",
                    errmsg()
                );
                assert_eq!(cavalry_close(member.client), 0);
                assert_eq!(cavalry_context_close(member.context), 0);
            }
            drop(member.module);
            member.driver.close().expect("the driver closes");
        }
        if let Some(driver) = self.client_driver.take() {
            driver.close().expect("the client's driver closes");
        }
    }
}

/// A service through the ABI on `client`.
fn new_service(client: *mut cavalry_t) -> *mut cavalry_cluster_service_t {
    let mut service = ptr::null_mut();
    // SAFETY: the ABI's conventions, held by this test.
    assert_eq!(
        unsafe { cavalry_cluster_service(&mut service, client) },
        0,
        "{}",
        errmsg()
    );
    assert!(!service.is_null());
    service
}

/// A session through the ABI, whose replies the test does not read: the services send
/// none.
struct Session {
    context: *mut cavalry_context_t,
    client: *mut cavalry_t,
    session: *mut cavalry_cluster_session_t,
}

unsafe extern "C" fn no_reply(
    _clientd: *mut c_void,
    _buffer: *const u8,
    _length: usize,
    _header: *const cavalry_header_t,
) {
    panic!("no service of this test replies");
}

impl Session {
    /// The session's duty cycle, which sends its keepalive.
    fn poll(&self) {
        // SAFETY: the ABI's conventions, held by this test.
        let read = unsafe {
            cavalry_cluster_session_poll(self.session, Some(no_reply), ptr::null_mut(), 16)
        };
        assert!(read >= 0, "the session polls: {}", errmsg());
    }

    fn close(self) {
        // SAFETY: the ABI's conventions, held by this test: each handle is live and
        // closed once.
        unsafe {
            assert_eq!(cavalry_cluster_session_close(self.session), 0);
            assert_eq!(cavalry_close(self.client), 0);
            assert_eq!(cavalry_context_close(self.context), 0);
        }
    }
}

/// A service through the ABI takes the snapshot its member lists, and a service that
/// starts again starts from it with the program's state; every refusal on the way is
/// the code the page gives it. One member is stopped while its driver runs, and its
/// service's snapshot is given up at the ack, still due.
#[test]
fn a_service_through_the_abi_takes_a_snapshot_its_member_lists_and_starts_from_it() {
    let mut cluster = Cluster::start("round");

    // Before its first poll: no snapshot is due, a snapshot is refused, and the start
    // finds no snapshot listed. A second start is too late.
    for member in &cluster.members {
        assert_eq!(member.due(), None);
        let (given, code, message) = member.snapshot();
        assert_eq!(
            (given, code),
            (-1, CAVALRY_ERR_CLUSTER_NO_SNAPSHOT_DUE),
            "{message}"
        );
        assert!(
            message.starts_with("cavalry_cluster_service_snapshot refused (no-snapshot-due)")
                && message
                    .ends_with("; see docs/reference/c-abi.md#a-services-snapshot-and-its-start"),
            "{message}"
        );
        assert_eq!(
            member.restore(),
            (CAVALRY_CLUSTER_RESTORED_FROM_START, None)
        );
        let mut state: *const u8 = ptr::null();
        let mut length = 0;
        // SAFETY: the ABI's conventions, held by this test.
        let again = unsafe {
            cavalry_cluster_service_restore(
                member.served.service,
                member.client,
                &mut state,
                &mut length,
            )
        };
        assert_eq!(
            (again, cavalry_errcode()),
            (-1, CAVALRY_ERR_CLUSTER_RESTORE_TOO_LATE),
            "{}",
            errmsg()
        );
        assert!(errmsg().starts_with("cavalry_cluster_service_restore refused (restore-too-late)"));
    }

    // Three messages of 600 bytes, which every service takes.
    cluster.connect_session();
    let messages: Vec<Vec<u8>> = (b'a'..=b'c').map(|byte| vec![byte; 600]).collect();
    for message in &messages {
        cluster.offer(message);
    }
    cluster.run_until("every service took the three messages", |cluster| {
        cluster.members.iter().all(|member| {
            messages
                .iter()
                .all(|message| member.served.took(message) == 1)
        })
    });

    // The leader's own process asks for a snapshot. Every service's poll ends at the
    // request, and the test polls it no more until it has given its state.
    cluster.members[0]
        .module
        .as_mut()
        .expect("the leader runs")
        .snapshot()
        .expect("the leader appends the request");
    cluster.run_until("every service was handed the request", |cluster| {
        cluster
            .members
            .iter()
            .all(|member| member.served.at_snapshot)
    });
    let position = cluster.members[0].due().expect("a snapshot is due");
    cluster.run_polling(false, "every member listed its own snapshot", |cluster| {
        cluster
            .members
            .iter()
            .all(|member| member.module().snapshot_position() == Some(position))
    });
    for member in &cluster.members {
        // SAFETY: the ABI's conventions, held by this test.
        let at = unsafe { cavalry_cluster_service_position(member.served.service) };
        assert_eq!(
            (member.due(), at),
            (Some(position), position),
            "member {}'s service is due at its position, the member's own snapshot's",
            member.id
        );
        assert!(
            matches!(
                member.served.handed.last(),
                Some(&(CAVALRY_CLUSTER_ENTRY_SNAPSHOT, at)) if at < position
            ),
            "the request is the last entry taken, and the snapshot is of the position \
             after it: {:?}",
            member.served.handed.last()
        );
        assert_eq!(member.served.state().len(), 16 + 1_800);
        // From inside the handler, at the request, each of the three calls was refused
        // with the poll's code, and the refused start cleared what it was given.
        assert_eq!(
            member.served.inside,
            Some(Inside {
                due: (false, CAVALRY_ERR_POLLING),
                snapshot: (-1, CAVALRY_ERR_POLLING),
                restore: (-1, CAVALRY_ERR_POLLING),
                restore_cleared: true,
            })
        );
    }

    // A state over 1 GiB is refused before a byte of it is read, and the snapshot stays
    // due; a null state with a length is refused as a null.
    let leader = &cluster.members[0];
    let over = (1usize << 30) + 1;
    // SAFETY: the page says the call reads no byte of a state over the bound, so a
    // pointer to no such buffer is never read.
    let given = unsafe {
        cavalry_cluster_service_snapshot(
            leader.served.service,
            leader.client,
            ptr::NonNull::<u8>::dangling().as_ptr(),
            over,
        )
    };
    assert_eq!(
        (given, cavalry_errcode()),
        (-1, CAVALRY_ERR_CLUSTER_SNAPSHOT_TOO_LONG)
    );
    assert!(
        errmsg().contains("the state is 1073741825 bytes and one snapshot holds 1073741824"),
        "{}",
        errmsg()
    );
    // SAFETY: the ABI's conventions, held by this test.
    let given = unsafe {
        cavalry_cluster_service_snapshot(leader.served.service, leader.client, ptr::null(), 5)
    };
    assert_eq!((given, cavalry_errcode()), (-1, CAVALRY_ERR_NULL));
    assert_eq!(
        leader.due(),
        Some(position),
        "a refusal leaves the snapshot due"
    );

    // A member stopped while its driver runs: its service writes its recording and
    // gives the snapshot up at the ack, after the snapshot timeout, still due.
    let stopped = 1;
    drop(cluster.members[stopped].module.take());
    let (given, code, message) = cluster.members[stopped].snapshot();
    assert_eq!(
        (given, code),
        (-1, CAVALRY_ERR_CLUSTER_SNAPSHOT_FAILED),
        "{message}"
    );
    assert!(
        message.starts_with("cluster service snapshot failed (snapshot-failed, ack): ")
            && message.contains("check that the cluster member runs on this driver"),
        "{message}"
    );
    assert_eq!(cluster.members[stopped].due(), Some(position));

    // The two services beside running members give their state, and each member lists
    // its service's snapshot. The snapshot is no longer due, and a second is refused.
    let given_state = cluster.members[0].served.state();
    for index in [0, 2] {
        let member = &cluster.members[index];
        assert_eq!(
            member.served.state(),
            given_state,
            "one state beside every member"
        );
        let (given, _, message) = member.snapshot();
        assert_eq!(
            given, 1,
            "member {}'s service writes its snapshot: {message}",
            member.id
        );
        assert_eq!(member.due(), None);
        let (again, code, _) = member.snapshot();
        assert_eq!((again, code), (-1, CAVALRY_ERR_CLUSTER_NO_SNAPSHOT_DUE));
    }
    cluster.run_polling(
        false,
        "both members listed their service's snapshot",
        |cluster| {
            [0, 2].iter().all(|index| {
                cluster.members[*index].module().service_snapshot_position() == Some(position)
            })
        },
    );

    // The services read on: a fourth message.
    for index in [0, 2] {
        cluster.members[index].served.at_snapshot = false;
    }
    let after = vec![b'd'; 600];
    cluster.offer(&after);
    cluster.run_until("the running services took the fourth message", |cluster| {
        [0, 2]
            .iter()
            .all(|index| cluster.members[*index].served.took(&after) == 1)
    });

    // The service beside the follower that runs starts again: a new service on the
    // same client starts from its snapshot, with the program's state as it gave it,
    // the session open, and the snapshot's position. It reads the fourth message and
    // nothing below, and its program holds what the leader's does.
    let restarted = &mut cluster.members[2];
    // SAFETY: the ABI's conventions, held by this test: the service is live and
    // closed once.
    assert_eq!(
        unsafe { cavalry_cluster_service_close(restarted.served.service, restarted.client) },
        0,
        "{}",
        errmsg()
    );
    *restarted.served = Served::new(new_service(restarted.client), restarted.client);
    let (restored, state) = restarted.restore();
    assert_eq!(restored, CAVALRY_CLUSTER_RESTORED_STATE);
    let state = state.expect("the state");
    assert_eq!(state, given_state, "the program's state as it gave it");
    restarted.served.take(&state);
    // SAFETY: the ABI's conventions, held by this test.
    unsafe {
        assert_eq!(
            cavalry_cluster_service_position(restarted.served.service),
            position
        );
        assert_eq!(
            cavalry_cluster_service_sessions(restarted.served.service),
            1
        );
    }
    let mut ignored: *const u8 = ptr::null();
    let mut ignored_length = 0;
    // SAFETY: the ABI's conventions, held by this test.
    let again = unsafe {
        cavalry_cluster_service_restore(
            restarted.served.service,
            restarted.client,
            &mut ignored,
            &mut ignored_length,
        )
    };
    assert_eq!(
        (again, cavalry_errcode()),
        (-1, CAVALRY_ERR_CLUSTER_RESTORE_TOO_LATE)
    );
    cluster.run_until("the restarted service took the fourth message", |cluster| {
        cluster.members[2].served.took(&after) == 1
    });
    let restarted = &cluster.members[2].served;
    assert!(
        restarted.handed.iter().all(|(_, at)| *at >= position),
        "no entry below the snapshot: {:?}",
        restarted.handed
    );
    assert_eq!(restarted.state(), cluster.members[0].served.state());

    cluster.stop();
}

/// Sets the thread's error to `CAVALRY_ERR_ABI_VERSION`, 135, a code none of the three
/// calls sets, so that the code after a call says whether that call set one.
fn sentinel() {
    let mut context = ptr::null_mut();
    // SAFETY: a version that is not the library's is refused before anything is made.
    assert_eq!(unsafe { cavalry_context_init_v(&mut context, -1) }, -1);
    assert_eq!(cavalry_errcode(), CAVALRY_ERR_ABI_VERSION);
}

/// Beside a driver that hosts no cluster member, a service has nothing to start from
/// and no snapshot due, and says so as often as it is asked: a poll that opens no log
/// begins nothing. Each of the three calls refuses a null handle or pointer with
/// `CAVALRY_ERR_NULL`, and a refused start writes a null state and a length of 0.
#[test]
fn a_service_beside_no_member_has_nothing_to_start_from_and_every_call_refuses_a_null() {
    let dir = TestDir::new("alone");
    let driver_dir = dir.path().join("driver");
    let driver = Driver::start(&driver_dir, dir.path().join("log")).expect("the driver starts");
    let (context, client) = connect(&driver_dir);
    let service = new_service(client);
    let bytes = [7u8; 4];
    // SAFETY: the ABI's conventions, held by this test.
    unsafe {
        // Nulls, each refused before anything is read or sent.
        let mut position = -7;
        sentinel();
        assert!(!cavalry_cluster_service_snapshot_due(
            ptr::null(),
            &mut position
        ));
        assert_eq!((cavalry_errcode(), position), (CAVALRY_ERR_NULL, -7));
        sentinel();
        assert!(!cavalry_cluster_service_snapshot_due(
            service,
            ptr::null_mut()
        ));
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_NULL);
        for (service, client) in [(ptr::null_mut(), client), (service, ptr::null_mut())] {
            sentinel();
            assert_eq!(
                cavalry_cluster_service_snapshot(service, client, bytes.as_ptr(), bytes.len()),
                -1
            );
            assert_eq!(cavalry_errcode(), CAVALRY_ERR_NULL);
            let mut state: *const u8 = bytes.as_ptr();
            let mut length = bytes.len();
            sentinel();
            assert_eq!(
                cavalry_cluster_service_restore(service, client, &mut state, &mut length),
                -1
            );
            assert_eq!(cavalry_errcode(), CAVALRY_ERR_NULL);
            assert!(
                state.is_null() && length == 0,
                "a refused start clears both"
            );
        }
        let mut state: *const u8 = bytes.as_ptr();
        let mut length = bytes.len();
        sentinel();
        assert_eq!(
            cavalry_cluster_service_restore(service, client, ptr::null_mut(), &mut length),
            -1
        );
        assert_eq!((cavalry_errcode(), length), (CAVALRY_ERR_NULL, 0));
        sentinel();
        assert_eq!(
            cavalry_cluster_service_restore(service, client, &mut state, ptr::null_mut()),
            -1
        );
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_NULL);
        assert!(state.is_null());

        // No member: nothing to start from, asked twice, with a poll between that reads
        // nothing and begins nothing; no snapshot due, and a snapshot refused.
        for _ in 0..2 {
            let mut state: *const u8 = bytes.as_ptr();
            let mut length = bytes.len();
            sentinel();
            assert_eq!(
                cavalry_cluster_service_restore(service, client, &mut state, &mut length),
                CAVALRY_CLUSTER_RESTORED_NO_MEMBER,
                "{}",
                errmsg()
            );
            assert_eq!(
                cavalry_errcode(),
                CAVALRY_ERR_ABI_VERSION,
                "an answer sets no error"
            );
            assert!(state.is_null() && length == 0);
            assert_eq!(
                cavalry_cluster_service_poll(service, client, Some(no_reply), ptr::null_mut(), 16),
                0,
                "{}",
                errmsg()
            );
        }
        let mut position = -7;
        sentinel();
        assert!(!cavalry_cluster_service_snapshot_due(
            service,
            &mut position
        ));
        assert_eq!(
            (cavalry_errcode(), position),
            (CAVALRY_ERR_ABI_VERSION, -7),
            "no snapshot due is an answer, with nothing written"
        );
        // A state of no bytes at a null pointer is a state, refused here only because
        // none is due.
        assert_eq!(
            cavalry_cluster_service_snapshot(service, client, ptr::null(), 0),
            -1
        );
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_CLUSTER_NO_SNAPSHOT_DUE);

        assert_eq!(
            cavalry_cluster_service_close(service, client),
            0,
            "{}",
            errmsg()
        );
        assert_eq!(cavalry_close(client), 0);
        assert_eq!(cavalry_context_close(context), 0);
    }
    driver.close().expect("the driver closes");
}
