//! A cluster session through the ABI (`spec/c-abi.md`, "Functions", cluster session,
//! "What a header carries"; ADR 0122), against a driver in the test's process. The
//! cluster is played by hand, by a Rust client of the same driver in a thread of its
//! own: a subscription on the first member's `ingress` endpoint and a publication to
//! the session's response channel, on which it answers as the test's script says.
//! What a real cluster does is `cavalry-client`'s and `cavalry-raft`'s to test; this
//! holds the C form to the page: the codes, the values a header carries, the calls a
//! running poll refuses, and the handle's life.

use std::ffi::{CStr, CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cavalry_client::{Cavalry, Publication};
use cavalry_driver::Driver;
use cavalry_ffi::*;
use cavalry_proto::cluster::{EGRESS_STREAM_ID, EventCode, INGRESS_STREAM_ID, Message, Stream};

// This test's ports, from the one file that says how they are taken and why, which is
// `cavalry-raft`'s: `crates/cavalry-raft/tests/ports/mod.rs`.
#[path = "../../cavalry-raft/tests/ports/mod.rs"]
mod ports;

struct TestDir(PathBuf);

impl TestDir {
    fn new(test: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("cavalry-ffi-session-{}-{test}", std::process::id()));
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

fn errmsg() -> String {
    // SAFETY: the message is NUL-terminated and valid until the next failing call.
    unsafe { CStr::from_ptr(cavalry_errmsg()) }
        .to_str()
        .expect("UTF-8")
        .to_string()
}

fn wait_until(timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while !done() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    true
}

/// The session id and the term the played leader gives, and the cluster's time it
/// stamps its replies with.
const SESSION: i64 = 7;
const TERM: i64 = 3;
const TIME: i64 = 1_700_000_000_123;

/// How the played leader answers a session connect.
#[derive(Clone, Copy)]
enum Answer {
    /// It opens the session.
    Ok,
    /// It refuses, with this detail.
    Refuse(&'static str),
    /// It leads a cluster of this protocol version.
    Version(i32),
}

/// What the played leader read from its ingress endpoint.
#[derive(Debug, PartialEq)]
enum Seen {
    Connect,
    Message(i64, i64, Vec<u8>),
    Keepalive,
    Close(i64, i64),
}

fn encode(message: &Message<'_>) -> Vec<u8> {
    let mut bytes = vec![0u8; message.encoded_length()];
    let length = message.write(&mut bytes).expect("encodes");
    bytes.truncate(length);
    bytes
}

/// A reply as a service sends it: a session message on the egress stream.
fn reply(term: i64, session: i64, time: i64, payload: &[u8]) -> Vec<u8> {
    encode(&Message::SessionMessage {
        leadership_term_id: term,
        cluster_session_id: session,
        timestamp: time,
        payload,
    })
}

/// The first of three members, played by one thread; the two others are in the
/// members string and nobody listens at them.
struct Played {
    members: CString,
    /// Bytes for the thread to send on the response channel.
    send: Sender<Vec<u8>>,
    seen: Receiver<Seen>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Played {
    fn start(dir: &Path, answer: Answer) -> Self {
        let ports = ports::free_ports(3);
        let members = (0..3)
            .map(|id| {
                let port = ports[id];
                format!("{id},127.0.0.1:{port},127.0.0.1:1,127.0.0.1:2,127.0.0.1:3,127.0.0.1:4")
            })
            .collect::<Vec<_>>()
            .join("|");
        let (send, receive) = channel();
        let (observe, seen) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (dir, stop, port) = (dir.to_path_buf(), Arc::clone(&stop), ports[0]);
            std::thread::spawn(move || play(&dir, port, answer, &observe, &receive, &stop))
        };
        Self {
            members: CString::new(members).expect("no NUL"),
            send,
            seen,
            stop,
            thread: Some(thread),
        }
    }

    /// Everything the played leader has read since the last call.
    fn seen(&self) -> Vec<Seen> {
        self.seen.try_iter().collect()
    }

    fn stop(mut self) {
        self.stop.store(true, Ordering::Release);
        self.thread
            .take()
            .expect("the thread")
            .join()
            .expect("the played leader ended");
    }
}

/// The played leader's loop: read the ingress endpoint, answer a connect as `answer`
/// says, answer a session message with its payload, its term and the cluster's time,
/// and send what the test asked for, until told to stop.
fn play(
    dir: &Path,
    port: u16,
    answer: Answer,
    observe: &Sender<Seen>,
    receive: &Receiver<Vec<u8>>,
    stop: &AtomicBool,
) {
    let mut client = Cavalry::connect_to(dir).expect("the played leader connects");
    let mut ingress = client
        .add_subscription(
            &format!("cavalry:udp?endpoint=127.0.0.1:{port}"),
            INGRESS_STREAM_ID,
        )
        .expect("the ingress subscription");
    let mut egress: Option<Publication> = None;
    let mut owed: Vec<Vec<u8>> = Vec::new();
    while !stop.load(Ordering::Acquire) {
        let mut read: Vec<Vec<u8>> = Vec::new();
        ingress
            .poll(|body, _| read.push(body.to_vec()), 16)
            .expect("the ingress polls");
        for bytes in read {
            let seen = match Message::parse(Stream::Ingress, &bytes).expect("an ingress message") {
                Message::SessionConnect {
                    correlation_id,
                    response_channel,
                    ..
                } => {
                    if egress.is_none() {
                        let channel = std::str::from_utf8(response_channel).expect("a channel");
                        egress = Some(
                            client
                                .add_exclusive_publication(channel, EGRESS_STREAM_ID)
                                .expect("the egress publication"),
                        );
                    }
                    let (code, version, detail, session): (_, _, &[u8], _) = match answer {
                        Answer::Ok => (EventCode::Ok, 0, b"", SESSION),
                        Answer::Refuse(detail) => (EventCode::Error, 0, detail.as_bytes(), -1),
                        Answer::Version(version) => (EventCode::Ok, version, b"", SESSION),
                    };
                    owed.push(encode(&Message::SessionEvent {
                        cluster_session_id: session,
                        correlation_id,
                        leadership_term_id: TERM,
                        leader_member_id: 0,
                        code,
                        version,
                        leader_heartbeat_timeout_ns: 10_000_000_000,
                        detail,
                    }));
                    Seen::Connect
                }
                Message::SessionMessage {
                    leadership_term_id,
                    cluster_session_id,
                    payload,
                    ..
                } => {
                    owed.push(reply(leadership_term_id, cluster_session_id, TIME, payload));
                    Seen::Message(leadership_term_id, cluster_session_id, payload.to_vec())
                }
                Message::SessionKeepalive { .. } => Seen::Keepalive,
                Message::SessionClose {
                    leadership_term_id,
                    cluster_session_id,
                } => Seen::Close(leadership_term_id, cluster_session_id),
                other => panic!("not an ingress message: {other:?}"),
            };
            // The test may have ended its read: what was seen then goes nowhere.
            let _ = observe.send(seen);
        }
        owed.extend(receive.try_iter());
        if let Some(publication) = egress.as_mut() {
            while let Some(bytes) = owed.first() {
                if publication.offer(bytes).is_err() {
                    break;
                }
                owed.remove(0);
            }
        }
        std::thread::sleep(Duration::from_micros(200));
    }
    drop(egress);
    drop(ingress);
    client.close().expect("the played leader closes");
}

/// A driver, a client of it through the ABI, a free port for the response channel,
/// and the played leader.
struct World {
    played: Played,
    context: *mut cavalry_context_t,
    client: *mut cavalry_t,
    response_channel: CString,
    driver: Driver,
    _dir: TestDir,
}

impl World {
    fn start(test: &str, answer: Answer) -> Self {
        let dir = TestDir::new(test);
        let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
        let dir_c = CString::new(dir.path().to_str().expect("a UTF-8 temp dir")).expect("no NUL");
        // SAFETY: the ABI's conventions, held by this test.
        let (context, client) = unsafe {
            let mut context = ptr::null_mut();
            assert_eq!(cavalry_context_init_v(&mut context, CAVALRY_ABI_VERSION), 0);
            assert_eq!(cavalry_context_set_dir(context, dir_c.as_ptr()), 0);
            let mut client = ptr::null_mut();
            assert_eq!(cavalry_init(&mut client, context), 0, "{}", errmsg());
            (context, client)
        };
        let played = Played::start(dir.path(), answer);
        let response_channel = CString::new(format!(
            "cavalry:udp?endpoint=127.0.0.1:{}",
            ports::free_port()
        ))
        .expect("no NUL");
        Self {
            played,
            context,
            client,
            response_channel,
            driver,
            _dir: dir,
        }
    }

    /// `cavalry_connect_cluster`, with what it returned and the handle it wrote.
    fn connect(&self) -> (i32, *mut cavalry_cluster_session_t) {
        let mut session = ptr::null_mut();
        // SAFETY: the ABI's conventions, held by this test.
        let returned = unsafe {
            cavalry_connect_cluster(
                &mut session,
                self.client,
                self.played.members.as_ptr(),
                self.response_channel.as_ptr(),
            )
        };
        (returned, session)
    }

    /// Stops the played leader, closes the client unless the test did, and the rest.
    fn close(self, client_open: bool) {
        self.played.stop();
        // SAFETY: the ABI's conventions; each handle is closed once.
        unsafe {
            if client_open {
                assert_eq!(cavalry_close(self.client), 0, "{}", errmsg());
            }
            assert_eq!(cavalry_context_close(self.context), 0);
        }
        self.driver.close().expect("the driver closes");
    }
}

/// What the test's handler read of each reply, and what each call it made from inside
/// the poll answered.
#[derive(Default)]
struct Read {
    session: Option<*mut cavalry_cluster_session_t>,
    /// The payload, the leadership term id and the cluster's time.
    replies: Vec<(Vec<u8>, i64, i64)>,
    /// What each call on the session answered from inside the poll, with the code it
    /// set, in the order of `inside`'s body; filled once.
    inside: Vec<(&'static str, i64, i32)>,
}

unsafe extern "C" fn collect(
    clientd: *mut c_void,
    buffer: *const u8,
    length: usize,
    header: *const cavalry_header_t,
) {
    // SAFETY: the test passes a `Read`; the payload and the header are valid for the
    // call; the session, when the test gave one, is the handle whose poll runs.
    unsafe {
        let read = &mut *clientd.cast::<Read>();
        let mut values = std::mem::zeroed::<cavalry_cluster_reply_values_t>();
        assert_eq!(
            cavalry_cluster_reply_values(header, &mut values),
            0,
            "{}",
            errmsg()
        );
        read.replies.push((
            std::slice::from_raw_parts(buffer, length).to_vec(),
            values.leadership_term_id,
            values.timestamp,
        ));
        if let Some(session) = read.session.take() {
            inside(session, header, &mut read.inside);
        }
    }
}

/// Every call the page's table names, made from inside the session's own handler.
///
/// # Safety
///
/// `session` is the handle whose poll runs, and `header` the one it handed over.
unsafe fn inside(
    session: *mut cavalry_cluster_session_t,
    header: *const cavalry_header_t,
    answers: &mut Vec<(&'static str, i64, i32)>,
) {
    // SAFETY: the caller's convention above; every pointer written through is this
    // function's own.
    unsafe {
        let mut frame = std::mem::zeroed::<cavalry_header_values_t>();
        frame.frame.term_id = 77;
        let returned = cavalry_header_values(header, &mut frame);
        assert_eq!(
            frame.frame.term_id, 77,
            "a refused read fills nothing of the struct"
        );
        answers.push(("header_values", returned.into(), cavalry_errcode()));
        answers.push((
            "entry_position",
            cavalry_committed_log_entry_position(header),
            cavalry_errcode(),
        ));
        let mut constants = std::mem::zeroed::<cavalry_cluster_session_constants_t>();
        answers.push((
            "constants",
            cavalry_cluster_session_constants(session, &mut constants).into(),
            constants.session_id as i32,
        ));
        answers.push((
            "offer",
            cavalry_cluster_session_offer(session, b"no".as_ptr(), 2),
            cavalry_errcode(),
        ));
        answers.push((
            "poll",
            cavalry_cluster_session_poll(session, Some(collect), ptr::null_mut(), 1).into(),
            cavalry_errcode(),
        ));
        let mut fault = std::mem::zeroed::<cavalry_fault_t>();
        answers.push((
            "fault",
            cavalry_cluster_session_fault(session, &mut fault).into(),
            cavalry_errcode(),
        ));
        answers.push((
            "leadership_term_id",
            cavalry_cluster_session_leadership_term_id(session),
            cavalry_errcode(),
        ));
        answers.push((
            "leader_member_id",
            cavalry_cluster_session_leader_member_id(session).into(),
            cavalry_errcode(),
        ));
        answers.push((
            "stale_replies",
            cavalry_cluster_session_stale_replies(session),
            cavalry_errcode(),
        ));
        answers.push((
            "closed",
            i64::from(!cavalry_cluster_session_closed(session).is_null()),
            cavalry_errcode(),
        ));
        answers.push((
            "close",
            cavalry_cluster_session_close(session).into(),
            cavalry_errcode(),
        ));
    }
}

/// Polls `session` into `read` until `done`.
fn poll_until(
    session: *mut cavalry_cluster_session_t,
    read: &mut Read,
    mut done: impl FnMut(&Read) -> bool,
) -> bool {
    wait_until(Duration::from_secs(5), || {
        // SAFETY: the ABI's conventions, held by this test.
        let handed = unsafe {
            cavalry_cluster_session_poll(session, Some(collect), (read as *mut Read).cast(), 10)
        };
        assert!(handed >= 0, "{}", errmsg());
        done(read)
    })
}

/// A session through the ABI, start to end: the constants and the reads of an open
/// session, a message offered and its reply handed to the C handler with its term and
/// the cluster's time, what each call from inside the handler answers, a message of
/// one byte too many, a stale reply passed over and counted, a new leader event
/// followed, the cluster's closed event with its reason, and the close.
#[test]
fn a_session_offers_polls_reads_its_replies_values_and_closes() {
    let world = World::start("session", Answer::Ok);
    let (returned, session) = world.connect();
    assert_eq!(returned, 0, "{}", errmsg());
    assert!(!session.is_null());
    assert_eq!(world.played.seen(), [Seen::Connect]);
    // SAFETY: the ABI's conventions, held by this test.
    unsafe {
        let mut constants = std::mem::zeroed::<cavalry_cluster_session_constants_t>();
        assert_eq!(
            cavalry_cluster_session_constants(session, &mut constants),
            0
        );
        assert_eq!(
            (constants.session_id, constants.max_message_length),
            (SESSION, 1408 - 32 - 32),
            "the leader's id, and one frame less the two headers"
        );
        assert_eq!(cavalry_cluster_session_leadership_term_id(session), TERM);
        assert_eq!(cavalry_cluster_session_leader_member_id(session), 0);
        assert_eq!(cavalry_cluster_session_stale_replies(session), 0);
        assert!(
            cavalry_cluster_session_closed(session).is_null(),
            "an open session has no reason"
        );
        let mut fault = std::mem::zeroed::<cavalry_fault_t>();
        assert_eq!(cavalry_cluster_session_fault(session, &mut fault), 0);
        assert_eq!(
            (fault.code, fault.session_id, fault.position),
            (0, 0, CAVALRY_NULL_POSITION),
            "no poll yet"
        );

        // A message, and its reply through the C handler.
        assert!(cavalry_cluster_session_offer(session, b"ping".as_ptr(), 4) > 0);
        let mut read = Read {
            session: Some(session),
            ..Read::default()
        };
        assert!(poll_until(session, &mut read, |read| !read
            .replies
            .is_empty()));
        assert_eq!(read.replies, [(b"ping".to_vec(), TERM, TIME)]);
        assert!(
            wait_until(Duration::from_secs(5), || world
                .played
                .seen()
                .contains(&Seen::Message(TERM, SESSION, b"ping".to_vec()))),
            "to the leader, with the session's term and id"
        );
        // From inside the handler: the page's table, row by row.
        let polling = CAVALRY_ERR_POLLING;
        assert_eq!(
            read.inside,
            [
                ("header_values", -1, CAVALRY_ERR_WRONG_HEADER),
                ("entry_position", -1, CAVALRY_ERR_NO_POSITION),
                ("constants", 0, SESSION as i32),
                ("offer", CAVALRY_PUBLICATION_ERROR, polling),
                ("poll", -1, polling),
                ("fault", -1, polling),
                ("leadership_term_id", -1, polling),
                ("leader_member_id", -1, polling),
                ("stale_replies", -1, polling),
                ("closed", 0, polling),
                ("close", -1, polling),
            ]
        );
        // The refused close freed nothing and sent nothing: the session goes on.
        assert_eq!(cavalry_cluster_session_leadership_term_id(session), TERM);

        // One frame: a byte more is the error code, with the lengths in the message.
        let too_long = vec![0u8; constants.max_message_length + 1];
        assert_eq!(
            cavalry_cluster_session_offer(session, too_long.as_ptr(), too_long.len()),
            CAVALRY_PUBLICATION_ERROR
        );
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_TOO_LONG, "{}", errmsg());
        // A null buffer is refused, but for an empty message.
        assert_eq!(
            cavalry_cluster_session_offer(session, ptr::null(), 1),
            CAVALRY_PUBLICATION_ERROR
        );
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_NULL);
        assert!(cavalry_cluster_session_offer(session, ptr::null(), 0) > 0);
        assert!(poll_until(session, &mut read, |read| read.replies.len() == 2));
        assert_eq!(read.replies[1], (Vec::new(), TERM, TIME));

        // A reply of the next term, then one of the first term again: stale, passed
        // over, counted, and in no poll's return.
        for bytes in [
            reply(TERM + 1, SESSION, TIME + 1, b"next"),
            reply(TERM, SESSION, TIME + 2, b"late"),
            reply(TERM + 1, SESSION, TIME + 3, b"after"),
        ] {
            world.played.send.send(bytes).expect("sent");
        }
        assert!(poll_until(session, &mut read, |read| read.replies.len() == 4));
        assert_eq!(
            read.replies[2..],
            [
                (b"next".to_vec(), TERM + 1, TIME + 1),
                (b"after".to_vec(), TERM + 1, TIME + 3)
            ]
        );
        assert_eq!(cavalry_cluster_session_stale_replies(session), 1);

        // A new leader event turns the session: the term and the member it sends to.
        world
            .played
            .send
            .send(encode(&Message::NewLeaderEvent {
                leadership_term_id: TERM + 1,
                cluster_session_id: SESSION,
                leader_member_id: 2,
                ingress_endpoints: b"",
            }))
            .expect("sent");
        assert!(wait_until(Duration::from_secs(5), || {
            assert!(
                cavalry_cluster_session_poll(
                    session,
                    Some(collect),
                    (&mut read as *mut Read).cast(),
                    10
                ) >= 0
            );
            cavalry_cluster_session_leader_member_id(session) == 2
        }));
        assert_eq!(
            cavalry_cluster_session_leadership_term_id(session),
            TERM + 1
        );

        // The cluster closes the session: the reason, and an offer refused as closed.
        world
            .played
            .send
            .send(encode(&Message::SessionEvent {
                cluster_session_id: SESSION,
                correlation_id: -1,
                leadership_term_id: TERM + 1,
                leader_member_id: 2,
                code: EventCode::Closed,
                version: 0,
                leader_heartbeat_timeout_ns: 10_000_000_000,
                detail: b"TIMEOUT",
            }))
            .expect("sent");
        let mut reason: *const c_char = ptr::null();
        assert!(wait_until(Duration::from_secs(5), || {
            assert!(
                cavalry_cluster_session_poll(
                    session,
                    Some(collect),
                    (&mut read as *mut Read).cast(),
                    10
                ) >= 0
            );
            reason = cavalry_cluster_session_closed(session);
            !reason.is_null()
        }));
        assert_eq!(CStr::from_ptr(reason).to_str(), Ok("TIMEOUT"));
        assert_eq!(
            cavalry_cluster_session_offer(session, b"too late".as_ptr(), 8),
            CAVALRY_PUBLICATION_CLOSED
        );
        // The reason is the handle's, for its life: a later poll leaves it where it is.
        assert_eq!(
            cavalry_cluster_session_poll(
                session,
                Some(collect),
                (&mut read as *mut Read).cast(),
                10
            ),
            0
        );
        assert_eq!(cavalry_cluster_session_closed(session), reason);
        assert_eq!(read.replies.len(), 4, "nothing more was handed over");

        // A closed session's close sends the leader nothing.
        let _ = world.played.seen();
        assert_eq!(cavalry_cluster_session_close(session), 0, "{}", errmsg());
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            !world
                .played
                .seen()
                .iter()
                .any(|seen| matches!(seen, Seen::Close(..))),
            "a session the cluster closed sends no session close"
        );
    }
    world.close(true);
}

/// An open session's close offers the leader a session close, with the session's term
/// and id, and a session outlives its client's close: its offer answers closed, its
/// poll the closed fault, and its own close still frees it.
#[test]
fn a_close_tells_the_leader_and_a_session_outlives_its_client() {
    let world = World::start("close", Answer::Ok);
    let (returned, first) = world.connect();
    assert_eq!(returned, 0, "{}", errmsg());
    // SAFETY: the ABI's conventions, held by this test.
    unsafe {
        assert_eq!(cavalry_cluster_session_close(first), 0, "{}", errmsg());
        assert!(
            wait_until(Duration::from_secs(5), || world
                .played
                .seen()
                .contains(&Seen::Close(TERM, SESSION))),
            "the leader read the session close"
        );
    }
    world.close(true);

    let world = World::start("outlives", Answer::Ok);
    let (returned, session) = world.connect();
    assert_eq!(returned, 0, "{}", errmsg());
    // SAFETY: the ABI's conventions, held by this test.
    unsafe {
        assert_eq!(cavalry_close(world.client), 0, "{}", errmsg());
        assert_eq!(
            cavalry_cluster_session_offer(session, b"late".as_ptr(), 4),
            CAVALRY_PUBLICATION_CLOSED
        );
        let mut read = Read::default();
        assert_eq!(
            cavalry_cluster_session_poll(
                session,
                Some(collect),
                (&mut read as *mut Read).cast(),
                10
            ),
            -1
        );
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_FAULT_CLOSED, "{}", errmsg());
        let mut fault = std::mem::zeroed::<cavalry_fault_t>();
        assert_eq!(cavalry_cluster_session_fault(session, &mut fault), 0);
        assert_eq!(fault.code, CAVALRY_ERR_FAULT_CLOSED);
        assert_eq!(cavalry_cluster_session_close(session), 0, "{}", errmsg());
    }
    world.close(false);
}

/// What the connect refuses, each with its code and a message that says what to do:
/// a null argument, a string that is not UTF-8, a members string that fails a check,
/// the leader's refusal with its reason, and a cluster of another major version. Each
/// leaves a null handle.
#[test]
fn the_connect_refuses_what_the_page_says_with_the_clusters_codes() {
    let world = World::start("refused", Answer::Refuse("concurrent session limit"));
    let members = world.played.members.as_ptr();
    let channel = world.response_channel.as_ptr();
    let dangling = ptr::dangling_mut::<cavalry_cluster_session_t>();
    // SAFETY: the ABI's conventions, held by this test.
    unsafe {
        assert_eq!(
            cavalry_connect_cluster(ptr::null_mut(), world.client, members, channel),
            -1
        );
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_NULL);
        for (client, members, channel, what) in [
            (ptr::null_mut(), members, channel, "client"),
            (world.client, ptr::null(), channel, "members"),
            (world.client, members, ptr::null(), "response_channel"),
        ] {
            let mut session = dangling;
            assert_eq!(
                cavalry_connect_cluster(&mut session, client, members, channel),
                -1
            );
            assert_eq!(cavalry_errcode(), CAVALRY_ERR_NULL);
            assert!(errmsg().contains(what), "{}", errmsg());
            assert!(session.is_null(), "a refused connect leaves a null handle");
        }

        let not_utf8 = CString::new(vec![0xffu8, 0xfe]).expect("no NUL");
        for (members, channel, what) in [
            (not_utf8.as_ptr(), channel, "the members string"),
            (members, not_utf8.as_ptr(), "the response channel"),
        ] {
            let mut session = dangling;
            assert_eq!(
                cavalry_connect_cluster(&mut session, world.client, members, channel),
                -1
            );
            assert_eq!(cavalry_errcode(), CAVALRY_ERR_NOT_UTF8);
            assert!(errmsg().contains(what), "{}", errmsg());
            assert!(session.is_null());
        }
        assert_eq!(world.played.seen(), [], "nothing was sent to any member");

        let bad_members = CString::new("0,127.0.0.1:9").expect("no NUL");
        let mut session = dangling;
        assert_eq!(
            cavalry_connect_cluster(&mut session, world.client, bad_members.as_ptr(), channel),
            -1
        );
        assert_eq!(
            cavalry_errcode(),
            CAVALRY_ERR_CLUSTER_MEMBERS,
            "{}",
            errmsg()
        );
        assert!(errmsg().contains("(members)"), "{}", errmsg());
        assert!(session.is_null());

        let (returned, session) = world.connect();
        assert_eq!(returned, -1);
        assert_eq!(
            cavalry_errcode(),
            CAVALRY_ERR_CLUSTER_REFUSED,
            "{}",
            errmsg()
        );
        assert!(
            errmsg().contains("concurrent session limit")
                && errmsg().contains("docs/reference/cluster.md#sessions"),
            "{}",
            errmsg()
        );
        assert!(session.is_null());
    }
    world.close(true);

    let world = World::start("version", Answer::Version(2 << 16));
    let (returned, session) = world.connect();
    assert_eq!(returned, -1);
    assert_eq!(
        cavalry_errcode(),
        CAVALRY_ERR_CLUSTER_VERSION,
        "{}",
        errmsg()
    );
    assert!(errmsg().contains("2.0.0"), "{}", errmsg());
    assert!(session.is_null());
    world.close(true);
}

