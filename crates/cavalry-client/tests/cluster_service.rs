//! A cluster's service, through the client API against a real driver in this process
//! (`spec/cluster.md`, "The service"; ADR 0118). The member is played by a client of
//! the same driver: a recording of a stream it appends the log's entries to, the four
//! counters a service reads of it, and the file of its log's recordings. A session's
//! client is a subscription on the session's response channel. The members' own side,
//! and a service beside each of three, is `cavalry-raft`'s to test, over three
//! drivers.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cavalry_client::{
    Action, Cavalry, ClusterError, ClusterService, Committed, Counter, Entry, Publication,
    Recording, Reply, Subscription,
};
use cavalry_driver::Driver;
use cavalry_proto::cluster::{
    CloseReason, EGRESS_STREAM_ID, LOG_RECORDINGS_FILE_PREFIX, LogRecording,
    MAX_LOG_RECORDINGS_LENGTH, MAX_SESSIONS, Message, Stream, TimeUnit, write_log_recordings,
};
use cavalry_shm::counters::generated::{
    CLUSTER_APPEND_POSITION, CLUSTER_COMMIT_POSITION, CLUSTER_LEADERSHIP_TERM_ID,
    CLUSTER_LOG_RECORDING_ID, CLUSTER_REPLIES_DROPPED,
};

// This test's ports, from the one file that says how they are taken and why, which is
// `cavalry-raft`'s: `crates/cavalry-raft/tests/ports/mod.rs`.
#[path = "../../cavalry-raft/tests/ports/mod.rs"]
mod ports;

/// A directory of its own per test, removed when the guard drops.
struct TestDir(PathBuf);

