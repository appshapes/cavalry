//! What the two gauges of ADR 0077 hold, one core at a time, driven by hand
//! (`spec/cluster.md`, "Counters"; ADR 0102): `cluster.leader_member_id`, the member
//! this one leads as or follows and −1 while it has none, and
//! `cluster.election_cause`, why its last election began. The causes a leader has,
//! a term above its own, a quorum lost and an election that failed, are asserted
//! where those elections are made, in `step_down.rs`.

mod common;

use cavalry_proto::cluster::Message;
use cavalry_raft::{Consensus, ElectionCause, ElectionState, Role};
use common::{Stub, at, canvass_position, follow_term, leadership_term, new_leadership_term};

/// A member that started has no leader and cause 0, on both paths, in init and in
/// canvass. Where the leader is appointed the core knows its id from the start, and
/// the gauge still reads −1: the member follows nobody yet.
#[test]
fn a_member_that_started_has_no_leader_and_the_cause_is_its_start() {
    let appointed = Consensus::new(&[0, 1, 2], 1).expect("a member");
    assert_eq!(appointed.leader_member_id(), 0);
    let electing = Consensus::with_election(&[0, 1, 2], 1, 7).expect("a member");
    assert_eq!(electing.leader_member_id(), -1);
    for mut member in [appointed, electing] {
        let mut io = Stub::new();
        assert_eq!(member.election_state(), ElectionState::Init);
        assert_eq!(member.leader_gauge(), -1);
        assert_eq!(member.election_cause(), ElectionCause::Started);
        assert_eq!(member.election_cause().code(), 0);
        member.do_work(at(1), &mut io).expect("no fault");
        assert_eq!(member.election_state(), ElectionState::Canvass);
        assert_eq!(member.leader_gauge(), -1);
        assert_eq!(member.election_cause(), ElectionCause::Started);
    }
}

/// A follower names its leader from the new leadership term it takes, through its
/// follower states and while its election is closed, and names none from the init of
/// its next election. The cause of that election is the leader heartbeat timeout,
/// code 1, and stays the cause while the election runs and after it closes. Both
/// paths.
#[test]
fn a_follower_names_its_leader_until_its_next_election_and_the_cause_is_the_heartbeat() {
    for mut member in [
        Consensus::new(&[0, 1, 2], 1).expect("a member"),
        Consensus::with_election(&[0, 1, 2], 1, 7).expect("a member"),
    ] {
        let mut io = Stub::new();
        member.do_work(at(1), &mut io).expect("no fault");
        member
            .on_message(at(2), &new_leadership_term(0, 0), &mut io)
            .expect("no fault");
        assert_eq!(member.election_state(), ElectionState::FollowerReplay);
        assert_eq!(member.leader_gauge(), 0, "from the term it takes");
        member.do_work(at(3), &mut io).expect("no fault");
        member.do_work(at(4), &mut io).expect("no fault");
        io.image = Some(0);
        member.do_work(at(5), &mut io).expect("no fault");
        member.do_work(at(6), &mut io).expect("no fault");
        assert_eq!(member.election_state(), ElectionState::Closed);
        assert_eq!(member.leader_gauge(), 0);
        assert_eq!(member.election_cause(), ElectionCause::Started);

        // Nothing from the leader for the leader heartbeat timeout.
        member.do_work(at(10_006), &mut io).expect("no fault");
        assert_eq!(member.election_state(), ElectionState::Init);
        assert_eq!(member.leader_gauge(), -1, "none from init");
        assert_eq!(
            member.election_cause(),
            ElectionCause::LeaderHeartbeatTimeout
        );
        assert_eq!(member.election_cause().code(), 1);
        assert_eq!(member.counts().leader_heartbeat_timeouts, 1);
        io.image = None;
        member.do_work(at(10_007), &mut io).expect("no fault");
        assert_eq!(member.election_state(), ElectionState::Canvass);
        assert_eq!(member.leader_gauge(), -1);

        // It follows the same leader's next term: the cause is still why this
        // election began, while it runs and once it has closed.
        let term_one = leadership_term(0, 1, 0, 0, 0, [1, 0, -1]);
        follow_term(&mut member, &mut io, 10_008, &term_one, 0);
        assert_eq!(member.leader_gauge(), 0);
        assert_eq!(
            member.election_cause(),
            ElectionCause::LeaderHeartbeatTimeout
        );
    }
}

