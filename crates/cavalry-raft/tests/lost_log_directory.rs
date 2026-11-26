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

use cavalry_raft::ElectionState;
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
    // Joined the live log, it holds every entry committed before it came back, and
    // its floor is cleared (ADR 0161).
    assert_eq!(cluster.member(holder).expect("a member").vote_floor(), -1);
    assert_eq!(
        cluster
            .counts(holder)
            .expect("a member")
            .vote_floors_cleared,
        1
    );
    let log = cluster.log_to(leader, committed);
    for &id in &ids {
        assert_eq!(cluster.log_to(id, committed), log, "member {id}");
    }
    // Its floor cleared, it is a voter as any other: the next election counts it.
    let next = offer(&mut cluster, &mut safety, 20, 5);
    safety.run(&mut cluster, 1_000);
    for &id in &ids {
        assert_eq!(cluster.commit_position(id), Some(next), "member {id}");
    }
}

/// A follower loses its log directory and starts again on an empty one while the leader
/// and the other follower run on, as `docs/how-to/run-a-cluster.md` brings a member
/// back. Neither of the two holds an election: the leader stays quorate with the other
/// follower, whose election is closed, so it sends no canvass position of its own. It
/// answers the new member's canvass position, whose log is empty, with its own
/// (`spec/cluster.md`, "A member that lost its log", "Taking a leader"; ADR 0145). With
/// that answer the new member has heard every voter, none naming a term above the
/// leader's, and it takes the leader's new leadership term. Its vote floor is the end of
/// the leader's log, it catches up, and a commit made while the other follower is cut
/// counts its acknowledgement.
#[test]
fn a_follower_that_lost_its_log_follows_the_leader_while_the_other_two_run_on() {
    let ids = [0, 1, 2];
    let mut safety = Safety::new(&ids);
    let (mut cluster, leader) = elected(5, &mut safety);
    let others = followers(&ids, leader);
    let (wiped, other) = (others[0], others[1]);
    let committed = offer(&mut cluster, &mut safety, 0, 20);
    safety.run(&mut cluster, 1_000);
    for &id in &ids {
        assert_eq!(cluster.commit_position(id), Some(committed), "member {id}");
    }
    let term = cluster
        .member(leader)
        .expect("a member")
        .leadership_term_id();
    let elections = |cluster: &Cluster| {
        [leader, other].map(|id| cluster.counts(id).expect("a member").elections)
    };
    let before = elections(&cluster);

    safety.wipe(&mut cluster, wiped);
    safety.run_until(
        &mut cluster,
        5_000,
        "the member that lost its log follows the leader and holds what was committed",
        |cluster| settled(cluster, &ids) && cluster.append_position(wiped) >= Some(committed),
    );
    assert_eq!(cluster.leader(), Some(leader));
    assert_eq!(
        elections(&cluster),
        before,
        "neither of the two held an election"
    );
    assert_eq!(
        cluster
            .member(leader)
            .expect("a member")
            .leadership_term_id(),
        term,
        "the same term"
    );
    // Joined the live log, it holds every entry committed before it came back, and
    // its floor is cleared (ADR 0161).
    assert_eq!(cluster.member(wiped).expect("a member").vote_floor(), -1);
    assert_eq!(
        cluster.counts(wiped).expect("a member").vote_floors_cleared,
        1
    );
    assert_eq!(
        cluster.log_to(wiped, committed),
        cluster.log_to(leader, committed)
    );

    // With the other follower cut, the leader commits only with the acknowledgement of
    // the member that lost its log.
    cluster.cut(other);
    let next = offer(&mut cluster, &mut safety, 20, 5);
    safety.run(&mut cluster, 1_000);
    assert_eq!(cluster.commit_position(leader), Some(next));
    assert!(cluster.append_position(other) < Some(next));
    assert_eq!(cluster.commit_position(wiped), Some(next));
    cluster.heal(other);
    safety.run_until(
        &mut cluster,
        5_000,
        "the three hold the next entries",
        |cluster| {
            settled(cluster, &ids)
                && ids
                    .iter()
                    .all(|&id| cluster.commit_position(id) == Some(next))
        },
    );
    assert_eq!(elections(&cluster), before, "still no election");
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
        assert_eq!(member.vote_floor(), -1, "member {id}: its floor is cleared");
        assert_eq!(counts.vote_floors_cleared, 1, "member {id}");
        assert_eq!(
            cluster.log_to(id, committed),
            cluster.log_to(leader, committed),
            "member {id}"
        );
    }
}