impl TestDir {
    fn new(test: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "cavalry-client-cluster-service-{}-{test}",
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

/// The played member's id, and the id of another member that leads a term.
const MEMBER: i32 = 3;
const OTHER: i32 = 5;
/// The cluster's time of the log's first entry; each entry after it is a millisecond
/// later.
const TIME: i64 = 1_700_000_000_000;

fn encode(message: &Message<'_>) -> Vec<u8> {
    let mut bytes = vec![0u8; message.encoded_length()];
    let length = message.write(&mut bytes).expect("encodes");
    bytes.truncate(length);
    bytes
}

/// A cluster member as a service sees one: a driver, the member's client, the log it
/// appends to and records, its counters and its driver directory.
struct Member {
    client: Cavalry,
    log: Publication,
    recording: Recording,
    /// The position after the last entry appended.
    end: i64,
    /// How many entries were appended.
    entries: i64,
    leadership_term_id: Counter,
    append_position: Counter,
    log_recording_id: Counter,
    commit_position: Counter,
    dir: PathBuf,
    driver: Driver,
    _test: TestDir,
}

impl Member {
    /// A driver and a member on it with an empty log that it names no recording of
    /// yet. Its counters are added in a consensus module's order: the term and the
    /// append position, then the recording's id, holding −1, then the commit position.
    fn start(test: &str) -> Self {
        let test = TestDir::new(test);
        let dir = test.path().join("driver");
        let driver = Driver::start(&dir, test.path().join("log")).expect("the driver starts");
        let mut client = Cavalry::connect_to(&dir).expect("the member's client connects");
        let key = MEMBER.to_le_bytes();
        let mut add = |id: i32, name: &str, value: i64| {
            let counter = client
                .add_counter(id, &key, &format!("{name} {MEMBER}"))
                .expect("the counter");
            counter.store_release(value).expect("stores");
            counter
        };
        let leadership_term_id = add(
            CLUSTER_LEADERSHIP_TERM_ID.0,
            "cluster.leadership_term_id",
            -1,
        );
        let append_position = add(CLUSTER_APPEND_POSITION.0, "cluster.append_position", 0);
        let log_recording_id = add(CLUSTER_LOG_RECORDING_ID.0, "cluster.log_recording_id", -1);
        let commit_position = add(CLUSTER_COMMIT_POSITION.0, "cluster.commit_position", 0);
        let log = client
            .add_exclusive_publication("cavalry:ipc", 100)
            .expect("the log publication");
        let recording = client
            .start_recording("cavalry:ipc", 100)
            .expect("the log's recording");
        Member {
            client,
            log,
            recording,
            end: 0,
            entries: 0,
            leadership_term_id,
            append_position,
            log_recording_id,
            commit_position,
            dir,
            driver,
            _test: test,
        }
    }

    /// The cluster's time of the next entry.
    fn time(&self) -> i64 {
        TIME + self.entries
    }

    /// Appends `bytes` as one entry, waits for the recording to have synced it, and
    /// stores the append position. Returns the position the entry begins at and its
    /// time.
    fn append_bytes(&mut self, bytes: &[u8]) -> (i64, i64) {
        let (begins, time) = (self.end, self.time());
        let mut after = self.end;
        wait("the offer goes through", || match self.log.offer(bytes) {
            Ok(position) => {
                after = position;
                true
            }
            Err(_) => false,
        });
        self.end = after;
        self.entries += 1;
        wait("the recording is synced", || {
            self.recording.synced_position() == self.end
        });
        self.append_position
            .store_release(self.end)
            .expect("stores");
        (begins, time)
    }

    /// The first entry of `term`, which `leader` leads, and the member's term stored.
    fn term(&mut self, term: i64, leader: i32) -> (i64, i64) {
        self.leadership_term_id.store_release(term).expect("stores");
        let entry = encode(&Message::NewLeadershipTermEvent {
            leadership_term_id: term,
            log_position: self.end,
            timestamp: self.time(),
            term_base_log_position: self.end,
            leader_member_id: leader,
            log_session_id: self.log.session_id(),
            time_unit: TimeUnit::Millis,
            app_version: 0,
        });
        self.append_bytes(&entry)
    }

    /// A session open event of `term` for `session`, with its response channel.
    fn open(&mut self, term: i64, session: i64, channel: &str) -> (i64, i64) {
        let entry = encode(&Message::SessionOpenEvent {
            leadership_term_id: term,
            correlation_id: 1,
            cluster_session_id: session,
            timestamp: self.time(),
            response_stream_id: EGRESS_STREAM_ID,
            response_channel: channel.as_bytes(),
            encoded_principal: &[],
        });
        self.append_bytes(&entry)
    }

    /// A session message of `term` from `session`.
    fn message(&mut self, term: i64, session: i64, payload: &[u8]) -> (i64, i64) {
        let entry = encode(&Message::SessionMessage {
            leadership_term_id: term,
            cluster_session_id: session,
            timestamp: self.time(),
            payload,
        });
        self.append_bytes(&entry)
    }

    /// A session close event of `term` for `session`.
    fn close_session(&mut self, term: i64, session: i64, reason: CloseReason) -> (i64, i64) {
        let entry = encode(&Message::SessionCloseEvent {
            leadership_term_id: term,
            cluster_session_id: session,
            timestamp: self.time(),
            close_reason: reason,
        });
        self.append_bytes(&entry)
    }

    /// Names the log's recording as the member's live one, after its file.
    fn name(&self) {
        let live = self.recording.recording_id();
        let mut bytes = vec![0; MAX_LOG_RECORDINGS_LENGTH];
        let length = write_log_recordings(
            &mut bytes,
            MEMBER,
            live,
            0,
            std::iter::empty::<LogRecording>(),
        )
        .expect("the file is written");
        std::fs::write(
            self.dir
                .join(format!("{LOG_RECORDINGS_FILE_PREFIX}{MEMBER}")),
            &bytes[..length],
        )
        .expect("the file is stored");
        self.log_recording_id.store_release(live).expect("stores");
    }

    /// Stores the end of the log as the member's commit position.
    fn commit(&self) {
        self.commit_position
            .store_release(self.end)
            .expect("stores");
    }

    fn close(self) {
        self.client.close().expect("the member's client closes");
        self.driver.close().expect("the driver closes");
    }
}

/// An entry as a handler was handed it, owned.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Kind {
    Message(i64, Vec<u8>),
    Open(i64),
    Close(i64, bool),
    Term(i32),
    Other,
}

/// What a service's handler saw.
#[derive(Default)]
struct Seen {
    /// Each entry taken: its position, its term, its time and what it was.
    entries: Vec<(i64, i64, i64, Kind)>,
    /// What became of the reply to each session message taken, by its position.
    replies: Vec<(i64, Reply)>,
    /// How many times a reply was to be tried again.
    retries: usize,
}

fn kind(entry: &Entry<'_>) -> Kind {
    match *entry {
        Entry::SessionMessage {
            session_id,
            payload,
        } => Kind::Message(session_id, payload.to_vec()),
        Entry::SessionOpen { session_id } => Kind::Open(session_id),
        Entry::SessionClose {
            session_id,
            timed_out,
        } => Kind::Close(session_id, timed_out),
        Entry::NewLeadershipTerm { leader_member_id } => Kind::Term(leader_member_id),
        Entry::Other => Kind::Other,
    }
}

/// One poll of `service` with a handler that answers every session message with its
/// payload, to the session that sent it, and has the entry handed over again when
/// the reply is to be tried again.
fn poll(service: &mut ClusterService, client: &mut Cavalry, seen: &mut Seen) -> usize {
    service
        .poll(
            client,
            |committed: &Committed<'_>, replies| {
                if let Entry::SessionMessage {
                    session_id,
                    payload,
                } = committed.entry
                {
                    let reply = replies.reply(session_id, payload);
                    if reply == Reply::Retry {
                        seen.retries += 1;
                        return Action::Abort;
                    }
                    seen.replies.push((committed.position, reply));
                }
                seen.entries.push((
                    committed.position,
                    committed.leadership_term_id,
                    committed.timestamp,
                    kind(&committed.entry),
                ));
                Action::Continue
            },
            16,
        )
        .expect("the service polls")
}

