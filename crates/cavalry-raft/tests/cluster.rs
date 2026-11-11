//! Three members over three drivers on the loopback: each a [`ConsensusModule`], a
//! client of its own driver, with its log recorded into its own log directory (ADR
//! 0060; ADR 0063). The appointed leader leads, the followers arm their recordings
//! before its first frame, a hundred thousand entries are committed at a quorum of synced
//! positions, and the three recordings hold the same bytes. A member is lost, a follower
//! or the leader, and a follower leaves the log and joins it again: what a member that
//! leaves the log holds as its log's end is where the recording it held stopped.

use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use cavalry_client::{Cavalry, ClientError, CommittedLog};
use cavalry_driver::Driver;
use cavalry_log::catalog::CatalogReader;
use cavalry_proto::cluster::{Message, SERVICE_LOG_STREAM_ID, Stream};
use cavalry_raft::{ConsensusModule, ElectionState, NO_SESSION, RaftError, Refused, Role};
use cavalry_shm::counters::CountersReader;
use cavalry_shm::counters::generated::{
    CLUSTER_APPEND_POSITION, CLUSTER_COMMIT_POSITION, CLUSTER_ELECTION_STATE,
    CLUSTER_ELECTION_STEPS, CLUSTER_ELECTIONS, CLUSTER_ELECTIONS_CLOSED,
    CLUSTER_LEADERSHIP_TERM_ID, CLUSTER_LOG_RECORDING_ID, CLUSTER_ROLE, CLUSTER_TERMS_LED,
    SUBSCRIPTION_POSITION,
};

struct TestDir(PathBuf);

impl TestDir {
    fn new(test: &str) -> Self {
        let path = std::env::temp_dir().join(format!("cavalry-raft-{}-{test}", std::process::id()));
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

/// Loopback ports for a cluster's endpoints, from 20000 to 31999: below the range the
/// operating system gives a socket bound to port 0, which starts at 32768 on Linux and
/// at 49152 on macOS. A follower binds its `log` endpoint again when it leaves the log
/// and joins it again (ADR 0068). While the endpoint is closed, a port from that range
/// can be given to any socket bound to port 0: a member's send socket, or a socket of
/// another test in this process. The bind then fails with `Address already in use`,
/// code 4, and the member's duty cycle fails. Each port is free when it is chosen, and
/// each is chosen once in this process, so that tests that run at once never share
/// one. The process id picks where the choice starts, so that two test processes on
/// one host start apart.
fn free_ports(count: usize) -> Vec<u16> {
    const FIRST: u32 = 20_000;
    const SPAN: u32 = 12_000;
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let start = (std::process::id() % 60) * 200;
    let mut sockets = Vec::with_capacity(count);
    for _ in 0..SPAN {
        if sockets.len() == count {
            break;
        }
        let offset = (start + NEXT.fetch_add(1, Ordering::Relaxed)) % SPAN;
        let port = u16::try_from(FIRST + offset).expect("below 32000");
        if let Ok(socket) = UdpSocket::bind(("127.0.0.1", port)) {
            sockets.push(socket);
        }
    }
    assert_eq!(
        sockets.len(),
        count,
        "free loopback ports from {FIRST} to {}",
        FIRST + SPAN - 1
    );
    sockets
        .iter()
        .map(|socket| socket.local_addr().expect("bound").port())
        .collect()
}

/// The members string of three members on the loopback.
fn members_string() -> String {
    let ports = free_ports(15);
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

fn start(dir: &Path, members: &str) -> Vec<Member> {
    (0..3)
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
                log_dir,
            }
        })
        .collect()
}

/// Drives every member's duty cycle until `done`, or fails with `what`.
fn run_until(members: &mut [Member], what: &str, mut done: impl FnMut(&[Member]) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !done(members) {
        assert!(Instant::now() < deadline, "{what}: {:?}", states(members));
        let mut work = 0;
        for member in members.iter_mut() {
            work += member.module.do_work().expect("the member's duty cycle");
        }
        if work == 0 {
            std::thread::sleep(Duration::from_micros(200));
        }
    }
}

fn states(members: &[Member]) -> Vec<String> {
    members.iter().map(|m| format!("{:?}", m.module)).collect()
}

/// The value of the member's counter of `type_id`, found in its driver's counters file
/// by the type and the member id in the key.
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

