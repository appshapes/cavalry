//! Three members over three drivers on the loopback: each a [`ConsensusModule`], a
//! client of its own driver, with its log recorded into its own log directory (ADR
//! 0060; ADR 0063). The members elect a leader, the followers arm their recordings
//! of its log, a hundred thousand entries are committed at a quorum of synced
//! positions, and the three recordings hold the same bytes but for their frames'
//! session ids. A member is lost, a follower or the leader, and a follower leaves the
//! log and joins it again: what a member that leaves the log holds as its log's end is
//! where the recording it held stopped. A follower that is slow at the start joins the
//! log after the term's first entry and catches up by a replay (ADR 0104, Evidence);
//! the three tests where the leader is lost or stands still make one follower do so on
//! every run. So do the tests of a service but one, the test of a member whose file of
//! its log's recordings cannot be written, which picks that member before the election
//! and holds whether it joined late or not. A service reads beside the late follower
//! across its recordings, or beside the follower that joined on time, whose log is one
//! recording.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cavalry_client::{Cavalry, CommittedLog, Fault};
use cavalry_driver::Driver;
use cavalry_log::catalog::CatalogReader;
use cavalry_proto::cluster::{
    LOG_RECORDINGS_FILE_PREFIX, LOG_RECORDINGS_TMP_SUFFIX, LogRecordings, Message,
    SERVICE_LOG_STREAM_ID, Stream,
};
use cavalry_raft::{ConsensusModule, Counts, ElectionState, NO_SESSION, RaftError, Refused, Role};
use cavalry_shm::counters::CountersReader;
use cavalry_shm::counters::generated::{
    CLUSTER_APPEND_POSITION, CLUSTER_APPOINTED_LEADER_BEHIND, CLUSTER_COMMIT_POSITION,
    CLUSTER_ELECTION_CAUSE, CLUSTER_ELECTION_STATE, CLUSTER_ELECTION_STEPS, CLUSTER_ELECTIONS,
    CLUSTER_ELECTIONS_CLOSED, CLUSTER_LEADER_MEMBER_ID, CLUSTER_LEADERSHIP_TERM_ID,
    CLUSTER_LOG_RECORDING_ID, CLUSTER_LOG_RECORDINGS_UNWRITTEN, CLUSTER_ROLE, CLUSTER_TERMS_LED,
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

// A cluster's ports, taken as `ports/mod.rs` says and for its reason.
mod ports;
use ports::free_ports;

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

/// Starts the three members in the order 1, 2, 0, so that no member's place in the
/// vector is its id, before the leader is put first and after: a test that needs the
/// leader finds it (`run_until_led`), and none may take a member's place for its id.
/// The members elect, so the leader is whichever of them won its ballot.
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

/// Runs the members until every election has closed, then puts the member that leads
/// first: from here `members[0]` is the leader and `members[1..]` are its followers,
/// whichever member the cluster elected (ADR 0100, change 3; ADR 0104). `start`
/// starts the members in the order 1, 2, 0, so that no member's place is its id.
/// Returns the place the leader was in: a test that holds a vector beside `members`,
/// one item for each member, swaps that place with the first there too.
fn run_until_led(members: &mut [Member]) -> usize {
    run_until(members, "every election closed", |members| {
        members
            .iter()
            .all(|m| m.module.election_state() == ElectionState::Closed)
    });
    lead_first(members)
}

/// Puts the member that leads first, and returns the place it was in.
fn lead_first(members: &mut [Member]) -> usize {
    let leader = members
        .iter()
        .position(|m| m.module.role() == Role::Leader)
        .expect("one member leads");
    members.swap(0, leader);
    leader
}

/// As [`run_until_led`], with one follower made to join the log late. A follower that
/// is slow at the start takes this path by chance (ADR 0104, Evidence); here it takes
/// it on every run. The first member found in follower log init has taken the leader's
/// term at position 0 and has not yet subscribed to the leader's log. It runs no duty
/// cycle until the other two have closed their elections and hold the term's first
/// entry: the leader closed on a quorum without it. Its image of the log then joins
/// past its append position. It counts the mismatch, fails its election, and catches
/// up in the next. Returns the late follower's member id, once it has been seen to do
/// so, and the place the leader was in, as [`run_until_led`] does.
fn run_until_led_with_a_late_follower(members: &mut [Member]) -> (i32, usize) {
    run_until_led_with_a_late_follower_beside(members, |_| {})
}

/// [`run_until_led_with_a_late_follower`], with `beside` called after each round of
/// duty cycles, while the late member stands still too: what a test runs beside the
/// members as they elect, its services asking for their logs.
fn run_until_led_with_a_late_follower_beside(
    members: &mut [Member],
    mut beside: impl FnMut(&[Member]),
) -> (i32, usize) {
    let mut late = None;
    run_until(members, "a member took a leader's term", |members| {
        beside(members);
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
            "the others closed without member {late_id}: {:?}",
            states(members)
        );
        let mut work = 0;
        for (index, member) in members.iter_mut().enumerate() {
            if index != late {
                work += member.module.do_work().expect("the member's duty cycle");
            }
        }
        beside(members);
        if work == 0 {
            std::thread::sleep(Duration::from_micros(200));
        }
    }
    run_until(members, "every election closed", |members| {
        beside(members);
        members
            .iter()
            .all(|m| m.module.election_state() == ElectionState::Closed)
    });
    let led_from = lead_first(members);
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
    (late_id, led_from)
}

/// Puts the follower that joined the log on time, the one whose id is not `late`, in
/// `place`, 1 or 2, and the late one in the other. Its log is one recording from 0, so
/// a service beside it that opens the log at 0 reads the live recording.
fn put_on_time_follower(members: &mut [Member], late: i32, place: usize) {
    let on_time = if members[1].module.member_id() == late {
        2
    } else {
        1
    };
    members.swap(on_time, place);
    assert_ne!(members[place].module.member_id(), late);
    assert_eq!(members[place].module.role(), Role::Follower);
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

/// Whether the member's driver holds a counter of `type_id` under the member's id.
fn has_counter(member: &Member, type_id: i32) -> bool {
    let reader = CountersReader::open(&member.dir).expect("the counters file opens");
    let key = member.module.member_id().to_le_bytes();
    (0..reader.capacity()).any(|id| {
        reader
            .record(id)
            .expect("a record")
            .is_some_and(|record| record.type_id() == type_id && record.key().starts_with(&key))
    })
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
        // It names no leader, and the cause of its election is its start (ADR 0102).
        assert_eq!(counter(member, CLUSTER_LEADER_MEMBER_ID.0), -1);
        assert_eq!(counter(member, CLUSTER_ELECTION_CAUSE.0), 0);
        // No member adds the counter of the appointed leader's check: that it is
        // not there is how `cavalry doctor` knows a member that elects (ADR 0104).
        assert!(
            !has_counter(member, CLUSTER_APPOINTED_LEADER_BEHIND.0),
            "member {} added cluster.appointed_leader_behind",
            member.module.member_id()
        );
    }

    run_until_led(&mut members);
    assert_eq!(members[0].module.role(), Role::Leader);
    let leader_id = members[0].module.member_id();
    // The term is 0 when the first ballot elects, and the one after for each ballot
    // that two candidates split: it is whatever the leader leads.
    let term = members[0].module.leadership_term_id();
    for (index, member) in members.iter().enumerate() {
        assert_eq!(member.module.leadership_term_id(), term);
        // The leader closes its election on a quorum, so a follower that was slow
        // can meet the log after the term's first entry: its image then joins past
        // where its log ends, its election fails once, and it catches up in the
        // next (ADR 0104, Evidence). The leader's own election fails for nothing.
        let counts = member.module.counts();
        let late = u64::from(index != 0);
        assert!(
            counts.elections >= 1 && counts.elections <= 1 + late,
            "member {}: {counts:?}",
            member.module.member_id()
        );
        assert_eq!(counts.elections_failed, counts.elections - 1, "{counts:?}");
        assert!(
            counts.join_position_mismatches <= counts.elections_failed,
            "{counts:?}"
        );
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

    // Each member's recordings hold the same bytes up to the commit position, but for
    // the session ids that `log_bytes` sets to 0: the leader's of its publication and
    // each follower's of the leader's image, and, for a follower that joined late, of
    // the replay it caught up by before it.
    let recordings: Vec<Vec<u8>> = members
        .iter()
        .map(|member| log_bytes(member, end))
        .collect();
    assert_eq!(
        recordings[1], recordings[0],
        "the first follower's recording"
    );
    assert_eq!(
        recordings[2], recordings[0],
        "the second follower's recording"
    );
    // The first entry is the term's event and the last is the hundred thousandth offer.
    let first = Message::parse(Stream::Log, &recordings[0][32..32 + 56]).expect("an entry");
    assert!(matches!(
        first,
        Message::NewLeadershipTermEvent {
            leadership_term_id,
            log_position: 0,
            leader_member_id,
            ..
        } if leader_member_id == leader_id && leadership_term_id == term
    ));
    let last_frame = &recordings[0][recordings[0].len() - 96..];
    let last = Message::parse(Stream::Log, &last_frame[32..32 + 40]).expect("an entry");
    let expected = [(ENTRIES - 1).to_le_bytes(), (!(ENTRIES - 1)).to_le_bytes()].concat();
    assert!(matches!(
        last,
        Message::SessionMessage {
            leadership_term_id,
            cluster_session_id: NO_SESSION,
            payload,
            ..
        } if payload == expected && leadership_term_id == term
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
        assert_eq!(counter(member, CLUSTER_LEADERSHIP_TERM_ID.0), term);
        assert_eq!(counter(member, CLUSTER_ELECTION_STATE.0), 17);
        // The file holds the elections the member counted: one, or two for a follower
        // that joined late, whose first failed (above).
        let counts = member.module.counts();
        assert_eq!(
            counter(member, CLUSTER_ELECTIONS.0),
            i64::try_from(counts.elections).expect("a count")
        );
        // Every member names the leader, the leader itself among them (ADR 0102). The
        // cause of a member's last election is its start, 0, where its one election
        // closed, and the failed election, 5, for a follower that joined late and
        // caught up in its second.
        assert_eq!(
            counter(member, CLUSTER_LEADER_MEMBER_ID.0),
            i64::from(leader_id)
        );
        assert_eq!(
            counter(member, CLUSTER_ELECTION_CAUSE.0),
            if counts.elections == 1 { 0 } else { 5 },
            "member {}: {counts:?}",
            member.module.member_id()
        );
        assert_eq!(
            counter(member, CLUSTER_ROLE.0),
            if index == 0 { 2 } else { 0 }
        );
        assert_eq!(counter(member, CLUSTER_TERMS_LED.0), i64::from(index == 0));
        // The two counts of ADR 0067 reach the file as the others do, through
        // `Counts::by_counter`: one election closed, in four steps on the leader,
        // which nominated itself and won its ballot, and in four on a follower that
        // took the leader's term from canvass or five on one that voted for it first.
        assert_eq!(counter(member, CLUSTER_ELECTIONS_CLOSED.0), 1);
        // Where the first ballot elected, the leader took four steps: to nominate,
        // and three from leader log replication to leader ready. A follower took
        // four from its ballot or from canvass to follower ready, and one more when
        // it was in nominate itself when the leader's request for its vote came. A
        // ballot that two candidates split sends both back to canvass, with steps of
        // its own.
        let steps = counter(member, CLUSTER_ELECTION_STEPS.0);
        // A count of steps is exact only for a member whose one election closed with
        // no catch-up and no ballot lost: one that joined late took the steps of a
        // catch-up too, and a voter whose candidate's term came later than the
        // election timeout went back to canvass and took the term from there.
        if term == 0 && counts.elections == 1 && counts.catchups == 0 && counts.ballots_lost == 0 {
            assert_eq!(counts.candidacies, u64::from(index == 0));
            assert!(
                if index == 0 {
                    steps == 4
                } else {
                    steps == 4 || steps == 5
                },
                "member {}: {steps} steps, {counts:?}",
                member.module.member_id()
            );
        } else {
            assert!(
                steps >= 4,
                "member {}: {steps} steps, {counts:?}",
                member.module.member_id()
            );
        }
    }

    // A member over a driver does not start again on the log it has: the refusal
    // says why and what to do.
    let Member {
        driver,
        module,
        dir: driver_dir,
        log_dir,
    } = members.remove(2);
    let member_id = module.member_id();
    module.close().expect("the member closes");
    let again = ConsensusModule::start(&driver_dir, &log_dir, &members_string, member_id)
        .expect_err("a log directory that holds a term");
    // Its directory holds the term, and the candidate term it stored when it voted
    // in a ballot, or none when it took the term from canvass.
    assert!(
        matches!(
            again,
            RaftError::Restart {
                log_leadership_term_id,
                candidate_term,
            } if log_leadership_term_id == term && (-1..=term).contains(&candidate_term)
        ),
        "{again}"
    );
    let text = again.to_string();
    assert!(
        text.contains("every member on a new log directory")
            && text.contains("Do not start this member alone on a new log directory"),
        "{text}"
    );
    assert!(
        text.contains("docs/reference/cluster.md#roles-and-the-election"),
        "{text}"
    );

    // With its `terms` file removed the directory still holds the member's
    // recordings of the log. It is not a new one: the member is refused, and the
    // file is not made again, so the directory is left as it was.
    let terms_file = log_dir.join("terms");
    std::fs::remove_file(&terms_file).expect("the terms file is removed");
    let lost = ConsensusModule::start(&driver_dir, &log_dir, &members_string, member_id)
        .expect_err("a log directory that holds the log and no terms file");
    assert!(
        matches!(lost, RaftError::TermsLost { recording_id } if recording_id >= 0),
        "{lost}"
    );
    let text = lost.to_string();
    assert!(
        text.starts_with("member refused (terms-lost): the log directory holds recording"),
        "{text}"
    );
    assert!(
        text.contains("Do not start this member alone on a new log directory")
            && text.contains("docs/reference/cluster.md#roles-and-the-election"),
        "{text}"
    );
    assert!(!terms_file.exists(), "the refusal made no terms file");
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
    run_until_led(&mut members);
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
    run_until(
        &mut members,
        "every member recorded every entry",
        |members| members.iter().all(|m| m.module.append_position() == end),
    );
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
    run_until_led(&mut members);
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
/// log, and canvasses. The recording it held is
/// stopped, by its own stop or by the driver's at the image's end, whichever came
/// first, at the committed end: that is the append position it reports, and it names
/// no recording in `cluster.log_recording_id`, since none records the live log
/// (`spec/cluster.md`, "The service"). The next test is the one where the member's own
/// stop alone ends its recording, and where the end it takes decides what it appends
/// after. One follower joined the log late at the start
/// (`run_until_led_with_a_late_follower`), so each run holds one follower of each
/// kind, and the counts asserted are those added after the leader is lost.
#[test]
fn a_leader_lost_leaves_each_follower_at_the_end_its_recording_stopped() {
    let dir = TestDir::new("leader-lost");
    let members_string = members_string();
    let mut members = start(dir.path(), &members_string);
    run_until_led_with_a_late_follower(&mut members);
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
    // What each follower counted before the leader is lost. The late follower's
    // election failed once at the start, so what follows asserts the change from
    // here, not totals counted from the start.
    let before: Vec<Counts> = members[1..].iter().map(|m| m.module.counts()).collect();

    let leader = members.remove(0);
    drop(leader.module);
    leader.driver.close().expect("the driver closes");

    run_until(
        &mut members,
        "each follower left the log and canvasses again",
        |members| {
            members.iter().zip(&before).all(|(m, before)| {
                let counts = m.module.counts();
                counts.log_images_lost + counts.leader_heartbeat_timeouts
                    == before.log_images_lost + before.leader_heartbeat_timeouts + 1
                    && m.module.election_state() == ElectionState::Canvass
            })
        },
    );
    for ((member, recording_id), before) in members.iter().zip(held).zip(&before) {
        assert_eq!(member.module.role(), Role::Follower);
        // One election more than before the leader was lost, and no more failed.
        let counts = member.module.counts();
        assert_eq!(counts.elections, before.elections + 1, "{counts:?}");
        assert_eq!(
            counts.elections_failed, before.elections_failed,
            "{counts:?}"
        );
        assert_eq!(
            counter(member, CLUSTER_ELECTIONS.0),
            i64::try_from(counts.elections).expect("a count")
        );
        // It names no leader, and the cause of its election is what it lost: the
        // image of the leader's log, 2, or the leader's word for the heartbeat
        // timeout, 1, whichever its duty cycle met first (ADR 0102).
        assert_eq!(counter(member, CLUSTER_LEADER_MEMBER_ID.0), -1);
        assert_eq!(
            counter(member, CLUSTER_ELECTION_CAUSE.0),
            if counts.log_images_lost > before.log_images_lost {
                2
            } else {
                1
            }
        );
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

/// A leader lost is replaced (`spec/cluster.md`, "Roles and the election"; ADR 0077).
/// The first leader and its driver stop with a hundred entries committed on all
/// three. Each member that remains loses the leader's word or its image of the log,
/// leaves the log where its recording stopped, and canvasses; the two elect one of
/// themselves, which leads a later term from where the log ends, and the other follows
/// it.
/// An entry offered to the new leader is committed by the two, a quorum of three,
/// within five seconds of the leader heartbeat timeout. Nothing that was committed
/// is lost: the two hold the first hundred entries byte for byte, but for the frames'
/// session ids, and what follows them is the new term's first entry and the one
/// offered. One follower joined the log late at the start
/// (`run_until_led_with_a_late_follower`), so one of the two holds its log in more
/// than one recording on every run. The election's time is reported and not
/// asserted, since a split vote takes a second longer by design.
#[test]
fn a_lost_leader_is_replaced_by_the_two_that_remain_and_they_commit() {
    let dir = TestDir::new("leader-replaced");
    let members_string = members_string();
    let mut members = start(dir.path(), &members_string);
    run_until_led_with_a_late_follower(&mut members);
    const ENTRIES: i64 = 100;
    offer_entries(&mut members, ENTRIES);
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
    let first_leader = members[0].module.member_id();
    // The term the first leader leads is 0 unless its election took more than one
    // ballot, which a vote split between two candidates does.
    let first_term = members[0].module.leadership_term_id();

    let leader = members.remove(0);
    drop(leader.module);
    leader.driver.close().expect("the driver closes");
    let lost = Instant::now();

    run_until(
        &mut members,
        "the two that remain have a leader",
        |members| {
            members.iter().all(|m| {
                m.module.election_state() == ElectionState::Closed
                    && m.module.leadership_term_id() > first_term
            }) && members
                .iter()
                .filter(|m| m.module.role() == Role::Leader)
                .count()
                == 1
        },
    );
    let elected = lost.elapsed();
    let at = members
        .iter()
        .position(|m| m.module.role() == Role::Leader)
        .expect("one member leads");
    members.swap(0, at);
    let new_leader = members[0].module.member_id();
    assert_ne!(new_leader, first_leader);
    let term = members[0].module.leadership_term_id();
    for member in &members {
        assert_eq!(member.module.leadership_term_id(), term);
        // Each names the new leader, and the cause of its election is what it lost.
        assert_eq!(
            counter(member, CLUSTER_LEADER_MEMBER_ID.0),
            i64::from(new_leader)
        );
        let counts = member.module.counts();
        assert_eq!(counts.log_images_lost + counts.leader_heartbeat_timeouts, 1);
        assert!(
            [1, 2, 5].contains(&counter(member, CLUSTER_ELECTION_CAUSE.0)),
            "member {}: {counts:?}",
            member.module.member_id()
        );
    }

    // The new term's first entry lies where the log ended, and one offered entry
    // after it: both are committed by the two that remain.
    offer_entries(&mut members, 1);
    let committed_end = end + 2 * 96;
    run_until(
        &mut members,
        "the two commit an entry of the new term",
        |members| {
            members.iter().all(|m| {
                m.module.commit_position() == committed_end
                    && m.module.append_position() == committed_end
            })
        },
    );
    let committed = lost.elapsed();
    println!(
        "cluster: a lost leader was replaced by member {new_leader} in term {term} after \
         {elected:?}, and the two that remain committed an entry after {committed:?}"
    );
    // What each member's ballots were, for a run whose election took more than one.
    for member in &members {
        let counts = member.module.counts();
        println!(
            "cluster: member {}: first term {first_term}, candidacies {}, ballots lost {}, \
             votes for {}, votes against {}, higher terms seen {}, elections {}, \
             heartbeat timeouts {}, images lost {}",
            member.module.member_id(),
            counts.candidacies,
            counts.ballots_lost,
            counts.votes_for,
            counts.votes_against,
            counts.higher_terms_seen,
            counts.elections,
            counts.leader_heartbeat_timeouts,
            counts.log_images_lost
        );
    }
    assert!(
        committed < Duration::from_secs(10 + 5),
        "an entry was committed {committed:?} after the leader was lost, more than five \
         seconds past the leader heartbeat timeout"
    );

    // Nothing committed was lost. Each member's log of the first term holds the
    // same bytes from position 0 to the end of that term, but for the session ids
    // that `log_bytes` sets to 0, read across the recordings its file names: the
    // follower that joined late at the start holds what lies below its live image in
    // the recording of the replay it caught up by.
    let held: Vec<Vec<u8>> = members
        .iter()
        .map(|member| log_bytes(member, end))
        .collect();
    assert_eq!(held[0], held[1], "the two logs of the first term");
    // The last of the hundred entries is the last frame of both.
    let last_frame = &held[0][held[0].len() - 96..];
    let last = Message::parse(Stream::Log, &last_frame[32..32 + 40]).expect("an entry");
    assert!(
        matches!(
            last,
            Message::SessionMessage {
                leadership_term_id,
                cluster_session_id: NO_SESSION,
                ..
            } if leadership_term_id == first_term
        ),
        "{last:?}, first leader {first_leader}"
    );
    // An election that failed and began again is the election's own remedy, a
    // late joiner's among them, and is no fault: a truncation refused would be one.
    for member in &members {
        assert_eq!(member.module.counts().truncations_refused, 0);
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
/// its driver runs on, so that a follower's image of the log stays available and the
/// stop of the recording it holds is its own `leave_log`'s alone. The follower counts
/// the timeout, leaves the log at the end its recording stopped at, and canvasses; the
/// leader, running again, answers, and the follower joins the log again on a new
/// recording whose image joins at that end. What the leader appends after is committed
/// on all three: the follower's append position goes on from the stop of the recording
/// it held through a new one, which begins where the member's log ends.
///
/// One follower alone runs while the leader stands still. Two that had both timed out
/// would be a quorum with no leader, and would hold a ballot of their own if the
/// leader's answer came later than a nomination delay: this test is of the member
/// that leaves and joins again, so it leaves no quorum to elect. The second follower
/// stands still with the leader and runs again once the first has joined. One of the
/// two joined the log late at the start (`run_until_led_with_a_late_follower`), so
/// the counts and the recordings asserted are those added after the leader stands
/// still.
#[test]
fn a_follower_that_leaves_the_log_joins_it_again_where_its_recording_stopped() {
    let dir = TestDir::new("join-again");
    let members_string = members_string();
    let mut members = start(dir.path(), &members_string);
    run_until_led_with_a_late_follower(&mut members);
    const ENTRIES: i64 = 10;
    offer_entries(&mut members, ENTRIES);
    let first = 96 + ENTRIES * 96;
    run_until(&mut members, "the first entries committed", |members| {
        members
            .iter()
            .all(|m| m.module.commit_position() == first && m.module.append_position() == first)
    });
    let term = members[0].module.leadership_term_id();
    // What the follower counted and what its file of recordings held before its
    // leader stands still: a follower that joined late at the start has a failed
    // election and a recording of its catch-up from then, so what follows asserts
    // the change from here.
    let before = members[1].module.counts();
    let (held, held_from, below_before) = log_recordings(&members[1]);
    assert_eq!(members[1].module.log_recording_id(), Some(held));

    // The leader's module stands still, and the second follower's with it; the
    // leader's driver, and so its log's image on the first follower, runs on.
    run_until(
        &mut members[1..2],
        "the follower timed out its silent leader and canvasses",
        |members| {
            members.iter().all(|m| {
                m.module.counts().leader_heartbeat_timeouts == before.leader_heartbeat_timeouts + 1
                    && m.module.election_state() == ElectionState::Canvass
            })
        },
    );
    {
        let member = &members[1];
        let recording_id = &held;
        assert_eq!(
            member.module.counts().log_images_lost,
            before.log_images_lost
        );
        assert_eq!(member.module.log_recording_id(), None);
        assert_eq!(counter(member, CLUSTER_LOG_RECORDING_ID.0), -1);
        assert_eq!(member.module.append_position(), first);
        let catalog = CatalogReader::open(&member.log_dir).expect("the catalog opens");
        assert_eq!(
            catalog
                .stop_position(*recording_id)
                .expect("the recording's slot is read"),
            Some(first),
            "the recording member {} held",
            member.module.member_id()
        );
    }

    // The leader runs again and answers the follower's canvass, and the follower
    // joins the log on a new recording. The leader leads the term it led.
    run_until(
        &mut members[..2],
        "the follower joined the log again",
        |members| {
            members
                .iter()
                .all(|m| m.module.election_state() == ElectionState::Closed)
                && members[1]
                    .module
                    .log_recording_id()
                    .is_some_and(|id| id != held)
        },
    );
    // The second follower runs again. Whether it had counted the silence too by the
    // time it read the leader's word again is its own clock's matter: it follows the
    // same leader in the same term either way.
    run_until(&mut members, "every member follows the leader", |members| {
        members
            .iter()
            .all(|m| m.module.election_state() == ElectionState::Closed)
    });
    assert_eq!(members[0].module.role(), Role::Leader);
    for member in &members {
        assert_eq!(member.module.leadership_term_id(), term);
    }
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
    // The follower held one election more, which closed, with no more failed and
    // no join mismatch: its image joined where its log ends. Its new recording begins
    // where the one it left stopped, and its file lists that one below it.
    let counts = members[1].module.counts();
    assert_eq!(counts.elections, before.elections + 1, "{counts:?}");
    assert_eq!(
        counts.elections_closed,
        before.elections_closed + 1,
        "{counts:?}"
    );
    assert_eq!(
        counts.elections_failed, before.elections_failed,
        "{counts:?}"
    );
    assert_eq!(
        counts.join_position_mismatches, before.join_position_mismatches,
        "{counts:?}"
    );
    let rejoined = members[1]
        .module
        .log_recording_id()
        .expect("the follower's new recording");
    let mut below = below_before;
    below.push((held, held_from, first));
    assert_eq!(log_recordings(&members[1]), (rejoined, first, below));
    for member in &members[1..] {
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

/// [`poll_log`], and once the handle has ended, the log opened again where it ended,
/// as a service does (`CommittedLog`; ADR 0103). A follower that joined its leader's
/// log late holds what lies below its live image in its recording of the replay it
/// caught up by, and a handle opened there ends at that recording's stop, with every
/// entry it held delivered. The next handle reads on in the recording that follows.
/// A replay that failed or an image that faulted is the test's failure, not an end to
/// read past.
fn poll_across(client: &mut Cavalry, log: &mut CommittedLog, read: &mut Read) -> usize {
    let delivered = poll_log(log, read);
    if log.is_ended() {
        assert_eq!(
            (log.failed_at(), log.faulted()),
            (None, None),
            "the handle ended on a failure: {log:?}"
        );
        let next = client
            .committed_log(log.position())
            .expect("the driver answers")
            .expect("the member has its log");
        std::mem::replace(log, next)
            .close(client)
            .expect("the ended handle closes");
    }
    delivered
}

/// Opens at 0 each service's log that is not open yet, one service beside each member,
/// in the members' order. Returns how many of them were told that their member has no
/// log yet.
fn open_at_0(logs: &mut [Option<CommittedLog>], clients: &mut [Cavalry]) -> usize {
    let mut not_yet = 0;
    for (log, client) in logs.iter_mut().zip(clients.iter_mut()) {
        if log.is_none() {
            *log = client.committed_log(0).expect("the driver answers");
            not_yet += usize::from(log.is_none());
        }
    }
    not_yet
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
/// bound it last took once its member is gone. One follower joins the log late
/// (`run_until_led_with_a_late_follower_beside`), and the services beside it read
/// across its recordings as a service does (`poll_across`): the replay's it caught up
/// by, then its live one.
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
    // The late follower names none until it has caught up.
    let mut logs: Vec<Option<CommittedLog>> = (0..3).map(|_| None).collect();
    let mut asked_early = 0;
    let (late_id, led_from) = run_until_led_with_a_late_follower_beside(&mut members, |_| {
        asked_early += open_at_0(&mut logs, &mut clients);
    });
    run_until(&mut members, "every service opened its log", |_| {
        open_at_0(&mut logs, &mut clients);
        logs.iter().all(Option::is_some)
    });
    assert!(
        asked_early > 0,
        "no service asked before its member had a log"
    );
    let mut logs: Vec<CommittedLog> = logs.into_iter().flatten().collect();
    // A member names its recording before the leader's election closes, state 5 on
    // the leader and 15 on a follower, and the leader refuses an offer until it has
    // closed: the offers below waited for every election to close. The leader is put
    // first, and the follower that joined late second.
    clients.swap(0, led_from);
    logs.swap(0, led_from);
    if members[2].module.member_id() == late_id {
        members.swap(1, 2);
        clients.swap(1, 2);
        logs.swap(1, 2);
    }
    for (index, log) in logs.iter().enumerate() {
        let member = &members[index].module;
        assert_eq!(log.member_id(), member.member_id());
        // The recording that holds position 0: the member's live one, but beside the
        // follower that joined late, whose file names the recording of its catch-up's
        // replay below the live one, from 0.
        let (live, start, below) = log_recordings(&members[index]);
        assert_eq!(Some(live), member.log_recording_id());
        if index == 1 {
            assert!(
                matches!(below[..], [(replayed, 0, stop)]
                    if replayed == log.recording_id() && stop == start && replayed != live),
                "member {late_id}: live {live} from {start}, below {below:?}, opened {log:?}"
            );
        } else {
            assert_eq!((log.recording_id(), start), (live, 0));
        }
        assert_eq!((log.position(), log.commit_position()), (0, 0));
        assert!(!log.is_ended());
    }
    let leader_id = members[0].module.member_id();
    let mut reads: Vec<Read> = vec![Vec::new(); 3];

    // Five hundred entries while every member runs: each service reads the term's
    // first entry and the five hundred, at the positions the leader appended them.
    const FIRST: u32 = 500;
    offer(&mut members, &[0, 1, 2], 0, FIRST);
    let first_end = 96 + i64::from(FIRST) * 96;
    run_until(&mut members, "every service read the first entries", |_| {
        for ((log, read), client) in logs.iter_mut().zip(reads.iter_mut()).zip(&mut clients) {
            poll_across(client, log, read);
        }
        reads.iter().all(|read| read.len() == FIRST as usize + 1)
    });
    for (index, read) in reads.iter().enumerate() {
        // A place is not an id once the leader is put first: name the member.
        let member = members[index].module.member_id();
        assert_eq!(
            read, &reads[0],
            "member {member}'s service, in place {index}"
        );
        assert_eq!(logs[index].position(), first_end);
        // Every service reads the live recording now, the late follower's too.
        assert_eq!(
            Some(logs[index].recording_id()),
            members[index].module.log_recording_id()
        );
    }
    for (index, (position, entry)) in reads[0].iter().enumerate() {
        assert_eq!(*position, 96 * i64::try_from(index).expect("an index"));
        let message = Message::parse(Stream::Log, entry).expect("an entry of the log");
        if index == 0 {
            assert!(matches!(
                message,
                Message::NewLeadershipTermEvent {
                    log_position: 0,
                    leader_member_id,
                    ..
                } if leader_member_id == leader_id
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
            for ((log, read), client) in logs.iter_mut().zip(reads.iter_mut()).zip(&mut clients) {
                poll_across(client, log, read);
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
    // driver: two replays on one stream, neither holding the other. The driver is the
    // late follower's, so the service from 0 reads across its two recordings.
    let mut late_client = Cavalry::connect_to(&members[1].dir).expect("a second client");
    let mut late = late_client
        .committed_log(0)
        .expect("the driver answers")
        .expect("the member has its log");
    assert_ne!(
        Some(late.recording_id()),
        members[1].module.log_recording_id()
    );
    let (middle, _) = reads[0][250];
    let mut returned = late_client
        .committed_log(middle)
        .expect("the driver answers")
        .expect("the member has its log");
    let (mut late_read, mut returned_read) = (Read::new(), Read::new());
    run_until(&mut members, "the late services read the log", |_| {
        poll_across(&mut late_client, &mut late, &mut late_read);
        poll_across(&mut late_client, &mut returned, &mut returned_read);
        poll_across(&mut clients[1], &mut logs[1], &mut reads[1]);
        late_read.len() == reads[0].len() && returned_read.len() == reads[0].len() - 250
    });
    assert_eq!(late_read, reads[0]);
    assert_eq!(returned_read[..], reads[0][250..]);

    // A log opened where no entry begins: an entry here is 96 bytes, so 32 past one is
    // inside it. The driver's replay finds no frame there and fails at once, with
    // nothing played. The handle ends, delivers nothing and says where, and its client
    // is open. Before ADR 0078 the client closed as `corrupt`: the driver freed the
    // failed replay's image before the client's conductor had read its announcement.
    let inside = middle + 32;
    let mut failed = late_client
        .committed_log(inside)
        .expect("the driver answers")
        .expect("the member has its log");
    let mut nothing = Read::new();
    run_until(&mut members, "the log opened inside an entry ended", |_| {
        poll_log(&mut failed, &mut nothing);
        failed.is_ended()
    });
    assert!(nothing.is_empty(), "nothing was delivered");
    assert_eq!(
        (failed.failed_at(), failed.withdrawn_at(), failed.position()),
        (Some(inside), None, inside),
        "{failed:?}"
    );
    assert_eq!(late_client.closed(), None);
    failed.close(&mut late_client).expect("the log closes");

    // A log closed straight after its open, with no poll between, sixty times over. The
    // close stops the replay and removes the subscription, and the client's conductor
    // may read the image's announcement only after that, when the image's position
    // counter is freed. The client stays open, where it closed one time in eight.
    for round in 0..60 {
        let log = late_client
            .committed_log(0)
            .expect("the driver answers")
            .expect("the member has its log");
        log.close(&mut late_client)
            .unwrap_or_else(|error| panic!("round {round}: {error}"));
        assert_eq!(late_client.closed(), None, "round {round}");
        // Every log open on this driver hears each replay's image come and go on the
        // stream they share, so each is polled, as a service polls: a subscription
        // that is not polled while a queue's worth of images is announced closes its
        // client as `overrun` (ADR 0021).
        poll_across(&mut late_client, &mut late, &mut late_read);
        poll_across(&mut late_client, &mut returned, &mut returned_read);
        poll_across(&mut clients[1], &mut logs[1], &mut reads[1]);
    }
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

/// A service beside the follower in place 2 whose handle has taken its bound, the
/// commit position of [`BEHIND_ENTRIES`] entries and the term's first, and has read at
/// most one entry under it: where the two tests below begin. The other follower joined
/// the log late (`run_until_led_with_a_late_follower`), and the service is beside the
/// one that joined on time, whose log is one recording: a handle opened at 0 beside the
/// late one would replay its catch-up's recording and end at that recording's stop,
/// after the term's first entry, and these tests are of a bound, not of that end.
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
        let (late, _) = run_until_led_with_a_late_follower(&mut members);
        put_on_time_follower(&mut members, late, 2);
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
        assert_eq!(
            Some(log.recording_id()),
            members[2].module.log_recording_id()
        );

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

    /// The consensus module of the follower in place 2 closes, and the driver frees
    /// its counters.
    /// Returns its driver, still running, and the driver's directory.
    fn close_follower(&mut self) -> (Driver, PathBuf) {
        let Member {
            driver,
            module,
            dir: driver_dir,
            ..
        } = self.members.remove(2);
        let member_id = module.member_id();
        module.close().expect("the member closes");
        let counters = CountersReader::open(&driver_dir).expect("the counters file opens");
        let deadline = Instant::now() + Duration::from_secs(30);
        while has_member_counter(&counters, member_id, CLUSTER_COMMIT_POSITION.0) {
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

/// The member's log from position 0 to `end`, as a service reads it: from the
/// recordings its file names, in their order, each from where the one before it left
/// off, and then from its live recording (`spec/cluster.md`, "The log's recordings").
/// Every recording here lies in its first segment file, whose base is position 0.
/// Each frame's session id, bytes 12 to 16 of its header, is set to 0. It is the one
/// field in which members' logs of a span may differ. A follower that joined late
/// holds the span below its live image in its recording of a catch-up replay, and a
/// replay has a session id of its own (`spec/cluster.md`, "The log").
fn log_bytes(member: &Member, end: i64) -> Vec<u8> {
    let (live, live_start, below) = log_recordings(member);
    let at = |position: i64| usize::try_from(position).expect("a position");
    let mut bytes = Vec::with_capacity(at(end));
    let mut position = 0;
    for (id, start, stop) in below.into_iter().chain([(live, live_start, end)]) {
        let stop = stop.min(end);
        if stop <= position {
            continue;
        }
        assert!(
            start <= position,
            "member {}: recording {id} begins at {start}, past {position}",
            member.module.member_id()
        );
        let file = std::fs::read(member.log_dir.join(format!("{id}-0.rec")))
            .expect("the segment file is read");
        bytes.extend_from_slice(&file[at(position)..at(stop)]);
        position = stop;
    }
    assert_eq!(position, end, "member {}", member.module.member_id());
    // Frames lie one after another from position 0, each at its length aligned to 32.
    let mut frame = 0;
    while frame < bytes.len() {
        let length =
            i32::from_le_bytes(bytes[frame..frame + 4].try_into().expect("a frame length"));
        assert!(
            length >= 32,
            "member {}: a frame of length {length} at {frame}",
            member.module.member_id()
        );
        bytes[frame + 12..frame + 16].fill(0);
        frame += usize::try_from(length)
            .expect("a length")
            .next_multiple_of(32);
    }
    assert_eq!(frame, bytes.len(), "member {}", member.module.member_id());
    bytes
}

/// The member's file of its log's recordings, read as a service reads it
/// (`spec/cluster.md`, "The log's recordings"): the live recording's id and start, and
/// the recordings below it, each as its id, its start and its stop.
fn log_recordings(member: &Member) -> (i64, i64, Vec<(i64, i64, i64)>) {
    let id = member.module.member_id();
    let bytes = std::fs::read(member.dir.join(format!("{LOG_RECORDINGS_FILE_PREFIX}{id}")))
        .expect("the member's file of its log's recordings");
    let file = LogRecordings::read(&bytes, id).expect("the file passes its checks");
    (
        file.live_recording_id(),
        file.live_start_position(),
        file.entries()
            .map(|entry| {
                (
                    entry.recording_id,
                    entry.start_position,
                    entry.stop_position,
                )
            })
            .collect(),
    )
}

/// A service beside a follower that left the log and joined it again
/// (`spec/cluster.md`, "The service"; ADR 0103). The member names a new recording,
/// which begins where it joined, its append position, and lists the recording it left
/// below it in its file. A service that had read to that position opens the log there
/// and reads on. A service that starts at 0 reads the recording the member left to
/// its stop, is ended there with nothing reported, opens the log again and reads on
/// in the new recording: every entry once, in order, across both. The other follower
/// joined the log late at the start (`run_until_led_with_a_late_follower`), so once it
/// has joined again its log is three recordings, the first its catch-up's replay, and
/// a service from 0 beside it reads across the three.
#[test]
fn a_service_reads_across_the_recordings_of_a_member_that_joined_the_log_again() {
    let dir = TestDir::new("service-joins-again");
    let members_string = members_string();
    let mut members = start(dir.path(), &members_string);
    let (late, _) = run_until_led_with_a_late_follower(&mut members);
    // The follower in place 1 joined on time, and the one in place 2 late.
    put_on_time_follower(&mut members, late, 1);
    const ENTRIES: i64 = 10;
    offer_entries(&mut members, ENTRIES);
    let first = 96 + ENTRIES * 96;
    run_until(&mut members, "the first entries committed", |members| {
        members
            .iter()
            .all(|m| m.module.commit_position() == first && m.module.append_position() == first)
    });
    // The recording each follower records the live log into, before it leaves.
    let held: Vec<i64> = members[1..]
        .iter()
        .map(|m| m.module.log_recording_id().expect("a follower's recording"))
        .collect();

    // A service on follower 1 reads every entry committed so far.
    let mut client = Cavalry::connect_to(&members[1].dir).expect("the service's client connects");
    let mut log = client
        .committed_log(0)
        .expect("the driver answers")
        .expect("the member has its log");
    let first_recording = log.recording_id();
    // The member's log is one recording, and its file says so.
    assert_eq!(
        log_recordings(&members[1]),
        (first_recording, 0, Vec::new())
    );
    let mut read = Read::new();
    run_until(&mut members, "the service read the first entries", |_| {
        poll_log(&mut log, &mut read);
        log.position() == first
    });

    // One follower at a time leaves the log and joins it again, while the leader's
    // module stands still and the other follower's with it. Two followers that had
    // both timed out would be a quorum with no leader, and would hold a ballot of
    // their own if the leader's answer came later than a nomination delay; one alone
    // can only canvass. A member whose module stood still reads its leader's word
    // first when it runs again, so it counts no timeout for a silence it did not see.
    let term = members[0].module.leadership_term_id();
    let timeouts: Vec<u64> = members[1..]
        .iter()
        .map(|m| m.module.counts().leader_heartbeat_timeouts)
        .collect();

    // First the follower the service is beside: it leaves the log, and the service's
    // handle comes to its end at the position it had read.
    run_until(
        &mut members[1..2],
        "the follower timed out its silent leader and the service's log ended",
        |members| {
            poll_log(&mut log, &mut read);
            log.is_ended()
                && members.iter().all(|m| {
                    m.module.counts().leader_heartbeat_timeouts == timeouts[0] + 1
                        && m.module.election_state() == ElectionState::Canvass
                })
        },
    );
    assert_eq!(log.position(), first);
    assert_eq!(read.len(), usize::try_from(ENTRIES).expect("a count") + 1);
    log.close(&mut client).expect("the handle closes");
    // The leader runs again, and the follower joins the log on a new recording.
    run_until(
        &mut members[..2],
        "the follower joined the log again",
        |members| {
            members
                .iter()
                .all(|m| m.module.election_state() == ElectionState::Closed)
                && members[1]
                    .module
                    .log_recording_id()
                    .is_some_and(|id| id != held[0])
        },
    );

    // Then the follower that joined late at the start, the same way: it reads the
    // leader's word of a moment ago, counts the silence that follows, and leaves.
    run_until(
        &mut members[2..],
        "the late follower timed out its silent leader",
        |members| {
            members.iter().all(|m| {
                m.module.counts().leader_heartbeat_timeouts == timeouts[1] + 1
                    && m.module.election_state() == ElectionState::Canvass
            })
        },
    );
    // The leader runs again beside it, and it joins the log on a new recording.
    members.swap(1, 2);
    run_until(
        &mut members[..2],
        "the late follower joined the log again",
        |members| {
            members
                .iter()
                .all(|m| m.module.election_state() == ElectionState::Closed)
                && members[1]
                    .module
                    .log_recording_id()
                    .is_some_and(|id| id != held[1])
        },
    );
    members.swap(1, 2);
    // Every member runs again, behind the leader it had, in the term it led, and
    // the leader appends more.
    run_until(&mut members, "every member follows the leader", |members| {
        members
            .iter()
            .all(|m| m.module.election_state() == ElectionState::Closed)
    });
    assert_eq!(members[0].module.role(), Role::Leader);
    for member in &members {
        assert_eq!(member.module.leadership_term_id(), term);
    }
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

    // The member's file: the recording it left, from 0 to where it stopped, below the
    // one it names now, which begins there. The counter holds the id the file names.
    let second_recording = members[1]
        .module
        .log_recording_id()
        .expect("the member names its new recording");
    assert_eq!(
        log_recordings(&members[1]),
        (second_recording, first, vec![(first_recording, 0, first)])
    );
    assert_eq!(
        counter(&members[1], CLUSTER_LOG_RECORDING_ID.0),
        second_recording
    );

    // At the new recording's start, the position the service had read to: the service
    // reads on, the ten entries appended after the join and nothing before them.
    let mut log = client
        .committed_log(first)
        .expect("the driver answers")
        .expect("the member has its log");
    assert_eq!(log.recording_id(), second_recording);
    let mut after = Read::new();
    run_until(&mut members, "the service read what came after", |_| {
        poll_log(&mut log, &mut after);
        log.position() == second
    });
    assert_eq!(after.len(), usize::try_from(ENTRIES).expect("a count"));
    assert_eq!(after[0].0, first);
    log.close(&mut client).expect("the handle closes");

    // A service that starts at 0: the recording the member left, to its stop.
    let mut log = client
        .committed_log(0)
        .expect("the driver answers")
        .expect("the member has its log");
    assert_eq!(log.recording_id(), first_recording);
    let mut all = Read::new();
    run_until(
        &mut members,
        "the service read the recording its member left",
        |_| {
            poll_log(&mut log, &mut all);
            log.is_ended()
        },
    );
    assert_eq!(log.position(), first, "it is ended at the recording's stop");
    assert_eq!(
        (log.failed_at(), log.withdrawn_at(), log.faulted()),
        (None, None, None),
        "and whole: every entry the recording holds was delivered"
    );
    assert_eq!(all.len(), usize::try_from(ENTRIES).expect("a count") + 1);
    let ended_at = log.position();
    log.close(&mut client).expect("the handle closes");
    // It opens the log again where the handle ended, and that is the live recording.
    let mut log = client
        .committed_log(ended_at)
        .expect("the driver answers")
        .expect("the member has its log");
    assert_eq!(log.recording_id(), second_recording);
    run_until(
        &mut members,
        "the service read on in the live recording",
        |_| {
            poll_log(&mut log, &mut all);
            log.position() == second
        },
    );
    // Every entry once, in order: what the two services read before, end to end.
    let mut expected = read;
    expected.extend(after);
    assert_eq!(all, expected);
    assert!(!log.is_ended(), "the live recording is followed");
    log.close(&mut client).expect("the handle closes");
    client.close().expect("the service's client closes");

    // The follower that joined late at the start: its file names the recording of its
    // catch-up's replay, from 0, the live recording it left, and the one it names now,
    // each beginning where the one before it stopped.
    let (live, start, below) = log_recordings(&members[2]);
    assert_eq!(Some(live), members[2].module.log_recording_id());
    assert!(
        matches!(below[..], [(_, 0, caught_up), (left, from, stop)]
            if from == caught_up && left == held[1] && stop == first && start == first),
        "member {late}: live {live} from {start}, below {below:?}"
    );
    // A service from 0 beside it reads across the three, handle after handle, and
    // reads what the service beside the other follower read.
    let mut client = Cavalry::connect_to(&members[2].dir).expect("the service's client connects");
    let mut log = client
        .committed_log(0)
        .expect("the driver answers")
        .expect("the member has its log");
    let mut across = Read::new();
    let mut recordings = vec![log.recording_id()];
    run_until(
        &mut members,
        "the service read across the late follower's recordings",
        |_| {
            poll_across(&mut client, &mut log, &mut across);
            if recordings.last() != Some(&log.recording_id()) {
                recordings.push(log.recording_id());
            }
            log.position() == second
        },
    );
    assert_eq!(across, expected);
    assert_eq!(
        recordings,
        below
            .iter()
            .map(|(id, ..)| *id)
            .chain([live])
            .collect::<Vec<_>>()
    );
    log.close(&mut client).expect("the handle closes");
    client.close().expect("the service's client closes");

    // A member that closes takes its file with it.
    let file = members[1].dir.join(format!(
        "{LOG_RECORDINGS_FILE_PREFIX}{}",
        members[1].module.member_id()
    ));
    assert!(file.exists());

    for member in members {
        member.module.close().expect("the member closes");
        member.driver.close().expect("the driver closes");
    }
    assert!(!file.exists(), "the member removed its file when it closed");
}

/// The third rule of order (`spec/cluster.md`, "The service"; ADR 0103): a member
/// writes the file of its log's recordings before it stores a recording's id in
/// `cluster.log_recording_id`. A member whose file cannot be written, a directory in
/// the place of the name it is written under, records the log and counts toward its
/// commits as before, says what could not be written and what to do each duty cycle,
/// and names no recording, so no service opens its log; once the file can be written
/// the next cycle writes it and the counter names the recording. The member is the one
/// with id 2, whichever role the election gives it, and its file is blocked before its
/// first duty cycle. It may be a follower that joined the log late, which this test
/// does not make happen: its file then lists its catch-up's recording below the live
/// one, and the service beside it reads across the two.
#[test]
fn a_member_names_no_recording_until_it_has_written_the_file_of_its_logs_recordings() {
    let dir = TestDir::new("file-before-counter");
    let members_string = members_string();
    let mut members = start(dir.path(), &members_string);
    // Member 2 is in place 1 (`start`), and stays there: nothing here puts the leader
    // first.
    let blocked = members
        .iter()
        .position(|m| m.module.member_id() == 2)
        .expect("member 2");
    let file = members[blocked]
        .dir
        .join(format!("{LOG_RECORDINGS_FILE_PREFIX}2"));
    let tmp = members[blocked].dir.join(format!(
        "{LOG_RECORDINGS_FILE_PREFIX}2{LOG_RECORDINGS_TMP_SUFFIX}"
    ));
    std::fs::create_dir(&tmp).expect("a directory where the file is written");

    // Every member's duty cycle; the blocked member's error is kept, not a failure.
    let run = |members: &mut [Member], what: &str, done: &mut dyn FnMut(&[Member]) -> bool| {
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut last = None;
        while !done(members) {
            assert!(Instant::now() < deadline, "{what}: {:?}", states(members));
            for (index, member) in members.iter_mut().enumerate() {
                match member.module.do_work() {
                    Ok(_) => {}
                    Err(error) if index == blocked => last = Some(error),
                    Err(error) => panic!("member {index}: {error}"),
                }
            }
            std::thread::sleep(Duration::from_micros(200));
        }
        last
    };

    let refused = run(&mut members, "every election closed", &mut |members| {
        members
            .iter()
            .all(|m| m.module.election_state() == ElectionState::Closed)
    })
    .expect("the member whose file cannot be written says so");
    assert!(
        matches!(&refused, RaftError::LogRecordings { path, .. } if *path == tmp),
        "{refused}"
    );
    let text = refused.to_string();
    assert!(text.contains("log-recordings-2.tmp"), "{text}");
    assert!(
        text.contains("Make the driver directory writable")
            && text.contains("docs/reference/cluster.md#the-logs-recordings"),
        "{text}"
    );

    // It records the log and counts toward a commit; it names no recording, and no
    // service finds a log on its driver.
    let leader = members
        .iter()
        .position(|m| m.module.role() == Role::Leader)
        .expect("one member leads");
    let offered_by = Instant::now() + Duration::from_secs(60);
    for index in 0..10u8 {
        loop {
            assert!(
                Instant::now() < offered_by,
                "entry {index} was not taken: {:?}",
                states(&members)
            );
            match members[leader].module.offer(&[index]) {
                Ok(()) => break,
                Err(RaftError::Offer(Refused::BackPressured)) => {
                    for (place, member) in members.iter_mut().enumerate() {
                        // The blocked member's cycle fails and still does its work;
                        // no other member's may fail.
                        if let Err(error) = member.module.do_work() {
                            assert_eq!(place, blocked, "member {place}: {error}");
                        }
                    }
                }
                Err(error) => panic!("{error}"),
            }
        }
    }
    let end = 96 + 10 * 96;
    run(
        &mut members,
        "the entries committed on every member",
        &mut |members| {
            members
                .iter()
                .all(|m| m.module.commit_position() == end && m.module.append_position() == end)
        },
    );
    assert!(members[blocked].module.log_recording_id().is_some());
    assert_eq!(counter(&members[blocked], CLUSTER_LOG_RECORDING_ID.0), -1);
    assert!(!file.exists());
    // Each cycle that could not write the file is counted, on this member alone:
    // what tells it from a member whose recording never started.
    let unwritten = counter(&members[blocked], CLUSTER_LOG_RECORDINGS_UNWRITTEN.0);
    assert!(unwritten > 0, "no failed write was counted");
    for (index, member) in members.iter().enumerate() {
        if index != blocked {
            assert_eq!(counter(member, CLUSTER_LOG_RECORDINGS_UNWRITTEN.0), 0);
        }
    }
    let mut client =
        Cavalry::connect_to(&members[blocked].dir).expect("the service's client connects");
    assert!(
        client
            .committed_log(0)
            .expect("the driver answers")
            .is_none(),
        "no member names a recording on this driver"
    );

    // The directory goes: the next cycle writes the file, and then the counter names
    // the recording the file names.
    std::fs::remove_dir(&tmp).expect("the directory is removed");
    let after = run(
        &mut members,
        "the member named its recording",
        &mut |members| counter(&members[blocked], CLUSTER_LOG_RECORDING_ID.0) >= 0,
    );
    assert!(
        after.is_none(),
        "no cycle failed once the file could be written"
    );
    let named = members[blocked]
        .module
        .log_recording_id()
        .expect("the member's recording");
    assert_eq!(
        counter(&members[blocked], CLUSTER_LOG_RECORDING_ID.0),
        named
    );
    // The file names the recording the counter names. The member's log is that one
    // recording, from 0, unless it was a follower that joined the log late and caught
    // up through a replay: the file then names the replay's recording below it, from 0
    // to where the live one begins (ADR 0103).
    let (live, start, below) = log_recordings(&members[blocked]);
    assert_eq!(live, named);
    let counts = members[blocked].module.counts();
    if counts.catchups == 0 {
        assert_eq!((start, below), (0, Vec::new()), "{counts:?}");
    } else {
        assert!(
            matches!(below[..], [(replayed, 0, stop)]
                if replayed != named && stop == start && start > 0),
            "live {live} from {start}, below {below:?}, {counts:?}"
        );
    }
    assert!(!tmp.exists(), "the file was renamed from its other name");
    // The count stands where the last failed cycle left it.
    let counted = counter(&members[blocked], CLUSTER_LOG_RECORDINGS_UNWRITTEN.0);
    assert!(counted >= unwritten);
    for member in members.iter_mut() {
        member.module.do_work().expect("the member's duty cycle");
    }
    assert_eq!(
        counter(&members[blocked], CLUSTER_LOG_RECORDINGS_UNWRITTEN.0),
        counted,
        "a cycle that has nothing to write counts nothing"
    );
    let mut log = client
        .committed_log(0)
        .expect("the driver answers")
        .expect("the member has its log");
    let mut read = Read::new();
    run(&mut members, "the service read the log", &mut |_| {
        poll_across(&mut client, &mut log, &mut read);
        log.position() == end
    });
    assert_eq!(read.len(), 11);
    log.close(&mut client).expect("the handle closes");
    client.close().expect("the service's client closes");

    for member in members {
        member.module.close().expect("the member closes");
        member.driver.close().expect("the driver closes");
    }
}

/// The log buffers under a driver directory's `publications/`, by name.
fn publication_logs(dir: &Path) -> Vec<PathBuf> {
    let mut logs: Vec<PathBuf> = std::fs::read_dir(dir.join("publications"))
        .expect("the driver's publications")
        .map(|entry| entry.expect("an entry").path())
        .collect();
    logs.sort();
    logs
}

/// A service whose read of the replay's image stops on a fault is told so by the poll
/// that meets it, once, and can ask again for as long as it holds the handle
/// (`CommittedLog::faulted`; ADR 0080). Before that read, a service that did not keep
/// the one poll's answer took this end for a whole one: `failed_at` and `withdrawn_at`
/// are both `None` for it.
///
/// The test makes the fault. The service's replay plays into a log buffer of its own
/// in the member's driver directory, the one file that appears under `publications/`
/// when the log is opened. The service reads five of eleven committed entries, and the
/// test then changes one byte of the sixth as the replay wrote it, its frame's version,
/// to one no reader accepts (`spec/frame.md`, "What a reader rejects"), which a log's
/// reader reports under the reason `frame`. The frame is below the service's bound, so
/// the next poll reads it, fails the check and retires the image. The member's
/// recording is as it was, so a log opened again where the handle ended reads on.
///
/// The other follower joined the log late (`run_until_led_with_a_late_follower`), and
/// the service is beside the one that joined on time: a handle opened at 0 beside the
/// late one would replay its catch-up's recording, which ends after the term's first
/// entry, and the eleven are read here through one handle.
#[test]
fn a_service_whose_image_faulted_reads_the_fault_for_the_life_of_its_handle() {
    use std::os::unix::fs::FileExt;

    const ENTRIES: u32 = 10;
    const READ_FIRST: usize = 5;
    let dir = TestDir::new("service-faulted");
    let members_string = members_string();
    let mut members = start(dir.path(), &members_string);
    let (late, _) = run_until_led_with_a_late_follower(&mut members);
    put_on_time_follower(&mut members, late, 1);
    let member_dir = members[1].dir.clone();
    let mut client = Cavalry::connect_to(&member_dir).expect("the service's client connects");
    let before = publication_logs(&member_dir);
    let mut opened = None;
    run_until(&mut members, "the service opened its log", |_| {
        if opened.is_none() {
            opened = client.committed_log(0).expect("the driver answers");
        }
        opened.is_some()
    });
    let mut log = opened.expect("the member has its log");
    assert_eq!(
        Some(log.recording_id()),
        members[1].module.log_recording_id()
    );
    let replayed: Vec<PathBuf> = publication_logs(&member_dir)
        .into_iter()
        .filter(|path| !before.contains(path))
        .collect();
    assert_eq!(replayed.len(), 1, "the replay's one log: {replayed:?}");
    let replay_log = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&replayed[0])
        .expect("the replay's log opens");

    // The term's first entry and ten more, committed on every member. The service
    // reads the first five of the eleven and takes the commit position as its bound.
    offer(&mut members, &[0, 1, 2], 0, ENTRIES);
    let end = 96 + i64::from(ENTRIES) * 96;
    let stopped = 96 * i64::try_from(READ_FIRST).expect("a count");
    let mut read = Read::new();
    let mut session_id = None;
    run_until(
        &mut members,
        "the service read five entries under its bound",
        |_| {
            let limit = READ_FIRST - read.len();
            log.poll(
                |entry, header, position| {
                    session_id = Some(header.session_id());
                    read.push((position, entry.to_vec()));
                },
                limit,
            )
            .expect("the committed log polls");
            read.len() == READ_FIRST && log.commit_position() == end
        },
    );
    let session_id = session_id.expect("an entry's header");
    assert_eq!(log.position(), stopped);
    assert_eq!(log.faulted(), None, "a live log has no fault: {log:?}");
    assert!(!log.is_ended());

    // The sixth entry's frame, once the replay has written it: term 0 is the file's
    // first bytes, and the frame begins at the position the service stopped at. Its
    // length is the frame's first field and its version the byte after.
    let at = u64::try_from(stopped).expect("a position");
    // The replay plays what the member's recorder has synced, which the member asks
    // for in its duty cycle, so the members run while the test waits for the frame.
    run_until(&mut members, "the replay wrote the sixth entry", |_| {
        let mut length = [0u8; 4];
        replay_log
            .read_exact_at(&mut length, at)
            .expect("the frame's length");
        i32::from_le_bytes(length) >= 32
    });
    replay_log
        .write_all_at(&[9], at + 4)
        .expect("the version byte");

    // The poll that meets it says so, once.
    let fault = log
        .poll(
            |_, _, position| panic!("an entry at {position} from a frame that fails its check"),
            64,
        )
        .expect_err("the image's fault");
    assert_eq!(
        fault,
        Fault::Image {
            session_id,
            position: stopped,
            reason: "frame",
        }
    );
    // And the handle says so for as long as it is held, whatever is polled after.
    assert_eq!(log.faulted(), Some(fault));
    assert!(log.is_ended(), "{log:?}");
    assert_eq!(
        (log.failed_at(), log.withdrawn_at()),
        (None, None),
        "{log:?}"
    );
    for _ in 0..50 {
        assert_eq!(
            log.poll(
                |_, _, position| panic!("an entry at {position} after the fault"),
                64
            ),
            Ok(0)
        );
        assert_eq!(log.faulted(), Some(fault));
    }
    assert_eq!(
        log.position(),
        stopped,
        "what it delivered ends where it read to"
    );
    assert_eq!(client.closed(), None, "the client is open");

    // The service opens the log again where the handle ended and reads on, the six
    // entries it was not given and five the member commits now, with no fault.
    log.close(&mut client).expect("the handle closes");
    let mut log = client
        .committed_log(stopped)
        .expect("the driver answers")
        .expect("the member has its log");
    offer(&mut members, &[0, 1, 2], ENTRIES, 5);
    let mut after = Read::new();
    run_until(&mut members, "the service read what came after", |_| {
        poll_log(&mut log, &mut after);
        log.position() == end + 5 * 96
    });
    assert_eq!(after.len(), 11 - READ_FIRST + 5);
    assert_eq!(after[0].0, stopped);
    assert_eq!(log.faulted(), None);
    log.close(&mut client).expect("the handle closes");
    client.close().expect("the service's client closes");

    for member in members {
        member.module.close().expect("the member closes");
        member.driver.close().expect("the driver closes");
    }
}
