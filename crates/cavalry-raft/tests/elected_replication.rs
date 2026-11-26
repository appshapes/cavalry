//! Replication and commit, three cores at a time in memory (`spec/cluster.md`, "The
//! commit position", "Members and streams"; ADR 0060, ADR 0069, ADR 0100): what the
//! log holds when a cluster starts together, what a commit needs, what an offer is
//! refused for, what a lossy and late consensus stream changes, the bound of the
//! members table and of the stream, a cluster of one, a cold start with a member
//! missing, a new cluster's first leader that stops before its word reaches anyone, a
//! leader that lost its log directory, members parted, and the bound of the
//! new leadership terms the in-memory cluster holds back. Every test checks, at every
//! step, that no committed entry is lost, and that the leader commits nothing a quorum
//! has not synced.

mod elected_cluster;

use cavalry_proto::cluster::{MAX_MEMBERS, Message, Stream, TimeUnit};
use cavalry_raft::memory::{Cluster, IN_FLIGHT_CAPACITY, MAX_DELAY_MS, MAX_HELD_BACK};
use cavalry_raft::{
    Consensus, ElectionCause, ElectionState, MembersRefused, NO_SESSION, Refused, Role,
};
use elected_cluster::{Safety, elected, holds, settled};

/// The two members of `ids` that `leader` leads.
fn followers(ids: &[i32], leader: i32) -> Vec<i32> {
    ids.iter().copied().filter(|&id| id != leader).collect()
}

/// The length of a new leadership term event, which has no variable field.
fn term_event_length() -> usize {
    Message::NewLeadershipTermEvent {
        leadership_term_id: 0,
        log_position: 0,
        timestamp: 0,
        term_base_log_position: 0,
        leader_member_id: 0,
        log_session_id: 0,
        time_unit: TimeUnit::Millis,
        app_version: 0,
    }
    .encoded_length()
}

/// A cluster that starts together elects a leader, and its log is what the page says
/// it is. Every member began one election and closed it, and recorded the term at its
/// base before it reported for it. The term's first entry is its new leadership term
/// event, at the term's base, with the leader's id. Each entry the leader's own
/// process appends is a session message of no session, cluster session id −1, with
/// the leader's term. A thousand of them are committed on all three, and the three
/// logs are one.
#[test]
fn a_cluster_started_together_elects_its_leader_replicates_and_commits() {
    let ids = [0, 1, 2];
    let mut safety = Safety::new(&ids);
    let (mut cluster, leader) = elected(1, &mut safety);
    safety.run(&mut cluster, 500);
    for id in ids {
        let member = cluster.member(id).expect("a member");
        assert_eq!(member.leadership_term_id(), 0, "member {id}");
        assert_eq!(member.leader_member_id(), leader, "member {id}");
        let counts = member.counts();
        assert_eq!(counts.elections, 1, "member {id}");
        assert_eq!(counts.elections_failed, 0, "member {id}");
        assert_eq!(counts.elections_closed, 1, "member {id}");
        assert_eq!(counts.terms_led, u64::from(id == leader), "member {id}");
        // The term each member stored is the one it voted in or proposed.
        assert_eq!(
            cluster.record_of_terms(id),
            Some((vec![(0, 0)], 0)),
            "member {id}"
        );
    }

    // The term's first entry is its new leadership term event, at the base.
    let log = cluster.log(leader);
    let event = Message::parse(Stream::Log, &log[..term_event_length()]).expect("an entry");
    assert!(matches!(
        event,
        Message::NewLeadershipTermEvent {
            leadership_term_id: 0,
            log_position: 0,
            term_base_log_position: 0,
            leader_member_id,
            ..
        } if leader_member_id == leader
    ));
    let after_event = cluster.commit_position(leader).expect("the leader");
    assert!(after_event > 0, "the term event is committed");

    // A thousand entries from the leader's own process, each a session message of no
    // session with the leader's term.
    let mut last = after_event;
    for i in 0..1_000u32 {
        last = cluster.offer(&i.to_le_bytes()).expect("the leader appends");
        safety.run(&mut cluster, 1);
    }
    safety.run(&mut cluster, 1_000);
    for id in ids {
        assert_eq!(cluster.commit_position(id), Some(last), "member {id}");
        assert_eq!(cluster.append_position(id), Some(last), "member {id}");
        assert_eq!(cluster.log(id), cluster.log(leader), "member {id}");
    }
    let log = cluster.log(leader);
    let first = &log[term_event_length()..];
    let entry = Message::parse(Stream::Log, &first[..32 + 4]).expect("an entry");
    assert!(matches!(
        entry,
        Message::SessionMessage {
            leadership_term_id: 0,
            cluster_session_id: NO_SESSION,
            payload,
            ..
        } if payload == 0u32.to_le_bytes()
    ));
    assert_eq!(cluster.leader(), Some(leader), "it led throughout");
}

