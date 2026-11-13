//! What a leader tells a member about the terms between the member's log and its own,
//! on one core driven by hand (`spec/cluster.md`, "New leadership term, template 53"
//! and "Messages during an election"): the next fields of a new leadership term name
//! the lowest term the leader's record holds above the one the member reports, with
//! its base and its end, and never a later term. A member truncates what it holds
//! above the next term's base, so a leader that named a later term would leave it a
//! tail of its own between the two bases.

mod common;

use cavalry_proto::cluster::Message;
use cavalry_raft::Consensus;
use common::{Stub, at, canvass_position, leader_of_term_two};

fn work(member: &mut Consensus, io: &mut Stub, ms: u64) {
    member.do_work(at(ms), io).expect("no fault");
}

fn read(member: &mut Consensus, io: &mut Stub, ms: u64, message: &Message<'_>) {
    member.on_message(at(ms), message, io).expect("no fault");
}

/// `member`'s canvass position, reporting a log of `term` that ends at `position`.
fn reports(member: i32, term: i64, position: i64) -> Message<'static> {
    canvass_position(member, term, position)
}

/// A member follows term 0 and term 1, each recorded at its base, and leads term 2.
/// To a member whose log is of term 0 it names term 1, from 64 to 96; to one of term 1,
/// its own term at 96 with no end; to one with no term, term 0 from 0 to 64; and to
/// one of its own term, nothing.
#[test]
fn a_leader_names_the_term_its_record_holds_next_after_the_members_own() {
    let mut io = Stub::new();
    let (mut member, ms) = leader_of_term_two(&mut io);

    // A member whose log is of term 0, at 64 or anywhere: term 1 is next, from 64 to
    // 96. Naming term 2 at 96 here is the fault this test is for.
    io.last_term = None;
    read(&mut member, &mut io, ms, &reports(2, 0, 96));
    assert_eq!(io.last_term, Some((2, 0, [1, 64, 96])));
    // A member whose log is of term 1: this leader's term is next, with no end.
    read(&mut member, &mut io, ms + 1, &reports(2, 1, 96));
    assert_eq!(io.last_term, Some((2, 1, [2, 96, -1])));
    // A member with no term: term 0 is next, from 0 to 64.
    read(&mut member, &mut io, ms + 2, &reports(1, -1, 0));
    assert_eq!(io.last_term, Some((1, -1, [0, 0, 64])));

    // A member that has joined the leader's term is told no next term, in the
    // leader's heartbeat.
    io.last_term = None;
    read(&mut member, &mut io, ms + 3, &reports(2, 2, 96));
    assert_eq!(io.last_term, None, "one at its own term is not answered");
    read(
        &mut member,
        &mut io,
        ms + 3,
        &Message::AppendPosition {
            leadership_term_id: 2,
            log_position: 96,
            follower_member_id: 2,
            flags: 0,
        },
    );
    work(&mut member, &mut io, ms + 495);
    assert_eq!(io.last_term, Some((2, 2, [-1, -1, -1])));
}
