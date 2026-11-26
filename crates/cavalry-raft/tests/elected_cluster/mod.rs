//! What the tests of an elected cluster in memory share: the check that holds after
//! every step whoever leads, and a cluster of three run until it has one leader
//! (`spec/cluster.md`, "Roles and the election"; ADR 0077, ADR 0100).

#![allow(dead_code)]

use std::collections::HashMap;

use cavalry_proto::cluster::quorum;
use cavalry_raft::memory::Cluster;
use cavalry_raft::{ElectionState, Role};

/// What must hold after every step (invariant 1), whoever leads. The bytes a leader
/// has committed are kept, and every later commit of any leader extends them: a
/// committed entry is never lost and never replaced. A member's own log up to the
/// commit position it knows is part of them. The leader's commit position is at or
/// below the append positions of a quorum, a quorum of the members the leader's own
/// members string gives a vote (ADR 0134), no member's commit position falls, and no
/// two members lead one term, running or stopped.
///
/// Across the whole run, and across a wipe, which a member's new run forgets (work item
/// 476): one term is led by one run of one member, never by two members nor by two
/// runs of one; and a member id that voted in a term for one run votes in it for
/// another only once the first can no longer lead it, being wiped or started again, or
/// out of its ballot for the term without leading it. The votes are those
/// [`Cluster::votes_cast`] records as they are written.
pub struct Safety {
    ids: Vec<i32>,
    committed: Vec<u8>,
    last_commit: Vec<i64>,
    /// For each member, what it had appended when it was last wiped: an append
    /// position it sent before it lost its log can still reach the leader and count
    /// toward a commit, as a datagram sent before a host's loss can, so the quorum
    /// below counts the member at that position until it holds as much again. The
    /// vote floor is what keeps that commit (ADR 0145).
    appended_before_wipe: Vec<i64>,
    /// The run that led each term: its member and its run number.
    leaders: HashMap<i64, (i32, i64)>,
    /// The run each member id last voted for in each term, and that run's number.
    votes: HashMap<(i32, i64), (i32, i64)>,
    /// How many of [`Cluster::votes_cast`] this check has read.
    votes_read: usize,
}

impl Safety {
    pub fn new(ids: &[i32]) -> Self {
        Self {
            ids: ids.to_vec(),
            committed: Vec::new(),
            last_commit: vec![0; ids.len()],
            appended_before_wipe: vec![0; ids.len()],
            leaders: HashMap::new(),
            votes: HashMap::new(),
            votes_read: 0,
        }
    }

    /// Forgets the terms led and the votes cast, as a cluster whose every member lost
    /// its log directory forgets them: a test that wipes them all and means to show
    /// what the check of the committed log says next calls it.
    pub fn forget_terms(&mut self) {
        self.leaders.clear();
        self.votes.clear();
    }

    /// One term, one run of one member to lead it; a second vote of a member id in a
    /// term only once the run it first voted for can no longer lead the term.
    fn check_terms(&mut self, cluster: &Cluster) {
        for &id in &self.ids {
            let Some(member) = cluster.member(id) else {
                continue;
            };
            if member.role() != Role::Leader {
                continue;
            }
            let term = member.leadership_term_id();
            let run = cluster.run(id).expect("a member");
            let first = *self.leaders.entry(term).or_insert((id, run));
            assert_eq!(
                first,
                (id, run),
                "term {term} led by member {} in its run {} and by member {id} in its run \
                 {run}",
                first.0,
                first.1
            );
        }
        let cast = cluster.votes_cast();
        for vote in cast.get(self.votes_read..).unwrap_or(&[]) {
            let key = (vote.voter, vote.term);
            let now = (vote.candidate, vote.run);
            let Some(&(candidate, run)) = self.votes.get(&key) else {
                self.votes.insert(key, now);
                continue;
            };
            if (candidate, run) == now {
                continue;
            }
            let can_lead = cluster.run(candidate) == Some(run)
                && cluster
                    .member(candidate)
                    .is_some_and(|first| match first.role() {
                        Role::Leader => first.leadership_term_id() == vote.term,
                        Role::Candidate => {
                            cluster.record_of_terms(candidate).map(|(_, term)| term)
                                == Some(vote.term)
                        }
                        Role::Follower => false,
                    });
            assert!(
                !can_lead,
                "member {} voted in term {} for member {candidate}'s run {run}, which can \
                 still lead it, and then for member {}'s run {}",
                vote.voter, vote.term, vote.candidate, vote.run
            );
            self.votes.insert(key, now);
        }
        self.votes_read = cast.len();
        assert_eq!(cluster.votes_unrecorded(), 0, "votes went unrecorded");
    }

