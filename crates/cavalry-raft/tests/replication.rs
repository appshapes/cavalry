//! The replicated log with an appointed leader, three cores at a time in memory
//! (`spec/cluster.md`, "The appointed leader"; ADR 0063): what a cluster does when it
//! starts together, when a member starts late, when syncs lag, when the leader loses
//! its quorum and when the consensus stream loses and delays messages. Every test
//! checks, at every step, that nothing uncommitted is called committed.

use cavalry_proto::cluster::{MAX_MEMBERS, Message, Stream, TimeUnit, quorum};
use cavalry_raft::memory::Cluster;
use cavalry_raft::{Consensus, ElectionState, MembersRefused, NO_SESSION, Refused, Role};

/// What must hold after every step (invariant 1): the leader's commit position is at
/// or below the append positions of a quorum of members, so a committed entry is
/// durable on a quorum; no member knows a commit position above the leader's; no
/// commit position falls; and every member's log is the leader's log up to where it
/// reaches.
struct Safety {
    ids: Vec<i32>,
    leader: i32,
    last_commit: Vec<i64>,
}

impl Safety {
    fn new(ids: &[i32]) -> Self {
        Self {
            ids: ids.to_vec(),
            leader: *ids.iter().min().expect("a member"),
            last_commit: vec![0; ids.len()],
        }
    }

    fn check(&mut self, cluster: &Cluster) {
        let commit = cluster.commit_position(self.leader).expect("the leader");
        let mut appended: Vec<i64> = self
            .ids
            .iter()
            .map(|&id| cluster.append_position(id).expect("a member"))
            .collect();
        appended.sort_unstable_by(|a, b| b.cmp(a));
        let durable_on_a_quorum = appended[quorum(self.ids.len()) - 1];
        assert!(
            commit <= durable_on_a_quorum,
            "commit position {commit} is above what a quorum has synced, {durable_on_a_quorum}"
        );
        let leader_log = cluster.log(self.leader);
        for (index, &id) in self.ids.iter().enumerate() {
            let known = cluster.commit_position(id).expect("a member");
            assert!(
                known <= commit,
                "member {id} knows {known}, above the leader's {commit}"
            );
            assert!(
                known >= self.last_commit[index],
                "member {id}'s commit position fell"
            );
            self.last_commit[index] = known;
            let log = cluster.log(id);
            assert!(
                leader_log.starts_with(&log),
                "member {id}'s log is not a prefix of the leader's"
            );
        }
    }

    fn run(&mut self, cluster: &mut Cluster, milliseconds: u64) {
        for _ in 0..milliseconds {
            cluster.step();
            self.check(cluster);
        }
    }
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

#[test]
fn a_cluster_started_together_appoints_its_leader_replicates_and_commits() {
    let ids = [0, 1, 2];
    let mut cluster = Cluster::new(&ids).expect("three members");
    let mut safety = Safety::new(&ids);
    safety.run(&mut cluster, 1_000);

    assert_eq!(cluster.role(0), Some(Role::Leader));
    for id in [1, 2] {
        assert_eq!(cluster.role(id), Some(Role::Follower));
    }
    for id in ids {
        assert_eq!(cluster.election_state(id), Some(ElectionState::Closed));
        let member = cluster.member(id).expect("a member");
        assert_eq!(member.leadership_term_id(), 0);
        assert_eq!(member.leader_member_id(), 0);
        let counts = member.counts();
        assert_eq!(counts.elections, 1);
        assert_eq!(counts.elections_failed, 0);
        assert_eq!(counts.terms_led, u64::from(id == 0));
    }
    // The leader stored the term it proposed before it led; a follower's raise on
    // following is in memory alone. Every member recorded term 0 at position 0 before
    // it reported for it.
    assert_eq!(cluster.record_of_terms(0), Some((vec![(0, 0)], 0)));
    assert_eq!(cluster.record_of_terms(1), Some((vec![(0, 0)], -1)));
    assert_eq!(cluster.record_of_terms(2), Some((vec![(0, 0)], -1)));

    // The term's first entry is its new leadership term event, at the base.
    let log = cluster.log(0);
    let event = Message::parse(Stream::Log, &log[..term_event_length()]).expect("an entry");
    assert!(matches!(
        event,
        Message::NewLeadershipTermEvent {
            leadership_term_id: 0,
            log_position: 0,
            term_base_log_position: 0,
            leader_member_id: 0,
            ..
        }
    ));
    let after_event = cluster.commit_position(0).expect("the leader");
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
        assert_eq!(cluster.commit_position(id), Some(last));
        assert_eq!(cluster.append_position(id), Some(last));
        assert_eq!(cluster.log(id), cluster.log(0));
    }
    let log = cluster.log(0);
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
}

