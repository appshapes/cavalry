//! Three members over three drivers on the loopback: each a [`ConsensusModule`], a
//! client of its own driver, with its log recorded into its own log directory (ADR
//! 0060; ADR 0063). The appointed leader leads, the followers arm their recordings
//! before its first frame, a hundred thousand entries are committed at a quorum of synced
//! positions, and the three recordings hold the same bytes. A member is lost, a follower
//! or the leader, and a follower leaves the log and joins it again: what a member that
//! leaves the log holds as its log's end is where the recording it held stopped.

use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cavalry_driver::Driver;
use cavalry_log::catalog::CatalogReader;
use cavalry_proto::cluster::{Message, Stream};
use cavalry_raft::{ConsensusModule, ElectionState, NO_SESSION, RaftError, Refused, Role};
use cavalry_shm::counters::CountersReader;
use cavalry_shm::counters::generated::{
    CLUSTER_APPEND_POSITION, CLUSTER_COMMIT_POSITION, CLUSTER_ELECTION_STATE,
    CLUSTER_ELECTION_STEPS, CLUSTER_ELECTIONS, CLUSTER_ELECTIONS_CLOSED,
    CLUSTER_LEADERSHIP_TERM_ID, CLUSTER_LOG_RECORDING_ID, CLUSTER_ROLE, CLUSTER_TERMS_LED,
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

/// Free loopback ports, each held until all are chosen so that none repeats.
fn free_ports(count: usize) -> Vec<u16> {
    let sockets: Vec<UdpSocket> = (0..count)
        .map(|_| UdpSocket::bind("127.0.0.1:0").expect("a loopback socket"))
        .collect();
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
