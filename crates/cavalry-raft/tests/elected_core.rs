//! One core, driven by hand through an [`Io`] that remembers what it was asked: the
//! order of a member's durable writes and its reports, and what it does when a write
//! fails (`spec/cluster.md`, "The log"; ADR 0060, ADR 0100).
//!
//! [`Io`]: cavalry_raft::Io

mod common;

use cavalry_proto::cluster::{Message, Template};
use cavalry_raft::{Consensus, ElectionState, Role};
use common::{Call, Stub, at, canvass_position, new_leadership_term};

/// A fault at the record of terms leaves a member that has won its ballot leading
/// nothing: no term led, no log added, nothing announced. The next cycle records the
/// same term, not the one after it, and the member leads it. Its candidate term was
/// stored once, before it asked for a vote. It canvasses, stores its term, records it,
/// adds its log and announces the term, in that order.
#[test]
fn a_record_that_fails_leaves_the_winner_leading_nothing_and_the_same_term_to_record() {
    let mut io = Stub::new();
    let mut member = Consensus::with_election(&[0, 1, 2], 0, 1).expect("a member");
    member.do_work(at(1), &mut io).expect("no fault");
    for other in [1, 2] {
        member
            .on_message(at(2), &canvass_position(other, -1, 0), &mut io)
            .expect("no fault");
    }
    member.do_work(at(3), &mut io).expect("no fault");
    assert_eq!(member.election_state(), ElectionState::Nominate);
    member.do_work(at(503), &mut io).expect("no fault");
    assert_eq!(member.election_state(), ElectionState::CandidateBallot);
    for voter in [1, 2] {
        let vote = Message::Vote {
            candidate_term_id: 0,
            log_leadership_term_id: -1,
            log_position: 0,
            candidate_member_id: 0,
            follower_member_id: voter,
            vote: true,
        };
        member
            .on_message(at(504), &vote, &mut io)
            .expect("no fault");
    }

    io.fail_record = true;
    let mut faults = 0;
    for ms in 505..510 {
        if member.do_work(at(ms), &mut io) == Err("the terms file cannot be written") {
            faults += 1;
        }
    }
    assert!(faults > 0, "the record of terms was asked for and failed");
    assert_eq!(member.counts().terms_led, 0);
    assert_eq!(io.count(|call| matches!(call, Call::AddLog(..))), 0);
    assert_eq!(
        io.count(|call| matches!(
            call,
            Call::Send {
                template: Template::NewLeadershipTerm,
                ..
            }
        )),
        0,
        "no term is announced before it is recorded"
    );

    io.fail_record = false;
    for ms in 510..515 {
        member.do_work(at(ms), &mut io).expect("no fault");
    }
    assert_eq!(member.role(), Role::Leader);
    assert_eq!(member.leadership_term_id(), 0);
    assert_eq!(member.counts().terms_led, 1);
    let recorded = io
        .first(|call| *call == Call::RecordTerm(0, 0))
        .expect("the term is recorded");
    let added = io
        .first(|call| *call == Call::AddLog(0, 0))
        .expect("the log is added");
    let canvassed = io
        .first(|call| {
            matches!(
                call,
                Call::Send {
                    template: Template::CanvassPosition,
                    ..
                }
            )
        })
        .expect("it canvassed");
    let stored = io
        .first(|call| *call == Call::StoreCandidateTerm(0))
        .expect("the term is stored");
    let announced = io
        .first(|call| {
            matches!(
                call,
                Call::Send {
                    template: Template::NewLeadershipTerm,
                    ..
                }
            )
        })
        .expect("the term is announced");
    assert!(
        canvassed < stored && stored < recorded && recorded < added && added < announced,
        "{:?}",
        io.calls
    );
    assert_eq!(
        io.count(|call| matches!(call, Call::RecordTerm(term, _) if *term != 0)),
        0,
        "no other term was recorded"
    );
    assert_eq!(io.count(|call| *call == Call::StoreCandidateTerm(0)), 1);
    assert_eq!(
        io.count(|call| matches!(call, Call::StoreCandidateTerm(term) if *term != 0)),
        0
    );
}

/// A follower records its leader's term before it reports a position for it, joins
/// the log before it looks for the image, and reports only once the image has joined
/// at its append position. In init it reads nothing. Its leader is whichever member
/// sent the new leadership term, here member 1, and it stores no candidate term for
/// a term it did not vote in.
#[test]
fn a_follower_records_the_term_before_it_reports_for_it() {
    let mut io = Stub::new();
    let mut follower = Consensus::with_election(&[0, 1, 2], 2, 7).expect("a member");

    follower
        .on_message(at(0), &new_leadership_term(1, 0), &mut io)
        .expect("no fault");
    assert_eq!(follower.election_state(), ElectionState::Init);

    follower.do_work(at(1), &mut io).expect("no fault");
    follower.do_work(at(2), &mut io).expect("no fault");
    assert_eq!(follower.election_state(), ElectionState::Canvass);
    assert_eq!(
        io.count(|call| matches!(
            call,
            Call::Send {
                template: Template::CanvassPosition,
                ..
            }
        )),
        2,
        "one canvass position to each other member"
    );

    follower
        .on_message(at(5), &new_leadership_term(1, 0), &mut io)
        .expect("no fault");
    assert_eq!(follower.election_state(), ElectionState::FollowerReplay);
    assert_eq!(follower.leader_member_id(), 1);
    for ms in 6..=9 {
        follower.do_work(at(ms), &mut io).expect("no fault");
    }
    assert_eq!(follower.election_state(), ElectionState::FollowerLogAwait);
    assert_eq!(
        io.count(|call| matches!(
            call,
            Call::Send {
                template: Template::AppendPosition,
                ..
            }
        )),
        0,
        "no report before the image"
    );
    io.image = Some(0);
    for ms in 10..=12 {
        follower.do_work(at(ms), &mut io).expect("no fault");
    }
    assert_eq!(follower.election_state(), ElectionState::Closed);
    assert_eq!(follower.role(), Role::Follower);

    let recorded = io
        .first(|call| *call == Call::RecordTerm(0, 0))
        .expect("recorded");
    let joined = io.first(|call| *call == Call::JoinLog).expect("joined");
    let reported = io
        .first(|call| {
            *call
                == Call::Send {
                    to: 1,
                    template: Template::AppendPosition,
                }
        })
        .expect("reported");
    assert!(recorded < joined && joined < reported, "{:?}", io.calls);
    assert_eq!(
        io.count(|call| matches!(call, Call::StoreCandidateTerm(_))),
        0
    );

    // Its append position goes to the leader when it changes, and no oftener than
    // the leader heartbeat interval when it does not.
    io.clear();
    follower.do_work(at(13), &mut io).expect("no fault");
    assert_eq!(io.made, 0, "unchanged and not yet due");
    io.append_position = 4096;
    follower.do_work(at(14), &mut io).expect("no fault");
    assert_eq!(
        io.first(|call| *call
            == Call::Send {
                to: 1,
                template: Template::AppendPosition
            }),
        Some(0)
    );
    assert_eq!(follower.append_position(), 4096);
}
