//! A cluster's service through the C ABI, end to end (`spec/c-abi.md`, "Functions",
//! cluster service, "Result codes", "Calls during a claim or a poll", "What a header
//! carries"; ADR 0123): three members over three drivers on the loopback, a service
//! through the ABI beside each, and a client with a session through the ABI on a
//! fourth driver. Nothing of the cluster protocol is written by hand here: the
//! members are `cavalry-raft`'s, and the session and the services are the ABI's.
//!
//! Every service runs one C handler, which answers a session message with the id of
//! its own member, a colon and the message, so that the reply a client reads says
//! which member's service sent it. The members and the services run in the test's
//! thread. The session's connect blocks while the members answer it, so a thread of
//! its own makes it and has ended before the test goes on.

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
            std::env::temp_dir().join(format!("cavalry-ffi-service-{}-{test}", std::process::id()));
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

/// One entry as a service's handler was handed it, and what the handler did with it.
#[derive(Clone, Debug, PartialEq)]
struct Handed {
    position: i64,
    term: i64,
    timestamp: i64,
    session: i64,
    r#type: i32,
    leader: i32,
    timed_out: bool,
    payload: Vec<u8>,
    /// What `cavalry_committed_log_entry_position` said of the same header.
    entry_position: i64,
    /// What `cavalry_cluster_service_reply` answered, for a session's message.
    reply: Option<i32>,
    /// What the handler said it does with the entry.
    action: i32,
}

