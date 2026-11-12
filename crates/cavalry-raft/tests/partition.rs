//! Three members over three drivers on the loopback, every datagram between them
//! through a relay that loses and reorders data frames and can cut a member off
//! (`tests/relay/mod.rs`; `CLAUDE.md`, gate 3; ADR 0077): a follower cut off and
//! joined again, and a leader cut off, replaced, and joined again as a follower with
//! the log the others committed. Each test holds that the cluster stays available
//! while a quorum remains and that a service beside every member reads every
//! committed entry, once and in order.
//!
//! # How long a member takes to join again
//!
//! Each test prints it and asserts no bound on it. A member cut off for the image
//! liveness timeout, 10 s, has lost its images of the others' streams, and they have
//! lost theirs of its own. A driver creates no second image for a session whose image
//! it retired until the image cool-down has passed, 10 s more (`spec/udp.md`, "The
//! receiver", step 5), and counts what it drops meanwhile in
//! `receiver.dropped_cool_down`. These tests mend the cut as soon as the member has
//! noticed it, at the start of that cool-down, so the member hears its peers again
//! about 10 s after the mend, the longest the wait can be, and joins within half a
//! second of that. A cut mended later in the cool-down, or after it, is joined
//! sooner.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cavalry_client::{Cavalry, CommittedLog};
use cavalry_driver::Driver;
use cavalry_raft::{ConsensusModule, ElectionState, RaftError, Refused, Role};

mod ports;
mod relay;
use ports::free_ports;
use relay::{Faults, Relay};

/// What the relay does to every link for the whole of each test: one data frame in 23
/// lost, one in 7 held back behind the next.
const FAULTS: Faults = Faults {
    drop_every: 23,
    hold_every: 7,
};

/// The length in the log of one entry these tests offer, and of a new leadership term
/// event: a 32-byte frame header, a 32-byte session header or the event's 64 bytes,
/// and the 8-byte payload, aligned to 32.
const FRAME: i64 = 96;

struct TestDir(PathBuf);

