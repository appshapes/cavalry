//! A standby over a driver (`spec/cluster.md`, "Standbys" and "Changing a standby's
//! mark"; ADR 0134): two voters and a standby, three members over three drivers on
//! the loopback. While it is a standby, the member follows the log, takes the
//! snapshot its leader asks for, and commits nothing by its own record. Then it is
//! promoted, by starting the members again with a string that gives it a vote, and
//! it leads: work item 230's "a standby promoted leads". The test holds the change
//! to invariant 1: a service beside each member reads every entry that was
//! committed before the change, during it and after it, in order.
//!
//! The clusters in memory of `tests/standby.rs` hold the rules of a change; this
//! holds the members string, the counters and the log over real drivers.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cavalry_client::{Action, Cavalry, ClusterService, Entry, NULL_LENGTH, NULL_POSITION};
use cavalry_driver::Driver;
use cavalry_log::snapshots::{ListedSnapshot, SnapshotsFile};
use cavalry_raft::{ConsensusModule, ElectionState, RaftError, Refused, Role};
use cavalry_shm::counters::CountersReader;
use cavalry_shm::counters::generated::{
    CLUSTER_CANDIDACIES, CLUSTER_STANDBY, CLUSTER_STANDBY_VOTE_REQUESTS, CLUSTER_TERMS_LED,
    CLUSTER_VOTERS, CLUSTER_VOTES_AGAINST, CLUSTER_VOTES_FOR,
};

mod ports;
use ports::free_ports;

/// The stream the test replays a snapshot's recording into: one no member uses.
const REPLAY_STREAM_ID: i32 = 2301;

/// The member that is a standby in the first string.
const STANDBY: i32 = 2;

struct TestDir(PathBuf);

impl TestDir {
    fn new(test: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "cavalry-raft-standby-{}-{test}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("the test's directory");
        Self(path)
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

/// Two members strings of three members on the loopback, one mark apart: the first
/// marks member 2 a standby, and the second gives every member a vote.
fn members_strings() -> (String, String) {
    let ports = free_ports(15);
    let string = |standby: Option<i32>| {
        (0..3)
            .map(|member: i32| {
                let first = usize::try_from(member).expect("a member's place") * 5;
                let endpoints: Vec<String> = ports[first..first + 5]
                    .iter()
                    .map(|port| format!("127.0.0.1:{port}"))
                    .collect();
                let mark = if standby == Some(member) {
                    ",standby"
                } else {
                    ""
                };
                format!("{member},{}{mark}", endpoints.join(","))
            })
            .collect::<Vec<_>>()
            .join("|")
    };
    (string(Some(STANDBY)), string(None))
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

/// Stops `member`, its module and then its driver.
fn stop(member: Member) {
    member.module.close().expect("the member closes");
    member.driver.close().expect("the driver closes");
}

/// The place in `members` of the member with `id`.
fn place(members: &[Member], id: i32) -> usize {
    members
        .iter()
        .position(|member| member.module.member_id() == id)
        .expect("the member runs")
}

/// A service beside a member, whose program holds the payload of every session
/// message it read, in order.
struct Service {
    client: Cavalry,
    service: ClusterService,
    state: Vec<u8>,
}

impl Service {
    fn beside(member: &Member) -> Self {
        let client = Cavalry::connect_to(&member.dir).expect("the service's client");
        let service = client.cluster_service();
        Self {
            client,
            service,
            state: Vec::new(),
        }
    }

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

    fn close(mut self) {
        self.service
            .close(&mut self.client)
            .expect("the service closes");
        self.client.close().expect("the service's client closes");
    }
}

/// Drives every member's duty cycle, and every service's poll, until `done`, or
/// fails with `what`. A service whose poll ended at a snapshot gives its program's
/// state, once its member has listed its own snapshot of that position.
fn run_until(
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
                .map(|m| format!("{:?}", m.module))
                .collect::<Vec<_>>()
        );
        cycle(members, services);
    }
}

/// One duty cycle of every member and one poll of every service.
fn cycle(members: &mut [Member], services: &mut [Service]) {
    let mut work = 0;
    for member in members.iter_mut() {
        work += member.module.do_work().expect("the member's duty cycle");
    }
    for service in services.iter_mut() {
        match service.service.snapshot_due() {
            None => work += service.poll(),
            Some(position) => {
                let listed = members.iter().all(|member| {
                    member
                        .module
                        .snapshot_position()
                        .is_some_and(|own| own >= position)
                });
                if listed {
                    let state = service.state.clone();
                    service
                        .service
                        .snapshot(&mut service.client, &state)
                        .expect("the service's snapshot");
                    work += 1;
                }
            }
        }
    }
    if work == 0 {
        std::thread::sleep(Duration::from_micros(200));
    }
}

/// Runs the members for `time`, whatever they do.
fn run_for(members: &mut [Member], time: Duration) {
    let end = Instant::now() + time;
    while Instant::now() < end {
        cycle(members, &mut []);
    }
}

/// Whether every member of `members` has closed its election in one term above
/// `above`, with one of them leading.
fn led(members: &[Member], above: i64) -> bool {
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
}

/// The id of the member that leads.
fn leader(members: &[Member]) -> i32 {
    members
        .iter()
        .find(|m| m.module.role() == Role::Leader)
        .expect("one member leads")
        .module
        .member_id()
}

/// The leader appends `count` entries numbered from `from`, a back-pressured offer
/// made again once every member has run.
fn offer_entries(members: &mut [Member], from: i64, count: i64) {
    let leads = place(members, leader(members));
    for i in from..from + count {
        loop {
            match members[leads].module.offer(&i.to_le_bytes()) {
                Ok(()) => break,
                Err(RaftError::Offer(Refused::BackPressured)) => cycle(members, &mut []),
                Err(other) => panic!("{other}"),
            }
        }
    }
}

/// The first `count` entries' payloads, as a service's program holds them.
fn entries(count: i64) -> Vec<u8> {
    (0..count).flat_map(i64::to_le_bytes).collect()
}

/// The value of the member's counter of `type_id`, found in its driver's counters
/// file by the type and the member id in the key.
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
    // How far the replay's image has been read: the driver withdraws the image once
    // its one subscriber has read it to the end of stream, which can be before this
    // client has read `replay stopped`, so the position is kept from the polls that
    // still had the image.
    let mut reached = 0;
    loop {
        let read = subscription
            .poll(|bytes, _| messages.push(bytes.to_vec()), 16)
            .expect("the replay's image is read");
        for image in subscription.images() {
            reached = reached.max(image.position());
        }
        if let Some((end, _)) = replay.ended()
            && reached >= end
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the replay of recording {recording_id} did not end: {} messages read to \
             position {reached}",
            messages.len()
        );
        if read == 0 {
            std::thread::sleep(Duration::from_micros(200));
        }
    }
    drop(subscription);
    client.close().expect("the client closes");
    messages
}