/// One call made from inside the handler: its name, what it returned, and the
/// thread's error code after it. Before each call the thread's error is set to
/// `CAVALRY_ERR_NULL`, 131, by a call that fails so, so a 131 here says the call set
/// no error.
type Probed = (&'static str, i64, i32);

/// The other handles a probe calls from inside the handler, all made with the
/// service's own client before the poll.
struct Others {
    /// A second service and a third, which never poll.
    second: *mut cavalry_cluster_service_t,
    third: *mut cavalry_cluster_service_t,
    /// A committed log of the same member.
    log: *mut cavalry_committed_log_t,
    /// A publication on `cavalry:ipc`.
    publication: *mut cavalry_publication_t,
    dir: CString,
    members: CString,
}

/// What a service's handler is given as its `clientd`: its handles, its script, and
/// what it was handed.
struct Served {
    member: i32,
    service: *mut cavalry_cluster_service_t,
    client: *mut cavalry_t,
    handed: Vec<Handed>,
    /// How many handlings of a message that begins `again` are still to be aborted,
    /// each after its reply.
    aborts: u32,
    /// Whether the handler breaks at the message `one`.
    breaks: bool,
    /// The handles the handler probes with at the message `probe`, and what the
    /// probe found.
    others: Option<Others>,
    probed: Vec<Probed>,
    /// The message of the thread's error after the reply that was too long.
    too_long: String,
}

/// Sets the thread's error to `CAVALRY_ERR_NULL`, runs `call`, and gives what it
/// returned with the thread's error code after it.
fn probe(name: &'static str, call: impl FnOnce() -> i64) -> Probed {
    // SAFETY: a null context is refused with `CAVALRY_ERR_NULL`, and nothing else.
    assert_eq!(unsafe { cavalry_context_close(ptr::null_mut()) }, -1);
    let returned = call();
    (name, returned, cavalry_errcode())
}

/// Every call the page refuses or allows from inside a service's handler, made from
/// inside one: on the header, on the service, on the client the poll holds, and on
/// other handles made with that client.
///
/// # Safety
///
/// Called from inside `served`'s handler, with `header` the handler's.
unsafe fn probe_from_inside(served: &mut Served, header: *const cavalry_header_t, position: i64) {
    let (service, client) = (served.service, served.client);
    let session = served_session(served);
    // The other handles as addresses, so that nothing borrows `served` below.
    let others = served
        .others
        .as_ref()
        .expect("the probing service's handles");
    let (second, third, log, publication) =
        (others.second, others.third, others.log, others.publication);
    let (dir, members) = (others.dir.as_ptr(), others.members.as_ptr());
    let channel = c"cavalry:ipc";
    let mut found = Vec::new();
    // SAFETY: the ABI's conventions, held by this test: every handle is live, and each
    // call below is one the page defines from inside a service's handler.
    unsafe {
        // The header: an entry's values and its position, and no frame, no reply.
        found.push(probe("cavalry_header_values", || {
            let mut values = std::mem::zeroed::<cavalry_header_values_t>();
            i64::from(cavalry_header_values(header, &mut values))
        }));
        found.push(probe("cavalry_cluster_reply_values", || {
            let mut values = std::mem::zeroed::<cavalry_cluster_reply_values_t>();
            i64::from(cavalry_cluster_reply_values(header, &mut values))
        }));
        found.push(probe("cavalry_committed_log_entry_position", || {
            i64::from(cavalry_committed_log_entry_position(header) == position)
        }));
        // The service's own functions: the poll holds the service.
        found.push(probe("cavalry_cluster_service_poll", || {
            i64::from(cavalry_cluster_service_poll(
                service,
                client,
                Some(serve),
                ptr::null_mut(),
                1,
            ))
        }));
        found.push(probe("cavalry_cluster_service_fault", || {
            let mut fault = std::mem::zeroed::<cavalry_fault_t>();
            i64::from(cavalry_cluster_service_fault(service, &mut fault))
        }));
        found.push(probe("cavalry_cluster_service_position", || {
            cavalry_cluster_service_position(service)
        }));
        found.push(probe("cavalry_cluster_service_answers", || {
            i64::from(cavalry_cluster_service_answers(service))
        }));
        found.push(probe("cavalry_cluster_service_sessions", || {
            i64::from(cavalry_cluster_service_sessions(service))
        }));
        found.push(probe("cavalry_cluster_service_close", || {
            i64::from(cavalry_cluster_service_close(service, client))
        }));
        found.push(probe("cavalry_cluster_service_close, no client", || {
            i64::from(cavalry_cluster_service_close(service, ptr::null_mut()))
        }));
        // The client: the poll holds it, and what its handle copied answers.
        found.push(probe("cavalry_client_id", || {
            i64::from(cavalry_client_id(client) >= 0)
        }));
        found.push(probe("cavalry_dir", || {
            let answered = cavalry_dir(client);
            i64::from(!answered.is_null() && CStr::from_ptr(answered) == CStr::from_ptr(dir))
        }));
        found.push(probe("cavalry_is_closed", || {
            i64::from(cavalry_is_closed(client))
        }));
        found.push(probe("cavalry_closed", || {
            i64::from(cavalry_closed(client))
        }));
        found.push(probe("cavalry_close", || i64::from(cavalry_close(client))));
        found.push(probe("cavalry_add_publication", || {
            let mut added = ptr::null_mut();
            let answer = cavalry_add_publication(&mut added, client, channel.as_ptr(), 2002);
            assert!(added.is_null(), "a refused add gives no handle");
            i64::from(answer)
        }));
        found.push(probe("cavalry_add_exclusive_publication", || {
            let mut added = ptr::null_mut();
            let answer =
                cavalry_add_exclusive_publication(&mut added, client, channel.as_ptr(), 2002);
            assert!(added.is_null(), "a refused add gives no handle");
            i64::from(answer)
        }));
        found.push(probe("cavalry_add_subscription", || {
            let mut added = ptr::null_mut();
            let answer = cavalry_add_subscription(&mut added, client, channel.as_ptr(), 2002);
            assert!(added.is_null(), "a refused add gives no handle");
            i64::from(answer)
        }));
        found.push(probe("cavalry_start_recording", || {
            let mut started = ptr::null_mut();
            let answer = cavalry_start_recording(&mut started, client, channel.as_ptr(), 2002);
            assert!(started.is_null(), "a refused start gives no handle");
            i64::from(answer)
        }));
        found.push(probe("cavalry_start_replay", || {
            let mut started = ptr::null_mut();
            let answer = cavalry_start_replay(
                &mut started,
                client,
                0,
                CAVALRY_NULL_POSITION,
                CAVALRY_NULL_LENGTH,
                2003,
                channel.as_ptr(),
            );
            assert!(started.is_null(), "a refused start gives no handle");
            i64::from(answer)
        }));
        found.push(probe("cavalry_stop_recording", || {
            i64::from(cavalry_stop_recording(client, 0))
        }));
        found.push(probe("cavalry_truncate_recording", || {
            i64::from(cavalry_truncate_recording(client, 0, 0))
        }));
        found.push(probe("cavalry_purge_segments", || {
            i64::from(cavalry_purge_segments(client, 0, 0))
        }));
        found.push(probe("cavalry_purge_recording", || {
            i64::from(cavalry_purge_recording(client, 0))
        }));
        found.push(probe("cavalry_stop_replay", || {
            i64::from(cavalry_stop_replay(client, 0))
        }));
        found.push(probe("cavalry_add_destination", || {
            i64::from(cavalry_add_destination(
                client,
                publication,
                channel.as_ptr(),
            ))
        }));
        found.push(probe("cavalry_remove_destination", || {
            i64::from(cavalry_remove_destination(
                client,
                publication,
                channel.as_ptr(),
            ))
        }));
        found.push(probe("cavalry_committed_log", || {
            let mut log = ptr::null_mut();
            let answer = cavalry_committed_log(&mut log, client, 0);
            assert!(log.is_null(), "a refused open gives no handle");
            i64::from(answer)
        }));
        found.push(probe("cavalry_committed_log_close, another log", || {
            i64::from(cavalry_committed_log_close(log, client))
        }));
        found.push(probe("cavalry_connect_cluster", || {
            let mut session = ptr::null_mut();
            let answer = cavalry_connect_cluster(
                &mut session,
                client,
                members,
                c"cavalry:udp?endpoint=127.0.0.1:9".as_ptr(),
            );
            assert!(session.is_null(), "a refused connect gives no handle");
            i64::from(answer)
        }));
        found.push(probe("cavalry_cluster_service", || {
            let mut made = ptr::null_mut();
            let answer = cavalry_cluster_service(&mut made, client);
            assert!(made.is_null(), "a refused service gives no handle");
            i64::from(answer)
        }));
        // Another service made with the client the poll holds: its poll and its close
        // with that client are refused, and it handles no entry.
        found.push(probe(
            "cavalry_cluster_service_poll, another service",
            || {
                i64::from(cavalry_cluster_service_poll(
                    second,
                    client,
                    Some(serve),
                    ptr::null_mut(),
                    1,
                ))
            },
        ));
        found.push(probe(
            "cavalry_cluster_service_close, another service",
            || i64::from(cavalry_cluster_service_close(second, client)),
        ));
        found.push(probe(
            "cavalry_cluster_service_reply, another service",
            || i64::from(cavalry_cluster_service_reply(second, 1, b"x".as_ptr(), 1)),
        ));
        found.push(probe(
            "cavalry_cluster_service_action, another service",
            || i64::from(cavalry_cluster_service_action(second, CAVALRY_ACTION_ABORT)),
        ));
        // What does not reach the client is allowed: a close with no client, and any
        // call on another handle.
        found.push(probe(
            "cavalry_cluster_service_close, another service, no client",
            || i64::from(cavalry_cluster_service_close(third, ptr::null_mut())),
        ));
        found.push(probe("cavalry_publication_offer", || {
            let offered = cavalry_publication_offer(publication, b"x".as_ptr(), 1);
            i64::from(offered != CAVALRY_PUBLICATION_ERROR)
        }));
        // The handler's two calls: a reply that is too long, a reply to no session,
        // an action that is none, and the last action as the one that counts.
        let long = [7u8; 1_345];
        found.push(probe("cavalry_cluster_service_reply, 1,345 bytes", || {
            i64::from(cavalry_cluster_service_reply(
                service,
                session,
                long.as_ptr(),
                long.len(),
            ))
        }));
        served.too_long = errmsg();
        found.push(probe("cavalry_cluster_service_reply, 1,344 bytes", || {
            i64::from(cavalry_cluster_service_reply(
                service,
                session,
                long.as_ptr(),
                1_344,
            ))
        }));
        found.push(probe("cavalry_cluster_service_reply, no session", || {
            i64::from(cavalry_cluster_service_reply(
                service,
                999,
                b"x".as_ptr(),
                1,
            ))
        }));
        found.push(probe("cavalry_cluster_service_reply, null buffer", || {
            i64::from(cavalry_cluster_service_reply(
                service,
                session,
                ptr::null(),
                1,
            ))
        }));
        found.push(probe("cavalry_cluster_service_action, 3", || {
            i64::from(cavalry_cluster_service_action(service, 3))
        }));
        found.push(probe("cavalry_cluster_service_action, 0", || {
            i64::from(cavalry_cluster_service_action(service, 0))
        }));
        found.push(probe("cavalry_cluster_service_action, abort", || {
            i64::from(cavalry_cluster_service_action(
                service,
                CAVALRY_ACTION_ABORT,
            ))
        }));
        found.push(probe("cavalry_cluster_service_action, continue", || {
            i64::from(cavalry_cluster_service_action(
                service,
                CAVALRY_ACTION_CONTINUE,
            ))
        }));
    }
    served.probed = found;
}

/// The session of the last session message the service was handed.
fn served_session(served: &Served) -> i64 {
    served
        .handed
        .iter()
        .rev()
        .find(|handed| handed.r#type == CAVALRY_CLUSTER_ENTRY_SESSION_MESSAGE)
        .map_or(-1, |handed| handed.session)
}

/// The services' C handler. It reads the entry's values, answers a session's message
/// with its member's id, a colon and the message, aborts on a reply that asks for it,
/// and does what its script says for the messages `again`, `one` and `probe`.
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
        assert!(!buffer.is_null(), "the buffer is a pointer for every entry");
        let payload = std::slice::from_raw_parts(buffer, length).to_vec();
        let mut handed = Handed {
            position: values.position,
            term: values.leadership_term_id,
            timestamp: values.timestamp,
            session: values.session_id,
            r#type: values.r#type,
            leader: values.leader_member_id,
            timed_out: values.timed_out,
            payload: payload.clone(),
            entry_position: cavalry_committed_log_entry_position(header),
            reply: None,
            action: CAVALRY_ACTION_CONTINUE,
        };
        if values.r#type == CAVALRY_CLUSTER_ENTRY_SESSION_MESSAGE && values.session_id >= 0 {
            let mut answer = [0u8; 64];
            answer[0] = b'0' + served.member as u8;
            answer[1] = b':';
            answer[2..2 + payload.len()].copy_from_slice(&payload);
            let reply = cavalry_cluster_service_reply(
                served.service,
                values.session_id,
                answer.as_ptr(),
                2 + payload.len(),
            );
            handed.reply = Some(reply);
            if reply == CAVALRY_CLUSTER_REPLY_RETRY {
                handed.action = CAVALRY_ACTION_ABORT;
            } else if payload.starts_with(b"again") && served.aborts > 0 {
                served.aborts -= 1;
                handed.action = CAVALRY_ACTION_ABORT;
            } else if payload == b"one" && served.breaks {
                handed.action = CAVALRY_ACTION_BREAK;
            }
            if handed.action != CAVALRY_ACTION_CONTINUE {
                assert_eq!(
                    cavalry_cluster_service_action(served.service, handed.action),
                    0,
                    "{}",
                    errmsg()
                );
            }
        }
        served.handed.push(handed);
        if payload == b"probe" && served.others.is_some() {
            probe_from_inside(served, header, values.position);
        }
    }
}

