//! The election, three cores at a time in memory (`spec/cluster.md`, "Roles and the
//! election"; ADR 0077, ADR 0081, ADR 0082): members that start together elect one
//! leader, a leader that stops is replaced by a member whose log a quorum votes for, a
//! member votes once in a term, a leader that is cut off or stood still gives up the
//! lead, a leader that a quorum cannot hear, or whose module stood still while
//! its driver received, steps down on the request vote of the term after its own, and
//! a leader closes its election with a quorum and waits for no other member.
//! Every test checks, at every step, that no committed entry is lost, what one
//! leader committed is in the log of every leader after it, and that no two members
//! lead one term.
//!
//! A member on this path truncates a tail no quorum recorded, replicates a term it
//! missed and catches up within its leader's term (ADR 0083, ADR 0086, ADR 0088), so
//! a member that comes back behind its leader ends each test on the leader's log.

use cavalry_proto::cluster::quorum;
use cavalry_raft::memory::Cluster;
use cavalry_raft::{ElectionState, Refused, Role};

/// What must hold after every step (invariant 1), whoever leads. The bytes a leader
/// has committed are kept, and every later commit of any leader extends them: a
/// committed entry is never lost and never replaced. A member's own log up to the
/// commit position it knows is part of them. The leader's commit position is at or
/// below the append positions of a quorum, no member's commit position falls, and no
/// two members lead one term, running or stopped.
struct Safety {
    ids: Vec<i32>,
    committed: Vec<u8>,
    last_commit: Vec<i64>,
}

impl Safety {
    fn new(ids: &[i32]) -> Self {
        Self {
            ids: ids.to_vec(),
            committed: Vec::new(),
            last_commit: vec![0; ids.len()],
        }
    }

    fn check(&mut self, cluster: &Cluster) {
        let mut led: Vec<i64> = self
            .ids
            .iter()
            .filter_map(|&id| cluster.member(id))
            .filter(|member| member.role() == Role::Leader)
            .map(|member| member.leadership_term_id())
            .collect();
        led.sort_unstable();
        assert!(
            led.windows(2).all(|pair| pair[0] != pair[1]),
            "two members lead one term: {led:?}"
        );
        if let Some(leader) = cluster.leader() {
            let commit = cluster.commit_position(leader).expect("the leader");
            let mut appended: Vec<i64> = self
                .ids
                .iter()
                .map(|&id| cluster.append_position(id).expect("a member"))
                .collect();
            appended.sort_unstable_by(|a, b| b.cmp(a));
            let durable_on_a_quorum = appended[quorum(self.ids.len()) - 1];
            assert!(
                commit <= durable_on_a_quorum,
                "member {leader} leads and commits {commit}, above what a quorum has \
                 synced, {durable_on_a_quorum}"
            );
            let committed = cluster.log_to(leader, commit);
            assert!(
                committed.starts_with(&self.committed) || self.committed.starts_with(&committed),
                "member {leader} leads with a log that differs from what was committed"
            );
            if committed.len() > self.committed.len() {
                self.committed = committed;
            }
        }
        for (index, &id) in self.ids.iter().enumerate() {
            let known = cluster.commit_position(id).expect("a member");
            assert!(
                known >= self.last_commit[index],
                "member {id}'s commit position fell"
            );
            self.last_commit[index] = known;
            let held = cluster.log_to(id, known);
            assert!(
                self.committed.starts_with(&held) || held.starts_with(&self.committed),
                "member {id} holds, below the commit position it knows, bytes that were \
                 not committed"
            );
        }
    }

    fn run(&mut self, cluster: &mut Cluster, milliseconds: u64) {
        for _ in 0..milliseconds {
            cluster.step();
            self.check(cluster);
        }
    }

    /// Starts `member` again on the log directory it kept ([`Cluster::restart`]). A
    /// member that starts knows no commit position until a leader tells it one, so
    /// the last it knew is forgotten here; what the cluster committed is not, and
    /// every check of it goes on.
    fn restart(&mut self, cluster: &mut Cluster, member: i32) {
        cluster.restart(member);
        if let Some(index) = self.ids.iter().position(|&id| id == member) {
            self.last_commit[index] = 0;
        }
    }

    /// Steps until `done`, for at most `limit_ms`, and returns how many steps it took.
    fn run_until(
        &mut self,
        cluster: &mut Cluster,
        limit_ms: u64,
        what: &str,
        done: impl Fn(&Cluster) -> bool,
    ) -> u64 {
        for step in 0..limit_ms {
            if done(cluster) {
                return step;
            }
            cluster.step();
            self.check(cluster);
        }
        assert!(done(cluster), "{what}: not within {limit_ms} ms");
        limit_ms
    }
}

/// Whether `log`, a member's log as [`Cluster::log`] gives it, holds `payload`.
fn holds(log: &[u8], payload: &[u8]) -> bool {
    log.windows(payload.len()).any(|bytes| bytes == payload)
}

/// Whether every member of `ids` has closed its election behind one leader.
fn settled(cluster: &Cluster, ids: &[i32]) -> bool {
    cluster.leader().is_some_and(|leader| {
        ids.iter().all(|&id| {
            let member = cluster.member(id).expect("a member");
            member.election_state() == ElectionState::Closed && member.leader_member_id() == leader
        })
    })
}

/// A cluster of three that elects, run until every member has closed its election
/// behind one leader; returns the leader's id.
fn elected(seed: u64, safety: &mut Safety) -> (Cluster, i32) {
    let ids = [0, 1, 2];
    let mut cluster = Cluster::with_election(&ids, seed).expect("three members");
    safety.run_until(&mut cluster, 3_000, "the first election", |cluster| {
        settled(cluster, &ids)
    });
    let leader = cluster.leader().expect("a leader");
    (cluster, leader)
}

/// Members that start together: each hears the other two, none ahead of it, so each is
/// a candidate at once and draws a delay; the first whose delay ends asks for the
/// votes, the others vote for it and follow, and it leads term 0. Which member that
/// is depends on the seed, and one seed gives the same leader at every run. None of
/// these seeds splits the vote; seed 12 does, in the test after this one.
#[test]
fn members_that_start_together_elect_one_leader() {
    let ids = [0, 1, 2];
    let mut leaders = Vec::new();
    for seed in 1..=11 {
        let mut safety = Safety::new(&ids);
        let (mut cluster, leader) = elected(seed, &mut safety);
        leaders.push(leader);
        for id in ids {
            let member = cluster.member(id).expect("a member");
            assert_eq!(member.leadership_term_id(), 0, "seed {seed}, member {id}");
            assert_eq!(
                member.role(),
                if id == leader {
                    Role::Leader
                } else {
                    Role::Follower
                },
                "seed {seed}, member {id}"
            );
            let counts = member.counts();
            assert_eq!(counts.terms_led, u64::from(id == leader), "seed {seed}");
            assert_eq!(counts.elections_failed, 0, "seed {seed}, member {id}");
            // The term a member stores is the one it voted in or proposed.
            let (terms, candidate_term) = cluster.record_of_terms(id).expect("a member");
            assert_eq!(terms, vec![(0, 0)], "seed {seed}, member {id}");
            assert_eq!(candidate_term, 0, "seed {seed}, member {id}");
        }
        // The leader was a candidate that won; each follower voted for it once.
        let counts = cluster.counts(leader).expect("the leader");
        assert_eq!(counts.candidacies, 1, "seed {seed}");
        assert_eq!(counts.ballots_lost, 0, "seed {seed}");
        let votes: u64 = ids
            .iter()
            .map(|&id| cluster.counts(id).expect("a member").votes_for)
            .sum();
        assert_eq!(votes, 2, "seed {seed}");

        // An entry offered to the leader is committed on all three.
        let entry = cluster.offer(b"elected").expect("the leader appends");
        safety.run(&mut cluster, 1_000);
        for id in ids {
            assert_eq!(cluster.commit_position(id), Some(entry), "seed {seed}");
            assert_eq!(cluster.log(id), cluster.log(leader), "seed {seed}");
        }
        // The same seed, the same leader.
        let (_, again) = elected(seed, &mut Safety::new(&ids));
        assert_eq!(again, leader, "seed {seed}");
    }
    leaders.sort_unstable();
    leaders.dedup();
    assert!(
        leaders.len() > 1,
        "eleven seeds gave one leader, {leaders:?}: the delay does not depend on the seed"
    );
}

/// A split vote. At this seed two members' delays end within a round trip of each
/// other: each stores term 0, votes for itself and asks for the others' votes, and
/// each answers the other against, having voted in that term. The third member votes
/// for the one it heard first. Neither ballot has every vote it waits for, and the one
/// with a quorum has a vote against, so at the election timeout both candidates, and
/// the member that waited for one of them, canvass again; they draw new delays, and
/// one leads term 1. The election takes one election timeout longer, and every member
/// counts the ballot it lost.
#[test]
fn a_split_vote_costs_one_election_timeout_and_the_next_ballot_elects() {
    let ids = [0, 1, 2];
    let mut safety = Safety::new(&ids);
    let mut cluster = Cluster::with_election(&ids, 12).expect("three members");
    let took = safety.run_until(&mut cluster, 3_000, "the first election", |cluster| {
        settled(cluster, &ids)
    });
    assert!(
        (1_000..2_000).contains(&took),
        "the election took {took} ms: one election timeout and a second ballot"
    );
    let leader = cluster.leader().expect("a leader");
    let mut candidates = 0;
    for id in ids {
        let member = cluster.member(id).expect("a member");
        assert_eq!(member.leadership_term_id(), 1, "member {id}");
        let counts = member.counts();
        assert_eq!(counts.ballots_lost, 1, "member {id}: {counts:?}");
        assert_eq!(counts.votes_against, 1, "member {id}: {counts:?}");
        assert_eq!(counts.elections_failed, 0, "member {id}: {counts:?}");
        // The two that split it were candidates of term 0; the winner was one again.
        candidates += u64::from(counts.candidacies > 0);
        let (terms, candidate_term) = cluster.record_of_terms(id).expect("a member");
        assert_eq!(
            terms,
            vec![(1, 0)],
            "member {id}: no member recorded term 0"
        );
        assert_eq!(candidate_term, 1, "member {id}");
    }
    assert_eq!(candidates, 2);
    assert_eq!(cluster.counts(leader).expect("the leader").candidacies, 2);
    let entry = cluster
        .offer(b"after the split")
        .expect("the leader appends");
    safety.run(&mut cluster, 1_000);
    for id in ids {
        assert_eq!(cluster.commit_position(id), Some(entry), "member {id}");
    }
}

