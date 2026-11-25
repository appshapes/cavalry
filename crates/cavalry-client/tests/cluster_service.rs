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
    Action, Cavalry, ClientError, Closed, ClusterError, ClusterService, Committed, Counter, Entry,
    Publication, Recording, Reply, Subscription,
};
use cavalry_driver::Driver;
use cavalry_proto::cluster::{
    CONSENSUS_MODULE_STREAM_ID, CloseReason, ClusterAction, EGRESS_STREAM_ID,
    LOG_RECORDINGS_FILE_PREFIX, LogRecording, MAX_LOG_RECORDINGS_LENGTH, MAX_SESSIONS, Message,
    Stream, TimeUnit, write_log_recordings,
};
use cavalry_shm::counters::CountersError;
use cavalry_shm::counters::generated::{
    CLUSTER_APPEND_POSITION, CLUSTER_COMMIT_POSITION, CLUSTER_LEADER_MEMBER_ID,
    CLUSTER_LEADERSHIP_TERM_ID, CLUSTER_LOG_RECORDING_ID, CLUSTER_REPLIES_DROPPED,
    CLUSTER_SERVICE_TIMER_CANCELS_DROPPED, CLUSTER_SERVICE_TIMER_EVENTS_PASSED_OVER,
    CLUSTER_SERVICE_TIMERS,
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
    /// `cluster.leader_member_id`: the member this one leads as or follows.
    leader_member_id: Counter,
    append_position: Counter,
    log_recording_id: Counter,
    commit_position: Counter,
    dir: PathBuf,
    driver: Driver,
    _test: TestDir,
}

impl Member {
    /// A driver and a member on it with an empty log that it names no recording of
    /// yet. Its counters are added in a consensus module's order: the term, the id of
    /// the member it leads as or follows, holding −1, and the append position, then
    /// the recording's id, holding −1, then the commit position.
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
        let leader_member_id = add(CLUSTER_LEADER_MEMBER_ID.0, "cluster.leader_member_id", -1);
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
            leader_member_id,
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

