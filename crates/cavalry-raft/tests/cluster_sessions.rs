//! A client's session with a cluster of three members over three drivers on the
//! loopback (`spec/cluster.md`, "Sessions"; ADR 0115, ADR 0116). The client is made by
//! hand, on a driver of its own: a subscription on its response channel, a
//! publication to one member's `ingress` endpoint, and the messages of the session
//! protocol written and read with `cavalry-proto`. A follower redirects it; the leader
//! opens its session, appends its message to the log with the leader's term and the
//! cluster's time, and closes the session when the client is silent for the session
//! timeout. When the leader is lost, the member that replaces it has the session from
//! its log, and tells the client so.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cavalry_client::{Cavalry, CommittedLog, Publication, Subscription};
use cavalry_driver::Driver;
use cavalry_proto::cluster::{EGRESS_STREAM_ID, INGRESS_STREAM_ID, Message, Stream};
use cavalry_raft::{ConsensusModule, ElectionState, Role};
use cavalry_shm::counters::CountersReader;
use cavalry_shm::counters::generated::{
    CLUSTER_CONNECTS_DROPPED, CLUSTER_INGRESS_IGNORED, CLUSTER_MESSAGES_REJECTED, CLUSTER_SESSIONS,
    CLUSTER_SESSIONS_ACCEPTED, CLUSTER_SESSIONS_CLOSED, CLUSTER_SESSIONS_OPENED,
    CLUSTER_SESSIONS_TIMED_OUT,
};

struct TestDir(PathBuf);