/// An offer made while no member leads is refused, and the caller offers again.
#[test]
fn an_offer_with_no_leader_is_refused() {
    let ids = [0, 1, 2];
    let mut cluster = Cluster::with_election(&ids, 3).expect("three members");
    assert_eq!(cluster.leader(), None);
    assert_eq!(cluster.offer(b"too soon"), Err(Refused::NotLeader));
    let mut safety = Safety::new(&ids);
    safety.run_until(&mut cluster, 3_000, "the first election", |cluster| {
        settled(cluster, &ids)
    });
    cluster.offer(b"now").expect("the leader appends");
}

/// The leader stops with its followers level with it, every entry committed on all
/// three. Each follower hears nothing for the leader heartbeat timeout and canvasses;
/// the two hear each other, neither ahead, and each is a candidate at once, since the
/// canvass deadline passed with the timeout; the one whose delay ends first asks for
/// the other's vote and leads the next term as soon as it has it, the member that
/// stopped having said nothing (ADR 0077). So a member leads the next term within a
/// second of the first follower's timeout, at every seed here, and an entry offered
/// to it is committed by the two.
#[test]
fn a_leader_that_stops_is_replaced_within_a_second_of_the_heartbeat_timeout() {
    let ids = [0, 1, 2];
    let mut slowest = 0;
    for seed in 1..=20 {
        let mut safety = Safety::new(&ids);
        let (mut cluster, first) = elected(seed, &mut safety);
        // Term 0, or term 1 where the first vote was split, as at seed 12.
        let first_term = cluster
            .member(first)
            .expect("a member")
            .leadership_term_id();
        for payload in [&b"one"[..], b"two", b"three"] {
            cluster.offer(payload).expect("the leader appends");
        }
        safety.run(&mut cluster, 1_000);
        let committed = cluster.commit_position(first).expect("the leader");
        let others: Vec<i32> = ids.iter().copied().filter(|&id| id != first).collect();
        for &id in &others {
            assert_eq!(cluster.append_position(id), Some(committed), "seed {seed}");
        }
        let before = cluster.log(first);

        cluster.halt(first);
        let timeouts = |cluster: &Cluster| -> u64 {
            others
                .iter()
                .map(|&id| {
                    cluster
                        .counts(id)
                        .expect("a member")
                        .leader_heartbeat_timeouts
                })
                .sum()
        };
        safety.run_until(
            &mut cluster,
            11_000,
            "a follower's heartbeat timeout",
            |cluster| timeouts(cluster) > 0,
        );
        let took = safety.run_until(
            &mut cluster,
            3_000,
            "a leader of the next term",
            |cluster| cluster.leader().is_some_and(|leader| leader != first),
        );
        assert!(
            took < 1_000,
            "seed {seed}: a member led the next term {took} ms after the first heartbeat \
             timeout"
        );
        slowest = slowest.max(took);
        let second = cluster.leader().expect("a leader");
        assert_eq!(
            cluster
                .member(second)
                .expect("a member")
                .leadership_term_id(),
            first_term + 1,
            "seed {seed}"
        );

        // The two that remain close the election, and commit what the new leader
        // appends after it: the term's first entry and the offer.
        safety.run_until(&mut cluster, 3_000, "the second election", |cluster| {
            settled(cluster, &others)
        });
        let entry = cluster.offer(b"four").expect("the new leader appends");
        safety.run(&mut cluster, 1_000);
        for &id in &others {
            assert_eq!(cluster.commit_position(id), Some(entry), "seed {seed}");
            assert_eq!(cluster.log(id), cluster.log(second), "seed {seed}");
            let (terms, candidate_term) = cluster.record_of_terms(id).expect("a member");
            assert_eq!(
                terms,
                vec![(first_term, 0), (first_term + 1, committed)],
                "seed {seed}, member {id}"
            );
            assert_eq!(candidate_term, first_term + 1, "seed {seed}, member {id}");
        }
        // Nothing the first leader committed is lost, and the member that stopped holds
        // what it held.
        assert!(cluster.log(second).starts_with(&before), "seed {seed}");
        assert_eq!(cluster.log(first), before, "seed {seed}");
    }
    println!("the slowest of twenty elections: {slowest} ms after the first heartbeat timeout");
}

/// A follower whose log is behind is no candidate while the other follower reports,
/// and gets its vote; the member ahead leads, at a base the member behind is below.
/// The member behind replicates what it lacks of its own term from the new leader's
/// recordings (state 9): it asks once, counted on both members, and the replay
/// arrives. Its sync is paused, so its append position does not move, and the new
/// leader waits in leader log replication with no quorum that recorded its term.
///
/// When the member syncs, it is at its stop, the leader's term base, which is also
/// the commit position the first leader told it: it waits for nothing. It records
/// the leader's term there, reports, canvasses again, and joins the leader's log at
/// the base, level with it. A quorum has recorded the term, the leader appends the
/// term's first entry and both elections close; the two commit.
#[test]
fn a_follower_that_is_behind_is_no_candidate_replicates_what_it_lacks_and_joins() {
    let ids = [0, 1, 2];
    let mut safety = Safety::new(&ids);
    let (mut cluster, first) = elected(5, &mut safety);
    let others: Vec<i32> = ids.iter().copied().filter(|&id| id != first).collect();
    let (ahead, behind) = (others[0], others[1]);

    // The member behind stops syncing: the log reaches it, and its append position
    // stays where it is. The leader commits with the other follower.
    cluster.pause_sync(behind);
    let entry = cluster
        .offer(b"reaches a quorum")
        .expect("the leader appends");
    safety.run(&mut cluster, 1_000);
    assert_eq!(cluster.commit_position(first), Some(entry));
    assert_eq!(cluster.append_position(ahead), Some(entry));
    let held = cluster.append_position(behind).expect("a member");
    assert!(held < entry);
    assert_eq!(cluster.commit_position(behind), Some(entry));

    cluster.halt(first);
    safety.run_until(&mut cluster, 14_000, "the member ahead leads", |cluster| {
        cluster.leader() == Some(ahead)
    });
    safety.run(&mut cluster, 3_000);
    let counts = cluster.counts(behind).expect("a member");
    assert_eq!(counts.candidacies, 0, "{counts:?}");
    assert_eq!(counts.votes_for, 2, "one vote in each term: {counts:?}");
    assert_eq!(counts.log_replications, 1, "{counts:?}");
    assert_eq!(counts.elections_failed, 0, "{counts:?}");
    assert_eq!(
        cluster.election_state(behind),
        Some(ElectionState::FollowerLogReplication)
    );
    assert_eq!(cluster.append_position(behind), Some(held));
    assert_eq!(cluster.counts(ahead).expect("a member").log_replications, 1);
    assert_eq!(
        cluster.election_state(ahead),
        Some(ElectionState::LeaderLogReplication),
        "no quorum has recorded the new term"
    );
    assert_eq!(cluster.commit_position(ahead), Some(entry));

    // The member behind syncs what the replay brought, is at the term's base, and
    // joins.
    cluster.resume_sync(behind);
    safety.run_until(&mut cluster, 3_000, "the second election", |cluster| {
        settled(cluster, &others)
    });
    let (terms, _) = cluster.record_of_terms(behind).expect("a member");
    assert_eq!(terms, vec![(0, 0), (1, entry)]);
    let counts = cluster.counts(behind).expect("a member");
    assert_eq!(counts.log_replications, 1, "{counts:?}");
    assert_eq!(counts.log_replications_unavailable, 0, "{counts:?}");
    assert_eq!(counts.join_position_mismatches, 0, "{counts:?}");
    assert_eq!(counts.elections_failed, 0, "{counts:?}");
    let after = cluster
        .offer(b"committed by the two")
        .expect("the leader appends");
    safety.run(&mut cluster, 1_000);
    assert_eq!(cluster.commit_position(behind), Some(after));
    assert_eq!(cluster.log(behind), cluster.log(ahead));
}