    /// The first entry of `term`, which `leader` leads, with the member's term stored
    /// and then its leader, in a consensus module's order.
    fn term(&mut self, term: i64, leader: i32) -> (i64, i64) {
        self.leadership_term_id.store_release(term).expect("stores");
        self.leads_as_or_follows(leader);
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

    /// Stores the member this one leads as or follows: −1 while it has none, which is
    /// what a member that holds an election stores.
    fn leads_as_or_follows(&self, leader: i32) {
        self.leader_member_id
            .store_release(i64::from(leader))
            .expect("stores");
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
    Timer(i64),
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
        Entry::Timer { correlation_id } => Kind::Timer(correlation_id),
        Entry::Snapshot | Entry::Other => Kind::Other,
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
///
/// A service that closes while this reads has the driver free the counter between the
/// read of its record and the read of its value (work item 421). The value of a freed
/// record reads as `Unallocated`, which is the counter gone, and so is a record that
/// another counter took meanwhile, which the second read of the record finds. Every
/// other error of the read is a fault and fails the test.
fn dropped(client: &Cavalry) -> Option<i64> {
    let counters = client.counters();
    let ours = |id: u32| {
        counters.record(id).ok().flatten().filter(|record| {
            record.type_id() == CLUSTER_REPLIES_DROPPED.0
                && record.key().starts_with(&MEMBER.to_le_bytes())
        })
    };
    (0..counters.capacity()).find_map(|id| {
        ours(id)?;
        let value = match counters.read(id) {
            Ok(value) => value,
            Err(CountersError::Unallocated { .. }) => return None,
            Err(error) => panic!("the counter reads: {error:?}"),
        };
        ours(id)?;
        Some(value)
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
/// longer than one frame carries. A session is open at its own open event and at its
/// own close event: a reply at each is sent. A payload of the longest length is sent. An entry
/// no leader of this version appends is handed over as such, a timer event whose
/// timer is not pending is handed to no handler, and an entry that is not
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
    // A cluster action that suspends: no leader of this version appends one. And a
    // timer event for a timer that nobody scheduled, which is handed to no handler.
    let other = encode(&Message::ClusterAction {
        leadership_term_id: 0,
        log_position: member.end,
        timestamp: member.time(),
        action: ClusterAction::Suspend,
        flags: 0,
    });
    let other = member.append_bytes(&other);
    let timer = encode(&Message::TimerEvent {
        leadership_term_id: 0,
        correlation_id: 9,
        timestamp: member.time(),
    });
    let timer = member.append_bytes(&timer);
    member.close_session(0, 0, CloseReason::ClientAction);
    member.message(0, 0, b"late");
    member.commit();

    // At the session's open event and at its close event: a reply to it. For "ask":
    // a reply to a session that never opened, one too long by a byte, and one of the
    // longest length. For "late": a reply to the session, now closed.
    let (full, long) = (vec![7u8; 1_344], vec![7u8; 1_345]);
    let mut outcomes = Vec::new();
    let mut kinds = Vec::new();
    wait("the service reads the log to its end", || {
        service
            .poll(
                &mut client,
                |committed, replies| {
                    match committed.entry {
                        Entry::SessionOpen { session_id } => {
                            let reply = replies.reply(session_id, b"welcome");
                            if reply == Reply::Retry {
                                return Action::Abort;
                            }
                            outcomes.push(reply);
                        }
                        Entry::SessionClose { session_id, .. } => {
                            let reply = replies.reply(session_id, b"goodbye");
                            if reply == Reply::Retry {
                                return Action::Abort;
                            }
                            outcomes.push(reply);
                        }
                        Entry::SessionMessage {
                            session_id,
                            payload,
                        } if payload == b"ask" => {
                            if replies.reply(session_id, &full) == Reply::Retry {
                                return Action::Abort;
                            }
                            outcomes.push(Reply::Sent);
                            outcomes.push(replies.reply(9, b"nobody"));
                            outcomes.push(replies.reply(session_id, &long));
                        }
                        Entry::SessionMessage {
                            session_id,
                            payload,
                        } => outcomes.push(replies.reply(session_id, payload)),
                        _ => {}
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
            Reply::Sent,
            Reply::NoSession,
            Reply::TooLong {
                found: 1_345,
                limit: 1_344
            },
            Reply::Sent,
            Reply::NoSession,
        ]
    );
    assert_eq!(kinds[3], (other.0, Kind::Other));
    assert!(
        kinds.iter().all(|(position, _)| *position != timer.0),
        "a timer event of no pending timer is passed over: {kinds:?}"
    );
    // A service of the C ABI below the version that gives a program timers hands
    // that event over as an entry of no kind, as its page says, and counts it as
    // passed over all the same.
    let mut c_client = Cavalry::connect_to(&member.dir).expect("a C program's client");
    let mut c_service = c_client.cluster_service();
    c_service.hand_passed_over_timer_events_as_other();
    let mut c_kinds = Vec::new();
    wait("the second service reads the log to its end", || {
        c_service
            .poll(
                &mut c_client,
                |committed, _| {
                    c_kinds.push((committed.position, kind(&committed.entry)));
                    Action::Continue
                },
                16,
            )
            .expect("the service polls");
        c_service.position() == member.end
    });
    assert!(
        c_kinds.contains(&(timer.0, Kind::Other)),
        "the timer event is handed over as an entry of no kind: {c_kinds:?}"
    );
    c_service.close(&mut c_client).expect("the service closes");
    c_client.close().expect("the client closes");
    assert_eq!(kinds[4].1, Kind::Close(0, false));
    assert_eq!(service.sessions(), 0);
    let mut replies = Read::new();
    wait("the client reads the three replies", || {
        read(&mut egress, &mut replies);
        replies.len() >= 3
    });
    assert_eq!(
        replies
            .iter()
            .map(|reply| reply.3.as_slice())
            .collect::<Vec<_>>(),
        [
            b"welcome".as_slice(),
            full.as_slice(),
            b"goodbye".as_slice()
        ]
    );
    assert_eq!(
        dropped(&client),
        Some(0),
        "a refusal that is the caller's is not a drop"
    );

    // An entry that is not of the page: the service stops there, and says where,
    // at every poll from the one that reads it. The polls before it, while the replay
    // has not brought the entry yet, read nothing.
    let (stray, _) = member.append_bytes(&[0xFF; 24]);
    member.commit();
    let mut stops = 0;
    wait(
        "the service stops at the entry that is not the page's",
        || {
            match service.poll(&mut client, |_, _| Action::Continue, 16) {
                Ok(taken) => {
                    assert_eq!((taken, stops), (0, 0), "an entry was taken past the stop");
                }
                Err(stopped) => {
                    assert!(
                        matches!(stopped, ClusterError::Entry { position } if position == stray),
                        "{stopped}"
                    );
                    stops += 1;
                }
            }
            assert_eq!(service.position(), stray, "the entry is not taken");
            stops == 3
        },
    );

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

/// An entry that is handed over again offers no session a reply it has taken. Two
/// sessions are open, the first with a client that reads and the second with none.
/// The handler answers one message with four replies, to the first session, twice
/// to the second, and to the first again, and has the entry handed over again while
/// any of them is to be tried again. The first session's client reads its two
/// replies once each and in their order, however many times the entry was handed
/// over; a reply behind one that was refused is not offered; and the second
/// session's two replies are dropped once the keepalive interval has passed.
#[test]
fn an_entry_handed_over_again_sends_no_session_a_reply_twice() {
    let mut member = Member::start("again");
    let mut client = Cavalry::connect_to(&member.dir).expect("the service's client");
    let mut service = client.cluster_service();
    member.name();
    open_log(&mut service, &mut client, &mut Seen::default());

    let (reading, absent) = (response_channel(), response_channel());
    let mut session = Cavalry::connect_to(&member.dir).expect("the session's client");
    let mut egress = session
        .add_subscription(&reading, EGRESS_STREAM_ID)
        .expect("the response channel");
    member.term(0, MEMBER);
    member.open(0, 0, &reading);
    member.open(0, 1, &absent);
    let asked = member.message(0, 0, b"ask");
    member.commit();

    // What each handling of the message was answered for its four replies.
    let mut handlings: Vec<[Reply; 4]> = Vec::new();
    let began = Instant::now();
    wait("the message is taken", || {
        service
            .poll(
                &mut client,
                |committed, replies| {
                    if let Entry::SessionMessage { .. } = committed.entry {
                        let answered = [
                            replies.reply(0, b"first"),
                            replies.reply(1, b"second"),
                            replies.reply(1, b"third"),
                            replies.reply(0, b"fourth"),
                        ];
                        handlings.push(answered);
                        if answered.contains(&Reply::Retry) {
                            return Action::Abort;
                        }
                    }
                    Action::Continue
                },
                16,
            )
            .expect("the service polls");
        service.position() == member.end
    });
    assert!(
        began.elapsed() >= Duration::from_millis(500),
        "the entry was held for the keepalive interval"
    );
    assert!(
        handlings.len() > 2,
        "the entry was handed over again: {} handlings",
        handlings.len()
    );
    let (last, earlier) = handlings.split_last().expect("a handling");
    assert_eq!(
        *last,
        [Reply::Sent, Reply::Dropped, Reply::Dropped, Reply::Sent]
    );
    for answered in earlier {
        // The second session takes nothing, and its second reply waits behind its
        // first; the first session's second reply waits behind its first too, while
        // its publication connects.
        assert_eq!(answered[1..3], [Reply::Retry, Reply::Retry], "{answered:?}");
        assert_eq!(answered[0], answered[3], "{answered:?}");
        assert!(
            matches!(answered[0], Reply::Sent | Reply::Retry),
            "{answered:?}"
        );
    }
    assert!(
        earlier
            .iter()
            .filter(|answered| answered[0] == Reply::Sent)
            .count()
            > 1,
        "the first session's replies were answered as sent by more than one handling"
    );
    assert_eq!(dropped(&client), Some(2));

    // The first session's client reads each of its replies once, in their order.
    let mut replies = Read::new();
    wait("the client reads its two replies", || {
        read(&mut egress, &mut replies);
        replies.len() >= 2
    });
    let quiet = Instant::now() + Duration::from_millis(200);
    while Instant::now() < quiet {
        read(&mut egress, &mut replies);
    }
    assert_eq!(
        replies,
        [
            (0, 0, asked.1, b"first".to_vec()),
            (0, 0, asked.1, b"fourth".to_vec())
        ],
        "each reply once, after {} handlings",
        handlings.len()
    );

    service.close(&mut client).expect("the service closes");
    egress.close().expect("the subscription closes");
    session.close().expect("the session's client closes");
    client.close().expect("the service's client closes");
    member.close();
}

/// A reply too long for one frame is refused before the service counts it, in every
/// handling of its entry alike, so it moves no other reply's place among those the
/// handlings of the entry settle. Each of two messages has a session that takes
/// nothing, so its entry is handed over again for the keepalive interval. For the
/// first, a too-long reply comes before a reply to a session whose client reads. For
/// the second, it comes behind a reply to a session whose client subscribes only
/// after the first handling, so that the reply before it is refused at first and it
/// waits behind that one. Every handling is told `TooLong`, and each client reads each
/// of its replies once and in their order.
#[test]
fn a_reply_too_long_for_a_frame_takes_no_place_among_its_sessions_replies() {
    let mut member = Member::start("too-long");
    let mut client = Cavalry::connect_to(&member.dir).expect("the service's client");
    let mut service = client.cluster_service();
    member.name();
    open_log(&mut service, &mut client, &mut Seen::default());

    let long = vec![7u8; 1_345];
    let too_long = Reply::TooLong {
        found: 1_345,
        limit: 1_344,
    };

    // A too-long reply before a reply the client takes.
    let (reading, absent) = (response_channel(), response_channel());
    let mut session = Cavalry::connect_to(&member.dir).expect("the sessions' client");
    let mut egress = session
        .add_subscription(&reading, EGRESS_STREAM_ID)
        .expect("the response channel");
    member.term(0, MEMBER);
    member.open(0, 0, &reading);
    member.open(0, 1, &absent);
    let asked = member.message(0, 0, b"ask");
    member.commit();
    let mut handlings: Vec<[Reply; 3]> = Vec::new();
    let began = Instant::now();
    wait("the first message is taken", || {
        service
            .poll(
                &mut client,
                |committed, replies| {
                    if let Entry::SessionMessage { .. } = committed.entry {
                        let answered = [
                            replies.reply(0, &long),
                            replies.reply(0, b"after"),
                            replies.reply(1, b"wait"),
                        ];
                        handlings.push(answered);
                        if answered.contains(&Reply::Retry) {
                            return Action::Abort;
                        }
                    }
                    Action::Continue
                },
                16,
            )
            .expect("the service polls");
        service.position() == member.end
    });
    assert!(
        began.elapsed() >= Duration::from_millis(500),
        "the entry was held for the keepalive interval"
    );
    assert!(
        handlings.len() > 2,
        "the entry was handed over again: {} handlings",
        handlings.len()
    );
    let (last, earlier) = handlings.split_last().expect("a handling");
    assert_eq!(*last, [too_long, Reply::Sent, Reply::Dropped]);
    for answered in earlier {
        assert_eq!(answered[0], too_long, "{answered:?}");
        assert!(
            matches!(answered[1], Reply::Sent | Reply::Retry),
            "{answered:?}"
        );
        assert_eq!(answered[2], Reply::Retry, "{answered:?}");
    }
    assert!(
        earlier
            .iter()
            .filter(|answered| answered[1] == Reply::Sent)
            .count()
            > 1,
        "the reply after the too-long one was answered as sent by more than one handling"
    );
    assert_eq!(dropped(&client), Some(1));
    let mut replies = Read::new();
    wait("the client reads its reply", || {
        read(&mut egress, &mut replies);
        !replies.is_empty()
    });
    let quiet = Instant::now() + Duration::from_millis(200);
    while Instant::now() < quiet {
        read(&mut egress, &mut replies);
    }
    assert_eq!(
        replies,
        [(0, 0, asked.1, b"after".to_vec())],
        "the reply once, after {} handlings",
        handlings.len()
    );

    // A too-long reply behind a refused reply to the same session: the session's
    // client subscribes once the first handling has been refused.
    let (late, absent) = (response_channel(), response_channel());
    member.open(0, 2, &late);
    member.open(0, 3, &absent);
    let again = member.message(0, 2, b"again");
    member.commit();
    let mut handlings: Vec<[Reply; 4]> = Vec::new();
    let mut subscribed: Option<Subscription> = None;
    let began = Instant::now();
    wait("the second message is taken", || {
        service
            .poll(
                &mut client,
                |committed, replies| {
                    if let Entry::SessionMessage { .. } = committed.entry {
                        let answered = [
                            replies.reply(2, b"one"),
                            replies.reply(2, &long),
                            replies.reply(2, b"two"),
                            replies.reply(3, b"wait"),
                        ];
                        handlings.push(answered);
                        if answered.contains(&Reply::Retry) {
                            return Action::Abort;
                        }
                    }
                    Action::Continue
                },
                16,
            )
            .expect("the service polls");
        if subscribed.is_none() && !handlings.is_empty() {
            subscribed = Some(
                session
                    .add_subscription(&late, EGRESS_STREAM_ID)
                    .expect("the late response channel"),
            );
        }
        service.position() == member.end
    });
    assert!(
        began.elapsed() >= Duration::from_millis(500),
        "the entry was held for the keepalive interval"
    );
    assert!(
        handlings.len() > 2,
        "the entry was handed over again: {} handlings",
        handlings.len()
    );
    assert_eq!(
        handlings[0],
        [Reply::Retry, too_long, Reply::Retry, Reply::Retry],
        "with no client yet, the first reply is refused and the third waits behind it"
    );
    let (last, earlier) = handlings.split_last().expect("a handling");
    assert_eq!(*last, [Reply::Sent, too_long, Reply::Sent, Reply::Dropped]);
    for answered in earlier {
        assert_eq!(answered[1], too_long, "{answered:?}");
        assert_eq!(answered[0], answered[2], "{answered:?}");
        assert!(
            matches!(answered[0], Reply::Sent | Reply::Retry),
            "{answered:?}"
        );
        assert_eq!(answered[3], Reply::Retry, "{answered:?}");
    }
    assert!(
        earlier
            .iter()
            .filter(|answered| answered[0] == Reply::Sent)
            .count()
            > 1,
        "the replies around the too-long one were answered as sent by more than one handling"
    );
    assert_eq!(dropped(&client), Some(2));
    let mut late_egress = subscribed.expect("the client subscribed");
    let mut replies = Read::new();
    wait("the client reads its two replies", || {
        read(&mut late_egress, &mut replies);
        replies.len() >= 2
    });
    let quiet = Instant::now() + Duration::from_millis(200);
    while Instant::now() < quiet {
        read(&mut late_egress, &mut replies);
    }
    assert_eq!(
        replies,
        [
            (0, 2, again.1, b"one".to_vec()),
            (0, 2, again.1, b"two".to_vec())
        ],
        "each reply once, after {} handlings",
        handlings.len()
    );

    service.close(&mut client).expect("the service closes");
    egress.close().expect("the subscription closes");
    late_egress.close().expect("the late subscription closes");
    session.close().expect("the sessions' client closes");
    client.close().expect("the service's client closes");
    member.close();
}

/// A service answers an entry of a term only while its member says it leads that term
/// still (`spec/cluster.md`, "Replies"). The member leads term 0, and its service
/// answers. The member then holds an election, its `cluster.leader_member_id` at −1:
/// an entry of term 0 that it commits is taken and not answered. The member then
/// stores term 1, which it leads too: an entry of term 0 is not answered either,
/// though its reply would go to the same client from the same service. Term 1's own
/// entries are answered. The session's client reads the first reply and the last,
/// and nothing between, and no reply is counted as dropped.
#[test]
fn a_service_answers_a_terms_entries_only_while_its_member_says_it_leads_the_term() {
    let mut member = Member::start("leads");
    let mut client = Cavalry::connect_to(&member.dir).expect("the service's client");
    let mut service = client.cluster_service();
    let mut seen = Seen::default();
    member.name();
    open_log(&mut service, &mut client, &mut seen);

    let channel = response_channel();
    let mut session = Cavalry::connect_to(&member.dir).expect("the session's client");
    let mut egress = session
        .add_subscription(&channel, EGRESS_STREAM_ID)
        .expect("the response channel");
    member.term(0, MEMBER);
    member.open(0, 0, &channel);
    let one = member.message(0, 0, b"one");
    member.commit();
    poll_to_end(&mut service, &mut client, &mut seen, &member);
    assert_eq!(seen.replies, [(one.0, Reply::Sent)]);
    assert!(service.answers());

    // The member holds an election: it leads no term, and term 0 is still its term.
    member.leads_as_or_follows(-1);
    let two = member.message(0, 0, b"two");
    member.commit();
    poll_to_end(&mut service, &mut client, &mut seen, &member);
    assert_eq!(seen.replies[1..], [(two.0, Reply::NotAnswering)]);
    assert!(!service.answers(), "it gave its publications back");

    // The member leads again, in term 1, whose first entry it has not appended yet.
    // The term is stored before the leader, as a consensus module stores them.
    member.leadership_term_id.store_release(1).expect("stores");
    member.leads_as_or_follows(MEMBER);
    let three = member.message(0, 0, b"three");
    member.commit();
    poll_to_end(&mut service, &mut client, &mut seen, &member);
    assert_eq!(seen.replies[2..], [(three.0, Reply::NotAnswering)]);
    assert!(!service.answers());

    // Term 1's own entries it answers.
    member.term(1, MEMBER);
    let four = member.message(1, 0, b"four");
    member.commit();
    poll_to_end(&mut service, &mut client, &mut seen, &member);
    assert_eq!(seen.replies[3..], [(four.0, Reply::Sent)]);
    assert!(service.answers());

    // The client reads the two replies that were sent, and in 300 ms more no other.
    let mut replies = Read::new();
    wait("the client reads the two replies", || {
        read(&mut egress, &mut replies);
        replies.len() >= 2
    });
    let quiet = Instant::now();
    while quiet.elapsed() < Duration::from_millis(300) {
        read(&mut egress, &mut replies);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        replies,
        [
            (0, 0, one.1, b"one".to_vec()),
            (1, 0, four.1, b"four".to_vec())
        ]
    );
    assert_eq!(
        dropped(&client),
        Some(0),
        "an entry that is not this service's to answer is not a drop"
    );

    service.close(&mut client).expect("the service closes");
    egress.close().expect("the subscription closes");
    session.close().expect("the session's client closes");
    client.close().expect("the service's client closes");
    member.close();
}

/// Polls a service whose member's driver has closed until a poll is an error, and
/// holds that error and the three polls after it to the one error a poll of a closed
/// client is (ADR 0155). A poll before the client's conductor has seen the close
/// may take nothing and be no error: the client is open still.
fn ends_on_the_clients_close(service: &mut ClusterService, client: &mut Cavalry) {
    let mut poll = |client: &mut Cavalry| service.poll(client, |_, _| Action::Continue, 16);
    let deadline = Instant::now() + Duration::from_secs(10);
    let first = loop {
        match poll(client) {
            Ok(_) => {
                assert!(
                    Instant::now() < deadline,
                    "no poll was an error within 10 s of the driver's close; the client says {:?}",
                    client.closed()
                );
                std::thread::sleep(Duration::from_micros(100));
            }
            Err(error) => break error,
        }
    };
    assert!(
        matches!(
            first,
            ClusterError::Client(ClientError::Closed(Closed::DriverClosed))
        ),
        "the first error of a poll after the driver's close: {first:?}"
    );
    assert_eq!(client.closed(), Some(Closed::DriverClosed));
    for after in 1..=3 {
        let again = poll(client);
        assert!(
            matches!(
                again,
                Err(ClusterError::Client(ClientError::Closed(
                    Closed::DriverClosed
                )))
            ),
            "poll {after} after the first error: {again:?}"
        );
    }
}

/// A service that had no log open, beside a member that names no recording, has no
/// command for its driver and reads nothing: its poll is the client's close all the
/// same, so a program's loop ends on it.
#[test]
fn a_poll_of_a_closed_client_is_the_clients_close_for_a_service_with_no_log() {
    let member = Member::start("closed-with-no-log");
    let mut client = Cavalry::connect_to(&member.dir).expect("the service's client connects");
    let mut service = client.cluster_service();
    let mut seen = Seen::default();
    assert_eq!(poll(&mut service, &mut client, &mut seen), 0);
    member.close();
    ends_on_the_clients_close(&mut service, &mut client);
}

/// A service that reads its member's log when the member and its driver stop meets
/// the close in the read of the log, or in the command that closes its handle, by
/// when it polls: one error either way.
#[test]
fn a_poll_of_a_closed_client_is_the_clients_close_for_a_service_that_reads_its_log() {
    for round in 0..10 {
        let mut member = Member::start(&format!("closed-with-a-log-{round}"));
        let mut client = Cavalry::connect_to(&member.dir).expect("the service's client connects");
        let mut service = client.cluster_service();
        let mut seen = Seen::default();
        member.term(0, MEMBER);
        member.leads_as_or_follows(MEMBER);
        member.name();
        member.commit();
        open_log(&mut service, &mut client, &mut seen);
        wait("the service reads the term's first entry", || {
            poll(&mut service, &mut client, &mut seen);
            !seen.entries.is_empty()
        });
        let end = member.end;
        member.close();
        ends_on_the_clients_close(&mut service, &mut client);
        assert_eq!(
            service.position(),
            end,
            "round {round}: the entry taken before the close is taken"
        );
    }
}

/// A program that ends on the poll's one error closes its service as it always
/// does: a driver that is gone cannot be told, and that is no error of the close.
/// The service holds its log's handle here, since the poll met the close before it
/// read, so the close has a replay it cannot stop.
#[test]
fn a_service_closes_with_no_error_at_a_client_that_is_closed() {
    let mut member = Member::start("close-at-a-closed-client");
    let mut client = Cavalry::connect_to(&member.dir).expect("the service's client connects");
    let mut service = client.cluster_service();
    let mut seen = Seen::default();
    member.term(0, MEMBER);
    member.leads_as_or_follows(MEMBER);
    member.name();
    member.commit();
    open_log(&mut service, &mut client, &mut seen);
    member.close();
    wait("the client sees its driver's close", || {
        client.closed().is_some()
    });
    let polled = service.poll(&mut client, |_, _| Action::Continue, 16);
    assert!(
        matches!(
            polled,
            Err(ClusterError::Client(ClientError::Closed(
                Closed::DriverClosed
            )))
        ),
        "{polled:?}"
    );
    let closed = service.close(&mut client);
    assert!(closed.is_ok(), "{closed:?}");
}

/// A session message that schedules a timer: `s`, the id and the deadline.
fn schedule(id: i64, deadline: i64) -> Vec<u8> {
    let mut payload = vec![b's'];
    payload.extend_from_slice(&id.to_le_bytes());
    payload.extend_from_slice(&deadline.to_le_bytes());
    payload
}

/// A session message that cancels a timer: `c` and the id.
fn cancel(id: i64) -> Vec<u8> {
    let mut payload = vec![b'c'];
    payload.extend_from_slice(&id.to_le_bytes());
    payload
}

/// Polls with a program whose session messages schedule and cancel timers, and that
/// does not take the first timer it is handed while `refuse_once` is set.
fn poll_timers(
    service: &mut ClusterService,
    client: &mut Cavalry,
    seen: &mut Seen,
    refuse_once: &mut bool,
) -> usize {
    service
        .poll(
            client,
            |committed: &Committed<'_>, replies| {
                let number = |bytes: &[u8]| {
                    i64::from_le_bytes(bytes.try_into().expect("eight bytes of a number"))
                };
                match committed.entry {
                    Entry::SessionMessage { payload, .. } if payload.first() == Some(&b's') => {
                        replies
                            .schedule_timer(number(&payload[1..9]), number(&payload[9..17]))
                            .expect("room for a timer");
                    }
                    Entry::SessionMessage { payload, .. } if payload.first() == Some(&b'c') => {
                        replies.cancel_timer(number(&payload[1..9]));
                    }
                    Entry::Timer { correlation_id } if *refuse_once => {
                        // What a handler does before it gives an entry back is undone.
                        replies.cancel_timer(correlation_id);
                        replies
                            .schedule_timer(correlation_id + 1_000, 0)
                            .expect("room for a timer");
                        *refuse_once = false;
                        seen.retries += 1;
                        return Action::Abort;
                    }
                    _ => {}
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

/// The value of one of the service's counters of the member, or `None` while no
/// service has added it.
fn service_counter(client: &Cavalry, counter: i32) -> Option<i64> {
    let counters = client.counters();
    let ours = |id: u32| {
        counters.record(id).ok().flatten().filter(|record| {
            record.type_id() == counter && record.key().starts_with(&MEMBER.to_le_bytes())
        })
    };
    (0..counters.capacity()).find_map(|id| {
        ours(id)?;
        let value = match counters.read(id) {
            Ok(value) => value,
            Err(CountersError::Unallocated { .. }) => return None,
            Err(error) => panic!("the counter reads: {error:?}"),
        };
        ours(id)?;
        Some(value)
    })
}

/// A program schedules a timer while it takes an entry and is handed the timer's
/// event once: not by an event before its deadline, not a second time, not after
/// its cancel, and once still when its handler gave the event back first
/// (`spec/cluster.md`, "The service", "Timers"; ADR 0151). The service tells the
/// member on its driver what each taken entry changed, on the consensus module
/// stream, and every pending timer again each second. A service that starts later
/// reads the same log, is handed the same timer at the same position, tells the
/// member nothing of that history, and at the end of it what is pending.
#[test]
fn a_program_is_handed_each_timer_once_and_its_member_is_told() {
    let mut member = Member::start("timers");
    let mut client = Cavalry::connect_to(&member.dir).expect("the service's client");
    let mut service = client.cluster_service();
    let mut seen = Seen::default();
    let mut refuse_once = false;
    // The member's side of the consensus module stream, read by hand.
    let mut stream = member
        .client
        .add_subscription("cavalry:ipc", CONSENSUS_MODULE_STREAM_ID)
        .expect("the member's subscription");
    let mut told: Vec<(char, i64, i64)> = Vec::new();
    let mut read_told = |told: &mut Vec<(char, i64, i64)>| {
        stream
            .poll(
                |bytes, _| match Message::parse(Stream::ConsensusModule, bytes) {
                    Ok(Message::ScheduleTimer {
                        correlation_id,
                        deadline,
                    }) => told.push(('s', correlation_id, deadline)),
                    Ok(Message::CancelTimer { correlation_id }) => {
                        told.push(('c', correlation_id, 0));
                    }
                    other => panic!("no timer message on the stream: {other:?}"),
                },
                16,
            )
            .expect("the stream is read");
    };
    member.name();
    // The service opens an empty log: it has no history to read.
    wait("the service opens the member's log", || {
        poll_timers(&mut service, &mut client, &mut seen, &mut refuse_once);
        dropped(&client).is_some()
    });
    let to_end = |service: &mut ClusterService,
                  client: &mut Cavalry,
                  seen: &mut Seen,
                  refuse_once: &mut bool,
                  member: &Member| {
        member.commit();
        wait("the service reads the log to its end", || {
            poll_timers(service, client, seen, refuse_once);
            service.position() == member.end
        });
    };
    let timer_event = |member: &mut Member, id: i64, timestamp: i64| {
        let entry = encode(&Message::TimerEvent {
            leadership_term_id: 0,
            correlation_id: id,
            timestamp,
        });
        member.append_bytes(&entry).0
    };

    // Two timers, scheduled by two entries. Nothing is told before the entries are
    // taken, and then both are, in the order of their ids.
    member.term(0, MEMBER);
    member.message(0, -1, &schedule(9, TIME + 10_000));
    member.message(0, -1, &schedule(7, TIME + 100));
    read_told(&mut told);
    assert_eq!(told, []);
    to_end(
        &mut service,
        &mut client,
        &mut seen,
        &mut refuse_once,
        &member,
    );
    // A word may come again before the next is looked for: a refresh's, each second.
    let seven = ('s', 7, TIME + 100);
    let nine = ('s', 9, TIME + 10_000);
    wait("the member is told of two timers", || {
        // The service's poll offers a word again that the stream did not take.
        poll_timers(&mut service, &mut client, &mut seen, &mut refuse_once);
        read_told(&mut told);
        told.contains(&seven) && told.contains(&nine)
    });
    assert!(
        told.iter().all(|word| *word == seven || *word == nine),
        "{told:?}"
    );
    told.clear();
    assert_eq!(service_counter(&client, CLUSTER_SERVICE_TIMERS.0), Some(2));

    // An event before the deadline is passed over: no handler sees it.
    timer_event(&mut member, 7, TIME + 99);
    to_end(
        &mut service,
        &mut client,
        &mut seen,
        &mut refuse_once,
        &member,
    );
    assert!(!seen.entries.iter().any(|entry| entry.3 == Kind::Timer(7)));
    // An event at the deadline is handed over. The handler gives it back once,
    // after cancelling the timer and scheduling another: both are undone, and it is
    // handed the same event again.
    refuse_once = true;
    let due = timer_event(&mut member, 7, TIME + 100);
    to_end(
        &mut service,
        &mut client,
        &mut seen,
        &mut refuse_once,
        &member,
    );
    assert_eq!(seen.retries, 1);
    let handed: Vec<_> = seen
        .entries
        .iter()
        .filter(|entry| matches!(entry.3, Kind::Timer(_)))
        .cloned()
        .collect();
    assert_eq!(handed, [(due, 0, TIME + 100, Kind::Timer(7))]);
    wait(
        "the member is told that the timer is pending no more",
        || {
            poll_timers(&mut service, &mut client, &mut seen, &mut refuse_once);
            read_told(&mut told);
            told.contains(&('c', 7, 0))
        },
    );
    assert!(
        told.iter()
            .all(|word| *word == ('c', 7, 0) || *word == seven || *word == nine),
        "nothing of timer 1007, which the handling that was given back scheduled: {told:?}"
    );
    assert_eq!(told.iter().filter(|word| word.0 == 'c').count(), 1);
    told.clear();
    // A second event for it, and one for a timer nobody scheduled: passed over.
    timer_event(&mut member, 7, TIME + 200);
    timer_event(&mut member, 8, TIME + 200);
    // A cancel, and an event for the cancelled timer after its deadline.
    member.message(0, -1, &cancel(9));
    timer_event(&mut member, 9, TIME + 20_000);
    to_end(
        &mut service,
        &mut client,
        &mut seen,
        &mut refuse_once,
        &member,
    );
    assert_eq!(
        seen.entries
            .iter()
            .filter(|entry| matches!(entry.3, Kind::Timer(_)))
            .count(),
        1
    );
    wait("the member is told of the cancel", || {
        poll_timers(&mut service, &mut client, &mut seen, &mut refuse_once);
        read_told(&mut told);
        told.contains(&('c', 9, 0))
    });
    assert!(
        told.iter()
            .all(|word| *word == ('c', 9, 0) || *word == nine),
        "{told:?}"
    );
    told.clear();
    wait("the service stores what it counted", || {
        poll_timers(&mut service, &mut client, &mut seen, &mut refuse_once);
        service_counter(&client, CLUSTER_SERVICE_TIMER_EVENTS_PASSED_OVER.0) == Some(4)
    });
    assert_eq!(service_counter(&client, CLUSTER_SERVICE_TIMERS.0), Some(0));
    assert_eq!(
        service_counter(&client, CLUSTER_SERVICE_TIMER_CANCELS_DROPPED.0),
        Some(0)
    );

    // A timer that stays pending is told again each timer refresh interval, 1 s.
    member.message(0, -1, &schedule(11, TIME + 1_000_000));
    to_end(
        &mut service,
        &mut client,
        &mut seen,
        &mut refuse_once,
        &member,
    );
    wait("the member is told of the timer, and again", || {
        poll_timers(&mut service, &mut client, &mut seen, &mut refuse_once);
        read_told(&mut told);
        told.len() >= 2
    });
    // The words after the first are refreshes, a second apart: in any stretch of
    // time the member is told once for each whole second of it, and once more at the
    // most. The stretch is measured from before the first of them to after the last,
    // so a thread that is held makes the bound looser and never tighter.
    let from = Instant::now();
    let had = told.len();
    wait("the member is told of the timer twice more", || {
        poll_timers(&mut service, &mut client, &mut seen, &mut refuse_once);
        read_told(&mut told);
        told.len() >= had + 2
    });
    let more = (told.len() - had) as u64;
    let seconds = from.elapsed().as_secs();
    assert!(told.iter().all(|word| *word == ('s', 11, TIME + 1_000_000)));
    assert!(
        more <= seconds + 1,
        "{more} refreshes in {seconds} whole seconds: a refresh comes a second after the one before it"
    );
    told.clear();

    // A service that starts now reads all of that as history. It is handed the one
    // timer at the same position, tells the member nothing on the way, and at the
    // end of its history tells it the one timer that is pending.
    let mut second_client = Cavalry::connect_to(&member.dir).expect("a second service's client");
    let mut second = second_client.cluster_service();
    let mut second_seen = Seen::default();
    let mut never = false;
    wait("the second service reads the log to its end", || {
        poll_timers(
            &mut second,
            &mut second_client,
            &mut second_seen,
            &mut never,
        );
        second.position() == member.end
    });
    let handed: Vec<_> = second_seen
        .entries
        .iter()
        .filter(|entry| matches!(entry.3, Kind::Timer(_)))
        .cloned()
        .collect();
    assert_eq!(handed, [(due, 0, TIME + 100, Kind::Timer(7))]);
    wait("the member is told by the second service", || {
        poll_timers(
            &mut second,
            &mut second_client,
            &mut second_seen,
            &mut never,
        );
        read_told(&mut told);
        !told.is_empty()
    });
    assert!(
        told.iter().all(|word| *word == ('s', 11, TIME + 1_000_000)),
        "nothing of the history: {told:?}"
    );

    second
        .close(&mut second_client)
        .expect("the service closes");
    second_client.close().expect("the client closes");
    service.close(&mut client).expect("the service closes");
    client.close().expect("the client closes");
    drop(stream);
    member.close();
}

/// A service holds 1,024 pending timers: a schedule of another id beyond them is
/// refused, and changes nothing. And a program may schedule a timer again inside
/// that timer's own event: it is pending again, and handed over again at its next
/// event (`spec/cluster.md`, "The service", "Timers"; ADR 0151).
#[test]
fn a_program_is_refused_a_timer_beyond_the_bound_and_schedules_one_again_in_its_event() {
    let mut member = Member::start("timers-bound");
    let mut client = Cavalry::connect_to(&member.dir).expect("the service's client connects");
    let mut service = client.cluster_service();
    let bound = cavalry_proto::cluster::MAX_SERVICE_TIMERS as i64;
    // What the program was refused, and each timer it was handed with its event's time.
    let mut refused: Vec<i64> = Vec::new();
    let mut handed: Vec<(i64, i64)> = Vec::new();
    let mut poll = |service: &mut ClusterService, client: &mut Cavalry| {
        service
            .poll(
                client,
                |committed: &Committed<'_>, replies| {
                    match committed.entry {
                        // One message fills the table and asks for one more.
                        Entry::SessionMessage { .. } => {
                            for id in 0..=bound {
                                if replies.schedule_timer(id, TIME + 1_000).is_err() {
                                    refused.push(id);
                                }
                            }
                        }
                        // The timer's own event: pending no more, and scheduled again.
                        Entry::Timer { correlation_id } => {
                            handed.push((correlation_id, committed.timestamp));
                            replies
                                .schedule_timer(correlation_id, committed.timestamp + 1_000)
                                .expect("the place the timer left");
                        }
                        _ => {}
                    }
                    Action::Continue
                },
                16,
            )
            .expect("the service polls")
    };
    member.name();
    wait("the service opens the member's log", || {
        poll(&mut service, &mut client);
        dropped(&client).is_some()
    });
    member.term(0, MEMBER);
    member.message(0, -1, b"fill");
    for timestamp in [TIME + 1_000, TIME + 1_500, TIME + 2_000] {
        let event = encode(&Message::TimerEvent {
            leadership_term_id: 0,
            correlation_id: 3,
            timestamp,
        });
        member.append_bytes(&event);
    }
    member.commit();
    wait("the service reads the log to its end", || {
        poll(&mut service, &mut client);
        service.position() == member.end
    });
    wait("the service stores what it counted", || {
        poll(&mut service, &mut client);
        service_counter(&client, CLUSTER_SERVICE_TIMER_EVENTS_PASSED_OVER.0) == Some(1)
    });
    assert_eq!(refused, [bound], "the one beyond the bound, and no other");
    // Handed at its deadline, scheduled again for a second later, passed over half
    // a second later, and handed again when that second has passed.
    assert_eq!(handed, [(3, TIME + 1_000), (3, TIME + 2_000)]);
    assert_eq!(
        service_counter(&client, CLUSTER_SERVICE_TIMERS.0),
        Some(bound),
        "the table is full still: the timer handed over is pending again"
    );

    service.close(&mut client).expect("the service closes");
    client.close().expect("the client closes");
    member.close();
}
