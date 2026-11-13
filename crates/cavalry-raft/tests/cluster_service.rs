//! A client's message answered by a cluster's service, end to end: three members over
//! three drivers on the loopback, a service beside each, and a client with a session
//! on a fourth driver (`spec/cluster.md`, "The service", "Replies"; ADR 0117, ADR
//! 0118). The client is `cavalry-client`'s own session and each service its own
//! `ClusterService`, so nothing of the protocol is written by hand here. The members
//! and their services run in one thread, as `cavalry run` and a program beside it
//! would on three hosts; the client runs in the test's thread, since its connect
//! blocks while the members answer it.
//!
//! Every service runs one handler, which answers a session message with the id of
//! its own member and the message. So the reply a client reads says which member's
//! service sent it, which a real service has no reason to say.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cavalry_client::{Action, Cavalry, ClusterService, ClusterSession, Entry, Reply};
use cavalry_driver::Driver;
use cavalry_raft::{ConsensusModule, ElectionState, Role};
use cavalry_shm::counters::CountersReader;
use cavalry_shm::counters::generated::{
    CLUSTER_APPEND_POSITION, CLUSTER_LEADERSHIP_TERM_ID, CLUSTER_LOG_RECORDING_ID,
    CLUSTER_REPLIES_DROPPED,
};

struct TestDir(PathBuf);

impl TestDir {
    fn new(test: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "cavalry-raft-service-{}-{test}",
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

// A cluster's ports, taken as `ports/mod.rs` says and for its reason.
mod ports;
use ports::free_ports;

/// How long any wait of this test may last.
const DEADLINE: Duration = Duration::from_secs(60);

/// The members string of three members on the loopback, from fifteen ports.
fn members_on(ports: &[u16]) -> String {
    (0..3)
        .map(|member| {
            let endpoints: Vec<String> = ports[member * 5..member * 5 + 5]
                .iter()
                .map(|port| format!("127.0.0.1:{port}"))
                .collect();
            format!("{member},{}", endpoints.join(","))
        })
        .collect::<Vec<_>>()
        .join("|")
}

fn member_dir(dir: &Path, id: i32) -> PathBuf {
    dir.join(format!("member-{id}"))
}

/// What the test asks of the cluster's thread.
enum Ask {
    /// The member with this id is lost: its service, its consensus module and its
    /// driver.
    Lose(i32),
    /// The service beside the member with this id stops, and another starts.
    Restart(i32),
    Stop,
}

/// What one service did with one session message.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Took {
    /// The service's member, and how many services ran beside that member before it.
    member: i32,
    generation: u32,
    position: i64,
    term: i64,
    timestamp: i64,
    session: i64,
    payload: Vec<u8>,
    reply: Reply,
}

/// What the cluster's thread tells the test.
enum Told {
    /// Every election has closed behind this leader, in this term.
    Led(i32, i64),
    Took(Took),
}

/// One member, its driver and the service beside it.
struct Member {
    id: i32,
    driver: Driver,
    module: ConsensusModule,
    client: Cavalry,
    service: ClusterService,
    generation: u32,
}

/// The cluster's thread: every member's duty cycle and every service's poll, until it
/// is asked to stop.
fn run(dir: PathBuf, members_string: String, asks: Receiver<Ask>, told: Sender<Told>) {
    let mut members: Vec<Member> = [1, 2, 0]
        .into_iter()
        .map(|id| {
            let driver_dir = member_dir(&dir, id);
            let log_dir = dir.join(format!("member-{id}-log"));
            let driver = Driver::start(&driver_dir, &log_dir).expect("the driver starts");
            let module = ConsensusModule::start(&driver_dir, &log_dir, &members_string, id)
                .expect("the member starts");
            let client = Cavalry::connect_to(&driver_dir).expect("the service's client");
            let service = client.cluster_service();
            Member {
                id,
                driver,
                module,
                client,
                service,
                generation: 0,
            }
        })
        .collect();
    let mut led = None;
    loop {
        match asks.try_recv() {
            Ok(Ask::Stop) | Err(TryRecvError::Disconnected) => break,
            Ok(Ask::Lose(id)) => {
                let index = members
                    .iter()
                    .position(|member| member.id == id)
                    .expect("the member to lose");
                let mut lost = members.remove(index);
                lost.service
                    .close(&mut lost.client)
                    .expect("the service closes");
                lost.client.close().expect("the service's client closes");
                drop(lost.module);
                lost.driver.close().expect("the driver closes");
            }
            Ok(Ask::Restart(id)) => {
                let member = members
                    .iter_mut()
                    .find(|member| member.id == id)
                    .expect("the member whose service starts again");
                let next = member.client.cluster_service();
                std::mem::replace(&mut member.service, next)
                    .close(&mut member.client)
                    .expect("the service closes");
                member.generation += 1;
            }
            Err(TryRecvError::Empty) => {}
        }
        let mut work = 0;
        for member in members.iter_mut() {
            work += member.module.do_work().expect("the member's duty cycle");
        }
        for member in members.iter_mut() {
            let (id, generation) = (member.id, member.generation);
            work += member
                .service
                .poll(
                    &mut member.client,
                    |committed, replies| {
                        if let Entry::SessionMessage {
                            session_id,
                            payload,
                        } = committed.entry
                        {
                            // The member's id, a colon and the message.
                            let mut answer = [0u8; 64];
                            answer[0] = b'0' + id as u8;
                            answer[1] = b':';
                            answer[2..2 + payload.len()].copy_from_slice(payload);
                            let reply = replies.reply(session_id, &answer[..2 + payload.len()]);
                            if reply == Reply::Retry {
                                return Action::Abort;
                            }
                            let _ = told.send(Told::Took(Took {
                                member: id,
                                generation,
                                position: committed.position,
                                term: committed.leadership_term_id,
                                timestamp: committed.timestamp,
                                session: session_id,
                                payload: payload.to_vec(),
                                reply,
                            }));
                        }
                        Action::Continue
                    },
                    16,
                )
                .expect("the service polls");
        }
        let closed = members
            .iter()
            .all(|member| member.module.election_state() == ElectionState::Closed);
        let mut leaders = members
            .iter()
            .filter(|member| member.module.role() == Role::Leader);
        if let (true, Some(leader), None) = (closed, leaders.next(), leaders.next()) {
            let now = Some((leader.id, leader.module.leadership_term_id()));
            if now != led {
                led = now;
                let _ = told.send(Told::Led(leader.id, leader.module.leadership_term_id()));
            }
        }
        if work == 0 {
            std::thread::sleep(Duration::from_micros(200));
        }
    }
    for mut member in members {
        member
            .service
            .close(&mut member.client)
            .expect("the service closes");
        member.client.close().expect("the service's client closes");
        drop(member.module);
        member.driver.close().expect("the driver closes");
    }
}

/// The cluster as the test holds it: the thread, what it was told, and what it said.
struct Cluster {
    asks: Sender<Ask>,
    told: Receiver<Told>,
    thread: Option<JoinHandle<()>>,
    /// The last leader the thread named, with its term.
    led: Option<(i32, i64)>,
    took: Vec<Took>,
}

impl Cluster {
    fn start(dir: &Path, members: &str) -> Self {
        let (asks, asked) = channel();
        let (tell, told) = channel();
        let (dir, members) = (dir.to_path_buf(), members.to_string());
        let thread = std::thread::Builder::new()
            .name("cluster".into())
            .spawn(move || run(dir, members, asked, tell))
            .expect("the cluster's thread starts");
        Self {
            asks,
            told,
            thread: Some(thread),
            led: None,
            took: Vec::new(),
        }
    }