/// A term that lies in two of the leader's recordings is replicated by two replays,
/// and the member asks for each (ADR 0092). A follower is cut off while the leader
/// commits two entries; the member ahead holds the first in one recording and the
/// second in another. When the leader stops, the member ahead leads, and the member
/// behind is told the new term, whose base it is below. Its first replay ends at the
/// end of the recording it began in, between the two entries: its image goes, it
/// subscribes again and asks from where its log then ends, and the second replay
/// brings the rest. Both catch-up positions and both replays are counted, no
/// replication is counted unavailable, no image joined elsewhere, and the member
/// holds the leader's log to the term's base byte for byte, where it records the
/// term.
#[test]
fn a_term_in_two_of_the_leaders_recordings_is_asked_for_twice() {
    let ids = [0, 1, 2];
    let mut safety = Safety::new(&ids);
    let (mut cluster, first) = elected(5, &mut safety);
    let others: Vec<i32> = ids.iter().copied().filter(|&id| id != first).collect();
    let (ahead, behind) = (others[0], others[1]);
    safety.run(&mut cluster, 10);
    let held = cluster.append_position(behind).expect("a member");

    cluster.cut(behind);
    let one = cluster
        .offer(b"in the first recording")
        .expect("the leader appends");
    safety.run(&mut cluster, 100);
    assert_eq!(cluster.append_position(ahead), Some(one));
    cluster.end_recording(ahead);
    let two = cluster
        .offer(b"in the second recording")
        .expect("the leader appends");
    safety.run(&mut cluster, 11_000);
    assert_eq!(cluster.commit_position(first), Some(two));
    assert_eq!(cluster.append_position(behind), Some(held));

    cluster.halt(first);
    cluster.heal(behind);
    safety.run_until(&mut cluster, 14_000, "the term replicated", |cluster| {
        cluster.append_position(behind) == Some(two)
    });
    safety.run(&mut cluster, 100);
    assert_eq!(cluster.leader(), Some(ahead));
    let counts = cluster.counts(behind).expect("a member");
    assert_eq!(counts.log_replications, 2, "{counts:?}");
    assert_eq!(counts.log_replications_unavailable, 0, "{counts:?}");
    assert_eq!(counts.join_position_mismatches, 0, "{counts:?}");
    assert_eq!(counts.elections_failed, 0, "{counts:?}");
    assert_eq!(cluster.counts(ahead).expect("a member").log_replications, 2);
    let (terms, _) = cluster.record_of_terms(behind).expect("a member");
    assert_eq!(terms, vec![(0, 0), (1, two)]);
    assert_eq!(cluster.log_to(behind, two), cluster.log_to(ahead, two));
}

/// A replication whose replay never arrives. A follower that was cut off while the
/// leader committed an entry comes back as that leader stops, votes for the member
/// ahead, and is told the new term, whose base it is below: it asks for what it
/// lacks. From then on nothing the new leader sends reaches it. The leader starts the
/// replay, counted, which finds no subscriber and ends after the replay connect
/// timeout. The follower's append position does not move for the leader heartbeat
/// timeout, so its election fails, counted in `cluster.log_replications_unavailable`,
/// and it canvasses. When the path is whole again it is told the term, asks again,
/// and the second replay brings it the leader's log to the term's base, byte for
/// byte, where it records the term.
#[test]
fn a_replication_whose_replay_never_arrives_fails_and_is_asked_again() {
    let ids = [0, 1, 2];
    let mut safety = Safety::new(&ids);
    let (mut cluster, first) = elected(5, &mut safety);
    let others: Vec<i32> = ids.iter().copied().filter(|&id| id != first).collect();
    let (ahead, behind) = (others[0], others[1]);
    safety.run(&mut cluster, 10);
    let held = cluster.append_position(behind).expect("a member");

    // Cut off for longer than the image liveness timeout: the follower loses its
    // image of the log, holds an election that nobody hears, and misses the entry.
    cluster.cut(behind);
    let entry = cluster
        .offer(b"reaches a quorum")
        .expect("the leader appends");
    safety.run(&mut cluster, 11_000);
    assert_eq!(cluster.commit_position(first), Some(entry));
    assert_eq!(cluster.append_position(behind), Some(held));
    cluster.halt(first);
    cluster.heal(behind);
    safety.run_until(&mut cluster, 14_000, "the member behind asks", |cluster| {
        cluster.election_state(behind) == Some(ElectionState::FollowerLogReplication)
    });
    assert_eq!(cluster.leader(), Some(ahead));
    cluster.cut_one_way(ahead, behind);

    safety.run(&mut cluster, 11_000);
    let counts = cluster.counts(behind).expect("a member");
    assert_eq!(counts.log_replications, 1, "{counts:?}");
    assert_eq!(counts.log_replications_unavailable, 1, "{counts:?}");
    assert_eq!(counts.join_position_mismatches, 0, "{counts:?}");
    assert_eq!(cluster.append_position(behind), Some(held));
    assert_eq!(cluster.counts(ahead).expect("a member").log_replications, 1);

    cluster.heal_between(ahead, behind);
    safety.run_until(&mut cluster, 3_000, "the term replicated", |cluster| {
        cluster.append_position(behind) == Some(entry)
    });
    safety.run(&mut cluster, 100);
    let counts = cluster.counts(behind).expect("a member");
    assert_eq!(counts.log_replications, 2, "{counts:?}");
    assert_eq!(counts.log_replications_unavailable, 1, "{counts:?}");
    assert_eq!(cluster.counts(ahead).expect("a member").log_replications, 2);
    let (terms, _) = cluster.record_of_terms(behind).expect("a member");
    assert_eq!(terms, vec![(0, 0), (1, entry)]);
    assert_eq!(cluster.log(behind), cluster.log_to(ahead, entry));
}

/// Card 267. A term that fewer than a quorum recorded does not stay in a member's
/// record under a leader whose record has none, and so strands nobody (ADR 0096).
///
/// Five members. W leads term 0 and commits to b; a tail of two entries more, to b2,
/// reaches Y alone, the first of them in one recording and the second in another. W
/// stops and Y is cut off. X wins term 1 at b, and its new leadership term reaches M
/// and not Z: X and M record term 1 at b, and X, with no quorum that recorded it,
/// appends nothing. Term 1 has no byte anywhere. X and M are cut off, and W or Y
/// leads a later term at b2 with Z's vote: in its log, term 0 runs on from b to b2,
/// and its record has no term 1.
///
/// M comes back and reports a log of term 1 that ends at b. Its leader answers for
/// term 0, the highest term its own record holds at or below 1: M drops term 1,
/// counted, cuts nothing, and replicates b to b2 as the rest of term 0. Its record is
/// then its leader's. Before ADR 0096 the leader answered "for term 1", M replicated
/// those bytes under term 1 and kept term 1 at b in its record, inside the leader's
/// term 0.
///
/// Z is parted from the leader one entry into the same span and is then told the
/// commit position b2. The leader and the other member stop. M, Z and X are three of
/// five, a quorum, and M, whose log is the longest, leads. It tells Z, whose log is
/// of term 0, that the leader's term of before is next, at b2, and Z replicates the
/// entry it lacks. Before ADR 0096 M told Z that term 1 was next, at b, below the
/// commit position Z knows: Z refused the truncation at every election, 295 times in
/// 30 s, M had X and no quorum, and three running members of five committed nothing.
#[test]
fn a_term_fewer_than_a_quorum_recorded_strands_no_member() {
    let ids = [0, 1, 2, 3, 4];
    let mut safety = Safety::new(&ids);
    let mut cluster = Cluster::with_election(&ids, 1).expect("five members");
    safety.run_until(&mut cluster, 6_000, "the first election", |cluster| {
        settled(cluster, &ids)
    });
    let w = cluster.leader().expect("a leader");
    safety.run(&mut cluster, 10);
    let frame = cluster.append_position(w).expect("the leader");
    cluster
        .offer(b"committed by all")
        .expect("the leader appends");
    safety.run(&mut cluster, 1_000);
    let b = cluster.commit_position(w).expect("the leader");
    let others: Vec<i32> = ids.iter().copied().filter(|&id| id != w).collect();
    let y = others[0];
    let three = &others[1..];

    // Two entries more reach Y alone: a tail of term 0 from b to b2, which W and Y
    // each hold in two recordings, so that a replay of it ends between the two.
    for &id in three {
        cluster.cut_between(w, id);
    }
    let b_mid = cluster.offer(&payload_of_frame(frame)).expect("appends");
    safety.run(&mut cluster, 50);
    assert_eq!(cluster.append_position(y), Some(b_mid));
    cluster.end_recording(w);
    cluster.end_recording(y);
    let b2 = cluster.offer(&payload_of_frame(frame)).expect("appends");
    safety.run(&mut cluster, 50);
    assert_eq!(cluster.append_position(y), Some(b2));
    assert_eq!(cluster.commit_position(w), Some(b));

    // W stops and Y is cut off: the three elect one of themselves for term 1 at b.
    cluster.halt(w);
    cluster.cut(y);
    safety.run_until(&mut cluster, 14_000, "the leader of term 1", |cluster| {
        cluster.leader().is_some_and(|id| three.contains(&id))
    });
    let x = cluster.leader().expect("a leader");
    let rest: Vec<i32> = three.iter().copied().filter(|&id| id != x).collect();
    let (m, z) = (rest[0], rest[1]);
    // Before its new leadership term has left, it is parted from one voter.
    cluster.cut_between(x, z);
    safety.run(&mut cluster, 2_000);
    assert_eq!(
        cluster.record_of_terms(m).expect("m").0,
        vec![(0, 0), (1, b)]
    );
    assert_eq!(
        cluster.record_of_terms(x).expect("x").0,
        vec![(0, 0), (1, b)]
    );
    assert_eq!(cluster.record_of_terms(z).expect("z").0, vec![(0, 0)]);
    assert_eq!(cluster.append_position(x), Some(b), "term 1 has no byte");

    // X and M are cut off; Y and W come back and one of them leads a later term
    // with Z.
    cluster.cut(x);
    cluster.cut(m);
    cluster.heal(y);
    cluster.resume(w);
    let leads_later = |cluster: &Cluster| {
        [w, y].into_iter().find(|&id| {
            let member = cluster.member(id).expect("a member");
            member.role() == Role::Leader && member.leadership_term_id() >= 2
        })
    };
    safety.run_until(
        &mut cluster,
        40_000,
        "the leader of a later term",
        |cluster| leads_later(cluster).is_some(),
    );
    let l2 = leads_later(&cluster).expect("a leader");
    let o = if l2 == w { y } else { w };
    let leaders_record = cluster.record_of_terms(l2).expect("l2").0;
    assert!(
        leaders_record.iter().all(|term| term.0 != 1),
        "its record has no term 1: {leaders_record:?}"
    );

    // Z replicates the span and is parted from the leader one entry in, where the
    // leader's first recording of the tail ends.
    safety.run_until(&mut cluster, 3_000, "Z is one entry in", |cluster| {
        cluster.append_position(z) == Some(b_mid)
    });
    cluster.cut_between(l2, z);

    // M comes back. It drops term 1, replicates b to b2 as the rest of term 0,
    // records the leader's term there and is counted: the leader's election closes
    // with a quorum, and it commits b2.
    cluster.heal(m);
    safety.run_until(
        &mut cluster,
        14_000,
        "the leader of the later term closes",
        |cluster| cluster.election_state(l2) == Some(ElectionState::Closed),
    );
    safety.run(&mut cluster, 100);
    // The leader's term began at b2 with its own first entry, which is committed
    // too.
    let committed = cluster.commit_position(l2).expect("l2");
    assert!(committed > b2, "{committed}");
    let counts = cluster.counts(m).expect("m");
    assert_eq!(counts.terms_dropped, 1, "{counts:?}");
    assert_eq!(counts.log_truncations, 0, "{counts:?}");
    assert_eq!(
        cluster.record_of_terms(m).expect("m").0,
        cluster.record_of_terms(l2).expect("l2").0,
        "M's record is its leader's: the term no quorum recorded is gone"
    );
    assert_eq!(cluster.append_position(m), Some(committed));
    assert_eq!(
        cluster.log(m),
        cluster.log(l2),
        "M's bytes are the leader's"
    );

    // Z is told the term again, with its commit position, while its recording does
    // not sync, and is parted again: it knows b2 is committed and holds one entry
    // of the two.
    cluster.pause_sync(z);
    cluster.heal_between(l2, z);
    safety.run_until(
        &mut cluster,
        3_000,
        "Z is told the commit position",
        |cluster| cluster.commit_position(z) == Some(committed),
    );
    cluster.cut_between(l2, z);
    safety.run(&mut cluster, 100);
    assert_eq!(cluster.append_position(z), Some(b_mid));

    // The leader and the other member stop. M, Z and X are a quorum, and M leads.
    cluster.halt(l2);
    cluster.halt(o);
    cluster.heal(x);
    safety.run_until(&mut cluster, 30_000, "M leads", |cluster| {
        cluster.leader() == Some(m)
    });
    // Z replicates from M what it lacks: it subscribes anew, which leaves what its
    // recording never synced, and its recording syncs again.
    safety.run_until(&mut cluster, 30_000, "Z replicates from M", |cluster| {
        cluster.election_state(z) == Some(ElectionState::FollowerLogReplication)
    });
    cluster.resume_sync(z);
    safety.run_until(&mut cluster, 30_000, "M's election closes", |cluster| {
        cluster.election_state(m) == Some(ElectionState::Closed)
    });
    safety.run(&mut cluster, 1_000);
    let counts = cluster.counts(z).expect("z");
    assert_eq!(counts.truncations_refused, 0, "{counts:?}");
    assert_eq!(counts.log_truncations, 0, "{counts:?}");
    assert_eq!(
        cluster.log_to(z, b2),
        cluster.log_to(m, b2),
        "Z holds the leader's log to b2"
    );
    let counts = cluster.counts(x).expect("x");
    assert_eq!(
        counts.terms_dropped, 1,
        "X drops the term it led: {counts:?}"
    );
    for id in [z, x] {
        assert_eq!(
            cluster.record_of_terms(id).expect("a member").0,
            cluster.record_of_terms(m).expect("m").0,
            "member {id}'s record is its leader's"
        );
    }
    let after = cluster
        .offer(b"committed by the three")
        .expect("the leader appends");
    safety.run(&mut cluster, 1_000);
    assert_eq!(cluster.commit_position(m), Some(after));
}