/// A cold start with a member missing elects no leader until that member starts (work
/// item 476). A member that holds nothing, its log empty and no candidate term stored,
/// as each does before its first vote, cannot tell a new cluster from one whose logs
/// were lost, and one that lost its log in the cluster's first ballot may have voted
/// there. So in a cluster of empty logs it neither stands nor votes until every other
/// voter has reported. Two of three run for 120 s, twice the startup canvass timeout:
/// both still canvass, neither has stood, voted or stored a term, and an offer is
/// refused. The third starts, the three hear each other and elect a leader, and what
/// it commits is on all three.
#[test]
fn two_of_three_elect_no_leader_until_the_third_starts_and_then_the_three_elect() {
    let ids = [0, 1, 2];
    let two = [0, 1];
    let mut cluster = Cluster::with_election(&ids, 1).expect("three members");
    let mut safety = Safety::new(&ids);
    cluster.halt(2);
    safety.run(&mut cluster, 120_000);
    assert_eq!(cluster.leader(), None);
    for id in two {
        assert_eq!(cluster.role(id), Some(Role::Follower), "member {id}");
        assert_eq!(
            cluster.election_state(id),
            Some(ElectionState::Canvass),
            "member {id}"
        );
        let counts = cluster.counts(id).expect("a member");
        assert_eq!(counts.candidacies, 0, "member {id}: {counts:?}");
        assert_eq!(counts.votes_for, 0, "member {id}: {counts:?}");
        assert_eq!(counts.votes_against, 0, "member {id}: {counts:?}");
        assert_eq!(
            cluster.record_of_terms(id),
            Some((vec![], -1)),
            "member {id} proposed nothing"
        );
        assert!(cluster.log(id).is_empty(), "member {id}");
    }
    assert_eq!(cluster.offer(b"early"), Err(Refused::NotLeader));

    // The third member starts: every voter has reported, and the three elect.
    cluster.resume(2);
    safety.run_until(&mut cluster, 5_000, "the three elect a leader", |cluster| {
        settled(cluster, &ids)
    });
    let leader = cluster.leader().expect("a leader");
    let first = cluster
        .offer(b"three of three")
        .expect("the leader appends");
    safety.run(&mut cluster, 500);
    for id in ids {
        assert_eq!(cluster.commit_position(id), Some(first), "member {id}");
        assert_eq!(cluster.log(id), cluster.log(leader), "member {id}");
        let counts = cluster.counts(id).expect("a member");
        assert_eq!(counts.elections_failed, 0, "member {id}: {counts:?}");
        assert_eq!(counts.elections_closed, 1, "member {id}: {counts:?}");
    }
    assert_eq!(cluster.leader(), Some(leader), "it led throughout");
    assert_eq!(cluster.counts(leader).expect("the leader").terms_led, 1);
}

/// Holds back every new leadership term each member of `ids` sends another, runs
/// `cluster` until a member leads, and cuts that leader off from the other two, so no
/// word of its term reaches them. Then runs until each of the two has stored the
/// leader's term, or a later one, as its candidate term, having voted in the election,
/// and stops the leader. Returns the leader and its term.
fn first_leader_stops_unheard(
    cluster: &mut Cluster,
    safety: &mut Safety,
    ids: &[i32],
    voters: impl Fn(i32) -> Vec<i32>,
) -> (i32, i64) {
    for &from in ids {
        for to in followers(ids, from) {
            cluster.hold_back_new_leadership_term(from, to);
        }
    }
    safety.run_until(cluster, 3_000, "a first leader", |cluster| {
        cluster.leader().is_some()
    });
    let first = cluster.leader().expect("a leader");
    let term = cluster
        .member(first)
        .expect("a member")
        .leadership_term_id();
    for id in followers(ids, first) {
        cluster.cut_one_way(first, id);
    }
    let voted = voters(first);
    safety.run_until(cluster, 1_000, "the voters' candidate terms", |cluster| {
        voted.iter().all(|&id| {
            cluster
                .record_of_terms(id)
                .is_some_and(|(_, candidate)| candidate >= term)
        })
    });
    cluster.halt(first);
    for &id in &voted {
        let member = cluster.member(id).expect("a member");
        assert_eq!(member.vote_floor(), -1, "member {id} took no leader");
        assert!(cluster.log(id).is_empty(), "member {id}");
    }
    (first, term)
}

/// A new cluster's first leader that stops before its new leadership term reaches
/// either member that voted for it is replaced as before work item 476 (ADR 0174). Each
/// of the two stored the term as its candidate term when it voted, so it remembers its
/// vote and holds something, and neither took the leader, so neither holds a vote
/// floor. Their election has not closed, and each heard every voter in it, the stopped
/// leader's canvass position among them, so each reads `voters_unheard` 0 and neither
/// waits: one of them leads a later term within 5 s of the stop, about 2 s at seeds 1
/// to 5, and what it commits is on both.
#[test]
fn a_new_clusters_first_leader_that_stops_unheard_is_replaced_by_the_two_that_voted() {
    let ids = [0, 1, 2];
    for seed in 1..=5 {
        let mut cluster = Cluster::with_election(&ids, seed).expect("three members");
        let mut safety = Safety::new(&ids);
        let (first, term) = first_leader_stops_unheard(&mut cluster, &mut safety, &ids, |first| {
            followers(&ids, first)
        });
        let two = followers(&ids, first);
        for &id in &two {
            let member = cluster.member(id).expect("a member");
            assert_eq!(member.voters_unheard(), 0, "seed {seed}: member {id}");
        }
        safety.run_until(&mut cluster, 5_000, "a leader of a later term", |cluster| {
            cluster.leader().is_some_and(|leader| {
                leader != first
                    && cluster
                        .member(leader)
                        .expect("a member")
                        .leadership_term_id()
                        > term
            })
        });
        safety.run_until(&mut cluster, 5_000, "the two settle", |cluster| {
            settled(cluster, &two)
        });
        let leader = cluster.leader().expect("a leader");
        let entry = cluster.offer(b"two of three").expect("the leader appends");
        safety.run(&mut cluster, 500);
        for &id in &two {
            assert_eq!(
                cluster.commit_position(id),
                Some(entry),
                "seed {seed}: member {id}"
            );
            assert_eq!(
                cluster.log(id),
                cluster.log(leader),
                "seed {seed}: member {id}"
            );
        }
    }
}