/// The full case of the member table: eight members, the bound, replicate as three
/// do, and a ninth is refused before anything runs.
#[test]
fn eight_members_replicate_and_a_ninth_is_refused() {
    let ids: Vec<i32> = (10..18).collect();
    assert_eq!(ids.len(), MAX_MEMBERS);
    let mut cluster = Cluster::new(&ids).expect("eight members");
    let mut safety = Safety::new(&ids);
    safety.run(&mut cluster, 1_000);
    let position = cluster.offer(b"eight").expect("the leader appends");
    safety.run(&mut cluster, 1_000);
    for &id in &ids {
        assert_eq!(
            cluster.role(id),
            Some(if id == 10 {
                Role::Leader
            } else {
                Role::Follower
            })
        );
        assert_eq!(cluster.commit_position(id), Some(position));
    }

    let nine: Vec<i32> = (0..9).collect();
    assert_eq!(
        Consensus::new(&nine, 0).err(),
        Some(MembersRefused::Count { found: 9 })
    );
    assert_eq!(
        Consensus::new(&[], 0).err(),
        Some(MembersRefused::Count { found: 0 })
    );
    assert_eq!(
        Consensus::new(&[0, 1, 1], 0).err(),
        Some(MembersRefused::Id { id: 1 })
    );
    assert_eq!(
        Consensus::new(&[0, -4, 1], 0).err(),
        Some(MembersRefused::Id { id: -4 })
    );
    let not_a_member = Consensus::new(&[0, 1, 2], 3).expect_err("3 is not a member");
    assert_eq!(not_a_member, MembersRefused::NotAMember { id: 3 });
    assert!(
        not_a_member
            .to_string()
            .contains("docs/reference/cluster.md#members-and-streams")
    );
}

/// A cold start with a member missing. The appointed leader canvasses for the startup
/// canvass timeout, 60 s, before it leads with a quorum, and closes its election once
/// the member it heard in its canvass has joined its log. The member that comes after
/// the term's first entry cannot join without the catch-up: it is counted and left
/// out, and the two go on committing.
#[test]
fn the_leader_canvasses_for_a_missing_member_then_leads_with_a_quorum_and_it_stays_out() {
    let ids = [0, 1, 2];
    let mut cluster = Cluster::new(&ids).expect("three members");
    let mut safety = Safety::new(&ids);
    cluster.halt(2);
    safety.run(&mut cluster, 59_900);
    // A quorum is there, and the appointed leader still canvasses for the third.
    assert_eq!(cluster.role(0), Some(Role::Follower));
    assert_eq!(cluster.election_state(0), Some(ElectionState::Canvass));
    assert_eq!(
        cluster.record_of_terms(0),
        Some((vec![], -1)),
        "nothing proposed yet"
    );
    assert_eq!(cluster.offer(b"early"), Err(Refused::Election));
    assert!(
        cluster.log(0).is_empty(),
        "no entry before the election closes"
    );

    // The startup canvass timeout: a quorum candidate leads, the member it heard joins
    // its log, and it closes with the two.
    safety.run(&mut cluster, 1_100);
    assert_eq!(cluster.role(0), Some(Role::Leader));
    assert_eq!(cluster.election_state(0), Some(ElectionState::Closed));
    assert_eq!(cluster.election_state(1), Some(ElectionState::Closed));
    let first = cluster.offer(b"two of three").expect("the leader appends");
    safety.run(&mut cluster, 500);
    assert_eq!(cluster.commit_position(0), Some(first));
    assert_eq!(cluster.commit_position(1), Some(first));

    // The third member starts now. The leader's log is past its position, which only
    // a catch-up could close: each election it begins fails and is counted.
    cluster.resume(2);
    safety.run(&mut cluster, 2_000);
    let late = cluster.counts(2).expect("a member");
    // One failed election for each round of canvass, every election status interval,
    // 100 ms: about twenty in two seconds, and no faster.
    assert!((10..=21).contains(&late.elections_failed), "{late:?}");
    assert!(late.elections - late.elections_failed <= 1, "{late:?}");
    assert_ne!(cluster.election_state(2), Some(ElectionState::Closed));
    assert_eq!(cluster.append_position(2), Some(0));
    assert!(cluster.log(2).is_empty());
    // And the cluster of two commits as before.
    let second = cluster.offer(b"still two").expect("the leader appends");
    safety.run(&mut cluster, 500);
    assert_eq!(cluster.commit_position(0), Some(second));
    assert_eq!(cluster.commit_position(1), Some(second));
    // The late member knows the commit position the leader's new leadership term told
    // it, and holds none of the log: its own log is still empty.
    assert!(cluster.commit_position(2) <= Some(second));
    assert!(cluster.log(2).is_empty());
}