#[test]
fn three_members_replicate_a_hundred_thousand_entries_and_their_recordings_are_equal() {
    let dir = TestDir::new("replicate");
    let members_string = members_string();
    let mut members = start(dir.path(), &members_string);

    // Before a duty cycle has run, a member has no term and names no recording: the
    // counters hold −1 from their add, never the 0 of their allocation, which is a
    // term and a recording id.
    for member in &members {
        assert_eq!(counter(member, CLUSTER_LEADERSHIP_TERM_ID.0), -1);
        assert_eq!(counter(member, CLUSTER_LOG_RECORDING_ID.0), -1);
        assert_eq!(counter(member, CLUSTER_COMMIT_POSITION.0), 0);
    }

    run_until(&mut members, "every election closed", |members| {
        members
            .iter()
            .all(|m| m.module.election_state() == ElectionState::Closed)
    });
    assert_eq!(members[0].module.role(), Role::Leader);
    for member in &members {
        assert_eq!(member.module.leadership_term_id(), 0);
        assert_eq!(member.module.counts().elections, 1);
        assert_eq!(member.module.counts().elections_failed, 0);
        assert_eq!(member.module.counts().join_position_mismatches, 0);
    }
    assert_eq!(members[1].module.role(), Role::Follower);
    assert_eq!(members[2].module.role(), Role::Follower);
    assert!(matches!(
        members[1].module.offer(b"not the leader's"),
        Err(RaftError::Offer(Refused::NotLeader))
    ));
    assert!(matches!(
        members[0].module.offer(&[0u8; 1345]),
        Err(RaftError::PayloadTooLong { length: 1345 })
    ));

    // A hundred thousand entries from the leader's own process, a back-pressured offer
    // made again once the members have run, and the members run every hundred offers
    // as they would between a program's own.
    const ENTRIES: u32 = 100_000;
    for i in 0..ENTRIES {
        let payload = [i.to_le_bytes(), (!i).to_le_bytes()].concat();
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
        if i % 100 == 99 {
            for member in members.iter_mut() {
                member.module.do_work().expect("the member's duty cycle");
            }
        }
    }
    // The term event, 56 bytes in a frame of 96, and a hundred thousand entries of 40
    // bytes, each in a frame of 96 (`spec/cluster.md`, "The log").
    let end = 96 + i64::from(ENTRIES) * 96;
    run_until(
        &mut members,
        "every member committed every entry",
        |members| {
            members
                .iter()
                .all(|m| m.module.commit_position() == end && m.module.append_position() == end)
        },
    );

    // Each member's recording holds the same bytes up to the commit position: the
    // leader's of its publication and each follower's of the leader's image.
    let recordings: Vec<Vec<u8>> = members
        .iter()
        .map(|member| {
            let id = member
                .module
                .log_recording_id()
                .expect("a recording of the log");
            let file = member.log_dir.join(format!("{id}-0.rec"));
            let bytes = std::fs::read(&file).expect("the segment file is read");
            bytes[..usize::try_from(end).expect("a position")].to_vec()
        })
        .collect();
    assert_eq!(recordings[1], recordings[0], "member 1's recording");
    assert_eq!(recordings[2], recordings[0], "member 2's recording");
    // The first entry is the term's event and the last is the hundred thousandth offer.
    let first = Message::parse(Stream::Log, &recordings[0][32..32 + 56]).expect("an entry");
    assert!(matches!(
        first,
        Message::NewLeadershipTermEvent {
            leadership_term_id: 0,
            log_position: 0,
            leader_member_id: 0,
            ..
        }
    ));
    let last_frame = &recordings[0][recordings[0].len() - 96..];
    let last = Message::parse(Stream::Log, &last_frame[32..32 + 40]).expect("an entry");
    let expected = [(ENTRIES - 1).to_le_bytes(), (!(ENTRIES - 1)).to_le_bytes()].concat();
    assert!(matches!(
        last,
        Message::SessionMessage {
            leadership_term_id: 0,
            cluster_session_id: NO_SESSION,
            payload,
            ..
        } if payload == expected
    ));

    // What each member counted is in its driver's counters file, where `cavalry doctor`
    // reads it.
    for (index, member) in members.iter().enumerate() {
        assert_eq!(counter(member, CLUSTER_COMMIT_POSITION.0), end);
        // The recording a service replays: the leader's of its publication, each
        // follower's of the leader's image (`spec/cluster.md`, "The service").
        assert_eq!(
            Some(counter(member, CLUSTER_LOG_RECORDING_ID.0)),
            member.module.log_recording_id()
        );
        assert_eq!(counter(member, CLUSTER_LEADERSHIP_TERM_ID.0), 0);
        assert_eq!(counter(member, CLUSTER_ELECTION_STATE.0), 17);
        assert_eq!(counter(member, CLUSTER_ELECTIONS.0), 1);
        assert_eq!(
            counter(member, CLUSTER_ROLE.0),
            if index == 0 { 2 } else { 0 }
        );
        assert_eq!(counter(member, CLUSTER_TERMS_LED.0), i64::from(index == 0));
        // The two counts of ADR 0067 reach the file as the others do, through
        // `Counts::by_counter`: one election closed, in three steps on the leader
        // and four on a follower.
        assert_eq!(counter(member, CLUSTER_ELECTIONS_CLOSED.0), 1);
        assert_eq!(
            counter(member, CLUSTER_ELECTION_STEPS.0),
            if index == 0 { 3 } else { 4 }
        );
    }

    // A member does not start again on the log it has: the refusal says why and what
    // to do.
    let Member {
        driver,
        module,
        dir: driver_dir,
        log_dir,
    } = members.remove(2);
    module.close().expect("the member closes");
    let again = ConsensusModule::start(&driver_dir, &log_dir, &members_string, 2)
        .expect_err("a log directory that holds a term");
    assert!(matches!(
        again,
        RaftError::Restart {
            log_leadership_term_id: 0,
            candidate_term: -1,
        }
    ));
    let text = again.to_string();
    assert!(
        text.contains("every member on a new log directory"),
        "{text}"
    );
    assert!(
        text.contains("docs/reference/cluster.md#roles-and-the-election"),
        "{text}"
    );
    driver.close().expect("the driver closes");

    for member in members {
        member.module.close().expect("the member closes");
        member.driver.close().expect("the driver closes");
    }
}