unsafe extern "C" fn ask_for_a_reply(
    clientd: *mut c_void,
    _buffer: *const u8,
    _length: usize,
    header: *const cavalry_header_t,
) {
    // SAFETY: the test passes a `Vec<(i32, i32, i64)>`; the header is valid for the
    // call.
    unsafe {
        let answers = &mut *clientd.cast::<Vec<(i32, i32, i64)>>();
        let mut values = std::mem::zeroed::<cavalry_cluster_reply_values_t>();
        values.leadership_term_id = 77;
        let returned = cavalry_cluster_reply_values(header, &mut values);
        answers.push((returned, cavalry_errcode(), values.leadership_term_id));
    }
}

/// A subscription's header carries a frame and no reply's values:
/// `cavalry_cluster_reply_values` refuses it with the code, fills nothing, and names
/// the page's section. So does a null header or a null struct, as any null is.
#[test]
fn a_subscriptions_header_carries_no_replys_values() {
    let dir = TestDir::new("header");
    let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
    let dir_c = CString::new(dir.path().to_str().expect("a UTF-8 temp dir")).expect("no NUL");
    let channel = CString::new("cavalry:ipc").expect("no NUL");
    // SAFETY: the ABI's conventions, held by this test.
    unsafe {
        let mut context = ptr::null_mut();
        assert_eq!(cavalry_context_init_v(&mut context, CAVALRY_ABI_VERSION), 0);
        assert_eq!(cavalry_context_set_dir(context, dir_c.as_ptr()), 0);
        let mut client = ptr::null_mut();
        assert_eq!(cavalry_init(&mut client, context), 0, "{}", errmsg());
        let mut subscription = ptr::null_mut();
        assert_eq!(
            cavalry_add_subscription(&mut subscription, client, channel.as_ptr(), 1001),
            0
        );
        let mut publication = ptr::null_mut();
        assert_eq!(
            cavalry_add_publication(&mut publication, client, channel.as_ptr(), 1001),
            0
        );
        let mut answers: Vec<(i32, i32, i64)> = Vec::new();
        assert!(wait_until(Duration::from_secs(5), || {
            let _ = cavalry_publication_offer(publication, b"x".as_ptr(), 1);
            assert!(
                cavalry_subscription_poll(
                    subscription,
                    Some(ask_for_a_reply),
                    (&mut answers as *mut Vec<_>).cast(),
                    1
                ) >= 0
            );
            !answers.is_empty()
        }));
        assert_eq!(answers[0], (-1, CAVALRY_ERR_WRONG_HEADER, 77));

        let mut values = std::mem::zeroed::<cavalry_cluster_reply_values_t>();
        assert_eq!(cavalry_cluster_reply_values(ptr::null(), &mut values), -1);
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_NULL);

        assert_eq!(cavalry_publication_close(publication), 0);
        assert_eq!(cavalry_subscription_close(subscription), 0);
        assert_eq!(cavalry_close(client), 0);
        assert_eq!(cavalry_context_close(context), 0);
    }
    driver.close().expect("the driver closes");
}

