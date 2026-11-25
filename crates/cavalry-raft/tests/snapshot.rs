//! Three members over three drivers on the loopback take the snapshot their leader
//! asks for (`spec/cluster.md`, "Snapshots"; ADR 0129): every member's consensus module
//! writes its state at the position after the cluster action, to a recording of its
//! own, and lists it in its snapshots file (`spec/durable-log.md`, "Snapshots"; ADR
//! 0128). The tests hold the snapshot to invariant 1: the three members' snapshots are
//! of one position and hold the same bytes, and a member that reads the action again,
//! after it started again on the log it kept, takes no second snapshot of it.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cavalry_client::{
    Action, Cavalry, ClientError, ClusterError, ClusterService, Entry, FOLLOW_LENGTH, NULL_LENGTH,
    NULL_POSITION, Publication, Restored, SnapshotCause, Subscription,
};
use cavalry_driver::Driver;
use cavalry_driver::spec::{MAX_PUBLICATIONS, MAX_REPLAYS};
use cavalry_log::recordings::RecordingsFile;
use cavalry_log::snapshots::{CONSENSUS_MODULE, ListedSnapshot, SnapshotsFile};
use cavalry_proto::client_protocol::ErrorCode;
use cavalry_proto::cluster::{
    CLOSE_REASON_NONE, CONSENSUS_MODULE_STREAM_ID, EGRESS_STREAM_ID, INGRESS_STREAM_ID,
    MAX_MEMBER_TIMERS, MAX_SERVICE_SNAPSHOT_STATE_LENGTH, Message, SERVICE_SNAPSHOT_TYPE_ID,
    SNAPSHOT_TYPE_ID, SnapshotMark, Stream,
};
use cavalry_raft::{
    ConsensusModule, ElectionState, ListedFound, RaftError, Refused, Role, ServiceSnapshotCounts,
    SnapshotCounts, TimerCounts,
};
use cavalry_shm::counters::CountersReader;
use cavalry_shm::counters::generated::{
    CLUSTER_ELECTION_CAUSE, CLUSTER_ELECTIONS, CLUSTER_ELECTIONS_CLOSED, CLUSTER_ELECTIONS_FAILED,
    CLUSTER_LEADER_HEARTBEAT_TIMEOUTS, CLUSTER_LEADER_QUORUM_LOSSES, CLUSTER_LEADER_STEP_DOWNS,
    CLUSTER_SESSIONS_TIMED_OUT,
};
use cavalry_shm::counters::generated::{
    CLUSTER_LOG_PURGE_POSITION, CLUSTER_MESSAGES_REJECTED, CLUSTER_SERVICE_ACKS_REJECTED,
    CLUSTER_SERVICE_SNAPSHOT_POSITION, CLUSTER_SERVICE_SNAPSHOT_RECORDING_ID,
    CLUSTER_SERVICE_SNAPSHOT_START_POSITION, CLUSTER_SERVICE_SNAPSHOTS_FAILED,
    CLUSTER_SERVICE_SNAPSHOTS_LISTED, CLUSTER_SERVICE_SNAPSHOTS_PASSED_OVER,
    CLUSTER_SERVICE_SNAPSHOTS_TAKEN, CLUSTER_SESSIONS, CLUSTER_SESSIONS_ACCEPTED,
    CLUSTER_SESSIONS_CLOSED, CLUSTER_SESSIONS_OPENED, CLUSTER_SNAPSHOT_ACTIONS,
    CLUSTER_SNAPSHOT_POSITION, CLUSTER_SNAPSHOT_START_POSITION, CLUSTER_SNAPSHOT_STEPS,
    CLUSTER_SNAPSHOTS_BEGUN, CLUSTER_SNAPSHOTS_FAILED, CLUSTER_SNAPSHOTS_PASSED_OVER,
    CLUSTER_SNAPSHOTS_TAKEN, CLUSTER_TIMER_EVENTS, CLUSTER_TIMERS, CLUSTER_TIMERS_CLEARED,
    CLUSTER_TIMERS_REPLACED, CLUSTER_TRUNCATIONS_FINISHED,
};

mod ports;
use ports::free_ports;

/// The length in the log of one entry these tests offer, of a new leadership term
/// event and of a cluster action: a 32-byte frame header and a message of at most 64
/// bytes, aligned to 32.
const FRAME: i64 = 96;

/// The stream a test replays a snapshot's recording into: one no member uses.
const REPLAY_STREAM_ID: i32 = 2300;

struct TestDir(PathBuf);

