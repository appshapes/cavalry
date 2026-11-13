//! One core, driven by hand through an [`Io`] that remembers what it was asked: the
//! order of a member's durable writes and its reports, and what it does when a write
//! fails (`spec/cluster.md`, "The log"; ADR 0060, ADR 0100).
//!
//! [`Io`]: cavalry_raft::Io

mod common;

use cavalry_proto::cluster::{Message, Template};
use cavalry_raft::{Consensus, ElectionState, Refused, Role};
use common::{
    Call, Stub, append_position, at, canvass_position, leadership_term, new_leadership_term,
};

/// Member 0 of three, which elects, taken by hand to the cycle after it has both
/// votes of its ballot for term 0: it canvassed, heard both others with logs level
/// with its own, nominated itself after its delay and asked for votes. Returns the
/// member and the next millisecond.
fn winner(io: &mut Stub) -> (Consensus, u64) {
    let mut member = Consensus::with_election(&[0, 1, 2], 0, 1).expect("a member");
    member.do_work(at(1), io).expect("no fault");
    for other in [1, 2] {
        member
            .on_message(at(2), &canvass_position(other, -1, 0), io)
            .expect("no fault");
    }
    member.do_work(at(3), io).expect("no fault");
    assert_eq!(member.election_state(), ElectionState::Nominate);
    member.do_work(at(503), io).expect("no fault");
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
        member.on_message(at(504), &vote, io).expect("no fault");
    }
    (member, 505)
}

/// The first call that sends a message of `template`.
fn first_sent(io: &Stub, template: Template) -> Option<usize> {
    io.first(|call| matches!(call, Call::Send { template: sent, .. } if *sent == template))
}

/// A member that wins its ballot stores its candidate term before it asks for a
/// vote, and records the term and adds its log before it sends a word of it. It
/// appends the term's first entry only once a quorum has reported for the term, its
/// own log among them: a report under an id no member has, and one under the
/// leader's own id, count for nothing. Back pressure on the first entry keeps the
/// election open, and the entry is appended once.
#[test]
fn a_winner_records_its_term_before_it_announces_it_and_appends_its_first_entry_once() {
    let mut io = Stub::new();
    let (mut leader, mut ms) = winner(&mut io);
    while leader.election_state() != ElectionState::LeaderReady {
        assert!(ms < 600, "leader ready was not reached: {:?}", io.calls);
        leader.do_work(at(ms), &mut io).expect("no fault");
        ms += 1;
    }
    assert_eq!(leader.role(), Role::Leader);
    assert_eq!(leader.leadership_term_id(), 0);

    let stored = io
        .first(|call| *call == Call::StoreCandidateTerm(0))
        .expect("stored");
    let asked = first_sent(&io, Template::RequestVote).expect("asked for a vote");
    let recorded = io
        .first(|call| *call == Call::RecordTerm(0, 0))
        .expect("recorded");
    let log = io
        .first(|call| *call == Call::AddLog(0, 0))
        .expect("the log");
    let announced = first_sent(&io, Template::NewLeadershipTerm).expect("announced");
    assert!(
        stored < asked && asked < recorded && recorded < log && log < announced,
        "{:?}",
        io.calls
    );
    assert_eq!(
        io.count(|call| matches!(call, Call::Append(_))),
        0,
        "no entry before a quorum's reports"
    );

    // A report under an id no member has, and one under the leader's own id, count
    // for nothing: the leader's own log is no quorum of three.
    leader
        .on_message(at(ms), &append_position(7, 0), &mut io)
        .expect("no fault");
    leader
        .on_message(at(ms), &append_position(0, 0), &mut io)
        .expect("no fault");
    leader.do_work(at(ms + 1), &mut io).expect("no fault");
    assert_eq!(leader.election_state(), ElectionState::LeaderReady);
    assert_eq!(
        leader
            .offer(at(ms + 1), b"early", &mut io)
            .expect("no fault"),
        Err(Refused::Election)
    );

    // One follower's report makes the quorum. Back pressure on the term's first
    // entry keeps the election open, and the entry is appended once.
    leader
        .on_message(at(ms + 2), &append_position(1, 0), &mut io)
        .expect("no fault");
    io.back_pressured = true;
    leader.do_work(at(ms + 3), &mut io).expect("no fault");
    assert_eq!(leader.election_state(), ElectionState::LeaderReady);
    io.back_pressured = false;
    leader.do_work(at(ms + 4), &mut io).expect("no fault");
    leader.do_work(at(ms + 5), &mut io).expect("no fault");
    assert_eq!(leader.election_state(), ElectionState::Closed);
    assert_eq!(
        io.count(|call| *call == Call::Append(Template::NewLeadershipTermEvent)),
        1
    );
    assert_eq!(
        leader.offer(at(ms + 6), b"now", &mut io).expect("no fault"),
        Ok(())
    );
    assert_eq!(
        io.count(|call| *call == Call::Append(Template::SessionMessage)),
        1
    );
}

