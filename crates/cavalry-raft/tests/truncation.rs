//! A member that truncates its log to join a leader, on one core driven by hand
//! (`spec/cluster.md`, "Messages during an election", new leadership term; ADR 0083).
//! A new leadership term names the term the leader's record holds next after the
//! member's own, with its base. A member whose append position is above that base
//! holds a tail the leader's log does not: it cuts it before it records or reports
//! anything, and reads the message with what it then holds.

mod common;

use cavalry_proto::cluster::Message;
use cavalry_raft::{Consensus, ElectionState};
use common::{Call, Stub, at, follow_term, leadership_term, lose_the_leader, new_leadership_term};

/// Member 0 of `[0, 1, 2]`, electing, a follower of member 1's term 0 whose election
/// has closed. Returns the millisecond it closed at.
fn follower_of_term_zero(io: &mut Stub) -> (Consensus, u64) {
    let mut member = Consensus::with_election(&[0, 1, 2], 0, 1).expect("a member");
    member.do_work(at(1), io).expect("no fault");
    let closed = follow_term(&mut member, io, 2, &new_leadership_term(1, 0), 0);
    (member, closed)
}

/// Member 2's new leadership term for term 1, which begins at 64, to a member whose
/// log is of term 0: term 1 is the next term, from 64.
fn term_one_from_64() -> Message<'static> {
    leadership_term(2, 1, 64, 64, 0, [1, 64, -1])
}

/// A member that holds 128 bytes of term 0 is told that term 1 begins at 64. It cuts
/// its log to 64 before it records term 1 there, and follows from that position.
#[test]
fn a_member_truncates_to_the_next_terms_base_before_it_records_the_term() {
    let mut io = Stub::new();
    let (mut member, closed) = follower_of_term_zero(&mut io);
    let ms = lose_the_leader(&mut member, &mut io, closed, 128);
    assert_eq!(member.append_position(), 128);
    io.clear();

    member
        .on_message(at(ms), &term_one_from_64(), &mut io)
        .expect("no fault");
    assert_eq!(member.election_state(), ElectionState::FollowerReplay);
    assert_eq!(member.append_position(), 64);
    assert_eq!(member.leader_member_id(), 2);
    assert_eq!(member.counts().log_truncations, 1);
    assert_eq!(member.counts().elections_failed, 0);
    let truncated = io
        .first(|call| *call == Call::TruncateLog(64))
        .expect("the log truncated");
    let recorded = io
        .first(|call| *call == Call::RecordTerm(1, 64))
        .expect("the term recorded");
    assert!(truncated < recorded, "{:?}", io.calls);
    assert_eq!(io.count(|call| matches!(call, Call::TruncateLog(_))), 1);
    assert_eq!(
        io.terms.iter().flatten().copied().collect::<Vec<_>>(),
        vec![(0, 0), (1, 64)]
    );
    // The position stays down: the next duty cycle reads it from the recordings.
    member.do_work(at(ms + 1), &mut io).expect("no fault");
    assert_eq!(member.append_position(), 64);
}

/// A member at or below the next term's base truncates nothing.
#[test]
fn a_member_at_the_next_terms_base_truncates_nothing() {
    let mut io = Stub::new();
    let (mut member, closed) = follower_of_term_zero(&mut io);
    let ms = lose_the_leader(&mut member, &mut io, closed, 64);
    io.clear();
    member
        .on_message(at(ms), &term_one_from_64(), &mut io)
        .expect("no fault");
    assert_eq!(member.election_state(), ElectionState::FollowerReplay);
    assert_eq!(io.count(|call| matches!(call, Call::TruncateLog(_))), 0);
    assert_eq!(member.counts().log_truncations, 0);
}

/// While the truncation is under way the member does nothing with the message: it
/// records no term, takes no leader and stays where it is. The leader's next new
/// leadership term asks again.
#[test]
fn a_truncation_under_way_moves_nothing_and_the_next_message_asks_again() {
    let mut io = Stub::new();
    let (mut member, closed) = follower_of_term_zero(&mut io);
    let ms = lose_the_leader(&mut member, &mut io, closed, 128);
    io.clear();

    io.truncating = true;
    member
        .on_message(at(ms), &term_one_from_64(), &mut io)
        .expect("no fault");
    assert_eq!(member.election_state(), ElectionState::Canvass);
    assert_eq!(member.append_position(), 128);
    assert_eq!(member.leader_member_id(), -1);
    assert_eq!(member.counts().log_truncations, 0);
    assert_eq!(io.count(|call| matches!(call, Call::RecordTerm(..))), 0);

    io.truncating = false;
    member
        .on_message(at(ms + 200), &term_one_from_64(), &mut io)
        .expect("no fault");
    assert_eq!(member.election_state(), ElectionState::FollowerReplay);
    assert_eq!(member.append_position(), 64);
    assert_eq!(member.counts().log_truncations, 1);
    assert_eq!(io.count(|call| *call == Call::TruncateLog(64)), 2);
}