/// Leader ready waits for the members the canvass heard, not for the first quorum of
/// them. Of five members four are there: the leader leads with them at the startup
/// canvass timeout and appends the term's first entry only once all four have joined
/// its log, so that none of the four misses it. And a member that canvassed and then
/// stopped before it joined is waited for no longer than the leader heartbeat timeout,
/// 10 s, after which the leader closes with its quorum.
#[test]
fn leader_ready_waits_for_every_member_the_canvass_heard_for_the_heartbeat_timeout() {
    let ids = [0, 1, 2, 3, 4];
    let mut cluster = Cluster::new(&ids).expect("five members");
    let mut safety = Safety::new(&ids);
    cluster.halt(4);
    safety.run(&mut cluster, 61_000);
    for id in [0, 1, 2, 3] {
        assert_eq!(
            cluster.election_state(id),
            Some(ElectionState::Closed),
            "member {id}"
        );
    }
    let entry = cluster.offer(b"four of five").expect("the leader appends");
    safety.run(&mut cluster, 500);
    for id in [0, 1, 2, 3] {
        assert_eq!(
            cluster.append_position(id),
            Some(entry),
            "member {id} holds the log"
        );
        assert_eq!(cluster.counts(id).expect("a member").elections_failed, 0);
    }

    // Three members, all there for the canvass; one stops as the leader begins to lead.
    let ids = [0, 1, 2];
    let mut cluster = Cluster::new(&ids).expect("three members");
    let mut safety = Safety::new(&ids);
    let mut steps = 0;
    while cluster.role(0) != Some(Role::Leader) {
        safety.run(&mut cluster, 1);
        steps += 1;
        assert!(steps < 1_000, "the leader did not lead");
    }
    cluster.halt(2);
    safety.run(&mut cluster, 9_000);
    assert_eq!(cluster.election_state(0), Some(ElectionState::LeaderReady));
    assert_eq!(cluster.offer(b"early"), Err(Refused::Election));
    assert!(
        cluster.log(0).is_empty(),
        "no entry while it waits for the member it heard"
    );
    safety.run(&mut cluster, 1_500);
    assert_eq!(cluster.election_state(0), Some(ElectionState::Closed));
    let entry = cluster.offer(b"two of three").expect("the leader appends");
    safety.run(&mut cluster, 500);
    assert_eq!(cluster.commit_position(1), Some(entry));
}

/// A member that passes follower replay on the leader's last word and then finds the
/// leader's image somewhere else: the leader had appended what it had not yet synced,
/// so its announced log position was behind its publication. The image joins past the
/// member's append position, which would leave a hole: the member counts the
/// mismatch, leaves the log and fails its election.
#[test]
fn an_image_that_joins_past_the_append_position_is_counted_and_left() {
    let ids = [0, 1, 2];
    let mut cluster = Cluster::new(&ids).expect("three members");
    let mut safety = Safety::new(&ids);
    cluster.halt(2);
    // The leader's recorder never syncs: its append position stays 0 while its
    // publication moves on.
    cluster.pause_sync(0);
    safety.run(&mut cluster, 61_000);
    assert_eq!(cluster.election_state(0), Some(ElectionState::Closed));
    assert_eq!(cluster.append_position(0), Some(0));
    assert!(!cluster.log(0).is_empty(), "the term event is appended");

    cluster.resume(2);
    safety.run(&mut cluster, 2_000);
    let late = cluster.counts(2).expect("a member");
    assert!(late.join_position_mismatches > 0, "{late:?}");
    assert!(late.elections_failed >= late.join_position_mismatches);
    assert_ne!(cluster.election_state(2), Some(ElectionState::Closed));
    assert!(
        cluster.log(2).is_empty(),
        "the image it left gave it nothing"
    );
    assert_eq!(cluster.append_position(2), Some(0));
}