/// What work item 476's rule costs a new cluster whose first leader stops early (ADR
/// 0174): a member that holds nothing and has not heard that leader in its present
/// election waits for it. Member 2 hears neither other member while members 0 and 1
/// elect, so it neither votes nor stores a term. The leader stops before its new
/// leadership term reaches the member that voted for it, which holds a candidate term
/// and no vote floor, and member 2 then hears that member. It has not heard the stopped
/// leader in this election, and it cannot tell that it never voted for that leader,
/// since a lost log directory would leave it as it is, so it neither stands nor votes.
/// For 120 s, twice the startup canvass timeout, no member leads, and `voters_unheard`
/// reads 1 on member 2, where before the rule the two elected within 2 s. The leader
/// runs again: member 2 hears every voter, and the three elect and commit.
#[test]
fn a_member_that_never_voted_waits_for_a_first_leader_that_stopped_unheard() {
    let ids = [0, 1, 2];
    let mut cluster = Cluster::with_election(&ids, 1).expect("three members");
    let mut safety = Safety::new(&ids);
    cluster.cut_one_way(0, 2);
    cluster.cut_one_way(1, 2);
    let (first, _) =
        first_leader_stops_unheard(&mut cluster, &mut safety, &ids, |first| vec![1 - first]);
    assert!(first == 0 || first == 1, "member 2 stood: {first}");
    let voter = 1 - first;
    cluster.heal_between(voter, 2);
    assert_eq!(
        cluster.record_of_terms(2),
        Some((vec![], -1)),
        "member 2 never voted"
    );

    safety.run(&mut cluster, 120_000);
    assert_eq!(cluster.leader(), None);
    let waiting = cluster.member(2).expect("a member");
    assert_eq!(
        waiting.voters_unheard(),
        1,
        "member 2 waits for the first leader"
    );
    let counts = cluster.counts(2).expect("a member");
    assert_eq!(counts.votes_for, 0, "{counts:?}");
    assert!(
        counts.votes_withheld > 0,
        "the voter stood and asked: {counts:?}"
    );
    assert_eq!(
        cluster.record_of_terms(2),
        Some((vec![], -1)),
        "member 2 stood for nothing"
    );

    // The first leader runs again, and member 2 hears every voter.
    cluster.heal_between(first, voter);
    cluster.heal_between(first, 2);
    cluster.resume(first);
    safety.run_until(&mut cluster, 70_000, "the three elect", |cluster| {
        settled(cluster, &ids)
    });
    let entry = cluster
        .offer(b"three of three")
        .expect("the leader appends");
    safety.run(&mut cluster, 500);
    for id in ids {
        assert_eq!(cluster.commit_position(id), Some(entry), "member {id}");
    }
}

/// The leader loses its log directory and starts again on an empty one, which a
/// container with no volume does by itself. Its followers hold a term and a log it no
/// longer has. They lose its image of the log and canvass. Every report the member
/// with no log reads is of a log ahead of its own, so it is no candidate and proposes
/// no term. One of the two that kept their logs leads the next term, from what was
/// committed. The member with no log replicates the term it lost from that leader's
/// recordings, with one replay, and records the new term at its base. Until it holds
/// what was committed it has been no candidate and has led nothing. Then it joins,
/// and the three end on one log that holds every entry that was committed.
#[test]
fn a_leader_that_lost_its_log_directory_leads_no_term_and_replicates_what_it_lost() {
    let ids = [0, 1, 2];
    let mut safety = Safety::new(&ids);
    let (mut cluster, first) = elected(1, &mut safety);
    let mut committed = 0;
    for i in 0..100u32 {
        committed = cluster.offer(&i.to_le_bytes()).expect("the leader appends");
        safety.run(&mut cluster, 1);
    }
    safety.run(&mut cluster, 1_000);
    let others = followers(&ids, first);
    for &id in &others {
        assert_eq!(cluster.commit_position(id), Some(committed), "member {id}");
    }
    let before = cluster.log(first);

    safety.wipe(&mut cluster, first);
    assert!(cluster.log(first).is_empty());
    safety.run_until(
        &mut cluster,
        30_000,
        "the member with no log holds what was committed",
        |cluster| cluster.append_position(first) >= Some(committed),
    );
    let second = cluster.leader().expect("a leader");
    assert!(others.contains(&second), "member {second} leads");
    assert_eq!(
        cluster
            .member(second)
            .expect("a member")
            .leadership_term_id(),
        1
    );
    let counts = cluster.counts(first).expect("a member");
    assert_eq!(counts.candidacies, 0, "{counts:?}");
    assert_eq!(counts.terms_led, 0, "{counts:?}");
    assert_eq!(counts.log_replications, 1, "{counts:?}");
    assert_eq!(
        cluster.record_of_terms(first).map(|(terms, _)| terms),
        Some(vec![(0, 0), (1, committed)]),
        "the term it lost, and the next at its base"
    );
    assert_eq!(cluster.log(first), before, "the log it lost, byte for byte");
    for &id in &others {
        assert_eq!(
            cluster.counts(id).expect("a member").log_images_lost,
            1,
            "member {id}"
        );
    }

    safety.run_until(&mut cluster, 30_000, "one leader and one log", |cluster| {
        settled(cluster, &ids)
    });
    let leads = cluster.leader().expect("a leader");
    let last = cluster
        .offer(b"after the loss")
        .expect("the leader appends");
    safety.run(&mut cluster, 1_000);
    for id in ids {
        assert_eq!(cluster.commit_position(id), Some(last), "member {id}");
        assert_eq!(cluster.log(id), cluster.log(leads), "member {id}");
        assert!(
            cluster.log(id).starts_with(&before),
            "member {id} holds what was committed"
        );
    }
}

/// A commit waits for a quorum of synced positions, the leader's own among them. With
/// neither follower syncing, an entry that reached all three is durable on one and is
/// not committed. With one follower syncing it is, and the other is told a commit
/// position it has not synced up to. With the leader's own recorder lagging, both
/// followers hold the next entry and it is not committed until the leader does too.
#[test]
fn a_commit_waits_for_a_quorum_of_synced_positions_and_for_the_leaders_own() {
    let ids = [0, 1, 2];
    let mut safety = Safety::new(&ids);
    let (mut cluster, leader) = elected(1, &mut safety);
    let [first, second] = followers(&ids, leader)[..] else {
        panic!("two followers");
    };
    safety.run(&mut cluster, 500);
    let before = cluster.commit_position(leader).expect("the leader");

    cluster.pause_sync(first);
    cluster.pause_sync(second);
    let entry = cluster.offer(b"one of three").expect("the leader appends");
    safety.run(&mut cluster, 1_000);
    assert_eq!(cluster.append_position(leader), Some(entry));
    assert_eq!(cluster.log(first), cluster.log(leader), "the entry arrived");
    assert_eq!(
        cluster.commit_position(leader),
        Some(before),
        "one member is no quorum"
    );
    assert_eq!(cluster.leader(), Some(leader), "it still leads");

    cluster.resume_sync(first);
    safety.run(&mut cluster, 500);
    for id in ids {
        assert_eq!(cluster.commit_position(id), Some(entry), "member {id}");
    }
    assert_eq!(cluster.append_position(second), Some(before));
    cluster.resume_sync(second);
    safety.run(&mut cluster, 500);

    cluster.pause_sync(leader);
    let next = cluster
        .offer(b"not the leader's yet")
        .expect("the leader appends");
    safety.run(&mut cluster, 1_000);
    assert_eq!(cluster.append_position(first), Some(next));
    assert_eq!(cluster.append_position(second), Some(next));
    assert_eq!(cluster.commit_position(leader), Some(entry));
    cluster.resume_sync(leader);
    safety.run(&mut cluster, 500);
    for id in ids {
        assert_eq!(cluster.commit_position(id), Some(next), "member {id}");
    }
    for id in ids {
        assert_eq!(
            cluster.counts(id).expect("a member").elections,
            1,
            "member {id} held no election over a sync that lagged"
        );
    }
}