/// Late consensus messages cut nothing that was committed. Since ADR 0096 a member
/// that reads a new leadership term naming a term below its own log term may drop
/// terms, so an old message must change nothing: an answer to a report the member
/// made before it recorded more, or a copy from a leader it has since joined. Three
/// members, at eight seeds, every consensus message up to two seconds late and out
/// of order. Three times over, the leader appends an entry, is stopped, another
/// member leads, and the first runs again. The check after every step holds
/// throughout: no committed entry is lost or replaced, and no member holds, below
/// the commit position it knows, bytes that were not committed. No member refuses a
/// truncation, and once the messages arrive in order again the three settle on one
/// leader and one log. The rule runs under these messages: at five of the eight
/// seeds members drop terms, twelve drops in all, and three of them cut their log
/// first, and the check holds through each. `dropped_terms.rs` has the rule's own
/// cases.
#[test]
fn late_new_leadership_terms_cut_nothing_that_was_committed() {
    let (mut dropped, mut truncated) = (0, 0);
    for seed in 1..=8 {
        let ids = [0, 1, 2];
        let mut safety = Safety::new(&ids);
        let mut cluster = Cluster::with_election(&ids, seed).expect("three members");
        assert_eq!(cluster.set_delivery(0, 2_000), 2_000);
        for round in 0..3u8 {
            // An entry, once a leader's election has closed and takes it.
            let mut offered = None;
            for _ in 0..120_000 {
                if let Ok(position) = cluster.offer(&[b'a' + round; 40]) {
                    offered = Some(position);
                    break;
                }
                cluster.step();
                safety.check(&cluster);
            }
            assert!(
                offered.is_some(),
                "seed {seed}, round {round}: no leader took an entry"
            );
            safety.run(&mut cluster, 3_000);
            // Whoever leads by now: with messages this late the member that took
            // the entry may have held an election since.
            safety.run_until(&mut cluster, 120_000, "a member leads", |cluster| {
                cluster.leader().is_some()
            });
            let leader = cluster.leader().expect("a leader");
            cluster.halt(leader);
            safety.run_until(&mut cluster, 120_000, "another member leads", |cluster| {
                cluster.leader().is_some_and(|id| id != leader)
            });
            cluster.resume(leader);
            safety.run(&mut cluster, 15_000);
        }
        assert_eq!(cluster.set_delivery(0, 0), 0);
        safety.run_until(&mut cluster, 120_000, "the three settle", |cluster| {
            settled(cluster, &ids)
        });
        safety.run(&mut cluster, 1_000);
        let leader = cluster.leader().expect("a leader");
        let commit = cluster.commit_position(leader).expect("the leader");
        for id in ids {
            let counts = cluster.counts(id).expect("a member");
            assert_eq!(counts.truncations_refused, 0, "seed {seed}: {counts:?}");
            dropped += counts.terms_dropped;
            truncated += counts.log_truncations;
            assert_eq!(
                cluster.log_to(id, commit),
                cluster.log_to(leader, commit),
                "seed {seed}: member {id}'s log is the leader's"
            );
        }
    }
    // The rule ran under these messages, and members truncated under them: the check
    // above held through drops and truncations, not in their absence. The count of
    // truncations is every cut, one to a next term's base as well as one before a
    // drop; `dropped_terms.rs` holds the cut that a drop makes.
    println!(
        "election: late messages at eight seeds: {dropped} terms dropped, {truncated} logs \
         truncated"
    );
    assert!(dropped > 0, "no member dropped a term at any seed");
    assert!(truncated > 0, "no member truncated at any seed");
}

/// A payload whose session message takes a frame of `frame` bytes in the log.
fn payload_of_frame(frame: i64) -> Vec<u8> {
    use cavalry_proto::cluster::{Message, entry_position};
    (0..1_024usize)
        .map(|length| vec![7u8; length])
        .find(|payload| {
            let entry = Message::SessionMessage {
                leadership_term_id: 0,
                cluster_session_id: -1,
                timestamp: 0,
                payload,
            };
            entry_position(0, entry.encoded_length(), 1_408)
                .is_some_and(|after| after.position_after == frame)
        })
        .expect("a session message as long as a new leadership term event")
}

