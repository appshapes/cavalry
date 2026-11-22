//! A member that lost its log votes no second time in a term: one core driven by hand
//! through an [`Io`] that remembers what it was asked (`spec/cluster.md`, "A member
//! that lost its log"; ADR 0145).
//!
//! A member whose log is empty withholds its vote, and votes all the same only when
//! every voter has reported a log the candidate's is at least. That case alone let a
//! member that had voted for one candidate in a term, and then lost its log, vote for
//! a second candidate of the same term, and two members led one term: the search of
//! ADR 0145's Evidence found it, with the two candidates cut off from each other. So
//! the case asks one thing more: each voter other than the member and the candidate
//! has named a term below the request's. A vote the member cast in the term before it
//! lost its log went to a candidate that stored the term first and names it since.
//!
//! [`Io`]: cavalry_raft::Io

mod common;

use cavalry_proto::cluster::{Message, Template};
use cavalry_raft::Consensus;
use common::{Call, Stub, at};

/// Member 2 of `[0, 1, 2]`, its log empty, as a member that lost its log directory
/// starts, in canvass after its first duty cycle.
fn wiped(seed: u64, io: &mut Stub) -> Consensus {
    let mut member = Consensus::with_election(&[0, 1, 2], 2, seed).expect("a member");
    member.do_work(at(1), io).expect("no fault");
    member
}

/// The canvass position of `member`, whose log ends at position 98,304 of term 3 and
/// whose candidate term is `candidate_term`.
fn canvass(member: i32, candidate_term: i64) -> Message<'static> {
    Message::CanvassPosition {
        log_leadership_term_id: 3,
        log_position: 98_304,
        leadership_term_id: candidate_term,
        follower_member_id: member,
        protocol_version: 0,
    }
}

/// `candidate`'s request vote for `term`, with a log as long as each report.
fn request(candidate: i32, term: i64) -> Message<'static> {
    Message::RequestVote {
        log_leadership_term_id: 3,
        log_position: 98_304,
        candidate_term_id: term,
        candidate_member_id: candidate,
        protocol_version: 0,
    }
}

/// How many votes the member sent to `candidate`, among the calls kept.
fn votes_to(io: &Stub, candidate: i32) -> usize {
    io.count(|call| {
        matches!(
            call,
            Call::Send { to, template: Template::Vote } if *to == candidate
        )
    })
}

/// Member 1 canvasses with candidate term 4: it stood in term 4, and a vote this member
/// cast in term 4 before it lost its log may be member 1's. Member 0 asks for term 4
/// with a log at least every report. The member withholds: it sends no vote, stores no
/// term and counts the request in `cluster.votes_withheld`.
#[test]
fn a_member_that_lost_its_log_votes_in_no_term_another_voter_named() {
    let mut io = Stub::new();
    let mut member = wiped(1, &mut io);
    member
        .on_message(at(2), &canvass(1, 4), &mut io)
        .expect("no fault");
    member
        .on_message(at(3), &canvass(0, 3), &mut io)
        .expect("no fault");
    io.clear();
    member
        .on_message(at(4), &request(0, 4), &mut io)
        .expect("no fault");
    assert_eq!(votes_to(&io, 0), 0, "no vote in a term member 1 named");
    assert_eq!(
        io.count(|call| matches!(call, Call::StoreCandidateTerm(_))),
        0,
        "no term stored"
    );
    assert_eq!(member.counts().votes_withheld, 1);
    assert_eq!(member.counts().votes_for, 0);

    // A later term than any voter named is a term the member cast no vote in before
    // it lost its log, unless for this candidate: it votes.
    member
        .on_message(at(5), &request(0, 5), &mut io)
        .expect("no fault");
    assert_eq!(votes_to(&io, 0), 1, "a vote for term 5");
    assert_eq!(member.counts().votes_for, 1);
}

/// With member 1 below the request's term, member 0's request for term 4 is the case
/// in which a member that withholds votes all the same: every voter has reported, the
/// candidate's log is at least each, and no voter but the candidate named term 4. The
/// candidate's own term is not counted against it: a vote the member cast in term 4
/// before it lost its log could only have been the candidate's.
#[test]
fn a_member_that_lost_its_log_votes_when_no_other_voter_named_the_term() {
    let mut io = Stub::new();
    let mut member = wiped(2, &mut io);
    member
        .on_message(at(2), &canvass(1, 3), &mut io)
        .expect("no fault");
    member
        .on_message(at(3), &canvass(0, 4), &mut io)
        .expect("no fault");
    io.clear();
    member
        .on_message(at(4), &request(0, 4), &mut io)
        .expect("no fault");
    assert_eq!(votes_to(&io, 0), 1, "a vote for member 0 in term 4");
    assert!(
        io.first(|call| matches!(call, Call::StoreCandidateTerm(4)))
            .is_some(),
        "term 4 is stored before the vote"
    );
    assert_eq!(member.counts().votes_withheld, 0);

    // A member that has not reported is a voter that may hold a committed entry the
    // candidate lacks: the case asks every voter, as before.
    let mut io = Stub::new();
    let mut member = wiped(3, &mut io);
    member
        .on_message(at(2), &canvass(0, 3), &mut io)
        .expect("no fault");
    io.clear();
    member
        .on_message(at(4), &request(0, 4), &mut io)
        .expect("no fault");
    assert_eq!(votes_to(&io, 0), 0, "member 1 has not reported");
    assert_eq!(member.counts().votes_withheld, 1);
}