    fn ask(&self, ask: Ask) {
        self.asks.send(ask).expect("the cluster's thread runs");
    }

    /// Takes what the thread has said so far. A thread that has stopped by a panic
    /// fails the test with that panic.
    fn hear(&mut self) {
        loop {
            match self.told.try_recv() {
                Ok(Told::Led(leader, term)) => self.led = Some((leader, term)),
                Ok(Told::Took(took)) => self.took.push(took),
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    if let Some(Err(panic)) = self.thread.take().map(JoinHandle::join) {
                        std::panic::resume_unwind(panic);
                    }
                    return;
                }
            }
        }
    }

    /// What the services did with the session message whose payload is `payload`, in
    /// the order of member and generation.
    fn took(&self, payload: &[u8]) -> Vec<Took> {
        let mut took: Vec<Took> = self
            .took
            .iter()
            .filter(|took| took.payload == payload)
            .cloned()
            .collect();
        took.sort_by_key(|took| (took.member, took.generation));
        took
    }

    fn stop(mut self) {
        self.ask(Ask::Stop);
        if let Some(Err(panic)) = self.thread.take().map(JoinHandle::join) {
            std::panic::resume_unwind(panic);
        }
    }
}

/// A reply as the client read it: its payload and the cluster's time on it.
type Replies = Vec<(Vec<u8>, i64)>;

/// One duty cycle of the test's thread: the session's poll into `replies`, and what
/// the cluster's thread said.
fn cycle(session: &mut ClusterSession, cluster: &mut Cluster, replies: &mut Replies) {
    session
        .poll(
            |payload, timestamp| replies.push((payload.to_vec(), timestamp)),
            16,
        )
        .expect("the session polls");
    cluster.hear();
    std::thread::sleep(Duration::from_millis(1));
}