/// A member that missed a whole term and holds, in its place, an entry no quorum
/// recorded that is exactly as long as that term. In a cluster of five, the first
/// leader's last entry reaches one follower alone, and the leader stops. That follower
/// is cut off while the other three elect the second leader, whose term holds its
/// first entry and nothing more, as long as the entry the follower holds; then the
/// second leader stops and the cut ends. The follower votes for the third leader,
/// whose log is of a later term than its own, and the third leads at a base that is
/// the follower's append position, byte for byte.
///
/// The follower's log differs from the third leader's between the second term's base
/// and the third's, so it must not join there. The new leadership term it reads names
/// the second term, the next its leader's record holds after the follower's own, at
/// that term's base. The follower's append position is above that base, so it
/// truncates to it, counted in `cluster.log_truncations`. It is then below the third
/// leader's base, at the base of the second term, of which it holds nothing. It
/// records that term there and replicates it from the third leader's recordings
/// (state 9), to its end, which is the third leader's base: there it records the
/// third term and reports. Its log is then the third leader's to that base, byte for
/// byte, and the third leader, with two members stopped, has the quorum that
/// recorded its term: its election closes, where a leader that named its own term as
/// the next would have counted the follower over bytes of its own.
///
/// The follower is then behind the leader's log by the third term's first entry. It
/// catches up with it from the leader's live recording (states 11 to 13), joins the
/// live log, and the three commit.
#[test]
fn a_member_whose_tail_is_as_long_as_a_term_it_missed_replicates_that_term() {
    let ids = [0, 1, 2, 3, 4];
    let mut safety = Safety::new(&ids);
    let mut cluster = Cluster::with_election(&ids, 1).expect("five members");
    safety.run_until(&mut cluster, 6_000, "the first election", |cluster| {
        settled(cluster, &ids)
    });
    let first = cluster.leader().expect("a leader");
    let first_term = cluster
        .member(first)
        .expect("a member")
        .leadership_term_id();
    // The term's first entry is synced a step after the election closes: its frame is
    // what the log holds then.
    safety.run(&mut cluster, 10);
    let frame = cluster.append_position(first).expect("the leader");
    cluster
        .offer(b"committed by all")
        .expect("the leader appends");
    safety.run(&mut cluster, 1_000);
    let second_base = cluster.commit_position(first).expect("the leader");
    let others: Vec<i32> = ids.iter().copied().filter(|&id| id != first).collect();
    let (tailed, rest) = (others[0], &others[1..]);

    // One entry more, as long as a term's first entry, to one follower alone.
    for &id in rest {
        cluster.cut_between(first, id);
    }
    let tail_end = cluster
        .offer(&payload_of_frame(frame))
        .expect("the leader appends");
    assert_eq!(tail_end, second_base + frame);
    safety.run(&mut cluster, 100);
    assert_eq!(cluster.append_position(tailed), Some(tail_end));
    assert_eq!(cluster.commit_position(first), Some(second_base));

    // The leader stops, and the follower that holds the entry is cut off: the other
    // three elect the second leader without its vote, at the second term's base.
    cluster.halt(first);
    cluster.cut(tailed);
    safety.run_until(&mut cluster, 14_000, "the second leader", |cluster| {
        cluster
            .leader()
            .is_some_and(|leader| rest.contains(&leader))
    });
    safety.run_until(&mut cluster, 3_000, "the second election", |cluster| {
        settled(cluster, rest)
    });
    let second = cluster.leader().expect("a leader");
    safety.run(&mut cluster, 1_000);
    let third_base = cluster.append_position(second).expect("the leader");
    assert_eq!(
        third_base, tail_end,
        "the second term is as long as the entry"
    );
    let remaining: Vec<i32> = rest.iter().copied().filter(|&id| id != second).collect();
    for &id in &remaining {
        assert_eq!(cluster.append_position(id), Some(third_base));
    }
    assert_ne!(
        cluster.log_to(tailed, third_base),
        cluster.log_to(second, third_base),
        "the two hold different bytes below one position"
    );

    // The second leader stops and the cut ends: the two that remain need the
    // follower's vote, and it gives it, to a log of a later term than its own.
    cluster.halt(second);
    cluster.heal(tailed);
    safety.run_until(&mut cluster, 14_000, "the third leader", |cluster| {
        cluster
            .leader()
            .is_some_and(|leader| remaining.contains(&leader))
    });
    let third = cluster.leader().expect("a leader");
    assert_eq!(
        cluster
            .member(third)
            .expect("a member")
            .leadership_term_id(),
        first_term + 2
    );
    // The safety check runs after every step. A follower that joined at the third
    // term's base over its own bytes would be told a commit position above bytes
    // that were never committed, and the check would fail on it.
    safety.run_until(
        &mut cluster,
        3_000,
        "the third leader's election",
        |cluster| cluster.election_state(third) == Some(ElectionState::Closed),
    );
    safety.run(&mut cluster, 12_000);
    let member = cluster.member(tailed).expect("a member");
    let counts = member.counts();
    // It cut the entry no quorum recorded, once, began the second term at its base
    // and replicated it, once, and recorded the third term at the end of it.
    assert_eq!(counts.log_truncations, 1, "{counts:?}");
    assert_eq!(counts.log_replications, 1, "{counts:?}");
    assert_eq!(counts.log_replications_unavailable, 0, "{counts:?}");
    assert_eq!(counts.join_position_mismatches, 0, "{counts:?}");
    assert_eq!(cluster.counts(third).expect("a member").log_replications, 1);
    let (terms, _) = cluster.record_of_terms(tailed).expect("a member");
    assert_eq!(
        terms,
        vec![
            (first_term, 0),
            (first_term + 1, second_base),
            (first_term + 2, third_base)
        ]
    );
    // It was then behind by the third term's first entry, caught up with it from
    // the leader's live recording, and joined: three of five follow one log again.
    assert_eq!(counts.catchups, 1, "{counts:?}");
    assert_eq!(cluster.counts(third).expect("a member").catchups, 1);
    assert_eq!(member.election_state(), ElectionState::Closed);
    assert_eq!(member.leader_member_id(), third);
    assert_eq!(
        cluster.log(tailed),
        cluster.log(third),
        "what it holds is the leader's"
    );
    assert!(cluster.commit_position(third).expect("a member") > third_base);
    let last = cluster
        .offer(b"committed by the three")
        .expect("the leader appends");
    safety.run(&mut cluster, 1_000);
    assert_eq!(cluster.commit_position(tailed), Some(last));
    assert_eq!(cluster.log(tailed), cluster.log(third));
}

/// The leader is cut off from both followers, its duty cycle still running. It leads
/// for the leader heartbeat timeout more, appends what its process offers and commits
/// none of it, with no quorum that records it. Then its active members fall below a
/// quorum and it holds an election: a follower from that cycle on, its log publication
/// given up, canvassing with nobody to hear it. The followers hear nothing from it for
/// the same timeout and elect one of themselves, which commits with the other.
///
/// When the cut ends the old leader canvasses to the new one and is told its term. It
/// holds an entry the new leader's log does not, above the new term's base, and
/// truncates it, counted in `cluster.log_truncations`: its log is then the new
/// leader's up to that base, and it records the new term there. It is behind the new
/// leader's log from there, catches up with it from the leader's live recording, and
/// joins: the three commit. What it appended while cut off was in no other member's
/// log, was never committed, and is gone.
#[test]
fn a_leader_cut_off_from_its_followers_gives_up_the_lead_and_the_others_elect() {
    let ids = [0, 1, 2];
    for seed in [1, 2, 3, 12] {
        let mut safety = Safety::new(&ids);
        let (mut cluster, first) = elected(seed, &mut safety);
        let first_term = cluster
            .member(first)
            .expect("a member")
            .leadership_term_id();
        let others: Vec<i32> = ids.iter().copied().filter(|&id| id != first).collect();
        cluster
            .offer(b"committed by all")
            .expect("the leader appends");
        safety.run(&mut cluster, 1_000);
        let committed = cluster.commit_position(first).expect("the leader");
        let before = cluster.log(first);

        cluster.cut(first);
        safety.run(&mut cluster, 100);
        let orphan = cluster
            .offer(b"appended with no quorum")
            .expect("the cut leader still appends");
        assert!(orphan > committed, "seed {seed}");
        // It gives up the lead when its followers' last reports are the leader
        // heartbeat timeout old, and not before.
        let took = safety.run_until(
            &mut cluster,
            11_000,
            "the leader's quorum loss",
            |cluster| cluster.role(first) == Some(Role::Follower),
        );
        assert!(
            (9_000..=10_000).contains(&took),
            "seed {seed}: the cut leader led for {took} ms more"
        );
        let counts = cluster.counts(first).expect("a member");
        assert_eq!(counts.leader_quorum_losses, 1, "seed {seed}: {counts:?}");
        assert_eq!(counts.leader_step_downs, 0, "seed {seed}: {counts:?}");
        assert_eq!(
            cluster.commit_position(first),
            Some(committed),
            "seed {seed}"
        );
        assert_eq!(cluster.append_position(first), Some(orphan), "seed {seed}");

        // The other two elect, within a second of their heartbeat timeouts, and commit.
        let took = safety.run_until(
            &mut cluster,
            3_000,
            "a leader of the next term",
            |cluster| cluster.leader().is_some_and(|leader| leader != first),
        );
        assert!(
            took < 1_500,
            "seed {seed}: {took} ms after the old leader gave up"
        );
        safety.run_until(&mut cluster, 3_000, "the second election", |cluster| {
            settled(cluster, &others)
        });
        let second = cluster.leader().expect("a leader");
        let entry = cluster
            .offer(b"committed by two")
            .expect("the new leader appends");
        safety.run(&mut cluster, 1_000);
        for &id in &others {
            assert_eq!(cluster.commit_position(id), Some(entry), "seed {seed}");
            assert_eq!(cluster.log(id), cluster.log(second), "seed {seed}");
        }
        assert!(cluster.log(second).starts_with(&before), "seed {seed}");
        // What the old leader appended with no quorum is in its own log alone.
        assert!(
            holds(&cluster.log(first), b"appended with no quorum"),
            "seed {seed}"
        );
        for &id in &others {
            assert!(
                !holds(&cluster.log(id), b"appended with no quorum"),
                "seed {seed}, member {id}"
            );
        }

        // The cut ends. The old leader is told the new term, whose base is below its
        // append position: it truncates the entry no quorum recorded, once, and then
        // holds the log to the new term's base and records the term there. It is
        // behind the new leader's log by what the two appended, catches up with it
        // from the leader's live recording, and joins.
        cluster.heal(first);
        safety.run(&mut cluster, 2_000);
        let member = cluster.member(first).expect("a member");
        assert_eq!(member.role(), Role::Follower, "seed {seed}");
        assert_eq!(
            member.election_state(),
            ElectionState::Closed,
            "seed {seed}"
        );
        assert_eq!(member.leader_member_id(), second, "seed {seed}");
        assert_eq!(member.counts().terms_led, 1, "seed {seed}");
        assert_eq!(member.counts().log_truncations, 1, "seed {seed}");
        assert_eq!(member.counts().catchups, 1, "seed {seed}");
        assert_eq!(
            cluster.append_position(first),
            cluster.append_position(second),
            "seed {seed}"
        );
        assert!(cluster.log(first).starts_with(&before), "seed {seed}");
        assert_eq!(cluster.log(first), cluster.log(second), "seed {seed}");
        assert!(
            !holds(&cluster.log(first), b"appended with no quorum"),
            "seed {seed}"
        );
        let (terms, _) = cluster.record_of_terms(first).expect("a member");
        assert_eq!(
            terms,
            vec![(first_term, 0), (first_term + 1, committed)],
            "seed {seed}"
        );
        assert_eq!(cluster.leader(), Some(second), "seed {seed}");
        assert_eq!(
            cluster
                .member(second)
                .expect("a member")
                .leadership_term_id(),
            first_term + 1,
            "seed {seed}"
        );
        // The three go on committing.
        let last = cluster.offer(b"and again").expect("the new leader appends");
        safety.run(&mut cluster, 1_000);
        for &id in &ids {
            assert_eq!(cluster.commit_position(id), Some(last), "seed {seed}");
            assert_eq!(cluster.log(id), cluster.log(second), "seed {seed}");
        }
    }
}

