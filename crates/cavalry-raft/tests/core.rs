//! One core at a time, driven by hand through an [`Io`] that remembers what it was
//! asked: the order of a member's durable writes and its reports, what it ignores, and
//! what it does when a write fails (`spec/cluster.md`, "The log" and "The appointed
//! leader"; ADR 0063).

mod common;

use cavalry_proto::cluster::{Message, Template};
use cavalry_raft::{Consensus, ElectionState, Refused, Role};
use common::{Call, Stub, append_position, at, canvass_position, new_leadership_term};

/// The appointed leader canvasses, and leads only once every member has reported a log
/// not ahead of its own. Then it stores the term it proposes and records it at its base
/// before it sends a word of it, and appends the term's first entry only once every
/// member has reported for the term.
#[test]
fn the_leader_canvasses_then_stores_and_records_its_term_before_it_announces_it() {
    let mut io = Stub::new();
    let mut leader = Consensus::new(&[0, 1, 2], 0).expect("a member");
    for ms in 1..=10 {
        leader.do_work(at(ms), &mut io).expect("no fault");
    }
    // One member's report is not every member's: it canvasses on, a follower still.
    leader
        .on_message(at(11), &canvass_position(1, -1, 0), &mut io)
        .expect("no fault");
    leader.do_work(at(12), &mut io).expect("no fault");
    assert_eq!(leader.election_state(), ElectionState::Canvass);
    assert_eq!(leader.role(), Role::Follower);
    assert_eq!(io.count(|c| matches!(c, Call::StoreCandidateTerm(_))), 0);

    leader
        .on_message(at(13), &canvass_position(2, -1, 0), &mut io)
        .expect("no fault");
    for ms in 14..=24 {
        leader.do_work(at(ms), &mut io).expect("no fault");
    }
    assert_eq!(leader.role(), Role::Leader);
    assert_eq!(leader.election_state(), ElectionState::LeaderReady);
    assert_eq!(leader.leadership_term_id(), 0);

    let canvassed = io
        .first(|c| {
            matches!(
                c,
                Call::Send {
                    template: Template::CanvassPosition,
                    ..
                }
            )
        })
        .expect("canvassed");
    let stored = io
        .first(|c| *c == Call::StoreCandidateTerm(0))
        .expect("stored");
    let recorded = io
        .first(|c| *c == Call::RecordTerm(0, 0))
        .expect("recorded");
    let log = io.first(|c| *c == Call::AddLog(0, 0)).expect("the log");
    let announced = io
        .first(|c| {
            matches!(
                c,
                Call::Send {
                    template: Template::NewLeadershipTerm,
                    ..
                }
            )
        })
        .expect("announced");
    assert!(
        canvassed < stored && stored < recorded && recorded < log && log < announced,
        "{:?}",
        io.calls
    );
    assert_eq!(
        io.count(|c| matches!(c, Call::Append(_))),
        0,
        "no entry before the reports"
    );

    // A report under an id no member has, and one under the leader's own id, count for
    // nothing: with one follower in, the leader waits for the other.
    leader
        .on_message(at(31), &append_position(7, 0), &mut io)
        .expect("no fault");
    leader
        .on_message(at(31), &append_position(0, 0), &mut io)
        .expect("no fault");
    leader
        .on_message(at(31), &append_position(1, 0), &mut io)
        .expect("no fault");
    leader.do_work(at(32), &mut io).expect("no fault");
    assert_eq!(leader.election_state(), ElectionState::LeaderReady);
    assert_eq!(
        leader.offer(at(32), b"early", &mut io).expect("no fault"),
        Err(Refused::Election)
    );

    // Back-pressure on the term's first entry keeps the election open, and the entry
    // is appended once.
    leader
        .on_message(at(33), &append_position(2, 0), &mut io)
        .expect("no fault");
    io.back_pressured = true;
    leader.do_work(at(34), &mut io).expect("no fault");
    assert_eq!(leader.election_state(), ElectionState::LeaderReady);
    io.back_pressured = false;
    leader.do_work(at(35), &mut io).expect("no fault");
    leader.do_work(at(36), &mut io).expect("no fault");
    assert_eq!(leader.election_state(), ElectionState::Closed);
    assert_eq!(
        io.count(|c| *c == Call::Append(Template::NewLeadershipTermEvent)),
        1
    );
    assert_eq!(
        leader.offer(at(37), b"now", &mut io).expect("no fault"),
        Ok(())
    );
    assert_eq!(
        io.count(|c| *c == Call::Append(Template::SessionMessage)),
        1
    );
}