/// Polls until the service has taken every entry of the member's log.
fn poll_to_end(
    service: &mut ClusterService,
    client: &mut Cavalry,
    seen: &mut Seen,
    member: &Member,
) {
    wait("the service reads the log to its end", || {
        poll(service, client, seen);
        service.position() == member.end
    });
}

/// The value of the member's `cluster.replies_dropped`, or `None` while no service
/// has added it: a service adds it when it first opens the member's log.
fn dropped(client: &Cavalry) -> Option<i64> {
    let counters = client.counters();
    (0..counters.capacity()).find_map(|id| {
        let record = counters.record(id).ok().flatten()?;
        (record.type_id() == CLUSTER_REPLIES_DROPPED.0
            && record.key().starts_with(&MEMBER.to_le_bytes()))
        .then(|| counters.read(id).expect("the counter reads"))
    })
}

/// Polls until the service has opened the member's log, which is when it reads what
/// the member holds already: its term and its append position.
fn open_log(service: &mut ClusterService, client: &mut Cavalry, seen: &mut Seen) {
    wait("the service opens the member's log", || {
        poll(service, client, seen);
        dropped(client).is_some()
    });
}

/// What a session's client read from its response channel: the term, the session,
/// the time and the payload of each session message.
type Read = Vec<(i64, i64, i64, Vec<u8>)>;

fn read(egress: &mut Subscription, into: &mut Read) {
    egress
        .poll(
            |body, _| match Message::parse(Stream::Egress, body) {
                Ok(Message::SessionMessage {
                    leadership_term_id,
                    cluster_session_id,
                    timestamp,
                    payload,
                }) => into.push((
                    leadership_term_id,
                    cluster_session_id,
                    timestamp,
                    payload.to_vec(),
                )),
                other => panic!("not a session message: {other:?}"),
            },
            16,
        )
        .expect("the response channel polls");
}

fn response_channel() -> String {
    format!("cavalry:udp?endpoint=127.0.0.1:{}", ports::free_port())
}