impl TestDir {
    fn new(test: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "cavalry-raft-sessions-{}-{test}",
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

/// The members string of three members on the loopback, from fifteen ports, and each
/// member's `ingress` endpoint by its id.
fn members_on(ports: &[u16]) -> (String, Vec<String>) {
    let string = (0..3)
        .map(|member| {
            let endpoints: Vec<String> = ports[member * 5..member * 5 + 5]
                .iter()
                .map(|port| format!("127.0.0.1:{port}"))
                .collect();
            format!("{member},{}", endpoints.join(","))
        })
        .collect::<Vec<_>>()
        .join("|");
    let ingress = (0..3)
        .map(|member| format!("127.0.0.1:{}", ports[member * 5]))
        .collect();
    (string, ingress)
}

/// What a redirect and a new leader event list: every member's ingress endpoint.
fn listed(ingress: &[String]) -> String {
    ingress
        .iter()
        .enumerate()
        .map(|(id, endpoint)| format!("{id}={endpoint}"))
        .collect::<Vec<_>>()
        .join(",")
}

struct Member {
    driver: Driver,
    module: ConsensusModule,
    dir: PathBuf,
}

fn start(dir: &Path, members: &str) -> Vec<Member> {
    [1, 2, 0]
        .into_iter()
        .map(|id| {
            let driver_dir = dir.join(format!("member-{id}"));
            let log_dir = dir.join(format!("member-{id}-log"));
            let driver = Driver::start(&driver_dir, &log_dir).expect("the driver starts");
            let module = ConsensusModule::start(&driver_dir, &log_dir, members, id)
                .expect("the member starts");
            Member {
                driver,
                module,
                dir: driver_dir,
            }
        })
        .collect()
}

/// Drives every member's duty cycle until `done`, or fails with `what`.
fn run_until(members: &mut [Member], what: &str, mut done: impl FnMut(&[Member]) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !done(members) {
        assert!(
            Instant::now() < deadline,
            "{what}: {:?}",
            members
                .iter()
                .map(|m| format!("{:?}", m.module))
                .collect::<Vec<_>>()
        );
        let mut work = 0;
        for member in members.iter_mut() {
            work += member.module.do_work().expect("the member's duty cycle");
        }
        if work == 0 {
            std::thread::sleep(Duration::from_micros(200));
        }
    }
}

/// Runs the members until every election has closed behind one leader, and puts the
/// leader first.
fn run_until_led(members: &mut [Member]) {
    run_until(members, "every election closed", |members| {
        members
            .iter()
            .all(|m| m.module.election_state() == ElectionState::Closed)
            && members
                .iter()
                .filter(|m| m.module.role() == Role::Leader)
                .count()
                == 1
    });
    let leader = members
        .iter()
        .position(|m| m.module.role() == Role::Leader)
        .expect("one member leads");
    members.swap(0, leader);
}

/// As [`run_until_led`], with one follower made to join the log late, as
/// `tests/cluster.rs` makes one on every run of its tests (ADR 0104, Evidence). The
/// first member found in follower log init has taken the leader's term at position 0
/// and has not yet subscribed to the leader's log. It runs no duty cycle until the
/// other two have closed their elections and hold the term's first entry. Its image
/// of the log then joins past its append position: it fails its election and catches
/// up in the next, and its log is then in more than one recording, which its
/// module's read of its own committed log must cross. Returns the late follower's id.
fn run_until_led_with_a_late_follower(members: &mut [Member]) -> i32 {
    let mut late = None;
    run_until(members, "a member took a leader's term", |members| {
        late = members
            .iter()
            .position(|m| m.module.election_state() == ElectionState::FollowerLogInit);
        late.is_some()
    });
    let late = late.expect("a member in follower log init");
    let late_id = members[late].module.member_id();
    let deadline = Instant::now() + Duration::from_secs(60);
    while !members.iter().enumerate().all(|(index, m)| {
        index == late
            || (m.module.election_state() == ElectionState::Closed
                && m.module.append_position() >= 96)
    }) {
        assert!(
            Instant::now() < deadline,
            "the others closed without member {late_id}"
        );
        let mut work = 0;
        for (index, member) in members.iter_mut().enumerate() {
            if index != late {
                work += member.module.do_work().expect("the member's duty cycle");
            }
        }
        if work == 0 {
            std::thread::sleep(Duration::from_micros(200));
        }
    }
    run_until_led(members);
    let member = members
        .iter()
        .find(|m| m.module.member_id() == late_id)
        .expect("the late member");
    assert_eq!(member.module.role(), Role::Follower);
    let counts = member.module.counts();
    assert_eq!(
        (
            counts.join_position_mismatches,
            counts.elections_failed,
            counts.elections
        ),
        (1, 1, 2),
        "member {late_id} joined late: {counts:?}"
    );
    late_id
}

/// The value of the member's counter of `type_id`.
fn counter(member: &Member, type_id: i32) -> i64 {
    let reader = CountersReader::open(&member.dir).expect("the counters file opens");
    let key = member.module.member_id().to_le_bytes();
    for id in 0..reader.capacity() {
        if let Some(record) = reader.record(id).expect("a record")
            && record.type_id() == type_id
            && record.key().starts_with(&key)
        {
            return reader.read_acquire(id).expect("the counter is read");
        }
    }
    panic!(
        "member {} has no counter of type {type_id}",
        member.module.member_id()
    );
}

/// A client of the cluster, by hand, on a driver of its own.
struct Client {
    driver: Driver,
    cavalry: Cavalry,
    response_channel: String,
    egress: Subscription,
    ingress: Option<Publication>,
    /// What arrived on the response channel, each message as a line.
    events: Vec<String>,
}

impl Client {
    fn start(dir: &Path, port: u16) -> Self {
        let driver_dir = dir.join("client");
        let driver =
            Driver::start(&driver_dir, dir.join("client-log")).expect("the client's driver");
        let mut cavalry = Cavalry::connect_to(&driver_dir).expect("connects");
        let response_channel = format!("cavalry:udp?endpoint=127.0.0.1:{port}");
        let egress = cavalry
            .add_subscription(&response_channel, EGRESS_STREAM_ID)
            .expect("the egress subscription");
        Self {
            driver,
            cavalry,
            response_channel,
            egress,
            ingress: None,
            events: Vec::new(),
        }
    }

    /// Turns to the member whose `ingress` endpoint is `endpoint`: a publication to
    /// it, in place of the one held.
    fn turn_to(&mut self, endpoint: &str) {
        if let Some(publication) = self.ingress.take() {
            publication.close().expect("the publication closes");
        }
        self.ingress = Some(
            self.cavalry
                .add_exclusive_publication(
                    &format!("cavalry:udp?endpoint={endpoint}"),
                    INGRESS_STREAM_ID,
                )
                .expect("the ingress publication"),
        );
    }

    fn connect(&self, correlation_id: i64) -> Vec<u8> {
        encode(&Message::SessionConnect {
            correlation_id,
            response_stream_id: EGRESS_STREAM_ID,
            version: 0,
            response_channel: self.response_channel.as_bytes(),
            encoded_credentials: &[],
            client_info: b"cluster_sessions.rs",
        })
    }

    /// Offers `bytes` to the member turned to; `false` while the publication does not
    /// take it, which is until the member's subscription has connected it.
    fn send(&mut self, bytes: &[u8]) -> bool {
        self.ingress
            .as_mut()
            .expect("turned to a member")
            .offer(bytes)
            .is_ok()
    }

    fn poll(&mut self) {
        let events = &mut self.events;
        self.egress
            .poll(
                |body, _| {
                    let line =
                        match Message::parse(Stream::Egress, body).expect("an egress message") {
                            Message::SessionEvent {
                                cluster_session_id,
                                correlation_id,
                                leadership_term_id,
                                leader_member_id,
                                code,
                                detail,
                                ..
                            } => format!(
                                "event {code:?} session {cluster_session_id} correlation \
                             {correlation_id} term {leadership_term_id} leader \
                             {leader_member_id} detail {:?}",
                                String::from_utf8_lossy(detail)
                            ),
                            Message::NewLeaderEvent {
                                leadership_term_id,
                                cluster_session_id,
                                leader_member_id,
                                ingress_endpoints,
                            } => format!(
                                "new leader session {cluster_session_id} term \
                             {leadership_term_id} leader {leader_member_id} endpoints {:?}",
                                String::from_utf8_lossy(ingress_endpoints)
                            ),
                            other => format!("{other:?}"),
                        };
                    events.push(line);
                },
                16,
            )
            .expect("the egress polls");
    }

    fn close(self) {
        drop(self.ingress);
        drop(self.egress);
        self.cavalry.close().expect("the client closes");
        self.driver.close().expect("the client's driver closes");
    }
}

fn encode(message: &Message<'_>) -> Vec<u8> {
    let mut bytes = vec![0u8; message.encoded_length()];
    let length = message.write(&mut bytes).expect("encodes");
    bytes.truncate(length);
    bytes
}

/// Sends `bytes` once the client's publication takes them, and runs the members until
/// the client has read `events` messages on its response channel.
fn converse(members: &mut [Member], client: &mut Client, what: &str, bytes: &[u8], events: usize) {
    let mut sent = false;
    run_until(members, what, |_| {
        sent = sent || client.send(bytes);
        client.poll();
        client.events.len() >= events
    });
}

/// A service beside one member: it reads the member's committed log from position 0,
/// across the member's recordings, into `entries`.
struct Service {
    cavalry: Cavalry,
    log: Option<CommittedLog>,
    position: i64,
    entries: Vec<Vec<u8>>,
}

impl Service {
    fn beside(member: &Member) -> Self {
        Self {
            cavalry: Cavalry::connect_to(&member.dir).expect("connects"),
            log: None,
            position: 0,
            entries: Vec::new(),
        }
    }

    fn poll(&mut self) {
        if self.log.is_none() {
            self.log = self
                .cavalry
                .committed_log(self.position)
                .expect("the committed log opens");
        }
        let Some(log) = self.log.as_mut() else {
            return;
        };
        let entries = &mut self.entries;
        log.poll(|entry, _, _| entries.push(entry.to_vec()), 64)
            .expect("the committed log polls");
        self.position = log.position();
        if log.is_ended() {
            self.log
                .take()
                .expect("held")
                .close(&mut self.cavalry)
                .expect("the committed log closes");
        }
    }

    /// The session messages read so far: each one's term, session id, whether its
    /// time is the cluster's and not the client's 0, and its payload.
    fn session_messages(&self) -> Vec<(i64, i64, bool, Vec<u8>)> {
        self.entries
            .iter()
            .filter_map(
                |entry| match Message::parse(Stream::Log, entry).expect("an entry") {
                    Message::SessionMessage {
                        leadership_term_id,
                        cluster_session_id,
                        timestamp,
                        payload,
                    } => Some((
                        leadership_term_id,
                        cluster_session_id,
                        timestamp > 1_600_000_000_000,
                        payload.to_vec(),
                    )),
                    _ => None,
                },
            )
            .collect()
    }

    fn close(mut self) {
        if let Some(log) = self.log.take() {
            log.close(&mut self.cavalry)
                .expect("the committed log closes");
        }
        self.cavalry.close().expect("the service's client closes");
    }
}

/// A follower redirects a connect to its leader with every member's ingress endpoint.
/// The leader opens the session: the ok event carries the connect's correlation id
/// and the first session id, and every member holds the session, read from its own
/// log. The session's message is committed with the leader's term and the cluster's
/// time, and a service beside the leader reads it. The client then says nothing, and
/// after the session timeout the leader closes the session and tells it so.
#[test]
fn a_client_is_redirected_opens_a_session_and_is_closed_when_it_falls_silent() {
    let dir = TestDir::new("open");
    let ports = free_ports(16);
    let (members_string, ingress) = members_on(&ports[..15]);
    let endpoints = listed(&ingress);
    let mut members = start(dir.path(), &members_string);
    run_until_led(&mut members);
    let leader = members[0].module.member_id();
    let follower = members[1].module.member_id();
    let term = members[0].module.leadership_term_id();
    let mut client = Client::start(dir.path(), ports[15]);

    client.turn_to(&ingress[follower as usize]);
    let connect = client.connect(1);
    converse(
        &mut members,
        &mut client,
        "a follower redirected the connect",
        &connect,
        1,
    );
    assert_eq!(
        client.events,
        [format!(
            "event Redirect session -1 correlation 1 term {term} leader {leader} detail \
             {endpoints:?}"
        )]
    );

    client.events.clear();
    client.turn_to(&ingress[leader as usize]);
    let connect = client.connect(2);
    converse(
        &mut members,
        &mut client,
        "the leader opened the session",
        &connect,
        1,
    );
    assert_eq!(
        client.events,
        [format!(
            "event Ok session 0 correlation 2 term {term} leader {leader} detail \"\""
        )]
    );
    run_until(&mut members, "every member holds the session", |members| {
        members.iter().all(|m| counter(m, CLUSTER_SESSIONS.0) == 1)
    });
    for member in &members {
        assert_eq!(counter(member, CLUSTER_SESSIONS_OPENED.0), 1);
        let accepted = i64::from(member.module.member_id() == leader);
        assert_eq!(counter(member, CLUSTER_SESSIONS_ACCEPTED.0), accepted);
    }

    // The session's message, with the client's term and a time of 0: the leader
    // appends it with its own term and the cluster's time.
    let message = encode(&Message::SessionMessage {
        leadership_term_id: term,
        cluster_session_id: 0,
        timestamp: 0,
        payload: b"hello, cluster",
    });
    let mut service = Service::beside(&members[0]);
    let mut sent = false;
    let asked = Instant::now();
    run_until(&mut members, "a service read the session's message", |_| {
        sent = sent || client.send(&message);
        service.poll();
        !service.session_messages().is_empty()
    });
    assert_eq!(
        service.session_messages(),
        [(term, 0, true, b"hello, cluster".to_vec())]
    );

    // Nothing more from the client: closed 10 s after its message, and told so.
    client.events.clear();
    run_until(&mut members, "the silent session was closed", |_| {
        client.poll();
        !client.events.is_empty()
    });
    assert!(
        asked.elapsed() >= Duration::from_secs(10),
        "closed {:?} after its last message",
        asked.elapsed()
    );
    assert_eq!(
        client.events,
        [format!(
            "event Closed session 0 correlation -1 term {term} leader {leader} detail \
             \"TIMEOUT\""
        )]
    );
    run_until(&mut members, "every member closed the session", |members| {
        members.iter().all(|m| counter(m, CLUSTER_SESSIONS.0) == 0)
    });
    for member in &members {
        let leads = i64::from(member.module.member_id() == leader);
        assert_eq!(counter(member, CLUSTER_SESSIONS_CLOSED.0), 1);
        assert_eq!(counter(member, CLUSTER_SESSIONS_TIMED_OUT.0), leads);
        assert_eq!(counter(member, CLUSTER_CONNECTS_DROPPED.0), 0);
        assert_eq!(counter(member, CLUSTER_INGRESS_IGNORED.0), 0);
        assert_eq!(counter(member, CLUSTER_MESSAGES_REJECTED.0), 0);
    }

    service.close();
    client.close();
    for member in members {
        member.module.close().expect("the member closes");
        member.driver.close().expect("the driver closes");
    }
}

/// A session survives its leader: the two members that remain elect one of them, which
/// holds the session from its own log, sends the client the new leader event of its
/// term, and appends the message the client then sends it with the new term. The
/// client's session close closes the session on both. One of the two joined the log
/// late, on every run.
#[test]
fn a_session_survives_its_leader_and_is_told_of_the_new_one() {
    let dir = TestDir::new("leader-lost");
    let ports = free_ports(16);
    let (members_string, ingress) = members_on(&ports[..15]);
    let endpoints = listed(&ingress);
    let mut members = start(dir.path(), &members_string);
    // One of the two that will remain joined the log late: its log is in more than
    // one recording, and it holds the session all the same, as the member that leads
    // next or as the one that follows it.
    let late = run_until_led_with_a_late_follower(&mut members);
    let first_leader = members[0].module.member_id();
    assert_ne!(late, first_leader);
    let first_term = members[0].module.leadership_term_id();
    let mut client = Client::start(dir.path(), ports[15]);
    client.turn_to(&ingress[first_leader as usize]);
    let connect = client.connect(7);
    converse(
        &mut members,
        &mut client,
        "the leader opened the session",
        &connect,
        1,
    );
    assert_eq!(
        client.events,
        [format!(
            "event Ok session 0 correlation 7 term {first_term} leader {first_leader} detail \
             \"\""
        )]
    );
    run_until(&mut members, "every member holds the session", |members| {
        members.iter().all(|m| counter(m, CLUSTER_SESSIONS.0) == 1)
    });

    // The leader is lost, its driver with it.
    let lost = members.remove(0);
    drop(lost.module);
    lost.driver.close().expect("the driver closes");
    client.events.clear();
    run_until(&mut members, "the client heard of the new leader", |_| {
        client.poll();
        !client.events.is_empty()
    });
    let at = members
        .iter()
        .position(|m| m.module.role() == Role::Leader)
        .expect("one member leads");
    members.swap(0, at);
    let leader = members[0].module.member_id();
    let term = members[0].module.leadership_term_id();
    assert!(leader != first_leader && term > first_term);
    assert_eq!(
        client.events,
        [format!(
            "new leader session 0 term {term} leader {leader} endpoints {endpoints:?}"
        )]
    );
    for member in &members {
        assert_eq!(
            counter(member, CLUSTER_SESSIONS.0),
            1,
            "the session is the log's"
        );
        assert_eq!(counter(member, CLUSTER_SESSIONS_ACCEPTED.0), 0);
    }

    // The client turns to the new leader and sends with the new term.
    client.turn_to(&ingress[leader as usize]);
    let message = encode(&Message::SessionMessage {
        leadership_term_id: term,
        cluster_session_id: 0,
        timestamp: 0,
        payload: b"after the election",
    });
    let mut service = Service::beside(&members[0]);
    let mut sent = false;
    run_until(&mut members, "a service read the session's message", |_| {
        sent = sent || client.send(&message);
        service.poll();
        !service.session_messages().is_empty()
    });
    assert_eq!(
        service.session_messages(),
        [(term, 0, true, b"after the election".to_vec())]
    );

    // The client closes its session: nothing is sent to it, and both members close it.
    client.events.clear();
    let close = encode(&Message::SessionClose {
        leadership_term_id: term,
        cluster_session_id: 0,
    });
    let mut sent = false;
    run_until(&mut members, "both members closed the session", |members| {
        sent = sent || client.send(&close);
        client.poll();
        members.iter().all(|m| counter(m, CLUSTER_SESSIONS.0) == 0)
    });
    assert_eq!(client.events, [] as [String; 0]);
    for member in &members {
        assert_eq!(counter(member, CLUSTER_SESSIONS_OPENED.0), 1);
        assert_eq!(counter(member, CLUSTER_SESSIONS_CLOSED.0), 1);
        assert_eq!(counter(member, CLUSTER_SESSIONS_TIMED_OUT.0), 0);
        assert_eq!(counter(member, CLUSTER_MESSAGES_REJECTED.0), 0);
    }

    service.close();
    client.close();
    for member in members {
        member.module.close().expect("the member closes");
        member.driver.close().expect("the driver closes");
    }
}