/// Two voters and a standby over three drivers. The standby follows: the two voters
/// elect one of themselves, every member says the same two voters, and the standby
/// has cast no vote and been asked for none. It takes the snapshot its leader asks
/// for, of the position the voters' are of and with the bytes they hold. Its record
/// commits nothing: with the voter that follows stopped, an entry that the leader and
/// the standby have recorded stays above the commit position, and is committed when
/// that voter runs again.
///
/// Then the standby is promoted. It is started with the string that gives it a vote
/// and follows the leader, which still counts two voters. The two older voters stop,
/// and one starts again with the new string: it canvasses, waiting for every member,
/// and the promoted member is the one that can be a candidate. It leads, on that
/// voter's vote, and the two commit. The last voter starts with the new string and
/// follows it. A service beside each member has then read every entry, those
/// committed while the member was a standby among them, in one order.
#[test]
fn a_standby_follows_takes_its_leaders_snapshot_and_leads_once_it_is_promoted() {
    let dir = TestDir::new("promoted");
    let (marked, promoted) = members_strings();
    let mut members: Vec<Member> = (0..3)
        .map(|id| start_member(dir.path(), &marked, id))
        .collect();
    run_until(
        &mut members,
        &mut [],
        "the two voters elect a leader",
        |members, _| led(members, -1),
    );
    let first = leader(&members);
    assert_ne!(first, STANDBY, "a standby leads nothing");
    let follower = 1 - first;
    for member in &members {
        let id = member.module.member_id();
        assert_eq!(counter(member, CLUSTER_VOTERS.0), 0b011, "member {id}");
        assert_eq!(
            counter(member, CLUSTER_STANDBY.0),
            i64::from(id == STANDBY),
            "member {id}"
        );
    }
    let standby = &members[place(&members, STANDBY)];
    assert_eq!(standby.module.role(), Role::Follower);
    for count in [
        CLUSTER_CANDIDACIES,
        CLUSTER_TERMS_LED,
        CLUSTER_VOTES_FOR,
        CLUSTER_VOTES_AGAINST,
        CLUSTER_STANDBY_VOTE_REQUESTS,
    ] {
        assert_eq!(counter(standby, count.0), 0, "counter {}", count.0);
    }

    // The standby records the log and knows what is committed.
    offer_entries(&mut members, 0, 3);
    let mut end = 0;
    run_until(
        &mut members,
        &mut [],
        "three entries are committed",
        |members, _| {
            end = members[0].module.append_position();
            end > 0
                && members
                    .iter()
                    .all(|m| m.module.append_position() == end && m.module.commit_position() == end)
        },
    );

    // It takes the snapshot its leader asks for, as the voters do.
    let leads = place(&members, first);
    loop {
        match members[leads].module.snapshot() {
            Ok(()) => break,
            Err(RaftError::Offer(Refused::BackPressured)) => cycle(&mut members, &mut []),
            Err(other) => panic!("{other}"),
        }
    }
    let mut position = -1;
    run_until(
        &mut members,
        &mut [],
        "every member lists the snapshot",
        |members, _| {
            position = members[0].module.snapshot_position().unwrap_or(-1);
            position > end
                && members
                    .iter()
                    .all(|m| m.module.snapshot_position() == Some(position))
        },
    );
    let bytes: Vec<Vec<Vec<u8>>> = members
        .iter()
        .map(|member| {
            let list = listed(member);
            let newest = list.last().expect("a listed snapshot");
            assert_eq!(newest.log_position, position);
            replayed(member, newest.recording_id)
        })
        .collect();
    assert!(!bytes[0].is_empty());
    assert_eq!(bytes[1], bytes[0]);
    assert_eq!(bytes[2], bytes[0]);

    // Its record commits nothing. The voter that follows stops, and the leader's
    // next entry reaches the standby and is not committed.
    stop(members.remove(place(&members, follower)));
    let committed = members[place(&members, first)].module.commit_position();
    offer_entries(&mut members, 3, 1);
    let mut appended = 0;
    run_until(
        &mut members,
        &mut [],
        "the standby recorded the entry",
        |members, _| {
            appended = members[place(members, first)].module.append_position();
            appended > committed
                && members[place(members, STANDBY)].module.append_position() == appended
        },
    );
    run_for(&mut members, Duration::from_millis(500));
    for member in &members {
        assert_eq!(
            member.module.commit_position(),
            committed,
            "member {}",
            member.module.member_id()
        );
    }
    members.push(start_member(dir.path(), &marked, follower));
    run_until(
        &mut members,
        &mut [],
        "the voters committed the entry",
        |members, _| {
            led(members, -1)
                && members.iter().all(|m| {
                    m.module.append_position() == appended && m.module.commit_position() == appended
                })
        },
    );
    let term = members[0].module.leadership_term_id();

    // The promotion. The standby is started with the string that gives it a vote,
    // and follows the leader, whose string still marks it.
    stop(members.remove(place(&members, STANDBY)));
    members.push(start_member(dir.path(), &promoted, STANDBY));
    run_until(
        &mut members,
        &mut [],
        "the promoted member follows",
        |members, _| led(members, term - 1),
    );
    assert_eq!(leader(&members), first);
    let voter = &members[place(&members, STANDBY)];
    assert_eq!(counter(voter, CLUSTER_VOTERS.0), 0b111);
    assert_eq!(counter(voter, CLUSTER_STANDBY.0), 0);
    assert_eq!(
        counter(&members[place(&members, first)], CLUSTER_VOTERS.0),
        0b011
    );

    // The two older voters stop. One starts again with the new string and waits in
    // canvass for every member; the promoted member has lost its leader, is a quorum
    // candidate with that voter's report, and leads on its vote.
    stop(members.remove(place(&members, first)));
    stop(members.remove(place(&members, follower)));
    members.push(start_member(dir.path(), &promoted, follower));
    run_until(
        &mut members,
        &mut [],
        "the promoted member leads",
        |members, _| led(members, term),
    );
    assert_eq!(leader(&members), STANDBY, "the promoted member leads");
    // At least one ballot and one term: a ballot that a loaded host made late is
    // taken again. The voter beside it asked for no vote at all.
    let leads = &members[place(&members, STANDBY)];
    assert!(counter(leads, CLUSTER_TERMS_LED.0) >= 1);
    assert!(counter(leads, CLUSTER_CANDIDACIES.0) >= 1);
    assert_eq!(
        counter(&members[place(&members, follower)], CLUSTER_CANDIDACIES.0),
        0
    );

    // It commits with that voter, and the last voter follows it when it starts.
    offer_entries(&mut members, 4, 2);
    members.push(start_member(dir.path(), &promoted, first));
    let mut services: Vec<Service> = members.iter().map(Service::beside).collect();
    let all = entries(6);
    run_until(
        &mut members,
        &mut services,
        "every service has read the six entries",
        |members, services| {
            let end = members[0].module.append_position();
            led(members, term)
                && members
                    .iter()
                    .all(|m| m.module.append_position() == end && m.module.commit_position() == end)
                && services.iter().all(|service| service.state == all)
        },
    );
    assert_eq!(leader(&members), STANDBY);
    for member in &members {
        let id = member.module.member_id();
        assert_eq!(counter(member, CLUSTER_VOTERS.0), 0b111, "member {id}");
        assert_eq!(counter(member, CLUSTER_STANDBY.0), 0, "member {id}");
    }
    for service in services {
        service.close();
    }
    for member in members {
        stop(member);
    }
}