/// The leader loses its log directory and starts again on an empty one, which a
/// container with no volume does by itself. Its followers hold a term and a log it no
/// longer has. It canvasses, every report it reads is of a log ahead of its own, and it
/// never leads: it does not lead term 0 a second time, and nothing it could append
/// would be counted as committed by positions of another log. The cluster stops, and
/// what was committed is where it was, on the two members that hold it.
#[test]
fn a_leader_that_lost_its_log_directory_never_leads_again() {
    let ids = [0, 1, 2];
    let mut cluster = Cluster::new(&ids).expect("three members");
    let mut safety = Safety::new(&ids);
    safety.run(&mut cluster, 1_000);
    let mut committed = 0;
    for i in 0..100u32 {
        committed = cluster.offer(&i.to_le_bytes()).expect("the leader appends");
        safety.run(&mut cluster, 1);
    }
    safety.run(&mut cluster, 1_000);
    assert_eq!(cluster.commit_position(1), Some(committed));
    let logs = [cluster.log(1), cluster.log(2)];

    cluster.wipe(0);
    // Past the startup canvass timeout and the leader heartbeat timeout both.
    cluster.run_ms(75_000);
    assert_eq!(cluster.role(0), Some(Role::Follower));
    assert_eq!(cluster.election_state(0), Some(ElectionState::Canvass));
    assert_eq!(
        cluster.record_of_terms(0),
        Some((vec![], -1)),
        "it proposed no term"
    );
    let counts = cluster.counts(0).expect("a member");
    assert_eq!(counts.terms_led, 0);
    assert_eq!(
        counts.appointed_leader_behind, 2,
        "each follower ahead, counted once"
    );
    assert!(cluster.log(0).is_empty());
    assert_eq!(cluster.offer(b"again"), Err(Refused::Election));
    for (id, log) in [1, 2].into_iter().zip(&logs) {
        // The followers lost the leader's image, began again and canvass; their logs
        // and what they know committed are untouched.
        assert_eq!(cluster.counts(id).expect("a member").log_images_lost, 1);
        assert_eq!(cluster.election_state(id), Some(ElectionState::Canvass));
        assert_eq!(&cluster.log(id), log);
        assert_eq!(cluster.append_position(id), Some(committed));
        assert_eq!(cluster.commit_position(id), Some(committed));
        assert_eq!(cluster.record_of_terms(id), Some((vec![(0, 0)], -1)));
    }
}

/// The commit position is a quorum of synced positions and never more than the
/// leader's own: what has only arrived, on a follower or on the leader, commits
/// nothing.
#[test]
fn a_commit_waits_for_a_quorum_of_synced_positions_and_for_the_leaders_own() {
    let ids = [0, 1, 2];
    let mut cluster = Cluster::new(&ids).expect("three members");
    let mut safety = Safety::new(&ids);
    safety.run(&mut cluster, 1_000);
    let before = cluster.commit_position(0).expect("the leader");

    // Neither follower syncs: the entry reaches all three and is durable on one.
    cluster.pause_sync(1);
    cluster.pause_sync(2);
    let entry = cluster.offer(b"one of three").expect("the leader appends");
    safety.run(&mut cluster, 1_000);
    assert_eq!(cluster.append_position(0), Some(entry));
    assert_eq!(cluster.log(1), cluster.log(0), "the entry arrived");
    assert_eq!(
        cluster.commit_position(0),
        Some(before),
        "one member is no quorum"
    );

    // One follower syncs: two of three, committed. The other is told the commit
    // position it has not yet synced up to.
    cluster.resume_sync(1);
    safety.run(&mut cluster, 500);
    for id in ids {
        assert_eq!(cluster.commit_position(id), Some(entry));
    }
    assert_eq!(cluster.append_position(2), Some(before));
    cluster.resume_sync(2);
    safety.run(&mut cluster, 500);

    // The leader's own recorder lags: both followers hold the next entry durably, and
    // it is not committed until the leader does too.
    cluster.pause_sync(0);
    let next = cluster
        .offer(b"not the leader's yet")
        .expect("the leader appends");
    safety.run(&mut cluster, 1_000);
    assert_eq!(cluster.append_position(1), Some(next));
    assert_eq!(cluster.append_position(2), Some(next));
    assert_eq!(cluster.commit_position(0), Some(entry));
    cluster.resume_sync(0);
    safety.run(&mut cluster, 500);
    for id in ids {
        assert_eq!(cluster.commit_position(id), Some(next));
    }
}