/// One member, its driver, and the service through the ABI beside it.
struct Member {
    id: i32,
    driver: Driver,
    module: ConsensusModule,
    context: *mut cavalry_context_t,
    client: *mut cavalry_t,
    served: Box<Served>,
    /// Whether `Cluster::work` polls the member's service.
    polled: bool,
}

impl Member {
    /// One poll of the member's service: the entries its handler took.
    fn poll(&mut self, fragment_limit: usize) -> usize {
        let service = self.served.service;
        let clientd: *mut Served = &mut *self.served;
        // SAFETY: the ABI's conventions, held by this test; the service and the client
        // are the member's own, and `clientd` is the member's `Served`.
        let taken = unsafe {
            cavalry_cluster_service_poll(
                service,
                self.client,
                Some(serve),
                clientd.cast(),
                fragment_limit,
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

    /// What the service did with the session message `payload`: every handling.
    fn took(&self, payload: &[u8]) -> Vec<&Handed> {
        self.served
            .handed
            .iter()
            .filter(|handed| {
                handed.r#type == CAVALRY_CLUSTER_ENTRY_SESSION_MESSAGE && handed.payload == payload
            })
            .collect()
    }
}

/// Three members over three drivers on the loopback, a service beside each, and the
/// directory of a fourth driver for a client.
struct Cluster {
    members: Vec<Member>,
    members_string: String,
    client_driver: Option<Driver>,
    client_dir: PathBuf,
    response_channel: String,
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
                let mut service = ptr::null_mut();
                // SAFETY: the ABI's conventions, held by this test.
                assert_eq!(
                    unsafe { cavalry_cluster_service(&mut service, client) },
                    0,
                    "{}",
                    errmsg()
                );
                assert!(!service.is_null());
                Member {
                    id,
                    driver,
                    module,
                    context,
                    client,
                    served: Box::new(Served {
                        member: id,
                        service,
                        client,
                        handed: Vec::new(),
                        aborts: 0,
                        breaks: false,
                        others: None,
                        probed: Vec::new(),
                        too_long: String::new(),
                    }),
                    polled: true,
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
            _dir: dir,
        };
        cluster.run_until("every election closed behind one leader", |cluster| {
            cluster
                .members
                .iter()
                .all(|member| member.module.election_state() == ElectionState::Closed)
                && cluster
                    .members
                    .iter()
                    .filter(|member| member.module.role() == Role::Leader)
                    .count()
                    == 1
        });
        let leader = cluster
            .members
            .iter()
            .position(|member| member.module.role() == Role::Leader)
            .expect("one member leads");
        cluster.members.swap(0, leader);
        cluster
    }

    /// Every member's duty cycle, then every polled service's poll.
    fn work(&mut self) -> usize {
        let mut work = 0;
        for member in self.members.iter_mut() {
            work += member.module.do_work().expect("the member's duty cycle");
        }
        for member in self.members.iter_mut().filter(|member| member.polled) {
            work += member.poll(16);
        }
        work
    }

    fn run_until(&mut self, what: &str, mut done: impl FnMut(&Self) -> bool) {
        let deadline = Instant::now() + DEADLINE;
        while !done(self) {
            assert!(Instant::now() < deadline, "{what}");
            if self.work() == 0 {
                std::thread::sleep(Duration::from_micros(200));
            }
        }
    }

    /// Polls the leader's service until a poll takes an entry, and gives how many
    /// that poll took.
    fn poll_leader(&mut self) -> usize {
        let deadline = Instant::now() + DEADLINE;
        loop {
            let taken = self.members[0].poll(16);
            if taken > 0 {
                return taken;
            }
            assert!(
                Instant::now() < deadline,
                "the leader's service takes an entry"
            );
            self.work();
        }
    }

    /// Opens a session through the ABI on the fourth driver. The connect blocks until
    /// the leader has opened the session, so a thread of its own makes it while this
    /// one runs the members; the handles cross back as addresses, since a handle may
    /// move between threads between calls (`spec/c-abi.md`, "Threads").
    fn connect_session(&mut self) -> Session {
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
        let session = session as *mut cavalry_cluster_session_t;
        // SAFETY: the ABI's conventions, held by this test.
        let id = unsafe {
            let mut constants = std::mem::zeroed::<cavalry_cluster_session_constants_t>();
            assert_eq!(
                cavalry_cluster_session_constants(session, &mut constants),
                0
            );
            constants.session_id
        };
        Session {
            context: context as *mut cavalry_context_t,
            client: client as *mut cavalry_t,
            session,
            id,
            replies: Vec::new(),
        }
    }

    /// Runs the members, the polled services and the session's poll until `done`.
    fn converse(
        &mut self,
        session: &mut Session,
        what: &str,
        mut done: impl FnMut(&Self, &Session) -> bool,
    ) {
        let deadline = Instant::now() + DEADLINE;
        while !done(self, session) {
            assert!(
                Instant::now() < deadline,
                "{what}: replies {:?}",
                session.replies
            );
            let work = self.work() + session.poll();
            if work == 0 {
                std::thread::sleep(Duration::from_micros(200));
            }
        }
    }

    /// Offers `message` on the session, running the members until it is taken.
    fn offer(&mut self, session: &mut Session, message: &[u8]) {
        let deadline = Instant::now() + DEADLINE;
        loop {
            // SAFETY: the ABI's conventions, held by this test.
            let offered = unsafe {
                cavalry_cluster_session_offer(session.session, message.as_ptr(), message.len())
            };
            if offered >= 0 {
                return;
            }
            assert_ne!(offered, CAVALRY_PUBLICATION_ERROR, "{}", errmsg());
            assert_ne!(offered, CAVALRY_PUBLICATION_CLOSED);
            assert!(Instant::now() < deadline, "the session takes the message");
            self.work();
            session.poll();
        }
    }

    /// Closes every service with its client, every client, member and driver.
    fn stop(mut self) {
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

/// A session through the ABI, and the replies its handler was handed: each reply's
/// payload, its leadership term id and the cluster's time.
struct Session {
    context: *mut cavalry_context_t,
    client: *mut cavalry_t,
    session: *mut cavalry_cluster_session_t,
    id: i64,
    replies: Vec<(Vec<u8>, i64, i64)>,
}

unsafe extern "C" fn on_reply(
    clientd: *mut c_void,
    buffer: *const u8,
    length: usize,
    header: *const cavalry_header_t,
) {
    // SAFETY: `clientd` is the replies vector its poll was given; the buffer and the
    // header are valid for the call.
    unsafe {
        let replies = &mut *clientd.cast::<Vec<(Vec<u8>, i64, i64)>>();
        let mut values = std::mem::zeroed::<cavalry_cluster_reply_values_t>();
        assert_eq!(cavalry_cluster_reply_values(header, &mut values), 0);
        // A reply's header carries no entry's values.
        let mut entry = std::mem::zeroed::<cavalry_cluster_entry_values_t>();
        assert_eq!(cavalry_cluster_entry_values(header, &mut entry), -1);
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_WRONG_HEADER);
        replies.push((
            std::slice::from_raw_parts(buffer, length).to_vec(),
            values.leadership_term_id,
            values.timestamp,
        ));
    }
}

impl Session {
    fn poll(&mut self) -> usize {
        let clientd: *mut Vec<(Vec<u8>, i64, i64)> = &mut self.replies;
        // SAFETY: the ABI's conventions, held by this test.
        let read = unsafe {
            cavalry_cluster_session_poll(self.session, Some(on_reply), clientd.cast(), 16)
        };
        assert!(read >= 0, "the session polls: {}", errmsg());
        read as usize
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

/// A client's message is handed to the service beside every member with the same
/// values, and answered once, by the service beside the leader, with the entry's term
/// and the cluster's time. From inside that handler every call the page names is
/// refused or answered as it says. An entry the handler aborts is handed over again
/// and its reply is sent once; an entry it breaks at ends the poll.
#[test]
fn a_service_takes_each_entry_answers_its_session_and_holds_its_client_while_it_polls() {
    let mut cluster = Cluster::start("round");
    let leader = cluster.members[0].id;
    let mut session = cluster.connect_session();

    // A first message: every service takes it, and the leader's answers it.
    cluster.offer(&mut session, b"ping");
    cluster.converse(
        &mut session,
        "the message is answered and every service took it",
        |cluster, session| {
            !session.replies.is_empty()
                && cluster
                    .members
                    .iter()
                    .all(|member| !member.took(b"ping").is_empty())
        },
    );
    let ping = cluster.members[0].took(b"ping")[0].clone();
    assert_eq!(
        (ping.session, ping.reply, ping.action),
        (
            session.id,
            Some(CAVALRY_CLUSTER_REPLY_SENT),
            CAVALRY_ACTION_CONTINUE
        )
    );
    assert_eq!(
        (ping.leader, ping.timed_out, ping.entry_position),
        (-1, false, ping.position),
        "a session message has no leader, no timeout, and one position by both functions"
    );
    assert!(ping.term >= 0 && ping.timestamp > 1_600_000_000_000);
    let mut answer = vec![b'0' + leader as u8, b':'];
    answer.extend_from_slice(b"ping");
    assert_eq!(
        session.replies,
        vec![(answer, ping.term, ping.timestamp)],
        "one reply, the leader's service's, with the entry's term and time"
    );
    for member in &cluster.members[1..] {
        let took = member.took(b"ping");
        assert_eq!(took.len(), 1);
        assert_eq!(
            took[0].reply,
            Some(CAVALRY_CLUSTER_REPLY_NOT_ANSWERING),
            "the service beside a follower sends nothing"
        );
        // The same entry, wherever it is read: the replay check's ground.
        assert_eq!(
            (
                took[0].position,
                took[0].term,
                took[0].timestamp,
                took[0].session
            ),
            (ping.position, ping.term, ping.timestamp, ping.session)
        );
    }
    // What each service was handed before the message, in the log's order: a term's
    // first entry names its leader, and the session's open event the session.
    for member in &cluster.members {
        let handed = &member.served.handed;
        let term = handed
            .iter()
            .rev()
            .find(|entry| entry.r#type == CAVALRY_CLUSTER_ENTRY_NEW_LEADERSHIP_TERM)
            .expect("a term's first entry");
        assert_eq!(
            (term.leader, term.session, term.payload.len(), term.term),
            (leader, -1, 0, ping.term)
        );
        let opened = handed
            .iter()
            .find(|entry| entry.r#type == CAVALRY_CLUSTER_ENTRY_SESSION_OPEN)
            .expect("the session's open event");
        assert_eq!(
            (opened.session, opened.leader, opened.payload.len()),
            (session.id, -1, 0)
        );
        assert!(term.position < opened.position && opened.position < ping.position);
        assert!(
            handed
                .windows(2)
                .all(|pair| pair[0].position < pair[1].position),
            "the entries are in the log's order"
        );
        assert!(
            handed
                .iter()
                .all(|entry| entry.entry_position == entry.position)
        );
        // SAFETY: the ABI's conventions, held by this test.
        unsafe {
            let service = member.served.service;
            assert_eq!(cavalry_cluster_service_sessions(service), 1);
            assert!(cavalry_cluster_service_position(service) > ping.position);
            assert_eq!(
                cavalry_cluster_service_answers(service),
                member.id == leader,
                "the leader's service answers, and no other"
            );
            let mut fault = std::mem::zeroed::<cavalry_fault_t>();
            assert_eq!(cavalry_cluster_service_fault(service, &mut fault), 0);
            assert_eq!(
                (fault.code, fault.session_id, fault.position),
                (0, 0, CAVALRY_NULL_POSITION)
            );
        }
    }

    // From inside the leader's handler: the handles a probe needs are made with the
    // service's own client, before the poll that holds it.
    let leader_dir = cluster._dir.path().join(format!("member-{leader}"));
    let channel = c"cavalry:ipc";
    // SAFETY: the ABI's conventions, held by this test.
    let (second, log, publication) = unsafe {
        let client = cluster.members[0].client;
        let (mut second, mut third) = (ptr::null_mut(), ptr::null_mut());
        assert_eq!(cavalry_cluster_service(&mut second, client), 0);
        assert_eq!(cavalry_cluster_service(&mut third, client), 0);
        let mut log = ptr::null_mut();
        assert_eq!(
            cavalry_committed_log(&mut log, client, 0),
            0,
            "{}",
            errmsg()
        );
        assert!(!log.is_null(), "the leader has a recording of its log");
        let mut publication = ptr::null_mut();
        assert_eq!(
            cavalry_add_exclusive_publication(&mut publication, client, channel.as_ptr(), 2001),
            0,
            "{}",
            errmsg()
        );
        cluster.members[0].served.others = Some(Others {
            second,
            third,
            log,
            publication,
            dir: CString::new(leader_dir.to_str().expect("UTF-8")).expect("no NUL"),
            members: CString::new(cluster.members_string.clone()).expect("no NUL"),
        });
        (second, log, publication)
    };
    let before = session.replies.len();
    cluster.offer(&mut session, b"probe");
    cluster.converse(&mut session, "the probe is answered", |_, session| {
        session.replies.len() > before + 1
    });
    let polling = CAVALRY_ERR_POLLING;
    let untouched = CAVALRY_ERR_NULL;
    let expected: Vec<Probed> = vec![
        ("cavalry_header_values", -1, CAVALRY_ERR_WRONG_HEADER),
        ("cavalry_cluster_reply_values", -1, CAVALRY_ERR_WRONG_HEADER),
        ("cavalry_committed_log_entry_position", 1, untouched),
        ("cavalry_cluster_service_poll", -1, polling),
        ("cavalry_cluster_service_fault", -1, polling),
        ("cavalry_cluster_service_position", -1, polling),
        ("cavalry_cluster_service_answers", 0, polling),
        ("cavalry_cluster_service_sessions", -1, polling),
        ("cavalry_cluster_service_close", -1, polling),
        ("cavalry_cluster_service_close, no client", -1, polling),
        ("cavalry_client_id", 1, untouched),
        ("cavalry_dir", 1, untouched),
        ("cavalry_is_closed", 0, polling),
        ("cavalry_closed", -1, polling),
        ("cavalry_close", -1, polling),
        ("cavalry_add_publication", -1, polling),
        ("cavalry_add_exclusive_publication", -1, polling),
        ("cavalry_add_subscription", -1, polling),
        ("cavalry_start_recording", -1, polling),
        ("cavalry_start_replay", -1, polling),
        ("cavalry_stop_recording", -1, polling),
        ("cavalry_truncate_recording", -1, polling),
        ("cavalry_purge_segments", -1, polling),
        ("cavalry_purge_recording", -1, polling),
        ("cavalry_stop_replay", -1, polling),
        ("cavalry_add_destination", -1, polling),
        ("cavalry_remove_destination", -1, polling),
        ("cavalry_committed_log", -1, polling),
        ("cavalry_committed_log_close, another log", -1, polling),
        ("cavalry_connect_cluster", -1, polling),
        ("cavalry_cluster_service", -1, polling),
        ("cavalry_cluster_service_poll, another service", -1, polling),
        (
            "cavalry_cluster_service_close, another service",
            -1,
            polling,
        ),
        (
            "cavalry_cluster_service_reply, another service",
            -1,
            CAVALRY_ERR_CLUSTER_NO_ENTRY,
        ),
        (
            "cavalry_cluster_service_action, another service",
            -1,
            CAVALRY_ERR_CLUSTER_NO_ENTRY,
        ),
        (
            "cavalry_cluster_service_close, another service, no client",
            0,
            untouched,
        ),
        ("cavalry_publication_offer", 1, untouched),
        (
            "cavalry_cluster_service_reply, 1,345 bytes",
            i64::from(CAVALRY_CLUSTER_REPLY_TOO_LONG),
            CAVALRY_ERR_TOO_LONG,
        ),
        (
            "cavalry_cluster_service_reply, 1,344 bytes",
            i64::from(CAVALRY_CLUSTER_REPLY_SENT),
            untouched,
        ),
        (
            "cavalry_cluster_service_reply, no session",
            i64::from(CAVALRY_CLUSTER_REPLY_NO_SESSION),
            untouched,
        ),
        ("cavalry_cluster_service_reply, null buffer", -1, untouched),
        (
            "cavalry_cluster_service_action, 3",
            -1,
            CAVALRY_ERR_CLUSTER_ACTION,
        ),
        (
            "cavalry_cluster_service_action, 0",
            -1,
            CAVALRY_ERR_CLUSTER_ACTION,
        ),
        ("cavalry_cluster_service_action, abort", 0, untouched),
        ("cavalry_cluster_service_action, continue", 0, untouched),
    ];
    let probed = &cluster.members[0].served.probed;
    for (found, expected) in probed.iter().zip(&expected) {
        assert_eq!(found, expected);
    }
    assert_eq!(probed.len(), expected.len());
    let too_long = &cluster.members[0].served.too_long;
    assert!(
        too_long.contains("1345 bytes")
            && too_long.contains("carries 1344")
            && too_long.ends_with("; see docs/reference/cluster.md#replies"),
        "{too_long}"
    );
    // The last action counted: the probe's entry was taken, once.
    assert_eq!(cluster.members[0].took(b"probe").len(), 1);
    // The session read the handler's own answer, then the 1,344 bytes of the second.
    let mut answer = vec![b'0' + leader as u8, b':'];
    answer.extend_from_slice(b"probe");
    assert_eq!(session.replies[before].0, answer);
    assert_eq!(session.replies[before + 1].0, vec![7u8; 1_344]);
    // Nothing a refused close named was freed: each closes now, and the client, which
    // a refused close left open, serves them.
    // SAFETY: the ABI's conventions, held by this test.
    unsafe {
        let client = cluster.members[0].client;
        assert!(!cavalry_is_closed(client));
        assert_eq!(cavalry_closed(client), 0);
        assert_eq!(
            cavalry_cluster_service_close(second, client),
            0,
            "{}",
            errmsg()
        );
        assert_eq!(cavalry_committed_log_close(log, client), 0, "{}", errmsg());
        assert_eq!(cavalry_publication_close(publication), 0);
    }
    cluster.members[0].served.others = None;

    // An entry the handler aborts is handed over again, with its reply sent once.
    cluster.members[0].served.aborts = 2;
    let before = session.replies.len();
    cluster.offer(&mut session, b"again");
    cluster.converse(
        &mut session,
        "the aborted entry is taken at its third handling",
        |cluster, session| {
            cluster.members[0].took(b"again").len() == 3 && session.replies.len() > before
        },
    );
    let again: Vec<(Option<i32>, i32, i64)> = cluster.members[0]
        .took(b"again")
        .iter()
        .map(|handed| (handed.reply, handed.action, handed.position))
        .collect();
    let at = again[0].2;
    let sent = Some(CAVALRY_CLUSTER_REPLY_SENT);
    assert_eq!(
        again,
        vec![
            (sent, CAVALRY_ACTION_ABORT, at),
            (sent, CAVALRY_ACTION_ABORT, at),
            (sent, CAVALRY_ACTION_CONTINUE, at),
        ],
        "the same entry three times, its reply answered as sent each time"
    );
    // A second reply would come in this time; none does.
    let quiet = Instant::now() + Duration::from_millis(300);
    cluster.converse(&mut session, "the quiet time passes", |_, _| {
        Instant::now() >= quiet
    });
    assert_eq!(
        session.replies.len(),
        before + 1,
        "the session is sent the reply once: {:?}",
        session.replies
    );

    // An entry the handler breaks at is taken, and the poll ends after it. The
    // leader's service stands still until one poll could take both messages: both
    // followers' services have taken both, the leader has committed and recorded
    // both, and its driver has had 300 ms more to play its recording to the service.
    // A service reads its own member's recording, which a commit by the two followers
    // does not wait for.
    cluster.members[0].polled = false;
    cluster.members[0].served.breaks = true;
    cluster.offer(&mut session, b"one");
    cluster.offer(&mut session, b"two");
    cluster.converse(
        &mut session,
        "every member committed and recorded both messages",
        |cluster, _| {
            let Some(two) = cluster.members[1]
                .took(b"two")
                .first()
                .map(|two| two.position)
            else {
                return false;
            };
            !cluster.members[2].took(b"two").is_empty()
                && cluster.members[0].module.commit_position() > two
                && cluster.members[0].module.append_position() > two
        },
    );
    let played = Instant::now() + Duration::from_millis(300);
    cluster.converse(&mut session, "the leader's recording is played", |_, _| {
        Instant::now() >= played
    });
    assert!(cluster.members[0].took(b"one").is_empty());
    assert_eq!(
        cluster.poll_leader(),
        1,
        "the poll ends after the break, with the next entry there to take"
    );
    assert_eq!(cluster.members[0].took(b"one").len(), 1);
    assert!(cluster.members[0].took(b"two").is_empty());
    assert_eq!(cluster.poll_leader(), 1, "the next poll goes on");
    assert_eq!(cluster.members[0].took(b"two").len(), 1);
    cluster.members[0].polled = true;

    session.close();
    cluster.stop();
}

/// A service beside no member takes nothing and says so; its handler's two calls are
/// refused from outside a handler; every function refuses a null handle or pointer;
/// and a service outlives its client and closes without one.
#[test]
fn a_service_refuses_what_the_page_says_and_outlives_its_client() {
    let dir = TestDir::new("refusals");
    let driver = Driver::start(dir.path().join("driver"), dir.path().join("log")).expect("driver");
    let (context, client) = connect(&dir.path().join("driver"));
    let refused = |code: i32, what: &str| {
        assert_eq!(cavalry_errcode(), code, "{what}: {}", errmsg());
        assert!(
            errmsg().contains("; see docs/reference/c-abi.md#"),
            "{what}: {}",
            errmsg()
        );
    };
    // SAFETY: the ABI's conventions, held by this test.
    unsafe {
        // The making: a null handle to fill, and a null client, which leaves a null
        // handle.
        assert_eq!(cavalry_cluster_service(ptr::null_mut(), client), -1);
        refused(CAVALRY_ERR_NULL, "no handle to fill");
        let mut service = ptr::dangling_mut::<cavalry_cluster_service_t>();
        assert_eq!(cavalry_cluster_service(&mut service, ptr::null_mut()), -1);
        refused(CAVALRY_ERR_NULL, "no client");
        assert!(service.is_null(), "a refused making leaves a null handle");
        assert_eq!(
            cavalry_cluster_service(&mut service, client),
            0,
            "{}",
            errmsg()
        );
        assert!(!service.is_null());

        // Beside no member: nothing to take, which is an answer.
        let mut served = Served {
            member: 0,
            service,
            client,
            handed: Vec::new(),
            aborts: 0,
            breaks: false,
            others: None,
            probed: Vec::new(),
            too_long: String::new(),
        };
        let clientd: *mut Served = &mut served;
        for _ in 0..3 {
            assert_eq!(
                cavalry_cluster_service_poll(service, client, Some(serve), clientd.cast(), 16),
                0,
                "{}",
                errmsg()
            );
        }
        assert!(served.handed.is_empty());
        assert_eq!(cavalry_cluster_service_position(service), 0);
        assert!(!cavalry_cluster_service_answers(service));
        assert_eq!(cavalry_cluster_service_sessions(service), 0);
        let mut fault = std::mem::zeroed::<cavalry_fault_t>();
        assert_eq!(cavalry_cluster_service_fault(service, &mut fault), 0);
        assert_eq!(
            (fault.code, fault.session_id, fault.position),
            (0, 0, CAVALRY_NULL_POSITION)
        );

        // The handler's two calls, from outside a handler: no entry to answer.
        assert_eq!(
            cavalry_cluster_service_reply(service, 1, b"x".as_ptr(), 1),
            -1
        );
        refused(CAVALRY_ERR_CLUSTER_NO_ENTRY, "a reply outside the handler");
        assert!(errmsg().contains("cavalry_cluster_service_reply refused (cluster-no-entry)"));
        assert_eq!(
            cavalry_cluster_service_action(service, CAVALRY_ACTION_ABORT),
            -1
        );
        refused(
            CAVALRY_ERR_CLUSTER_NO_ENTRY,
            "an action outside the handler",
        );
        assert_eq!(cavalry_cluster_service_action(service, 3), -1);
        refused(
            CAVALRY_ERR_CLUSTER_NO_ENTRY,
            "no action outside the handler",
        );

        // The poll's null arguments.
        assert_eq!(
            cavalry_cluster_service_poll(ptr::null_mut(), client, Some(serve), clientd.cast(), 1),
            -1
        );
        refused(CAVALRY_ERR_NULL, "a poll of no service");
        assert_eq!(
            cavalry_cluster_service_poll(service, ptr::null_mut(), Some(serve), clientd.cast(), 1),
            -1
        );
        refused(CAVALRY_ERR_NULL, "a poll with no client");
        assert!(errmsg().contains("client is null"), "{}", errmsg());
        assert_eq!(
            cavalry_cluster_service_poll(service, client, None, clientd.cast(), 1),
            -1
        );
        refused(CAVALRY_ERR_NULL, "a poll with no handler");

        // Every other function, of no service or with no struct to fill.
        let null = ptr::null_mut::<cavalry_cluster_service_t>();
        let mut values = std::mem::zeroed::<cavalry_cluster_entry_values_t>();
        assert_eq!(cavalry_cluster_entry_values(ptr::null(), &mut values), -1);
        refused(CAVALRY_ERR_NULL, "no header");
        assert_eq!(cavalry_cluster_service_reply(null, 1, b"x".as_ptr(), 1), -1);
        refused(CAVALRY_ERR_NULL, "a reply of no service");
        assert_eq!(
            cavalry_cluster_service_reply(service, 1, ptr::null(), 1),
            -1
        );
        refused(CAVALRY_ERR_NULL, "a reply of no bytes");
        assert_eq!(
            cavalry_cluster_service_action(null, CAVALRY_ACTION_ABORT),
            -1
        );
        refused(CAVALRY_ERR_NULL, "an action of no service");
        assert_eq!(cavalry_cluster_service_fault(null, &mut fault), -1);
        refused(CAVALRY_ERR_NULL, "the fault of no service");
        assert_eq!(cavalry_cluster_service_fault(service, ptr::null_mut()), -1);
        refused(CAVALRY_ERR_NULL, "no fault to fill");
        assert_eq!(cavalry_cluster_service_position(null), -1);
        refused(CAVALRY_ERR_NULL, "the position of no service");
        assert!(!cavalry_cluster_service_answers(null));
        refused(CAVALRY_ERR_NULL, "whether no service answers");
        assert_eq!(cavalry_cluster_service_sessions(null), -1);
        refused(CAVALRY_ERR_NULL, "the sessions of no service");
        assert_eq!(cavalry_cluster_service_close(null, client), -1);
        refused(CAVALRY_ERR_NULL, "the close of no service");

        // A service closed with its client; and one that outlives its client, whose
        // reads answer and whose close takes no client.
        assert_eq!(
            cavalry_cluster_service_close(service, client),
            0,
            "{}",
            errmsg()
        );
        let mut outliving = ptr::null_mut();
        assert_eq!(cavalry_cluster_service(&mut outliving, client), 0);
        assert_eq!(cavalry_close(client), 0);
        assert_eq!(cavalry_cluster_service_position(outliving), 0);
        assert_eq!(cavalry_cluster_service_sessions(outliving), 0);
        assert_eq!(cavalry_cluster_service_close(outliving, ptr::null_mut()), 0);
        assert_eq!(cavalry_context_close(context), 0);
    }
    driver.close().expect("the driver closes");
}

/// A subscription's header carries a frame and no entry's values: the function that
/// reads an entry's refuses it, and fills nothing.
#[test]
fn a_subscriptions_header_carries_no_entrys_values() {
    unsafe extern "C" fn ask(
        clientd: *mut c_void,
        _buffer: *const u8,
        _length: usize,
        header: *const cavalry_header_t,
    ) {
        // SAFETY: `clientd` is the answers vector its poll was given; the header is
        // valid for the call.
        unsafe {
            let answers = &mut *clientd.cast::<Vec<(i32, i32, i64, String)>>();
            let mut values = std::mem::zeroed::<cavalry_cluster_entry_values_t>();
            values.position = 77;
            let answer = cavalry_cluster_entry_values(header, &mut values);
            answers.push((answer, cavalry_errcode(), values.position, errmsg()));
        }
    }
    let dir = TestDir::new("frame");
    let driver = Driver::start(dir.path().join("driver"), dir.path().join("log")).expect("driver");
    let (context, client) = connect(&dir.path().join("driver"));
    let channel = c"cavalry:ipc";
    let mut answers: Vec<(i32, i32, i64, String)> = Vec::new();
    // SAFETY: the ABI's conventions, held by this test.
    unsafe {
        let (mut subscription, mut publication) = (ptr::null_mut(), ptr::null_mut());
        assert_eq!(
            cavalry_add_subscription(&mut subscription, client, channel.as_ptr(), 1001),
            0
        );
        assert_eq!(
            cavalry_add_publication(&mut publication, client, channel.as_ptr(), 1001),
            0
        );
        let clientd: *mut Vec<(i32, i32, i64, String)> = &mut answers;
        let deadline = Instant::now() + DEADLINE;
        let mut offered = false;
        while answers.is_empty() {
            assert!(Instant::now() < deadline, "the fragment arrived");
            if !offered {
                offered = cavalry_publication_offer(publication, b"hello".as_ptr(), 5) >= 0;
            }
            assert!(cavalry_subscription_poll(subscription, Some(ask), clientd.cast(), 1) >= 0);
        }
        assert_eq!(cavalry_publication_close(publication), 0);
        assert_eq!(cavalry_subscription_close(subscription), 0);
        assert_eq!(cavalry_close(client), 0);
        assert_eq!(cavalry_context_close(context), 0);
    }
    let (answer, code, position, message) = &answers[0];
    assert_eq!(
        (*answer, *code, *position),
        (-1, CAVALRY_ERR_WRONG_HEADER, 77),
        "refused, with nothing filled"
    );
    assert!(
        message.starts_with("cavalry_cluster_entry_values refused (wrong-header)")
            && message.ends_with("; see docs/reference/c-abi.md#what-a-header-carries"),
        "{message}"
    );
    driver.close().expect("the driver closes");
}