/// What an offer is refused for, each with what to do next: while no member leads,
/// as not the leader's; from a leader whose election has not closed, for its
/// election; and under back pressure, where nothing is written.
#[test]
fn an_offer_is_refused_with_no_leader_before_its_election_closes_and_under_back_pressure() {
    let ids = [0, 1, 2];
    let mut cluster = Cluster::with_election(&ids, 1).expect("three members");
    let refused = cluster.offer(b"too early").expect_err("no member leads");
    assert_eq!(refused, Refused::NotLeader);
    assert!(
        refused.to_string().contains("offer to the leader")
            && refused
                .to_string()
                .contains("docs/reference/cluster.md#roles-and-the-election"),
        "{refused}"
    );
    let mut safety = Safety::new(&ids);
    // A member leads from the ballot it wins, some steps before its election closes.
    let mut refused_for_its_election = 0;
    for _ in 0..3_000 {
        if settled(&cluster, &ids) {
            break;
        }
        if let Some(leader) = cluster.leader()
            && cluster.election_state(leader) != Some(ElectionState::Closed)
        {
            let refused = cluster
                .offer(b"too early")
                .expect_err("its election has not closed");
            assert_eq!(refused, Refused::Election);
            let text = refused.to_string();
            assert!(
                text.contains(
                    "offer again once a quorum of members, the leader among them, has \
                     joined its log"
                ) && text.contains("docs/reference/cluster.md#roles-and-the-election"),
                "{refused}"
            );
            refused_for_its_election += 1;
        }
        cluster.step();
        safety.check(&cluster);
    }
    assert!(settled(&cluster, &ids), "the first election closed");
    assert!(
        refused_for_its_election > 0,
        "no step had a leader whose election was open"
    );
    let leader = cluster.leader().expect("a leader");

    cluster.back_pressure(true);
    let refused = cluster.offer(b"not now").expect_err("back-pressured");
    assert_eq!(refused, Refused::BackPressured);
    let text = refused.to_string();
    assert!(
        text.contains("offer again")
            && text.contains("docs/reference/cluster.md#roles-and-the-election"),
        "{refused}"
    );
    let before = cluster.log(leader);
    safety.run(&mut cluster, 100);
    assert_eq!(cluster.log(leader), before, "a refused offer wrote nothing");
    cluster.back_pressure(false);
    let entry = cluster.offer(b"now").expect("the leader appends");
    safety.run(&mut cluster, 500);
    for id in ids {
        assert_eq!(cluster.commit_position(id), Some(entry), "member {id}");
    }
}

/// The consensus stream loses one message in three and delivers the rest up to 50 ms
/// late and out of order, from the first step to the last: the cluster still elects a
/// leader, and replicates and commits 500 entries under that loss; the three logs are
/// one, and the safety check holds at every step. An election may take more than one
/// ballot on such a stream. Once every election has closed none begins again: from
/// then to the end no member counts an election, a failed election or a leader
/// heartbeat timeout, no offer is refused, and the same member leads.
#[test]
fn lost_and_late_consensus_messages_change_no_outcome() {
    let ids = [0, 1, 2];
    let mut cluster = Cluster::with_election(&ids, 1).expect("three members");
    let mut safety = Safety::new(&ids);
    cluster.set_delivery(3, 50);
    safety.run_until(
        &mut cluster,
        30_000,
        "a leader on a lossy stream",
        |cluster| settled(cluster, &ids),
    );
    let leader = cluster.leader().expect("a leader");
    let settled_counts = ids.map(|id| cluster.counts(id).expect("a member"));
    let mut last = 0;
    for entry in 0..500u32 {
        last = cluster
            .offer(&entry.to_le_bytes())
            .expect("the leader appends");
        safety.run(&mut cluster, 3);
    }
    // The stream stays lossy and late while the last entries commit.
    safety.run_until(
        &mut cluster,
        30_000,
        "every member committed under the loss",
        |cluster| {
            ids.iter()
                .all(|&id| cluster.commit_position(id) >= Some(last))
        },
    );
    assert_eq!(cluster.leader(), Some(leader), "the same member leads");
    for (id, before) in ids.into_iter().zip(settled_counts) {
        assert_eq!(cluster.log(id), cluster.log(leader), "member {id}");
        assert_eq!(
            cluster.election_state(id),
            Some(ElectionState::Closed),
            "member {id}"
        );
        let counts = cluster.counts(id).expect("a member");
        assert_eq!(counts.elections, before.elections, "member {id}");
        assert_eq!(
            counts.elections_failed, before.elections_failed,
            "member {id}"
        );
        assert_eq!(
            counts.leader_heartbeat_timeouts, before.leader_heartbeat_timeouts,
            "member {id}"
        );
    }
}