/// The leader's process stands still, as one whose host stopped it does, for longer
/// than the leader heartbeat timeout. The followers elect one of themselves and
/// commit. When the old leader runs again it still reads leader of its term. On its
/// first duty cycles it gives up the lead, by whichever it meets first: its
/// followers' reports are older than the leader heartbeat timeout, so its active
/// members are below a quorum, or the new leader's commit position carries a term
/// above its own. It is a follower from that call on, and offers go to the new leader
/// alone.
///
/// It then canvasses to the new leader and is told the new term, whose base is its
/// own append position. It records the term there, and is behind the new leader's log
/// by the entries appended since: it catches up with them and joins, and the three
/// commit.
#[test]
fn a_leader_that_stood_still_gives_up_the_lead_when_it_runs_again() {
    let ids = [0, 1, 2];
    for seed in [1, 2, 3, 12] {
        let mut safety = Safety::new(&ids);
        let (mut cluster, first) = elected(seed, &mut safety);
        let first_term = cluster
            .member(first)
            .expect("a member")
            .leadership_term_id();
        let others: Vec<i32> = ids.iter().copied().filter(|&id| id != first).collect();
        cluster
            .offer(b"committed by all")
            .expect("the leader appends");
        safety.run(&mut cluster, 1_000);
        let committed = cluster.commit_position(first).expect("the leader");

        cluster.halt(first);
        safety.run_until(
            &mut cluster,
            14_000,
            "a leader of the next term",
            |cluster| cluster.leader().is_some_and(|leader| leader != first),
        );
        safety.run_until(&mut cluster, 3_000, "the second election", |cluster| {
            settled(cluster, &others)
        });
        let second = cluster.leader().expect("a leader");
        let entry = cluster
            .offer(b"committed by two")
            .expect("the new leader appends");
        safety.run(&mut cluster, 1_000);
        assert_eq!(cluster.commit_position(second), Some(entry), "seed {seed}");
        // Stopped, it still reads leader of the term it led.
        assert_eq!(cluster.role(first), Some(Role::Leader), "seed {seed}");

        cluster.resume(first);
        let took = safety.run_until(
            &mut cluster,
            1_000,
            "the old leader's step-down",
            |cluster| cluster.role(first) == Some(Role::Follower),
        );
        assert!(
            took <= 2,
            "seed {seed}: it led for {took} ms after it ran again"
        );
        let counts = cluster.counts(first).expect("a member");
        assert_eq!(
            counts.leader_quorum_losses + counts.leader_step_downs,
            1,
            "seed {seed}: {counts:?}"
        );
        assert_eq!(cluster.leader(), Some(second), "seed {seed}");

        safety.run(&mut cluster, 2_000);
        let member = cluster.member(first).expect("a member");
        assert_eq!(member.role(), Role::Follower, "seed {seed}");
        assert_eq!(member.counts().terms_led, 1, "seed {seed}");
        // It recorded the new term at its base, the position it held to, caught up
        // with what the two appended since, and joined.
        let (terms, _) = cluster.record_of_terms(first).expect("a member");
        assert_eq!(
            terms,
            vec![(first_term, 0), (first_term + 1, committed)],
            "seed {seed}"
        );
        assert_eq!(
            member.election_state(),
            ElectionState::Closed,
            "seed {seed}"
        );
        assert_eq!(member.leader_member_id(), second, "seed {seed}");
        assert_eq!(member.counts().catchups, 1, "seed {seed}");
        assert_eq!(
            cluster.append_position(first),
            cluster.append_position(second),
            "seed {seed}"
        );
        // The three go on committing.
        let last = cluster.offer(b"and again").expect("the new leader appends");
        safety.run(&mut cluster, 1_000);
        for &id in &ids {
            assert_eq!(cluster.commit_position(id), Some(last), "seed {seed}");
            assert_eq!(cluster.log(id), cluster.log(second), "seed {seed}");
        }
    }
}

/// A partition that cuts one way, in a cluster of five. What the leader sends stops
/// reaching three of its followers, and they still reach it; the fourth follower hears
/// everyone and is heard by everyone. The three hear nothing from the leader for the
/// leader heartbeat timeout, canvass, and are a quorum by themselves, so one of them
/// asks for votes in the next term. Until then the leader leads: every member still
/// reports to it, so it loses no quorum.
///
/// The candidate's request vote reaches the old leader and the fourth follower, each
/// with its election closed. Each holds an election on the higher term: the leader
/// steps down, counted in `cluster.leader_step_downs`, and the follower, whose leader
/// it heard to the last, counts `cluster.higher_terms_seen`. The follower then
/// canvasses to the new leader and joins it, level with the new term's base. The old
/// leader stays in canvass while it is not heard, and when the partition ends it is
/// told the new term, is behind the new leader's log, catches up with it and joins.
///
/// The UDP transport holds a partition that cuts one way for the connection timeout,
/// 5 s, at most ([`Cluster::cut_one_way`]). The in-memory cluster holds it for the ten
/// seconds this takes: the test is of what the two members do with the request vote,
/// whatever brings it.
#[test]
fn a_leader_that_three_of_five_cannot_hear_steps_down_on_their_request_vote() {
    let ids = [0, 1, 2, 3, 4];
    for seed in [1, 2, 3] {
        let mut safety = Safety::new(&ids);
        let mut cluster = Cluster::with_election(&ids, seed).expect("five members");
        safety.run_until(&mut cluster, 6_000, "the first election", |cluster| {
            settled(cluster, &ids)
        });
        let first = cluster.leader().expect("a leader");
        let first_term = cluster
            .member(first)
            .expect("a member")
            .leadership_term_id();
        cluster
            .offer(b"committed by all")
            .expect("the leader appends");
        safety.run(&mut cluster, 1_000);
        let committed = cluster.commit_position(first).expect("the leader");
        let others: Vec<i32> = ids.iter().copied().filter(|&id| id != first).collect();
        let (unhearing, hearing) = (&others[..3], others[3]);
        for &id in &others {
            assert_eq!(cluster.append_position(id), Some(committed), "seed {seed}");
        }

        for &id in unhearing {
            cluster.cut_one_way(first, id);
        }
        // For nine seconds nothing moves: the leader leads, and all four follow it.
        safety.run(&mut cluster, 9_000);
        assert_eq!(cluster.leader(), Some(first), "seed {seed}");
        for &id in &others {
            assert_eq!(
                cluster.election_state(id),
                Some(ElectionState::Closed),
                "seed {seed}, member {id}"
            );
        }

        let took = safety.run_until(&mut cluster, 3_000, "the leader's step-down", |cluster| {
            cluster.role(first) == Some(Role::Follower)
        });
        assert!(took < 2_000, "seed {seed}: {took} ms");
        let counts = cluster.counts(first).expect("a member");
        assert_eq!(counts.leader_step_downs, 1, "seed {seed}: {counts:?}");
        assert_eq!(counts.leader_quorum_losses, 0, "seed {seed}: {counts:?}");
        // The follower that heard the leader throughout held its election on the same
        // request vote, in the same step: no timeout ran, and it had not lost its image.
        let counts = cluster.counts(hearing).expect("a member");
        assert_eq!(counts.higher_terms_seen, 1, "seed {seed}: {counts:?}");
        assert_eq!(
            counts.leader_heartbeat_timeouts, 0,
            "seed {seed}: {counts:?}"
        );
        assert_eq!(counts.log_images_lost, 0, "seed {seed}: {counts:?}");
        for &id in unhearing {
            let counts = cluster.counts(id).expect("a member");
            assert_eq!(
                counts.leader_heartbeat_timeouts, 1,
                "seed {seed}: {counts:?}"
            );
            assert_eq!(counts.higher_terms_seen, 0, "seed {seed}: {counts:?}");
        }

        // One of the three leads the next term, and the four close behind it.
        safety.run_until(&mut cluster, 13_000, "the second election", |cluster| {
            settled(cluster, &others)
        });
        let second = cluster.leader().expect("a leader");
        assert!(
            unhearing.contains(&second),
            "seed {seed}: member {second} leads"
        );
        assert!(
            cluster
                .member(second)
                .expect("a member")
                .leadership_term_id()
                > first_term,
            "seed {seed}"
        );
        let entry = cluster
            .offer(b"committed by four")
            .expect("the new leader appends");
        safety.run(&mut cluster, 1_000);
        for &id in &others {
            assert_eq!(cluster.commit_position(id), Some(entry), "seed {seed}");
            assert_eq!(cluster.log(id), cluster.log(second), "seed {seed}");
        }
        // The old leader hears the new one and is not heard: it canvasses, a follower
        // with no leader.
        let member = cluster.member(first).expect("a member");
        assert_eq!(member.role(), Role::Follower, "seed {seed}");
        assert_eq!(
            member.election_state(),
            ElectionState::Canvass,
            "seed {seed}"
        );
        assert_eq!(member.leader_member_id(), -1, "seed {seed}");
        assert_eq!(member.counts().terms_led, 1, "seed {seed}");

        // The partition ends: it is told the term, is behind the new leader's log,
        // catches up with it and joins.
        for &id in unhearing {
            cluster.heal_between(first, id);
        }
        safety.run(&mut cluster, 2_000);
        let member = cluster.member(first).expect("a member");
        assert_eq!(member.role(), Role::Follower, "seed {seed}");
        assert_eq!(
            member.election_state(),
            ElectionState::Closed,
            "seed {seed}"
        );
        assert_eq!(member.leader_member_id(), second, "seed {seed}");
        assert_eq!(member.counts().catchups, 1, "seed {seed}");
        assert_eq!(cluster.leader(), Some(second), "seed {seed}");
        assert_eq!(cluster.log(first), cluster.log(second), "seed {seed}");
    }
}

