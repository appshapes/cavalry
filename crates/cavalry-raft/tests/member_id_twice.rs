//! A member id that two hosts run, in memory (`spec/cluster.md`, "Members and
//! streams"; work item 470). Two hosts can both bind a member's endpoints only when
//! each holds the address its endpoints resolve to: a name that each resolves to
//! itself, or an address that moves from one to the other (`tests/member_id_hosts.rs`
//! holds the refusals). [`Cluster::start_twin`] starts the second run, and
//! [`Cluster::move_address`] moves the address, as such an address moves.
//!
//! What its peers do with two runs, held here. A peer sends to the member id at the
//! one address its endpoints resolve to, so only the run there takes a leader,
//! follows, acknowledges and receives a vote. The other run hears nothing, so it
//! canvasses and never stands; what it sends is still read, as the member id's. And a
//! leader counts the acknowledgements of whichever run the address reaches as the
//! member id's, so an entry can be committed on one run's log that the other run's
//! lacks. What a cluster does when the address moves back, to a run whose log ends
//! below such an entry, is work item 470's rule.

mod elected_cluster;

use cavalry_raft::ElectionState;
use cavalry_raft::memory::Cluster;
use elected_cluster::{Safety, elected, holds};

/// Steps until `done`, for at most `limit_ms`, with no check of the cluster's safety:
/// the check knows each member id by one run.
fn run_until(cluster: &mut Cluster, limit_ms: u64, what: &str, done: impl Fn(&Cluster) -> bool) {
    for _ in 0..limit_ms {
        if done(cluster) {
            return;
        }
        cluster.step();
    }
    assert!(done(cluster), "{what}: not within {limit_ms} ms");
}

/// A cluster of three that elected and holds ten entries, its leader, and a member that
/// follows it.
fn cluster_with_entries(seed: u64) -> (Cluster, i32, i32) {
    let ids = [0, 1, 2];
    let mut safety = Safety::new(&ids);
    let (mut cluster, leader) = elected(seed, &mut safety);
    for entry in 0..10u32 {
        cluster.offer(&entry.to_le_bytes()).expect("the leader takes an entry");
        safety.run(&mut cluster, 1);
    }
    safety.run(&mut cluster, 1_000);
    let member = ids.into_iter().find(|&id| id != leader).expect("a follower");
    (cluster, leader, member)
}

/// A second run of a follower's member id, on a host that does not hold the address,
/// hears nothing. It canvasses and never stands, since a member stands only once it
/// has heard its peers: it takes no leader, raises no term and receives no entry. The
/// cluster keeps its leader and its term, and the first run, whose address it is,
/// follows on.
#[test]
fn a_run_without_its_member_ids_address_hears_nothing_and_canvasses_on() {
    let (mut cluster, leader, member) = cluster_with_entries(7);
    let term = cluster.member(leader).expect("a member").leadership_term_id();
    let twin = cluster.start_twin(member).expect("a second run");
    cluster.run_ms(120_000);
    let second = cluster.member(twin).expect("the second run");
    assert_eq!(second.election_state(), ElectionState::Canvass);
    assert_eq!(second.leadership_term_id(), -1);
    assert_eq!(cluster.append_position(twin), Some(0), "it received nothing");
    assert_eq!(cluster.counts(twin).expect("the second run").candidacies, 0);
    assert_eq!(cluster.leader(), Some(leader));
    let first = cluster.member(member).expect("a member");
    assert_eq!(first.election_state(), ElectionState::Closed);
    assert_eq!(first.leadership_term_id(), term);
}

/// Once the address is the second run's, the second run is the member id to its
/// peers: it takes the leader, with a floor since its log is empty (ADR 0145), catches
/// up, and its acknowledgements count toward a commit; and the first run, which no
/// longer holds the address, hears nothing, while its log stays where it was. So an
/// entry is committed on the second run's log that the first run's lacks.
#[test]
fn a_leader_counts_the_acknowledgements_of_the_run_the_address_reaches() {
    let (mut cluster, leader, member) = cluster_with_entries(11);
    let held = cluster.append_position(member).expect("a member");
    let twin = cluster.start_twin(member).expect("a second run");
    cluster.move_address(twin);
    run_until(
        &mut cluster,
        30_000,
        "the second run follows and clears its floor",
        |cluster| {
            let second = cluster.member(twin).expect("the second run");
            cluster.append_position(twin) >= Some(held)
                && second.vote_floor() == -1
                && second.election_state() == ElectionState::Closed
        },
    );
    assert!(
        cluster.counts(twin).expect("the second run").vote_floors_cleared >= 1,
        "it took a floor and cleared it"
    );

    // The third member stops hearing the leader: an entry committed now is committed
    // by the leader and the member id, whose acknowledgement is the second run's.
    let leader = cluster.leader().expect("a leader");
    assert_ne!(leader, twin);
    let third = [0, 1, 2]
        .into_iter()
        .find(|&id| id != leader && id != member)
        .expect("a third member");
    cluster.cut_between(leader, third);
    let entry = b"an entry the first run never holds";
    let end = cluster.offer(entry).expect("the leader takes the entry");
    run_until(&mut cluster, 10_000, "the entry is committed", |cluster| {
        cluster
            .commit_position(leader)
            .is_some_and(|commit| commit >= end)
    });
    assert!(holds(&cluster.log(twin), entry));
    assert!(!holds(&cluster.log(member), entry));
    assert!(!holds(&cluster.log(third), entry));
    assert_eq!(cluster.append_position(member), Some(held));
}