/// The full case of the members table: eight members, the bound, elect a leader and
/// replicate as three do, and a ninth is refused before anything runs, as every
/// members string the page refuses is. Each refusal names the check of
/// `spec/cluster.md`, "What a reader rejects", that failed, in the page's order:
/// grammar, duplicate, count, self.
#[test]
fn eight_members_elect_and_replicate_and_a_ninth_is_refused() {
    let ids: Vec<i32> = (10..18).collect();
    assert_eq!(ids.len(), MAX_MEMBERS);
    let mut cluster = Cluster::with_election(&ids, 1).expect("eight members");
    let mut safety = Safety::new(&ids);
    safety.run_until(&mut cluster, 5_000, "a leader of eight", |cluster| {
        settled(cluster, &ids)
    });
    let leader = cluster.leader().expect("a leader");
    let position = cluster.offer(b"eight").expect("the leader appends");
    safety.run(&mut cluster, 1_000);
    for &id in &ids {
        assert_eq!(
            cluster.role(id),
            Some(if id == leader {
                Role::Leader
            } else {
                Role::Follower
            })
        );
        assert_eq!(cluster.commit_position(id), Some(position), "member {id}");
    }

    let nine: Vec<i32> = (0..9).collect();
    let mut nine_with_a_duplicate = nine.clone();
    nine_with_a_duplicate.push(3);
    for (members, own, refused, reason, says) in [
        (
            &[][..],
            0,
            MembersRefused::Empty,
            "members-grammar",
            "no member was given",
        ),
        (
            &[0, -4, 1][..],
            0,
            MembersRefused::Negative { id: -4 },
            "members-grammar",
            "member id -4 is below 0",
        ),
        // Each check runs over every member before the next: an id below 0 after a
        // duplicate is the grammar's.
        (
            &[0, 1, 1, -4][..],
            0,
            MembersRefused::Negative { id: -4 },
            "members-grammar",
            "member id -4 is below 0",
        ),
        (
            &[0, 1, 1][..],
            0,
            MembersRefused::Duplicate { id: 1 },
            "members-duplicate",
            "two members have the id 1",
        ),
        (
            &nine_with_a_duplicate[..],
            0,
            MembersRefused::Duplicate { id: 3 },
            "members-duplicate",
            "two members have the id 3",
        ),
        (
            &nine[..],
            0,
            MembersRefused::Count { found: 9 },
            "members-count",
            "9 members",
        ),
        (
            &[0, 1, 2][..],
            3,
            MembersRefused::NotAMember { id: 3 },
            "members-self",
            "member id 3 is not in the members string",
        ),
    ] {
        let found = Consensus::with_election(members, own, 1).expect_err("refused");
        assert_eq!(found, refused, "{members:?}");
        assert_eq!(found.reason(), reason, "{members:?}");
        let message = found.to_string();
        assert!(
            message.starts_with(&format!("members refused ({reason}): {says}")),
            "{message}"
        );
        assert!(
            message.ends_with("; see docs/reference/cluster.md#what-a-reader-rejects"),
            "{message}"
        );
    }
}

/// The consensus stream in memory is bounded (invariant 3). Eight members whose
/// messages are up to two seconds late, with an entry offered every millisecond, send
/// more than the queue holds: it holds `IN_FLIGHT_CAPACITY` and never more, and what
/// is sent while it is full is lost and counted. Every consensus message is sent
/// again at its interval, so once delivery is prompt again the cluster has one leader
/// and every member learns its commit position; and once the late messages are
/// delivered, two seconds on, the queue has room and nothing more is lost. A delay
/// past `MAX_DELAY_MS` is held to it.
#[test]
fn the_consensus_stream_in_memory_holds_its_bound_and_loses_what_it_cannot_hold() {
    let ids: Vec<i32> = (0..8).collect();
    let mut cluster = Cluster::with_election(&ids, 1).expect("eight members");
    let mut safety = Safety::new(&ids);
    safety.run_until(&mut cluster, 5_000, "a leader of eight", |cluster| {
        settled(cluster, &ids)
    });
    assert_eq!(cluster.full_queue_losses(), 0);
    assert_eq!(cluster.set_delivery(0, 2_000), 2_000);
    let mut fullest = 0;
    for i in 0..3_000u32 {
        // A leader that hears its followers two seconds late may be refused an offer
        // or replaced; the stream's bound holds either way.
        let _ = cluster.offer(&i.to_le_bytes());
        safety.run(&mut cluster, 1);
        assert!(cluster.in_flight() <= IN_FLIGHT_CAPACITY, "{cluster:?}");
        fullest = fullest.max(cluster.in_flight());
    }
    assert_eq!(fullest, IN_FLIGHT_CAPACITY);
    let lost = cluster.full_queue_losses();
    assert!(lost > 0);

    assert_eq!(cluster.set_delivery(0, 0), 0);
    safety.run_until(&mut cluster, 30_000, "one leader again", |cluster| {
        settled(cluster, &ids)
    });
    // The queue still holds what was sent up to two seconds late, and what is sent
    // while it is full is still lost. Two seconds on every late message is delivered.
    safety.run(&mut cluster, 2_000);
    assert!(cluster.in_flight() < IN_FLIGHT_CAPACITY);
    let lost = cluster.full_queue_losses();
    let leader = cluster.leader().expect("a leader");
    let last = cluster.offer(b"the last").expect("the leader appends");
    safety.run(&mut cluster, 3_000);
    assert!(cluster.in_flight() < IN_FLIGHT_CAPACITY);
    assert_eq!(
        cluster.full_queue_losses(),
        lost,
        "a queue with room loses nothing"
    );
    for &id in &ids {
        assert_eq!(cluster.commit_position(id), Some(last), "member {id}");
        assert_eq!(cluster.log(id), cluster.log(leader), "member {id}");
    }

    assert_eq!(cluster.set_delivery(0, u64::MAX), MAX_DELAY_MS);
    safety.run(&mut cluster, 100);
    assert!(cluster.in_flight() <= IN_FLIGHT_CAPACITY);
}