/// The service beside the member that leads a term answers the term's entries, with
/// the entry's term and the cluster's time of it, and answers none of a term another
/// member leads. Before the member names a recording there is nothing to read.
#[test]
fn the_service_beside_a_terms_leader_answers_its_entries_and_no_other_terms() {
    let mut member = Member::start("answers");
    let mut client = Cavalry::connect_to(&member.dir).expect("the service's client");
    let mut service = client.cluster_service();
    let mut seen = Seen::default();
    assert_eq!(poll(&mut service, &mut client, &mut seen), 0);
    assert_eq!(dropped(&client), None, "no log is open yet");

    let channel = response_channel();
    let mut session = Cavalry::connect_to(&member.dir).expect("the session's client");
    let mut egress = session
        .add_subscription(&channel, EGRESS_STREAM_ID)
        .expect("the response channel");
    member.name();
    open_log(&mut service, &mut client, &mut seen);
    assert_eq!((service.position(), service.answers()), (0, false));

    let term = member.term(0, MEMBER);
    let opened = member.open(0, 0, &channel);
    let ping = member.message(0, 0, b"ping");
    // Nothing is read above the commit position.
    assert_eq!(poll(&mut service, &mut client, &mut seen), 0);
    member.commit();
    poll_to_end(&mut service, &mut client, &mut seen, &member);
    assert_eq!(
        seen.entries,
        [
            (term.0, 0, term.1, Kind::Term(MEMBER)),
            (opened.0, 0, opened.1, Kind::Open(0)),
            (ping.0, 0, ping.1, Kind::Message(0, b"ping".to_vec())),
        ]
    );
    assert_eq!(seen.replies, [(ping.0, Reply::Sent)]);
    assert_eq!((service.answers(), service.sessions()), (true, 1));
    let mut replies = Read::new();
    wait("the client reads its reply", || {
        read(&mut egress, &mut replies);
        !replies.is_empty()
    });
    assert_eq!(replies, [(0, 0, ping.1, b"ping".to_vec())]);

    // Another member leads the next term: this service applies its entries and
    // answers none.
    let next = member.term(1, OTHER);
    let after = member.message(1, 0, b"after");
    let closed = member.close_session(1, 0, CloseReason::Timeout);
    member.commit();
    poll_to_end(&mut service, &mut client, &mut seen, &member);
    assert_eq!(
        seen.entries[3..],
        [
            (next.0, 1, next.1, Kind::Term(OTHER)),
            (after.0, 1, after.1, Kind::Message(0, b"after".to_vec())),
            (closed.0, 1, closed.1, Kind::Close(0, true)),
        ]
    );
    assert_eq!(seen.replies[1..], [(after.0, Reply::NotAnswering)]);
    assert_eq!((service.answers(), service.sessions()), (false, 0));
    let quiet = Instant::now() + Duration::from_millis(100);
    while Instant::now() < quiet {
        read(&mut egress, &mut replies);
    }
    assert_eq!(replies.len(), 1, "nothing more reached the client");
    assert_eq!(dropped(&client), Some(0));

    service.close(&mut client).expect("the service closes");
    egress.close().expect("the subscription closes");
    session.close().expect("the session's client closes");
    client.close().expect("the service's client closes");
    member.close();
}

/// A service that starts beside a member with a log applies what the member held
/// already and answers none of it: the terms below the member's term then, and the
/// entries of that term below its append position then. What the member appends
/// after is answered.
#[test]
fn a_service_applies_the_history_it_finds_and_answers_what_comes_after() {
    let mut member = Member::start("history");
    let channel = response_channel();
    let mut session = Cavalry::connect_to(&member.dir).expect("the session's client");
    let mut egress = session
        .add_subscription(&channel, EGRESS_STREAM_ID)
        .expect("the response channel");
    member.term(1, MEMBER);
    member.open(1, 0, &channel);
    let old_term = member.message(1, 0, b"of an old term");
    member.term(2, MEMBER);
    let before = member.message(2, 0, b"before the service");
    member.commit();
    member.name();

    let mut client = Cavalry::connect_to(&member.dir).expect("the service's client");
    let mut service = client.cluster_service();
    let mut seen = Seen::default();
    poll_to_end(&mut service, &mut client, &mut seen, &member);
    assert_eq!(seen.entries.len(), 5, "every entry is applied");
    assert_eq!(
        seen.replies,
        [
            (old_term.0, Reply::NotAnswering),
            (before.0, Reply::NotAnswering)
        ]
    );
    assert_eq!((service.answers(), service.sessions()), (false, 1));

    let live = member.message(2, 0, b"live");
    member.commit();
    poll_to_end(&mut service, &mut client, &mut seen, &member);
    assert_eq!(seen.replies[2..], [(live.0, Reply::Sent)]);
    assert!(service.answers());
    let mut replies = Read::new();
    wait("the client reads its reply", || {
        read(&mut egress, &mut replies);
        !replies.is_empty()
    });
    let quiet = Instant::now() + Duration::from_millis(100);
    while Instant::now() < quiet {
        read(&mut egress, &mut replies);
    }
    assert_eq!(replies, [(2, 0, live.1, b"live".to_vec())]);
    assert_eq!(dropped(&client), Some(0));

    service.close(&mut client).expect("the service closes");
    egress.close().expect("the subscription closes");
    session.close().expect("the session's client closes");
    client.close().expect("the service's client closes");
    member.close();
}