/// The leader's consensus module stands still while its driver runs, for longer than
/// the leader heartbeat timeout. Its followers elect one of themselves and commit.
/// What they sent to the old leader in that time waited for it: their last append
/// positions, their canvass positions, the request vote of the candidate that now
/// leads. When its module runs again it reads them in order, before its duty cycle.
/// The append positions are read at the time it runs, so by them its followers
/// report, and it would lose no quorum; the request vote, of a term above its own,
/// is what makes it give up the lead, counted in `cluster.leader_step_downs`. It
/// reads nothing after that in init, and canvasses; told the new term, it catches up
/// with the new leader's log and joins.
#[test]
fn a_leader_whose_module_stood_still_steps_down_on_the_request_vote_that_waited() {
    let ids = [0, 1, 2];
    for seed in [1, 2, 3, 12] {
        let mut safety = Safety::new(&ids);
        let (mut cluster, first) = elected(seed, &mut safety);
        let first_term = cluster
            .member(first)
            .expect("a member")
            .leadership_term_id();
        let others: Vec<i32> = ids.iter().copied().filter(|&id| id != first).collect();
        cluster
            .offer(b"committed by all")
            .expect("the leader appends");
        safety.run(&mut cluster, 1_000);
        let committed = cluster.commit_position(first).expect("the leader");

        cluster.stall(first);
        assert_eq!(cluster.leader(), None, "seed {seed}");
        assert_eq!(
            cluster.offer(b"to a module that stands still"),
            Err(Refused::NotLeader),
            "seed {seed}"
        );
        safety.run_until(
            &mut cluster,
            14_000,
            "a leader of the next term",
            |cluster| cluster.leader().is_some(),
        );
        safety.run_until(&mut cluster, 3_000, "the second election", |cluster| {
            settled(cluster, &others)
        });
        let second = cluster.leader().expect("a leader");
        let entry = cluster
            .offer(b"committed by two")
            .expect("the new leader appends");
        safety.run(&mut cluster, 1_000);
        assert_eq!(cluster.commit_position(second), Some(entry), "seed {seed}");
        // It still reads leader of its term, and what was sent to it waits.
        assert_eq!(cluster.role(first), Some(Role::Leader), "seed {seed}");
        assert!(
            cluster.in_flight() > 100,
            "seed {seed}: {}",
            cluster.in_flight()
        );
        assert_eq!(cluster.full_queue_losses(), 0, "seed {seed}");

        cluster.unstall(first);
        safety.run(&mut cluster, 1);
        assert_eq!(cluster.role(first), Some(Role::Follower), "seed {seed}");
        let counts = cluster.counts(first).expect("a member");
        assert_eq!(counts.leader_step_downs, 1, "seed {seed}: {counts:?}");
        assert_eq!(counts.leader_quorum_losses, 0, "seed {seed}: {counts:?}");
        // Everything that waited was taken out, read or, after the step-down, not.
        assert!(
            cluster.in_flight() < 20,
            "seed {seed}: {}",
            cluster.in_flight()
        );
        assert_eq!(cluster.leader(), Some(second), "seed {seed}");

        // It is told the new term at the position it holds to, records it, catches
        // up with the new leader's log and joins.
        safety.run(&mut cluster, 2_000);
        let member = cluster.member(first).expect("a member");
        assert_eq!(member.role(), Role::Follower, "seed {seed}");
        assert_eq!(
            member.election_state(),
            ElectionState::Closed,
            "seed {seed}"
        );
        assert_eq!(member.leader_member_id(), second, "seed {seed}");
        assert_eq!(member.counts().catchups, 1, "seed {seed}");
        let (terms, _) = cluster.record_of_terms(first).expect("a member");
        assert_eq!(
            terms,
            vec![(first_term, 0), (first_term + 1, committed)],
            "seed {seed}"
        );
        let last = cluster.offer(b"and again").expect("the new leader appends");
        safety.run(&mut cluster, 1_000);
        for &id in &others {
            assert_eq!(cluster.commit_position(id), Some(last), "seed {seed}");
        }
    }
}

/// A catch-up is not stopped short of the live log. The leader has appended an entry
/// that its own recording has not synced: its publication is past its append
/// position. A member that was cut off comes back in that moment and catches up. The
/// leader's replay reads no further than the leader has synced, and the member's
/// live image joins at the publication's position, past it. The member reports the
/// position the replay brought it to, which is the leader's own append position,
/// without the catch-up flag, since it is not on the live log yet, and the leader
/// does not stop the replay. When the leader syncs, the replay brings the rest, the
/// member is on the live log, reports with the flag, is told to stop and joins: one
/// catch-up, and no election failed. A member that set the flag from its first
/// report was told to stop at the leader's append position, held a hole below its
/// live image that nothing filled, and failed its election after the leader
/// heartbeat timeout, which the first review of pull request 229 found.
#[test]
fn a_catch_up_is_not_stopped_while_the_leader_holds_what_it_has_not_synced() {
    let ids = [0, 1, 2];
    for seed in [1, 2, 3] {
        let mut safety = Safety::new(&ids);
        let (mut cluster, leader) = elected(seed, &mut safety);
        let others: Vec<i32> = ids.iter().copied().filter(|&id| id != leader).collect();
        let (level, behind) = (others[0], others[1]);

        // Cut off for longer than the leader heartbeat timeout, the member behind
        // holds an election and canvasses alone. The other two commit an entry.
        cluster.cut(behind);
        let first = cluster
            .offer(b"while it is away")
            .expect("the leader appends");
        safety.run(&mut cluster, 11_000);
        assert_eq!(cluster.commit_position(leader), Some(first), "seed {seed}");
        assert_eq!(cluster.leader(), Some(leader), "seed {seed}");
        let before = cluster.counts(behind).expect("a member");
        assert_eq!(before.catchups, 0, "seed {seed}: {before:?}");

        // The leader appends an entry and does not sync it.
        cluster.pause_sync(leader);
        let second = cluster
            .offer(b"appended, not synced")
            .expect("the leader appends");
        safety.run(&mut cluster, 50);
        assert_eq!(cluster.append_position(leader), Some(first), "seed {seed}");
        assert_eq!(cluster.append_position(level), Some(second), "seed {seed}");

        // The member behind comes back: told the term, it asks, the replay brings
        // it what the leader has synced, and its live image joins past that.
        cluster.heal(behind);
        safety.run(&mut cluster, 1_000);
        assert_eq!(
            cluster.election_state(behind),
            Some(ElectionState::FollowerCatchup),
            "seed {seed}"
        );
        assert_eq!(cluster.append_position(behind), Some(first), "seed {seed}");
        let counts = cluster.counts(behind).expect("a member");
        assert_eq!(counts.catchups, 1, "seed {seed}: {counts:?}");
        assert_eq!(
            counts.elections_failed, before.elections_failed,
            "seed {seed}: {counts:?}"
        );

        // The leader syncs: the replay brings the rest, and the member joins.
        cluster.resume_sync(leader);
        safety.run_until(&mut cluster, 1_000, "the member behind joins", |cluster| {
            settled(cluster, &ids)
        });
        let counts = cluster.counts(behind).expect("a member");
        assert_eq!(counts.catchups, 1, "seed {seed}: {counts:?}");
        assert_eq!(
            counts.elections_failed, before.elections_failed,
            "seed {seed}: {counts:?}"
        );
        assert_eq!(
            counts.join_position_mismatches, before.join_position_mismatches,
            "seed {seed}: {counts:?}"
        );
        assert_eq!(cluster.log(behind), cluster.log(leader), "seed {seed}");
        assert_eq!(cluster.leader(), Some(leader), "seed {seed}");
        let last = cluster.offer(b"by all three").expect("the leader appends");
        safety.run(&mut cluster, 500);
        for &id in &ids {
            assert_eq!(cluster.commit_position(id), Some(last), "seed {seed}");
        }
    }
}

