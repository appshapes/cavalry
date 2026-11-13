//! A client's session with a cluster, through the client API against a real driver in
//! this process (`spec/cluster.md`, "The session protocol" and "Sessions"; ADR 0117).
//! The cluster is played by hand, by a second client of the same driver in a thread
//! of its own: a subscription on each member's `ingress` endpoint, and one publication
//! to the session's response channel, on which it answers as its script says. The
//! members' own side, which opens a session by an entry of the log, is `cavalry-raft`'s
//! to test, over three drivers.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cavalry_client::{Cavalry, ClusterError, ClusterSession, Offer, Publication, Subscription};
use cavalry_driver::Driver;
use cavalry_proto::cluster::{EGRESS_STREAM_ID, EventCode, INGRESS_STREAM_ID, Message, Stream};

// This test's ports, from the one file that says how they are taken and why, which is
// `cavalry-raft`'s: `crates/cavalry-raft/tests/ports/mod.rs`.
#[path = "../../cavalry-raft/tests/ports/mod.rs"]
mod ports;

/// A directory of its own per test, removed when the guard drops.
struct TestDir(PathBuf);

impl TestDir {
    fn new(test: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "cavalry-client-cluster-session-{}-{test}",
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

/// The session id and the term the played leader gives.
const SESSION: i64 = 7;
const TERM: i64 = 3;
/// The cluster's time the played leader stamps its replies with.
const TIME: i64 = 1_700_000_000_123;

/// What a played member answers a session connect with.
#[derive(Clone)]
enum Reply {
    /// It leads: the ok event.
    Ok,
    /// It follows the member with this id, −1 for none.
    Redirect(i32),
    /// It leads, and refuses with this detail.
    Refuse(&'static str),
    /// It leads a cluster of this protocol version.
    Version(i32),
    /// Nothing: the member is down.
    Silent,
}

/// What the played members read from their ingress endpoints, each with the place of
/// the member that read it.
#[derive(Default)]
struct Seen {
    connects: Vec<usize>,
    /// The term, the session id, the time and the payload of each session message.
    messages: Vec<(usize, i64, i64, i64, Vec<u8>)>,
    /// The term of each keepalive.
    keepalives: Vec<(usize, i64)>,
    closes: Vec<(usize, i64, i64)>,
}

/// Three members played by one thread.
struct Played {
    members: String,
    endpoints: String,
    /// What the thread read, as it sends it, and all of it so far.
    observed: Receiver<Seen>,
    seen: RefCell<Seen>,
    /// Bytes for the thread to send on the response channel.
    send: Sender<Vec<u8>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

fn encode(message: &Message<'_>) -> Vec<u8> {
    let mut bytes = vec![0u8; message.encoded_length()];
    let length = message.write(&mut bytes).expect("encodes");
    bytes.truncate(length);
    bytes
}

impl Played {
    /// Plays three members on `dir`'s driver, each answering a connect as `script`
    /// says, in the members string's order.
    fn start(dir: &Path, script: [Reply; 3]) -> Self {
        let ports = ports::free_ports(3);
        // Only the ingress endpoint of a member is used by a client; the four others
        // are in the string because a members string has them.
        let members = (0..3)
            .map(|id| {
                let port = ports[id];
                format!("{id},127.0.0.1:{port},127.0.0.1:1,127.0.0.1:2,127.0.0.1:3,127.0.0.1:4")
            })
            .collect::<Vec<_>>()
            .join("|");
        let endpoints = (0..3)
            .map(|id| format!("{id}=127.0.0.1:{}", ports[id]))
            .collect::<Vec<_>>()
            .join(",");
        let (observe, observed) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let (send, receive) = channel();
        let thread = {
            let (dir, stop, endpoints) = (dir.to_path_buf(), Arc::clone(&stop), endpoints.clone());
            std::thread::spawn(move || {
                play(&dir, &ports, &script, &endpoints, &observe, &receive, &stop)
            })
        };
        Self {
            members,
            endpoints,
            observed,
            seen: RefCell::new(Seen::default()),
            send,
            stop,
            thread: Some(thread),
        }
    }

    fn seen<T>(&self, read: impl FnOnce(&Seen) -> T) -> T {
        let mut seen = self.seen.borrow_mut();
        while let Ok(more) = self.observed.try_recv() {
            seen.connects.extend(more.connects);
            seen.messages.extend(more.messages);
            seen.keepalives.extend(more.keepalives);
            seen.closes.extend(more.closes);
        }
        read(&seen)
    }

    fn stop(mut self) {
        self.stop.store(true, Ordering::Release);
        self.thread
            .take()
            .expect("the thread")
            .join()
            .expect("the played members ended");
    }
}

/// The played members' loop: read every ingress endpoint, answer, send what the test
/// asked for, until told to stop.
fn play(
    dir: &Path,
    ports: &[u16],
    script: &[Reply; 3],
    endpoints: &str,
    observe: &Sender<Seen>,
    receive: &Receiver<Vec<u8>>,
    stop: &AtomicBool,
) {
    let mut client = Cavalry::connect_to(dir).expect("the played members connect");
    let mut ingress: Vec<Subscription> = ports
        .iter()
        .map(|port| {
            client
                .add_subscription(
                    &format!("cavalry:udp?endpoint=127.0.0.1:{port}"),
                    INGRESS_STREAM_ID,
                )
                .expect("an ingress subscription")
        })
        .collect();
    let mut egress: Option<Publication> = None;
    // What is yet to go out on the response channel, oldest first.
    let mut owed: Vec<Vec<u8>> = Vec::new();
    while !stop.load(Ordering::Acquire) {
        let mut read: Vec<(usize, Vec<u8>)> = Vec::new();
        for (member, subscription) in ingress.iter_mut().enumerate() {
            subscription
                .poll(|body, _| read.push((member, body.to_vec())), 16)
                .expect("the ingress polls");
        }
        for (member, bytes) in read {
            let mut seen = Seen::default();
            match Message::parse(Stream::Ingress, &bytes).expect("an ingress message") {
                Message::SessionConnect {
                    correlation_id,
                    response_stream_id,
                    response_channel,
                    ..
                } => {
                    seen.connects.push(member);
                    assert_eq!(response_stream_id, EGRESS_STREAM_ID);
                    if egress.is_none() {
                        let channel = std::str::from_utf8(response_channel).expect("a channel");
                        egress = Some(
                            client
                                .add_exclusive_publication(channel, EGRESS_STREAM_ID)
                                .expect("the egress publication"),
                        );
                    }
                    let event = |code, leader, version, detail: &[u8], session| {
                        encode(&Message::SessionEvent {
                            cluster_session_id: session,
                            correlation_id,
                            leadership_term_id: TERM,
                            leader_member_id: leader,
                            code,
                            version,
                            leader_heartbeat_timeout_ns: 10_000_000_000,
                            detail,
                        })
                    };
                    match &script[member] {
                        Reply::Ok => {
                            owed.push(event(EventCode::Ok, member as i32, 0, b"", SESSION))
                        }
                        Reply::Redirect(leader) => owed.push(event(
                            EventCode::Redirect,
                            *leader,
                            0,
                            endpoints.as_bytes(),
                            -1,
                        )),
                        Reply::Refuse(detail) => owed.push(event(
                            EventCode::Error,
                            member as i32,
                            0,
                            detail.as_bytes(),
                            -1,
                        )),
                        Reply::Version(version) => {
                            owed.push(event(EventCode::Ok, member as i32, *version, b"", SESSION))
                        }
                        Reply::Silent => {}
                    }
                }
                Message::SessionMessage {
                    leadership_term_id,
                    cluster_session_id,
                    timestamp,
                    payload,
                } => {
                    seen.messages.push((
                        member,
                        leadership_term_id,
                        cluster_session_id,
                        timestamp,
                        payload.to_vec(),
                    ));
                    // The service's reply: the payload back, with the cluster's time.
                    owed.push(encode(&Message::SessionMessage {
                        leadership_term_id,
                        cluster_session_id,
                        timestamp: TIME,
                        payload,
                    }));
                }
                Message::SessionKeepalive {
                    leadership_term_id, ..
                } => seen.keepalives.push((member, leadership_term_id)),
                Message::SessionClose {
                    leadership_term_id,
                    cluster_session_id,
                } => seen
                    .closes
                    .push((member, leadership_term_id, cluster_session_id)),
                other => panic!("not an ingress message: {other:?}"),
            }
            // The test may have ended its read: what was seen then goes nowhere.
            let _ = observe.send(seen);
        }
        while let Ok(bytes) = receive.try_recv() {
            owed.push(bytes);
        }
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
    client.close().expect("the played members close");
}

/// A driver, a client of it with a free port for its response channel, and the three
/// played members.
struct World {
    _dir: TestDir,
    driver: Driver,
    client: Cavalry,
    response_channel: String,
    played: Played,
}

impl World {
    fn start(test: &str, script: [Reply; 3]) -> Self {
        let dir = TestDir::new(test);
        let driver = Driver::start(dir.path(), dir.path().join("log")).expect("the driver starts");
        let client = Cavalry::connect_to(dir.path()).expect("connects");
        let played = Played::start(dir.path(), script);
        let response_channel = format!("cavalry:udp?endpoint=127.0.0.1:{}", ports::free_port());
        Self {
            _dir: dir,
            driver,
            client,
            response_channel,
            played,
        }
    }

    fn connect(&mut self) -> Result<ClusterSession, ClusterError> {
        self.client
            .connect_cluster(&self.played.members, &self.response_channel)
    }

    fn close(self) {
        self.played.stop();
        self.client.close().expect("closes");
        self.driver.close().expect("the driver closes");
    }
}

/// Polls `session` until `done`, collecting what it is handed.
fn poll_until(
    session: &mut ClusterSession,
    read: &mut Vec<(Vec<u8>, i64)>,
    mut done: impl FnMut(&ClusterSession, &[(Vec<u8>, i64)]) -> bool,
) -> bool {
    wait_until(Duration::from_secs(5), || {
        session
            .poll(|payload, time| read.push((payload.to_vec(), time)), 10)
            .expect("the session polls");
        done(session, read)
    })
}

/// A session with the first member, which leads: the leader's session id and term, a
/// message sent with them and a time of 0, the reply read with the cluster's time, a
/// message longer than one frame refused, a keepalive after 500 ms of nothing sent,
/// and a session close when the session is closed.
#[test]
fn a_session_opens_sends_reads_keeps_alive_and_closes() {
    let mut world = World::start("open", [Reply::Ok, Reply::Silent, Reply::Silent]);
    let mut session = world.connect().expect("the session opens");
    assert_eq!(
        (
            session.session_id(),
            session.leadership_term_id(),
            session.leader_member_id(),
            session.closed()
        ),
        (SESSION, TERM, 0, None)
    );
    assert_eq!(world.played.seen(|seen| seen.connects.clone()), [0]);

    // One frame: 1,408 bytes of MTU less the frame header and the session message
    // header.
    assert_eq!(session.max_message_length(), 1408 - 32 - 32);
    let too_long = vec![0u8; session.max_message_length() + 1];
    assert!(
        matches!(session.offer(&too_long), Err(Offer::TooLong { .. })),
        "a message of more than one frame is refused"
    );
    let longest = vec![9u8; session.max_message_length()];
    session
        .offer(b"ping")
        .expect("the leader takes the message");
    session.offer(&longest).expect("and the longest one");
    let mut read = Vec::new();
    assert!(poll_until(&mut session, &mut read, |_, read| read.len() == 2));
    assert_eq!(read, [(b"ping".to_vec(), TIME), (longest.clone(), TIME)]);
    assert_eq!(
        world.played.seen(|seen| seen.messages.clone()),
        [
            (0, TERM, SESSION, 0, b"ping".to_vec()),
            (0, TERM, SESSION, 0, longest)
        ],
        "to the leader, with the session's term and id and a time of 0"
    );

    // Nothing sent for the keepalive interval: a keepalive, with the term, and no
    // sooner.
    let quiet = Instant::now();
    assert!(poll_until(&mut session, &mut read, |_, _| {
        world.played.seen(|seen| !seen.keepalives.is_empty())
    }));
    assert!(
        quiet.elapsed() >= Duration::from_millis(450),
        "a keepalive {:?} after the last message",
        quiet.elapsed()
    );
    assert_eq!(world.played.seen(|seen| seen.keepalives[0]), (0, TERM));

    session.close().expect("the session closes");
    assert!(wait_until(Duration::from_secs(5), || {
        world
            .played
            .seen(|seen| seen.closes == [(0, TERM, SESSION)])
    }));
    world.close();
}

/// A member that knows no leader is passed, and a redirect is followed to the member
/// it names: the connects go to the first member, the second and the third, and the
/// session is with the third.
#[test]
fn a_redirect_is_followed_and_a_member_with_no_leader_is_passed() {
    let mut world = World::start(
        "redirect",
        [Reply::Redirect(-1), Reply::Redirect(2), Reply::Ok],
    );
    let mut session = world.connect().expect("the session opens");
    assert_eq!(world.played.seen(|seen| seen.connects.clone()), [0, 1, 2]);
    assert_eq!(session.leader_member_id(), 2);
    session.offer(b"to the leader").expect("taken");
    assert!(wait_until(Duration::from_secs(5), || {
        world.played.seen(|seen| seen.messages.len() == 1)
    }));
    assert_eq!(world.played.seen(|seen| seen.messages[0].0), 2);
    session.close().expect("the session closes");
    world.close();
}

/// What stops a connect at once: the leader's refusal, with its reason; a cluster of
/// another protocol version; and a members string that fails a check, before
/// anything is sent. Each error says what to do and where it is explained.
#[test]
fn a_refusal_another_version_and_a_bad_members_string_are_reported() {
    let mut world = World::start(
        "refused",
        [
            Reply::Refuse("concurrent session limit"),
            Reply::Silent,
            Reply::Silent,
        ],
    );
    let error = world.connect().expect_err("refused");
    assert!(
        matches!(&error, ClusterError::Refused { detail } if detail == "concurrent session limit"),
        "{error}"
    );
    assert_eq!(error.reason(), "refused");
    let text = error.to_string();
    assert!(
        text.contains("`concurrent session limit`")
            && text.contains("docs/reference/cluster.md#sessions"),
        "{text}"
    );
    let error = world
        .client
        .connect_cluster("0,127.0.0.1:1", &world.response_channel)
        .expect_err("a member of two fields");
    assert!(matches!(error, ClusterError::Members(_)), "{error}");
    assert_eq!(
        world.played.seen(|seen| seen.connects.len()),
        1,
        "nothing was sent for the bad string"
    );
    world.close();

    let mut world = World::start(
        "version",
        [Reply::Version(1 << 16), Reply::Silent, Reply::Silent],
    );
    let error = world.connect().expect_err("another major version");
    assert!(
        matches!(error, ClusterError::Version { version } if version == 1 << 16),
        "{error}"
    );
    assert!(error.to_string().contains("1.0.0"), "{error}");
    world.close();
}

/// A new leader event turns the session to the member it names, with its term: the
/// next thing sent is a keepalive to that member, at once, and a message after it
/// carries the new term. An event for another session, or for an older term, changes
/// nothing. A session event of code closed closes the session with the cluster's
/// reason, and an offer is then refused.
#[test]
fn a_new_leader_event_turns_the_session_and_a_closed_event_closes_it() {
    let mut world = World::start("new-leader", [Reply::Ok, Reply::Silent, Reply::Silent]);
    let mut session = world.connect().expect("the session opens");
    let mut read = Vec::new();
    let new_leader = |term: i64, session: i64, leader: i32, endpoints: &str| {
        encode(&Message::NewLeaderEvent {
            leadership_term_id: term,
            cluster_session_id: session,
            leader_member_id: leader,
            ingress_endpoints: endpoints.as_bytes(),
        })
    };
    let endpoints = world.played.endpoints.clone();
    // Another session's, then this session's for an older term: neither is followed.
    world
        .played
        .send
        .send(new_leader(TERM + 5, SESSION + 1, 1, &endpoints))
        .expect("sent");
    world
        .played
        .send
        .send(new_leader(TERM - 1, SESSION, 1, &endpoints))
        .expect("sent");
    world
        .played
        .send
        .send(new_leader(TERM + 1, SESSION, 2, &endpoints))
        .expect("sent");
    assert!(poll_until(&mut session, &mut read, |session, _| {
        session.leader_member_id() == 2
    }));
    assert_eq!(session.leadership_term_id(), TERM + 1);
    // The keepalive goes to the new leader with the new term, well inside the
    // keepalive interval.
    let turned = Instant::now();
    assert!(poll_until(&mut session, &mut read, |_, _| {
        world.played.seen(|seen| !seen.keepalives.is_empty())
    }));
    assert!(turned.elapsed() < Duration::from_millis(400));
    assert_eq!(world.played.seen(|seen| seen.keepalives[0]), (2, TERM + 1));
    session.offer(b"after").expect("taken");
    assert!(wait_until(Duration::from_secs(5), || {
        world.played.seen(|seen| seen.messages.len() == 1)
    }));
    assert_eq!(
        world.played.seen(|seen| seen.messages[0].clone()),
        (2, TERM + 1, SESSION, 0, b"after".to_vec())
    );

    let closed = |session: i64| {
        encode(&Message::SessionEvent {
            cluster_session_id: session,
            correlation_id: -1,
            leadership_term_id: TERM + 1,
            leader_member_id: 2,
            code: EventCode::Closed,
            version: 0,
            leader_heartbeat_timeout_ns: 10_000_000_000,
            detail: b"TIMEOUT",
        })
    };
    world.played.send.send(closed(SESSION + 1)).expect("sent");
    world.played.send.send(closed(SESSION)).expect("sent");
    assert!(poll_until(&mut session, &mut read, |session, _| {
        session.closed().is_some()
    }));
    assert_eq!(session.closed(), Some("TIMEOUT"));
    assert_eq!(session.offer(b"too late"), Err(Offer::Closed));
    // A closed session sends no keepalive and, when it is closed, no session close.
    let keepalives = world.played.seen(|seen| seen.keepalives.len());
    let quiet = Instant::now();
    while quiet.elapsed() < Duration::from_millis(700) {
        session.poll(|_, _| {}, 10).expect("polls");
        std::thread::sleep(Duration::from_millis(1));
    }
    session.close().expect("the session closes");
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        world
            .played
            .seen(|seen| (seen.keepalives.len(), seen.closes.len())),
        (keepalives, 0)
    );
    world.close();
}

/// A cluster no member of which answers: every member is asked in turn, each connect
/// waits the session connect timeout, and the call gives up after 20 s and says what
/// to check.
#[test]
fn a_cluster_that_does_not_answer_is_given_up_after_twenty_seconds() {
    let mut world = World::start("silent", [Reply::Silent, Reply::Silent, Reply::Silent]);
    let asked = Instant::now();
    let error = world.connect().expect_err("no member answers");
    let waited = asked.elapsed();
    assert!(
        matches!(error, ClusterError::ConnectTimeout { connects: 4 }),
        "{error}"
    );
    assert!(
        waited >= Duration::from_secs(20) && waited < Duration::from_secs(23),
        "gave up after {waited:?}"
    );
    assert_eq!(error.reason(), "connect-timeout");
    assert!(
        error
            .to_string()
            .contains("no member opened a session in 20 s"),
        "{error}"
    );
    // The first member, the second, the third, and the first again.
    assert_eq!(
        world.played.seen(|seen| seen.connects.clone()),
        [0, 1, 2, 0]
    );
    world.close();
}
