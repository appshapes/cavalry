//! A member whose truncation faulted part way, on one core driven by hand
//! (`spec/cluster.md`, "Messages during an election"; ADR 0148). The member holds 128
//! bytes of term 0, and a new leadership term says that term 1 begins at 64: it is to
//! cut its log to 64. A truncation whose command faults leaves the member's recordings
//! cut as far as its steps went, and the end it reports as it was: the stub's fault
//! changes nothing, and the member reports 128.
//! The member owes the truncation from then on. It asks for it again each duty cycle,
//! and until one finishes it neither nominates nor reads a new leadership term, so it
//! neither leads nor joins a log on an end it no longer holds.

mod common;

use cavalry_proto::cluster::Message;
use cavalry_raft::{Consensus, ElectionState};
use common::{
    Call, Stub, at, canvass_position, follow_term, leadership_term, lose_the_leader,
    new_leadership_term,
};

/// Member 0 of three, a follower of member 1's term 0 whose leader went quiet, with 128
/// bytes of term 0 in its recordings, in canvass. Returns the next millisecond.
fn holding_128_of_term_zero(io: &mut Stub) -> (Consensus, u64) {
    let mut member = Consensus::with_election(&[0, 1, 2], 0, 1).expect("a member");
    member.do_work(at(1), io).expect("no fault");
    let closed = follow_term(&mut member, io, 2, &new_leadership_term(1, 0), 0);
    let ms = lose_the_leader(&mut member, io, closed, 128);
    assert_eq!(member.append_position(), 128);
    io.clear();
    (member, ms)
}

/// Member 2's new leadership term for term 1, which begins at 64, to a member whose
/// log is of term 0: term 1 is the next term, from 64.
fn term_one_from_64() -> Message<'static> {
    leadership_term(2, 1, 64, 64, 0, [1, 64, -1])
}

/// The truncation's command faults: the member owes it, counts it once, fails its
/// election and keeps reporting the end it had. It asks for the truncation again at
/// each duty cycle, and a cycle whose truncation faults again does nothing else, so
/// it stays in init, where it reads and sends nothing. When the fault is gone the
/// truncation finishes, once, and the member canvasses with the log cut to 64.
#[test]
fn a_truncation_that_faults_is_owed_and_asked_for_each_cycle_until_it_finishes() {
    let mut io = Stub::new();
    let (mut member, ms) = holding_128_of_term_zero(&mut io);
    let failed = member.counts().elections_failed;

    io.truncate_faults = 3;
    assert!(
        member
            .on_message(at(ms), &term_one_from_64(), &mut io)
            .is_err(),
        "the fault comes back"
    );
    assert_eq!(member.truncation_owed(), 64);
    assert_eq!(member.counts().truncations_faulted, 1);
    assert_eq!(member.counts().elections_failed, failed + 1);
    assert_eq!(member.counts().log_truncations, 0);
    assert_eq!(member.election_state(), ElectionState::Init);
    assert_eq!(member.append_position(), 128, "the end it reported");
    assert_eq!(member.leader_member_id(), -1);

    for cycle in 1..=2 {
        assert!(
            member.do_work(at(ms + cycle), &mut io).is_err(),
            "the truncation asked again faults again"
        );
        assert_eq!(member.election_state(), ElectionState::Init);
        assert_eq!(member.truncation_owed(), 64);
    }
    assert_eq!(
        member.counts().truncations_faulted,
        1,
        "one truncation, counted once"
    );
    assert_eq!(io.count(|call| matches!(call, Call::Send { .. })), 0);

    member.do_work(at(ms + 3), &mut io).expect("no fault");
    assert_eq!(member.truncation_owed(), -1);
    assert_eq!(member.append_position(), 64);
    assert_eq!(member.counts().log_truncations, 1);
    assert_eq!(member.counts().truncations_faulted, 1);
    assert_eq!(io.count(|call| *call == Call::TruncateLog(64)), 4);
    assert_eq!(io.count(|call| matches!(call, Call::TruncateLog(_))), 4);
    member.do_work(at(ms + 4), &mut io).expect("no fault");
    assert_eq!(member.election_state(), ElectionState::Canvass);
}