/// The new leadership terms the in-memory cluster holds back are bounded (invariant 3;
/// ADR 0161): at most `MAX_HELD_BACK` pairs armed and at most `MAX_HELD_BACK` frames
/// held, each bound with its behaviour when full.
///
/// The cluster has eight members, as the stream's bound test has, since fewer send too
/// little to fill the in-flight queue; the first two parts use three of the followers.
///
/// - **Arming.** Of `MAX_HELD_BACK + 1` pairs asked, the six between the three
///   followers and the leader's to two of them are armed, and the leader's to the third,
///   past the bound, is not. Each of the three starts again and canvasses: the leader's
///   answers to the first two are held, and its answer to the third is sent as any
///   message is, and the third follows on it.
/// - **Sending.** Frames are held up to the bound, and then a pair still armed has its
///   frame sent as any message is, the frames held staying as they were.
/// - **Releasing.** A release while the in-flight queue is full loses each frame it hands
///   over and counts it in `full_queue_losses`, as `send` does, and keeps the frames
///   held for other members.
#[test]
fn the_new_leadership_terms_held_back_in_memory_hold_their_bound() {
    let ids: Vec<i32> = (0..8).collect();
    let mut cluster = Cluster::with_election(&ids, 1).expect("eight members");
    let mut safety = Safety::new(&ids);
    safety.run_until(&mut cluster, 5_000, "a leader of eight", |cluster| {
        settled(cluster, &ids)
    });
    let leader = cluster.leader().expect("a leader");
    let others = followers(&ids, leader)[..3].to_vec();
    let term = cluster
        .member(leader)
        .expect("a member")
        .leadership_term_id();

    // Arming: the six pairs between followers, which see no new leadership term while
    // the leader leads, and then the leader's three; the last is past the bound.
    let mut asked = Vec::new();
    for &from in &others {
        for &to in &others {
            if from != to {
                asked.push((from, to));
            }
        }
    }
    asked.extend(others.iter().map(|&to| (leader, to)));
    assert_eq!(asked.len(), MAX_HELD_BACK + 1);
    for &(from, to) in &asked {
        cluster.hold_back_new_leadership_term(from, to);
    }
    for &member in &others {
        safety.restart(&mut cluster, member);
        safety.run_until(&mut cluster, 5_000, "the member follows again", |cluster| {
            settled(cluster, &ids)
        });
    }
    let held = cluster.held_back();
    assert_eq!(
        held.iter().map(|&(_, to, _, _)| to).collect::<Vec<_>>(),
        others[..2],
        "the third follower's answer was past the bound and not held: {held:?}"
    );
    assert!(
        held.iter()
            .all(|&(from, _, _, held_term)| from == leader && held_term == term),
        "{held:?}"
    );

    // Sending: one pair armed at a time, beside the six that stay armed, until the
    // frames held reach the bound.
    for (round, &member) in others.iter().cycle().take(MAX_HELD_BACK - 2).enumerate() {
        cluster.hold_back_new_leadership_term(leader, member);
        safety.restart(&mut cluster, member);
        safety.run_until(&mut cluster, 5_000, "the member follows again", |cluster| {
            settled(cluster, &ids)
        });
        assert_eq!(
            cluster.held_back().len(),
            3 + round,
            "{:?}",
            cluster.held_back()
        );
    }
    let full = cluster.held_back();
    assert_eq!(full.len(), MAX_HELD_BACK);
    // A pair armed now, with the bound held, has its frame sent: nothing more is held,
    // and the member follows on it.
    cluster.hold_back_new_leadership_term(leader, others[2]);
    safety.restart(&mut cluster, others[2]);
    safety.run_until(&mut cluster, 5_000, "the member follows again", |cluster| {
        settled(cluster, &ids)
    });
    assert_eq!(cluster.held_back(), full, "nothing past the bound is held");

    // Releasing into a full queue, which messages two seconds late fill, as in the
    // stream's bound test.
    assert_eq!(cluster.set_delivery(0, 2_000), 2_000);
    let mut filled = false;
    for i in 0..3_000u32 {
        let _ = cluster.offer(&i.to_le_bytes());
        safety.run(&mut cluster, 1);
        assert!(cluster.held_back().len() <= MAX_HELD_BACK);
        if cluster.in_flight() == IN_FLIGHT_CAPACITY {
            filled = true;
            break;
        }
    }
    assert!(filled, "the in-flight queue fills: {cluster:?}");
    let to_first = u64::try_from(
        full.iter()
            .filter(|&&(_, to, _, _)| to == others[0])
            .count(),
    )
    .expect("at most MAX_HELD_BACK");
    assert!(to_first > 0, "{full:?}");
    let lost = cluster.full_queue_losses();
    cluster.release_held_back(others[0]);
    assert_eq!(
        cluster.full_queue_losses(),
        lost + to_first,
        "each frame released into the full queue is lost and counted"
    );
    assert_eq!(cluster.in_flight(), IN_FLIGHT_CAPACITY);
    let kept = cluster.held_back();
    assert_eq!(
        kept,
        full.iter()
            .copied()
            .filter(|&(_, to, _, _)| to != others[0])
            .collect::<Vec<_>>(),
        "the frames held for the other members stay held"
    );

    assert_eq!(cluster.set_delivery(0, 0), 0);
    safety.run_until(&mut cluster, 30_000, "one leader again", |cluster| {
        settled(cluster, &ids)
    });
    assert!(cluster.held_back().len() <= MAX_HELD_BACK);
}

/// A cluster of one member is its own quorum: it leads from init, with nobody to
/// canvass and no ballot, closes its election, and commits what its own recorder has
/// synced.
#[test]
fn a_cluster_of_one_leads_from_init_and_commits_alone() {
    let ids = [5];
    let mut cluster = Cluster::with_election(&ids, 1).expect("one member");
    let mut safety = Safety::new(&ids);
    safety.run(&mut cluster, 1);
    assert_eq!(cluster.role(5), Some(Role::Leader));
    assert_ne!(cluster.election_state(5), Some(ElectionState::Canvass));
    safety.run(&mut cluster, 20);
    assert_eq!(cluster.election_state(5), Some(ElectionState::Closed));
    let counts = cluster.counts(5).expect("the member");
    assert_eq!((counts.elections, counts.terms_led), (1, 1));
    assert_eq!(counts.candidacies, 0, "a cluster of one holds no ballot");
    let entry = cluster.offer(b"alone").expect("the leader appends");
    safety.run(&mut cluster, 20);
    assert_eq!(cluster.commit_position(5), Some(entry));
}

