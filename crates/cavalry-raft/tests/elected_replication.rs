//! Replication and commit under an elected leader, in memory (`spec/cluster.md`, "The
//! commit position", "Members and streams"; ADR 0060, ADR 0069, ADR 0100): the rules
//! a cluster has whoever leads it, run on the election's path. What a commit needs,
//! what an offer is refused for, what a lossy and late consensus stream changes, the
//! bound of the members table and of the stream, a cluster of one, and two followers
//! parted. Every test checks, at every step, that no committed entry is lost, and
//! that the leader commits nothing a quorum has not synced.
//!
//! These rules were tested on the appointed leader's path in `replication.rs`, which
//! keeps the tests of that path's own rules until the path leaves (ADR 0100).

mod elected_cluster;

use cavalry_proto::cluster::MAX_MEMBERS;
use cavalry_raft::memory::{Cluster, IN_FLIGHT_CAPACITY, MAX_DELAY_MS};
use cavalry_raft::{Consensus, ElectionState, MembersRefused, Refused, Role};
use elected_cluster::{Safety, elected, holds, settled};

/// The two members of `ids` that `leader` leads.
fn followers(ids: &[i32], leader: i32) -> Vec<i32> {
    ids.iter().copied().filter(|&id| id != leader).collect()
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
            assert!(
                refused
                    .to_string()
                    .contains("docs/reference/cluster.md#roles-and-the-election"),
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
    assert!(
        refused
            .to_string()
            .contains("docs/reference/cluster.md#roles-and-the-election")
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
/// late and out of order, from the first step: the cluster still elects a leader,
/// replicates and commits, the three logs are one, and the safety check holds at
/// every step. An election may take more than one ballot on such a stream; none
/// fails once a leader has closed.
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
    let mut last = 0;
    let mut offered = 0u32;
    // An offer is refused while no leader has closed; on this stream that can be
    // again, so each entry is offered until it is taken.
    while offered < 500 {
        match cluster.offer(&offered.to_le_bytes()) {
            Ok(position) => {
                last = position;
                offered += 1;
            }
            Err(Refused::Election) => {}
            Err(other) => panic!("{other}"),
        }
        safety.run(&mut cluster, 3);
    }
    cluster.set_delivery(0, 0);
    safety.run_until(&mut cluster, 30_000, "every member committed", |cluster| {
        settled(cluster, &ids)
            && ids
                .iter()
                .all(|&id| cluster.commit_position(id) >= Some(last))
    });
    let leader = cluster.leader().expect("a leader");
    for id in ids {
        assert_eq!(cluster.log(id), cluster.log(leader), "member {id}");
    }
}

/// The full case of the members table: eight members, the bound, elect a leader and
/// replicate as three do, and a ninth is refused before anything runs, as every
/// members string the page refuses is.
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
/// and every member learns its commit position. A delay past `MAX_DELAY_MS` is held
/// to it.
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
    let leader = cluster.leader().expect("a leader");
    let last = cluster.offer(b"the last").expect("the leader appends");
    safety.run(&mut cluster, 3_000);
    assert!(cluster.in_flight() < IN_FLIGHT_CAPACITY);
    for &id in &ids {
        assert_eq!(cluster.commit_position(id), Some(last), "member {id}");
        assert_eq!(cluster.log(id), cluster.log(leader), "member {id}");
    }

    assert_eq!(cluster.set_delivery(0, u64::MAX), MAX_DELAY_MS);
    safety.run(&mut cluster, 100);
    assert!(cluster.in_flight() <= IN_FLIGHT_CAPACITY);
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
/// ends. Behind it, the leader having appended meanwhile, it catches up and joins all
/// the same, where a member whose leader is appointed stayed out
/// (`replication.rs`).
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
        cluster
            .member(first)
            .expect("a member")
            .election_cause()
            .code(),
        2
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
/// keeps what a quorum committed while a quorum keeps its logs, and no protocol keeps
/// it when a quorum loses them: two of three start again on empty log directories
/// while the third, which kept its own, is cut off. The two elect a leader of term 0
/// a second time and commit another history, and the check says so at the step that
/// commits it.
#[test]
#[should_panic(expected = "leads with a log that differs from what was committed")]
fn a_quorum_that_lost_its_logs_commits_another_history_and_the_check_fails() {
    let ids = [0, 1, 2];
    let mut safety = Safety::new(&ids);
    let (mut cluster, leader) = elected(1, &mut safety);
    cluster
        .offer(b"the first history, entry 1")
        .expect("the leader appends");
    let end = cluster
        .offer(b"the first history, entry 2")
        .expect("the leader appends");
    safety.run(&mut cluster, 1_000);
    let [kept, lost] = followers(&ids, leader)[..] else {
        panic!("two followers");
    };
    assert_eq!(cluster.commit_position(kept), Some(end));

    cluster.cut(kept);
    safety.wipe(&mut cluster, leader);
    safety.wipe(&mut cluster, lost);
    // The two hear no member ahead of them and elect at the startup canvass timeout,
    // 60 s. The first entry the new leader commits is not the first history's.
    safety.run(&mut cluster, 70_000);
}

/// A partition between the leader and one follower stops the log and the consensus
/// messages between those two alone. The leader commits with the other follower, and
/// the parted follower holds what it held. It hears nothing from its leader for the
/// leader heartbeat timeout and holds an election, which it cannot win: the other
/// follower's log is ahead of its own. When the partition ends the three end behind
/// one leader on one log that holds what was committed while they were parted, where
/// a member whose leader is appointed stayed out (`replication.rs`).
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
    assert_ne!(cluster.leader(), Some(parted), "a log behind leads nothing");

    cluster.heal_between(parted, leader);
    safety.run_until(&mut cluster, 30_000, "one leader and one log", |cluster| {
        settled(cluster, &ids)
            && ids
                .iter()
                .all(|&id| cluster.commit_position(id) >= Some(entry))
    });
    let leads = cluster.leader().expect("a leader");
    for id in ids {
        assert_eq!(cluster.log(id), cluster.log(leads), "member {id}");
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

    cluster.cut(leader);
    let entry = cluster
        .offer(b"the leader is cut")
        .expect("the leader appends");
    safety.run(&mut cluster, 2_000);
    assert_eq!(cluster.append_position(leader), Some(entry));
    assert_eq!(cluster.commit_position(leader), Some(before));
    for id in followers(&ids, leader) {
        assert_eq!(cluster.append_position(id), Some(before), "member {id}");
    }

    cluster.heal(leader);
    safety.run(&mut cluster, 2_000);
    assert_eq!(cluster.leader(), Some(leader), "it led throughout");
    for id in ids {
        assert_eq!(cluster.commit_position(id), Some(entry), "member {id}");
        assert_eq!(cluster.log(id), cluster.log(leader), "member {id}");
        assert_eq!(
            cluster.counts(id).expect("a member").elections,
            1,
            "member {id}"
        );
    }
}
