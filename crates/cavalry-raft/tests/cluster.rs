//! Three members over three drivers on the loopback: each a [`ConsensusModule`], a
//! client of its own driver, with its log recorded into its own log directory (ADR
//! 0060; ADR 0063). The appointed leader leads, the followers arm their recordings
//! before its first frame, a thousand entries are committed at a quorum of synced
//! positions, and the three recordings hold the same bytes.

use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cavalry_driver::Driver;
use cavalry_proto::cluster::{Message, Stream};
use cavalry_raft::{ConsensusModule, ElectionState, NO_SESSION, RaftError, Refused, Role};
use cavalry_shm::counters::CountersReader;
use cavalry_shm::counters::generated::{
    CLUSTER_COMMIT_POSITION, CLUSTER_ELECTION_STATE, CLUSTER_ELECTIONS, CLUSTER_ROLE,
    CLUSTER_TERMS_LED,
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
fn three_members_replicate_a_thousand_entries_and_their_recordings_are_equal() {
    let dir = TestDir::new("replicate");
    let members_string = members_string();
    let mut members = start(dir.path(), &members_string);

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

    // A thousand entries from the leader's own process, a back-pressured offer made
    // again once the members have run.
    const ENTRIES: u32 = 1_000;
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
    }
    // The term event, 56 bytes in a frame of 96, and a thousand entries of 40 bytes,
    // each in a frame of 96 (`spec/cluster.md`, "The log").
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
    // The first entry is the term's event and the last is the thousandth offer.
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
        assert_eq!(counter(member, CLUSTER_ELECTION_STATE.0), 17);
        assert_eq!(counter(member, CLUSTER_ELECTIONS.0), 1);
        assert_eq!(
            counter(member, CLUSTER_ROLE.0),
            if index == 0 { 2 } else { 0 }
        );
        assert_eq!(counter(member, CLUSTER_TERMS_LED.0), i64::from(index == 0));
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
    assert_eq!(members[0].module.counts().quorum_lost, 0);
    assert_eq!(members[1].module.counts().leader_heartbeat_timeouts, 0);
    assert_eq!(members[1].module.counts().log_images_lost, 0);

    for member in members {
        member.module.close().expect("the member closes");
        member.driver.close().expect("the driver closes");
    }
}