/// A partition between two followers parts nobody from the leader: the log reaches
/// both, both report, an entry is committed on all three, and no member holds an
/// election in more than the leader heartbeat timeout.
#[test]
fn a_partition_between_two_followers_leaves_the_log_whole() {
    let ids = [0, 1, 2];
    let mut safety = Safety::new(&ids);
    let (mut cluster, leader) = elected(1, &mut safety);
    let [first, second] = followers(&ids, leader)[..] else {
        panic!("two followers");
    };
    cluster.cut_between(first, second);
    // A second cut of the same two, either way round, and a cut of a member from
    // itself or from an id no member has, are nothing more.
    cluster.cut_between(second, first);
    cluster.cut_between(first, first);
    cluster.cut_between(first, 9);
    let entry = cluster
        .offer(b"the followers are parted")
        .expect("the leader appends");
    safety.run(&mut cluster, 11_000);
    for id in ids {
        assert_eq!(cluster.commit_position(id), Some(entry), "member {id}");
        assert_eq!(cluster.log(id), cluster.log(leader), "member {id}");
        assert_eq!(
            cluster.election_state(id),
            Some(ElectionState::Closed),
            "member {id}"
        );
        assert_eq!(
            cluster.counts(id).expect("a member").elections,
            1,
            "member {id}"
        );
    }
    assert_eq!(cluster.leader(), Some(leader));
}

/// A follower whose image of the log is gone while its leader still speaks cannot
/// append from an image it no longer has: it counts the loss, begins again and names
/// the loss as the cause. Level with its leader, the new image joins where its log
/// ends, and following the same term again records nothing. Behind it, the leader
/// having appended meanwhile, it catches up and joins all the same.
#[test]
fn a_follower_that_loses_its_image_begins_again_and_joins_level_or_behind() {
    let ids = [0, 1, 2];
    let mut safety = Safety::new(&ids);
    let (mut cluster, leader) = elected(1, &mut safety);
    let [first, second] = followers(&ids, leader)[..] else {
        panic!("two followers");
    };
    safety.run(&mut cluster, 500);

    // Level.
    cluster.lose_image(first);
    safety.run_until(
        &mut cluster,
        3_000,
        "the follower joined again",
        |cluster| {
            settled(cluster, &ids)
                && cluster
                    .counts(first)
                    .is_some_and(|counts| counts.log_images_lost == 1)
        },
    );
    let counts = cluster.counts(first).expect("a member");
    assert_eq!((counts.elections, counts.elections_failed), (2, 0));
    assert_eq!(
        cluster.record_of_terms(first).map(|(terms, _)| terms),
        Some(vec![(0, 0)]),
        "the term was recorded once"
    );
    assert_eq!(
        cluster.member(first).expect("a member").election_cause(),
        ElectionCause::LogImageLost
    );
    assert_eq!(
        cluster.leader(),
        Some(leader),
        "the leader was not disturbed"
    );
    let entry = cluster
        .offer(b"after the first loss")
        .expect("the leader appends");
    safety.run(&mut cluster, 500);
    assert_eq!(cluster.append_position(first), Some(entry));

    // Behind: the leader appends while the other follower has no image.
    cluster.lose_image(second);
    let missed = cluster
        .offer(b"this follower has no image")
        .expect("the leader appends");
    safety.run_until(&mut cluster, 15_000, "the follower caught up", |cluster| {
        settled(cluster, &ids) && cluster.append_position(second) == Some(missed)
    });
    assert_eq!(cluster.counts(second).expect("a member").log_images_lost, 1);
    for id in ids {
        assert_eq!(cluster.commit_position(id), Some(missed), "member {id}");
        assert_eq!(cluster.log(id), cluster.log(leader), "member {id}");
    }
}

/// The history on which the check fails, so that it is known to fail. An election
/// keeps what a quorum committed while a member that holds it is heard, and no
/// protocol keeps it once every member has lost it: all three start again on empty
/// log directories. Two that lost theirs beside a third that kept its own elect
/// nobody while the third is not heard (work item 476), and vote its log in once it
/// is (`lost_log_directory.rs`,
/// `two_members_that_lost_their_logs_vote_in_the_one_that_holds_the_log`), so the
/// whole cluster is wiped here. The three hear each other's empty logs and elect a
/// leader of term 0 a second time, which commits its term's first entry where the
/// first history holds another, and the check says so at the step that commits it.
/// That first entry is the whole of the other history here: nothing is offered after
/// the wipe, and the test passes on the first panic that carries the check's words.
#[test]
#[should_panic(expected = "leads with a log that differs from what was committed")]
fn a_quorum_that_lost_its_logs_commits_another_history_and_the_check_fails() {
    let ids = [0, 1, 2];
    let mut safety = Safety::new(&ids);
    let (mut cluster, _) = elected(1, &mut safety);
    cluster
        .offer(b"the first history, entry 1")
        .expect("the leader appends");
    let end = cluster
        .offer(b"the first history, entry 2")
        .expect("the leader appends");
    safety.run(&mut cluster, 1_000);
    for id in ids {
        assert_eq!(cluster.commit_position(id), Some(end), "member {id}");
    }

    for id in ids {
        safety.wipe(&mut cluster, id);
    }
    // Every voter reports an empty log, and the three elect at once. The first entry
    // the new leader commits is not the first history's.
    safety.run(&mut cluster, 10_000);
}

