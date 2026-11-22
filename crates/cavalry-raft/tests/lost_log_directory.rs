//! A member that lost its log directory comes back with no vote on what it forgot
//! (`spec/cluster.md`, "A member that lost its log"; ADR 0145; CLAUDE.md, invariant 1).
//! Three members in memory, the check of `elected_cluster` after every step: what a
//! leader committed is never lost and never replaced.
//!
//! The history work item 286 names: an entry committed by two members, one of the two
//! started again on an empty log directory, the other stopped, and the third, which
//! lacks the entry, running. A member that had forgotten its votes would vote the
//! third into a term, and the cluster would commit a log without the entry. The member
//! with no log withholds its vote while a voter it has not heard from may hold more
//! than the candidate, so no leader is elected until the stopped member comes back;
//! then its log leads, and every member ends on one log that holds the entry.

mod elected_cluster;

use cavalry_raft::memory::Cluster;
use elected_cluster::{Safety, elected, settled};

/// The members of `ids` other than `leader`.
fn followers(ids: &[i32], leader: i32) -> Vec<i32> {
    ids.iter().copied().filter(|&id| id != leader).collect()
}

/// Offers `count` entries to the leader, each run a millisecond, and returns where the
/// last ends.
fn offer(cluster: &mut Cluster, safety: &mut Safety, from: u32, count: u32) -> i64 {
    let mut end = 0;
    for i in from..from + count {
        end = cluster.offer(&i.to_le_bytes()).expect("the leader appends");
        safety.run(cluster, 1);
    }
    end
}

#[test]
fn a_member_that_lost_its_log_votes_no_leader_in_that_lacks_what_it_had_acknowledged() {
    let ids = [0, 1, 2];
    let mut safety = Safety::new(&ids);
    let (mut cluster, leader) = elected(7, &mut safety);
    let others = followers(&ids, leader);
    let (holder, behind) = (others[0], others[1]);
    // Ten entries on all three, then ten more that the third does not receive: the
    // leader and the holder commit them, two of three.
    let shared = offer(&mut cluster, &mut safety, 0, 10);
    safety.run(&mut cluster, 1_000);
    assert_eq!(cluster.commit_position(behind), Some(shared));
    cluster.cut(behind);
    let committed = offer(&mut cluster, &mut safety, 10, 10);
    safety.run(&mut cluster, 1_000);
    assert_eq!(cluster.commit_position(leader), Some(committed));
    assert_eq!(cluster.append_position(holder), Some(committed));
    assert!(cluster.append_position(behind) < Some(committed));

    // The holder loses its log directory and starts again on an empty one; the
    // leader stops; the third, which lacks the last ten entries, runs and is heard.
    cluster.halt(leader);
    safety.wipe(&mut cluster, holder);
    cluster.heal(behind);
    safety.run(&mut cluster, 30_000);
    assert_eq!(
        cluster.leader(),
        None,
        "no leader without the entries committed"
    );
    let wiped = cluster.counts(holder).expect("a member");
    assert!(wiped.votes_withheld > 0, "{wiped:?}");
    assert_eq!(wiped.votes_for, 0, "{wiped:?}");
    assert_eq!(wiped.candidacies, 0, "{wiped:?}");
    assert!(
        cluster.counts(behind).expect("a member").candidacies > 0,
        "the third stood, and was not elected"
    );

    // The leader comes back: its log is at least every other's, the member with no
    // log votes for it, and the three end on one log that holds the entries.
    cluster.resume(leader);
    safety.run_until(
        &mut cluster,
        60_000,
        "the three settle behind one leader and hold what was committed",
        |cluster| {
            settled(cluster, &ids)
                && ids
                    .iter()
                    .all(|&id| cluster.append_position(id) >= Some(committed))
        },
    );
    let floor = cluster.member(holder).expect("a member").vote_floor();
    assert!(
        floor >= committed,
        "the floor {floor} holds every entry committed before it came back, {committed}"
    );
    let log = cluster.log_to(leader, committed);
    for &id in &ids {
        assert_eq!(cluster.log_to(id, committed), log, "member {id}");
    }
    // Past its floor it is a voter as any other: the next election counts it.
    let next = offer(&mut cluster, &mut safety, 20, 5);
    safety.run(&mut cluster, 1_000);
    for &id in &ids {
        assert_eq!(cluster.commit_position(id), Some(next), "member {id}");
    }
}