impl TestDir {
    fn new(test: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "cavalry-raft-partition-{}-{test}",
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

struct Member {
    driver: Driver,
    module: ConsensusModule,
    dir: PathBuf,
}

/// Starts the three members, each with the members string the relay gives it.
fn start(dir: &Path, relay: &Relay) -> Vec<Member> {
    [1, 0, 2]
        .into_iter()
        .map(|id| {
            let driver_dir = dir.join(format!("member-{id}"));
            let log_dir = dir.join(format!("member-{id}-log"));
            let driver = Driver::start(&driver_dir, &log_dir).expect("the driver starts");
            let module =
                ConsensusModule::start(&driver_dir, &log_dir, relay.members_string(id), id)
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

/// True when every member of `members` has closed its election in one term above
/// `above` and exactly one of them leads.
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

/// Puts the member that leads first.
fn leader_first(members: &mut [Member]) {
    let leader = members
        .iter()
        .position(|m| m.module.role() == Role::Leader)
        .expect("one member leads");
    members.swap(0, leader);
}

/// The leader, `members[0]`, appends `count` entries numbered from `from`, a
/// back-pressured offer made again once every member has run.
fn offer(members: &mut [Member], from: u32, count: u32) {
    let deadline = Instant::now() + Duration::from_secs(60);
    for i in from..from + count {
        let payload = [i.to_le_bytes(), (!i).to_le_bytes()].concat();
        loop {
            assert!(
                Instant::now() < deadline,
                "the leader's offer of entry {i} was back-pressured for 60 s: {:?}",
                states(members)
            );
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
}

/// True when every member of `members` has committed and holds the log to `end`.
fn committed(members: &[Member], end: i64) -> bool {
    members
        .iter()
        .all(|m| m.module.commit_position() == end && m.module.append_position() == end)
}

/// What a service read: each entry's position and its bytes.
type Read = Vec<(i64, Vec<u8>)>;

/// The committed log of the member at `at`, from 0 to `end`, as a service beside it
/// reads it (`spec/cluster.md`, "The service"): one handle for each recording of the
/// member's log, each read to its whole end and the log opened again where it ended.
fn read_log(members: &mut [Member], at: usize, end: i64) -> Read {
    let mut client = Cavalry::connect_to(&members[at].dir).expect("the service's client connects");
    let mut read = Read::new();
    let mut position = 0;
    while position < end {
        let mut log: CommittedLog = client
            .committed_log(position)
            .expect("the driver answers")
            .expect("the member has its log");
        run_until(members, "the service read a recording of the log", |_| {
            log.poll(
                |entry, _, position| read.push((position, entry.to_vec())),
                64,
            )
            .expect("the committed log polls");
            log.is_ended() || log.position() >= end
        });
        assert_eq!(
            (log.failed_at(), log.withdrawn_at(), log.faulted()),
            (None, None, None),
            "member {}: a handle that ends, ends whole",
            members[at].module.member_id()
        );
        assert!(
            log.position() > position,
            "member {}: a handle opened at {position} read nothing",
            members[at].module.member_id()
        );
        position = log.position();
        log.close(&mut client).expect("the handle closes");
    }
    client.close().expect("the service's client closes");
    read
}

/// The numbers of the entries a service read, a new leadership term event read as
/// `None`: an entry these tests offer is its number and the number's complement.
fn numbers(read: &Read) -> Vec<Option<u32>> {
    read.iter()
        .map(|(_, entry)| {
            let payload = &entry[entry.len() - 8..];
            let number = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
            let check = u32::from_le_bytes([payload[4], payload[5], payload[6], payload[7]]);
            (entry.len() == 40 && check == !number).then_some(number)
        })
        .collect()
}

fn close(members: Vec<Member>) {
    for member in members {
        member.module.close().expect("the member closes");
        member.driver.close().expect("the driver closes");
    }
}

/// A follower is cut off from both other members. The leader and the follower that
/// remains go on committing. The one cut off hears no leader for the leader heartbeat
/// timeout, leaves the log and canvasses; alone, it is no quorum, proposes no term and
/// unseats nobody. When the cut is mended it is told the term it left, joins the log
/// again and holds what was committed while it was away. The leader led throughout,
/// and a service beside each member reads the same entries at the same positions.
#[test]
fn a_follower_cut_off_joins_again_and_holds_what_was_committed_without_it() {
    let dir = TestDir::new("follower-cut-off");
    let relay = Relay::start(&free_ports(15), FAULTS);
    let mut members = start(dir.path(), &relay);
    run_until(&mut members, "a leader", |members| led(members, -1));
    leader_first(&mut members);
    let leader = members[0].module.member_id();
    let term = members[0].module.leadership_term_id();
    offer(&mut members, 0, 100);
    let first = FRAME + 100 * FRAME;
    run_until(&mut members, "the first hundred committed", |members| {
        committed(members, first)
    });

    let away = members[2].module.member_id();
    // What the member counted before the cut: a first election whose vote was split
    // makes a candidate of a member that then follows.
    let candidacies = members[2].module.counts().candidacies;
    relay.cut(away);
    let cut_at = Instant::now();
    offer(&mut members, 100, 100);
    let second = first + 100 * FRAME;
    run_until(
        &mut members,
        "the two that remain committed and the one cut off canvasses",
        |members| {
            let counts = members[2].module.counts();
            committed(&members[..2], second)
                && counts.leader_heartbeat_timeouts + counts.log_images_lost >= 1
                && members[2].module.election_state() == ElectionState::Canvass
        },
    );
    let noticed = cut_at.elapsed();
    assert_eq!(members[2].module.append_position(), first);

    relay.mend(away);
    let mended_at = Instant::now();
    run_until(&mut members, "the three hold one log", |members| {
        led(members, term - 1) && committed(members, second)
    });
    let joined = mended_at.elapsed();
    offer(&mut members, 200, 10);
    let third = second + 10 * FRAME;
    run_until(&mut members, "the three commit", |members| {
        committed(members, third)
    });

    assert_eq!(members[0].module.member_id(), leader, "it led throughout");
    assert_eq!(members[0].module.leadership_term_id(), term);
    for member in &members {
        let counts = member.module.counts();
        let id = member.module.member_id();
        if id == away {
            assert_eq!(
                counts.candidacies, candidacies,
                "member {id} was a candidate while it was cut off or after: {counts:?}"
            );
        }
        assert_eq!(counts.truncations_refused, 0, "member {id}: {counts:?}");
        assert_eq!(counts.log_truncations, 0, "member {id}: {counts:?}");
        assert_eq!(
            counts.leader_step_downs + counts.leader_quorum_losses,
            0,
            "member {id}: {counts:?}"
        );
    }
    let expected: Vec<Option<u32>> = std::iter::once(None).chain((0..210).map(Some)).collect();
    let logs: Vec<Read> = (0..3).map(|at| read_log(&mut members, at, third)).collect();
    for (at, log) in logs.iter().enumerate() {
        let id = members[at].module.member_id();
        assert_eq!(numbers(log), expected, "member {id}");
        assert_eq!(log, &logs[0], "member {id}");
    }
    for member in &members {
        println!(
            "partition: member {}: {:?}",
            member.module.member_id(),
            member.module.counts()
        );
    }
    let totals = relay.totals();
    println!(
        "partition: follower {away} was cut off; it canvassed {noticed:?} after the cut and \
         held the log {joined:?} after the mend; the relay: {totals:?}"
    );
    assert!(totals.forwarded > 0 && totals.dropped > 0 && totals.held > 0 && totals.cut > 0);
    close(members);
}

/// The leader is cut off from both followers, and appends five entries no follower
/// receives. The two followers hear no leader for the leader heartbeat timeout and
/// elect one of them, which commits a hundred entries with the other. The member cut
/// off hears from no quorum, gives up the lead and canvasses. When the cut is mended
/// it is told a term based below the end of its log: it truncates the five entries
/// no quorum held, replicates what the two committed and joins the log as a
/// follower. A service beside each member reads the same entries at the same
/// positions, the five among none of them.
#[test]
fn a_leader_cut_off_is_replaced_and_joins_again_with_the_log_the_others_committed() {
    let dir = TestDir::new("leader-cut-off");
    let relay = Relay::start(&free_ports(15), FAULTS);
    let mut members = start(dir.path(), &relay);
    run_until(&mut members, "a leader", |members| led(members, -1));
    leader_first(&mut members);
    let old = members[0].module.member_id();
    let old_term = members[0].module.leadership_term_id();
    offer(&mut members, 0, 100);
    let first = FRAME + 100 * FRAME;
    run_until(&mut members, "the first hundred committed", |members| {
        committed(members, first)
    });

    relay.cut(old);
    let cut_at = Instant::now();
    offer(&mut members, 100, 5);
    run_until(
        &mut members,
        "the two that remain have a leader and the one cut off gave up the lead",
        |members| {
            led(&members[1..], old_term)
                && members[0].module.role() == Role::Follower
                && members[0].module.counts().leader_quorum_losses == 1
        },
    );
    let replaced = cut_at.elapsed();
    assert_eq!(members[0].module.commit_position(), first);
    assert_eq!(members[0].module.append_position(), first + 5 * FRAME);

    leader_first(&mut members);
    let new = members[0].module.member_id();
    assert_ne!(new, old);
    offer(&mut members, 200, 100);
    let second = first + FRAME + 100 * FRAME;
    run_until(&mut members, "the two commit a hundred", |members| {
        members
            .iter()
            .filter(|m| m.module.member_id() != old)
            .all(|m| m.module.commit_position() == second && m.module.append_position() == second)
    });
    let new_term = members[0].module.leadership_term_id();

    relay.mend(old);
    let mended_at = Instant::now();
    run_until(&mut members, "the three hold one log", |members| {
        led(members, new_term - 1) && committed(members, second)
    });
    let joined = mended_at.elapsed();
    offer(&mut members, 300, 10);
    let third = second + 10 * FRAME;
    run_until(&mut members, "the three commit", |members| {
        committed(members, third)
    });

    assert_eq!(
        members[0].module.member_id(),
        new,
        "it led from its election"
    );
    assert_eq!(members[0].module.leadership_term_id(), new_term);
    for member in &members {
        let counts = member.module.counts();
        let id = member.module.member_id();
        assert_eq!(counts.truncations_refused, 0, "member {id}: {counts:?}");
        assert_eq!(
            counts.log_truncations,
            u64::from(id == old),
            "member {id}: {counts:?}"
        );
    }
    let expected: Vec<Option<u32>> = std::iter::once(None)
        .chain((0..100).map(Some))
        .chain(std::iter::once(None))
        .chain((200..310).map(Some))
        .collect();
    let logs: Vec<Read> = (0..3).map(|at| read_log(&mut members, at, third)).collect();
    for (at, log) in logs.iter().enumerate() {
        let id = members[at].module.member_id();
        assert_eq!(numbers(log), expected, "member {id}");
        assert_eq!(log, &logs[0], "member {id}");
    }
    for member in &members {
        println!(
            "partition: member {}: {:?}",
            member.module.member_id(),
            member.module.counts()
        );
    }
    let totals = relay.totals();
    println!(
        "partition: leader {old} was cut off in term {old_term}; member {new} led term \
         {new_term} {replaced:?} after the cut; member {old} held the log {joined:?} after \
         the mend; the relay: {totals:?}"
    );
    assert!(totals.forwarded > 0 && totals.dropped > 0 && totals.held > 0 && totals.cut > 0);
    close(members);
}