/// A partition between the leader and one follower stops the log and the consensus
/// messages between those two alone. The leader commits with the other follower, and
/// the parted follower holds what it held. It hears nothing from its leader for the
/// leader heartbeat timeout and holds an election, which it cannot win: the other
/// follower's log is ahead of its own, so it leads no term from the cut to the end.
/// The other follower's election stays closed throughout. When the partition ends
/// the three end behind the leader they had, on one log that holds what was
/// committed while they were parted.
#[test]
fn a_partition_between_the_leader_and_a_follower_ends_with_one_log() {
    let ids = [0, 1, 2];
    let mut safety = Safety::new(&ids);
    let (mut cluster, leader) = elected(1, &mut safety);
    let [parted, other] = followers(&ids, leader)[..] else {
        panic!("two followers");
    };
    safety.run(&mut cluster, 500);
    let held = cluster.append_position(parted).expect("a member");
    let held_log = cluster.log(parted);

    cluster.cut_between(leader, parted);
    let entry = cluster
        .offer(b"one follower is parted from the leader")
        .expect("the leader appends");
    safety.run(&mut cluster, 1_000);
    assert_eq!(cluster.commit_position(leader), Some(entry));
    assert_eq!(cluster.commit_position(other), Some(entry));
    assert_eq!(cluster.append_position(parted), Some(held));

    // Past the leader heartbeat timeout: the parted follower holds an election.
    safety.run(&mut cluster, 11_000);
    let counts = cluster.counts(parted).expect("a member");
    assert!(
        counts.leader_heartbeat_timeouts + counts.log_images_lost >= 1,
        "{counts:?}"
    );
    assert!(counts.elections >= 2, "it began an election: {counts:?}");
    assert_eq!(cluster.leader(), Some(leader), "the leader still leads");
    assert_eq!(
        cluster.log(parted),
        held_log,
        "the parted follower holds what it held"
    );
    assert_eq!(
        cluster.election_state(other),
        Some(ElectionState::Closed),
        "the other follower's election stayed closed"
    );
    assert_eq!(cluster.counts(other).expect("a member").elections, 1);

    cluster.heal_between(parted, leader);
    safety.run_until(&mut cluster, 30_000, "one leader and one log", |cluster| {
        settled(cluster, &ids)
            && ids
                .iter()
                .all(|&id| cluster.commit_position(id) >= Some(entry))
    });
    assert_eq!(cluster.leader(), Some(leader), "the same member leads");
    assert_eq!(
        cluster.counts(parted).expect("a member").terms_led,
        0,
        "a log behind led nothing, from the cut to the end"
    );
    for id in ids {
        assert_eq!(cluster.log(id), cluster.log(leader), "member {id}");
        assert!(
            holds(&cluster.log(id), b"one follower is parted from the leader"),
            "member {id} holds what was committed"
        );
    }
}

/// A cut leader's log reaches no follower: what it appends while cut extends its own
/// log alone, so a quorum has not synced it and it is not committed. When the cut
/// ends inside the leader heartbeat timeout nobody has held an election, each
/// follower's image of the log is still there, what the cut kept from it arrives,
/// and the entry is committed on all three under the same leader.
#[test]
fn a_cut_leaders_log_reaches_no_follower_until_the_cut_ends() {
    let ids = [0, 1, 2];
    let mut safety = Safety::new(&ids);
    let (mut cluster, leader) = elected(1, &mut safety);
    safety.run(&mut cluster, 500);
    let before = cluster.commit_position(leader).expect("the leader");
    let held: Vec<(i32, Vec<u8>)> = followers(&ids, leader)
        .into_iter()
        .map(|id| (id, cluster.log(id)))
        .collect();

    cluster.cut(leader);
    let entry = cluster
        .offer(b"the leader is cut")
        .expect("the leader appends");
    safety.run(&mut cluster, 2_000);
    assert_eq!(cluster.append_position(leader), Some(entry));
    assert_eq!(cluster.commit_position(leader), Some(before));
    for (id, log) in &held {
        assert_eq!(cluster.append_position(*id), Some(before), "member {id}");
        assert_eq!(&cluster.log(*id), log, "member {id}'s log did not move");
    }

    cluster.heal(leader);
    safety.run(&mut cluster, 2_000);
    assert_eq!(cluster.leader(), Some(leader), "it led throughout");
    for id in ids {
        assert_eq!(cluster.commit_position(id), Some(entry), "member {id}");
        assert_eq!(cluster.log(id), cluster.log(leader), "member {id}");
        let counts = cluster.counts(id).expect("a member");
        assert_eq!(counts.elections, 1, "member {id} held no election");
        assert_eq!(counts.log_images_lost, 0, "member {id} kept its image");
    }
}

/// A member that replicated canvasses again with no member's report (ADR 0106). The
/// leader loses its log directory and starts again on an empty one. The two that
/// kept their logs elect one of them, and the member with no log replicates the term
/// it lost and goes back to canvass. The reports it held were read with no log of
/// its own, of logs that were then ahead of it; with the log replicated they are of
/// logs no longer ahead, so it was a unanimous candidate the moment it had the log,
/// nominated itself, and the leader the two had elected gave up the lead on its
/// request vote. With no report it is no candidate: its canvass position is
/// answered by its leader, and it follows. At every seed the leader the two elected
/// still leads once the three hold one log, has given up the lead to nobody, and
/// the member that replicated was never a candidate.
#[test]
fn a_member_that_replicated_canvasses_with_no_reports_and_unseats_no_leader() {
    let ids = [0, 1, 2];
    for seed in 1..=20 {
        let mut safety = Safety::new(&ids);
        let (mut cluster, first) = elected(seed, &mut safety);
        let mut committed = 0;
        for i in 0..10u32 {
            committed = cluster.offer(&i.to_le_bytes()).expect("the leader appends");
            safety.run(&mut cluster, 1);
        }
        safety.run(&mut cluster, 1_000);
        let others = followers(&ids, first);

        safety.wipe(&mut cluster, first);
        safety.run_until(
            &mut cluster,
            30_000,
            "the member with no log holds what was committed",
            |cluster| cluster.append_position(first) >= Some(committed),
        );
        let second = cluster.leader().expect("a leader");
        assert!(
            others.contains(&second),
            "seed {seed}: member {second} leads"
        );
        let term = cluster
            .member(second)
            .expect("a member")
            .leadership_term_id();
        // Term 1, or a later one where the two split their first vote (seed 12).
        assert!(term >= 1, "seed {seed}: the two elected in term {term}");

        // Long enough for a member that nominated itself to have unseated the leader
        // and for the three to have settled again.
        safety.run(&mut cluster, 5_000);
        assert!(settled(&cluster, &ids), "seed {seed}");
        assert_eq!(
            cluster.leader(),
            Some(second),
            "seed {seed}: the same leader"
        );
        for id in ids {
            assert_eq!(
                cluster.member(id).expect("a member").leadership_term_id(),
                term,
                "seed {seed}: member {id} follows the term the two elected in"
            );
        }
        let replicated = cluster.counts(first).expect("a member");
        assert_eq!(replicated.candidacies, 0, "seed {seed}: {replicated:?}");
        let leads = cluster.counts(second).expect("the leader");
        assert_eq!(leads.leader_step_downs, 0, "seed {seed}: {leads:?}");
        assert_eq!(cluster.log(first), cluster.log(second), "seed {seed}");
    }
}
