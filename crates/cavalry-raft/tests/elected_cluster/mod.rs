//! What the tests of an elected cluster in memory share: the check that holds after
//! every step whoever leads, and a cluster of three run until it has one leader
//! (`spec/cluster.md`, "Roles and the election"; ADR 0077, ADR 0100).

#![allow(dead_code)]

use cavalry_proto::cluster::quorum;
use cavalry_raft::memory::Cluster;
use cavalry_raft::{ElectionState, Role};

/// What must hold after every step (invariant 1), whoever leads. The bytes a leader
/// has committed are kept, and every later commit of any leader extends them: a
/// committed entry is never lost and never replaced. A member's own log up to the
/// commit position it knows is part of them. The leader's commit position is at or
/// below the append positions of a quorum, no member's commit position falls, and no
/// two members lead one term, running or stopped.
pub struct Safety {
    ids: Vec<i32>,
    committed: Vec<u8>,
    last_commit: Vec<i64>,
}

impl Safety {
    pub fn new(ids: &[i32]) -> Self {
        Self {
            ids: ids.to_vec(),
            committed: Vec::new(),
            last_commit: vec![0; ids.len()],
        }
    }

    pub fn check(&mut self, cluster: &Cluster) {
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

    /// Starts `member` again on an empty log directory ([`Cluster::wipe`]). It knows
    /// no commit position, so the last it knew is forgotten here; what the cluster
    /// committed is not, so a quorum that loses its directories and commits another
    /// history fails the check.
    pub fn wipe(&mut self, cluster: &mut Cluster, member: i32) {
        cluster.wipe(member);
        if let Some(index) = self.ids.iter().position(|&id| id == member) {
            self.last_commit[index] = 0;
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