/// ADR 0145's accepted case, held now (work item 476): two members of three lose their
/// log directories while the third, which holds the log, is stopped. Before, the two
/// heard no report that holds a log, voted as a new cluster's members do, and after
/// the startup canvass timeout elected between them an empty history, so that what the
/// cluster committed was gone from every member that ran. Now a member that holds
/// nothing, its log empty and no candidate term stored, as a lost log directory leaves
/// it, stands and votes only once every voter has reported: the two elect no leader in
/// 120 s while the third is stopped. Once it runs, its report holds a log, so they
/// withhold, it leads in a later term, and the two catch up to everything committed.
#[test]
fn two_members_that_lost_their_logs_while_the_third_is_stopped_wait_for_it() {
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
    cluster.halt(leader);
    for &id in &lost {
        cluster.wipe(id);
    }
    // Twice the startup canvass timeout: before work item 476 the two elected by now.
    cluster.run_ms(120_000);
    assert_eq!(cluster.leader(), None, "the two elect no leader");
    for &id in &lost {
        assert_eq!(
            cluster.append_position(id),
            Some(0),
            "member {id} holds nothing"
        );
        let counts = cluster.counts(id).expect("a member");
        assert_eq!(counts.candidacies, 0, "member {id}: {counts:?}");
        assert_eq!(
            cluster.member(id).expect("a member").voters_unheard(),
            1,
            "member {id} waits for the one it has not heard"
        );
    }
    cluster.resume(leader);
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
        assert_eq!(
            cluster.log_to(id, committed),
            cluster.log_to(leader, committed),
            "member {id} holds what the cluster committed"
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
    follows_no_deposed_leader(false);
}

/// The schedule above, with the canvass positions `parted` sends while it canvasses for
/// t + 1 up to 10 s late, so that some of them, which name t, the term `parted` followed,
/// are still on their way to `voter` when it loses its log. A member that read them would
/// hear `parted` name no term above A's, and take A. A member started again reads no
/// message that a peer sent before that start, as on the new driver it starts on
/// ([`Cluster::wipe`]; `spec/cluster.md`, "Taking a leader"; ADR 0145), so it takes no
/// leader while `parted` is stopped, as above.
#[test]
fn a_member_that_lost_its_log_reads_no_canvass_position_sent_before_it_lost_it() {
    follows_no_deposed_leader(true);
}

/// The schedule of the two tests above; `late_canvass` makes the canvass positions sent
/// while `parted` canvasses for t + 1 up to 10 s late.
fn follows_no_deposed_leader(late_canvass: bool) {
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
    if late_canvass {
        safety.run_until(&mut cluster, 30_000, "`parted` canvasses", |cluster| {
            cluster.election_state(parted) == Some(cavalry_raft::ElectionState::Canvass)
        });
        cluster.set_delivery(0, 10_000);
        safety.run(&mut cluster, 300);
        cluster.set_delivery(0, 0);
    }
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
    // Joined the live log, it holds every entry committed before it came back, and
    // its floor is cleared (ADR 0161).
    assert_eq!(cluster.member(voter).expect("a member").vote_floor(), -1);
    assert_eq!(
        cluster.counts(voter).expect("a member").vote_floors_cleared,
        1
    );
}

/// ADR 0145's cost, held: a new cluster in which one member holds nothing yet when the
/// first leader stops elects no leader until that leader runs again. The member that
/// holds nothing cannot tell its empty log from one it lost, so it withholds, and the
/// other member that runs cannot win alone. Nothing committed is lost meanwhile, and
/// once the leader runs again the three end on its log.
///
/// The member that holds nothing is heard from the start but hears nothing, as a host
/// that drops the packets it receives does. A new cluster elects only once every voter
/// has reported (work item 476), so a member that never ran would keep the first leader
/// from being elected at all.
#[test]
fn a_new_clusters_member_that_holds_nothing_when_its_leader_stops_waits_for_the_leader() {
    let ids = [0, 1, 2];
    let mut safety = Safety::new(&ids);
    let mut cluster = Cluster::with_election(&ids, 3).expect("three members");
    // One member hears nothing from the start, and the log does not reach it: the two
    // others hear all three, elect, and commit ten entries between them.
    let late = 2;
    let others = [0, 1];
    for other in others {
        cluster.cut_one_way(other, late);
    }
    safety.run_until(&mut cluster, 3_000, "two members elect", |cluster| {
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

    // The leader stops, and the late member hears the others at last.
    cluster.halt(leader);
    for other in others {
        cluster.heal_between(other, late);
    }
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

/// The frame ADR 0145 left open (work item 411; ADR 0161; `spec/cluster.md`, "Taking a
/// leader"): a new leadership term that the leader wrote before an entry the member
/// acknowledged, that its sender held back, and that the member reads after it lost its
/// log. Such a frame is one the member's empty log can read: one that answered a report
/// of an empty log, written when the member had lost its log before.
///
/// A term begins, and its leader's log ends at `f`. The member loses its log directory a
/// first time, and the leader's answer to its canvass, at `f`, is held back; the member
/// follows on a later one. The third member then stops hearing the leader, one way, and the leader appends
/// entries that the member acknowledges, committed by the two. The member loses its log
/// again, and the held frame reaches its new run while the third still names the
/// leader's term: the member takes the leader with a vote floor of `f`, below the
/// entries, and replicates the log from the leader, one entry a step as a replay over a
/// driver does over time. The leader stops once the member's log has passed `f`. Under ADR 0145's rule a member at its floor votes, and it and the third,
/// neither holding the entries, would elect one of them and lose what was committed.
/// Under ADR 0161's it withholds until it has joined a leader's live log as well, so no
/// leader is elected until the leader comes back; then the member catches up, joins,
/// clears its floor, and every member holds the entries.
#[test]
fn a_held_back_new_leadership_term_gives_a_member_that_lost_its_log_no_vote_until_it_joins_the_live_log()
 {
    let ids = [0, 1, 2];
    let mut safety = Safety::new(&ids);
    let (mut cluster, first) = elected(7, &mut safety);
    offer(&mut cluster, &mut safety, 0, 10);
    safety.run(&mut cluster, 1_000);

    // A new term: the first leader is parted from the other two, which elect one of
    // them, and then it is healed and follows.
    let others = followers(&ids, first);
    cluster.cut_between(first, others[0]);
    cluster.cut_between(first, others[1]);
    safety.run_until(
        &mut cluster,
        30_000,
        "a leader of the next term",
        |cluster| {
            cluster
                .leader()
                .is_some_and(|leader| leader != first && settled(cluster, &others))
        },
    );
    let leader = cluster.leader().expect("a leader");
    let member = followers(&others, leader)[0];
    let third = first;
    cluster.heal_between(third, others[0]);
    cluster.heal_between(third, others[1]);
    safety.run_until(&mut cluster, 30_000, "the three settle", |cluster| {
        settled(cluster, &ids)
    });
    let term = cluster
        .member(leader)
        .expect("a member")
        .leadership_term_id();
    let floor = cluster.append_position(leader).expect("a member");

    // The member loses its log a first time; the leader's answer to its empty log, at
    // `floor`, is held back, and it follows on a later one.
    cluster.hold_back_new_leadership_term(leader, member);
    safety.wipe(&mut cluster, member);
    safety.run_until(
        &mut cluster,
        30_000,
        "the member follows again",
        |cluster| settled(cluster, &ids),
    );
    let held = cluster.held_back();
    assert_eq!(
        held,
        vec![(leader, member, floor, term)],
        "held at the leader's log end then"
    );

    // The third stops hearing the leader, and the leader appends entries the member
    // acknowledges: the two commit them, and the third lacks them.
    cluster.cut_one_way(leader, third);
    let committed = offer(&mut cluster, &mut safety, 10, 10);
    safety.run(&mut cluster, 500);
    assert_eq!(cluster.commit_position(leader), Some(committed));
    assert_eq!(cluster.append_position(member), Some(committed));
    assert!(cluster.append_position(third) < Some(committed));

    // The member loses its log again, the leader kept from it until the held frame is
    // released, so that its answers to the member's canvass, at the end of its log, do
    // not come first. The member hears the third, which still follows the leader's
    // term, and then the held frame.
    // Replays bring one entry a step, as a replay over a driver brings its frames over
    // time, so that the member's log passes through the floor on its way to the entries.
    cluster.set_replay_rate(1);
    cluster.cut_between(member, leader);
    safety.wipe(&mut cluster, member);
    safety.run_until(&mut cluster, 5_000, "the member canvasses", |cluster| {
        cluster.election_state(member) == Some(cavalry_raft::ElectionState::Canvass)
    });
    safety.run(&mut cluster, 300);
    cluster.release_held_back(member);
    cluster.heal_between(member, leader);
    safety.run_until(
        &mut cluster,
        5_000,
        "the member takes the leader on the held frame, and its log reaches the floor",
        |cluster| {
            cluster.member(member).expect("a member").vote_floor() == floor
                && cluster.append_position(member) >= Some(floor)
        },
    );
    assert!(
        cluster.append_position(member) < Some(committed),
        "its log lacks the entries"
    );
    cluster.halt(leader);
    safety.run(&mut cluster, 30_000);
    assert_eq!(
        cluster.leader(),
        None,
        "no leader without the entries committed"
    );
    let counts = cluster.counts(member).expect("a member");
    assert_eq!(counts.votes_for, 0, "{counts:?}");
    assert_eq!(counts.candidacies, 0, "{counts:?}");

    // The leader comes back: the member catches up, joins its live log, and clears its
    // floor, and every member holds the entries.
    cluster.set_replay_rate(usize::MAX);
    cluster.resume(leader);
    cluster.heal_between(leader, third);
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
    assert_eq!(cluster.member(member).expect("a member").vote_floor(), -1);
    assert!(
        cluster
            .counts(member)
            .expect("a member")
            .vote_floors_cleared
            >= 1,
        "the floor was cleared once the member joined"
    );
    let log = cluster.log_to(leader, committed);
    for &id in &ids {
        assert_eq!(cluster.log_to(id, committed), log, "member {id}");
    }
}

/// Work item 463, ADR 0163: a member that lost its log takes its leader on a frame held
/// back from before its start, below entries its earlier run acknowledged from a
/// replay, and that run's report of them reaches the leader late. The floor answer
/// keeps the member from voting until its log holds them.
///
/// The member loses its log, and the leader's answer to its empty log is held back.
/// The leader's live sender stands still, as one does that no receiver has answered,
/// while the leader appends entries that its recording holds: the third lacks them, and
/// the member's next run has them from a replay of the leader's recording, slowly. One
/// report of that run is held on its way, and the rest are lost. The member loses its
/// log again, takes the leader on the held frame with a floor below the entries, and
/// catches up only to where the live sender stands, where its live image joins. The
/// leader's first floor answer to the earlier run, at the floor, was held back too, and
/// reaches the new run with the held frame. Then the held report reaches the leader,
/// which commits the entries with it, and the leader stops.
///
/// Under ADR 0161's rule alone the member clears its floor at the join, below the
/// entries, and votes, and the check of every step finds the leader that it and the
/// third elect without them. Under ADR 0163's the member also waits for a floor answer
/// of its present run, at the leader's append position, which its log has not
/// reached, and ignores the answer to its earlier run, which names that run's session:
/// taken, it would clear the floor at the join as ADR 0161's rule does. So no leader is
/// elected until the leader comes back.
#[test]
fn a_floor_answer_keeps_a_member_from_voting_before_it_holds_what_a_late_report_commits() {
    let ids = [0, 1, 2];
    let mut safety = Safety::new(&ids);
    let (mut cluster, first) = elected(7, &mut safety);
    offer(&mut cluster, &mut safety, 0, 10);
    safety.run(&mut cluster, 1_000);

    // A new term, as in the held-back frame's test above: the first leader is parted
    // from the other two, which elect one of them, and then it is healed and follows.
    let others = followers(&ids, first);
    cluster.cut_between(first, others[0]);
    cluster.cut_between(first, others[1]);
    safety.run_until(
        &mut cluster,
        30_000,
        "a leader of the next term",
        |cluster| {
            cluster
                .leader()
                .is_some_and(|leader| leader != first && settled(cluster, &others))
        },
    );
    let leader = cluster.leader().expect("a leader");
    let member = followers(&others, leader)[0];
    let third = first;
    cluster.heal_between(third, others[0]);
    cluster.heal_between(third, others[1]);
    safety.run_until(&mut cluster, 30_000, "the three settle", |cluster| {
        settled(cluster, &ids)
    });
    let floor = cluster.append_position(leader).expect("a member");

    // 1. The member loses its log, and the leader's answer to its empty log, at the
    // floor, is held back.
    cluster.hold_back_new_leadership_term(leader, member);
    safety.wipe(&mut cluster, member);
    safety.run_until(&mut cluster, 5_000, "the answer held", |cluster| {
        !cluster.held_back().is_empty()
    });

    // The member follows on a later answer, and asks for a floor answer, as a member
    // that holds a floor does: the leader's first answer to this run, written before
    // the entries below, is held on its way too.
    cluster.set_replay_rate(1);
    cluster.hold_back_floor_answer(leader, member);
    safety.run_until(&mut cluster, 30_000, "a floor answer held", |cluster| {
        !cluster.held_back_answers().is_empty()
    });
    let stale = cluster.held_back_answers()[0];
    assert!(
        stale.2 <= floor,
        "an answer to this run, below the entries: {stale:?}"
    );

    // 2. The live sender stands still at the floor while the leader appends entries
    // that its recording holds: the third lacks them, and the member's run has them
    // only from a replay, one entry a step.
    cluster.hold_sender(leader, Some(floor));
    let end = offer(&mut cluster, &mut safety, 10, 40);

    // 3. The member follows on a later answer and replays the entries. Its reports stop
    // reaching the leader once its catch-up replay runs, so that the leader counts few
    // of them and never stops the replay; one report of every entry is held on its
    // way.
    safety.run_until(
        &mut cluster,
        30_000,
        "the member catches up in the leader's term",
        |cluster| {
            cluster.election_state(member) == Some(cavalry_raft::ElectionState::FollowerCatchup)
        },
    );
    cluster.cut_one_way(member, leader);
    safety.run_until(
        &mut cluster,
        30_000,
        "the member replays the entries",
        |cluster| cluster.append_position(member) >= Some(end),
    );
    cluster.hold_back_append_position(member, leader);
    safety.run_until(&mut cluster, 5_000, "a report held", |cluster| {
        !cluster.held_back_reports().is_empty()
    });
    let reported = cluster.held_back_reports()[0].2;
    assert_eq!(reported, end, "a report of every entry");
    assert!(cluster.commit_position(leader) <= Some(floor));

    // 4 and 5. The member loses its log again and takes the leader on the held frame,
    // with a floor below the entries.
    cluster.cut_between(member, leader);
    safety.wipe(&mut cluster, member);
    safety.run_until(&mut cluster, 5_000, "the member canvasses", |cluster| {
        cluster.election_state(member) == Some(cavalry_raft::ElectionState::Canvass)
    });
    safety.run(&mut cluster, 300);
    cluster.release_held_back(member);
    cluster.heal_between(member, leader);
    safety.run(&mut cluster, 5);
    assert!(
        cluster.held_back_answers().is_empty(),
        "the held answer was delivered"
    );
    let ignored_at_release = cluster
        .counts(member)
        .expect("a member")
        .floor_answers_ignored;

    // 6. It catches up to where the live sender stands and joins the live log there,
    // below the report.
    safety.run_until(
        &mut cluster,
        10_000,
        "the member joins the live log below the report",
        |cluster| {
            cluster
                .member(member)
                .is_some_and(|core| core.leadership_term_id() >= 0)
                && cluster.election_state(member) == Some(cavalry_raft::ElectionState::Closed)
        },
    );
    safety.run(&mut cluster, 200);
    assert!(
        cluster.append_position(member) < Some(reported),
        "its log lacks what its earlier run reported"
    );

    // 7. The report reaches the leader, which commits the entries with it.
    cluster.release_held_back(leader);
    safety.run_until(
        &mut cluster,
        2_000,
        "the leader commits with it",
        |cluster| cluster.commit_position(leader) >= Some(reported),
    );

    // 8. The leader stops: the member withholds, and the third alone elects nobody.
    cluster.halt(leader);
    safety.run(&mut cluster, 30_000);
    assert_eq!(cluster.leader(), None, "no leader without the entries");
    assert_eq!(
        ignored_at_release, 1,
        "the answer to its earlier run, released with the held frame, is ignored at once"
    );
    let core = cluster.member(member).expect("a member");
    assert_eq!(core.vote_floor(), floor, "it still holds its floor");
    assert!(
        core.floor_answer() >= reported,
        "its leader answered at its log's end"
    );
    let counts = cluster.counts(member).expect("a member");
    assert_eq!(counts.votes_for, 0, "{counts:?}");
    assert!(
        cluster.counts(leader).expect("a member").floor_answers_sent >= 1,
        "the leader answered"
    );

    // The leader comes back and its sender sends again: the member and the third catch
    // up, the member clears its floor, and every member holds the entries.
    cluster.set_replay_rate(usize::MAX);
    cluster.hold_sender(leader, None);
    cluster.resume(leader);
    cluster.heal_between(member, leader);
    safety.run_until(
        &mut cluster,
        60_000,
        "the three settle and hold the entries",
        |cluster| {
            settled(cluster, &ids)
                && ids
                    .iter()
                    .all(|&id| cluster.append_position(id) >= Some(end))
        },
    );
    assert_eq!(cluster.member(member).expect("a member").vote_floor(), -1);
}

/// Work item 463, ADR 0163: the floor answer carries the leader's append position as it
/// reads it when it writes the answer, and not as its last duty cycle read it.
///
/// The eight steps above, with the entries appended while the leader's module stands
/// still. The leader appends them after its duty cycle has read its append position,
/// and its module then stalls, as one does that an fsync, a throttle or a pause holds,
/// while its driver runs on: its recording syncs the entries and its catch-up replay
/// brings them to the member's earlier run, whose report of them is held on its way. The
/// member loses its log again and takes the leader on the held frame. When the leader's
/// module runs again, it reads the member's ask before its duty cycle. An answer at the
/// position its last duty cycle read lies below the entries, and the member would clear
/// its floor at the join, below them, and vote, as under ADR 0161's rule alone. The
/// answer is at the position its recording has synced, past the entries, so no leader
/// is elected until the leader comes back.
#[test]
fn a_floor_answer_holds_what_the_leaders_recording_synced_after_its_last_duty_cycle() {
    let ids = [0, 1, 2];
    let mut safety = Safety::new(&ids);
    let (mut cluster, first) = elected(7, &mut safety);
    offer(&mut cluster, &mut safety, 0, 10);
    safety.run(&mut cluster, 1_000);

    // A new term, as in the eight steps.
    let others = followers(&ids, first);
    cluster.cut_between(first, others[0]);
    cluster.cut_between(first, others[1]);
    safety.run_until(
        &mut cluster,
        30_000,
        "a leader of the next term",
        |cluster| {
            cluster
                .leader()
                .is_some_and(|leader| leader != first && settled(cluster, &others))
        },
    );
    let leader = cluster.leader().expect("a leader");
    let member = followers(&others, leader)[0];
    let third = first;
    cluster.heal_between(third, others[0]);
    cluster.heal_between(third, others[1]);
    safety.run_until(&mut cluster, 30_000, "the three settle", |cluster| {
        settled(cluster, &ids)
    });
    let floor = cluster.append_position(leader).expect("a member");

    // 1. The member loses its log, and the leader's answer to its empty log, at the
    // floor, is held back.
    cluster.hold_back_new_leadership_term(leader, member);
    safety.wipe(&mut cluster, member);
    safety.run_until(&mut cluster, 5_000, "the answer held", |cluster| {
        !cluster.held_back().is_empty()
    });

    // 2. The member follows on a later answer and catches up from a replay, one entry a
    // step, while the live sender stands still at the floor. Its reports stop reaching
    // the leader, so that the leader never stops the replay, which goes on following
    // the leader's recording.
    cluster.set_replay_rate(1);
    cluster.hold_sender(leader, Some(floor));
    safety.run_until(
        &mut cluster,
        30_000,
        "the member catches up in the leader's term",
        |cluster| {
            cluster.election_state(member) == Some(cavalry_raft::ElectionState::FollowerCatchup)
        },
    );
    cluster.cut_one_way(member, leader);
    safety.run_until(
        &mut cluster,
        30_000,
        "the member replays to the floor",
        |cluster| cluster.append_position(member) >= Some(floor),
    );

    // 3. The leader appends the entries after its duty cycle, and its module stalls.
    // Its recording syncs them and the replay brings them to the member, one report of
    // which is held on its way.
    let mut end = 0;
    for i in 10..40_u32 {
        end = cluster.offer(&i.to_le_bytes()).expect("the leader appends");
    }
    cluster.stall(leader);
    safety.run_until(
        &mut cluster,
        5_000,
        "the member replays the entries",
        |cluster| cluster.append_position(member) >= Some(end),
    );
    cluster.hold_back_append_position(member, leader);
    safety.run_until(&mut cluster, 5_000, "a report held", |cluster| {
        !cluster.held_back_reports().is_empty()
    });
    let reported = cluster.held_back_reports()[0].2;
    assert_eq!(reported, end, "a report of every entry");
    assert!(cluster.commit_position(leader) <= Some(floor));

    // 4 and 5. The member loses its log again and takes the leader on the held frame,
    // with a floor below the entries, and asks for a floor answer, which waits for the
    // leader's module.
    cluster.cut_between(member, leader);
    safety.wipe(&mut cluster, member);
    safety.run_until(&mut cluster, 5_000, "the member canvasses", |cluster| {
        cluster.election_state(member) == Some(cavalry_raft::ElectionState::Canvass)
    });
    safety.run(&mut cluster, 300);
    cluster.release_held_back(member);
    cluster.heal_between(member, leader);
    safety.run_until(
        &mut cluster,
        5_000,
        "the member takes the leader on the held frame",
        |cluster| {
            cluster
                .member(member)
                .is_some_and(|core| core.leadership_term_id() >= 0 && core.vote_floor() >= 0)
        },
    );
    safety.run(&mut cluster, 50);
    assert_eq!(
        cluster.member(member).expect("a member").vote_floor(),
        floor,
        "a floor below the entries"
    );

    // The leader's module runs again: it reads the ask before its duty cycle.
    cluster.unstall(leader);

    // 6. The member catches up to where the live sender stands and joins the live log
    // there, below the report.
    safety.run_until(
        &mut cluster,
        10_000,
        "the member joins the live log below the report",
        |cluster| cluster.election_state(member) == Some(cavalry_raft::ElectionState::Closed),
    );
    safety.run(&mut cluster, 200);
    assert!(
        cluster.append_position(member) < Some(reported),
        "its log lacks what its earlier run reported"
    );

    // 7. The report reaches the leader, which commits the entries with it.
    cluster.release_held_back(leader);
    safety.run_until(
        &mut cluster,
        2_000,
        "the leader commits with it",
        |cluster| cluster.commit_position(leader) >= Some(reported),
    );

    // 8. The leader stops: the member withholds, and the third alone elects nobody.
    cluster.halt(leader);
    safety.run(&mut cluster, 30_000);
    assert_eq!(cluster.leader(), None, "no leader without the entries");
    let core = cluster.member(member).expect("a member");
    assert_eq!(core.vote_floor(), floor, "it still holds its floor");
    assert!(
        core.floor_answer() >= reported,
        "its leader answered at what its recording had synced"
    );
}

/// Work item 463, ADR 0163: a member that holds a vote floor and reaches follower ready
/// with no catch-up is answered all the same, and clears its floor. A new cluster's
/// follower stores a floor of 0 when it takes its first leader. With every answer to it
/// lost, it joins and holds the leader's log with its floor still held. Started again
/// on the log it kept, it reads that floor at its start, takes the leader, holds the
/// leader's log already, and goes to follower ready with no catch-up. It asks in every
/// report, its leader answers in whatever state it reads one, and it clears.
#[test]
fn a_member_that_holds_a_floor_and_needs_no_catch_up_is_answered_and_clears_it() {
    let ids = [0, 1, 2];
    let mut safety = Safety::new(&ids);
    let mut cluster = Cluster::with_election(&ids, 11).expect("three members");
    for &id in &ids {
        cluster.lose_floor_answers_to(id, true);
    }
    safety.run_until(&mut cluster, 3_000, "the first election", |cluster| {
        settled(cluster, &ids)
    });
    let leader = cluster.leader().expect("a leader");
    let member = followers(&ids, leader)[0];
    offer(&mut cluster, &mut safety, 0, 4);
    safety.run(&mut cluster, 500);
    assert_eq!(
        cluster.append_position(member),
        cluster.append_position(leader),
        "it holds the leader's log"
    );
    let core = cluster.member(member).expect("a member");
    assert_eq!(
        core.vote_floor(),
        0,
        "a new cluster's follower's floor, unanswered"
    );
    assert_eq!(core.floor_answer(), -1);

    // Started again on the log it kept, its floor read at its start, with answers that
    // reach it.
    for &id in &ids {
        cluster.lose_floor_answers_to(id, false);
    }
    safety.restart(&mut cluster, member);
    safety.run(&mut cluster, 1);
    assert_eq!(cluster.member(member).expect("a member").vote_floor(), 0);
    let mut caught_up = false;
    let mut cleared = false;
    for _ in 0..5_000 {
        cluster.step();
        safety.check(&cluster);
        caught_up |= matches!(
            cluster.election_state(member),
            Some(
                cavalry_raft::ElectionState::FollowerCatchupInit
                    | cavalry_raft::ElectionState::FollowerCatchup
            )
        );
        if settled(&cluster, &ids) && cluster.member(member).expect("a member").vote_floor() == -1 {
            cleared = true;
            break;
        }
    }
    assert!(cleared, "the member clears its floor within 5 s");
    assert!(!caught_up, "it went to follower ready with no catch-up");
    let counts = cluster.counts(member).expect("a member");
    assert_eq!(counts.vote_floors_cleared, 1, "{counts:?}");
    assert!(
        cluster
            .counts(cluster.leader().expect("a leader"))
            .expect("a member")
            .floor_answers_sent
            >= 1
    );
}

/// Work item 463, ADR 0163: a floor answer lost on its way is answered again. A new
/// cluster's followers each hold a floor of 0 from their first leader. While every
/// answer to them is lost, they keep asking in every report and their leader keeps
/// answering, once each heartbeat interval, and the floors hold. Once the answers get
/// through, the next one each follower is sent clears its floor within a few intervals.
#[test]
fn a_floor_answer_lost_on_its_way_is_sent_again_in_the_next_interval() {
    let ids = [0, 1, 2];
    let mut safety = Safety::new(&ids);
    let mut cluster = Cluster::with_election(&ids, 5).expect("three members");
    for &id in &ids {
        cluster.lose_floor_answers_to(id, true);
    }
    let elected = safety.run_until(&mut cluster, 3_000, "the first election", |cluster| {
        settled(cluster, &ids)
    });
    let leader = cluster.leader().expect("a leader");
    let others = followers(&ids, leader);
    safety.run(&mut cluster, 2_000);
    for &id in &others {
        let core = cluster.member(id).expect("a member");
        assert_eq!(core.vote_floor(), 0, "member {id} holds its floor");
        assert_eq!(core.floor_answer(), -1, "member {id} took no answer");
    }
    let sent = cluster.counts(leader).expect("a member").floor_answers_sent;
    assert!(
        sent >= 4,
        "answered again each interval while they asked: {sent}"
    );
    // At most once each heartbeat interval, 200 ms, for each of the two followers.
    let intervals = (elected + 2_000).div_ceil(200);
    assert!(
        sent <= 2 * (intervals + 1),
        "no more than once an interval for each follower: {sent} in {intervals} intervals"
    );

    for &id in &ids {
        cluster.lose_floor_answers_to(id, false);
    }
    safety.run_until(
        &mut cluster,
        1_000,
        "the next answers clear the floors",
        |cluster| {
            others.iter().all(|&id| {
                cluster
                    .member(id)
                    .is_some_and(|core| core.vote_floor() == -1)
            })
        },
    );
}

/// The voters `member` waits for, holding nothing (ADR 0174).
fn unheard(cluster: &Cluster, member: i32) -> i64 {
    cluster.member(member).expect("a member").voters_unheard()
}

/// The schedule for which a member that holds nothing must stand above each term
/// named, so that one term has one leader (ADR 0174), from the manager's read of work
/// item 476. In a new cluster, member 0 stands in term 0 and member 1 votes for it,
/// while member 2 reads nothing of member 0, that stream held, and has heard member 1
/// name −1, member 1's stream to it held from then on. Member 1's wait for its
/// candidate ends first, and it canvasses again: its log empty, naming term 0, on a
/// stream to member 0 held from its vote on. Member 0 leads term 0 with member 1's
/// vote, and member 1 takes it as its leader; every report member 1 writes from then
/// on waits behind its held canvass positions, so member 0 appends nothing. Member 0
/// loses its log directory. Its new run reads member 1's held canvass positions alone,
/// as frames past a receiver window are sent in parts: an empty log, term 0. Member 2
/// reports an empty log and names −1, and stands still while the new run stands, so as
/// not to stand first. Every voter has reported, no log is heard, and the new run
/// stands in term 1, above the term member 1 named, and leads it with member 2's vote;
/// the three end behind it on one log. Standing in candidate term + 1, term 0, it led
/// term 0 a second time, which the check of the terms fails on. No entry is lost there:
/// a leader appends its term's first entry only once a quorum has reported at its
/// position, and a member whose report the first run read names a log in every frame
/// the new run reads of it.
#[test]
fn a_member_that_holds_nothing_stands_above_the_term_a_stale_report_names() {
    let ids = [0, 1, 2];
    let (m, w, v) = (0, 1, 2);
    let mut cluster = Cluster::with_election(&ids, 1).expect("three members");
    let mut safety = Safety::new(&ids);
    cluster.hold_stream(m, v);
    safety.run_until(&mut cluster, 1_000, "the first reports", |cluster| {
        unheard(cluster, m) == 0 && unheard(cluster, w) == 0 && unheard(cluster, v) == 1
    });
    // Member 1 stands still, so that member 0 stands first; member 2 has heard it
    // name -1, and hears nothing of it from now on.
    cluster.stall(w);
    cluster.hold_stream(w, v);
    assert_eq!(cluster.counts(w).expect("a member").candidacies, 0);
    safety.run_until(&mut cluster, 2_000, "member 0 stands", |cluster| {
        cluster.counts(m).expect("a member").candidacies == 1
    });
    assert_eq!(cluster.record_of_terms(m).map(|(_, term)| term), Some(0));

    // Member 1 votes for member 0 in term 0, and member 0 reads the vote.
    cluster.unstall(w);
    safety.run_until(&mut cluster, 1_000, "member 1's vote", |cluster| {
        cluster
            .votes_cast()
            .iter()
            .any(|vote| vote.voter == w && vote.term == 0 && vote.candidate == m)
    });
    safety.run(&mut cluster, 2);
    cluster.hold_stream(w, m);
    cluster.hold_stream(m, w);
    safety.run_until(&mut cluster, 5_000, "member 0 leads term 0", |cluster| {
        cluster.leader() == Some(m)
    });
    safety.run_until(&mut cluster, 5_000, "member 1 canvasses again", |cluster| {
        cluster.held_stream_count(w, m) > 0
    });
    let canvassed = cluster.held_stream_count(w, m);

    // Member 1 takes member 0 as its leader; its reports wait behind its canvass
    // positions, so member 0 appends nothing.
    cluster.release_stream(m, w);
    safety.run_until(
        &mut cluster,
        5_000,
        "member 1 follows member 0",
        |cluster| cluster.member(w).expect("a member").leader_member_id() == m,
    );
    safety.run(&mut cluster, 1_000);
    assert_eq!(
        cluster.append_position(m),
        Some(0),
        "member 0 appended nothing"
    );

    // Member 0 loses its log directory. Its new run reads member 1's canvass
    // positions alone; member 2 waits, so that it does not stand first.
    safety.wipe(&mut cluster, m);
    cluster.release_stream(m, v);
    safety.run_until(&mut cluster, 100, "the new run canvasses", |cluster| {
        cluster.election_state(m) == Some(ElectionState::Canvass)
    });
    cluster.release_stream_first(w, m, canvassed);
    safety.run_until(&mut cluster, 1_000, "the new run hears both", |cluster| {
        unheard(cluster, m) == 0
    });
    cluster.stall(v);
    assert_eq!(cluster.counts(v).expect("a member").candidacies, 0);
    safety.run_until(&mut cluster, 2_000, "the new run stands", |cluster| {
        cluster.counts(m).expect("a member").candidacies == 1
    });
    assert_eq!(
        cluster.record_of_terms(m).map(|(_, term)| term),
        Some(1),
        "above the term member 1 named"
    );
    cluster.unstall(v);
    safety.run_until(&mut cluster, 5_000, "the new run leads", |cluster| {
        cluster.leader() == Some(m)
    });
    assert_eq!(cluster.member(m).expect("a member").leadership_term_id(), 1);

    // Member 1's reports of term 0 reach the new run late, and count for nothing; the
    // three end on one log.
    cluster.release_stream(w, m);
    cluster.release_stream(w, v);
    safety.run_until(
        &mut cluster,
        10_000,
        "the new run's election closes",
        |cluster| cluster.election_state(m) == Some(ElectionState::Closed),
    );
    let entry = cluster.offer(b"the new run's").expect("the leader appends");
    safety.run_until(&mut cluster, 10_000, "the three settle", |cluster| {
        settled(cluster, &ids)
            && ids
                .iter()
                .all(|&id| cluster.commit_position(id) >= Some(entry))
    });
    for id in ids {
        assert_eq!(cluster.log(id), cluster.log(m), "member {id}");
    }
}