/// While the truncation is owed and under way the member canvasses. Every other member
/// reports a log behind its own, which would make it a unanimous candidate, and it
/// does not nominate. A new leadership term that asks no truncation of the end it
/// reports, 128, is not read: no term recorded, no leader taken. Once the truncation
/// finishes the member nominates on the log it then holds.
#[test]
fn a_member_that_owes_a_truncation_neither_nominates_nor_takes_a_leader() {
    let mut io = Stub::new();
    let (mut member, ms) = holding_128_of_term_zero(&mut io);
    io.truncate_faults = 1;
    assert!(
        member
            .on_message(at(ms), &term_one_from_64(), &mut io)
            .is_err()
    );
    io.truncating = true;
    member.do_work(at(ms + 1), &mut io).expect("no fault");
    member.do_work(at(ms + 2), &mut io).expect("no fault");
    assert_eq!(member.election_state(), ElectionState::Canvass);
    assert_eq!(member.truncation_owed(), 64);

    for other in [1, 2] {
        member
            .on_message(at(ms + 3), &canvass_position(other, 0, 32), &mut io)
            .expect("no fault");
    }
    for cycle in 4..40 {
        member.do_work(at(ms + cycle), &mut io).expect("no fault");
        assert_eq!(member.election_state(), ElectionState::Canvass);
    }
    assert_eq!(member.counts().candidacies, 0);

    // Term 1 from 128: the end the member reports, so no truncation is asked of it.
    member
        .on_message(
            at(ms + 40),
            &leadership_term(1, 1, 128, 128, 0, [1, 128, -1]),
            &mut io,
        )
        .expect("no fault");
    assert_eq!(member.election_state(), ElectionState::Canvass);
    assert_eq!(member.leader_member_id(), -1);
    assert_eq!(
        member.counts().leadership_terms_ignored,
        1,
        "counted as not read"
    );
    assert_eq!(io.count(|call| matches!(call, Call::RecordTerm(..))), 0);
    assert_eq!(io.count(|call| matches!(call, Call::JoinLog)), 0);
    assert_eq!(io.count(|call| matches!(call, Call::AddCatchup)), 0);

    io.truncating = false;
    member.do_work(at(ms + 41), &mut io).expect("no fault");
    assert_eq!(member.truncation_owed(), -1);
    assert_eq!(member.append_position(), 64);
    let mut cycle = 42;
    while member.election_state() == ElectionState::Canvass && cycle < 2_000 {
        member.do_work(at(ms + cycle), &mut io).expect("no fault");
        cycle += 1;
    }
    assert_eq!(member.election_state(), ElectionState::Nominate);
}

/// A new leadership term read while a truncation is owed finishes the owed one first,
/// and is then read with the log the member holds. This one names a base, 96, above
/// the owed position: the member never truncates to it, since its recordings may end
/// below it, and replicates from 64 to 96 instead.
#[test]
fn a_new_leadership_term_is_read_once_the_owed_truncation_finishes() {
    let mut io = Stub::new();
    let (mut member, ms) = holding_128_of_term_zero(&mut io);
    io.truncate_faults = 1;
    assert!(
        member
            .on_message(at(ms), &term_one_from_64(), &mut io)
            .is_err()
    );
    io.truncating = true;
    member.do_work(at(ms + 1), &mut io).expect("no fault");
    member.do_work(at(ms + 2), &mut io).expect("no fault");
    assert_eq!(member.election_state(), ElectionState::Canvass);
    io.truncating = false;
    io.clear();

    member
        .on_message(
            at(ms + 3),
            &leadership_term(2, 1, 96, 96, 0, [1, 96, -1]),
            &mut io,
        )
        .expect("no fault");
    assert_eq!(member.truncation_owed(), -1);
    assert_eq!(member.append_position(), 64);
    assert_eq!(io.count(|call| matches!(call, Call::TruncateLog(_))), 1);
    assert_eq!(
        io.first(|call| matches!(call, Call::TruncateLog(_))),
        io.first(|call| *call == Call::TruncateLog(64))
    );
    assert_eq!(
        member.election_state(),
        ElectionState::FollowerLogReplication
    );
    assert_eq!(member.leader_member_id(), 2);
}

/// A member that went on to canvass while the truncation it owes was under way goes
/// back to init when the truncation, asked again before a new leadership term, faults:
/// there it reads and sends nothing while the fault lasts, an election failed again,
/// and the truncation is still counted once.
#[test]
fn a_member_that_canvasses_while_it_owes_a_truncation_goes_back_to_init_at_a_fault() {
    let mut io = Stub::new();
    let (mut member, ms) = holding_128_of_term_zero(&mut io);
    io.truncate_faults = 1;
    assert!(
        member
            .on_message(at(ms), &term_one_from_64(), &mut io)
            .is_err()
    );
    let failed = member.counts().elections_failed;
    io.truncating = true;
    member.do_work(at(ms + 1), &mut io).expect("no fault");
    member.do_work(at(ms + 2), &mut io).expect("no fault");
    assert_eq!(member.election_state(), ElectionState::Canvass);

    io.truncating = false;
    io.truncate_faults = 1;
    assert!(
        member
            .on_message(at(ms + 3), &term_one_from_64(), &mut io)
            .is_err()
    );
    assert_eq!(member.election_state(), ElectionState::Init);
    assert_eq!(member.counts().elections_failed, failed + 1);
    assert_eq!(member.counts().truncations_faulted, 1);
    assert_eq!(member.truncation_owed(), 64);

    member.do_work(at(ms + 4), &mut io).expect("no fault");
    assert_eq!(member.truncation_owed(), -1);
    assert_eq!(member.append_position(), 64);
}