/// A leader closes its election with a quorum and waits for no other member
/// (`spec/cluster.md`, row 8). Of three members that start together, one stands still
/// from the step a member leads: the leader heard it in canvass, and it has not joined
/// the leader's log. The leader closes with the other follower in under 100 ms, where
/// it waited the leader heartbeat timeout, 10 s, for a member it had heard, and the
/// two commit an entry. The member that stood still then runs again. The first
/// message it reads is the new leadership term that waited for it, which names the
/// leader's log position of that time, 0: by that word it is not behind, it joins the
/// live log, and the image joins past the end of its own log, counted in
/// `cluster.join_position_mismatches`, so its election fails once. It canvasses, the
/// leader's answer names where its log is now, and the member catches up within the
/// term and holds the entry. The leader leads the same term throughout.
#[test]
fn a_leader_closes_with_a_quorum_and_a_member_that_stood_still_catches_up() {
    let ids = [0, 1, 2];
    for seed in [1, 2, 3, 12] {
        let mut safety = Safety::new(&ids);
        let mut cluster = Cluster::with_election(&ids, seed).expect("three members");
        safety.run_until(&mut cluster, 3_000, "a leader", |cluster| {
            cluster.leader().is_some()
        });
        let leader = cluster.leader().expect("a leader");
        let term = cluster
            .member(leader)
            .expect("a member")
            .leadership_term_id();
        let still = ids
            .iter()
            .copied()
            .rev()
            .find(|&id| id != leader)
            .expect("a follower");
        let other = ids
            .iter()
            .copied()
            .find(|&id| id != leader && id != still)
            .expect("another follower");
        assert_ne!(
            cluster.election_state(still),
            Some(ElectionState::Closed),
            "seed {seed}"
        );
        cluster.stall(still);
        let stood_in = cluster.election_state(still);

        let steps = safety.run_until(
            &mut cluster,
            1_000,
            "the leader closes with a quorum",
            |cluster| cluster.election_state(leader) == Some(ElectionState::Closed),
        );
        assert!(steps < 100, "seed {seed}: closed after {steps} ms");
        let entry = cluster
            .offer(b"committed by two")
            .expect("the leader appends");
        safety.run(&mut cluster, 500);
        assert_eq!(cluster.commit_position(leader), Some(entry), "seed {seed}");
        assert_eq!(cluster.commit_position(other), Some(entry), "seed {seed}");
        assert_eq!(cluster.election_state(still), stood_in, "seed {seed}");
        assert_eq!(cluster.append_position(still), Some(0), "seed {seed}");

        cluster.unstall(still);
        safety.run_until(
            &mut cluster,
            3_000,
            "the member that stood still joins",
            |cluster| settled(cluster, &ids) && cluster.append_position(still) == Some(entry),
        );
        let member = cluster.member(still).expect("a member");
        assert_eq!(member.leader_member_id(), leader, "seed {seed}");
        assert_eq!(member.leadership_term_id(), term, "seed {seed}");
        let counts = member.counts();
        assert_eq!(
            counts.join_position_mismatches, 1,
            "seed {seed}: {counts:?}"
        );
        assert_eq!(counts.elections_failed, 1, "seed {seed}: {counts:?}");
        assert_eq!(counts.catchups, 1, "seed {seed}: {counts:?}");
        assert_eq!(cluster.log(still), cluster.log(leader), "seed {seed}");
        assert!(
            holds(&cluster.log(still), b"committed by two"),
            "seed {seed}"
        );
        let counts = cluster.counts(leader).expect("a member");
        assert_eq!(counts.terms_led, 1, "seed {seed}: {counts:?}");
        assert_eq!(counts.leader_step_downs, 0, "seed {seed}: {counts:?}");
        assert_eq!(counts.leader_quorum_losses, 0, "seed {seed}: {counts:?}");

        let last = cluster.offer(b"by all three").expect("the leader appends");
        safety.run(&mut cluster, 500);
        for &id in &ids {
            assert_eq!(cluster.commit_position(id), Some(last), "seed {seed}");
        }
    }
}

/// A follower starts again on the log directory it kept (ADR 0089): a new process
/// over its stored candidate term, its record of terms and what its recordings had
/// synced. It canvasses with the term its record holds, the leader answers with its
/// term, which is that one, and the member joins the leader's log again under the
/// same leader and term, with the record it had and the log the leader holds. The
/// leader leads on with the other follower meanwhile and commits an entry, which the
/// member that started again holds when it has joined.
#[test]
fn a_follower_that_starts_again_on_its_log_joins_its_leader_again() {
    let ids = [0, 1, 2];
    for seed in [1, 2, 3, 12] {
        let mut safety = Safety::new(&ids);
        let (mut cluster, leader) = elected(seed, &mut safety);
        let term = cluster
            .member(leader)
            .expect("a member")
            .leadership_term_id();
        let follower = ids
            .iter()
            .copied()
            .find(|&id| id != leader)
            .expect("a follower");
        let first = cluster
            .offer(b"before it starts again")
            .expect("the leader appends");
        safety.run(&mut cluster, 500);
        for &id in &ids {
            assert_eq!(cluster.commit_position(id), Some(first), "seed {seed}");
        }
        let record = cluster.record_of_terms(follower).expect("a member");

        safety.restart(&mut cluster, follower);
        let member = cluster.member(follower).expect("a member");
        assert_eq!(member.leadership_term_id(), -1, "seed {seed}");
        assert_eq!(member.leader_member_id(), -1, "seed {seed}");
        assert_eq!(member.commit_position(), 0, "seed {seed}");
        assert_eq!(member.election_state(), ElectionState::Init, "seed {seed}");
        let second = cluster
            .offer(b"while it starts")
            .expect("the leader appends");
        let steps = safety.run_until(
            &mut cluster,
            3_000,
            "the member that started again joins",
            |cluster| settled(cluster, &ids) && cluster.append_position(follower) == Some(second),
        );
        assert!(steps < 1_000, "seed {seed}: {steps} ms");
        assert_eq!(cluster.leader(), Some(leader), "seed {seed}");
        let member = cluster.member(follower).expect("a member");
        assert_eq!(member.leadership_term_id(), term, "seed {seed}");
        assert_eq!(
            cluster.record_of_terms(follower),
            Some(record),
            "seed {seed}"
        );
        assert_eq!(cluster.log(follower), cluster.log(leader), "seed {seed}");
        assert!(
            holds(&cluster.log(follower), b"before it starts again"),
            "seed {seed}"
        );
        let counts = member.counts();
        assert_eq!(counts.elections_closed, 1, "seed {seed}: {counts:?}");
        assert_eq!(counts.log_truncations, 0, "seed {seed}: {counts:?}");
        assert_eq!(counts.candidacies, 0, "seed {seed}: {counts:?}");
        let counts = cluster.counts(leader).expect("a member");
        assert_eq!(counts.terms_led, 1, "seed {seed}: {counts:?}");
        assert_eq!(counts.leader_step_downs, 0, "seed {seed}: {counts:?}");
        assert_eq!(counts.leader_quorum_losses, 0, "seed {seed}: {counts:?}");
        safety.run(&mut cluster, 500);
        assert_eq!(
            cluster.commit_position(follower),
            Some(second),
            "seed {seed}"
        );
    }
}

/// A leader starts again on the log directory it kept. Its log publication went with
/// its process, so its followers lose their images of the log and hold an election;
/// the member that started again canvasses with them, with the term it led in its
/// record and the candidate term it stored. One of the three, at two of these seeds
/// the member that started again, leads a later term within a second, and all three
/// end on its log, which holds every entry the first leader committed.
#[test]
fn a_leader_that_starts_again_on_its_log_ends_on_the_log_of_the_next_leader() {
    let ids = [0, 1, 2];
    for seed in [1, 2, 3, 12] {
        let mut safety = Safety::new(&ids);
        let (mut cluster, first) = elected(seed, &mut safety);
        let first_term = cluster
            .member(first)
            .expect("a member")
            .leadership_term_id();
        let committed = cluster
            .offer(b"committed by all")
            .expect("the leader appends");
        safety.run(&mut cluster, 500);
        for &id in &ids {
            assert_eq!(cluster.commit_position(id), Some(committed), "seed {seed}");
        }

        safety.restart(&mut cluster, first);
        assert_eq!(cluster.leader(), None, "seed {seed}");
        safety.run_until(&mut cluster, 1_000, "a leader of a later term", |cluster| {
            cluster.leader().is_some()
        });
        let second = cluster.leader().expect("a leader");
        let second_term = cluster
            .member(second)
            .expect("a member")
            .leadership_term_id();
        assert!(second_term > first_term, "seed {seed}");
        safety.run_until(&mut cluster, 3_000, "the second election", |cluster| {
            settled(cluster, &ids)
        });
        let last = cluster.offer(b"and again").expect("the new leader appends");
        safety.run(&mut cluster, 500);
        for &id in &ids {
            assert_eq!(cluster.commit_position(id), Some(last), "seed {seed}");
            assert_eq!(cluster.log(id), cluster.log(second), "seed {seed}");
            assert!(holds(&cluster.log(id), b"committed by all"), "seed {seed}");
        }
    }
}

/// Every member starts again at once, each on the log directory it kept, as a
/// cluster does when its hosts lose power together. Each canvasses with the term its
/// record holds and hears the other two, none ahead of it, so each is a candidate at
/// once, as at the cluster's first start, and one leads the next term in under two
/// seconds: no member waits the startup canvass timeout, 60 s. Every entry that was
/// committed is in every member's log, at its position, and the cluster commits
/// again. An entry that the leader had appended and no recording had synced is gone,
/// as a power loss takes it; it was never committed.
#[test]
fn every_member_starts_again_and_the_cluster_keeps_what_it_committed() {
    let ids = [0, 1, 2];
    for seed in [1, 2, 3, 12] {
        let mut safety = Safety::new(&ids);
        let (mut cluster, first) = elected(seed, &mut safety);
        let first_term = cluster
            .member(first)
            .expect("a member")
            .leadership_term_id();
        let committed = cluster
            .offer(b"committed by all")
            .expect("the leader appends");
        safety.run(&mut cluster, 500);
        let before = cluster.log_to(first, committed);
        cluster
            .offer(b"appended and not synced")
            .expect("the leader appends");
        for &id in &ids {
            safety.restart(&mut cluster, id);
        }
        assert_eq!(cluster.leader(), None, "seed {seed}");

        let steps = safety.run_until(
            &mut cluster,
            3_000,
            "a leader after every member started again",
            |cluster| settled(cluster, &ids),
        );
        assert!(steps < 2_000, "seed {seed}: {steps} ms");
        let second = cluster.leader().expect("a leader");
        let second_term = cluster
            .member(second)
            .expect("a member")
            .leadership_term_id();
        assert!(second_term > first_term, "seed {seed}");
        for &id in &ids {
            assert_eq!(cluster.log_to(id, committed), before, "seed {seed}");
            assert!(
                !holds(&cluster.log(id), b"appended and not synced"),
                "seed {seed}"
            );
        }
        let last = cluster.offer(b"and again").expect("the new leader appends");
        safety.run(&mut cluster, 500);
        for &id in &ids {
            assert_eq!(cluster.commit_position(id), Some(last), "seed {seed}");
            assert_eq!(cluster.log(id), cluster.log(second), "seed {seed}");
        }
    }
}
