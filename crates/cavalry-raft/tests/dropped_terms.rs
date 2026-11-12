//! A term that fewer than a quorum recorded, on one core driven by hand
//! (`spec/cluster.md`, "New leadership term, template 53", and "Messages during an
//! election"; ADR 0096; card 267). A leader answers a member for the highest term its
//! own record holds at or below the one the member reported. A member that reads a
//! term below its own log term, with a next term above it, holds terms its leader's
//! record lacks: it drops them from its record, cutting first what it holds above
//! the base of the lowest, and reads the message again with the log it then holds.

mod common;

use cavalry_proto::cluster::Message;
use cavalry_raft::{Consensus, ElectionState};
use common::{
    Call, Stub, at, canvass_position, follow_term, leader_of_term_two, leadership_term,
    lose_the_leader, new_leadership_term,
};

/// Member 0 of three, electing, in canvass. It followed term 0, led by member 1 from
/// 0, and then term 1, led by member 2 from 64, and lost that leader holding
/// `appended` bytes: its record is (0, 0), (1, 64) and its log term 1. Returns the
/// member and the next millisecond, the calls so far forgotten.
fn member_of_term_one(io: &mut Stub, appended: i64) -> (Consensus, u64) {
    let mut member = Consensus::with_election(&[0, 1, 2], 0, 1).expect("a member");
    member.do_work(at(1), io).expect("no fault");
    let closed = follow_term(&mut member, io, 2, &new_leadership_term(1, 0), 0);
    let ms = lose_the_leader(&mut member, io, closed, 64);
    let term_one = leadership_term(2, 1, 64, 64, 0, [1, 64, -1]);
    let closed = follow_term(&mut member, io, ms, &term_one, 64);
    let ms = lose_the_leader(&mut member, io, closed, appended);
    assert_eq!(
        io.terms.iter().flatten().copied().collect::<Vec<_>>(),
        vec![(0, 0), (1, 64)]
    );
    io.clear();
    (member, ms)
}

/// Member 1's new leadership term for its term 3, which begins at 128, from a record
/// that holds terms 0 and 3 and no term 1: answered to a member that reported term 1,
/// it is for term 0, the highest the record holds at or below 1, and names term 3 at
/// 128 as the next.
fn term_three_for_a_record_without_term_one() -> Message<'static> {
    leadership_term(1, 3, 128, 128, 0, [3, 128, -1])
}

fn record(io: &Stub) -> Vec<(i64, i64)> {
    io.terms.iter().flatten().copied().collect()
}

/// The member holds term 1 at 64 and no byte of it, and its leader's record has no
/// such term. Nothing is cut; term 1 leaves the record, counted; and the message is
/// read again by a member whose log is of term 0 and ends at 64, below the leader's
/// base: it replicates the rest of term 0, to 128, from that leader.
#[test]
fn a_member_drops_a_term_its_leaders_record_lacks_and_reads_the_message_again() {
    let mut io = Stub::new();
    let (mut member, ms) = member_of_term_one(&mut io, 64);

    member
        .on_message(at(ms), &term_three_for_a_record_without_term_one(), &mut io)
        .expect("no fault");
    assert_eq!(io.count(|call| *call == Call::DropTermsAbove(0)), 1);
    assert_eq!(io.count(|call| matches!(call, Call::TruncateLog(_))), 0);
    assert_eq!(record(&io), vec![(0, 0)]);
    assert_eq!(member.counts().terms_dropped, 1);
    assert_eq!(member.counts().log_truncations, 0);
    assert_eq!(member.counts().elections_failed, 0);
    assert_eq!(
        member.election_state(),
        ElectionState::FollowerLogReplication
    );
    assert_eq!(member.leader_member_id(), 1);
    assert_eq!(member.append_position(), 64);
}

/// The member holds 32 bytes above the base of the term its leader lacks. It cuts its
/// log to that base first, counted as a truncation, and then drops the term.
#[test]
fn a_member_cuts_what_it_holds_above_a_dropped_terms_base_before_it_drops_the_term() {
    let mut io = Stub::new();
    let (mut member, ms) = member_of_term_one(&mut io, 96);

    member
        .on_message(at(ms), &term_three_for_a_record_without_term_one(), &mut io)
        .expect("no fault");
    let cut = io
        .first(|call| *call == Call::TruncateLog(64))
        .expect("the log truncated");
    let dropped = io
        .first(|call| *call == Call::DropTermsAbove(0))
        .expect("the term dropped");
    assert!(cut < dropped, "{:?}", io.calls);
    assert_eq!(record(&io), vec![(0, 0)]);
    assert_eq!(member.append_position(), 64);
    assert_eq!(member.counts().log_truncations, 1);
    assert_eq!(member.counts().terms_dropped, 1);
    assert_eq!(
        member.election_state(),
        ElectionState::FollowerLogReplication
    );
}