/// A commit waits for one sync on a quorum, not for the recorders' next second
/// (`spec/cluster.md`, "The log"; ADR 0060, ADR 0069). Ten entries are appended one
/// after another, each once every member has committed the one before. A recorder
/// syncs a recording by itself once a second, counted from its last sync, and the
/// leader's own append position bounds the commit position, so without the members'
/// sync requests two commits are at least a second apart and the ten take nine
/// seconds or more. The test allows five, and prints what they took.
#[test]
fn ten_entries_each_appended_once_the_last_was_committed_do_not_wait_a_second_each() {
    let dir = TestDir::new("commit-on-sync");
    let members_string = members_string();
    let mut members = start(dir.path(), &members_string);
    run_until(&mut members, "every election closed", |members| {
        members
            .iter()
            .all(|m| m.module.election_state() == ElectionState::Closed)
    });
    // The term event, 56 bytes in a frame of 96, then each entry in a frame of 96.
    let mut end = 96;
    run_until(
        &mut members,
        "the term's first entry committed",
        |members| members.iter().all(|m| m.module.commit_position() == end),
    );

    const ENTRIES: u32 = 10;
    let started = Instant::now();
    for i in 0..ENTRIES {
        offer(&mut members, &[0, 1, 2], i, 1);
        end += 96;
        run_until(
            &mut members,
            "every member committed the entry",
            |members| members.iter().all(|m| m.module.commit_position() == end),
        );
    }
    let took = started.elapsed();
    println!("{ENTRIES} entries, each appended once the last was committed: {took:?}");
    assert!(
        took < Duration::from_secs(5),
        "{ENTRIES} commits one after another took {took:?}: each waited for a recorder's \
         sync of its own, once a second, so the members' sync requests are not reaching \
         their recorders"
    );
    // A quorum committed each entry; the member outside it syncs the entry a moment
    // later, and its append position follows.
    run_until(&mut members, "every member recorded every entry", |members| {
        members.iter().all(|m| m.module.append_position() == end)
    });
}