/// A committed entry is never truncated. A member that was told a commit position of
/// 128 and is then told to truncate to 64 does not: no leader that holds what was
/// committed names such a base. It fails its election and keeps its log.
#[test]
fn a_member_does_not_truncate_below_the_commit_position_it_knows() {
    let mut io = Stub::new();
    let (mut member, closed) = follower_of_term_zero(&mut io);
    io.append_position = 128;
    member.do_work(at(closed + 1), &mut io).expect("no fault");
    member
        .on_message(
            at(closed + 2),
            &Message::CommitPosition {
                leadership_term_id: 0,
                log_position: 128,
                leader_member_id: 1,
            },
            &mut io,
        )
        .expect("no fault");
    assert_eq!(member.commit_position(), 128);
    let ms = lose_the_leader(&mut member, &mut io, closed + 2, 128);
    let failed = member.counts().elections_failed;
    io.clear();

    member
        .on_message(at(ms), &term_one_from_64(), &mut io)
        .expect("no fault");
    assert_eq!(member.election_state(), ElectionState::Init);
    assert_eq!(member.counts().elections_failed, failed + 1);
    assert_eq!(member.counts().truncations_refused, 1);
    assert_eq!(member.counts().log_truncations, 0);
    assert_eq!(member.append_position(), 128);
    assert_eq!(
        io.made, 0,
        "nothing is cut and nothing recorded: {:?}",
        io.calls
    );
}

/// A truncation takes out of the record every term recorded above it, and the member's
/// log term is then the highest the record still holds. A member joined term 1 at 96,
/// behind a leader that knew a commit position of 64, and holds none of term 1's
/// entries: the 32 bytes from 64 are a tail of term 0 that this member and that leader
/// alone held. A leader of term 2 whose record lacks term 1 names term 2 as the next,
/// from 64. The member cuts its log to 64, its record loses term 1, and it records
/// term 2 at 64 as a member whose log is of term 0.
#[test]
fn a_truncation_takes_the_terms_recorded_above_it_out_of_the_log_term() {
    let mut io = Stub::new();
    let (mut member, closed) = follower_of_term_zero(&mut io);
    let ms = lose_the_leader(&mut member, &mut io, closed, 96);
    let mut term_one_from_96 = leadership_term(2, 1, 96, 96, 0, [1, 96, -1]);
    if let Message::NewLeadershipTerm {
        commit_position, ..
    } = &mut term_one_from_96
    {
        *commit_position = 64;
    }
    let closed = follow_term(&mut member, &mut io, ms, &term_one_from_96, 96);
    assert_eq!(member.commit_position(), 64);
    assert_eq!(
        io.terms.iter().flatten().copied().collect::<Vec<_>>(),
        vec![(0, 0), (1, 96)]
    );
    let ms = lose_the_leader(&mut member, &mut io, closed, 96);
    io.clear();

    // Member 1 leads term 2 from 64, and describes term 1, which this member reports
    // and its own record lacks: the next term above it is its own.
    let term_two_from_64 = leadership_term(1, 2, 64, 64, 1, [2, 64, -1]);
    member
        .on_message(at(ms), &term_two_from_64, &mut io)
        .expect("no fault");
    assert_eq!(member.election_state(), ElectionState::FollowerReplay);
    assert_eq!(member.append_position(), 64);
    assert_eq!(member.leadership_term_id(), 2);
    let mut terms: Vec<(i64, i64)> = io.terms.iter().flatten().copied().collect();
    terms.sort_unstable();
    assert_eq!(terms, vec![(0, 0), (2, 64)]);
    let truncated = io
        .first(|call| *call == Call::TruncateLog(64))
        .expect("the log truncated");
    let recorded = io
        .first(|call| *call == Call::RecordTerm(2, 64))
        .expect("the term recorded");
    assert!(truncated < recorded, "{:?}", io.calls);
}

/// A member whose leader is appointed has no truncation: it fails its election where
/// the election's member truncates, as before.
#[test]
fn a_member_whose_leader_is_appointed_truncates_nothing() {
    let mut io = Stub::new();
    io.append_position = 64;
    let mut member = Consensus::new(&[0, 1, 2], 1).expect("a member");
    member.do_work(at(1), &mut io).expect("no fault");
    member
        .on_message(at(2), &new_leadership_term(0, 0), &mut io)
        .expect("no fault");
    assert_eq!(member.election_state(), ElectionState::Init);
    assert_eq!(member.counts().elections_failed, 1);
    assert_eq!(member.counts().log_truncations, 0);
    assert_eq!(io.count(|call| matches!(call, Call::TruncateLog(_))), 0);
    assert_eq!(member.append_position(), 64);
}