/// Two of three lose their log directories while the third holds the whole log: both
/// are down long enough for the leader to lose its quorum and hold an election, and
/// both come back on empty log directories. They hear all three voters, and the
/// third's log is at least each, so they vote it in: withholding never stops an
/// election in which every voter is heard. Two lost logs of three is past what Raft
/// holds, a quorum's record gone, so the check of every step stops at the loss; what
/// the test holds after it is that the cluster elects the member that kept the log and
/// ends on that log.
#[test]
fn two_members_that_lost_their_logs_vote_in_the_one_that_holds_the_log() {
    let ids = [0, 1, 2];
    let mut safety = Safety::new(&ids);
    let (mut cluster, leader) = elected(11, &mut safety);
    let committed = offer(&mut cluster, &mut safety, 0, 20);
    safety.run(&mut cluster, 1_000);
    let term = cluster
        .member(leader)
        .expect("a member")
        .leadership_term_id();
    let lost = followers(&ids, leader);
    for &id in &lost {
        cluster.halt(id);
    }
    // Past the leader heartbeat timeout, 10 s: the leader's active members are below
    // a quorum, and it holds an election.
    cluster.run_ms(15_000);
    assert_eq!(cluster.leader(), None);
    for &id in &lost {
        cluster.wipe(id);
    }
    let mut steps = 0;
    while !(settled(&cluster, &ids)
        && cluster.leader() == Some(leader)
        && lost
            .iter()
            .all(|&id| cluster.append_position(id) >= Some(committed)))
    {
        assert!(
            steps < 60_000,
            "the one that holds the log leads again and the two hold it: not within 60 s"
        );
        cluster.step();
        steps += 1;
    }
    assert!(
        cluster
            .member(leader)
            .expect("a member")
            .leadership_term_id()
            > term,
        "elected again, in a later term"
    );
    for &id in &lost {
        let counts = cluster.counts(id).expect("a member");
        assert!(counts.votes_for > 0, "member {id} voted: {counts:?}");
        assert_eq!(counts.candidacies, 0, "member {id}: {counts:?}");
        let member = cluster.member(id).expect("a member");
        assert!(member.vote_floor() >= committed, "member {id}");
        assert_eq!(
            cluster.log_to(id, committed),
            cluster.log_to(leader, committed),
            "member {id}"
        );
    }
}

/// A member that lost its log follows no leader that a later term has deposed. A leads
/// term t and can reach nobody; `voter` still reaches A, so its canvass positions keep A
/// quorate. `voter` and `parted` elect `parted` in t + 1 and commit entries that A lacks.
/// Then `voter` loses its log, `parted` stops, and A reaches `voter` again, all within the
/// leader heartbeat timeout, while A's election is still closed. A answers the new
/// member's canvass, since no term is below −1. A member that took A's log end as its
/// vote floor would follow A, and A and it are a quorum. Entries offered to A would be
/// committed where `parted`'s committed entries of t + 1 lie, and a committed entry
/// would be replaced. The member with no log hears every voter first, and `parted`, which
/// holds the later term, is stopped, so it takes no leader (`spec/cluster.md`, "A member
/// that lost its log"; ADR 0145). Once `parted` runs again, the member takes it and the
/// three end on its log.
#[test]
fn a_member_that_lost_its_log_follows_no_leader_a_later_term_deposed() {
    let ids = [0, 1, 2];
    let mut safety = Safety::new(&ids);
    let (mut cluster, old) = elected(7, &mut safety);
    let others = followers(&ids, old);
    let (parted, voter) = (others[0], others[1]);
    offer(&mut cluster, &mut safety, 0, 10);
    // One more entry that `voter` receives and does not sync, so that `parted`'s log is
    // ahead of its own and `parted` is the candidate of t + 1: a request vote of `voter`'s
    // would reach A and end A's lead.
    cluster.pause_sync(voter);
    let held_by_old = offer(&mut cluster, &mut safety, 10, 1);
    safety.run(&mut cluster, 100);
    assert_eq!(cluster.commit_position(old), Some(held_by_old));
    let term = cluster.member(old).expect("a member").leadership_term_id();

    // A reaches nobody; `voter` still reaches A.
    cluster.cut_between(old, parted);
    cluster.cut_one_way(old, voter);
    safety.run_until(&mut cluster, 30_000, "`parted` leads t + 1", |cluster| {
        cluster.role(parted) == Some(cavalry_raft::Role::Leader)
    });
    cluster.resume_sync(voter);
    let pair = [parted, voter];
    safety.run_until(&mut cluster, 5_000, "`voter` follows `parted`", |cluster| {
        settled(cluster, &pair)
    });
    assert!(
        cluster
            .member(parted)
            .expect("a member")
            .leadership_term_id()
            > term
    );
    let committed = offer(&mut cluster, &mut safety, 20, 10);
    safety.run(&mut cluster, 100);
    assert_eq!(cluster.commit_position(parted), Some(committed));
    assert!(committed > held_by_old);
    let log = cluster.log_to(parted, committed);
    // A has not heard that it was deposed: it still leads t, its election closed.
    let deposed = cluster.member(old).expect("a member");
    assert_eq!(deposed.role(), cavalry_raft::Role::Leader);
    assert_eq!(deposed.leadership_term_id(), term);
    assert_eq!(
        deposed.election_state(),
        cavalry_raft::ElectionState::Closed
    );

    // `voter` loses its log, `parted` stops, and A reaches `voter` again.
    safety.wipe(&mut cluster, voter);
    cluster.halt(parted);
    cluster.heal_between(old, voter);
    safety.run(&mut cluster, 1_000);
    // Entries offered to A are appended to its log, and none is committed.
    for i in 30..40u32 {
        if cluster.offer(&i.to_le_bytes()).is_ok() {
            safety.run(&mut cluster, 1);
        }
    }
    safety.run(&mut cluster, 5_000);
    assert_eq!(cluster.commit_position(old), Some(held_by_old));
    let wiped = cluster.member(voter).expect("a member");
    assert_eq!(wiped.vote_floor(), -1, "it took no leader");
    assert_ne!(wiped.leader_member_id(), old);
    let counts = cluster.counts(voter).expect("a member");
    assert!(counts.leaders_withheld > 0, "{counts:?}");

    // `parted` runs again and A hears it: `parted`'s log leads, and the three end on it.
    // The commit `parted` knows counted the acknowledgement of the log `voter` lost, so
    // a quorum's append positions are below it until `voter` has caught up. The check
    // of every step stops here, as in the test of two lost logs; what is held is the
    // end.
    cluster.resume(parted);
    cluster.heal_between(old, parted);
    let mut steps = 0;
    while !(settled(&cluster, &ids)
        && ids
            .iter()
            .all(|&id| cluster.append_position(id) >= Some(committed)))
    {
        assert!(
            steps < 60_000,
            "the three settle behind one leader and hold what `parted` committed: not within 60 s"
        );
        cluster.step();
        steps += 1;
    }
    assert_eq!(cluster.leader(), Some(parted));
    for &id in &ids {
        assert_eq!(cluster.log_to(id, committed), log, "member {id}");
    }
    let floor = cluster.member(voter).expect("a member").vote_floor();
    assert!(floor >= committed, "the floor {floor} holds {committed}");
}