/// A fault at the record of terms leaves the appointed leader in canvass, leading
/// nothing, and the next cycle proposes the same term again, not the one after it.
#[test]
fn a_write_that_fails_leaves_the_leader_in_canvass_with_the_same_term_to_propose() {
    let mut io = Stub::new();
    let mut leader = Consensus::new(&[0, 1, 2], 0).expect("a member");
    leader.do_work(at(1), &mut io).expect("no fault");
    for follower in [1, 2] {
        leader
            .on_message(at(2), &canvass_position(follower, -1, 0), &mut io)
            .expect("no fault");
    }
    io.fail_record = true;
    assert_eq!(
        leader.do_work(at(3), &mut io),
        Err("the terms file cannot be written")
    );
    assert_eq!(leader.election_state(), ElectionState::Canvass);
    assert_eq!(leader.role(), Role::Follower);
    assert_eq!(leader.leadership_term_id(), -1);
    assert_eq!(leader.counts().terms_led, 0);

    io.fail_record = false;
    leader.do_work(at(4), &mut io).expect("no fault");
    assert_eq!(leader.election_state(), ElectionState::LeaderLogReplication);
    assert_eq!(leader.leadership_term_id(), 0);
    assert_eq!(io.count(|c| *c == Call::StoreCandidateTerm(0)), 2);
    assert_eq!(
        io.count(|c| matches!(c, Call::StoreCandidateTerm(term) if *term != 0)),
        0
    );
    assert_eq!(leader.counts().terms_led, 1);
}

/// The hole a leader that skipped canvass would leave: an appointed leader with an
/// empty log, its log directory lost, among members that hold term 0 up to a position
/// it no longer has. Every report it reads is of a log ahead of its own, so it is no
/// candidate, unanimous or at the deadline: it proposes nothing and announces nothing,
/// however long it canvasses.
#[test]
fn an_appointed_leader_whose_log_is_behind_a_members_never_leads() {
    let mut io = Stub::new();
    let mut leader = Consensus::new(&[0, 1, 2], 0).expect("a member");
    // Two minutes, past the startup canvass timeout, with both followers reporting
    // their logs every 100 ms.
    for ms in (1..=120_000u64).step_by(100) {
        for follower in [1, 2] {
            leader
                .on_message(at(ms), &canvass_position(follower, 0, 4096), &mut io)
                .expect("no fault");
        }
        leader.do_work(at(ms), &mut io).expect("no fault");
    }
    assert_eq!(leader.election_state(), ElectionState::Canvass);
    assert_eq!(leader.role(), Role::Follower);
    assert_eq!(leader.leadership_term_id(), -1);
    assert_eq!(leader.counts().terms_led, 0);
    assert_eq!(
        io.count(|c| !matches!(
            c,
            Call::Send {
                template: Template::CanvassPosition,
                ..
            }
        )),
        0
    );

    // One member ahead and one silent is no quorum of logs not ahead, at any deadline;
    // one member level and one silent is, once the deadline has passed.
    let mut io = Stub::new();
    let mut leader = Consensus::new(&[0, 1, 2], 0).expect("a member");
    for ms in (1..=59_901u64).step_by(100) {
        leader
            .on_message(at(ms), &canvass_position(1, -1, 0), &mut io)
            .expect("no fault");
        leader.do_work(at(ms), &mut io).expect("no fault");
    }
    assert_eq!(
        leader.election_state(),
        ElectionState::Canvass,
        "before the deadline"
    );
    leader.do_work(at(60_002), &mut io).expect("no fault");
    assert_eq!(leader.election_state(), ElectionState::LeaderLogReplication);
    assert_eq!(leader.role(), Role::Leader);
}