/// A reply to a session whose client is not there is tried again for the keepalive
/// interval, 500 ms, and then dropped and counted; the next reply to that session is
/// dropped at once, so that one client holds the log back once; and when the client
/// is there the session is answered again.
#[test]
fn a_reply_no_client_takes_is_tried_for_the_keepalive_interval_and_then_dropped() {
    let mut member = Member::start("dropped");
    let mut client = Cavalry::connect_to(&member.dir).expect("the service's client");
    let mut service = client.cluster_service();
    let mut seen = Seen::default();
    member.name();
    open_log(&mut service, &mut client, &mut seen);

    let channel = response_channel();
    member.term(0, MEMBER);
    member.open(0, 0, &channel);
    let lost = member.message(0, 0, b"lost");
    member.commit();
    let began = Instant::now();
    poll_to_end(&mut service, &mut client, &mut seen, &member);
    let took = began.elapsed();
    assert!(seen.retries > 0, "the reply was tried again");
    assert!(
        took >= Duration::from_millis(500) && took < Duration::from_secs(5),
        "the entry was held for the keepalive interval: {took:?}"
    );
    assert_eq!(seen.replies, [(lost.0, Reply::Dropped)]);
    assert_eq!(dropped(&client), Some(1));

    let retries = seen.retries;
    let again = member.message(0, 0, b"lost too");
    member.commit();
    poll_to_end(&mut service, &mut client, &mut seen, &member);
    assert_eq!(
        seen.retries, retries,
        "a session that takes nothing is not waited for"
    );
    assert_eq!(seen.replies[1..], [(again.0, Reply::Dropped)]);
    assert_eq!(dropped(&client), Some(2));

    // The client is there now: a reply is sent once its stream has connected.
    let mut session = Cavalry::connect_to(&member.dir).expect("the session's client");
    let mut egress = session
        .add_subscription(&channel, EGRESS_STREAM_ID)
        .expect("the response channel");
    let mut heard = (0, 0);
    wait("a reply is sent to the client that is there", || {
        heard = member.message(0, 0, b"heard");
        member.commit();
        poll_to_end(&mut service, &mut client, &mut seen, &member);
        seen.replies.last() == Some(&(heard.0, Reply::Sent))
    });
    assert_eq!(seen.retries, retries);
    let mut replies = Read::new();
    wait("the client reads its reply", || {
        read(&mut egress, &mut replies);
        !replies.is_empty()
    });
    assert_eq!(replies, [(0, 0, heard.1, b"heard".to_vec())]);

    service.close(&mut client).expect("the service closes");
    egress.close().expect("the subscription closes");
    session.close().expect("the session's client closes");
    client.close().expect("the service's client closes");
    member.close();
}