/// Runs the test's thread until `done`, or fails with `what`.
fn until(
    session: &mut ClusterSession,
    cluster: &mut Cluster,
    replies: &mut Replies,
    what: &str,
    mut done: impl FnMut(&ClusterSession, &Cluster, &Replies) -> bool,
) {
    let deadline = Instant::now() + DEADLINE;
    while !done(session, cluster, replies) {
        assert!(
            Instant::now() < deadline,
            "{what}: session closed {:?}, led {:?}, replies {replies:?}, took {:?}",
            session.closed(),
            cluster.led,
            cluster.took
        );
        cycle(session, cluster, replies);
    }
}

/// Sends `message` on the session and runs until the client has read a reply and
/// `services` services have taken the message, then for 300 ms more, in which a
/// second reply would come. Returns the replies read and what each service did.
fn converse(
    session: &mut ClusterSession,
    cluster: &mut Cluster,
    message: &[u8],
    services: usize,
) -> (Replies, Vec<Took>) {
    let mut replies = Replies::new();
    let mut sent = false;
    let deadline = Instant::now() + DEADLINE;
    while !sent {
        assert!(Instant::now() < deadline, "the session takes the message");
        sent = session.offer(message).is_ok();
        cycle(session, cluster, &mut replies);
    }
    until(
        session,
        cluster,
        &mut replies,
        "the message is answered and every service took it",
        |_, cluster, replies| !replies.is_empty() && cluster.took(message).len() >= services,
    );
    let quiet = Instant::now() + Duration::from_millis(300);
    while Instant::now() < quiet {
        cycle(session, cluster, &mut replies);
    }
    let took = cluster.took(message);
    (replies, took)
}

/// The record and the value of the counter of `type_id` that is keyed by the member
/// with `id`, in that member's counters file.
fn counter(dir: &Path, id: i32, type_id: i32) -> (u32, i64) {
    let reader = CountersReader::open(&member_dir(dir, id)).expect("the counters file opens");
    for record_id in 0..reader.capacity() {
        if let Some(record) = reader.record(record_id).expect("a record")
            && record.type_id() == type_id
            && record.key().starts_with(&id.to_le_bytes())
        {
            return (
                record_id,
                reader.read_acquire(record_id).expect("the counter is read"),
            );
        }
    }
    panic!("member {id} has no counter of type {type_id}");
}

/// The value of `cluster.replies_dropped` beside the member with `id`, which the
/// member's service added.
fn dropped(dir: &Path, id: i32) -> i64 {
    counter(dir, id, CLUSTER_REPLIES_DROPPED.0).1
}

/// What each of `took` was answered, by its member.
fn answers(took: &[Took]) -> Vec<(i32, u32, Reply)> {
    took.iter()
        .map(|took| (took.member, took.generation, took.reply))
        .collect()
}