impl TestDir {
    fn new(test: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "cavalry-raft-snapshot-{}-{test}",
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

/// The members string of three members on the loopback.
fn members_string() -> String {
    members_string_on(&free_ports(15))
}

/// The members string of three members on the loopback, on the first 15 of `ports`:
/// member n's ingress endpoint is the port at 5 n.
fn members_string_on(ports: &[u16]) -> String {
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

struct Member {
    driver: Driver,
    module: ConsensusModule,
    dir: PathBuf,
    log_dir: PathBuf,
}

/// Starts member `id` on its directories under `dir`: a driver, and the member over
/// it. A log directory that is there is the one the member kept.
fn start_member(dir: &Path, members: &str, id: i32) -> Member {
    let driver_dir = dir.join(format!("member-{id}"));
    let log_dir = dir.join(format!("member-{id}-log"));
    let driver = timed(&format!("member {id}: Driver::start"), || {
        Driver::start(&driver_dir, &log_dir).expect("the driver starts")
    });
    let module = timed(&format!("member {id}: ConsensusModule::start"), || {
        ConsensusModule::start(&driver_dir, &log_dir, members, id)
            .expect("the member starts, on a new log directory or on the one it kept")
    });
    Member {
        driver,
        module,
        dir: driver_dir,
        log_dir,
    }
}

thread_local! {
    /// When this test's members last ran a round of duty cycles.
    static LAST_ROUND: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

/// Notes a round of the members' duty cycles, and says on standard error when the
/// members stood still for a second or more before it: one thread drives them all,
/// so a step of the test that waits, a start, a stop, a replay, a snapshot, stops
/// every member, and the cluster's timeouts are of real time.
fn round() {
    LAST_ROUND.with(|last| {
        let now = Instant::now();
        if let Some(before) = last.replace(Some(now))
            && now.duration_since(before) >= Duration::from_secs(1)
        {
            eprintln!(
                "the members stood still for {:?}",
                now.duration_since(before)
            );
        }
    });
}

/// Runs `step`, a step of a test that waits on a driver or a disk, and says on
/// standard error how long it took when that is 200 ms or more.
///
/// `CAVALRY_TEST_STEP_DELAY_MS` makes every such step that much slower, which is a
/// runner with a slow disk on any machine. At 4000, `cargo test -p cavalry-raft
/// --test snapshot` takes two minutes, and three such steps in a row on a test's own
/// thread fail the test, which is how the tests failed on the runner. At 11000,
/// above the cluster's timeouts of 10 s, one such step fails its test with an
/// election or a closed session, and so does a session that no keepalive holds
/// open; the whole file then takes more than half an hour, so name a test (work
/// item 446).
fn timed<T>(what: &str, step: impl FnOnce() -> T) -> T {
    let began = Instant::now();
    if let Some(delay) = std::env::var("CAVALRY_TEST_STEP_DELAY_MS")
        .ok()
        .and_then(|delay| delay.parse::<u64>().ok())
    {
        std::thread::sleep(Duration::from_millis(delay));
    }
    let out = step();
    if began.elapsed() >= Duration::from_millis(200) {
        eprintln!("{what} took {:?}", began.elapsed());
    }
    out
}

/// Runs `step`, a step of a test that waits on a driver or on a disk, on a thread of
/// its own, and drives `members` and their sessions' keepalives until it returns.
///
/// One thread drives every member of a test, and a cluster's timeouts are of real
/// time: a follower that hears nothing from its leader for the leader heartbeat
/// timeout holds an election, and a leader closes a session it has heard nothing
/// from for the session timeout, both 10 s (`spec/cluster.md`, "Values"). A step
/// that the test's thread waits on stops every duty cycle and every keepalive for
/// as long as it takes. On a laptop that is milliseconds. On a runner whose disk
/// takes a second to sync, a member's start is two seconds and a service's
/// snapshot three, and a few of them in a row are the ten (work item 446). In a
/// cluster that runs, each member has a thread of its own and no step of another
/// program stops it: this is that.
fn meanwhile<T: Send>(members: &mut [Member], step: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|scope| {
        let step = scope.spawn(step);
        while !step.is_finished() {
            round();
            keep_sessions_alive(members);
            let mut work = 0;
            for member in members.iter_mut() {
                work += member.module.do_work().expect("the member's duty cycle");
            }
            if work == 0 {
                std::thread::sleep(Duration::from_micros(200));
            }
        }
        match step.join() {
            Ok(out) => out,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    })
}

/// A member and a service are handed to the thread a step runs on.
#[allow(dead_code)]
fn handed_to_a_steps_thread() {
    fn send<T: Send>() {}
    send::<Member>();
    send::<Service>();
}

/// Drives every member's duty cycle until `done`, or fails with `what`.
fn run_until(members: &mut [Member], what: &str, mut done: impl FnMut(&[Member]) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !done(members) {
        keep_sessions_alive(members);
        assert!(
            Instant::now() < deadline,
            "{what}: {:?}; {}",
            members
                .iter()
                .map(|m| format!("{:?} {:?}", m.module, m.module.snapshot_counts()))
                .collect::<Vec<_>>(),
            describe(members)
        );
        round();
        let mut work = 0;
        for member in members.iter_mut() {
            work += member.module.do_work().expect("the member's duty cycle");
        }
        if work == 0 {
            std::thread::sleep(Duration::from_micros(200));
        }
    }
}

/// The sessions of this test's clients that the run loop keeps alive: for each, its
/// cluster session id and a publication to every member's `ingress` endpoint, by
/// the member's id. A test runs on a thread of its own, so the sessions are its own.
#[derive(Default)]
struct KeptAlive {
    sessions: Vec<(i64, Vec<(i32, Publication)>)>,
    /// When the keepalives were last sent.
    sent: Option<Instant>,
}

thread_local! {
    static KEPT_ALIVE: RefCell<KeptAlive> = RefCell::new(KeptAlive::default());
}

/// How often the run loop sends a keepalive for each session: a client's keepalive
/// interval is 500 ms, and a leader closes a session it has heard nothing of for
/// the session timeout, 10 s (`spec/cluster.md`, "Sessions").
const KEEPALIVE_EVERY: Duration = Duration::from_millis(250);

/// Sends a session keepalive for each session of this test's clients to the member
/// that leads, in its term, as a client that runs does. Without it a test that takes
/// longer than the session timeout, as on a loaded runner, has its leader close both
/// sessions, and their two session close events are entries the test did not ask
/// for. A keepalive that a publication does not take, or that reaches a member which
/// no longer leads, is sent again at the next turn.
fn keep_sessions_alive(members: &[Member]) {
    KEPT_ALIVE.with(|kept| {
        let mut kept = kept.borrow_mut();
        if kept.sessions.is_empty()
            || kept
                .sent
                .is_some_and(|sent| sent.elapsed() < KEEPALIVE_EVERY)
        {
            return;
        }
        let Some(leader) = members.iter().find(|m| {
            m.module.role() == Role::Leader && m.module.election_state() == ElectionState::Closed
        }) else {
            return;
        };
        kept.sent = Some(Instant::now());
        let leader_id = leader.module.member_id();
        let leadership_term_id = leader.module.leadership_term_id();
        for (cluster_session_id, publications) in kept.sessions.iter_mut() {
            let message = Message::SessionKeepalive {
                leadership_term_id,
                cluster_session_id: *cluster_session_id,
            };
            let mut bytes = [0u8; 64];
            let length = message.write(&mut bytes).expect("encodes");
            if let Some((_, publication)) = publications
                .iter_mut()
                .find(|(member, _)| *member == leader_id)
            {
                let _ = publication.offer(&bytes[..length]);
            }
        }
    });
}

/// Runs the members until they are led in a term above `above`, then puts the member
/// that leads first.
fn run_until_led(members: &mut [Member], above: i64) {
    run_until(members, "the members have a leader", |members| {
        let term = members[0].module.leadership_term_id();
        term > above
            && members.iter().all(|m| {
                m.module.election_state() == ElectionState::Closed
                    && m.module.leadership_term_id() == term
            })
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

/// The leader, `members[0]`, appends `count` entries numbered from `from`, a
/// back-pressured offer made again once every member has run.
fn offer_entries(members: &mut [Member], from: i64, count: i64) {
    for i in from..from + count {
        loop {
            match members[0].module.offer(&i.to_le_bytes()) {
                Ok(()) => break,
                Err(RaftError::Offer(Refused::BackPressured)) => {
                    round();
                    for member in members.iter_mut() {
                        member.module.do_work().expect("the member's duty cycle");
                    }
                }
                Err(other) => panic!("{other}: {}", describe(members)),
            }
        }
    }
}

/// What each member says of itself and of its elections, for a failure's message:
/// its role, its election's state, its term and its positions, and the counters
/// that say why it last held an election (`spec/cluster.md`, "Counters").
fn describe(members: &[Member]) -> String {
    members
        .iter()
        .map(|member| {
            format!(
                "{:?} elections {} closed {} failed {} cause {} heartbeat timeouts {} quorum \
                 losses {} step downs {} sessions timed out {} snapshots {:?}",
                member.module,
                counter(member, CLUSTER_ELECTIONS.0),
                counter(member, CLUSTER_ELECTIONS_CLOSED.0),
                counter(member, CLUSTER_ELECTIONS_FAILED.0),
                counter(member, CLUSTER_ELECTION_CAUSE.0),
                counter(member, CLUSTER_LEADER_HEARTBEAT_TIMEOUTS.0),
                counter(member, CLUSTER_LEADER_QUORUM_LOSSES.0),
                counter(member, CLUSTER_LEADER_STEP_DOWNS.0),
                counter(member, CLUSTER_SESSIONS_TIMED_OUT.0),
                listed(member)
                    .iter()
                    .map(|entry| (entry.service_id, entry.log_position))
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// The leader, `members[0]`, asks for a snapshot, a back-pressured ask made again once
/// every member has run.
fn ask_snapshot(members: &mut [Member]) {
    loop {
        match members[0].module.snapshot() {
            Ok(()) => break,
            Err(RaftError::Offer(Refused::BackPressured)) => {
                round();
                for member in members.iter_mut() {
                    member.module.do_work().expect("the member's duty cycle");
                }
            }
            Err(other) => panic!("{other}: {}", describe(members)),
        }
    }
}

/// Runs the members until each has appended and committed to `end`.
fn run_until_committed(members: &mut [Member], what: &str, end: i64) {
    run_until(members, what, |members| {
        members
            .iter()
            .all(|m| m.module.commit_position() == end && m.module.append_position() == end)
    });
}

/// The value of the member's counter of `type_id`, found in its driver's counters file
/// by the type and the member id in the key.
fn counter(member: &Member, type_id: i32) -> i64 {
    counter_in(&member.dir, member.module.member_id(), type_id)
}

/// The value of the counter of `type_id` whose key begins with `member_id`, in the
/// counters file of the driver in `dir`: a member's, or its service's.
fn counter_in(dir: &Path, member_id: i32, type_id: i32) -> i64 {
    let reader = CountersReader::open(dir).expect("the counters file opens");
    let key = member_id.to_le_bytes();
    for id in 0..reader.capacity() {
        if let Some(record) = reader.record(id).expect("a record")
            && record.type_id() == type_id
            && record.key().starts_with(&key)
        {
            return reader.read_acquire(id).expect("the counter is read");
        }
    }
    panic!("member {member_id} has no counter of type {type_id}");
}

/// The snapshots the member's file lists, read from its log directory.
fn listed(member: &Member) -> Vec<ListedSnapshot> {
    SnapshotsFile::open(&member.log_dir, member.module.member_id())
        .expect("the snapshots file opens")
        .entries()
        .to_vec()
}

/// One recording of each member, the one `which` names for it, played back while the
/// members run: a replay waits on the driver, and three of them in a row are a long
/// time for a cluster to stand still on a runner with a slow disk.
fn replayed_each(members: &mut [Member], which: impl Fn(&Member) -> i64) -> Vec<Vec<Vec<u8>>> {
    let wanted: Vec<(PathBuf, i64)> = members
        .iter()
        .map(|member| (member.dir.clone(), which(member)))
        .collect();
    meanwhile(members, || {
        wanted
            .iter()
            .map(|(dir, recording_id)| replayed(dir, *recording_id))
            .collect()
    })
}

/// The messages a snapshot's recording holds, each as its bytes: the recording played
/// back whole through a client of the driver whose directory is `dir`, a member's.
fn replayed(dir: &Path, recording_id: i64) -> Vec<Vec<u8>> {
    timed(&format!("a replay of recording {recording_id}"), || {
        replayed_whole(dir, recording_id)
    })
}

/// [`replayed`], untimed.
fn replayed_whole(dir: &Path, recording_id: i64) -> Vec<Vec<u8>> {
    let mut client = Cavalry::connect_to(dir).expect("a client of the member's driver");
    let mut subscription = client
        .add_subscription("cavalry:ipc", REPLAY_STREAM_ID)
        .expect("a subscription for the replay");
    let replay = client
        .start_replay(
            recording_id,
            NULL_POSITION,
            NULL_LENGTH,
            REPLAY_STREAM_ID,
            "cavalry:ipc",
        )
        .expect("the snapshot's recording replays");
    let mut messages = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    // How far the replay's image has been read. The driver withdraws the image once
    // its one subscriber has read it to the end of stream, which can be before this
    // client has read `replay stopped` (`spec/client-protocol.md`, "Commands", start
    // replay). The image is then gone by the time the end is known, so the position
    // is kept from the polls that still had the image.
    let mut reached = 0;
    loop {
        let read = subscription
            .poll(|bytes, _| messages.push(bytes.to_vec()), 16)
            .expect("the replay's image is read");
        for image in subscription.images() {
            reached = reached.max(image.position());
        }
        // The replay has ended, and the image was read to where it ended.
        if let Some((end, _)) = replay.ended()
            && reached >= end
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the replay of recording {recording_id} did not end: {} messages read to \
             position {reached}, and the replay's end is {:?}",
            messages.len(),
            replay.ended()
        );
        if read == 0 {
            std::thread::sleep(Duration::from_micros(200));
        }
    }
    drop(subscription);
    client.close().expect("the client closes");
    messages
}

#[test]
fn every_member_takes_the_snapshot_its_leader_asks_for_and_the_three_hold_the_same_bytes() {
    let dir = TestDir::new("taken");
    let members_string = members_string();
    let mut members: Vec<Member> = [1, 0, 2]
        .into_iter()
        .map(|id| start_member(dir.path(), &members_string, id))
        .collect();
    run_until_led(&mut members, -1);
    // A new member lists no snapshot, and says so in its counter.
    for member in &members {
        assert_eq!(member.module.snapshot_position(), None);
        assert_eq!(counter(member, CLUSTER_SNAPSHOT_POSITION.0), -1);
        assert_eq!(listed(member), vec![]);
    }
    // A follower is refused: the leader alone appends the action.
    let refused = members[1]
        .module
        .snapshot()
        .expect_err("a follower asks for no snapshot");
    assert!(
        matches!(refused, RaftError::Offer(Refused::NotLeader)),
        "{refused}"
    );

    // The term's event and five entries, then the action: the snapshot is of the
    // position after the action's entry.
    offer_entries(&mut members, 0, 5);
    run_until_committed(&mut members, "five entries are committed", 6 * FRAME);
    ask_snapshot(&mut members);
    let position = 7 * FRAME;
    run_until(&mut members, "every member lists its snapshot", |members| {
        members
            .iter()
            .all(|m| m.module.snapshot_position() == Some(position))
    });
    let term = members[0].module.leadership_term_id();
    let mut bytes: Vec<Vec<Vec<u8>>> = Vec::new();
    let mut timestamps = Vec::new();
    // Each member's recording, played back while the members run.
    let mut replays = std::collections::VecDeque::from(replayed_each(&mut members, |member| {
        listed(member)[0].recording_id
    }));
    for (index, member) in members.iter().enumerate() {
        let id = member.module.member_id();
        assert_eq!(
            member.module.snapshot_counts(),
            SnapshotCounts {
                actions: u64::from(index == 0),
                begun: 1,
                steps: 1,
                taken: 1,
                failed: 0,
            },
            "member {id}"
        );
        for (type_id, value) in [
            (CLUSTER_SNAPSHOT_POSITION, position),
            (CLUSTER_SNAPSHOT_ACTIONS, i64::from(index == 0)),
            (CLUSTER_SNAPSHOTS_BEGUN, 1),
            (CLUSTER_SNAPSHOT_STEPS, 1),
            (CLUSTER_SNAPSHOTS_TAKEN, 1),
            (CLUSTER_SNAPSHOTS_FAILED, 0),
        ] {
            assert_eq!(
                counter(member, type_id.0),
                value,
                "member {id}, {type_id:?}"
            );
        }
        let list = listed(member);
        assert_eq!(list.len(), 1, "member {id}: {list:?}");
        let snapshot = list[0];
        assert_eq!(
            (
                snapshot.service_id,
                snapshot.log_position,
                snapshot.leadership_term_id,
                snapshot.term_base_log_position
            ),
            (CONSENSUS_MODULE, position, term, 0),
            "member {id}"
        );
        timestamps.push(snapshot.timestamp);
        // The recording holds a marker, the module's own state and a marker: no
        // session is open.
        let messages = replays.pop_front().expect("a replay for each member");
        let parsed: Vec<Message<'_>> = messages
            .iter()
            .map(|bytes| Message::parse(Stream::Snapshot, bytes).expect("a snapshot's message"))
            .collect();
        let marker = |mark| Message::SnapshotMarker {
            type_id: SNAPSHOT_TYPE_ID,
            log_position: position,
            leadership_term_id: term,
            index: 0,
            mark,
            time_unit: cavalry_proto::cluster::TimeUnit::Millis,
            app_version: 0,
        };
        assert_eq!(
            parsed,
            vec![
                marker(SnapshotMark::Begin),
                Message::ConsensusModule {
                    next_session_id: 0,
                    next_service_session_id: 0,
                    log_service_session_id: 0,
                    pending_message_capacity: 0,
                },
                marker(SnapshotMark::End),
            ],
            "member {id}"
        );
        bytes.push(messages);
    }
    // One action, one time: every member's entry carries the action's.
    assert!(timestamps[0] > 0, "{timestamps:?}");
    assert!(
        timestamps.iter().all(|t| *t == timestamps[0]),
        "{timestamps:?}"
    );
    assert_eq!(bytes[0], bytes[1], "the leader's snapshot and a follower's");
    assert_eq!(
        bytes[0], bytes[2],
        "the leader's snapshot and the other follower's"
    );

    // A second action, further on: the newest snapshot moves, and the list holds both.
    offer_entries(&mut members, 5, 3);
    ask_snapshot(&mut members);
    let second = position + 4 * FRAME;
    run_until(
        &mut members,
        "every member lists its second snapshot",
        |members| {
            members
                .iter()
                .all(|m| m.module.snapshot_position() == Some(second))
        },
    );
    for member in &members {
        let list = listed(member);
        assert_eq!(
            list.iter().map(|s| s.log_position).collect::<Vec<_>>(),
            vec![position, second],
            "member {}",
            member.module.member_id()
        );
        assert_eq!(member.module.snapshot_counts().taken, 2);
        assert_eq!(member.module.snapshot_counts().failed, 0);
    }
    for member in members {
        member.module.close().expect("the member closes");
        member.driver.close().expect("the driver closes");
    }
}

#[test]
fn a_member_that_starts_again_begins_at_its_snapshot_and_takes_no_second_one_of_it() {
    let dir = TestDir::new("again");
    let members_string = members_string();
    let mut members: Vec<Member> = [1, 0, 2]
        .into_iter()
        .map(|id| start_member(dir.path(), &members_string, id))
        .collect();
    run_until_led(&mut members, -1);
    offer_entries(&mut members, 0, 2);
    ask_snapshot(&mut members);
    let position = 4 * FRAME;
    run_until(&mut members, "every member lists its snapshot", |members| {
        members
            .iter()
            .all(|m| m.module.snapshot_position() == Some(position))
    });
    offer_entries(&mut members, 2, 2);
    let end = position + 2 * FRAME;
    run_until_committed(&mut members, "two more entries are committed", end);

    // A follower stops and starts again on the log directory it kept. Its file lists
    // the snapshot from its first cycle on, its counter says so, and it begins its
    // read of its log at the snapshot's position (ADR 0130).
    let stopped = members.pop().expect("a follower");
    let id = stopped.module.member_id();
    let before = listed(&stopped);
    meanwhile(&mut members, || stop(stopped));
    let again = meanwhile(&mut members, || {
        start_member(dir.path(), &members_string, id)
    });
    assert_eq!(again.module.snapshot_position(), Some(position));
    assert_eq!(counter(&again, CLUSTER_SNAPSHOT_POSITION.0), position);
    assert_eq!(again.module.session_log_position(), position);
    assert_eq!(counter(&again, CLUSTER_SNAPSHOT_START_POSITION.0), position);
    assert_eq!(counter(&again, CLUSTER_SNAPSHOTS_PASSED_OVER.0), 0);
    members.push(again);
    // It reads what follows the snapshot and catches up with what the others
    // committed.
    run_until(
        &mut members,
        "the member has read its log again",
        |members| {
            members.iter().all(|m| {
                m.module.election_state() == ElectionState::Closed
                    && m.module.commit_position() >= end
                    && m.module.session_log_position() >= end
            })
        },
    );
    let again = members.last().expect("the member that started again");
    assert_eq!(
        again.module.snapshot_counts(),
        SnapshotCounts::default(),
        "a start takes no snapshot"
    );
    assert_eq!(listed(again), before);
    assert_eq!(again.module.snapshot_position(), Some(position));
    for member in members {
        member.module.close().expect("the member closes");
        member.driver.close().expect("the driver closes");
    }
}

/// What a snapshot costs the thread that runs a member: over 50 snapshots asked one
/// after the other, the longest duty cycle of each member while the snapshot was
/// taken, and how long it was from the ask until all three members listed it. The
/// median, the 90th percentile and the longest are printed. Ignored, since it
/// measures and asserts nothing: `cargo test --release -p cavalry-raft --test snapshot
/// -- --ignored --nocapture a_snapshots_duty_cycles_timed`.
#[test]
#[ignore = "a measurement for ADR 0129, run by hand"]
fn a_snapshots_duty_cycles_timed() {
    let dir = TestDir::new("timed");
    let members_string = members_string();
    let mut members: Vec<Member> = [1, 0, 2]
        .into_iter()
        .map(|id| start_member(dir.path(), &members_string, id))
        .collect();
    run_until_led(&mut members, -1);
    let mut longest_cycles = Vec::new();
    let mut whole = Vec::new();
    // The longest duty cycle of any member with no snapshot in hand, for comparison.
    let mut idle = Duration::ZERO;
    for round in 0..50i64 {
        offer_entries(&mut members, round, 1);
        let quiet = Instant::now() + Duration::from_millis(20);
        while Instant::now() < quiet {
            for member in members.iter_mut() {
                let from = Instant::now();
                member.module.do_work().expect("the member's duty cycle");
                idle = idle.max(from.elapsed());
            }
        }
        let taken = members[0].module.snapshot_counts().taken + 1;
        ask_snapshot(&mut members);
        let asked = Instant::now();
        let mut longest = Duration::ZERO;
        let deadline = asked + Duration::from_secs(20);
        while members
            .iter()
            .any(|m| m.module.snapshot_counts().taken < taken)
        {
            assert!(Instant::now() < deadline, "snapshot {round} was not taken");
            for member in members.iter_mut() {
                let from = Instant::now();
                member.module.do_work().expect("the member's duty cycle");
                longest = longest.max(from.elapsed());
            }
        }
        whole.push(asked.elapsed());
        longest_cycles.push(longest);
    }
    for (what, mut took) in [
        (
            "the longest duty cycle while a snapshot was taken",
            longest_cycles,
        ),
        ("from the ask until three members listed it", whole),
    ] {
        took.sort();
        println!(
            "{what}, 50 snapshots: p50 {:.2} ms, p90 {:.2} ms, max {:.2} ms",
            took[24].as_secs_f64() * 1e3,
            took[44].as_secs_f64() * 1e3,
            took[49].as_secs_f64() * 1e3
        );
    }
    println!(
        "the longest duty cycle with no snapshot in hand: {:.2} ms",
        idle.as_secs_f64() * 1e3
    );
    for member in &members {
        assert_eq!(member.module.snapshot_counts().failed, 0);
    }
    for member in members {
        member.module.close().expect("the member closes");
        member.driver.close().expect("the driver closes");
    }
}

/// How fast a member reads its log back at a start, which is what the snapshot
/// interval bounds: three members commit 64 MiB of entries of the longest payload, a
/// follower stops and starts again on the log it kept, and the time from its start
/// until its consensus module has read the log to its end is printed with the rate.
/// Ignored, since it measures and asserts nothing: `cargo test --release -p
/// cavalry-raft --test snapshot -- --ignored --nocapture
/// a_members_read_of_its_log_at_a_start_timed`.
#[test]
#[ignore = "a measurement for ADR 0129, run by hand"]
fn a_members_read_of_its_log_at_a_start_timed() {
    const PAYLOAD: usize = cavalry_raft::MAX_PAYLOAD_LENGTH;
    const ENTRY: i64 = 32 + 32 + PAYLOAD as i64;
    const ENTRIES: i64 = (64 << 20) / ENTRY;
    let dir = TestDir::new("read-back");
    let members_string = members_string();
    let mut members: Vec<Member> = [1, 0, 2]
        .into_iter()
        .map(|id| start_member(dir.path(), &members_string, id))
        .collect();
    run_until_led(&mut members, -1);
    let payload = [0x5Au8; PAYLOAD];
    let offered = Instant::now();
    for _ in 0..ENTRIES {
        loop {
            match members[0].module.offer(&payload) {
                Ok(()) => break,
                Err(RaftError::Offer(Refused::BackPressured)) => {
                    for member in members.iter_mut() {
                        member.module.do_work().expect("the member's duty cycle");
                    }
                }
                Err(other) => panic!("{other}"),
            }
        }
    }
    // Where the log ends is past the entries' own bytes: an entry that does not fit
    // the end of a term leaves a padding frame there.
    let least = FRAME + ENTRIES * ENTRY;
    let deadline = Instant::now() + Duration::from_secs(300);
    let end = loop {
        let end = members[0].module.append_position();
        if end >= least
            && members.iter().all(|m| {
                m.module.commit_position() == end
                    && m.module.append_position() == end
                    && m.module.session_log_position() == end
            })
        {
            break end;
        }
        assert!(Instant::now() < deadline, "the entries were not committed");
        for member in members.iter_mut() {
            member.module.do_work().expect("the member's duty cycle");
        }
    };
    println!(
        "{ENTRIES} entries of {ENTRY} bytes, {:.1} MiB, committed and read by three members in {:.2} s",
        end as f64 / (1 << 20) as f64,
        offered.elapsed().as_secs_f64()
    );
    let stopped = members.pop().expect("a follower");
    let id = stopped.module.member_id();
    stopped.module.close().expect("the member closes");
    stopped.driver.close().expect("the driver closes");
    let started = Instant::now();
    members.push(start_member(dir.path(), &members_string, id));
    // The driver's start and the member's, with no snapshot to read back.
    let plain_start = started.elapsed();
    // The member reads no further than the commit position it knows, which is 0
    // until its leader tells it one: the read is timed from then.
    let mut bound_known = None;
    let deadline = Instant::now() + Duration::from_secs(300);
    loop {
        let again = &members.last().expect("the member").module;
        if bound_known.is_none() && again.commit_position() >= end {
            bound_known = Some((Instant::now(), again.session_log_position()));
        }
        if again.session_log_position() >= end {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the member did not read its log back"
        );
        for member in members.iter_mut() {
            member.module.do_work().expect("the member's duty cycle");
        }
    }
    let whole = started.elapsed().as_secs_f64();
    let (from, read_before) = bound_known.expect("the commit position was learned");
    let reading = from.elapsed().as_secs_f64();
    let mib = |bytes: i64| bytes as f64 / (1 << 20) as f64;
    println!(
        "a member started again, its driver's start and its own taking {:.1} ms, read {:.1} MiB \
         of its log back {whole:.2} s after its start; it \
         knew the commit position after {:.2} s, having read {:.1} MiB, and read the other \
         {:.1} MiB in {:.3} s: {:.0} MiB/s",
        plain_start.as_secs_f64() * 1e3,
        mib(end),
        whole - reading,
        mib(read_before),
        mib(end - read_before),
        reading,
        mib(end - read_before) / reading
    );

    // The same start from a snapshot of the log's end (ADR 0130): the leader asks
    // for one, the member stops and starts again, and reads what follows it.
    run_until_led(&mut members, -1);
    ask_snapshot(&mut members);
    let deadline = Instant::now() + Duration::from_secs(60);
    while members
        .iter()
        .any(|m| m.module.snapshot_position().is_none_or(|p| p <= end))
    {
        assert!(Instant::now() < deadline, "the snapshot was not taken");
        for member in members.iter_mut() {
            member.module.do_work().expect("the member's duty cycle");
        }
    }
    let position = members[0].module.snapshot_position().expect("a snapshot");
    let follower = members
        .iter()
        .position(|m| m.module.role() != Role::Leader)
        .expect("a follower");
    let stopped = members.remove(follower);
    let id = stopped.module.member_id();
    stopped.module.close().expect("the member closes");
    stopped.driver.close().expect("the driver closes");
    let driver_dir = dir.path().join(format!("member-{id}"));
    let log_dir = dir.path().join(format!("member-{id}-log"));
    let from = Instant::now();
    let driver = Driver::start(&driver_dir, &log_dir).expect("the driver starts");
    let module = ConsensusModule::start(&driver_dir, &log_dir, &members_string, id)
        .expect("the member starts from its snapshot");
    let start = from.elapsed();
    assert_eq!(module.session_log_position(), position);
    members.push(Member {
        driver,
        module,
        dir: driver_dir,
        log_dir,
    });
    let deadline = Instant::now() + Duration::from_secs(60);
    while {
        let again = &members.last().expect("the member").module;
        again.election_state() != ElectionState::Closed || again.commit_position() < position
    } {
        assert!(Instant::now() < deadline, "the member did not join again");
        for member in members.iter_mut() {
            member.module.do_work().expect("the member's duty cycle");
        }
    }
    println!(
        "from a snapshot of position {position}, {:.1} MiB into its log: the driver's start \
         and the member's took {:.1} ms with the snapshot read back, it began its read at \
         the snapshot, and it had joined again {:.2} s after its start began",
        mib(position),
        start.as_secs_f64() * 1e3,
        from.elapsed().as_secs_f64()
    );
    for member in members {
        member.module.close().expect("the member closes");
        member.driver.close().expect("the driver closes");
    }
}

/// A client of the cluster, by hand, on a driver of its own: a subscription on its
/// response channel and a publication to one member's `ingress` endpoint.
struct Client {
    driver: Driver,
    cavalry: Cavalry,
    response_channel: String,
    egress: Subscription,
    ingress: Publication,
    correlation_id: i64,
    /// The session the leader opened for it, once its ok event has arrived.
    session_id: Option<i64>,
}

impl Client {
    fn start(dir: &Path, name: &str, port: u16, ingress: &str, correlation_id: i64) -> Self {
        let driver_dir = dir.join(name);
        let driver = timed(&format!("{name}: its driver's start"), || {
            Driver::start(&driver_dir, dir.join(format!("{name}-log")))
                .expect("the client's driver")
        });
        let mut cavalry = Cavalry::connect_to(&driver_dir).expect("connects");
        let response_channel = format!("cavalry:udp?endpoint=127.0.0.1:{port}");
        let egress = cavalry
            .add_subscription(&response_channel, EGRESS_STREAM_ID)
            .expect("the egress subscription");
        let ingress = cavalry
            .add_exclusive_publication(
                &format!("cavalry:udp?endpoint={ingress}"),
                INGRESS_STREAM_ID,
            )
            .expect("the ingress publication");
        Self {
            driver,
            cavalry,
            response_channel,
            egress,
            ingress,
            correlation_id,
            session_id: None,
        }
    }

    /// Offers the session connect; `false` while the publication does not take it.
    fn connect(&mut self) -> bool {
        let message = Message::SessionConnect {
            correlation_id: self.correlation_id,
            response_stream_id: EGRESS_STREAM_ID,
            version: 0,
            response_channel: self.response_channel.as_bytes(),
            encoded_credentials: &[],
            client_info: b"snapshot.rs",
        };
        let mut bytes = vec![0u8; message.encoded_length()];
        let length = message.write(&mut bytes).expect("encodes");
        self.ingress.offer(&bytes[..length]).is_ok()
    }

    /// Reads the response channel for the ok event of the client's connect.
    fn poll(&mut self) {
        let (correlation, opened) = (self.correlation_id, &mut self.session_id);
        self.egress
            .poll(
                |body, _| {
                    if let Ok(Message::SessionEvent {
                        cluster_session_id,
                        correlation_id,
                        code,
                        ..
                    }) = Message::parse(Stream::Egress, body)
                        && correlation_id == correlation
                        && code.code() == 0
                    {
                        *opened = Some(cluster_session_id);
                    }
                },
                16,
            )
            .expect("the egress polls");
    }

    /// Has the run loop keep this client's session alive, whichever member leads:
    /// a publication to each member's `ingress` endpoint, on `ports`, the ports the
    /// members string was made on.
    fn keep_alive(&mut self, ports: &[u16]) {
        let cluster_session_id = self.session_id.expect("an open session");
        let publications = (0..3i32)
            .map(|member| {
                let port = ports[usize::try_from(member).expect("a member id") * 5];
                let publication = self
                    .cavalry
                    .add_exclusive_publication(
                        &format!("cavalry:udp?endpoint=127.0.0.1:{port}"),
                        INGRESS_STREAM_ID,
                    )
                    .expect("a publication to a member's ingress endpoint");
                (member, publication)
            })
            .collect();
        KEPT_ALIVE.with(|kept| {
            kept.borrow_mut()
                .sessions
                .push((cluster_session_id, publications));
        });
    }

    fn close(self) {
        // The keepalives' publications are this client's, and go before it closes.
        KEPT_ALIVE.with(|kept| {
            kept.borrow_mut()
                .sessions
                .retain(|(id, _)| Some(*id) != self.session_id);
        });
        drop(self.ingress);
        drop(self.egress);
        self.cavalry.close().expect("the client closes");
        self.driver.close().expect("the client's driver closes");
    }
}

#[test]
fn a_snapshot_holds_the_open_sessions_in_the_order_of_their_ids_on_every_member() {
    let dir = TestDir::new("sessions");
    let ports = free_ports(17);
    let members_string = members_string_on(&ports);
    let mut members: Vec<Member> = [1, 0, 2]
        .into_iter()
        .map(|id| start_member(dir.path(), &members_string, id))
        .collect();
    run_until_led(&mut members, -1);
    let leader = usize::try_from(members[0].module.member_id()).expect("a member id");
    let ingress = format!("127.0.0.1:{}", ports[leader * 5]);
    // Two clients, one after the other, so that the first has session 0 and the
    // second session 1.
    let mut clients = Vec::new();
    for (index, correlation_id) in [41i64, 42].into_iter().enumerate() {
        let mut client = meanwhile(&mut members, || {
            Client::start(
                dir.path(),
                &format!("client-{index}"),
                ports[15 + index],
                &ingress,
                correlation_id,
            )
        });
        let mut sent = false;
        run_until(&mut members, "the client's session opens", |_| {
            sent = sent || client.connect();
            client.poll();
            client.session_id.is_some()
        });
        assert_eq!(client.session_id, Some(index as i64));
        // The session stays open for as long as the test runs, as a client's does.
        client.keep_alive(&ports);
        clients.push(client);
    }
    // Every member has read both session open events before the action is asked.
    run_until(
        &mut members,
        "every member holds both sessions",
        |members| {
            let end = members[0].module.append_position();
            members.iter().all(|m| {
                m.module.commit_position() == end && m.module.session_log_position() == end
            })
        },
    );
    let before = members[0].module.append_position();
    ask_snapshot(&mut members);
    let position = before + FRAME;
    run_until(&mut members, "every member lists its snapshot", |members| {
        members
            .iter()
            .all(|m| m.module.snapshot_position() == Some(position))
    });
    let term = members[0].module.leadership_term_id();
    let mut bytes: Vec<Vec<Vec<u8>>> = Vec::new();
    // Each member's recording, played back while the members run.
    let mut replays = std::collections::VecDeque::from(replayed_each(&mut members, |member| {
        listed(member)[0].recording_id
    }));
    for member in &members {
        let id = member.module.member_id();
        let list = listed(member);
        assert_eq!(list.len(), 1, "member {id}: {list:?}");
        let messages = replays.pop_front().expect("a replay for each member");
        let parsed: Vec<Message<'_>> = messages
            .iter()
            .map(|bytes| Message::parse(Stream::Snapshot, bytes).expect("a snapshot's message"))
            .collect();
        assert_eq!(parsed.len(), 5, "member {id}: {parsed:?}");
        assert!(
            matches!(
                parsed[0],
                Message::SnapshotMarker {
                    type_id: SNAPSHOT_TYPE_ID,
                    log_position,
                    leadership_term_id,
                    mark: SnapshotMark::Begin,
                    ..
                } if log_position == position && leadership_term_id == term
            ),
            "member {id}: {:?}",
            parsed[0]
        );
        // The sessions, in the order of their ids, each with its client's channel and
        // the position of its session open event, which is below the snapshot's.
        let mut opened_at = FRAME;
        for (index, client) in clients.iter().enumerate() {
            let Message::ClusterSession {
                cluster_session_id,
                correlation_id,
                opened_log_position,
                time_of_last_activity,
                close_reason,
                response_stream_id,
                response_channel,
            } = parsed[1 + index]
            else {
                panic!("member {id}: {:?}", parsed[1 + index]);
            };
            assert_eq!(
                (
                    cluster_session_id,
                    correlation_id,
                    time_of_last_activity,
                    close_reason,
                    response_stream_id,
                    response_channel
                ),
                (
                    index as i64,
                    client.correlation_id,
                    -1,
                    CLOSE_REASON_NONE,
                    EGRESS_STREAM_ID,
                    client.response_channel.as_bytes()
                ),
                "member {id}"
            );
            assert!(
                opened_log_position >= opened_at && opened_log_position < before,
                "member {id}: session {index} opened at {opened_log_position}"
            );
            opened_at = opened_log_position + 32;
        }
        assert_eq!(
            parsed[3],
            Message::ConsensusModule {
                next_session_id: 2,
                next_service_session_id: 0,
                log_service_session_id: 0,
                pending_message_capacity: 0,
            },
            "member {id}"
        );
        assert!(
            matches!(
                parsed[4],
                Message::SnapshotMarker {
                    mark: SnapshotMark::End,
                    ..
                }
            ),
            "member {id}: {:?}",
            parsed[4]
        );
        bytes.push(messages);
    }
    assert_eq!(bytes[0], bytes[1], "the leader's snapshot and a follower's");
    assert_eq!(
        bytes[0], bytes[2],
        "the leader's snapshot and the other follower's"
    );
    for client in clients {
        client.close();
    }
    for member in members {
        member.module.close().expect("the member closes");
        member.driver.close().expect("the driver closes");
    }
}

/// A connect that the leader accepts after it appended the action, and before its
/// read of its committed log reached the action, is in no member's snapshot
/// (`spec/cluster.md`, "Consensus module, template 105"; ADR 0129). The leader gives
/// the session its id at the connect, and its session open event goes in after the
/// action. Both followers are held meanwhile, so that the action cannot commit, and no
/// member can take the snapshot, before the leader has accepted the connect. Every
/// member's snapshot then holds the one session below the action and the next session
/// id that the log gives there, 1, and the three hold the same bytes. The client is
/// then given session 1, as the leader's own next id said.
#[test]
fn a_connect_the_leader_accepts_before_it_reads_the_action_is_in_no_members_snapshot() {
    let dir = TestDir::new("accepted");
    let ports = free_ports(17);
    let members_string = members_string_on(&ports);
    let mut members: Vec<Member> = [1, 0, 2]
        .into_iter()
        .map(|id| start_member(dir.path(), &members_string, id))
        .collect();
    run_until_led(&mut members, -1);
    let leader = usize::try_from(members[0].module.member_id()).expect("a member id");
    let ingress = format!("127.0.0.1:{}", ports[leader * 5]);

    // The first client's session, 0, opened and read by every member.
    let mut first = meanwhile(&mut members, || {
        Client::start(dir.path(), "client-0", ports[15], &ingress, 41)
    });
    let mut sent = false;
    run_until(&mut members, "the first client's session opens", |_| {
        sent = sent || first.connect();
        first.poll();
        first.session_id.is_some()
    });
    assert_eq!(first.session_id, Some(0));
    // The session stays open for as long as the test runs, as a client's does.
    first.keep_alive(&ports);
    run_until(
        &mut members,
        "every member holds the first session",
        |members| {
            let end = members[0].module.append_position();
            members.iter().all(|m| {
                m.module.commit_position() == end && m.module.session_log_position() == end
            })
        },
    );
    // The second client's publication is connected to the leader's ingress endpoint
    // before the action is asked, so that its connect goes when it is offered.
    let mut second = meanwhile(&mut members, || {
        Client::start(dir.path(), "client-1", ports[16], &ingress, 42)
    });
    run_until(
        &mut members,
        "the second client's publication is connected",
        |_| second.ingress.is_connected(),
    );

    let before = members[0].module.append_position();
    ask_snapshot(&mut members);
    let position = before + FRAME;
    // From here until the leader has accepted the connect, only the leader runs. The
    // followers report no append position, so the commit position stays at `before`
    // and no member's read reaches the action.
    let deadline = Instant::now() + Duration::from_secs(20);
    while !second.connect() {
        assert!(
            Instant::now() < deadline,
            "the second client's connect is not taken"
        );
        std::thread::sleep(Duration::from_micros(200));
    }
    while counter(&members[0], CLUSTER_SESSIONS_ACCEPTED.0) < 2 {
        assert!(
            Instant::now() < deadline,
            "the leader did not accept the second connect: {:?}",
            members[0].module
        );
        if members[0]
            .module
            .do_work()
            .expect("the leader's duty cycle")
            == 0
        {
            std::thread::sleep(Duration::from_micros(200));
        }
    }
    assert!(
        members[0].module.commit_position() < position,
        "the action was committed before the connect was accepted"
    );
    assert_eq!(
        members[0].module.snapshot_counts().begun,
        0,
        "the leader has not read the action"
    );

    run_until(&mut members, "every member lists its snapshot", |members| {
        members
            .iter()
            .all(|m| m.module.snapshot_position() == Some(position))
    });
    let term = members[0].module.leadership_term_id();
    let marker = |mark| Message::SnapshotMarker {
        type_id: SNAPSHOT_TYPE_ID,
        log_position: position,
        leadership_term_id: term,
        index: 0,
        mark,
        time_unit: cavalry_proto::cluster::TimeUnit::Millis,
        app_version: 0,
    };
    let mut bytes: Vec<Vec<Vec<u8>>> = Vec::new();
    // Each member's recording, played back while the members run.
    let mut replays = std::collections::VecDeque::from(replayed_each(&mut members, |member| {
        listed(member)[0].recording_id
    }));
    for member in &members {
        let id = member.module.member_id();
        let list = listed(member);
        assert_eq!(list.len(), 1, "member {id}: {list:?}");
        let messages = replays.pop_front().expect("a replay for each member");
        let parsed: Vec<Message<'_>> = messages
            .iter()
            .map(|bytes| Message::parse(Stream::Snapshot, bytes).expect("a snapshot's message"))
            .collect();
        let Some(&Message::ClusterSession {
            opened_log_position,
            ..
        }) = parsed.get(1)
        else {
            panic!("member {id}: {parsed:?}");
        };
        // The one session below the action, and the next session id that the log
        // gives at the snapshot's position: the second connect's id is not in it.
        assert_eq!(
            parsed,
            vec![
                marker(SnapshotMark::Begin),
                Message::ClusterSession {
                    cluster_session_id: 0,
                    correlation_id: first.correlation_id,
                    opened_log_position,
                    time_of_last_activity: -1,
                    close_reason: CLOSE_REASON_NONE,
                    response_stream_id: EGRESS_STREAM_ID,
                    response_channel: first.response_channel.as_bytes(),
                },
                Message::ConsensusModule {
                    next_session_id: 1,
                    next_service_session_id: 0,
                    log_service_session_id: 0,
                    pending_message_capacity: 0,
                },
                marker(SnapshotMark::End),
            ],
            "member {id}"
        );
        assert!(opened_log_position < before, "member {id}");
        bytes.push(messages);
    }
    assert_eq!(bytes[0], bytes[1], "the leader's snapshot and a follower's");
    assert_eq!(
        bytes[0], bytes[2],
        "the leader's snapshot and the other follower's"
    );

    // The connect the leader accepted is opened by its session open event, after the
    // action, with the id the leader gave it.
    run_until(&mut members, "the second client's session opens", |_| {
        second.poll();
        second.session_id.is_some()
    });
    assert_eq!(second.session_id, Some(1));
    for client in [first, second] {
        client.close();
    }
    for member in members {
        member.module.close().expect("the member closes");
        member.driver.close().expect("the driver closes");
    }
}

/// Three members led, with two clients' sessions open, 0 and 1, and every member
/// holding both: the members with the leader first, the clients, and where the log
/// then ends. The run loop keeps the two sessions alive from here on
/// ([`keep_sessions_alive`]).
fn led_with_two_sessions(dir: &TestDir) -> (String, Vec<Member>, Vec<Client>, i64) {
    let ports = free_ports(17);
    let members_string = members_string_on(&ports);
    let mut members: Vec<Member> = [1, 0, 2]
        .into_iter()
        .map(|id| start_member(dir.path(), &members_string, id))
        .collect();
    run_until_led(&mut members, -1);
    let leader = usize::try_from(members[0].module.member_id()).expect("a member id");
    let ingress = format!("127.0.0.1:{}", ports[leader * 5]);
    let mut clients = Vec::new();
    for (index, correlation_id) in [41i64, 42].into_iter().enumerate() {
        let mut client = meanwhile(&mut members, || {
            Client::start(
                dir.path(),
                &format!("client-{index}"),
                ports[15 + index],
                &ingress,
                correlation_id,
            )
        });
        let mut sent = false;
        run_until(&mut members, "the client's session opens", |_| {
            sent = sent || client.connect();
            client.poll();
            client.session_id.is_some()
        });
        assert_eq!(client.session_id, Some(index as i64));
        // The session stays open for as long as the test runs, as a client's does.
        client.keep_alive(&ports);
        clients.push(client);
    }
    // A client reads its ok event once its session open event is committed, so the
    // leader's commit position is past both.
    let committed = members[0].module.commit_position();
    let end = run_until_settled(&mut members, committed);
    (members_string, members, clients, end)
}

/// Runs the members until every election has closed and each has committed, and
/// read for its sessions, the log to one end that is `at_least` or more: what the
/// caller knows was appended, which a member's append position does not show
/// until its recording of it is synced. Returns that end.
fn run_until_settled(members: &mut [Member], at_least: i64) -> i64 {
    let mut end = 0;
    run_until(members, "every member has read the whole log", |members| {
        end = members
            .iter()
            .map(|m| m.module.append_position())
            .max()
            .unwrap_or(0);
        end >= at_least
            && members.iter().all(|m| {
                m.module.election_state() == ElectionState::Closed
                    && m.module.append_position() == end
                    && m.module.commit_position() == end
                    && m.module.session_log_position() == end
            })
    });
    end
}

/// The leader asks for a snapshot, and every member lists one at the same position,
/// past every snapshot listed before: that position.
fn snapshot_everywhere(members: &mut [Member]) -> i64 {
    let before = members
        .iter()
        .filter_map(|m| m.module.snapshot_position())
        .max()
        .unwrap_or(-1);
    ask_snapshot(members);
    let mut position = 0;
    run_until(members, "every member lists the snapshot", |members| {
        position = members[0].module.snapshot_position().unwrap_or(-1);
        position > before
            && members
                .iter()
                .all(|m| m.module.snapshot_position() == Some(position))
    });
    position
}

/// Stops `member`, its module and then its driver, and gives back its id.
fn stop(member: Member) -> i32 {
    let id = member.module.member_id();
    timed(&format!("member {id}: module close"), || {
        member.module.close().expect("the member closes");
    });
    timed(&format!("member {id}: driver close"), || {
        member.driver.close().expect("the driver closes");
    });
    id
}

/// Purges `recording_id` from the log directory of stopped member `id`, as an
/// operator does: a driver on the member's directories, and the client's command.
fn purge(dir: &Path, id: i32, recording_id: i64) {
    purge_each(dir, id, &[recording_id]);
}

/// [`purge`] for several recordings, with one driver.
fn purge_each(dir: &Path, id: i32, recording_ids: &[i64]) {
    timed(&format!("member {id}: a purge of its recordings"), || {
        let driver_dir = dir.join(format!("member-{id}"));
        let log_dir = dir.join(format!("member-{id}-log"));
        let driver =
            Driver::start(&driver_dir, &log_dir).expect("a driver on the member's directories");
        let mut client = Cavalry::connect_to(&driver_dir).expect("a client of it");
        for recording_id in recording_ids {
            client
                .purge_recording(*recording_id)
                .unwrap_or_else(|error| panic!("recording {recording_id} is purged: {error}"));
        }
        client.close().expect("the client closes");
        driver.close().expect("the driver closes");
    });
}

/// Where the recordings a member's record names divide at `position`: the index of
/// the last one that begins at or below it. The member's log is needed from that
/// recording, and an operator may purge every one before it, which is the rule a
/// member's start holds a purge to (`spec/cluster.md`, "A member's start"). With
/// the one election a test makes below the position that is the second of two
/// recordings.
fn needed_from(recordings: &[(i64, i64)], position: i64) -> usize {
    let kept = recordings
        .iter()
        .rposition(|(_, start)| *start <= position)
        .expect("a recording that begins at or below the position");
    assert!(
        kept >= 1 && recordings[kept].1 > 0,
        "no recording to purge below {position}: {recordings:?}"
    );
    kept
}

/// Truncates stopped recording `recording_id` to `position` in the log directory of
/// stopped member `id`, as a client's command does on a driver over the member's
/// directories.
fn truncate(dir: &Path, id: i32, recording_id: i64, position: i64) {
    timed(&format!("member {id}: a truncation of a recording"), || {
        let driver_dir = dir.join(format!("member-{id}"));
        let log_dir = dir.join(format!("member-{id}-log"));
        let driver =
            Driver::start(&driver_dir, &log_dir).expect("a driver on the member's directories");
        let mut client = Cavalry::connect_to(&driver_dir).expect("a client of it");
        client
            .truncate_recording(recording_id, position)
            .unwrap_or_else(|error| {
                panic!("recording {recording_id} is cut to {position}: {error}")
            });
        client.close().expect("the client closes");
        driver.close().expect("the driver closes");
    });
}

/// The bytes of the newest snapshot each member lists, which must be of `position`.
fn newest_bytes(members: &mut [Member], position: i64) -> Vec<Vec<Vec<u8>>> {
    for member in members.iter() {
        let list = listed(member);
        let newest = list.last().expect("a listed snapshot");
        assert_eq!(
            newest.log_position,
            position,
            "member {}",
            member.module.member_id()
        );
    }
    replayed_each(members, |member| {
        listed(member)
            .last()
            .expect("a listed snapshot")
            .recording_id
    })
}

#[test]
fn a_member_that_starts_again_takes_its_sessions_from_its_snapshot_and_reads_what_follows() {
    let dir = TestDir::new("restored");
    let (members_string, mut members, clients, _) = led_with_two_sessions(&dir);
    let position = snapshot_everywhere(&mut members);
    offer_entries(&mut members, 0, 3);
    let end = run_until_settled(&mut members, position + 3 * FRAME);
    assert_eq!(end, position + 3 * FRAME);

    // A follower stops and starts again. Before its first duty cycle it has read
    // nothing of its log: it begins at the snapshot's position, and says so.
    let follower = members.pop().expect("a follower");
    let id = meanwhile(&mut members, || stop(follower));
    let again = meanwhile(&mut members, || {
        start_member(dir.path(), &members_string, id)
    });
    assert_eq!(again.module.session_log_position(), position);
    assert_eq!(counter(&again, CLUSTER_SNAPSHOT_START_POSITION.0), position);
    assert_eq!(counter(&again, CLUSTER_SNAPSHOTS_PASSED_OVER.0), 0);
    members.push(again);
    // Its table holds the two sessions from its first cycle, before it has a leader
    // or a commit position, which no read of its log could have given it yet.
    members[2]
        .module
        .do_work()
        .expect("the member's duty cycle");
    assert_eq!(counter(&members[2], CLUSTER_SESSIONS.0), 2);
    assert_eq!(members[2].module.session_log_position(), position);
    // It reads the three entries that follow and no more.
    assert_eq!(run_until_settled(&mut members, end), end);
    assert_eq!(counter(&members[2], CLUSTER_SESSIONS.0), 2);
    // The two it took from the snapshot count as opened, so that the sessions it
    // holds are the ones it opened less the ones it closed.
    assert_eq!(counter(&members[2], CLUSTER_SESSIONS_OPENED.0), 2);
    assert_eq!(counter(&members[2], CLUSTER_SESSIONS_CLOSED.0), 0);

    // What it holds is what the others hold: the next snapshot's bytes are the same
    // on the three, the two sessions and the next session id among them.
    run_until_led(&mut members, -1);
    let second = snapshot_everywhere(&mut members);
    let bytes = newest_bytes(&mut members, second);
    assert_eq!(
        bytes[0].len(),
        5,
        "two markers, two sessions and the module's state"
    );
    assert_eq!(bytes[0], bytes[1]);
    assert_eq!(bytes[0], bytes[2]);
    for client in clients {
        client.close();
    }
    for member in members {
        stop(member);
    }
}

#[test]
fn a_member_whose_log_was_purged_below_its_snapshot_starts_from_the_snapshot() {
    let dir = TestDir::new("purged");
    let (members_string, mut members, clients, end) = led_with_two_sessions(&dir);
    offer_entries(&mut members, 0, 2);
    let end = run_until_settled(&mut members, end + 2 * FRAME);
    // The leader stops: the two others elect one of them, and the first term's log
    // is from then on a recording that each of them has stopped.
    let first_term = members[0].module.leadership_term_id();
    let leader = members.remove(0);
    let old_leader = meanwhile(&mut members, || stop(leader));
    run_until_led(&mut members, first_term);
    let started = meanwhile(&mut members, || {
        start_member(dir.path(), &members_string, old_leader)
    });
    members.push(started);
    // The next term's first entry is past where the first term's log ended.
    run_until_settled(&mut members, end + FRAME);
    run_until_led(&mut members, first_term);
    offer_entries(&mut members, 2, 2);
    let position = snapshot_everywhere(&mut members);
    offer_entries(&mut members, 4, 2);
    let end = run_until_settled(&mut members, position + 2 * FRAME);

    // The follower that followed in both terms. Its log is in more than one
    // recording, and the first ends below the snapshot.
    let follower = members
        .iter()
        .position(|m| m.module.role() != Role::Leader && m.module.member_id() != old_leader)
        .expect("a follower of both terms");
    let stopped = members.remove(follower);
    let log_dir = stopped.log_dir.clone();
    let id = meanwhile(&mut members, || stop(stopped));
    let record = RecordingsFile::open_kept(&log_dir, id).expect("the member's record of its log");
    let recordings: Vec<(i64, i64)> = record
        .entries()
        .iter()
        .map(|entry| (entry.recording_id, entry.start_position))
        .collect();
    drop(record);
    assert_eq!(recordings[0].1, 0, "{recordings:?}");
    let kept = needed_from(&recordings, position);
    let (kept_id, kept_start) = recordings[kept];
    // An operator purges the recordings before it, which the snapshot covers.
    let purged: Vec<i64> = recordings[..kept].iter().map(|(id, _)| *id).collect();
    meanwhile(&mut members, || purge_each(dir.path(), id, &purged));

    // The member starts: from its snapshot, on the log that is left.
    let again = meanwhile(&mut members, || {
        start_member(dir.path(), &members_string, id)
    });
    assert_eq!(again.module.session_log_position(), position);
    assert_eq!(counter(&again, CLUSTER_SNAPSHOT_START_POSITION.0), position);
    // A purge below the snapshot is no truncation that the member's stop cut short.
    assert_eq!(counter(&again, CLUSTER_TRUNCATIONS_FINISHED.0), 0);
    let record = RecordingsFile::open_kept(&log_dir, id).expect("the member's record of its log");
    assert_eq!(
        record.floor_position(),
        kept_start,
        "the floor is where the log now begins"
    );
    assert_eq!(
        record.entries().first().map(|entry| entry.recording_id),
        Some(kept_id),
        "the purged recording has left the record"
    );
    drop(record);
    members.push(again);
    // Nothing committed is lost: it reads on to the end, holds the two sessions, and
    // the next snapshot's bytes are the same on the three.
    assert_eq!(run_until_settled(&mut members, end), end);
    assert_eq!(counter(&members[2], CLUSTER_SESSIONS.0), 2);
    run_until_led(&mut members, first_term);
    let second = snapshot_everywhere(&mut members);
    let bytes = newest_bytes(&mut members, second);
    assert_eq!(
        bytes[0].len(),
        5,
        "two markers, two sessions and the module's state"
    );
    assert_eq!(bytes[0], bytes[1]);
    assert_eq!(bytes[0], bytes[2]);

    // The same member with the recordings of both its snapshots purged as well. It
    // has no snapshot it can read and its log begins above 0, so it does not
    // start, and the refusal says how many snapshots the start passed over.
    let member = members
        .pop()
        .expect("the member that started from its snapshot");
    let snapshots: Vec<i64> = listed(&member)
        .iter()
        .map(|entry| entry.recording_id)
        .collect();
    assert_eq!(snapshots.len(), 2, "{snapshots:?}");
    let id = meanwhile(&mut members, || stop(member));
    for recording_id in snapshots {
        meanwhile(&mut members, || purge(dir.path(), id, recording_id));
    }
    let driver_dir = dir.path().join(format!("member-{id}"));
    let driver = Driver::start(&driver_dir, &log_dir).expect("the driver starts");
    let refused = ConsensusModule::start(&driver_dir, &log_dir, &members_string, id)
        .expect_err("a log that begins above 0, and no snapshot to read");
    assert!(
        matches!(
            &refused,
            RaftError::LogForgotten {
                floor_position,
                from_position: 0,
                snapshots_passed_over: 2,
                ..
            } if *floor_position == kept_start
        ),
        "{refused}"
    );
    let text = refused.to_string();
    assert!(text.contains("passed over: 2."), "{text}");
    driver.close().expect("the driver closes");
    for client in clients {
        client.close();
    }
    for member in members {
        stop(member);
    }
}

/// A member whose log ends below the position of its newest snapshot does not start
/// (`spec/cluster.md`, "A member's start"; ADR 0130): it read its log to that
/// position as committed, so a log that ends below it is shorter than the one the
/// member reported. First the end of the log is cut, the catalog holding every
/// recording the member's record names; then the record's last recording is purged.
/// Each start is refused, and changes nothing in the member's log directory.
#[test]
fn a_member_whose_log_ends_below_its_snapshot_does_not_start() {
    let dir = TestDir::new("ends-below");
    let (members_string, mut members, clients, _) = led_with_two_sessions(&dir);
    offer_entries(&mut members, 0, 2);
    let position = snapshot_everywhere(&mut members);
    offer_entries(&mut members, 2, 2);
    run_until_settled(&mut members, position + 2 * FRAME);

    let follower = members
        .iter()
        .position(|m| m.module.role() != Role::Leader)
        .expect("a follower");
    let stopped = members.remove(follower);
    let log_dir = stopped.log_dir.clone();
    let id = meanwhile(&mut members, || stop(stopped));
    let recordings = |log_dir: &Path| -> (i64, Vec<(i64, i64)>) {
        let record =
            RecordingsFile::open_kept(log_dir, id).expect("the member's record of its log");
        (
            record.floor_position(),
            record
                .entries()
                .iter()
                .map(|entry| (entry.recording_id, entry.start_position))
                .collect(),
        )
    };
    let snapshots = |log_dir: &Path| -> Vec<(i64, i64)> {
        SnapshotsFile::open(log_dir, id)
            .expect("the member's snapshots file")
            .entries()
            .iter()
            .map(|entry| (entry.recording_id, entry.log_position))
            .collect()
    };
    let (floor, record) = recordings(&log_dir);
    let listed_snapshots = snapshots(&log_dir);
    let (last_id, last_start) = *record.last().expect("a recording of the member's log");
    // The frame of the cluster action that asked for the snapshot begins here.
    let cut = position - FRAME;
    assert!(
        last_start <= cut,
        "the last recording begins at {last_start}, the cut is at {cut}: {record:?}"
    );
    let driver_dir = dir.path().join(format!("member-{id}"));
    let refused_start = || {
        timed(&format!("member {id}: a start that is refused"), || {
            let driver = Driver::start(&driver_dir, &log_dir).expect("the driver starts");
            let refused = ConsensusModule::start(&driver_dir, &log_dir, &members_string, id)
                .expect_err("a log that ends below the member's snapshot");
            driver.close().expect("the driver closes");
            refused
        })
    };

    // The end of the member's log is cut to one entry below the snapshot's position.
    meanwhile(&mut members, || truncate(dir.path(), id, last_id, cut));
    let refused = meanwhile(&mut members, refused_start);
    assert!(
        matches!(
            &refused,
            RaftError::LogShort { end_position, from_position }
                if *end_position == cut && *from_position == position
        ),
        "{refused}"
    );
    assert_eq!(refused.reason(), "log-short");
    assert_eq!(recordings(&log_dir), (floor, record.clone()));
    assert_eq!(snapshots(&log_dir), listed_snapshots);

    // The record's last recording is purged: one recording too many.
    meanwhile(&mut members, || purge(dir.path(), id, last_id));
    let refused = meanwhile(&mut members, refused_start);
    assert!(
        matches!(
            &refused,
            RaftError::ListedRecording {
                recording_id,
                found: ListedFound::EndsBelowSnapshot { end_position, from_position },
                ..
            } if *recording_id == last_id && *end_position < position && *from_position == position
        ),
        "{refused}"
    );
    assert_eq!(refused.reason(), "listed-recording");
    assert_eq!(recordings(&log_dir), (floor, record));
    assert_eq!(snapshots(&log_dir), listed_snapshots);

    for client in clients {
        client.close();
    }
    for member in members {
        stop(member);
    }
}

#[test]
fn a_start_passes_over_a_snapshot_whose_recording_is_gone_for_the_one_before_it() {
    let dir = TestDir::new("passed-over");
    let (members_string, mut members, clients, _) = led_with_two_sessions(&dir);
    let first = snapshot_everywhere(&mut members);
    offer_entries(&mut members, 0, 2);
    let second = snapshot_everywhere(&mut members);
    offer_entries(&mut members, 2, 2);
    let end = run_until_settled(&mut members, second + 2 * FRAME);
    assert_eq!(
        second,
        first + 3 * FRAME,
        "two entries and the second action"
    );

    // A follower stops, and the recording of its newest snapshot is purged.
    let stopped = members.pop().expect("a follower");
    let list = listed(&stopped);
    assert_eq!(
        list.iter().map(|s| s.log_position).collect::<Vec<_>>(),
        vec![first, second]
    );
    let id = meanwhile(&mut members, || stop(stopped));
    meanwhile(&mut members, || purge(dir.path(), id, list[1].recording_id));

    // It starts from the snapshot before, says that it passed one over, and reads
    // the log from there: the second action among its entries, for which it takes
    // no snapshot, since its list still names one at that position.
    let again = meanwhile(&mut members, || {
        start_member(dir.path(), &members_string, id)
    });
    assert_eq!(again.module.session_log_position(), first);
    assert_eq!(counter(&again, CLUSTER_SNAPSHOT_START_POSITION.0), first);
    assert_eq!(counter(&again, CLUSTER_SNAPSHOTS_PASSED_OVER.0), 1);
    assert_eq!(again.module.snapshot_position(), Some(second));
    members.push(again);
    assert_eq!(run_until_settled(&mut members, end), end);
    assert_eq!(counter(&members[2], CLUSTER_SESSIONS.0), 2);
    assert_eq!(
        members[2].module.snapshot_counts(),
        SnapshotCounts::default()
    );

    // With every snapshot's recording gone it starts from position 0, on a log it
    // holds whole, and reads all of it.
    let stopped = members.pop().expect("the member");
    let id = meanwhile(&mut members, || stop(stopped));
    meanwhile(&mut members, || purge(dir.path(), id, list[0].recording_id));
    let again = meanwhile(&mut members, || {
        start_member(dir.path(), &members_string, id)
    });
    assert_eq!(again.module.session_log_position(), 0);
    assert_eq!(counter(&again, CLUSTER_SNAPSHOT_START_POSITION.0), -1);
    assert_eq!(counter(&again, CLUSTER_SNAPSHOTS_PASSED_OVER.0), 2);
    members.push(again);
    assert_eq!(run_until_settled(&mut members, end), end);
    assert_eq!(counter(&members[2], CLUSTER_SESSIONS.0), 2);
    for client in clients {
        client.close();
    }
    for member in members {
        stop(member);
    }
}

/// The service beside one member, as a program holds it: its client, the service,
/// and the program's state, which is every session message's payload in the order of
/// the log.
struct Service {
    /// The member the service is beside.
    id: i32,
    client: Cavalry,
    service: ClusterService,
    state: Vec<u8>,
    /// Whether the program gives its state when a snapshot is due.
    gives: bool,
    /// What each call of `snapshot` answered, with the position it was for.
    taken: Vec<(i64, bool)>,
    /// Each timer the program was handed: the position of its event, and its id.
    handed: Vec<(i64, i64)>,
}

impl Service {
    fn beside(member: &Member) -> Self {
        Self::at(&member.dir, member.module.member_id())
    }

    /// A service beside the member `id`, whose driver's directory is `dir`.
    fn at(dir: &Path, id: i32) -> Self {
        let client = Cavalry::connect_to(dir).expect("the service's client");
        let service = client.cluster_service();
        Self {
            id,
            client,
            service,
            state: Vec::new(),
            gives: true,
            taken: Vec::new(),
            handed: Vec::new(),
        }
    }

    /// A service beside `member` whose program asks it to start from a snapshot: the
    /// service, with the state its program was given back, and what its start found.
    /// The member must lead or follow, so that it names a recording of its log. The
    /// start plays the snapshot's recording back, which waits on the driver: the
    /// members run meanwhile.
    fn restored_beside(members: &mut [Member], beside: usize) -> (Self, Restored) {
        let (dir, id) = (
            members[beside].dir.clone(),
            members[beside].module.member_id(),
        );
        meanwhile(members, || {
            let mut service = Self::at(&dir, id);
            let restored = timed(&format!("member {id}: a service's start"), || {
                service
                    .service
                    .restore(&mut service.client)
                    .expect("the service's start")
            });
            if let Restored::State(state) = &restored {
                service.state.clone_from(state);
            }
            (service, restored)
        })
    }

    /// One poll: the program adds each session message's payload to its state.
    fn poll(&mut self) -> usize {
        let state = &mut self.state;
        let handed = &mut self.handed;
        self.service
            .poll(
                &mut self.client,
                |committed, replies| {
                    match committed.entry {
                        // A message that schedules a timer, `T`, an id and how long
                        // after the entry's time it is due; one that cancels one,
                        // `C` and an id; and any other, which is the program's state.
                        Entry::SessionMessage { payload, .. }
                            if payload.len() == 17 && payload[0] == b'T' =>
                        {
                            let number = |at: usize| {
                                i64::from_le_bytes(payload[at..at + 8].try_into().unwrap())
                            };
                            replies
                                .schedule_timer(number(1), committed.timestamp + number(9))
                                .expect("room for a timer");
                        }
                        Entry::SessionMessage { payload, .. }
                            if payload.len() == 9 && payload[0] == b'C' =>
                        {
                            replies.cancel_timer(i64::from_le_bytes(
                                payload[1..9].try_into().unwrap(),
                            ));
                        }
                        Entry::SessionMessage { payload, .. } => state.extend_from_slice(payload),
                        // A timer handed over is part of the state, as what a program
                        // does at one is.
                        Entry::Timer { correlation_id } => {
                            handed.push((committed.position, correlation_id));
                            state.extend_from_slice(&correlation_id.to_le_bytes());
                        }
                        _ => {}
                    }
                    Action::Continue
                },
                16,
            )
            .expect("the service polls")
    }

    /// The program gives the service its state for the snapshot that is due at
    /// `position`.
    fn give(&mut self, position: i64) {
        let taken = timed(&format!("member {}: a service's snapshot", self.id), || {
            self.service
                .snapshot(&mut self.client, &self.state)
                .expect("the service's snapshot")
        });
        self.taken.push((position, taken));
    }

    fn close(mut self) {
        self.service
            .close(&mut self.client)
            .expect("the service closes");
        self.client.close().expect("the service's client closes");
    }
}

/// Drives every member's duty cycle and every service's poll until `done`, or fails
/// with `what`. A service whose poll ended at a snapshot polls no more until its
/// program has given its state, so the snapshot stays due. A program that gives its
/// state does so once every member has listed its own snapshot of that position.
/// The members and the services' polls share this one thread, where each has a
/// thread of its own outside a test. A service's `snapshot` waits for the driver, so
/// it is given on a thread of its own while the members run ([`meanwhile`]): a
/// member whose duty cycle waited behind three of them would give its own snapshot
/// up at the snapshot timeout, and behind enough of them would hold an election.
fn run_with_services(
    members: &mut [Member],
    services: &mut [Service],
    what: &str,
    mut done: impl FnMut(&[Member], &[Service]) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !done(members, services) {
        assert!(
            Instant::now() < deadline,
            "{what}: {:?}; {}",
            members
                .iter()
                .map(|m| format!("{:?} {:?}", m.module, m.module.service_snapshot_counts()))
                .collect::<Vec<_>>(),
            describe(members)
        );
        round();
        let mut work = 0;
        for member in members.iter_mut() {
            work += member.module.do_work().expect("the member's duty cycle");
        }
        // The sessions' keepalives, as in `run_until`: a test's sessions are open
        // while its services read and take their snapshots too.
        keep_sessions_alive(members);
        for service in services.iter_mut() {
            match service.service.snapshot_due() {
                None => work += service.poll(),
                Some(position) if service.gives => {
                    let listed = members.iter().all(|member| {
                        member
                            .module
                            .snapshot_position()
                            .is_some_and(|own| own >= position)
                    });
                    if listed {
                        // A service's snapshot waits on its driver's recorder: the
                        // members run meanwhile, as beside a service of its own
                        // process.
                        meanwhile(members, || service.give(position));
                        work += 1;
                    }
                }
                // The test gives this service its state by hand.
                Some(_) => {}
            }
        }
        if work == 0 {
            std::thread::sleep(Duration::from_micros(200));
        }
    }
}

/// What a snapshot marker holds: its type id, log position, term, index and mark.
fn marker(bytes: &[u8]) -> (i64, i64, i64, i32, SnapshotMark) {
    match Message::parse(Stream::Snapshot, bytes).expect("a message of a snapshot") {
        Message::SnapshotMarker {
            type_id,
            log_position,
            leadership_term_id,
            index,
            mark,
            ..
        } => (type_id, log_position, leadership_term_id, index, mark),
        other => panic!("not a marker: {other:?}"),
    }
}

#[test]
fn a_service_takes_the_snapshot_its_leader_asks_for_and_its_member_lists_it() {
    let dir = TestDir::new("service");
    let (_, mut members, clients, end) = led_with_two_sessions(&dir);
    let mut services: Vec<Service> = members.iter().map(Service::beside).collect();
    // 131 entries of eight bytes: a state that one message of a snapshot does not
    // carry, 1,048 bytes.
    offer_entries(&mut members, 0, 131);
    run_until_settled(&mut members, end + 131 * FRAME);
    let state: Vec<u8> = (0..131i64).flat_map(i64::to_le_bytes).collect();

    ask_snapshot(&mut members);
    let mut position = -1;
    run_with_services(
        &mut members,
        &mut services,
        "every member lists its own snapshot and its service's",
        |members, _| {
            position = members[0].module.service_snapshot_position().unwrap_or(-1);
            position > 0
                && members.iter().all(|m| {
                    m.module.service_snapshot_position() == Some(position)
                        && m.module.snapshot_position() == Some(position)
                })
        },
    );

    // Every service gave the same state once, at the position its member's own
    // snapshot is of, and every member lists both with the same term, base and time.
    let mut recordings = Vec::new();
    // Each member's recording, played back while the members run.
    let mut replays = std::collections::VecDeque::from(replayed_each(&mut members, |member| {
        listed(member)
            .iter()
            .find(|entry| entry.service_id == 0)
            .expect("the service's snapshot")
            .recording_id
    }));
    for (member, service) in members.iter().zip(&services) {
        assert_eq!(service.state, state);
        assert_eq!(service.taken, [(position, true)]);
        assert_eq!(service.service.snapshot_due(), None);
        let list = listed(member);
        assert_eq!(list.len(), 2, "{list:?}");
        let own = list
            .iter()
            .find(|entry| entry.service_id == CONSENSUS_MODULE)
            .expect("the consensus module's snapshot");
        let services = list
            .iter()
            .find(|entry| entry.service_id == 0)
            .expect("the service's snapshot");
        assert_eq!(services.log_position, position);
        assert_eq!(
            (
                services.leadership_term_id,
                services.term_base_log_position,
                services.timestamp
            ),
            (
                own.leadership_term_id,
                own.term_base_log_position,
                own.timestamp
            )
        );
        assert_ne!(services.recording_id, own.recording_id);
        assert_eq!(
            counter(member, CLUSTER_SERVICE_SNAPSHOT_POSITION.0),
            position
        );
        assert_eq!(counter(member, CLUSTER_SERVICE_SNAPSHOTS_LISTED.0), 1);
        assert_eq!(counter(member, CLUSTER_SERVICE_ACKS_REJECTED.0), 0);
        assert_eq!(counter(member, CLUSTER_SERVICE_SNAPSHOTS_TAKEN.0), 1);
        assert_eq!(counter(member, CLUSTER_SERVICE_SNAPSHOTS_FAILED.0), 0);
        assert_eq!(
            member.module.service_snapshot_counts(),
            ServiceSnapshotCounts {
                listed: 1,
                rejected: 0
            }
        );
        recordings.push(replays.pop_front().expect("a replay for each member"));
    }

    // What a service wrote: a marker, the two sessions by their ids, a marker, the
    // state in two messages, and a marker.
    let term = members[0].module.leadership_term_id();
    let messages = &recordings[0];
    assert_eq!(messages.len(), 7, "{messages:02x?}");
    assert_eq!(
        marker(&messages[0]),
        (
            SERVICE_SNAPSHOT_TYPE_ID,
            position,
            term,
            0,
            SnapshotMark::Begin
        )
    );
    for (index, client) in clients.iter().enumerate() {
        match Message::parse(Stream::Snapshot, &messages[1 + index]).expect("a client session") {
            Message::ClientSession {
                cluster_session_id,
                response_stream_id,
                response_channel,
                encoded_principal,
            } => {
                assert_eq!(cluster_session_id, index as i64);
                assert_eq!(response_stream_id, EGRESS_STREAM_ID);
                assert_eq!(response_channel, client.response_channel.as_bytes());
                assert!(encoded_principal.is_empty());
            }
            other => panic!("not a client session: {other:?}"),
        }
    }
    assert_eq!(
        marker(&messages[3]),
        (
            SERVICE_SNAPSHOT_TYPE_ID,
            position,
            term,
            1,
            SnapshotMark::Section
        )
    );
    assert_eq!(messages[4].len(), 1024);
    assert_eq!(
        [messages[4].as_slice(), messages[5].as_slice()].concat(),
        state
    );
    assert_eq!(
        marker(&messages[6]),
        (
            SERVICE_SNAPSHOT_TYPE_ID,
            position,
            term,
            0,
            SnapshotMark::End
        )
    );
    // The three services wrote the same bytes.
    assert_eq!(recordings[0], recordings[1]);
    assert_eq!(recordings[0], recordings[2]);

    // No snapshot is due now: a program that gives a state is refused.
    let service = &mut services[0];
    assert!(matches!(
        service.service.snapshot(&mut service.client, b"late"),
        Err(ClusterError::NoSnapshotDue)
    ));

    // A service that starts again reads its member's log from the start and meets
    // the action again. Its member lists a snapshot of that position, so it writes
    // none, and the member is sent no ack.
    services.pop().expect("a service").close();
    let mut again = Service::beside(&members[2]);
    run_with_services(
        &mut members,
        std::slice::from_mut(&mut again),
        "the service that started again has read the log and been asked",
        |_, services| !services[0].taken.is_empty(),
    );
    assert_eq!(again.service.position(), position);
    assert_eq!(again.state, state);
    assert_eq!(again.taken, [(position, false)]);
    assert_eq!(listed(&members[2]).len(), 2);
    assert_eq!(
        members[2].module.service_snapshot_counts(),
        ServiceSnapshotCounts {
            listed: 1,
            rejected: 0
        }
    );
    again.close();
    for service in services {
        service.close();
    }
    for client in clients {
        client.close();
    }
    for member in members {
        stop(member);
    }
}

#[test]
fn a_services_poll_ends_at_the_action_and_a_program_that_gives_no_state_reads_on() {
    let dir = TestDir::new("poll-ends");
    let members_string = members_string();
    let mut members: Vec<Member> = [1, 0, 2]
        .into_iter()
        .map(|id| start_member(dir.path(), &members_string, id))
        .collect();
    run_until_led(&mut members, -1);
    // Before any service polls, the log holds three entries, the action and three
    // entries after it, all committed.
    offer_entries(&mut members, 0, 3);
    let position = snapshot_everywhere(&mut members);
    offer_entries(&mut members, 3, 3);
    let end = run_until_settled(&mut members, position + 3 * FRAME);
    assert_eq!(end, position + 3 * FRAME);
    let before: Vec<u8> = (0..3i64).flat_map(i64::to_le_bytes).collect();
    let all: Vec<u8> = (0..6i64).flat_map(i64::to_le_bytes).collect();

    // The service's replay plays the log into its image: polls that take nothing
    // until its first entry is there. The driver replays up to 1 MiB of a recording
    // at once, and the whole log, recorded and synced before the replay began, is
    // under 1 KiB, so the entries after the action are in the image with it; the
    // pause is there to make sure of that.
    let mut service = Service::beside(&members[0]);
    let mut handed = false;
    run_until(&mut members, "the service's image holds the log", |_| {
        service
            .service
            .poll(
                &mut service.client,
                |_, _| {
                    handed = true;
                    Action::Abort
                },
                16,
            )
            .expect("the service polls");
        handed
    });
    assert_eq!(service.service.position(), 0);
    std::thread::sleep(Duration::from_millis(100));

    // One poll whose fragment limit reaches past the action ends at the action: the
    // program's state is its state at the member's own snapshot's position.
    let mut state = Vec::new();
    let mut read: Vec<(i64, bool)> = Vec::new();
    service
        .service
        .poll(
            &mut service.client,
            |committed, _| {
                if let Entry::SessionMessage { payload, .. } = committed.entry {
                    state.extend_from_slice(payload);
                }
                read.push((committed.position, committed.entry == Entry::Snapshot));
                Action::Continue
            },
            16,
        )
        .expect("the service polls");
    assert!(read.len() + 3 <= 16, "{read:?}");
    assert_eq!(read.last(), Some(&(position - FRAME, true)), "{read:?}");
    assert_eq!(read.iter().filter(|(_, action)| *action).count(), 1);
    assert_eq!(state, before);
    assert_eq!(service.service.position(), position);
    assert_eq!(service.service.snapshot_due(), Some(position));

    // The program gives no state and polls on: the next poll lets the snapshot go,
    // and reads the entries after the action.
    let mut after: Vec<bool> = Vec::new();
    run_until(
        &mut members,
        "the service reads the entries after the action",
        |_| {
            service
                .service
                .poll(
                    &mut service.client,
                    |committed, _| {
                        if let Entry::SessionMessage { payload, .. } = committed.entry {
                            state.extend_from_slice(payload);
                        }
                        after.push(committed.entry == Entry::Snapshot);
                        Action::Continue
                    },
                    16,
                )
                .expect("the service polls");
            assert_eq!(service.service.snapshot_due(), None);
            service.service.position() == end
        },
    );
    assert_eq!(after, [false, false, false]);
    assert_eq!(state, all);
    assert!(matches!(
        service.service.snapshot(&mut service.client, &state),
        Err(ClusterError::NoSnapshotDue)
    ));

    // The service wrote nothing and sent no ack, so no member lists a snapshot of a
    // service for the action.
    let settled = Instant::now() + Duration::from_millis(200);
    run_until(&mut members, "the members run on", |_| {
        Instant::now() >= settled
    });
    for member in &members {
        assert_eq!(member.module.snapshot_position(), Some(position));
        assert_eq!(member.module.service_snapshot_position(), None);
        assert_eq!(
            member.module.service_snapshot_counts(),
            ServiceSnapshotCounts {
                listed: 0,
                rejected: 0
            }
        );
        assert_eq!(counter(member, CLUSTER_SERVICE_SNAPSHOT_POSITION.0), -1);
        assert_eq!(counter(member, CLUSTER_SERVICE_SNAPSHOTS_LISTED.0), 0);
        assert_eq!(listed(member).len(), 1);
    }
    service.close();
    for member in members {
        stop(member);
    }
}

#[test]
fn a_member_lists_nothing_for_a_service_ack_it_cannot_hold() {
    let dir = TestDir::new("acks");
    let members_string = members_string();
    let mut members: Vec<Member> = [1, 0, 2]
        .into_iter()
        .map(|id| start_member(dir.path(), &members_string, id))
        .collect();
    run_until_led(&mut members, -1);
    offer_entries(&mut members, 0, 2);
    let position = snapshot_everywhere(&mut members);
    // The service beside the leader takes the snapshot, and the leader lists it.
    let mut service = Service::beside(&members[0]);
    run_with_services(
        &mut members,
        std::slice::from_mut(&mut service),
        "the leader lists its service's snapshot",
        |members, _| members[0].module.service_snapshot_position() == Some(position),
    );
    // Two entries more, so that a position above the snapshot's is committed.
    offer_entries(&mut members, 2, 2);
    let end = run_until_settled(&mut members, position + 2 * FRAME);
    assert_eq!(end, position + 2 * FRAME);
    let list = listed(&members[0]);
    let services = list
        .iter()
        .find(|entry| entry.service_id == 0)
        .expect("the service's snapshot")
        .recording_id;
    let own = list
        .iter()
        .find(|entry| entry.service_id == CONSENSUS_MODULE)
        .expect("the consensus module's snapshot")
        .recording_id;

    // A second writer on the consensus module stream of the leader's driver, by
    // hand. An ack that names the service's recording at a committed position above
    // the snapshot's is one the member lists; each ack here differs from it in one
    // field, and the member lists nothing for it.
    let mut client = Cavalry::connect_to(&members[0].dir).expect("a client of the leader's driver");
    let mut acks = client
        .add_exclusive_publication("cavalry:ipc", CONSENSUS_MODULE_STREAM_ID)
        .expect("a publication on the consensus module stream");
    let mut send = |members: &mut [Member], message: &Message<'_>| {
        let mut bytes = [0u8; 64];
        let length = message.write(&mut bytes).expect("a message");
        run_until(
            members,
            "the consensus module stream takes a message",
            |_| acks.offer(&bytes[..length]).is_ok(),
        );
    };
    let ack = |log_position: i64, relevant_id: i64, service_id: i32| Message::ServiceAck {
        log_position,
        timestamp: 1_700_000_000_000,
        ack_id: 0,
        relevant_id,
        service_id,
    };
    let above = position + FRAME;
    let refused = [
        ("of another service", ack(above, services, 1)),
        (
            "of a recording the catalog does not hold",
            ack(above, 9_999, 0),
        ),
        ("of a recording of another stream", ack(above, own, 0)),
        (
            "of a position above the commit position",
            ack(end + FRAME, services, 0),
        ),
        ("of position 0", ack(0, services, 0)),
        (
            "of the position the member lists already",
            ack(position, services, 0),
        ),
        (
            "of a position that is not a multiple of 32, which the snapshots file \
             does not take",
            ack(above + 8, services, 0),
        ),
    ];
    for (rejected, (what, message)) in refused.iter().enumerate() {
        send(&mut members, message);
        run_until(&mut members, what, |members| {
            members[0].module.service_snapshot_counts().rejected == rejected as u64 + 1
        });
        assert_eq!(
            members[0].module.service_snapshot_counts().listed,
            1,
            "{what}"
        );
        assert_eq!(
            members[0].module.service_snapshot_position(),
            Some(position),
            "{what}"
        );
    }
    assert_eq!(listed(&members[0]).len(), 2);
    assert_eq!(
        counter(&members[0], CLUSTER_SERVICE_ACKS_REJECTED.0),
        refused.len() as i64
    );

    // A message the stream does not carry is rejected as a reader rejects it, and is
    // no ack: a session keepalive.
    let before = counter(&members[0], CLUSTER_MESSAGES_REJECTED.0);
    send(
        &mut members,
        &Message::SessionKeepalive {
            leadership_term_id: 0,
            cluster_session_id: 0,
        },
    );
    run_until(&mut members, "the message is rejected", |members| {
        counter(&members[0], CLUSTER_MESSAGES_REJECTED.0) == before + 1
    });
    assert_eq!(
        members[0].module.service_snapshot_counts().rejected,
        refused.len() as u64
    );

    // The ack that differs in nothing is listed: the member takes a service at its
    // word for what its recording holds.
    send(&mut members, &ack(above, services, 0));
    run_until(
        &mut members,
        "the member lists the ack's snapshot",
        |members| members[0].module.service_snapshot_position() == Some(above),
    );
    assert_eq!(
        members[0].module.service_snapshot_counts(),
        ServiceSnapshotCounts {
            listed: 2,
            rejected: refused.len() as u64
        }
    );
    assert_eq!(
        counter(&members[0], CLUSTER_SERVICE_SNAPSHOT_POSITION.0),
        above
    );
    drop(acks);
    client.close().expect("the client closes");
    service.close();
    for member in members {
        stop(member);
    }
}

#[test]
fn a_service_whose_member_has_stopped_gives_its_snapshot_up() {
    let dir = TestDir::new("no-member");
    let members_string = members_string();
    let mut members: Vec<Member> = [1, 0, 2]
        .into_iter()
        .map(|id| start_member(dir.path(), &members_string, id))
        .collect();
    run_until_led(&mut members, -1);
    offer_entries(&mut members, 0, 2);
    // A service beside a follower, whose program gives its state by hand.
    let mut service = Service::beside(&members[2]);
    service.gives = false;
    ask_snapshot(&mut members);
    run_with_services(
        &mut members,
        std::slice::from_mut(&mut service),
        "the service has read the action",
        |_, services| services[0].service.snapshot_due().is_some(),
    );
    let position = service.service.snapshot_due().expect("a snapshot is due");
    assert_eq!(service.service.position(), position);

    // A state longer than a snapshot holds is refused before anything is written,
    // and the snapshot is still due.
    let long = vec![0u8; MAX_SERVICE_SNAPSHOT_STATE_LENGTH + 1];
    assert!(matches!(
        service.service.snapshot(&mut service.client, &long),
        Err(ClusterError::SnapshotTooLong { found, limit })
            if found == long.len() && limit == MAX_SERVICE_SNAPSHOT_STATE_LENGTH
    ));
    drop(long);
    assert_eq!(service.service.snapshot_due(), Some(position));

    // The member stops, and its driver runs on. The service writes its recording
    // and has nobody to tell: the snapshot is given up at the ack, after the
    // snapshot timeout, counted, and still due.
    let Member {
        driver,
        module,
        dir: driver_dir,
        ..
    } = members.pop().expect("the follower");
    let id = module.member_id();
    module.close().expect("the member closes");
    let asked = Instant::now();
    let failed = service
        .service
        .snapshot(&mut service.client, &service.state)
        .expect_err("a snapshot with no member to tell");
    assert!(
        matches!(
            failed,
            ClusterError::SnapshotFailed {
                cause: SnapshotCause::Ack
            }
        ),
        "{failed}"
    );
    assert!(
        asked.elapsed() >= Duration::from_secs(1),
        "{:?}",
        asked.elapsed()
    );
    let text = failed.to_string();
    assert!(
        text.contains("check that the cluster member runs on this driver"),
        "{text}"
    );
    assert_eq!(service.service.snapshot_due(), Some(position));
    assert_eq!(
        counter_in(&driver_dir, id, CLUSTER_SERVICE_SNAPSHOTS_FAILED.0),
        1
    );
    assert_eq!(
        counter_in(&driver_dir, id, CLUSTER_SERVICE_SNAPSHOTS_TAKEN.0),
        0
    );
    service.close();
    driver.close().expect("the driver closes");
    for member in members {
        stop(member);
    }
}

/// How many counters of `type_id` whose key begins with `member_id` the counters file
/// of the driver in `dir` holds.
fn counters_in(dir: &Path, member_id: i32, type_id: i32) -> usize {
    let reader = CountersReader::open(dir).expect("the counters file opens");
    let key = member_id.to_le_bytes();
    (0..reader.capacity())
        .filter(|&id| {
            reader
                .record(id)
                .expect("a record")
                .is_some_and(|record| record.type_id() == type_id && record.key().starts_with(&key))
        })
        .count()
}

#[test]
fn a_service_counts_a_snapshot_whose_publication_its_driver_refuses() {
    let dir = TestDir::new("refused");
    let members_string = members_string();
    let mut members: Vec<Member> = [1, 0, 2]
        .into_iter()
        .map(|id| start_member(dir.path(), &members_string, id))
        .collect();
    run_until_led(&mut members, -1);
    offer_entries(&mut members, 0, 2);
    // A service beside a follower, whose program gives its state by hand, once every
    // member lists its own snapshot of the action.
    let mut service = Service::beside(&members[2]);
    service.gives = false;
    ask_snapshot(&mut members);
    run_with_services(
        &mut members,
        std::slice::from_mut(&mut service),
        "the service has read the action and every member has taken its snapshot",
        |members, services| {
            services[0].service.snapshot_due().is_some_and(|due| {
                members
                    .iter()
                    .all(|member| member.module.snapshot_position() == Some(due))
            })
        },
    );
    let position = service.service.snapshot_due().expect("a snapshot is due");
    let id = members[2].module.member_id();
    let driver_dir = members[2].dir.clone();

    // Another client of the follower's driver fills the driver's publications
    // registry, every member running a duty cycle after each add. Its publication is
    // one of the shortest term a channel names, 64 KiB, to an endpoint nobody reads:
    // a client maps and touches a publication's whole log at each add, which is 192
    // MiB for a `cavalry:ipc` one.
    let channel = format!(
        "cavalry:udp?endpoint=127.0.0.1:{}|term-length=64k",
        free_ports(1)[0]
    );
    let mut filler = Cavalry::connect_to(&driver_dir).expect("a client of the follower's driver");
    let mut held = Vec::new();
    loop {
        match filler.add_publication(&channel, REPLAY_STREAM_ID + 1) {
            Ok(publication) => held.push(publication),
            Err(ClientError::Refused { .. }) => break,
            Err(other) => panic!("{other}"),
        }
        assert!(held.len() <= MAX_PUBLICATIONS, "the registry is bounded");
        round();
        for member in members.iter_mut() {
            member.module.do_work().expect("the member's duty cycle");
        }
    }

    // At the service's first snapshot the driver refuses its publication: the
    // snapshot is given up, counted, and still due. A second call is refused alike,
    // counted again, and adds no counter twice.
    for failed in 1..=2 {
        let refused = service
            .service
            .snapshot(&mut service.client, &service.state)
            .expect_err("a snapshot whose publication the driver refuses");
        assert!(
            matches!(refused, ClusterError::Client(ClientError::Refused { .. })),
            "{refused}"
        );
        assert_eq!(service.service.snapshot_due(), Some(position));
        assert_eq!(
            counter_in(&driver_dir, id, CLUSTER_SERVICE_SNAPSHOTS_FAILED.0),
            failed
        );
        assert_eq!(
            counter_in(&driver_dir, id, CLUSTER_SERVICE_SNAPSHOTS_TAKEN.0),
            0
        );
    }
    for type_id in [
        CLUSTER_SERVICE_SNAPSHOTS_TAKEN.0,
        CLUSTER_SERVICE_SNAPSHOTS_FAILED.0,
    ] {
        assert_eq!(counters_in(&driver_dir, id, type_id), 1);
    }

    // The registry has room again once the other client has closed, which removes
    // its publications, and the call after that writes the snapshot, which the
    // member lists.
    filler.close().expect("the other client closes");
    drop(held);
    let taken = meanwhile(&mut members, || {
        timed("a service's snapshot", || {
            service
                .service
                .snapshot(&mut service.client, &service.state)
                .expect("the service's snapshot")
        })
    });
    assert!(taken);
    run_with_services(
        &mut members,
        std::slice::from_mut(&mut service),
        "the follower lists its service's snapshot",
        |members, _| members[2].module.service_snapshot_position() == Some(position),
    );
    assert_eq!(
        counter_in(&driver_dir, id, CLUSTER_SERVICE_SNAPSHOTS_FAILED.0),
        2
    );
    assert_eq!(
        counter_in(&driver_dir, id, CLUSTER_SERVICE_SNAPSHOTS_TAKEN.0),
        1
    );
    for type_id in [
        CLUSTER_SERVICE_SNAPSHOTS_TAKEN.0,
        CLUSTER_SERVICE_SNAPSHOTS_FAILED.0,
    ] {
        assert_eq!(counters_in(&driver_dir, id, type_id), 1);
    }
    service.close();
    for member in members {
        stop(member);
    }
}

/// How long a service's `snapshot` takes, for ADR 0131: the call waits for the
/// driver, on the program's own thread. Three members, a service beside the leader,
/// and states of four lengths, each given ten times.
#[test]
#[ignore = "a measurement for ADR 0131, run by hand"]
fn a_services_snapshot_timed() {
    let dir = TestDir::new("service-timed");
    let members_string = members_string();
    let mut members: Vec<Member> = [1, 0, 2]
        .into_iter()
        .map(|id| start_member(dir.path(), &members_string, id))
        .collect();
    run_until_led(&mut members, -1);
    let mut service = Service::beside(&members[0]);
    service.gives = false;
    for length in [0usize, 1 << 10, 1 << 20, 64 << 20] {
        let state = vec![0x5au8; length];
        let mut took: Vec<Duration> = Vec::new();
        for _ in 0..10 {
            ask_snapshot(&mut members);
            run_with_services(
                &mut members,
                std::slice::from_mut(&mut service),
                "the service has read the action",
                |members, services| {
                    services[0].service.snapshot_due().is_some_and(|due| {
                        members
                            .iter()
                            .all(|member| member.module.snapshot_position() == Some(due))
                    })
                },
            );
            let position = service.service.snapshot_due().expect("a snapshot is due");
            let asked = Instant::now();
            let taken = service
                .service
                .snapshot(&mut service.client, &state)
                .expect("the service's snapshot");
            took.push(asked.elapsed());
            assert!(taken);
            run_with_services(
                &mut members,
                std::slice::from_mut(&mut service),
                "the leader lists the service's snapshot",
                |members, _| members[0].module.service_snapshot_position() == Some(position),
            );
        }
        took.sort();
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        println!(
            "a state of {length} bytes, 10 snapshots: p50 {:.1} ms, max {:.1} ms; {:.0} MiB/s at the median",
            ms(took[took.len() / 2]),
            ms(took[took.len() - 1]),
            length as f64 / (1 << 20) as f64 / took[took.len() / 2].as_secs_f64()
        );
    }
    service.close();
    for member in members {
        stop(member);
    }
}

/// The leader asks for a snapshot, and every member lists its own and its service's
/// at one position, past every snapshot listed before: that position.
fn snapshot_with_services(members: &mut [Member], services: &mut [Service]) -> i64 {
    let before = members
        .iter()
        .filter_map(|m| m.module.service_snapshot_position())
        .max()
        .unwrap_or(-1);
    ask_snapshot(members);
    let mut position = -1;
    run_with_services(
        members,
        services,
        "every member lists its own snapshot and its service's",
        |members, _| {
            position = members[0].module.service_snapshot_position().unwrap_or(-1);
            position > before
                && members.iter().all(|m| {
                    m.module.service_snapshot_position() == Some(position)
                        && m.module.snapshot_position() == Some(position)
                })
        },
    );
    position
}

/// Runs the members and the services until every service has read the log to `end`.
fn read_to(members: &mut [Member], services: &mut [Service], end: i64) {
    run_with_services(
        members,
        services,
        "every service has read the log",
        |_, services| services.iter().all(|s| s.service.position() == end),
    );
}

/// The recording of the newest snapshot of a service that `member` lists.
fn service_recording(member: &Member) -> i64 {
    listed(member)
        .iter()
        .rev()
        .find(|entry| entry.service_id == 0)
        .expect("a service's snapshot")
        .recording_id
}

/// The first `count` entries' payloads, as a service's program holds them.
fn entries(count: i64) -> Vec<u8> {
    (0..count).flat_map(i64::to_le_bytes).collect()
}

#[test]
fn a_service_that_starts_again_takes_its_state_from_its_snapshot_and_reads_what_follows() {
    let dir = TestDir::new("service-restored");
    let (_, mut members, clients, end) = led_with_two_sessions(&dir);
    let mut services: Vec<Service> = members.iter().map(Service::beside).collect();
    offer_entries(&mut members, 0, 3);
    run_until_settled(&mut members, end + 3 * FRAME);
    let position = snapshot_with_services(&mut members, &mut services);
    // What a service's start reads, and what an operator may purge: every member
    // names its service's recording, and needs its log from the snapshots' position.
    for member in &members {
        assert_eq!(
            counter(member, CLUSTER_SERVICE_SNAPSHOT_RECORDING_ID.0),
            service_recording(member)
        );
        assert_eq!(member.module.log_purge_position(), position);
        assert_eq!(counter(member, CLUSTER_LOG_PURGE_POSITION.0), position);
    }
    // Two entries more, which every service reads.
    offer_entries(&mut members, 3, 2);
    let end = run_until_settled(&mut members, position + 2 * FRAME);
    read_to(&mut members, &mut services, end);
    for service in &services {
        assert_eq!(service.state, entries(5));
    }

    // The service beside a follower stops, and one starts from its snapshot: its
    // program is given the state at the snapshot's position, and the service holds
    // the two sessions and that position before it has read an entry.
    services.pop().expect("a service").close();
    let (mut again, restored) = Service::restored_beside(&mut members, 2);
    assert_eq!(restored, Restored::State(entries(3)));
    assert_eq!(again.service.position(), position);
    assert_eq!(again.service.sessions(), 2);
    assert_eq!(
        counter(&members[2], CLUSTER_SERVICE_SNAPSHOT_START_POSITION.0),
        position
    );
    assert_eq!(
        counter(&members[2], CLUSTER_SERVICE_SNAPSHOTS_PASSED_OVER.0),
        0
    );
    // The service has begun: a second start is refused.
    assert!(matches!(
        again.service.restore(&mut again.client),
        Err(ClusterError::RestoreTooLate)
    ));
    // It reads the two entries that follow its snapshot, and none below it: it
    // meets no action, and holds what the others hold.
    read_to(&mut members, std::slice::from_mut(&mut again), end);
    assert_eq!(again.state, entries(5));
    assert!(again.taken.is_empty());
    assert!(!again.service.answers());

    // The leader's service starts from its snapshot too, and reads what follows,
    // which its member held when it opened the log: history, answered by none.
    services.remove(0).close();
    let (leaders, restored) = Service::restored_beside(&mut members, 0);
    assert_eq!(restored, Restored::State(entries(3)));
    let mut restarted = vec![leaders, again];
    read_to(&mut members, &mut restarted, end);
    assert!(!restarted[0].service.answers());
    // An entry its member appends from here on is the snapshot's term's, whose
    // event the service did not read: the service beside the leader answers it,
    // on its member's word, and the one beside a follower does not.
    offer_entries(&mut members, 5, 1);
    let end = run_until_settled(&mut members, end + FRAME);
    read_to(&mut members, &mut restarted, end);
    assert_eq!(restarted[0].state, entries(6));
    assert_eq!(restarted[1].state, entries(6));
    assert!(restarted[0].service.answers());
    assert!(!restarted[1].service.answers());

    // The next snapshot: the two services that started from one and the one that
    // never stopped give the same state, and their recordings hold the same bytes.
    restarted.append(&mut services);
    let second = snapshot_with_services(&mut members, &mut restarted);
    assert!(second > position);
    let recordings = replayed_each(&mut members, service_recording);
    assert_eq!(recordings[0], recordings[1]);
    assert_eq!(recordings[0], recordings[2]);
    assert_eq!(
        recordings[0][4],
        entries(6),
        "the state, after two sessions"
    );
    for service in restarted {
        service.close();
    }
    for client in clients {
        client.close();
    }
    for member in members {
        stop(member);
    }
}

#[test]
fn a_member_and_its_service_start_from_their_snapshots_on_a_log_purged_below_them() {
    let dir = TestDir::new("service-purged");
    let (members_string, mut members, clients, end) = led_with_two_sessions(&dir);
    offer_entries(&mut members, 0, 2);
    let end = run_until_settled(&mut members, end + 2 * FRAME);
    // The leader stops: the two others elect one of them, and the first term's log
    // is from then on a recording that each of them has stopped.
    let first_term = members[0].module.leadership_term_id();
    let leader = members.remove(0);
    let old_leader = meanwhile(&mut members, || stop(leader));
    run_until_led(&mut members, first_term);
    let started = meanwhile(&mut members, || {
        start_member(dir.path(), &members_string, old_leader)
    });
    members.push(started);
    run_until_settled(&mut members, end + FRAME);
    run_until_led(&mut members, first_term);
    offer_entries(&mut members, 2, 2);
    // A service beside each member reads both terms' log from its start, and the
    // members and the services take a snapshot.
    let mut services: Vec<Service> = members.iter().map(Service::beside).collect();
    let position = snapshot_with_services(&mut members, &mut services);
    offer_entries(&mut members, 4, 2);
    let end = run_until_settled(&mut members, position + 2 * FRAME);
    read_to(&mut members, &mut services, end);

    // The follower that followed in both terms, and its service, stop.
    let follower = members
        .iter()
        .position(|m| m.module.role() != Role::Leader && m.module.member_id() != old_leader)
        .expect("a follower of both terms");
    let stopped = members.remove(follower);
    let id = stopped.module.member_id();
    assert_eq!(stopped.module.log_purge_position(), position);
    let its_service = services
        .iter()
        .position(|service| service.id == id)
        .expect("the follower's service");
    services.remove(its_service).close();
    let log_dir = stopped.log_dir.clone();
    meanwhile(&mut members, || stop(stopped));
    let record = RecordingsFile::open_kept(&log_dir, id).expect("the member's record of its log");
    let recordings: Vec<(i64, i64)> = record
        .entries()
        .iter()
        .map(|entry| (entry.recording_id, entry.start_position))
        .collect();
    drop(record);
    // An operator purges the recordings before the one the member's log is needed
    // from, the last that begins at or below the position of both snapshots.
    let kept = needed_from(&recordings, position);
    let purged: Vec<i64> = recordings[..kept].iter().map(|(id, _)| *id).collect();
    meanwhile(&mut members, || purge_each(dir.path(), id, &purged));

    // The member starts from its snapshot, on the log that is left, and its service
    // from its own: the program is given its state at the snapshot's position.
    let again = meanwhile(&mut members, || {
        start_member(dir.path(), &members_string, id)
    });
    assert_eq!(
        again.module.session_log_position(),
        position,
        "the member that started again: {}; the two that ran on: {}; the log ended at {end}",
        describe(std::slice::from_ref(&again)),
        describe(&members)
    );
    members.push(again);
    let settled = run_until_settled(&mut members, end);
    assert_eq!(settled, end, "{}", describe(&members));
    let (service, restored) = Service::restored_beside(&mut members, 2);
    assert_eq!(restored, Restored::State(entries(4)));
    assert_eq!(service.service.position(), position);
    services.push(service);
    // Nothing committed is lost: every service holds the six entries, and the next
    // snapshot's bytes are the same beside the three members.
    read_to(&mut members, &mut services, end);
    for service in &services {
        assert_eq!(
            service.state,
            entries(6),
            "the service beside member {}",
            service.id
        );
    }
    run_until_led(&mut members, first_term);
    let second = snapshot_with_services(&mut members, &mut services);
    assert!(second > position);
    let written = replayed_each(&mut members, service_recording);
    assert_eq!(written[0], written[1]);
    assert_eq!(written[0], written[2]);
    for member in &members {
        assert_eq!(member.module.log_purge_position(), second);
    }
    for service in services {
        service.close();
    }
    for client in clients {
        client.close();
    }
    for member in members {
        stop(member);
    }
}

#[test]
fn a_service_passes_over_a_snapshot_whose_recording_is_gone_and_reads_from_the_start() {
    let dir = TestDir::new("service-passed-over");
    let members_string = members_string();
    let mut members: Vec<Member> = [1, 0, 2]
        .into_iter()
        .map(|id| start_member(dir.path(), &members_string, id))
        .collect();
    run_until_led(&mut members, -1);
    run_until_settled(&mut members, 0);
    // A service whose program asks for a start from a snapshot, beside a member that
    // lists none: it reads from position 0, and adds no counter of its start.
    let (first, restored) = Service::restored_beside(&mut members, 2);
    assert_eq!(restored, Restored::FromStart);
    let mut services = vec![
        Service::beside(&members[0]),
        Service::beside(&members[1]),
        first,
    ];
    offer_entries(&mut members, 0, 3);
    let position = snapshot_with_services(&mut members, &mut services);
    offer_entries(&mut members, 3, 1);
    let end = run_until_settled(&mut members, position + FRAME);
    read_to(&mut members, &mut services, end);
    assert_eq!(services[2].state, entries(4));

    // The service and its member stop, and the recording of the service's snapshot
    // is purged, by mistake. The member's own snapshot is whole.
    services.pop().expect("the service").close();
    let stopped = members.pop().expect("the follower");
    let recording_id = service_recording(&stopped);
    let id = meanwhile(&mut members, || stop(stopped));
    meanwhile(&mut members, || purge(dir.path(), id, recording_id));
    let started = meanwhile(&mut members, || {
        start_member(dir.path(), &members_string, id)
    });
    members.push(started);
    assert_eq!(run_until_settled(&mut members, end), end);
    assert_eq!(
        counter(&members[2], CLUSTER_SERVICE_SNAPSHOT_RECORDING_ID.0),
        recording_id,
        "the member still lists the snapshot"
    );

    // The service is told of that snapshot, cannot play it back, and passes it
    // over: its program is told to read from the start, and the log is whole.
    let (mut again, restored) = Service::restored_beside(&mut members, 2);
    assert_eq!(restored, Restored::FromStart);
    assert_eq!(again.service.position(), 0);
    assert_eq!(
        counter(&members[2], CLUSTER_SERVICE_SNAPSHOT_START_POSITION.0),
        -1
    );
    assert_eq!(
        counter(&members[2], CLUSTER_SERVICE_SNAPSHOTS_PASSED_OVER.0),
        1
    );
    read_to(&mut members, std::slice::from_mut(&mut again), end);
    assert_eq!(again.state, entries(4));
    // It met the action again, and its member lists a snapshot of that position,
    // whose recording is gone: it writes none.
    assert_eq!(again.taken, [(position, false)]);
    again.close();
    for service in services {
        service.close();
    }
    for member in members {
        stop(member);
    }
}

#[test]
fn a_service_whose_driver_has_no_room_for_its_replay_has_not_started_and_asks_again() {
    let dir = TestDir::new("service-replays-full");
    let members_string = members_string();
    let mut members: Vec<Member> = [1, 0, 2]
        .into_iter()
        .map(|id| start_member(dir.path(), &members_string, id))
        .collect();
    run_until_led(&mut members, -1);
    run_until_settled(&mut members, 0);
    let mut services: Vec<Service> = members.iter().map(Service::beside).collect();
    offer_entries(&mut members, 0, 3);
    let position = snapshot_with_services(&mut members, &mut services);
    offer_entries(&mut members, 3, 1);
    let end = run_until_settled(&mut members, position + FRAME);
    read_to(&mut members, &mut services, end);
    // The service beside a follower stops.
    services.pop().expect("the follower's service").close();
    let id = members[2].module.member_id();
    let driver_dir = members[2].dir.clone();

    // Another client of the follower's driver holds the driver's replays registry at
    // its bound, every member running a duty cycle after each start. Each replay
    // follows a live recording into a subscription of that client, so none ends. The
    // recording is of a publication of the shortest term a channel names, 64 KiB, to
    // an endpoint nobody reads: a replay's log has its recording's term, and that of
    // a `cavalry:ipc` one is 64 MiB.
    let stream_id = REPLAY_STREAM_ID + 1;
    let channel = format!(
        "cavalry:udp?endpoint=127.0.0.1:{}|term-length=64k",
        free_ports(1)[0]
    );
    let mut filler = Cavalry::connect_to(&driver_dir).expect("a client of the follower's driver");
    let publication = filler
        .add_publication(&channel, stream_id)
        .expect("a publication to record");
    let recording = filler
        .start_recording(&channel, stream_id)
        .expect("the publication's recording");
    let mut subscription = filler
        .add_subscription("cavalry:ipc", stream_id)
        .expect("a subscription for the replays");
    // A poll with a fragment limit of 0 takes the images the driver announced and
    // withdrew, and reads no frame, so that the subscription's queue of them, 64,
    // does not fill and close the client.
    let take_images = |subscription: &mut Subscription| {
        subscription
            .poll(|_, _| {}, 0)
            .expect("the subscription takes its images");
    };
    let mut held = Vec::new();
    loop {
        match filler.start_replay(
            recording.recording_id(),
            NULL_POSITION,
            FOLLOW_LENGTH,
            stream_id,
            "cavalry:ipc",
        ) {
            Ok(replay) => held.push(replay),
            Err(ClientError::Refused {
                code: Some(ErrorCode::ResourceTemporarilyUnavailable),
                ..
            }) => break,
            Err(other) => panic!("{other}"),
        }
        assert!(held.len() <= MAX_REPLAYS, "the registry is bounded");
        take_images(&mut subscription);
        round();
        for member in members.iter_mut() {
            member.module.do_work().expect("the member's duty cycle");
        }
    }
    take_images(&mut subscription);

    // A service starts, and its driver has no room for its snapshot's replay. That is
    // the start's error, with the driver's code, and no snapshot passed over: the
    // service has not started, and a second call is refused alike, not as too late.
    let mut again = Service::beside(&members[2]);
    for _ in 0..2 {
        match again.service.restore(&mut again.client) {
            Err(ClusterError::Client(ClientError::Refused {
                code: Some(ErrorCode::ResourceTemporarilyUnavailable),
                ..
            })) => {}
            other => panic!("a start whose replay the driver has no room for: {other:?}"),
        }
        assert_eq!(again.service.position(), 0);
    }
    // It counted nothing, and holds no counter of its start once the driver has
    // taken the removes its refused calls sent.
    run_until(
        &mut members,
        "the driver removes the counters of the refused starts",
        |_| {
            counters_in(&driver_dir, id, CLUSTER_SERVICE_SNAPSHOTS_PASSED_OVER.0) == 0
                && counters_in(&driver_dir, id, CLUSTER_SERVICE_SNAPSHOT_START_POSITION.0) == 0
        },
    );

    // One of the other client's replays stops, and the next call takes the snapshot:
    // the program is given its state at the snapshot's position.
    let freed = held.pop().expect("a replay");
    freed.stop(&mut filler).expect("the replay stops");
    run_until(&mut members, "the stopped replay has ended", |_| {
        take_images(&mut subscription);
        freed.ended().is_some()
    });
    let restored = meanwhile(&mut members, || {
        timed("a service's start", || {
            again
                .service
                .restore(&mut again.client)
                .expect("the service's start, once the driver has room")
        })
    });
    assert_eq!(restored, Restored::State(entries(3)));
    if let Restored::State(state) = restored {
        again.state = state;
    }
    assert_eq!(again.service.position(), position);
    assert_eq!(
        counter_in(&driver_dir, id, CLUSTER_SERVICE_SNAPSHOT_START_POSITION.0),
        position
    );
    assert_eq!(
        counter_in(&driver_dir, id, CLUSTER_SERVICE_SNAPSHOTS_PASSED_OVER.0),
        0
    );
    for type_id in [
        CLUSTER_SERVICE_SNAPSHOT_START_POSITION.0,
        CLUSTER_SERVICE_SNAPSHOTS_PASSED_OVER.0,
    ] {
        assert_eq!(counters_in(&driver_dir, id, type_id), 1);
    }
    // It reads what follows its snapshot, and holds what the others hold.
    read_to(&mut members, std::slice::from_mut(&mut again), end);
    assert_eq!(again.state, entries(4));

    // The other client's replays and the client go: the subscription goes first, so
    // that no withdrawn image waits in its queue.
    drop(subscription);
    for replay in &held {
        replay.stop(&mut filler).expect("the replay stops");
    }
    drop(publication);
    filler.close().expect("the other client closes");
    again.close();
    for service in services {
        service.close();
    }
    for member in members {
        stop(member);
    }
}

/// How long a service's `restore` takes, for ADR 0132: the call plays the snapshot's
/// recording back and waits for the driver, on the program's own thread. Three
/// members, a service beside the leader that takes a snapshot of each of four
/// states, and ten starts from each.
#[test]
#[ignore = "a measurement for ADR 0132, run by hand"]
fn a_services_start_from_its_snapshot_timed() {
    let dir = TestDir::new("service-start-timed");
    let members_string = members_string();
    let mut members: Vec<Member> = [1, 0, 2]
        .into_iter()
        .map(|id| start_member(dir.path(), &members_string, id))
        .collect();
    run_until_led(&mut members, -1);
    let mut service = Service::beside(&members[0]);
    service.gives = false;
    for length in [0usize, 1 << 10, 1 << 20, 64 << 20] {
        let state = vec![0x5au8; length];
        ask_snapshot(&mut members);
        run_with_services(
            &mut members,
            std::slice::from_mut(&mut service),
            "the service has read the action",
            |members, services| {
                services[0].service.snapshot_due().is_some_and(|due| {
                    members
                        .iter()
                        .all(|member| member.module.snapshot_position() == Some(due))
                })
            },
        );
        let position = service.service.snapshot_due().expect("a snapshot is due");
        assert!(
            service
                .service
                .snapshot(&mut service.client, &state)
                .expect("the service's snapshot")
        );
        run_with_services(
            &mut members,
            std::slice::from_mut(&mut service),
            "the leader lists the service's snapshot",
            |members, _| members[0].module.service_snapshot_position() == Some(position),
        );
        let mut took: Vec<Duration> = Vec::new();
        for _ in 0..10 {
            let mut client =
                Cavalry::connect_to(&members[0].dir).expect("the starting service's client");
            let mut starting = client.cluster_service();
            let asked = Instant::now();
            let restored = starting.restore(&mut client).expect("the service's start");
            took.push(asked.elapsed());
            assert!(
                matches!(&restored, Restored::State(given) if given.len() == length),
                "a state of {length} bytes: the start took {:?} and found {}",
                asked.elapsed(),
                match &restored {
                    Restored::State(given) => format!("a state of {} bytes", given.len()),
                    other => format!("{other:?}"),
                }
            );
            assert_eq!(starting.position(), position);
            starting.close(&mut client).expect("the service closes");
            client.close().expect("the client closes");
        }
        took.sort();
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        println!(
            "a state of {length} bytes, 10 starts: p50 {:.1} ms, max {:.1} ms",
            ms(took[took.len() / 2]),
            ms(took[took.len() - 1]),
        );
    }
    service.close();
    for member in members {
        stop(member);
    }
}

/// A command's side of a control toggle, as a test holds it: a client of the member's
/// driver, the counter it added with the member's id and its own client id as the
/// key, and that id (`spec/cluster.md`, "The control toggle"; ADR 0147).
struct Toggle {
    client: Cavalry,
    counter: cavalry_client::Counter,
    id: i64,
}

impl Toggle {
    /// Adds a toggle for `member` on its driver, holding `value`.
    fn add(member: &Member, value: i64) -> Self {
        let mut client = Cavalry::connect_to(&member.dir).expect("the command's client connects");
        let id = client.client_id();
        let member_id = member.module.member_id();
        let counter = client
            .add_counter(
                cavalry_shm::counters::generated::CLUSTER_CONTROL_TOGGLE.0,
                &cavalry_proto::cluster::control_toggle_key(member_id, id),
                &format!("cluster.control_toggle {member_id}"),
            )
            .expect("the driver adds the toggle");
        counter.store_release(value).expect("the toggle is stored");
        Self {
            client,
            counter,
            id,
        }
    }

    /// Removes the toggle, as a command does when it ends.
    fn remove(self) {
        self.counter.close();
        self.client.close().expect("the command's client closes");
    }
}

/// What a member holds of the control toggle it took last: its id, and the log
/// position of the snapshot it asked for it.
fn toggle_taken(member: &Member) -> (i64, i64) {
    use cavalry_shm::counters::generated::{
        CLUSTER_CONTROL_TOGGLE_SNAPSHOT_POSITION, CLUSTER_CONTROL_TOGGLE_TAKEN,
    };
    (
        counter(member, CLUSTER_CONTROL_TOGGLE_TAKEN.0),
        counter(member, CLUSTER_CONTROL_TOGGLE_SNAPSHOT_POSITION.0),
    )
}

/// Runs the members until the leader, `members[0]`, has looked over its driver's whole
/// counters file once. A leader looks at the first duty cycle that begins a
/// millisecond or more after its last look, 16 records a look, and this loop runs its
/// duty cycle at every turn. So each span of two milliseconds of this loop holds a
/// look or more, whenever in a cycle the leader reads its clock: the loop counts such
/// spans, and ends when their records are the file's and 16 more. A slow runner makes
/// it longer, never shorter.
fn run_past_a_whole_look(members: &mut [Member]) {
    use cavalry_proto::cluster::{CONTROL_TOGGLE_LOOK_INTERVAL_NS, CONTROL_TOGGLE_LOOK_RECORDS};

    let capacity = CountersReader::open(&members[0].dir)
        .expect("the counters file opens")
        .capacity();
    let looks = capacity / CONTROL_TOGGLE_LOOK_RECORDS + 1;
    let span = Duration::from_nanos(2 * CONTROL_TOGGLE_LOOK_INTERVAL_NS);
    let mut counted = 0;
    let mut last = Instant::now();
    run_until(members, "the leader looks over its counters file", |_| {
        if last.elapsed() >= span {
            counted += 1;
            last = Instant::now();
        }
        counted >= looks
    });
}

/// A leader takes a control toggle that asks for a snapshot, once, and every member
/// takes the snapshot at the position the leader stored for it. A member that does not
/// lead takes none, a toggle of another value is passed over, a toggle that stays is
/// not taken again and holds no later one back, and one whose id is below the last
/// taken is never taken.
#[test]
fn a_leader_takes_a_control_toggle_once_and_every_member_takes_its_snapshot() {
    use cavalry_proto::cluster::CONTROL_TOGGLE_SNAPSHOT;

    let dir = TestDir::new("toggle");
    let members_string = members_string();
    let mut members: Vec<Member> = [1, 0, 2]
        .into_iter()
        .map(|id| start_member(dir.path(), &members_string, id))
        .collect();
    run_until_led(&mut members, -1);
    offer_entries(&mut members, 0, 3);
    let end = 4 * FRAME;
    run_until_committed(&mut members, "the three entries are committed", end);
    for member in &members {
        assert_eq!(toggle_taken(member), (-1, -1), "no toggle taken yet");
    }

    // A toggle beside a follower, and one beside the leader that holds Aeron's
    // suspend, 2: neither is taken, however long they stay.
    let beside_follower = Toggle::add(&members[1], CONTROL_TOGGLE_SNAPSHOT);
    let suspend = Toggle::add(&members[0], 2);
    run_past_a_whole_look(&mut members);
    for member in &members {
        assert_eq!(toggle_taken(member), (-1, -1));
        assert_eq!(counter(member, CLUSTER_SNAPSHOT_ACTIONS.0), 0);
        assert_eq!(member.module.snapshot_position(), None);
    }

    // A toggle beside the leader that asks for a snapshot: the leader appends one
    // action, stores the snapshot's position and then the toggle's id, and every
    // member lists its snapshot at that position.
    let first = Toggle::add(&members[0], CONTROL_TOGGLE_SNAPSHOT);
    assert!(
        first.id > suspend.id,
        "a driver gives its clients rising ids"
    );
    run_until(&mut members, "the leader took the toggle", |members| {
        toggle_taken(&members[0]).0 == first.id
    });
    let (_, position) = toggle_taken(&members[0]);
    assert_eq!(
        position,
        end + FRAME,
        "the position after the action's entry"
    );
    run_until(
        &mut members,
        "every member listed the snapshot",
        |members| {
            members
                .iter()
                .all(|m| m.module.snapshot_position() == Some(position))
        },
    );
    assert_eq!(counter(&members[0], CLUSTER_SNAPSHOT_ACTIONS.0), 1);

    // The toggle stays, as that of a command that did not end, and the command that
    // held suspend stores snapshot in its own, whose id is below the one taken. The
    // leader takes neither, however often it looks: one request is one action, and a
    // toggle below the last taken is passed over.
    suspend
        .counter
        .store_release(CONTROL_TOGGLE_SNAPSHOT)
        .expect("the toggle is stored");
    run_past_a_whole_look(&mut members);
    assert_eq!(toggle_taken(&members[0]), (first.id, position));
    assert_eq!(counter(&members[0], CLUSTER_SNAPSHOT_ACTIONS.0), 1);

    // A second toggle, while the first two stay: they hold it back for nothing. It is
    // taken, with a snapshot of its own that every member lists.
    let second = Toggle::add(&members[0], CONTROL_TOGGLE_SNAPSHOT);
    run_until(
        &mut members,
        "the leader took the second toggle",
        |members| toggle_taken(&members[0]).0 == second.id,
    );
    let (_, next) = toggle_taken(&members[0]);
    assert_eq!(next, position + FRAME);
    run_until(
        &mut members,
        "every member listed the second snapshot",
        |members| {
            members
                .iter()
                .all(|m| m.module.snapshot_position() == Some(next))
        },
    );
    // And with all three still there, no more: each was taken once or not at all.
    run_past_a_whole_look(&mut members);
    assert_eq!(toggle_taken(&members[0]), (second.id, next));
    assert_eq!(counter(&members[0], CLUSTER_SNAPSHOT_ACTIONS.0), 2);
    for member in &members[1..] {
        assert_eq!(
            toggle_taken(member),
            (-1, -1),
            "a follower takes no toggle, its own among them"
        );
        assert_eq!(counter(member, CLUSTER_SNAPSHOT_ACTIONS.0), 0);
    }

    first.remove();
    second.remove();
    suspend.remove();
    beside_follower.remove();
    for member in members {
        stop(member);
    }
}

/// What one look costs the thread that runs a leader: 16 metadata records of a
/// counters file of 16,384, read as the leader reads them, 20,000 looks over the
/// file. The median, the 99.99th percentile and the longest are printed. Ignored,
/// since it measures and asserts nothing: `cargo test --release -p cavalry-raft --test
/// snapshot -- --ignored --nocapture a_control_toggles_look_timed`.
#[test]
#[ignore = "a measurement for ADR 0147, run by hand"]
fn a_control_toggles_look_timed() {
    use cavalry_proto::cluster::{CONTROL_TOGGLE_LOOK_RECORDS, control_toggle_id};
    use cavalry_shm::counters::generated::CLUSTER_CONTROL_TOGGLE;

    let dir = TestDir::new("look-timed");
    let members_string = members_string();
    let member = start_member(dir.path(), &members_string, 0);
    let reader = CountersReader::open(&member.dir).expect("the counters file opens");
    let capacity = reader.capacity();
    let mut next = 0u32;
    let mut found = 0u32;
    let mut took = Vec::with_capacity(20_000);
    for _ in 0..20_000 {
        let from = Instant::now();
        for _ in 0..CONTROL_TOGGLE_LOOK_RECORDS {
            let counter = if next < capacity { next } else { 0 };
            if let Ok(Some(record)) = reader.record(counter)
                && record.type_id() == CLUSTER_CONTROL_TOGGLE.0
                && control_toggle_id(record.key(), 0).is_some()
            {
                found += 1;
            }
            next = counter + 1;
        }
        took.push(from.elapsed());
    }
    took.sort();
    println!(
        "a look of {CONTROL_TOGGLE_LOOK_RECORDS} records of {capacity}, 20,000 looks, {found} toggles found: \
         p50 {} ns, p99.99 {} ns, max {} ns",
        took[took.len() / 2].as_nanos(),
        took[took.len() * 9_999 / 10_000].as_nanos(),
        took[took.len() - 1].as_nanos(),
    );
    stop(member);
}

/// What a timer event takes of the log: a frame header and the event, 32 bytes each.
const TIMER_EVENT_FRAME: i64 = 64;

/// A member holds the timers that a service on its driver tells it of, whatever its
/// role, and while it leads it appends one timer event for each whose deadline its
/// clock has reached, marks it, and appends another only when a schedule timer names
/// the id again (`spec/cluster.md`, "The service", "Timers", "The member's timers"
/// and "The leader's timer events"; ADR 0151). The service is a publication on the
/// consensus module stream, by hand: what a member does with a timer is decided by
/// what it is sent and by nothing a service is.
#[test]
fn a_leader_appends_one_event_for_each_due_timer_it_is_told_of_and_a_follower_none() {
    let dir = TestDir::new("timers");
    let members_string = members_string();
    let mut members: Vec<Member> = [1, 0, 2]
        .into_iter()
        .map(|id| start_member(dir.path(), &members_string, id))
        .collect();
    run_until_led(&mut members, -1);
    offer_entries(&mut members, 0, 2);
    // Where the log ends with the two entries: every position below is from here.
    let base = run_until_settled(&mut members, 2 * FRAME);
    let mut end = base;
    assert_eq!(
        members[0].module.timer_counts(),
        (TimerCounts::default(), 0)
    );

    let mut client = Cavalry::connect_to(&members[0].dir).expect("a client of the leader's driver");
    let mut stream = client
        .add_exclusive_publication("cavalry:ipc", CONSENSUS_MODULE_STREAM_ID)
        .expect("a publication on the consensus module stream");
    let mut send = |members: &mut [Member], message: &Message<'_>| {
        let mut bytes = [0u8; 64];
        let length = message.write(&mut bytes).expect("a message");
        run_until(
            members,
            "the consensus module stream takes a message",
            |_| stream.offer(&bytes[..length]).is_ok(),
        );
    };
    let schedule = |correlation_id: i64, deadline: i64| Message::ScheduleTimer {
        correlation_id,
        deadline,
    };
    let far = i64::MAX / 2;

    // Two timers: one due long ago, one due in no test's time. The leader appends
    // one event, for the first, and every member records it.
    send(&mut members, &schedule(7, 1));
    send(&mut members, &schedule(9, far));
    run_until(&mut members, "the leader appends the event", |members| {
        members[0].module.timer_counts().0.events == 1
    });
    end = run_until_settled(&mut members, end + TIMER_EVENT_FRAME);
    assert_eq!(end, base + TIMER_EVENT_FRAME);
    assert_eq!(
        members[0].module.timer_counts(),
        (
            TimerCounts {
                events: 1,
                ..TimerCounts::default()
            },
            2
        ),
        "the timer stays, marked, and the one that is not due stays"
    );
    // The mark holds through more duty cycles and more entries: no second event.
    offer_entries(&mut members, 2, 2);
    end = run_until_settled(&mut members, end + 2 * FRAME);
    assert_eq!(end, base + 2 * FRAME + TIMER_EVENT_FRAME);
    assert_eq!(members[0].module.timer_counts().0.events, 1);

    // A schedule timer for the id, the same deadline included, takes the mark off:
    // the service says the timer is pending still, and the leader appends again.
    send(&mut members, &schedule(7, 1));
    run_until(
        &mut members,
        "the leader appends a second event",
        |members| members[0].module.timer_counts().0.events == 2,
    );
    end = run_until_settled(&mut members, end + TIMER_EVENT_FRAME);
    assert_eq!(end, base + 2 * FRAME + 2 * TIMER_EVENT_FRAME);

    // A cancel timer removes a timer with its mark.
    send(&mut members, &Message::CancelTimer { correlation_id: 7 });
    run_until(&mut members, "the leader holds one timer", |members| {
        members[0].module.timer_counts().1 == 1
    });
    // A timer scheduled for later and then for now is due by its last deadline.
    send(&mut members, &schedule(9, 2));
    run_until(
        &mut members,
        "the leader appends a third event",
        |members| members[0].module.timer_counts().0.events == 3,
    );
    send(&mut members, &Message::CancelTimer { correlation_id: 9 });
    run_until(&mut members, "the leader holds no timer", |members| {
        members[0].module.timer_counts().1 == 0
    });
    end = run_until_settled(&mut members, end + TIMER_EVENT_FRAME);
    assert_eq!(end, base + 2 * FRAME + 3 * TIMER_EVENT_FRAME);
    assert_eq!(counter(&members[0], CLUSTER_TIMER_EVENTS.0), 3);
    assert_eq!(counter(&members[0], CLUSTER_TIMERS.0), 0);
    assert_eq!(counter(&members[0], CLUSTER_TIMERS_REPLACED.0), 0);
    assert_eq!(counter(&members[0], CLUSTER_TIMERS_CLEARED.0), 0);

    // A follower holds what it is told and appends nothing: its timers wait until
    // it leads.
    let mut beside_follower =
        Cavalry::connect_to(&members[1].dir).expect("a client of a follower's driver");
    let mut follower_stream = beside_follower
        .add_exclusive_publication("cavalry:ipc", CONSENSUS_MODULE_STREAM_ID)
        .expect("a publication on the follower's consensus module stream");
    let mut bytes = [0u8; 64];
    let length = schedule(5, 1).write(&mut bytes).expect("a message");
    run_until(
        &mut members,
        "the follower's stream takes a message",
        |_| follower_stream.offer(&bytes[..length]).is_ok(),
    );
    run_until(&mut members, "the follower holds the timer", |members| {
        members[1].module.timer_counts().1 == 1
    });
    offer_entries(&mut members, 4, 2);
    end = run_until_settled(&mut members, end + 2 * FRAME);
    assert_eq!(
        end,
        base + 4 * FRAME + 3 * TIMER_EVENT_FRAME,
        "no event of a follower's"
    );
    assert_eq!(
        members[1].module.timer_counts(),
        (TimerCounts::default(), 1)
    );
    assert_eq!(counter(&members[1], CLUSTER_TIMERS.0), 1);
    assert_eq!(counter(&members[1], CLUSTER_TIMER_EVENTS.0), 0);

    // At its bound, with no timer in its table that it has marked, a member removes
    // every timer for a schedule timer of another id, keeps that one, and counts it.
    // The follower marks none: it holds timer 5 and is told of 2,047 more.
    let tell = |members: &mut [Member], stream: &mut Publication, message: &Message<'_>| {
        let mut bytes = [0u8; 64];
        let length = message.write(&mut bytes).expect("a message");
        run_until(
            members,
            "the consensus module stream takes a message",
            |_| stream.offer(&bytes[..length]).is_ok(),
        );
    };
    let more = MAX_MEMBER_TIMERS as i64 - 1;
    for id in 0..more {
        tell(
            &mut members,
            &mut follower_stream,
            &schedule(1_000 + id, far),
        );
    }
    run_until(&mut members, "the follower's table is full", |members| {
        members[1].module.timer_counts().1 == MAX_MEMBER_TIMERS
    });
    assert_eq!(members[1].module.timer_counts().0, TimerCounts::default());
    tell(&mut members, &mut follower_stream, &schedule(9_000, far));
    run_until(&mut members, "the follower clears its table", |members| {
        members[1].module.timer_counts().0.cleared == 1
    });
    assert_eq!(
        members[1].module.timer_counts(),
        (
            TimerCounts {
                cleared: 1,
                ..TimerCounts::default()
            },
            1
        ),
        "the one it was sent is the one it holds"
    );
    assert_eq!(counter(&members[1], CLUSTER_TIMERS.0), 1);
    assert_eq!(counter(&members[1], CLUSTER_TIMERS_CLEARED.0), 1);
    assert_eq!(counter(&members[1], CLUSTER_TIMERS_REPLACED.0), 0);

    // At its bound with a marked timer in its table, a member gives that one up for
    // a schedule timer of another id, and counts it. The leader appends an event
    // for timer 1, which marks it, and is told of 2,047 more that are not due.
    tell(&mut members, &mut stream, &schedule(1, 1));
    run_until(
        &mut members,
        "the leader appends a fourth event",
        |members| members[0].module.timer_counts().0.events == 4,
    );
    end = run_until_settled(&mut members, end + TIMER_EVENT_FRAME);
    assert_eq!(end, base + 4 * FRAME + 4 * TIMER_EVENT_FRAME);
    for id in 0..more {
        tell(&mut members, &mut stream, &schedule(2_000 + id, far));
    }
    run_until(&mut members, "the leader's table is full", |members| {
        members[0].module.timer_counts().1 == MAX_MEMBER_TIMERS
    });
    tell(&mut members, &mut stream, &schedule(9_001, far));
    run_until(
        &mut members,
        "the leader gives its marked timer up",
        |members| members[0].module.timer_counts().0.replaced == 1,
    );
    assert_eq!(
        members[0].module.timer_counts(),
        (
            TimerCounts {
                events: 4,
                replaced: 1,
                cleared: 0,
            },
            MAX_MEMBER_TIMERS
        ),
        "the table stays full, and no timer in it is due"
    );
    assert_eq!(
        counter(&members[0], CLUSTER_TIMERS.0),
        MAX_MEMBER_TIMERS as i64
    );
    assert_eq!(counter(&members[0], CLUSTER_TIMERS_REPLACED.0), 1);
    assert_eq!(counter(&members[0], CLUSTER_TIMERS_CLEARED.0), 0);
    assert_eq!(counter(&members[0], CLUSTER_TIMER_EVENTS.0), 4);
    assert_eq!(
        run_until_settled(&mut members, end),
        end,
        "no event for a timer that is not due"
    );

    // No message here was rejected: the stream carries all three.
    assert_eq!(counter(&members[0], CLUSTER_MESSAGES_REJECTED.0), 0);
    assert_eq!(counter(&members[1], CLUSTER_MESSAGES_REJECTED.0), 0);

    drop(stream);
    client.close().expect("the client closes");
    drop(follower_stream);
    beside_follower.close().expect("the client closes");
    for member in members {
        stop(member);
    }
}

/// A session message that schedules the timer `id` for `after_ms` past the entry's
/// own time, as the services of these tests read it.
fn schedule_timer(id: i64, after_ms: i64) -> Vec<u8> {
    let mut payload = vec![b'T'];
    payload.extend_from_slice(&id.to_le_bytes());
    payload.extend_from_slice(&after_ms.to_le_bytes());
    payload
}

/// Offers the leader, `members[0]`, one payload, through back pressure.
fn offer_payload(members: &mut [Member], payload: &[u8]) {
    loop {
        match members[0].module.offer(payload) {
            Ok(()) => break,
            Err(RaftError::Offer(Refused::BackPressured)) => {
                for member in members.iter_mut() {
                    member.module.do_work().expect("the member's duty cycle");
                }
            }
            Err(other) => panic!("{other}"),
        }
    }
}

/// A timer that a program schedules is handed to it once, at one position beside
/// every member, with nothing between the program and the cluster but the library
/// and the members (`spec/cluster.md`, "The service", "Timers"; ADR 0151): the
/// service tells its member, the leader appends the event when the timer is due,
/// and every member's service is handed it. A service that starts from a snapshot
/// holds the timers that were pending at it, and is handed one of them where every
/// other service is. And a timer that is pending when its leader stops is handed
/// over under the next leader, whose member was told of it by its own service.
#[test]
fn a_timer_is_handed_over_once_at_one_position_across_a_snapshot_and_a_leaders_stop() {
    let dir = TestDir::new("timers-through");
    let members_string = members_string();
    let mut members: Vec<Member> = [1, 0, 2]
        .into_iter()
        .map(|id| start_member(dir.path(), &members_string, id))
        .collect();
    run_until_led(&mut members, -1);
    let mut services: Vec<Service> = members.iter().map(Service::beside).collect();

    // A timer due in 200 ms, and one due in 6 s.
    offer_payload(&mut members, &schedule_timer(7, 200));
    offer_payload(&mut members, &schedule_timer(9, 6_000));
    run_with_services(
        &mut members,
        &mut services,
        "every service is handed timer 7",
        |_, services| services.iter().all(|s| !s.handed.is_empty()),
    );
    let first = services[0].handed.clone();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].1, 7);
    for service in &services {
        assert_eq!(service.handed, first, "member {}", service.id);
    }
    // The leader appended one event, and no follower any.
    assert_eq!(members[0].module.timer_counts().0.events, 1);
    for member in &members[1..] {
        assert_eq!(member.module.timer_counts().0.events, 0);
    }

    // A snapshot while timer 9 is pending, and a service that starts from it beside
    // a follower: it holds the timer without having read the entry that scheduled
    // it, and is handed its event where the others are.
    let position = snapshot_with_services(&mut members, &mut services);
    let follower = services.remove(2);
    let follower_id = follower.id;
    follower.close();
    let beside = members
        .iter()
        .position(|m| m.module.member_id() == follower_id)
        .expect("the follower");
    let (again, restored) = Service::restored_beside(&mut members, beside);
    assert!(matches!(restored, Restored::State(_)), "{restored:?}");
    assert_eq!(again.service.position(), position);
    services.push(again);
    run_with_services(
        &mut members,
        &mut services,
        "every service is handed timer 9",
        |_, services| {
            services
                .iter()
                .all(|s| s.handed.iter().any(|(_, id)| *id == 9))
        },
    );
    let ninth = *services[0]
        .handed
        .iter()
        .find(|(_, id)| *id == 9)
        .expect("timer 9");
    assert!(ninth.0 >= position, "after the snapshot");
    for service in &services[..2] {
        assert_eq!(service.handed, [first[0], ninth], "member {}", service.id);
    }
    assert_eq!(
        services[2].handed,
        [ninth],
        "the service that started from the snapshot is handed what follows it"
    );
    // Every program is in one state: the restored one took its from the snapshot.
    assert_eq!(services[0].state, services[1].state);
    assert_eq!(services[0].state, services[2].state);

    // A timer due in 2 s, and the leader stops with its service once every service
    // has taken the entry that scheduled it. The next leader's member was told of
    // the timer by the service beside it, and appends its event.
    offer_payload(&mut members, &schedule_timer(11, 2_000));
    let end = run_until_settled(&mut members, 0);
    read_to(&mut members, &mut services, end);
    let term = members[0].module.leadership_term_id();
    let leader = members.remove(0);
    let leader_id = leader.module.member_id();
    let leaders = services
        .iter()
        .position(|s| s.id == leader_id)
        .expect("the leader's service");
    services.remove(leaders).close();
    meanwhile(&mut members, || stop(leader));
    run_until_led(&mut members, term);
    run_with_services(
        &mut members,
        &mut services,
        "both services are handed timer 11 under the next leader",
        |_, services| {
            services
                .iter()
                .all(|s| s.handed.iter().any(|(_, id)| *id == 11))
        },
    );
    let eleventh: Vec<(i64, i64)> = services
        .iter()
        .map(|s| *s.handed.iter().find(|(_, id)| *id == 11).expect("timer 11"))
        .collect();
    assert_eq!(eleventh[0], eleventh[1], "one position beside both members");
    for service in &services {
        assert_eq!(
            service.handed.iter().filter(|(_, id)| *id == 11).count(),
            1,
            "member {}",
            service.id
        );
    }
    assert_eq!(
        members[0].module.timer_counts().0.events,
        1,
        "the next leader appended the event"
    );

    for service in services {
        service.close();
    }
    for member in members {
        stop(member);
    }
}