/// A follower's host is lost while the cluster runs: its module and its driver stop
/// at once. The leader and the other follower are a quorum, so entries offered after
/// it are committed all the same, once the leader's log is no longer held back by the
/// receiver that went silent; the leader never loses its quorum.
#[test]
fn a_follower_lost_leaves_a_quorum_that_goes_on_committing() {
    let dir = TestDir::new("follower-lost");
    let members_string = members_string();
    let mut members = start(dir.path(), &members_string);
    run_until(&mut members, "every election closed", |members| {
        members
            .iter()
            .all(|m| m.module.election_state() == ElectionState::Closed)
    });
    members[0]
        .module
        .offer(b"three of three")
        .expect("the leader appends");
    let before = 2 * 96;
    run_until(
        &mut members,
        "the first entry committed on all three",
        |members| members.iter().all(|m| m.module.commit_position() == before),
    );

    let lost = members.remove(2);
    drop(lost.module);
    lost.driver.close().expect("the driver closes");

    const ENTRIES: i64 = 100;
    for i in 0..ENTRIES {
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
    let end = before + ENTRIES * 96;
    run_until(
        &mut members,
        "the two committed what the leader appended",
        |members| members.iter().all(|m| m.module.commit_position() == end),
    );
    assert_eq!(members[0].module.role(), Role::Leader);
    assert_eq!(members[0].module.counts().leader_quorum_losses, 0);
    assert_eq!(members[1].module.counts().leader_heartbeat_timeouts, 0);
    assert_eq!(members[1].module.counts().log_images_lost, 0);

    for member in members {
        member.module.close().expect("the member closes");
        member.driver.close().expect("the driver closes");
    }
}

/// The leader's host is lost once entries are committed: its module and its driver
/// stop at once. Each follower finds its image of the log gone, or its leader silent
/// for the leader heartbeat timeout, begins its election again at init and leaves the
/// log, and canvasses with no appointed leader to answer it. The recording it held is
/// stopped, by its own stop or by the driver's at the image's end, whichever came
/// first, at the committed end: that is the append position it reports, and it names
/// no recording in `cluster.log_recording_id`, since none records the live log
/// (`spec/cluster.md`, "The service"). The next test is the one where the member's own
/// stop alone ends its recording, and where the end it takes decides what it appends
/// after.
#[test]
fn a_leader_lost_leaves_each_follower_at_the_end_its_recording_stopped() {
    let dir = TestDir::new("leader-lost");
    let members_string = members_string();
    let mut members = start(dir.path(), &members_string);
    run_until(&mut members, "every election closed", |members| {
        members
            .iter()
            .all(|m| m.module.election_state() == ElectionState::Closed)
    });
    const ENTRIES: i64 = 100;
    offer_entries(&mut members, ENTRIES);
    // The term's event and a hundred entries, each in a frame of 96.
    let end = 96 + ENTRIES * 96;
    run_until(
        &mut members,
        "every member committed every entry",
        |members| {
            members
                .iter()
                .all(|m| m.module.commit_position() == end && m.module.append_position() == end)
        },
    );
    // Each follower holds the recording of the leader's image it armed, and names it.
    let held: Vec<i64> = members[1..]
        .iter()
        .map(|member| {
            let id = member
                .module
                .log_recording_id()
                .expect("a follower's recording of the log");
            assert_eq!(counter(member, CLUSTER_LOG_RECORDING_ID.0), id);
            id
        })
        .collect();

    let leader = members.remove(0);
    drop(leader.module);
    leader.driver.close().expect("the driver closes");

    run_until(
        &mut members,
        "each follower left the log and canvasses again",
        |members| {
            members.iter().all(|m| {
                let counts = m.module.counts();
                counts.log_images_lost + counts.leader_heartbeat_timeouts == 1
                    && m.module.election_state() == ElectionState::Canvass
            })
        },
    );
    for (member, recording_id) in members.iter().zip(held) {
        assert_eq!(member.module.role(), Role::Follower);
        assert_eq!(member.module.counts().elections, 2);
        assert_eq!(member.module.counts().elections_failed, 0);
        assert_eq!(counter(member, CLUSTER_ELECTIONS.0), 2);
        // Its log ends where its recording stopped, and its commit position stands.
        assert_eq!(member.module.append_position(), end);
        assert_eq!(counter(member, CLUSTER_APPEND_POSITION.0), end);
        assert_eq!(member.module.commit_position(), end);
        assert_eq!(counter(member, CLUSTER_COMMIT_POSITION.0), end);
        // The recording it held is stopped at that end, and it names none now.
        let catalog = CatalogReader::open(&member.log_dir).expect("the catalog opens");
        assert_eq!(
            catalog
                .stop_position(recording_id)
                .expect("the recording's slot is read"),
            Some(end),
            "member {}'s recording {recording_id}",
            member.module.member_id()
        );
        assert_eq!(member.module.log_recording_id(), None);
        assert_eq!(counter(member, CLUSTER_LOG_RECORDING_ID.0), -1);
    }

    for member in members {
        member.module.close().expect("the member closes");
        member.driver.close().expect("the driver closes");
    }
}

/// A leave whose stop position was not read in time is an error that names the
/// recording held, what the operator does next and where the leave is explained.
/// No test here makes a stop fail on a running driver, which takes a seam the client
/// and the driver do not have; this is the text such a leave returns.
#[test]
fn a_stop_timeout_says_what_to_do_next() {
    let error = RaftError::StopTimeout { recording_id: 7 };
    assert_eq!(error.reason(), "stop-timeout");
    let text = error.to_string();
    assert!(text.contains("recording 7"), "{text}");
    assert!(text.contains("holding the recording"), "{text}");
    assert!(text.contains("Call do_work again"), "{text}");
    assert!(text.contains("cavalry doctor"), "{text}");
    assert!(
        text.contains("docs/reference/cluster.md#roles-and-the-election"),
        "{text}"
    );
}

/// Offers `count` entries on the leader, `members[0]`, each made again once every
/// member has run while the log is back-pressured.
fn offer_entries(members: &mut [Member], count: i64) {
    for i in 0..count {
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

/// The leader's module stands still for longer than the leader heartbeat timeout while
/// its driver runs on, so that each follower's image of the log stays available and the
/// stop of the recording it holds is its own `leave_log`'s alone. Each follower counts
/// the timeout, leaves the log at the end its recording stopped at, and canvasses; the
/// leader, running again, answers, and each follower joins the log again on a new
/// recording whose image joins at that end. What the leader appends after is committed
/// on all three: a follower's append position goes on from the stop of its first
/// recording through its second, which begins where the member's log ends.
#[test]
fn a_follower_that_leaves_the_log_joins_it_again_where_its_recording_stopped() {
    let dir = TestDir::new("join-again");
    let members_string = members_string();
    let mut members = start(dir.path(), &members_string);
    run_until(&mut members, "every election closed", |members| {
        members
            .iter()
            .all(|m| m.module.election_state() == ElectionState::Closed)
    });
    const ENTRIES: i64 = 10;
    offer_entries(&mut members, ENTRIES);
    let first = 96 + ENTRIES * 96;
    run_until(&mut members, "the first entries committed", |members| {
        members
            .iter()
            .all(|m| m.module.commit_position() == first && m.module.append_position() == first)
    });
    let held: Vec<i64> = members[1..]
        .iter()
        .map(|m| m.module.log_recording_id().expect("a follower's recording"))
        .collect();

    // The leader's module stands still; its driver, and so its log's image on each
    // follower, runs on.
    run_until(
        &mut members[1..],
        "each follower timed out its silent leader and canvasses",
        |members| {
            members.iter().all(|m| {
                m.module.counts().leader_heartbeat_timeouts == 1
                    && m.module.election_state() == ElectionState::Canvass
            })
        },
    );
    for (member, recording_id) in members[1..].iter().zip(&held) {
        assert_eq!(member.module.counts().log_images_lost, 0);
        assert_eq!(member.module.log_recording_id(), None);
        assert_eq!(counter(member, CLUSTER_LOG_RECORDING_ID.0), -1);
        assert_eq!(member.module.append_position(), first);
        let catalog = CatalogReader::open(&member.log_dir).expect("the catalog opens");
        assert_eq!(
            catalog
                .stop_position(*recording_id)
                .expect("the recording's slot is read"),
            Some(first),
            "member {}'s first recording",
            member.module.member_id()
        );
    }

    // The leader runs again and answers the followers' canvass; each joins the log on
    // a new recording.
    run_until(
        &mut members,
        "each follower joined the log again",
        |members| {
            members
                .iter()
                .all(|m| m.module.election_state() == ElectionState::Closed)
                && members[1..]
                    .iter()
                    .zip(&held)
                    .all(|(m, first)| m.module.log_recording_id().is_some_and(|id| id != *first))
        },
    );
    offer_entries(&mut members, ENTRIES);
    let second = first + ENTRIES * 96;
    run_until(
        &mut members,
        "the entries after the join committed on all three",
        |members| {
            members.iter().all(|m| {
                m.module.commit_position() == second && m.module.append_position() == second
            })
        },
    );
    for member in &members[1..] {
        let counts = member.module.counts();
        assert_eq!(counts.elections, 2);
        assert_eq!(counts.elections_closed, 2);
        assert_eq!(counts.join_position_mismatches, 0);
        assert_eq!(counter(member, CLUSTER_APPEND_POSITION.0), second);
        assert_eq!(
            Some(counter(member, CLUSTER_LOG_RECORDING_ID.0)),
            member.module.log_recording_id()
        );
    }

    for member in members {
        member.module.close().expect("the member closes");
        member.driver.close().expect("the driver closes");
    }
}

/// What a service read: each entry's position and its bytes.
type Read = Vec<(i64, Vec<u8>)>;

/// Polls `log` once into `read`, and holds the poll to the service's rule: every entry
/// it was given begins below the commit position the poll took as its bound.
fn poll_log(log: &mut CommittedLog, read: &mut Read) -> usize {
    poll_log_at_most(log, read, 64)
}

/// [`poll_log`] with a fragment limit of the caller's.
fn poll_log_at_most(log: &mut CommittedLog, read: &mut Read, fragment_limit: usize) -> usize {
    let before = read.len();
    let delivered = log
        .poll(
            |entry, _, position| read.push((position, entry.to_vec())),
            fragment_limit,
        )
        .expect("the committed log polls");
    assert_eq!(delivered, read.len() - before);
    for (position, _) in &read[before..] {
        assert!(
            *position < log.commit_position(),
            "an entry at {position} read with a bound of {}",
            log.commit_position()
        );
    }
    if let Some((position, entry)) = read.last() {
        let frame = (32 + i64::try_from(entry.len()).expect("a length") + 31) & !31;
        assert_eq!(log.position(), position + frame);
    }
    delivered
}

/// The leader appends `count` entries numbered from `from`, a back-pressured offer made
/// again once the members that run have run.
fn offer(members: &mut [Member], running: &[usize], from: u32, count: u32) {
    for i in from..from + count {
        let payload = [i.to_le_bytes(), (!i).to_le_bytes()].concat();
        loop {
            match members[0].module.offer(&payload) {
                Ok(()) => break,
                Err(RaftError::Offer(Refused::BackPressured)) => {
                    for &index in running {
                        members[index]
                            .module
                            .do_work()
                            .expect("the member's duty cycle");
                    }
                }
                Err(other) => panic!("{other}"),
            }
        }
    }
}

/// A service beside each member reads the committed log through its member's driver
/// (`spec/cluster.md`, "The service"; ADR 0066): the same entries at the same positions
/// on the leader and on both followers, none past the commit position even when the
/// member's own recording holds more, from any position and at any time, and to the
/// bound it last took once its member is gone.
#[test]
fn a_service_reads_the_committed_log_on_every_member_and_nothing_past_the_commit_position() {
    let dir = TestDir::new("service");
    let members_string = members_string();
    let mut members = start(dir.path(), &members_string);
    let mut clients: Vec<Cavalry> = members
        .iter()
        .map(|member| Cavalry::connect_to(&member.dir).expect("the service's client connects"))
        .collect();

    // A service that starts with its member asks until the member has its log: the
    // members are in canvass, and a follower's recording starts after its election.
    let mut logs: Vec<Option<CommittedLog>> = (0..3).map(|_| None).collect();
    let mut asked_early = 0;
    run_until(&mut members, "every service opened its log", |_| {
        for (log, client) in logs.iter_mut().zip(clients.iter_mut()) {
            if log.is_none() {
                *log = client.committed_log(0).expect("the driver answers");
                asked_early += usize::from(log.is_none());
            }
        }
        logs.iter().all(Option::is_some)
    });
    assert!(
        asked_early > 0,
        "no service asked before its member had a log"
    );
    let mut logs: Vec<CommittedLog> = logs.into_iter().flatten().collect();
    for (index, log) in logs.iter().enumerate() {
        assert_eq!(log.member_id(), i32::try_from(index).expect("an id"));
        assert_eq!(
            Some(log.recording_id()),
            members[index].module.log_recording_id()
        );
        assert_eq!((log.position(), log.commit_position()), (0, 0));
        assert!(!log.is_ended());
    }
    // A member names its recording before the leader's election closes, state 5 on
    // the leader and 15 on a follower, and the leader refuses an offer until it has
    // closed: the offers below wait for every election to close.
    run_until(&mut members, "every election closed", |members| {
        members
            .iter()
            .all(|m| m.module.election_state() == ElectionState::Closed)
    });
    let mut reads: Vec<Read> = vec![Vec::new(); 3];

    // Five hundred entries while every member runs: each service reads the term's
    // first entry and the five hundred, at the positions the leader appended them.
    const FIRST: u32 = 500;
    offer(&mut members, &[0, 1, 2], 0, FIRST);
    let first_end = 96 + i64::from(FIRST) * 96;
    run_until(&mut members, "every service read the first entries", |_| {
        for (log, read) in logs.iter_mut().zip(reads.iter_mut()) {
            poll_log(log, read);
        }
        reads.iter().all(|read| read.len() == FIRST as usize + 1)
    });
    for (index, read) in reads.iter().enumerate() {
        assert_eq!(read, &reads[0], "member {index}'s service");
        assert_eq!(logs[index].position(), first_end);
    }
    for (index, (position, entry)) in reads[0].iter().enumerate() {
        assert_eq!(*position, 96 * i64::try_from(index).expect("an index"));
        let message = Message::parse(Stream::Log, entry).expect("an entry of the log");
        if index == 0 {
            assert!(matches!(
                message,
                Message::NewLeadershipTermEvent {
                    leadership_term_id: 0,
                    log_position: 0,
                    leader_member_id: 0,
                    ..
                }
            ));
        } else {
            let i = u32::try_from(index - 1).expect("an index");
            let expected = [i.to_le_bytes(), (!i).to_le_bytes()].concat();
            assert!(matches!(
                message,
                Message::SessionMessage { cluster_session_id: NO_SESSION, payload, .. }
                    if payload == expected
            ));
        }
    }

    // The followers' consensus modules stop running, so nothing is committed, while
    // the leader appends a hundred entries and its recorder syncs them: the leader's
    // recording holds them, its service's replay has played them, and its service
    // reads none of them.
    const SECOND: u32 = 100;
    offer(&mut members, &[0], FIRST, SECOND);
    let second_end = first_end + i64::from(SECOND) * 96;
    let deadline = Instant::now() + Duration::from_secs(30);
    while members[0].module.append_position() < second_end {
        assert!(Instant::now() < deadline, "the leader's recorder synced");
        members[0]
            .module
            .do_work()
            .expect("the leader's duty cycle");
        assert_eq!(poll_log(&mut logs[0], &mut reads[0]), 0);
    }
    let held = Instant::now() + Duration::from_millis(300);
    while Instant::now() < held {
        members[0]
            .module
            .do_work()
            .expect("the leader's duty cycle");
        assert_eq!(poll_log(&mut logs[0], &mut reads[0]), 0);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(members[0].module.commit_position(), first_end);
    assert_eq!(
        (logs[0].position(), logs[0].commit_position()),
        (first_end, first_end),
        "the leader's service stopped at the commit position"
    );

    // The followers run again, the hundred are committed, and every service reads
    // them.
    run_until(
        &mut members,
        "every service read the second entries",
        |_| {
            for (log, read) in logs.iter_mut().zip(reads.iter_mut()) {
                poll_log(log, read);
            }
            reads
                .iter()
                .all(|read| read.len() == (FIRST + SECOND) as usize + 1)
        },
    );
    assert_eq!(reads[1], reads[0]);
    assert_eq!(reads[2], reads[0]);

    // A service that starts late reads the log from its first entry, and one that
    // returns reads from the position it had, beside the service already on that
    // driver: two replays on one stream, neither holding the other.
    let mut late_client = Cavalry::connect_to(&members[1].dir).expect("a second client");
    let mut late = late_client
        .committed_log(0)
        .expect("the driver answers")
        .expect("the member has its log");
    let (middle, _) = reads[0][250];
    let mut returned = late_client
        .committed_log(middle)
        .expect("the driver answers")
        .expect("the member has its log");
    let (mut late_read, mut returned_read) = (Read::new(), Read::new());
    run_until(&mut members, "the late services read the log", |_| {
        poll_log(&mut late, &mut late_read);
        poll_log(&mut returned, &mut returned_read);
        poll_log(&mut logs[1], &mut reads[1]);
        late_read.len() == reads[0].len() && returned_read.len() == reads[0].len() - 250
    });
    assert_eq!(late_read, reads[0]);
    assert_eq!(returned_read[..], reads[0][250..]);
    assert_eq!(reads[1], reads[0], "the first service read nothing twice");
    late.close(&mut late_client).expect("the log closes");
    returned.close(&mut late_client).expect("the log closes");
    late_client.close().expect("the client closes");

    // A follower's consensus module closes. Its service has come to its end: it reads
    // nothing more, its bound is the last it took, and no log is opened until a member
    // has one again.
    let Member {
        driver,
        module,
        dir: driver_dir,
        ..
    } = members.remove(2);
    module.close().expect("the member closes");
    let mut log = logs.remove(2);
    let mut read = reads.remove(2);
    let mut client = clients.remove(2);
    let deadline = Instant::now() + Duration::from_secs(30);
    while !log.is_ended() {
        assert!(
            Instant::now() < deadline,
            "the service's log ended: {log:?}"
        );
        poll_log(&mut log, &mut read);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(poll_log(&mut log, &mut read), 0);
    assert_eq!(read, reads[0]);
    assert_eq!(
        (log.position(), log.commit_position()),
        (second_end, second_end)
    );
    let position = log.position();
    log.close(&mut client).expect("the log closes");
    let deadline = Instant::now() + Duration::from_secs(30);
    while client
        .committed_log(position)
        .expect("the driver answers")
        .is_some()
    {
        // The driver frees a closed client's counters as it handles the close.
        assert!(Instant::now() < deadline, "the member's counters went");
        std::thread::sleep(Duration::from_millis(1));
    }
    client.close().expect("the client closes");
    driver.close().expect("the driver closes");
    let _ = std::fs::remove_dir_all(driver_dir);

    for log in logs {
        drop(log);
    }
    for client in clients {
        client.close().expect("the client closes");
    }
    for member in members {
        member.module.close().expect("the member closes");
        member.driver.close().expect("the driver closes");
    }
}

/// Whether `counters` holds a counter of `type_id` whose key begins with `member_id`.
fn has_member_counter(counters: &CountersReader, member_id: i32, type_id: i32) -> bool {
    let key = member_id.to_le_bytes();
    (0..counters.capacity()).any(|id| {
        matches!(
            counters.record(id),
            Ok(Some(record)) if record.type_id() == type_id && record.key().starts_with(&key)
        )
    })
}

/// A service on follower 2 whose handle has taken its bound, the commit position of
/// [`BEHIND_ENTRIES`] entries and the term's first, and has read at most one entry
/// under it: where the two tests below begin.
struct BehindItsBound {
    members: Vec<Member>,
    client: Cavalry,
    log: CommittedLog,
    read: Read,
    /// The bound the handle took: the position after the last entry.
    end: i64,
}

/// The entries committed before [`BehindItsBound`]'s service reads any.
const BEHIND_ENTRIES: i64 = 1000;

impl BehindItsBound {
    fn start(dir: &Path) -> Self {
        let members_string = members_string();
        let mut members = start(dir, &members_string);
        run_until(&mut members, "every election closed", |members| {
            members
                .iter()
                .all(|m| m.module.election_state() == ElectionState::Closed)
        });
        let mut client =
            Cavalry::connect_to(&members[2].dir).expect("the service's client connects");
        let mut opened = None;
        run_until(&mut members, "the service opened its log", |_| {
            if opened.is_none() {
                opened = client.committed_log(0).expect("the driver answers");
            }
            opened.is_some()
        });
        let mut log = opened.expect("the member has its log");

        // The entries, committed on every member, before the service reads any.
        // Every member's append position is the end too, so the follower's recording
        // holds them all; the replay plays them as its recorder syncs them, and flow
        // control lets it run a window ahead of a service that has read one entry.
        offer_entries(&mut members, BEHIND_ENTRIES);
        let end = 96 + BEHIND_ENTRIES * 96;
        run_until(
            &mut members,
            "every member committed and appended every entry",
            |members| {
                members
                    .iter()
                    .all(|m| m.module.commit_position() == end && m.module.append_position() == end)
            },
        );

        // The service takes the member's commit position as its bound and reads at
        // most one entry under it.
        let mut read = Read::new();
        let deadline = Instant::now() + Duration::from_secs(30);
        while log.commit_position() < end {
            assert!(
                Instant::now() < deadline,
                "the service took its bound: {log:?}"
            );
            poll_log_at_most(&mut log, &mut read, 1);
        }
        assert_eq!(log.commit_position(), end);
        assert!(log.position() <= 96, "{log:?}");
        assert!(!log.is_ended());
        Self {
            members,
            client,
            log,
            read,
            end,
        }
    }

    /// Follower 2's consensus module closes, and the driver frees its counters.
    /// Returns its driver, still running, and the driver's directory.
    fn close_follower(&mut self) -> (Driver, PathBuf) {
        let Member {
            driver,
            module,
            dir: driver_dir,
            ..
        } = self.members.remove(2);
        module.close().expect("the member closes");
        let counters = CountersReader::open(&driver_dir).expect("the counters file opens");
        let deadline = Instant::now() + Duration::from_secs(30);
        while has_member_counter(&counters, 2, CLUSTER_COMMIT_POSITION.0) {
            assert!(Instant::now() < deadline, "the member's counters went");
            std::thread::sleep(Duration::from_millis(1));
        }
        (driver, driver_dir)
    }

    fn close(self, driver: Driver) {
        let Self {
            members,
            mut client,
            log,
            ..
        } = self;
        log.close(&mut client).expect("the log closes");
        client.close().expect("the client closes");
        driver.close().expect("the driver closes");
        for member in members {
            member.module.close().expect("the member closes");
            member.driver.close().expect("the driver closes");
        }
    }
}

/// A service whose member's consensus module closes while entries below the bound the
/// service took are still to be read (`spec/cluster.md`, "The service"). The handle
/// stops taking the member's commit position at the first poll after the member's
/// counters go, and it is not ended until it has delivered every entry below the bound
/// it took: a service that stops reading once its handle is ended, as
/// `Cavalry::committed_log`'s example and the how-to's step 4 do, has then read every
/// entry its bound covered, and opens the log again at that bound. The same holds for a
/// member that leaves the log, which stores −1 as its recording's id once the recording
/// has stopped: the bound stops, and the replay plays on to the stop.
#[test]
fn a_service_is_ended_only_once_it_has_read_every_entry_below_its_bound() {
    let dir = TestDir::new("service-ended");
    let mut service = BehindItsBound::start(dir.path());
    let (driver, _) = service.close_follower();

    // The next poll finds the counter gone and keeps the bound it took. The handle
    // ends only once it has delivered every entry below that bound, and delivers
    // nothing after.
    let BehindItsBound { log, read, end, .. } = &mut service;
    let end = *end;
    let deadline = Instant::now() + Duration::from_secs(30);
    while !log.is_ended() {
        assert!(
            Instant::now() < deadline,
            "the service's log ended: {log:?}"
        );
        poll_log(log, read);
    }
    assert_eq!(
        (log.position(), log.commit_position()),
        (end, end),
        "an ended handle has delivered every entry below its bound"
    );
    assert_eq!(log.withdrawn_at(), None, "{log:?}");
    assert_eq!(log.failed_at(), None, "{log:?}");
    assert_eq!(poll_log(log, read), 0);
    assert_eq!(
        read.len(),
        usize::try_from(BEHIND_ENTRIES).expect("a count") + 1
    );
    for (index, (position, _)) in read.iter().enumerate() {
        assert_eq!(*position, 96 * i64::try_from(index).expect("an index"));
    }
    service.close(driver);
}

/// Whether `counters` holds a `subscription.position` counter of an image on
/// `stream_id`: the key is the publication's registration id, eight bytes, its session
/// id, four, and the stream id, four (`counters.toml`).
fn has_image_on_stream(counters: &CountersReader, stream_id: i32) -> bool {
    (0..counters.capacity()).any(|id| {
        matches!(
            counters.record(id),
            Ok(Some(record)) if record.type_id() == SUBSCRIPTION_POSITION.0
                && record.key().get(12..16) == Some(&stream_id.to_le_bytes()[..])
        )
    })
}

/// A service that does not poll for longer than the publication linger, 5 s, after its
/// member's consensus module closes, with entries below its bound unread. The member's
/// recording stops, the replay ends at the stop, and the driver withdraws the replay's
/// image once the linger has passed, read or not (`spec/client-protocol.md`,
/// "Events"). The handle is ended, short of its bound, and it says so:
/// `withdrawn_at` is where the entries it was not given begin, which is where the
/// service had read to, so that a service is never told it read what it did not.
#[test]
fn a_service_whose_image_was_withdrawn_unread_is_told_where_its_entries_stop() {
    let dir = TestDir::new("service-withdrawn");
    let mut service = BehindItsBound::start(dir.path());
    let closed = Instant::now();
    let (driver, driver_dir) = service.close_follower();

    // The service does not poll. The driver withdraws the image the linger after the
    // replay's end, and frees the image's `subscription.position` counter as it sends
    // `unavailable image`.
    let counters = CountersReader::open(&driver_dir).expect("the counters file opens");
    let deadline = Instant::now() + Duration::from_secs(30);
    while has_image_on_stream(&counters, SERVICE_LOG_STREAM_ID) {
        assert!(
            Instant::now() < deadline,
            "the driver withdrew the replay's image"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        closed.elapsed() >= Duration::from_secs(5),
        "the image went at the linger's end, not at its end of stream: {:?}",
        closed.elapsed()
    );
    // The client's conductor hands the event to the subscription on a later duty
    // cycle, and a poll that read frames before then would read on from the image's
    // log, which the client still maps. A poll with a fragment limit of 0 applies the
    // events the subscription has and reads no frame, so the service waits for the
    // withdrawal with such polls, however long the conductor takes: the handle is
    // ended short of its bound, says where, and delivers nothing more.
    let BehindItsBound { log, read, end, .. } = &mut service;
    let end = *end;
    let deadline = Instant::now() + Duration::from_secs(30);
    while !log.is_ended() {
        assert!(
            Instant::now() < deadline,
            "the service's log ended: {log:?}"
        );
        assert_eq!(poll_log_at_most(log, read, 0), 0);
        std::thread::sleep(Duration::from_millis(1));
    }
    let position = log.position();
    assert!(position < end, "{log:?}");
    assert_eq!(log.commit_position(), end);
    assert_eq!(
        log.withdrawn_at(),
        Some(position),
        "an ended handle short of its bound says where: {log:?}"
    );
    assert_eq!(log.failed_at(), None, "{log:?}");
    assert_eq!(poll_log(log, read), 0);
    assert_eq!(log.withdrawn_at(), Some(position));
    assert_eq!(
        i64::try_from(read.len()).expect("a count") * 96,
        position,
        "every entry below the position was delivered, and none past it"
    );
    for (index, (entry_position, _)) in read.iter().enumerate() {
        assert_eq!(
            *entry_position,
            96 * i64::try_from(index).expect("an index")
        );
    }
    service.close(driver);
}

/// A service beside a follower that left the log and joined it again
/// (`spec/cluster.md`, "The service"). The member names a new recording, which begins
/// where it joined, its append position. A service that had read to that position
/// opens the log there and reads on. A service whose position is below it is refused
/// the replay with code 23: the entries before it are in the recording the member
/// left, which this version names to no service.
#[test]
fn a_service_reads_on_where_the_recording_its_member_joined_again_on_begins() {
    let dir = TestDir::new("service-joins-again");
    let members_string = members_string();
    let mut members = start(dir.path(), &members_string);
    run_until(&mut members, "every election closed", |members| {
        members
            .iter()
            .all(|m| m.module.election_state() == ElectionState::Closed)
    });
    const ENTRIES: i64 = 10;
    offer_entries(&mut members, ENTRIES);
    let first = 96 + ENTRIES * 96;
    run_until(&mut members, "the first entries committed", |members| {
        members
            .iter()
            .all(|m| m.module.commit_position() == first && m.module.append_position() == first)
    });

    // A service on follower 1 reads every entry committed so far.
    let mut client = Cavalry::connect_to(&members[1].dir).expect("the service's client connects");
    let mut log = client
        .committed_log(0)
        .expect("the driver answers")
        .expect("the member has its log");
    let first_recording = log.recording_id();
    let mut read = Read::new();
    run_until(&mut members, "the service read the first entries", |_| {
        poll_log(&mut log, &mut read);
        log.position() == first
    });

    // The leader's module stands still: each follower leaves the log, and the
    // service's handle comes to its end at the position it had read.
    run_until(
        &mut members[1..],
        "each follower timed out its silent leader and the service's log ended",
        |members| {
            poll_log(&mut log, &mut read);
            log.is_ended()
                && members.iter().all(|m| {
                    m.module.counts().leader_heartbeat_timeouts == 1
                        && m.module.election_state() == ElectionState::Canvass
                })
        },
    );
    assert_eq!(log.position(), first);
    assert_eq!(read.len(), usize::try_from(ENTRIES).expect("a count") + 1);
    log.close(&mut client).expect("the handle closes");

    // The leader runs again, the follower joins the log on a new recording, and the
    // leader appends more.
    run_until(
        &mut members,
        "each follower joined the log again",
        |members| {
            members
                .iter()
                .all(|m| m.module.election_state() == ElectionState::Closed)
                && members[1..].iter().all(|m| {
                    m.module
                        .log_recording_id()
                        .is_some_and(|id| id != first_recording)
                })
        },
    );
    offer_entries(&mut members, ENTRIES);
    let second = first + ENTRIES * 96;
    run_until(
        &mut members,
        "the entries after the join committed",
        |members| {
            members.iter().all(|m| {
                m.module.commit_position() == second && m.module.append_position() == second
            })
        },
    );

    // Below the start of the recording the member names now: refused, and said so.
    let refused = client
        .committed_log(0)
        .expect_err("position 0 is below the start of the member's new recording");
    assert!(
        matches!(refused, ClientError::Refused { raw_code: 23, .. }),
        "{refused}"
    );
    // At its start, the position the service had read to: the service reads on, the
    // ten entries appended after the join and nothing before them.
    let mut log = client
        .committed_log(first)
        .expect("the driver answers")
        .expect("the member has its log");
    assert_ne!(log.recording_id(), first_recording);
    assert_eq!(
        Some(log.recording_id()),
        members[1].module.log_recording_id()
    );
    let mut after = Read::new();
    run_until(&mut members, "the service read what came after", |_| {
        poll_log(&mut log, &mut after);
        log.position() == second
    });
    assert_eq!(after.len(), usize::try_from(ENTRIES).expect("a count"));
    assert_eq!(after[0].0, first);
    log.close(&mut client).expect("the handle closes");
    client.close().expect("the service's client closes");

    for member in members {
        member.module.close().expect("the member closes");
        member.driver.close().expect("the driver closes");
    }
}