/// ADR 0145's cost, held: a new cluster in which one member holds nothing yet when the
/// first leader stops elects no leader until that leader runs again. The member that
/// holds nothing cannot tell its empty log from one it lost, so it withholds, and the
/// other member that runs cannot win alone. Nothing committed is lost meanwhile, and
/// once the leader runs again the three end on its log.
#[test]
fn a_new_clusters_member_that_holds_nothing_when_its_leader_stops_waits_for_the_leader() {
    let ids = [0, 1, 2];
    let mut safety = Safety::new(&ids);
    let mut cluster = Cluster::with_election(&ids, 3).expect("three members");
    // One member hears nothing from the start: the two others elect after the startup
    // canvass timeout, 60 s, and commit ten entries between them.
    let late = 2;
    cluster.cut(late);
    let others = [0, 1];
    safety.run_until(&mut cluster, 120_000, "two members elect", |cluster| {
        settled(cluster, &others)
    });
    let leader = cluster.leader().expect("a leader");
    let committed = offer(&mut cluster, &mut safety, 0, 10);
    safety.run(&mut cluster, 1_000);
    assert_eq!(
        cluster.append_position(late),
        Some(0),
        "the late member holds nothing"
    );
    let holder = others
        .into_iter()
        .find(|&id| id != leader)
        .expect("a member");
    assert_eq!(cluster.append_position(holder), Some(committed));

    // The leader stops, and the late member is heard at last.
    cluster.halt(leader);
    cluster.heal(late);
    safety.run(&mut cluster, 30_000);
    assert_eq!(
        cluster.leader(),
        None,
        "no leader while the late member withholds"
    );
    let withheld = cluster.counts(late).expect("a member");
    assert!(withheld.votes_withheld > 0, "{withheld:?}");
    assert_eq!(withheld.candidacies, 0, "{withheld:?}");

    cluster.resume(leader);
    safety.run_until(
        &mut cluster,
        60_000,
        "the three settle and hold what was committed",
        |cluster| {
            settled(cluster, &ids)
                && ids
                    .iter()
                    .all(|&id| cluster.append_position(id) >= Some(committed))
        },
    );
    let log = cluster.log_to(holder, committed);
    for &id in &ids {
        assert_eq!(cluster.log_to(id, committed), log, "member {id}");
    }
}
