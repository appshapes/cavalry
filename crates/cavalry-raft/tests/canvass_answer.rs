//! A member that holds a log answers the canvass position of a member whose log is
//! empty with a canvass position of its own: one core at a time, driven by hand
//! through an [`Io`] that remembers what it was asked (`spec/cluster.md`, "A member
//! that lost its log", "Taking a leader"; ADR 0145). A member whose election has
//! closed sends no canvass position of its own. Without the answer, a member that lost
//! its log would never hear such a member, so it would take no leader while the
//! others run on.
//!
//! What the tests pin:
//!
//! - Who answers: a member that holds a log, does not lead, and is not in canvass or
//!   nominate.
//! - What it sends: one canvass position, of its own log, its candidate term and its
//!   own id, to the member whose log is empty and to nobody else.
//! - When it sends nothing: in answer to a canvass position that holds a log, when
//!   its own log is empty, and in canvass or nominate, beyond the canvass position it
//!   sends to each member at its own interval.
//!
//! [`Io`]: cavalry_raft::Io

mod common;

use cavalry_proto::cluster::{Message, Template};
use cavalry_raft::{Consensus, ElectionState, Role};
use common::{Call, Stub, at, canvass_position, follow_term, new_leadership_term};

/// Member 0 of `[0, 1, 2]`, electing.
fn elector(seed: u64) -> Consensus {
    Consensus::with_election(&[0, 1, 2], 0, seed).expect("a member")
}

/// The canvass position of `member`, whose log is empty: log leadership term id −1 at
/// position 0, as a member that lost its log directory sends.
fn empty(member: i32) -> Message<'static> {
    canvass_position(member, -1, 0)
}

/// How many messages the member sent, among the calls kept.
fn sends(io: &Stub) -> usize {
    io.count(|call| matches!(call, Call::Send { .. }))
}

/// How many canvass positions the member sent to `member`, among the calls kept.
fn canvass_positions_to(io: &Stub, member: i32) -> usize {
    io.count(|call| {
        matches!(
            call,
            Call::Send { to, template: Template::CanvassPosition } if *to == member
        )
    })
}

/// Takes member 0 to a follower of member 1 in term 0, its election closed, with 64
/// bytes of the leader's log in its recordings. Returns the millisecond of its last
/// duty cycle.
fn closed_follower(member: &mut Consensus, io: &mut Stub) -> u64 {
    member.do_work(at(1), io).expect("no fault");
    let closed = follow_term(member, io, 2, &new_leadership_term(1, 0), 0);
    io.append_position = 64;
    member.do_work(at(closed + 1), io).expect("no fault");
    assert_eq!(member.election_state(), ElectionState::Closed);
    assert_eq!(member.role(), Role::Follower);
    assert_eq!(member.append_position(), 64);
    closed + 1
}

/// A follower whose election has closed and whose log holds term 0 to position 64
/// answers the canvass position of member 2, whose log is empty. It sends one canvass
/// position, to member 2 alone, with its own log, its candidate term and its own id.
/// It answers again each time it reads one, and member 2's canvass interval bounds
/// how often that is.
#[test]
fn a_closed_follower_that_holds_a_log_answers_a_canvass_position_whose_log_is_empty() {
    let mut io = Stub::new();
    let mut member = elector(1);
    let ms = closed_follower(&mut member, &mut io);
    io.clear();
    io.last_canvass = None;
    io.last_canvass_from = None;
    member
        .on_message(at(ms + 1), &empty(2), &mut io)
        .expect("no fault");
    assert_eq!(sends(&io), 1, "one message, and nothing else");
    assert_eq!(canvass_positions_to(&io, 2), 1, "to member 2");
    assert_eq!(
        io.last_canvass,
        Some((0, 64, 0)),
        "its log leadership term id, its append position and its candidate term"
    );
    assert_eq!(io.last_canvass_from, Some(0), "in its own name");
    assert_eq!(member.election_state(), ElectionState::Closed);
    assert_eq!(member.leader_member_id(), 1, "its leader is unchanged");

    // The next canvass position of member 2, an election status interval later.
    member
        .on_message(at(ms + 101), &empty(2), &mut io)
        .expect("no fault");
    assert_eq!(sends(&io), 2);
    assert_eq!(canvass_positions_to(&io, 2), 2);
}

