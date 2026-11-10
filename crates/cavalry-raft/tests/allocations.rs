//! The core allocates nothing: not in a duty cycle, not for a message it reads, not for
//! an entry it appends (`CLAUDE.md`, invariant 2). A test of its own, since the counting
//! allocator is the whole process's.

mod common;

use std::alloc::System;

use cavalry_proto::cluster::Message;
use cavalry_raft::{Consensus, ElectionState};
use common::{Stub, append_position, at, canvass_position, new_leadership_term};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

#[test]
fn a_leader_and_a_follower_run_ten_thousand_cycles_without_an_allocation() {
    // A leader with its election closed.
    let mut leader_io = Stub::new();
    let mut leader = Consensus::new(&[0, 1, 2], 0).expect("a member");
    leader.do_work(at(1), &mut leader_io).expect("no fault");
    for follower in [1, 2] {
        leader
            .on_message(at(2), &canvass_position(follower, -1, 0), &mut leader_io)
            .expect("no fault");
    }
    for ms in 3..=10 {
        leader.do_work(at(ms), &mut leader_io).expect("no fault");
    }
    for follower in [1, 2] {
        leader
            .on_message(at(11), &append_position(follower, 0), &mut leader_io)
            .expect("no fault");
    }
    leader.do_work(at(12), &mut leader_io).expect("no fault");
    assert_eq!(leader.election_state(), ElectionState::Closed);

    // A follower with its election closed.
    let mut follower_io = Stub::new();
    let mut follower = Consensus::new(&[0, 1, 2], 1).expect("a member");
    follower.do_work(at(1), &mut follower_io).expect("no fault");
    follower
        .on_message(at(2), &new_leadership_term(0, 0), &mut follower_io)
        .expect("no fault");
    follower_io.image = Some(0);
    for ms in 3..=8 {
        follower
            .do_work(at(ms), &mut follower_io)
            .expect("no fault");
    }
    assert_eq!(follower.election_state(), ElectionState::Closed);

    // Ten seconds of the cluster's clock, a cycle a millisecond: every heartbeat and
    // every interval falls due many times. Each cycle the leader appends an entry and
    // reads both followers' reports, and the follower syncs it and reads the leader's
    // commit position.
    let payload = [7u8; 64];
    let region = Region::new(GLOBAL);
    let mut position = 0i64;
    for ms in 13..10_013u64 {
        let clock = at(ms);
        position += 96;
        leader_io.append_position = position;
        assert_eq!(leader.offer(clock, &payload, &mut leader_io), Ok(Ok(())));
        for id in [1, 2] {
            leader
                .on_message(clock, &append_position(id, position), &mut leader_io)
                .expect("no fault");
        }
        leader.do_work(clock, &mut leader_io).expect("no fault");

        follower_io.append_position = position;
        follower
            .on_message(
                clock,
                &Message::CommitPosition {
                    leadership_term_id: 0,
                    log_position: leader.commit_position(),
                    leader_member_id: 0,
                },
                &mut follower_io,
            )
            .expect("no fault");
        follower.do_work(clock, &mut follower_io).expect("no fault");
    }
    let change = region.change();
    assert_eq!(leader.commit_position(), position);
    assert_eq!(follower.commit_position(), position);
    assert_eq!(
        (change.allocations, change.reallocations),
        (0, 0),
        "the core allocated in its duty cycle: {change:?}"
    );
}
