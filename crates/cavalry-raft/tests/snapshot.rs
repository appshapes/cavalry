//! Three members over three drivers on the loopback take the snapshot their leader
//! asks for (`spec/cluster.md`, "Snapshots"; ADR 0129): every member's consensus module
//! writes its state at the position after the cluster action, to a recording of its
//! own, and lists it in its snapshots file (`spec/durable-log.md`, "Snapshots"; ADR
//! 0128). The tests hold the snapshot to invariant 1: the three members' snapshots are
//! of one position and hold the same bytes, and a member that reads the action again,
//! after it started again on the log it kept, takes no second snapshot of it.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cavalry_client::{
    Action, Cavalry, ClusterError, ClusterService, Entry, NULL_LENGTH, NULL_POSITION, Publication,
    SnapshotCause, Subscription,
};
use cavalry_driver::Driver;
use cavalry_log::recordings::RecordingsFile;
use cavalry_log::snapshots::{CONSENSUS_MODULE, ListedSnapshot, SnapshotsFile};
use cavalry_proto::cluster::{
    CLOSE_REASON_NONE, CONSENSUS_MODULE_STREAM_ID, EGRESS_STREAM_ID, INGRESS_STREAM_ID,
    MAX_SERVICE_SNAPSHOT_STATE_LENGTH, Message, SERVICE_SNAPSHOT_TYPE_ID, SNAPSHOT_TYPE_ID,
    SnapshotMark, Stream,
};
use cavalry_raft::{
    ConsensusModule, ElectionState, RaftError, Refused, Role, ServiceSnapshotCounts, SnapshotCounts,
};
use cavalry_shm::counters::CountersReader;
use cavalry_shm::counters::generated::{
    CLUSTER_MESSAGES_REJECTED, CLUSTER_SERVICE_ACKS_REJECTED, CLUSTER_SERVICE_SNAPSHOT_POSITION,
    CLUSTER_SERVICE_SNAPSHOTS_FAILED, CLUSTER_SERVICE_SNAPSHOTS_LISTED,
    CLUSTER_SERVICE_SNAPSHOTS_TAKEN, CLUSTER_SESSIONS, CLUSTER_SESSIONS_ACCEPTED,
    CLUSTER_SESSIONS_CLOSED, CLUSTER_SESSIONS_OPENED, CLUSTER_SNAPSHOT_ACTIONS,
    CLUSTER_SNAPSHOT_POSITION, CLUSTER_SNAPSHOT_START_POSITION, CLUSTER_SNAPSHOT_STEPS,
    CLUSTER_SNAPSHOTS_BEGUN, CLUSTER_SNAPSHOTS_FAILED, CLUSTER_SNAPSHOTS_PASSED_OVER,
    CLUSTER_SNAPSHOTS_TAKEN,
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
    let driver = Driver::start(&driver_dir, &log_dir).expect("the driver starts");
    let module = ConsensusModule::start(&driver_dir, &log_dir, members, id)
        .expect("the member starts, on a new log directory or on the one it kept");
    Member {
        driver,
        module,
        dir: driver_dir,
        log_dir,
    }
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
                .map(|m| format!("{:?} {:?}", m.module, m.module.snapshot_counts()))
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
                    for member in members.iter_mut() {
                        member.module.do_work().expect("the member's duty cycle");
                    }
                }
                Err(other) => panic!("{other}"),
            }
        }
    }
}