/// A follower whose election has closed answers no canvass position that holds a log:
/// log leadership term id 0 or more, or a position above 0. A member that holds a log
/// hears the others by their own canvass positions, as every member did before ADR
/// 0145.
#[test]
fn a_closed_follower_answers_no_canvass_position_that_holds_a_log() {
    let mut io = Stub::new();
    let mut member = elector(2);
    let ms = closed_follower(&mut member, &mut io);
    io.clear();
    for (term, position) in [(0, 64), (0, 0), (-1, 32)] {
        member
            .on_message(at(ms + 1), &canvass_position(2, term, position), &mut io)
            .expect("no fault");
        assert_eq!(sends(&io), 0, "a log of term {term} at {position}");
    }
    assert_eq!(member.election_state(), ElectionState::Closed);
}

/// A member whose own log is empty answers no canvass position, even one whose log is
/// empty too and in a state other than canvass and nominate. Two members that hold no
/// log never answer each other's answers. Member 0 has heard both other members, each
/// with no log, as a member whose log is empty must before it votes (work item 476).
/// It has voted for member 1 in term 0, with no log, and waits in follower ballot.
#[test]
fn a_member_whose_log_is_empty_answers_no_canvass_position() {
    let mut io = Stub::new();
    let mut member = elector(3);
    member.do_work(at(1), &mut io).expect("no fault");
    assert_eq!(member.election_state(), ElectionState::Canvass);
    for other in [1, 2] {
        member
            .on_message(at(2), &empty(other), &mut io)
            .expect("no fault");
    }
    let request = Message::RequestVote {
        log_leadership_term_id: -1,
        log_position: 0,
        candidate_term_id: 0,
        candidate_member_id: 1,
        protocol_version: 0,
    };
    member
        .on_message(at(2), &request, &mut io)
        .expect("no fault");
    assert_eq!(member.election_state(), ElectionState::FollowerBallot);
    assert_eq!(member.counts().votes_for, 1);
    io.clear();
    member
        .on_message(at(3), &empty(2), &mut io)
        .expect("no fault");
    assert_eq!(sends(&io), 0);
}

/// A member in canvass or in nominate already sends its canvass position to every
/// other member, once each election status interval, 100 ms. It sends nothing more in
/// answer to a canvass position whose log is empty. Member 0 starts on a log directory
/// that holds term 0 to position 64, so it canvasses until the startup canvass
/// timeout, 60 s, unless every member has reported.
#[test]
fn a_member_in_canvass_or_nominate_answers_with_nothing_beyond_its_own_canvass() {
    let mut io = Stub::new();
    io.candidate_term = 0;
    io.log_term = 0;
    io.append_position = 64;
    let mut member = elector(4);
    member.do_work(at(1), &mut io).expect("no fault");
    assert_eq!(member.election_state(), ElectionState::Canvass);
    // Its first canvass position, one to each other member.
    member.do_work(at(2), &mut io).expect("no fault");
    assert_eq!(canvass_positions_to(&io, 1), 1);
    assert_eq!(canvass_positions_to(&io, 2), 1);
    io.clear();

    // Canvass: the canvass position of member 2, whose log is empty, gets no answer.
    member
        .on_message(at(3), &empty(2), &mut io)
        .expect("no fault");
    assert_eq!(sends(&io), 0, "no answer in canvass");
    member.do_work(at(4), &mut io).expect("no fault");
    member.do_work(at(101), &mut io).expect("no fault");
    assert_eq!(member.election_state(), ElectionState::Canvass);
    assert_eq!(sends(&io), 0, "nothing before its interval");
    // Its own, at its interval: one to each other member, and nothing more.
    member.do_work(at(102), &mut io).expect("no fault");
    assert_eq!(member.election_state(), ElectionState::Canvass);
    assert_eq!(canvass_positions_to(&io, 1), 1);
    assert_eq!(canvass_positions_to(&io, 2), 1);
    assert_eq!(sends(&io), 2);

    // Nominate: every member has reported, and no log is ahead of its own.
    member
        .on_message(at(103), &canvass_position(1, 0, 64), &mut io)
        .expect("no fault");
    member.do_work(at(104), &mut io).expect("no fault");
    assert_eq!(member.election_state(), ElectionState::Nominate);
    io.clear();
    member
        .on_message(at(105), &empty(2), &mut io)
        .expect("no fault");
    assert_eq!(sends(&io), 0, "no answer in nominate");
}