/// The guard of a truncation holds for a drop: a member that knows a commit position
/// above the base it would cut to keeps its log and its record, counted, and fails
/// its election (ADR 0083).
#[test]
fn a_member_does_not_drop_a_term_when_the_cut_is_below_the_commit_position_it_knows() {
    let mut io = Stub::new();
    let mut member = Consensus::with_election(&[0, 1, 2], 0, 1).expect("a member");
    member.do_work(at(1), &mut io).expect("no fault");
    let closed = follow_term(&mut member, &mut io, 2, &new_leadership_term(1, 0), 0);
    let ms = lose_the_leader(&mut member, &mut io, closed, 64);
    let term_one = leadership_term(2, 1, 64, 64, 0, [1, 64, -1]);
    let closed = follow_term(&mut member, &mut io, ms, &term_one, 64);
    io.append_position = 96;
    member.do_work(at(closed + 1), &mut io).expect("no fault");
    member
        .on_message(
            at(closed + 2),
            &Message::CommitPosition {
                leadership_term_id: 1,
                log_position: 96,
                leader_member_id: 2,
            },
            &mut io,
        )
        .expect("no fault");
    assert_eq!(member.commit_position(), 96);
    let ms = lose_the_leader(&mut member, &mut io, closed + 2, 96);
    let failed = member.counts().elections_failed;
    io.clear();

    member
        .on_message(at(ms), &term_three_for_a_record_without_term_one(), &mut io)
        .expect("no fault");
    assert_eq!(member.election_state(), ElectionState::Init);
    assert_eq!(member.counts().truncations_refused, 1);
    assert_eq!(member.counts().terms_dropped, 0);
    assert_eq!(member.counts().elections_failed, failed + 1);
    assert_eq!(record(&io), vec![(0, 0), (1, 64)]);
    assert_eq!(
        io.made, 0,
        "nothing is cut and nothing dropped: {:?}",
        io.calls
    );
}

/// An answer to an older report drops nothing. A new leadership term names as its
/// next term the lowest in the leader's record above the term the member reported.
/// One whose next term is at or below the member's log term answers a report the
/// member made before it recorded that term: the member canvasses on, with its record
/// as it is. So does a late copy from the leader the member joined, whose term is the
/// member's log term.
#[test]
fn an_answer_to_an_older_report_drops_nothing() {
    let mut io = Stub::new();
    let (mut member, ms) = member_of_term_one(&mut io, 96);

    // From a leader of term 3 whose record holds term 1: the answer to a report of
    // term 0, which this member made before it recorded term 1.
    let older = leadership_term(1, 3, 128, 128, 0, [1, 64, 128]);
    member
        .on_message(at(ms), &older, &mut io)
        .expect("no fault");
    // A late copy of what member 2 sent this member to bring it into term 1.
    let late = leadership_term(2, 1, 64, 64, 0, [1, 64, -1]);
    member
        .on_message(at(ms + 1), &late, &mut io)
        .expect("no fault");

    assert_eq!(member.election_state(), ElectionState::Canvass);
    assert_eq!(record(&io), vec![(0, 0), (1, 64)]);
    assert_eq!(member.append_position(), 96);
    assert_eq!(member.counts().terms_dropped, 0);
    assert_eq!(member.counts().log_truncations, 0);
    assert_eq!(
        io.count(|call| matches!(call, Call::DropTermsAbove(_) | Call::TruncateLog(_))),
        0
    );
}

/// A leader answers for a term its own record holds. Its record is (0, 0), (2, 96):
/// it never recorded term 1. To a member that reports a log of term 1 it answers for
/// term 0, and names its own term 2 at 96 as the next, the lowest it holds above the
/// term reported. With term 1 in its record it answers for term 1, as before.
#[test]
fn a_leader_answers_for_the_highest_term_its_record_holds_at_or_below_the_one_reported() {
    let mut io = Stub::new();
    let (mut leader, ms) = leader_of_term_two(&mut io);
    io.last_term = None;
    leader
        .on_message(at(ms), &canvass_position(2, 1, 64), &mut io)
        .expect("no fault");
    assert_eq!(io.last_term, Some((2, 1, [2, 96, -1])));

    // The same leader with no term 1 in its record.
    for slot in &mut io.terms {
        if slot.is_some_and(|(term, _)| term == 1) {
            *slot = None;
        }
    }
    io.last_term = None;
    leader
        .on_message(at(ms + 1), &canvass_position(2, 1, 64), &mut io)
        .expect("no fault");
    assert_eq!(io.last_term, Some((2, 0, [2, 96, -1])));
}