/// A fault at the record of terms leaves a member that has won its ballot leading
/// nothing: no term led, no log added, nothing announced. The next cycle records the
/// same term, not the one after it, and the member leads it. Its candidate term was
/// stored once, before it asked for a vote. It canvasses, stores its term, records it,
/// adds its log and announces the term, in that order.
#[test]
fn a_record_that_fails_leaves_the_winner_leading_nothing_and_the_same_term_to_record() {
    let mut io = Stub::new();
    let (mut member, _) = winner(&mut io);

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
    let stored = io
        .first(|call| *call == Call::StoreCandidateTerm(0))
        .expect("stored");
    let asked = first_sent(&io, Template::RequestVote).expect("asked for a vote");
    assert!(stored < asked, "{:?}", io.calls);
    // Once recorded, the term is announced, and not before.
    let announced = first_sent(&io, Template::NewLeadershipTerm).expect("announced");
    assert!(recorded < announced, "{:?}", io.calls);
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
    assert_eq!(io.made, 0, "in init it reads nothing");

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

/// A member whose elections fail one after another sends one round of canvass
/// positions every election status interval, 100 ms, and no oftener (invariant 3): the
/// interval runs on from the last election and does not begin again with each. A
/// leader answers a canvass position and sends no new leadership term unasked, so
/// such a member fails one election an interval at most. Here each election fails
/// three milliseconds after the leader's answer: the image of the leader's log joins
/// past the end of the member's own log, in follower log await. The member is back
/// in canvass at once, and waits there for the rest of the interval. In a second it
/// sends ten rounds, two messages each, and fails ten elections, or one more of each.
#[test]
fn a_member_whose_elections_fail_one_after_another_canvasses_once_an_interval() {
    let mut io = Stub::new();
    let mut member = Consensus::with_election(&[0, 1, 2], 1, 7).expect("a member");
    io.image = Some(64);
    let mut canvass_positions = 0;
    let mut longest_wait_in_canvass = 0;
    let mut in_canvass_since = None;
    for ms in 1..=1_000 {
        member.do_work(at(ms), &mut io).expect("no fault");
        canvass_positions += io.count(|call| {
            matches!(
                call,
                Call::Send {
                    template: Template::CanvassPosition,
                    ..
                }
            )
        });
        let asked_the_leader = io
            .first(|call| {
                *call
                    == Call::Send {
                        to: 0,
                        template: Template::CanvassPosition,
                    }
            })
            .is_some();
        io.clear();
        if member.election_state() == ElectionState::Canvass {
            let since = *in_canvass_since.get_or_insert(ms);
            longest_wait_in_canvass = longest_wait_in_canvass.max(ms - since);
        } else {
            in_canvass_since = None;
        }
        if asked_the_leader {
            // The leader's answer to the log the member reported: no term before
            // its first election recorded term 0, and term 0 from then on.
            let answer = if member.counts().elections_failed == 0 {
                new_leadership_term(0, 0)
            } else {
                leadership_term(0, 0, 0, 0, 0, [-1, -1, -1])
            };
            member
                .on_message(at(ms), &answer, &mut io)
                .expect("no fault");
            assert_eq!(
                member.election_state(),
                ElectionState::FollowerReplay,
                "at {ms} ms the member takes its leader's answer"
            );
        }
    }
    let counts = member.counts();
    assert_eq!(
        counts.join_position_mismatches, counts.elections_failed,
        "{counts:?}"
    );
    assert!(
        (10..=11).contains(&counts.elections_failed),
        "one failed election an interval: {counts:?}"
    );
    assert!(
        (20..=22).contains(&canvass_positions),
        "{canvass_positions} canvass positions in a second; {counts:?}"
    );
    assert!(
        (90..100).contains(&longest_wait_in_canvass),
        "the member waited {longest_wait_in_canvass} ms in canvass for its next round"
    );
}