/// A leader whose followers are gone stays the leader, since no election can replace
/// it, and commits nothing more. A follower that hears nothing for the leader
/// heartbeat timeout, 10 s, counts it and begins again; with nothing appended
/// meanwhile it is level with the leader and follows again.
#[test]
fn a_leader_below_a_quorum_stays_and_stops_committing_and_level_followers_rejoin() {
    let ids = [0, 1, 2];
    let mut cluster = Cluster::new(&ids).expect("three members");
    let mut safety = Safety::new(&ids);
    safety.run(&mut cluster, 1_000);
    let before = cluster.commit_position(0).expect("the leader");

    cluster.cut(1);
    cluster.cut(2);
    safety.run(&mut cluster, 11_000);
    assert_eq!(cluster.role(0), Some(Role::Leader));
    assert_eq!(cluster.election_state(0), Some(ElectionState::Closed));
    assert_eq!(cluster.commit_position(0), Some(before));
    assert_eq!(
        cluster.counts(0).expect("the leader").leader_quorum_losses,
        1
    );
    for id in [1, 2] {
        let counts = cluster.counts(id).expect("a member");
        assert_eq!(counts.leader_heartbeat_timeouts, 1, "member {id}");
        assert_eq!(cluster.election_state(id), Some(ElectionState::Canvass));
    }

    cluster.heal(1);
    cluster.heal(2);
    safety.run(&mut cluster, 1_000);
    for id in [1, 2] {
        assert_eq!(
            cluster.election_state(id),
            Some(ElectionState::Closed),
            "member {id}"
        );
        let counts = cluster.counts(id).expect("a member");
        assert_eq!((counts.elections, counts.elections_failed), (2, 0));
        assert_eq!(counts.join_position_mismatches, 0);
        // The term was recorded once: following it again records nothing.
        assert_eq!(cluster.record_of_terms(id), Some((vec![(0, 0)], -1)));
    }
    let after = cluster.offer(b"three again").expect("the leader appends");
    safety.run(&mut cluster, 500);
    for id in ids {
        assert_eq!(cluster.commit_position(id), Some(after));
    }
    // The quorum came back and was lost once: a second loss would be counted again.
    assert_eq!(
        cluster.counts(0).expect("the leader").leader_quorum_losses,
        1
    );
}

/// The same loss of a quorum with the leader appending through it: what it appended
/// is on one member, so it is not committed, and the followers that come back are
/// behind the leader's log, which only a catch-up could close. They are counted and
/// stay out; nothing uncommitted is ever called committed.
#[test]
fn entries_appended_below_a_quorum_are_never_committed_without_one() {
    let ids = [0, 1, 2];
    let mut cluster = Cluster::new(&ids).expect("three members");
    let mut safety = Safety::new(&ids);
    safety.run(&mut cluster, 1_000);
    let before = cluster.commit_position(0).expect("the leader");

    cluster.cut(1);
    cluster.cut(2);
    let alone = cluster.offer(b"alone").expect("the leader appends");
    safety.run(&mut cluster, 11_000);
    assert_eq!(cluster.append_position(0), Some(alone));
    assert_eq!(cluster.commit_position(0), Some(before));

    cluster.heal(1);
    cluster.heal(2);
    safety.run(&mut cluster, 2_000);
    assert_eq!(cluster.commit_position(0), Some(before));
    for id in [1, 2] {
        let counts = cluster.counts(id).expect("a member");
        assert!(counts.elections_failed > 0, "member {id}: {counts:?}");
        assert_ne!(cluster.election_state(id), Some(ElectionState::Closed));
        assert_eq!(cluster.commit_position(id), Some(before));
    }
}

/// The consensus stream loses one message in three and delivers the rest up to 50 ms
/// late and out of order, from the first step: the cluster still forms, replicates
/// and commits, and the safety check holds at every step.
#[test]
fn lost_and_late_consensus_messages_change_no_outcome() {
    let ids = [0, 1, 2];
    let mut cluster = Cluster::new(&ids).expect("three members");
    let mut safety = Safety::new(&ids);
    cluster.set_delivery(3, 50);
    safety.run(&mut cluster, 5_000);
    for id in ids {
        assert_eq!(
            cluster.election_state(id),
            Some(ElectionState::Closed),
            "member {id}"
        );
    }
    let mut last = 0;
    for i in 0..500u32 {
        last = cluster.offer(&i.to_le_bytes()).expect("the leader appends");
        safety.run(&mut cluster, 3);
    }
    safety.run(&mut cluster, 3_000);
    for id in ids {
        assert_eq!(cluster.commit_position(id), Some(last), "member {id}");
        assert_eq!(cluster.log(id), cluster.log(0), "member {id}");
        let counts = cluster.counts(id).expect("a member");
        assert_eq!(counts.elections_failed, 0, "member {id}");
        assert_eq!(counts.leader_heartbeat_timeouts, 0, "member {id}");
    }
}