/// A client's message is answered once, by the service beside the leader, with the
/// cluster's time of its entry, while the service beside each follower takes the same
/// entry and sends nothing. When the leader is lost, the service beside the member
/// that leads next answers the session's next message, and no other. A service that
/// starts again beside that member reads the log from its start, takes both messages
/// again and answers neither, and answers the next.
#[test]
fn a_message_is_answered_by_the_service_beside_its_terms_leader_alone() {
    let dir = TestDir::new("replies");
    let ports = free_ports(16);
    let members = members_on(&ports[..15]);
    let mut cluster = Cluster::start(dir.path(), &members);
    let deadline = Instant::now() + DEADLINE;
    while cluster.led.is_none() {
        assert!(Instant::now() < deadline, "the cluster has a leader");
        cluster.hear();
        std::thread::sleep(Duration::from_millis(1));
    }
    let (first_leader, first_term) = cluster.led.expect("a leader");

    // The fourth rule of order a member keeps for its service (`spec/cluster.md`,
    // "The service"): its term and its append position are added before its
    // recording's id, which a service finds the member by. A member adds its
    // counters one after another to a driver that has freed none, so a counter added
    // earlier has the lower record.
    for id in 0..3 {
        let recording = counter(dir.path(), id, CLUSTER_LOG_RECORDING_ID.0).0;
        let term = counter(dir.path(), id, CLUSTER_LEADERSHIP_TERM_ID.0).0;
        let append = counter(dir.path(), id, CLUSTER_APPEND_POSITION.0).0;
        assert!(
            term < recording && append < recording,
            "member {id}: term {term}, append position {append}, recording id {recording}"
        );
    }

    // The client, on a driver of its own.
    let client_dir = dir.path().join("client");
    let driver =
        Driver::start(&client_dir, dir.path().join("client-log")).expect("the client's driver");
    let mut cavalry = Cavalry::connect_to(&client_dir).expect("the client connects");
    let channel = format!("cavalry:udp?endpoint=127.0.0.1:{}", ports[15]);
    let mut session = cavalry
        .connect_cluster(&members, &channel)
        .expect("the session opens");
    assert_eq!(
        (session.leader_member_id(), session.leadership_term_id()),
        (first_leader, first_term)
    );

    // One message: one reply, from the leader's service.
    let (replies, took) = converse(&mut session, &mut cluster, b"one", 3);
    assert_eq!(
        took.len(),
        3,
        "each member's service took it once: {took:?}"
    );
    let entry = &took[0];
    assert!(
        took.iter().all(
            |other| (other.position, other.term, other.timestamp, other.session)
                == (
                    entry.position,
                    first_term,
                    entry.timestamp,
                    session.session_id()
                )
        ),
        "every service read the same entry: {took:?}"
    );
    assert!(entry.timestamp > 1_600_000_000_000, "the cluster's time");
    for took in &took {
        let expected = if took.member == first_leader {
            Reply::Sent
        } else {
            Reply::NotAnswering
        };
        assert_eq!(took.reply, expected, "member {}", took.member);
    }
    assert_eq!(
        replies,
        [(format!("{first_leader}:one").into_bytes(), entry.timestamp)]
    );

    // The leader is lost, its service and its driver with it. The session follows
    // the cluster to the member that leads next.
    cluster.ask(Ask::Lose(first_leader));
    let mut replies = Replies::new();
    until(
        &mut session,
        &mut cluster,
        &mut replies,
        "the session follows the next leader",
        |session, cluster, _| {
            cluster.led.is_some_and(|(leader, term)| {
                leader != first_leader
                    && (session.leader_member_id(), session.leadership_term_id()) == (leader, term)
            })
        },
    );
    let (leader, term) = cluster.led.expect("a leader");
    assert!(term > first_term);
    assert!(replies.is_empty(), "nothing was answered meanwhile");
    let (replies, took) = converse(&mut session, &mut cluster, b"two", 2);
    assert_eq!(took.len(), 2, "{took:?}");
    let entry = took
        .iter()
        .find(|took| took.member == leader)
        .expect("the leader's service took it");
    assert_eq!((entry.term, entry.reply), (term, Reply::Sent));
    assert!(
        took.iter()
            .all(|other| other.member == leader || other.reply == Reply::NotAnswering),
        "{took:?}"
    );
    assert_eq!(
        replies,
        [(format!("{leader}:two").into_bytes(), entry.timestamp)]
    );

    // The service beside the leader stops and another starts: it reads the log from
    // its start, and what its member held already it answers to nobody.
    cluster.ask(Ask::Restart(leader));
    let mut replies = Replies::new();
    until(
        &mut session,
        &mut cluster,
        &mut replies,
        "the new service took the two messages again",
        |_, cluster, _| {
            [&b"one"[..], b"two"].iter().all(|payload| {
                cluster
                    .took(payload)
                    .iter()
                    .any(|took| (took.member, took.generation) == (leader, 1))
            })
        },
    );
    for payload in [&b"one"[..], b"two"] {
        let again: Vec<_> = answers(&cluster.took(payload))
            .into_iter()
            .filter(|&(member, generation, _)| (member, generation) == (leader, 1))
            .collect();
        assert_eq!(again, [(leader, 1, Reply::NotAnswering)]);
    }
    let (replies_after, took) = converse(&mut session, &mut cluster, b"three", 2);
    assert!(
        replies.is_empty(),
        "the history was answered again: {replies:?}"
    );
    let follower = took
        .iter()
        .map(|took| took.member)
        .find(|&member| member != leader)
        .expect("the follower's service took it");
    assert_eq!(
        answers(&took),
        {
            // In the order of member, as `took` is.
            let mut expected = vec![(leader, 1, Reply::Sent), (follower, 0, Reply::NotAnswering)];
            expected.sort_by_key(|&(member, generation, _)| (member, generation));
            expected
        },
        "{took:?}"
    );
    let entry = took
        .iter()
        .find(|took| took.member == leader)
        .expect("the leader's service took it");
    assert_eq!(
        replies_after,
        [(format!("{leader}:three").into_bytes(), entry.timestamp)]
    );

    // No reply was dropped: each was sent or was not this service's to send.
    for id in [leader, follower] {
        assert_eq!(dropped(dir.path(), id), 0, "member {id}");
    }

    session.close().expect("the session closes");
    cavalry.close().expect("the client closes");
    cluster.stop();
    driver.close().expect("the client's driver closes");
}