/// A follower whose image of its leader's log is no longer available begins again
/// with cause 2. Both paths.
#[test]
fn a_follower_that_lost_the_image_of_the_log_has_cause_two() {
    for mut member in [
        Consensus::new(&[0, 1, 2], 1).expect("a member"),
        Consensus::with_election(&[0, 1, 2], 1, 7).expect("a member"),
    ] {
        let mut io = Stub::new();
        member.do_work(at(1), &mut io).expect("no fault");
        let closed = follow_term(&mut member, &mut io, 2, &new_leadership_term(0, 0), 0);
        io.image = None;
        member.do_work(at(closed + 1), &mut io).expect("no fault");
        assert_eq!(member.election_state(), ElectionState::Init);
        assert_eq!(member.election_cause(), ElectionCause::LogImageLost);
        assert_eq!(member.election_cause().code(), 2);
        assert_eq!(member.counts().log_images_lost, 1);
        assert_eq!(member.counts().leader_heartbeat_timeouts, 0);
        assert_eq!(member.leader_gauge(), -1);
    }
}

/// A member that elects names no leader in nominate or in its ballot, and names
/// itself from the cycle it leads in. Member 0 of three, both others level with it
/// in canvass and both voting for it.
#[test]
fn a_candidate_names_no_leader_until_it_leads_and_then_names_itself() {
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
    assert_eq!(member.leader_gauge(), -1);
    member.do_work(at(503), &mut io).expect("no fault");
    assert_eq!(member.election_state(), ElectionState::CandidateBallot);
    assert_eq!(member.role(), Role::Candidate);
    assert_eq!(member.leader_gauge(), -1);
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
    member.do_work(at(505), &mut io).expect("no fault");
    assert_eq!(member.election_state(), ElectionState::LeaderLogReplication);
    assert_eq!(
        member.leader_gauge(),
        0,
        "itself, from the cycle it leads in"
    );
    assert_eq!(member.election_cause(), ElectionCause::Started);
}

/// A member that votes names no leader in follower ballot, and names the candidate
/// once it takes that candidate's new leadership term.
#[test]
fn a_voter_names_no_leader_in_its_ballot() {
    let mut io = Stub::new();
    let mut member = Consensus::with_election(&[0, 1, 2], 1, 7).expect("a member");
    member.do_work(at(1), &mut io).expect("no fault");
    let request = Message::RequestVote {
        log_leadership_term_id: -1,
        log_position: 0,
        candidate_term_id: 0,
        candidate_member_id: 2,
        protocol_version: 0,
    };
    member
        .on_message(at(2), &request, &mut io)
        .expect("no fault");
    assert_eq!(member.election_state(), ElectionState::FollowerBallot);
    assert_eq!(member.leader_gauge(), -1);
    member
        .on_message(at(3), &new_leadership_term(2, 0), &mut io)
        .expect("no fault");
    assert_eq!(member.election_state(), ElectionState::FollowerReplay);
    assert_eq!(member.leader_gauge(), 2);
}

/// The appointed path has four of the six causes: its member starts, loses its
/// leader's heartbeat, loses the image, or fails its election. Here the last: a
/// follower that is behind its appointed leader's log after follower replay fails
/// its election, which this path does where the election's follower would catch up,
/// and its next election has cause 5. The other two, a quorum lost and a term above
/// its own, begin no election on this path (`step_down.rs`).
#[test]
fn an_election_that_failed_is_the_cause_of_the_next_on_the_appointed_path() {
    let mut io = Stub::new();
    let mut member = Consensus::new(&[0, 1, 2], 1).expect("a member");
    member.do_work(at(1), &mut io).expect("no fault");
    // The leader's log is at 64 and this member holds nothing.
    member
        .on_message(at(2), &new_leadership_term(0, 64), &mut io)
        .expect("no fault");
    let mut ms = 3;
    while member.election_state() != ElectionState::Init && ms < 20 {
        member.do_work(at(ms), &mut io).expect("no fault");
        ms += 1;
    }
    assert_eq!(member.election_state(), ElectionState::Init);
    assert_eq!(member.counts().elections_failed, 1);
    assert_eq!(member.election_cause(), ElectionCause::ElectionFailed);
    assert_eq!(member.election_cause().code(), 5);
    assert_eq!(member.leader_gauge(), -1);
}