/// What an offer is refused for, each with what to do next.
#[test]
fn an_offer_is_refused_before_the_election_closes_and_under_back_pressure() {
    let mut cluster = Cluster::new(&[0, 1, 2]).expect("three members");
    let refused = cluster
        .offer(b"too early")
        .expect_err("the election has not closed");
    assert_eq!(refused, Refused::Election);
    assert!(
        refused
            .to_string()
            .contains("offer again once every member has joined")
    );
    cluster.run_ms(1_000);

    cluster.back_pressure(true);
    let refused = cluster.offer(b"not now").expect_err("back-pressured");
    assert_eq!(refused, Refused::BackPressured);
    assert!(
        refused
            .to_string()
            .contains("docs/reference/cluster.md#roles-and-the-election")
    );
    let before = cluster.log(0);
    cluster.run_ms(100);
    assert_eq!(cluster.log(0), before, "a refused offer wrote nothing");
    cluster.back_pressure(false);
    cluster.offer(b"now").expect("the leader appends");
}

/// A follower whose image of the log is gone while its leader still speaks: it cannot
/// append from an image it no longer has, so it counts the loss and begins again. It
/// left the log in init, the image it joins next is a new one, and that image joins at
/// its append position only when the leader appended nothing meanwhile.
#[test]
fn a_follower_that_loses_its_image_begins_again_and_joins_a_new_one_only_when_level() {
    let ids = [0, 1, 2];
    let mut cluster = Cluster::new(&ids).expect("three members");
    let mut safety = Safety::new(&ids);
    safety.run(&mut cluster, 1_000);

    // Level: the new image joins where the member's log ends, and it follows again.
    cluster.lose_image(1);
    safety.run(&mut cluster, 1_000);
    let counts = cluster.counts(1).expect("a member");
    assert_eq!(counts.log_images_lost, 1);
    assert_eq!((counts.elections, counts.elections_failed), (2, 0));
    assert_eq!(cluster.election_state(1), Some(ElectionState::Closed));
    let entry = cluster
        .offer(b"after the first loss")
        .expect("the leader appends");
    safety.run(&mut cluster, 500);
    assert_eq!(cluster.append_position(1), Some(entry));
    assert_eq!(cluster.log(1), cluster.log(0));

    // Not level: the leader appends while member 2 has no image. Its new image would
    // join past its append position, so it stays out, counted, and the two commit.
    cluster.lose_image(2);
    let missed = cluster
        .offer(b"member 2 has no image")
        .expect("the leader appends");
    safety.run(&mut cluster, 2_000);
    let counts = cluster.counts(2).expect("a member");
    assert_eq!(counts.log_images_lost, 1);
    assert!(counts.elections_failed > 0, "{counts:?}");
    assert_ne!(cluster.election_state(2), Some(ElectionState::Closed));
    assert_eq!(cluster.append_position(2), Some(entry));
    assert_eq!(cluster.commit_position(0), Some(missed));
    assert_eq!(cluster.commit_position(1), Some(missed));
}

/// A cluster of one member is its own quorum: it leads from init, with nobody to
/// canvass, closes its election, and commits what its own recorder has synced.
#[test]
fn a_cluster_of_one_leads_from_init_and_commits_alone() {
    let ids = [5];
    let mut cluster = Cluster::new(&ids).expect("one member");
    let mut safety = Safety::new(&ids);
    safety.run(&mut cluster, 1);
    assert_eq!(cluster.role(5), Some(Role::Leader));
    assert_ne!(cluster.election_state(5), Some(ElectionState::Canvass));
    safety.run(&mut cluster, 20);
    assert_eq!(cluster.election_state(5), Some(ElectionState::Closed));
    let counts = cluster.counts(5).expect("the member");
    assert_eq!((counts.elections, counts.terms_led), (1, 1));
    let entry = cluster.offer(b"alone").expect("the leader appends");
    safety.run(&mut cluster, 20);
    assert_eq!(cluster.commit_position(5), Some(entry));
}
