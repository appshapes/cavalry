//! What a leader tells a member about the terms between the member's log and its own,
//! on one core driven by hand (`spec/cluster.md`, "New leadership term, template 53"
//! and "Messages during an election"): the next fields of a new leadership term name
//! the lowest term the leader's record holds above the one the member reports, with
//! its base and its end, and never a later term. A member truncates what it holds
//! above the next term's base, so a leader that named a later term would leave it a
//! tail of its own between the two bases.

mod common;

use cavalry_proto::cluster::Message;
use cavalry_raft::{Consensus, ElectionState, Role};
use common::{Stub, append_position, at, canvass_position, new_leadership_term};

fn work(member: &mut Consensus, io: &mut Stub, ms: u64) {
    member.do_work(at(ms), io).expect("no fault");
}

fn read(member: &mut Consensus, io: &mut Stub, ms: u64, message: &Message<'_>) {
    member.on_message(at(ms), message, io).expect("no fault");
}

/// Member 2's new leadership term for term 1, which begins at 64, to a member whose
/// log is of term 0.
fn term_one() -> Message<'static> {
    Message::NewLeadershipTerm {
        log_leadership_term_id: 0,
        next_leadership_term_id: 1,
        next_term_base_log_position: 64,
        next_log_position: -1,
        leadership_term_id: 1,
        term_base_log_position: 64,
        log_position: 64,
        leader_recording_id: 5,
        timestamp: 1_700_000_000_000,
        leader_member_id: 2,
        log_session_id: 78,
        app_version: 0,
        is_startup: true,
        commit_position: 64,
    }
}

/// Follows the leader's term from canvass to closed, with the image of its log joining
/// at `join`, from `ms` on; returns the millisecond its election closed at.
fn follow(member: &mut Consensus, io: &mut Stub, ms: u64, term: &Message<'_>, join: i64) -> u64 {
    assert_eq!(member.election_state(), ElectionState::Canvass);
    read(member, io, ms, term);
    assert_eq!(member.election_state(), ElectionState::FollowerReplay);
    work(member, io, ms + 1);
    work(member, io, ms + 2);
    io.image = Some(join);
    work(member, io, ms + 3);
    work(member, io, ms + 4);
    assert_eq!(member.election_state(), ElectionState::Closed);
    ms + 4
}

/// The follower hears nothing for the leader heartbeat timeout after `closed_at` and
/// canvasses again, with `appended` in its recordings; returns the next millisecond.
fn lose_the_leader(member: &mut Consensus, io: &mut Stub, closed_at: u64, appended: i64) -> u64 {
    let ms = closed_at + 10_000;
    work(member, io, ms);
    assert_eq!(member.election_state(), ElectionState::Init);
    io.image = None;
    io.append_position = appended;
    work(member, io, ms + 1);
    assert_eq!(member.election_state(), ElectionState::Canvass);
    ms + 2
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
    let mut member = Consensus::with_election(&[0, 1, 2], 0, 1).expect("a member");

    // Term 0, led by member 1 from 0.
    work(&mut member, &mut io, 1);
    let closed = follow(&mut member, &mut io, 2, &new_leadership_term(1, 0), 0);
    // Term 1, led by member 2 from 64, after 64 bytes of term 0.
    let ms = lose_the_leader(&mut member, &mut io, closed, 64);
    let closed = follow(&mut member, &mut io, ms, &term_one(), 64);
    assert_eq!(member.leader_member_id(), 2);
    // Term 2, its own, from 96, after 32 bytes of term 1.
    let ms = lose_the_leader(&mut member, &mut io, closed, 96);
    for other in [1, 2] {
        read(&mut member, &mut io, ms, &reports(other, 1, 96));
    }
    work(&mut member, &mut io, ms + 1);
    assert_eq!(member.election_state(), ElectionState::Nominate);
    work(&mut member, &mut io, ms + 501);
    assert_eq!(member.election_state(), ElectionState::CandidateBallot);
    for voter in [1, 2] {
        let vote = Message::Vote {
            candidate_term_id: 2,
            log_leadership_term_id: 1,
            log_position: 96,
            candidate_member_id: 0,
            follower_member_id: voter,
            vote: true,
        };
        read(&mut member, &mut io, ms + 502, &vote);
    }
    work(&mut member, &mut io, ms + 503);
    assert_eq!(member.role(), Role::Leader);
    assert_eq!(member.leadership_term_id(), 2);
    work(&mut member, &mut io, ms + 504);
    assert_eq!(
        io.terms.iter().flatten().copied().collect::<Vec<_>>(),
        vec![(0, 0), (1, 64), (2, 96)],
        "its record of terms"
    );
    assert_eq!(member.election_state(), ElectionState::LeaderLogReplication);

    // A member whose log is of term 0, at 64 or anywhere: term 1 is next, from 64 to
    // 96. Naming term 2 at 96 here is the fault this test is for.
    io.last_term = None;
    read(&mut member, &mut io, ms + 505, &reports(2, 0, 96));
    assert_eq!(io.last_term, Some((2, 0, [1, 64, 96])));
    // A member whose log is of term 1: this leader's term is next, with no end.
    read(&mut member, &mut io, ms + 506, &reports(2, 1, 96));
    assert_eq!(io.last_term, Some((2, 1, [2, 96, -1])));
    // A member with no term: term 0 is next, from 0 to 64.
    read(&mut member, &mut io, ms + 507, &reports(1, -1, 0));
    assert_eq!(io.last_term, Some((1, -1, [0, 0, 64])));

    // A member that has joined the leader's term is told no next term, in the
    // leader's heartbeat.
    io.last_term = None;
    read(&mut member, &mut io, ms + 508, &reports(2, 2, 96));
    assert_eq!(io.last_term, None, "one at its own term is not answered");
    read(
        &mut member,
        &mut io,
        ms + 508,
        &Message::AppendPosition {
            leadership_term_id: 2,
            log_position: 96,
            follower_member_id: 2,
            flags: 0,
        },
    );
    work(&mut member, &mut io, ms + 1_000);
    assert_eq!(io.last_term, Some((2, 2, [-1, -1, -1])));
}

/// The appointed leader's record holds its own term alone, so that term is the next
/// of any member's, at its base with no end, as before.
#[test]
fn the_appointed_leader_names_its_own_term() {
    let mut io = Stub::new();
    let mut leader = Consensus::new(&[0, 1, 2], 0).expect("a member");
    work(&mut leader, &mut io, 1);
    for other in [1, 2] {
        read(&mut leader, &mut io, 2, &reports(other, -1, 0));
    }
    work(&mut leader, &mut io, 3);
    work(&mut leader, &mut io, 4);
    assert_eq!(leader.election_state(), ElectionState::LeaderLogReplication);
    io.last_term = None;
    read(&mut leader, &mut io, 5, &reports(2, -1, 0));
    assert_eq!(io.last_term, Some((2, -1, [0, 0, -1])));
    // Keep the helper used by both paths of the crate's tests in use here.
    read(&mut leader, &mut io, 6, &append_position(1, 0));
}
