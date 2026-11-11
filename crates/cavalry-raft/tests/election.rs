//! The election, three cores at a time in memory (`spec/cluster.md`, "Roles and the
//! election"; ADR 0077, ADR 0081, ADR 0082): members that start together elect one
//! leader, a leader that stops is replaced by a member whose log a quorum votes for, a
//! member votes once in a term, and a leader that is cut off or stood still gives up
//! the lead. Every test checks, at every step, that no committed entry is lost, what
//! one leader committed is in the log of every leader after it, and that no two
//! members lead one term.
//!
//! A member on this path has no truncation and no catch-up yet (the crate's README,
//! "What the election does not do yet"), so the tests stop a leader when its
//! followers are level with it, or say what the member that is not level does instead.

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
/// and gets its vote; the member ahead leads. The member behind cannot join the new
/// term at its base without the replication of the span it lacks, state 9, which this
/// step does not have: it fails its election and canvasses, counted, and the new
/// leader waits in leader log replication with no quorum that recorded its term. When
/// the member behind has synced what had reached it, it is level, and joins.
#[test]
fn a_follower_that_is_behind_is_no_candidate_and_joins_once_it_is_level() {
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
    assert!(cluster.append_position(behind).expect("a member") < entry);

    cluster.halt(first);
    safety.run_until(&mut cluster, 14_000, "the member ahead leads", |cluster| {
        cluster.leader() == Some(ahead)
    });
    safety.run(&mut cluster, 3_000);
    let counts = cluster.counts(behind).expect("a member");
    assert_eq!(counts.candidacies, 0, "{counts:?}");
    assert_eq!(counts.votes_for, 2, "one vote in each term: {counts:?}");
    assert!(counts.elections_failed > 0, "{counts:?}");
    assert_ne!(cluster.election_state(behind), Some(ElectionState::Closed));
    assert_eq!(
        cluster.election_state(ahead),
        Some(ElectionState::LeaderLogReplication),
        "no quorum has recorded the new term"
    );
    assert_eq!(cluster.commit_position(ahead), Some(entry));

    // The member behind syncs what it held, is level with the term's base, and joins.
    cluster.resume_sync(behind);
    safety.run_until(&mut cluster, 3_000, "the second election", |cluster| {
        settled(cluster, &others)
    });
    let after = cluster
        .offer(b"committed by the two")
        .expect("the leader appends");
    safety.run(&mut cluster, 1_000);
    assert_eq!(cluster.commit_position(behind), Some(after));
    assert_eq!(cluster.log(behind), cluster.log(ahead));
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
/// that term's base. A member whose append position is above that base truncates to
/// it, which this path does not do yet: its election fails, and it records no term at
/// a base its log does not reach by the leader's bytes. With two members stopped and
/// this one not joined, the third leader has no quorum that recorded its term, and
/// commits nothing: the cluster waits, where a leader that named its own term as the
/// next would have counted the follower and committed over its bytes.
#[test]
fn a_member_whose_tail_is_as_long_as_a_term_it_missed_does_not_join_on_it() {
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
    // term's base would be told a commit position above bytes that were never
    // committed, and the check would fail on it.
    safety.run(&mut cluster, 12_000);
    let member = cluster.member(tailed).expect("a member");
    assert_ne!(member.election_state(), ElectionState::Closed);
    assert!(member.counts().elections_failed > 0);
    let (terms, _) = cluster.record_of_terms(tailed).expect("a member");
    assert_eq!(
        terms,
        vec![(first_term, 0)],
        "it records no term at a base it does not reach by the leader's bytes"
    );
    // No quorum has recorded the third term, so nothing of it is committed.
    assert_eq!(
        cluster.election_state(third),
        Some(ElectionState::LeaderLogReplication)
    );
    assert_eq!(cluster.commit_position(third), Some(third_base));
}

/// The leader is cut off from both followers, its duty cycle still running. It leads
/// for the leader heartbeat timeout more, appends what its process offers and commits
/// none of it, with no quorum that records it. Then its active members fall below a
/// quorum and it holds an election: a follower from that cycle on, its log publication
/// given up, canvassing with nobody to hear it. The followers hear nothing from it for
/// the same timeout and elect one of themselves, which commits with the other.
///
/// When the cut ends the old leader canvasses to the new one and is told its term. It
/// holds an entry the new leader's log does not, above the new term's base, and joins
/// only by truncating it, which this step does not have: its elections fail, counted,
/// and it stays a follower that commits nothing. What it appended while cut off is in
/// no other member's log and is never committed.
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

        // The cut ends. The old leader is told the new term and cannot join it: it
        // holds the entry no quorum recorded, above the term's base.
        cluster.heal(first);
        let failed = cluster.counts(first).expect("a member").elections_failed;
        safety.run(&mut cluster, 2_000);
        let member = cluster.member(first).expect("a member");
        assert_eq!(member.role(), Role::Follower, "seed {seed}");
        assert_ne!(
            member.election_state(),
            ElectionState::Closed,
            "seed {seed}"
        );
        assert!(member.counts().elections_failed > failed, "seed {seed}");
        assert_eq!(member.counts().terms_led, 1, "seed {seed}");
        assert_eq!(
            cluster.commit_position(first),
            Some(committed),
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
        // The two go on committing while it canvasses.
        let last = cluster.offer(b"and again").expect("the new leader appends");
        safety.run(&mut cluster, 1_000);
        for &id in &others {
            assert_eq!(cluster.commit_position(id), Some(last), "seed {seed}");
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
/// by the entries appended since: it joins only by catching up, which this step does
/// not have, so its elections fail, counted, until that step.
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
        assert!(member.counts().elections_failed > 0, "seed {seed}");
        // It recorded the new term at its base, the position it holds to.
        let (terms, _) = cluster.record_of_terms(first).expect("a member");
        assert_eq!(
            terms,
            vec![(first_term, 0), (first_term + 1, committed)],
            "seed {seed}"
        );
        assert_eq!(
            cluster.append_position(first),
            Some(committed),
            "seed {seed}"
        );
        // The two go on committing.
        let last = cluster.offer(b"and again").expect("the new leader appends");
        safety.run(&mut cluster, 1_000);
        for &id in &others {
            assert_eq!(cluster.commit_position(id), Some(last), "seed {seed}");
            assert_eq!(cluster.log(id), cluster.log(second), "seed {seed}");
        }
    }
}