/// The leader, `members[0]`, asks for a snapshot, a back-pressured ask made again once
/// every member has run.
fn ask_snapshot(members: &mut [Member]) {
    loop {
        match members[0].module.snapshot() {
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

/// The messages a snapshot's recording holds, each as its bytes: the recording played
/// back whole through a client of the member's driver.
fn replayed(member: &Member, recording_id: i64) -> Vec<Vec<u8>> {
    let mut client = Cavalry::connect_to(&member.dir).expect("a client of the member's driver");
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
        let messages = replayed(member, snapshot.recording_id);
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
    stopped.module.close().expect("the member closes");
    stopped.driver.close().expect("the driver closes");
    let again = start_member(dir.path(), &members_string, id);
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
        let driver = Driver::start(&driver_dir, dir.join(format!("{name}-log")))
            .expect("the client's driver");
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

    fn close(self) {
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
        let mut client = Client::start(
            dir.path(),
            &format!("client-{index}"),
            ports[15 + index],
            &ingress,
            correlation_id,
        );
        let mut sent = false;
        run_until(&mut members, "the client's session opens", |_| {
            sent = sent || client.connect();
            client.poll();
            client.session_id.is_some()
        });
        assert_eq!(client.session_id, Some(index as i64));
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
    for member in &members {
        let id = member.module.member_id();
        let list = listed(member);
        assert_eq!(list.len(), 1, "member {id}: {list:?}");
        let messages = replayed(member, list[0].recording_id);
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
    let mut first = Client::start(dir.path(), "client-0", ports[15], &ingress, 41);
    let mut sent = false;
    run_until(&mut members, "the first client's session opens", |_| {
        sent = sent || first.connect();
        first.poll();
        first.session_id.is_some()
    });
    assert_eq!(first.session_id, Some(0));
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
    let mut second = Client::start(dir.path(), "client-1", ports[16], &ingress, 42);
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
    for member in &members {
        let id = member.module.member_id();
        let list = listed(member);
        assert_eq!(list.len(), 1, "member {id}: {list:?}");
        let messages = replayed(member, list[0].recording_id);
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
/// then ends.
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
        let mut client = Client::start(
            dir.path(),
            &format!("client-{index}"),
            ports[15 + index],
            &ingress,
            correlation_id,
        );
        let mut sent = false;
        run_until(&mut members, "the client's session opens", |_| {
            sent = sent || client.connect();
            client.poll();
            client.session_id.is_some()
        });
        assert_eq!(client.session_id, Some(index as i64));
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
    member.module.close().expect("the member closes");
    member.driver.close().expect("the driver closes");
    id
}

/// Purges `recording_id` from the log directory of stopped member `id`, as an
/// operator does: a driver on the member's directories, and the client's command.
fn purge(dir: &Path, id: i32, recording_id: i64) {
    let driver_dir = dir.join(format!("member-{id}"));
    let log_dir = dir.join(format!("member-{id}-log"));
    let driver =
        Driver::start(&driver_dir, &log_dir).expect("a driver on the member's directories");
    let mut client = Cavalry::connect_to(&driver_dir).expect("a client of it");
    client
        .purge_recording(recording_id)
        .unwrap_or_else(|error| panic!("recording {recording_id} is purged: {error}"));
    client.close().expect("the client closes");
    driver.close().expect("the driver closes");
}

/// The bytes of the newest snapshot each member lists, which must be of `position`.
fn newest_bytes(members: &[Member], position: i64) -> Vec<Vec<Vec<u8>>> {
    members
        .iter()
        .map(|member| {
            let list = listed(member);
            let newest = list.last().expect("a listed snapshot");
            assert_eq!(
                newest.log_position,
                position,
                "member {}",
                member.module.member_id()
            );
            replayed(member, newest.recording_id)
        })
        .collect()
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
    let id = stop(members.pop().expect("a follower"));
    let again = start_member(dir.path(), &members_string, id);
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
    let bytes = newest_bytes(&members, second);
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
    let old_leader = stop(members.remove(0));
    run_until_led(&mut members, first_term);
    members.push(start_member(dir.path(), &members_string, old_leader));
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
    let id = stop(stopped);
    let record = RecordingsFile::open_kept(&log_dir, id).expect("the member's record of its log");
    let recordings: Vec<(i64, i64)> = record
        .entries()
        .iter()
        .map(|entry| (entry.recording_id, entry.start_position))
        .collect();
    drop(record);
    assert!(recordings.len() >= 2, "{recordings:?}");
    assert_eq!(recordings[0].1, 0, "{recordings:?}");
    let (kept_id, kept_start) = recordings[1];
    assert!(
        kept_start > 0 && kept_start <= position,
        "the second recording begins at {kept_start}, the snapshot is of {position}"
    );
    // An operator purges the first recording, which the snapshot covers.
    purge(dir.path(), id, recordings[0].0);

    // The member starts: from its snapshot, on the log that is left.
    let again = start_member(dir.path(), &members_string, id);
    assert_eq!(again.module.session_log_position(), position);
    assert_eq!(counter(&again, CLUSTER_SNAPSHOT_START_POSITION.0), position);
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
    let bytes = newest_bytes(&members, second);
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
    let id = stop(member);
    for recording_id in snapshots {
        purge(dir.path(), id, recording_id);
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
    let id = stop(stopped);
    purge(dir.path(), id, list[1].recording_id);

    // It starts from the snapshot before, says that it passed one over, and reads
    // the log from there: the second action among its entries, for which it takes
    // no snapshot, since its list still names one at that position.
    let again = start_member(dir.path(), &members_string, id);
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
    let id = stop(stopped);
    purge(dir.path(), id, list[0].recording_id);
    let again = start_member(dir.path(), &members_string, id);
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
    client: Cavalry,
    service: ClusterService,
    state: Vec<u8>,
    /// Whether the program gives its state when a snapshot is due.
    gives: bool,
    /// What each call of `snapshot` answered, with the position it was for.
    taken: Vec<(i64, bool)>,
}

impl Service {
    fn beside(member: &Member) -> Self {
        let client = Cavalry::connect_to(&member.dir).expect("the service's client");
        let service = client.cluster_service();
        Self {
            client,
            service,
            state: Vec::new(),
            gives: true,
            taken: Vec::new(),
        }
    }

    /// One poll: the program adds each session message's payload to its state.
    fn poll(&mut self) -> usize {
        let state = &mut self.state;
        self.service
            .poll(
                &mut self.client,
                |committed, _| {
                    if let Entry::SessionMessage { payload, .. } = committed.entry {
                        state.extend_from_slice(payload);
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
        let taken = self
            .service
            .snapshot(&mut self.client, &self.state)
            .expect("the service's snapshot");
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
/// The members and the services share this one thread, where each has a thread of
/// its own outside a test: a service's `snapshot` waits for the driver, and a
/// member whose duty cycle waited behind three of them would give its own snapshot
/// up at the snapshot timeout.
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
            "{what}: {:?}",
            members
                .iter()
                .map(|m| format!("{:?} {:?}", m.module, m.module.service_snapshot_counts()))
                .collect::<Vec<_>>()
        );
        let mut work = 0;
        for member in members.iter_mut() {
            work += member.module.do_work().expect("the member's duty cycle");
        }
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
                        service.give(position);
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
        recordings.push(replayed(member, services.recording_id));
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
            "of a position that is no entry's end",
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