/// Every function of a session refuses a null handle, with the value the page gives
/// its return type.
#[test]
fn a_null_session_is_refused_by_every_function() {
    // SAFETY: nulls, which every function checks before anything else.
    unsafe {
        let null = ptr::null_mut::<cavalry_cluster_session_t>();
        let mut read = Read::default();
        let mut fault = std::mem::zeroed::<cavalry_fault_t>();
        let mut constants = std::mem::zeroed::<cavalry_cluster_session_constants_t>();
        let answers: [(&str, i64); 9] = [
            (
                "offer",
                cavalry_cluster_session_offer(null, b"x".as_ptr(), 1),
            ),
            (
                "poll",
                cavalry_cluster_session_poll(
                    null,
                    Some(collect),
                    (&mut read as *mut Read).cast(),
                    1,
                )
                .into(),
            ),
            (
                "fault",
                cavalry_cluster_session_fault(null, &mut fault).into(),
            ),
            (
                "leadership_term_id",
                cavalry_cluster_session_leadership_term_id(null),
            ),
            (
                "leader_member_id",
                cavalry_cluster_session_leader_member_id(null).into(),
            ),
            ("stale_replies", cavalry_cluster_session_stale_replies(null)),
            (
                "closed",
                i64::from(!cavalry_cluster_session_closed(null).is_null()),
            ),
            (
                "constants",
                cavalry_cluster_session_constants(null, &mut constants).into(),
            ),
            ("close", cavalry_cluster_session_close(null).into()),
        ];
        assert_eq!(
            answers,
            [
                ("offer", CAVALRY_PUBLICATION_ERROR),
                ("poll", -1),
                ("fault", -1),
                ("leadership_term_id", -1),
                ("leader_member_id", -1),
                ("stale_replies", -1),
                ("closed", 0),
                ("constants", -1),
                ("close", -1),
            ]
        );
        assert_eq!(cavalry_errcode(), CAVALRY_ERR_NULL);
    }
}