/// A follower records the leader's term before it reports a position for it, joins
/// the log before it looks for the image, and reports only from an image that joined
/// at its append position.
#[test]
fn a_follower_records_the_term_before_it_reports_for_it() {
    let mut io = Stub::new();
    let mut follower = Consensus::new(&[0, 1, 2], 2).expect("a member");

    // In init a member reads nothing.
    follower
        .on_message(at(0), &new_leadership_term(0, 0), &mut io)
        .expect("no fault");
    assert_eq!(follower.election_state(), ElectionState::Init);
    assert_eq!(io.made, 0);

    follower.do_work(at(1), &mut io).expect("no fault");
    follower.do_work(at(2), &mut io).expect("no fault");
    assert_eq!(follower.election_state(), ElectionState::Canvass);
    assert_eq!(
        io.count(|c| matches!(
            c,
            Call::Send {
                template: Template::CanvassPosition,
                ..
            }
        )),
        2,
        "one canvass position to each other member"
    );

    // A new leadership term from a member that is not the appointed leader is not its
    // leader's, whatever it says.
    follower
        .on_message(at(3), &new_leadership_term(1, 0), &mut io)
        .expect("no fault");
    assert_eq!(follower.election_state(), ElectionState::Canvass);
    assert_eq!(io.count(|c| matches!(c, Call::RecordTerm(..))), 0);

    // The four messages no member of this version sends are ignored.
    let before = io.made;
    for message in [
        Message::RequestVote {
            log_leadership_term_id: 3,
            log_position: 4096,
            candidate_term_id: 4,
            candidate_member_id: 1,
            protocol_version: 0,
        },
        Message::Vote {
            candidate_term_id: 4,
            log_leadership_term_id: 3,
            log_position: 4096,
            candidate_member_id: 2,
            follower_member_id: 1,
            vote: true,
        },
        Message::CatchupPosition {
            leadership_term_id: 0,
            log_position: 0,
            follower_member_id: 1,
            catchup_endpoint: b"localhost:9000",
        },
        Message::StopCatchup {
            leadership_term_id: 0,
            follower_member_id: 2,
        },
    ] {
        follower
            .on_message(at(4), &message, &mut io)
            .expect("no fault");
    }
    assert_eq!(follower.election_state(), ElectionState::Canvass);
    assert_eq!(io.made, before, "nothing was done for them");
    assert_eq!(follower.counts().elections_failed, 0);

    // The leader's: the term is recorded, then the log joined, then the image awaited.
    follower
        .on_message(at(5), &new_leadership_term(0, 0), &mut io)
        .expect("no fault");
    assert_eq!(follower.election_state(), ElectionState::FollowerReplay);
    for ms in 6..=9 {
        follower.do_work(at(ms), &mut io).expect("no fault");
    }
    assert_eq!(follower.election_state(), ElectionState::FollowerLogAwait);
    assert_eq!(
        io.count(|c| matches!(
            c,
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
        .first(|c| *c == Call::RecordTerm(0, 0))
        .expect("recorded");
    let joined = io.first(|c| *c == Call::JoinLog).expect("joined");
    let reported = io
        .first(|c| {
            *c == Call::Send {
                to: 0,
                template: Template::AppendPosition,
            }
        })
        .expect("reported");
    assert!(recorded < joined && joined < reported, "{:?}", io.calls);
    assert_eq!(io.count(|c| matches!(c, Call::StoreCandidateTerm(_))), 0);

    // Its append position goes to the leader when it changes, and no oftener than the
    // leader heartbeat interval when it does not.
    io.clear();
    follower.do_work(at(13), &mut io).expect("no fault");
    assert_eq!(io.made, 0, "unchanged and not yet due");
    io.append_position = 4096;
    follower.do_work(at(14), &mut io).expect("no fault");
    assert_eq!(
        io.first(|c| *c
            == Call::Send {
                to: 0,
                template: Template::AppendPosition
            }),
        Some(0)
    );
    assert_eq!(follower.append_position(), 4096);
}