/// What a reply is refused for besides a client that does not take it: a session
/// that is not open, before its open event and after its close event, and a payload
/// longer than one frame carries. A payload of the longest length is sent. An entry
/// no leader of this version appends is handed over as such, and an entry that is not
/// of the page stops the service at its position.
#[test]
fn a_reply_names_a_session_the_log_holds_open_and_fits_one_frame() {
    let mut member = Member::start("refusals");
    let mut client = Cavalry::connect_to(&member.dir).expect("the service's client");
    let mut service = client.cluster_service();
    member.name();
    open_log(&mut service, &mut client, &mut Seen::default());

    let channel = response_channel();
    let mut session = Cavalry::connect_to(&member.dir).expect("the session's client");
    let mut egress = session
        .add_subscription(&channel, EGRESS_STREAM_ID)
        .expect("the response channel");
    member.term(0, MEMBER);
    member.open(0, 0, &channel);
    member.message(0, 0, b"ask");
    let timer = encode(&Message::TimerEvent {
        leadership_term_id: 0,
        correlation_id: 9,
        timestamp: member.time(),
    });
    let timer = member.append_bytes(&timer);
    member.close_session(0, 0, CloseReason::ClientAction);
    member.message(0, 0, b"late");
    member.commit();

    // For "ask": a reply to a session that never opened, one too long by a byte, and
    // one of the longest length. For "late": a reply to the session, now closed.
    let (full, long) = (vec![7u8; 1_344], vec![7u8; 1_345]);
    let mut outcomes = Vec::new();
    let mut kinds = Vec::new();
    wait("the service reads the log to its end", || {
        service
            .poll(
                &mut client,
                |committed, replies| {
                    if let Entry::SessionMessage {
                        session_id,
                        payload,
                    } = committed.entry
                    {
                        if payload == b"ask" {
                            if replies.reply(session_id, &full) == Reply::Retry {
                                return Action::Abort;
                            }
                            outcomes.push(Reply::Sent);
                            outcomes.push(replies.reply(9, b"nobody"));
                            outcomes.push(replies.reply(session_id, &long));
                        } else {
                            outcomes.push(replies.reply(session_id, payload));
                        }
                    }
                    kinds.push((committed.position, kind(&committed.entry)));
                    Action::Continue
                },
                16,
            )
            .expect("the service polls");
        service.position() == member.end
    });
    assert_eq!(
        outcomes,
        [
            Reply::Sent,
            Reply::NoSession,
            Reply::TooLong {
                found: 1_345,
                limit: 1_344
            },
            Reply::NoSession,
        ]
    );
    assert_eq!(kinds[3], (timer.0, Kind::Other));
    assert_eq!(kinds[4].1, Kind::Close(0, false));
    assert_eq!(service.sessions(), 0);
    let mut replies = Read::new();
    wait("the client reads the longest reply", || {
        read(&mut egress, &mut replies);
        !replies.is_empty()
    });
    assert_eq!(replies[0].3, full);
    assert_eq!(
        dropped(&client),
        Some(0),
        "a refusal that is the caller's is not a drop"
    );

    // An entry that is not of the page: the service stops there, and says where.
    let (stray, _) = member.append_bytes(&[0xFF; 24]);
    member.commit();
    for _ in 0..3 {
        let stopped = service
            .poll(&mut client, |_, _| Action::Continue, 16)
            .expect_err("the entry is not the page's");
        assert!(
            matches!(stopped, ClusterError::Entry { position } if position == stray),
            "{stopped}"
        );
        assert_eq!(service.position(), stray, "the entry is not taken");
    }

    service.close(&mut client).expect("the service closes");
    egress.close().expect("the subscription closes");
    session.close().expect("the session's client closes");
    client.close().expect("the service's client closes");
    member.close();
}

/// The service holds as many sessions as a cluster has, 10. A session open event past
/// them, which no leader appends, opens nothing; a session's close gives its place to
/// the next.
#[test]
fn a_service_holds_ten_sessions_and_no_more() {
    let mut member = Member::start("capacity");
    let mut client = Cavalry::connect_to(&member.dir).expect("the service's client");
    let mut service = client.cluster_service();
    member.name();
    open_log(&mut service, &mut client, &mut Seen::default());

    let channel = response_channel();
    member.term(0, MEMBER);
    for session in 0..=MAX_SESSIONS as i64 {
        member.open(0, session, &channel);
    }
    // A session that is open is offered its reply, which nobody is there to take; a
    // session that is not has none. The handler never has an entry handed over again.
    let mut outcomes = Vec::new();
    let mut ask = |member: &mut Member, service: &mut ClusterService, sessions: &[i64]| {
        member.message(0, -1, b"ask");
        member.commit();
        outcomes.clear();
        wait("the service reads the log to its end", || {
            service
                .poll(
                    &mut client,
                    |committed, replies| {
                        if let Entry::SessionMessage { payload, .. } = committed.entry {
                            for &session in sessions {
                                outcomes.push(replies.reply(session, payload));
                            }
                        }
                        Action::Continue
                    },
                    32,
                )
                .expect("the service polls");
            service.position() == member.end
        });
        outcomes.clone()
    };
    assert_eq!(
        ask(&mut member, &mut service, &[9, 10]),
        [Reply::Retry, Reply::NoSession]
    );
    assert_eq!(service.sessions(), MAX_SESSIONS);

    member.close_session(0, 4, CloseReason::ClientAction);
    member.open(0, 11, &channel);
    assert_eq!(
        ask(&mut member, &mut service, &[4, 11]),
        [Reply::NoSession, Reply::Retry]
    );
    assert_eq!(service.sessions(), MAX_SESSIONS);

    service.close(&mut client).expect("the service closes");
    client.close().expect("the service's client closes");
    member.close();
}