    pub fn check(&mut self, cluster: &Cluster) {
        self.check_terms(cluster);
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
            // The voters as the leader counts them: one bit of its `voters` for each
            // member in the rising order of the ids.
            let mask = cluster.member(leader).expect("the leader").voters();
            let mut rising = self.ids.clone();
            rising.sort_unstable();
            let mut appended: Vec<i64> = rising
                .iter()
                .enumerate()
                .filter(|(place, _)| mask >> place & 1 == 1)
                .map(|(_, &id)| {
                    let appended = cluster.append_position(id).expect("a member");
                    self.ids
                        .iter()
                        .position(|&known| known == id)
                        .and_then(|index| self.appended_before_wipe.get(index))
                        .map_or(appended, |&before| appended.max(before))
                })
                .collect();
            // A member that holds as much again as it had appended before its wipe is
            // counted at its own position from then on.
            for (index, &id) in self.ids.iter().enumerate() {
                if cluster.append_position(id) >= self.appended_before_wipe.get(index).copied() {
                    self.appended_before_wipe[index] = 0;
                }
            }
            appended.sort_unstable_by(|a, b| b.cmp(a));
            let durable_on_a_quorum = appended[quorum(appended.len()) - 1];
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

    pub fn run(&mut self, cluster: &mut Cluster, milliseconds: u64) {
        for _ in 0..milliseconds {
            cluster.step();
            self.check(cluster);
        }
    }

    /// Starts `member` again on the log directory it kept ([`Cluster::restart`]). A
    /// member that starts knows no commit position until a leader tells it one, so
    /// the last it knew is forgotten here; what the cluster committed is not, and
    /// every check of it goes on.
    pub fn restart(&mut self, cluster: &mut Cluster, member: i32) {
        cluster.restart(member);
        if let Some(index) = self.ids.iter().position(|&id| id == member) {
            self.last_commit[index] = 0;
        }
    }

    /// Starts `member` again on the log directory it kept, with a members string that
    /// marks the members of `standby_ids` its standbys ([`Cluster::restart_with`]):
    /// one member's step of a promotion. The commit position it knew is forgotten,
    /// as in [`Safety::restart`].
    pub fn restart_with(&mut self, cluster: &mut Cluster, member: i32, standby_ids: &[i32]) {
        cluster
            .restart_with(member, standby_ids)
            .expect("the marks leave a voter");
        if let Some(index) = self.ids.iter().position(|&id| id == member) {
            self.last_commit[index] = 0;
        }
    }

    /// Starts `member` again on an empty log directory ([`Cluster::wipe`]). It knows
    /// no commit position, so the last it knew is forgotten here; what the cluster
    /// committed is not, so a quorum that loses its directories and commits another
    /// history fails the check.
    pub fn wipe(&mut self, cluster: &mut Cluster, member: i32) {
        let appended = cluster.append_position(member).unwrap_or(0);
        cluster.wipe(member);
        if let Some(index) = self.ids.iter().position(|&id| id == member) {
            self.last_commit[index] = 0;
            // A second wipe before the member holds as much again keeps the larger: an
            // append position sent before the first can still be on its way.
            self.appended_before_wipe[index] = self.appended_before_wipe[index].max(appended);
        }
    }

    /// Steps until `done`, for at most `limit_ms`, and returns how many steps it took.
    pub fn run_until(
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
pub fn holds(log: &[u8], payload: &[u8]) -> bool {
    log.windows(payload.len()).any(|bytes| bytes == payload)
}

/// Whether every member of `ids` has closed its election behind one leader.
pub fn settled(cluster: &Cluster, ids: &[i32]) -> bool {
    cluster.leader().is_some_and(|leader| {
        ids.iter().all(|&id| {
            let member = cluster.member(id).expect("a member");
            member.election_state() == ElectionState::Closed && member.leader_member_id() == leader
        })
    })
}

/// A cluster of three that elects, run until every member has closed its election
/// behind one leader; returns the leader's id.
pub fn elected(seed: u64, safety: &mut Safety) -> (Cluster, i32) {
    let ids = [0, 1, 2];
    let mut cluster = Cluster::with_election(&ids, seed).expect("three members");
    safety.run_until(&mut cluster, 3_000, "the first election", |cluster| {
        settled(cluster, &ids)
    });
    let leader = cluster.leader().expect("a leader");
    (cluster, leader)
}
