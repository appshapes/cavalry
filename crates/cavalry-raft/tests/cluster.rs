//! Three members over three drivers on the loopback: each a [`ConsensusModule`], a
//! client of its own driver, with its log recorded into its own log directory (ADR
//! 0060; ADR 0063). The appointed leader leads, the followers arm their recordings
//! before its first frame, a thousand entries are committed at a quorum of synced
//! positions, and the three recordings hold the same bytes.

use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cavalry_client::{Cavalry, CommittedLog};
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

/// A commit waits for one sync on a quorum, not for the recorders' next second
/// (`spec/cluster.md`, "The log"; ADR 0060, ADR 0067). Ten entries are appended one
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

/// What a service read: each entry's position and its bytes.
type Read = Vec<(i64, Vec<u8>)>;

/// Polls `log` once into `read`, and holds the poll to the service's rule: every entry
/// it was given begins below the commit position the poll took as its bound.
fn poll_log(log: &mut CommittedLog, read: &mut Read) -> usize {
    let before = read.len();
    let delivered = log
        .poll(
            |entry, _, position| read.push((position, entry.to_vec())),
            64,
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
