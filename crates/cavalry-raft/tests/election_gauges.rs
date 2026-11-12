//! What the two gauges of ADR 0077 hold, one core at a time, driven by hand
//! (`spec/cluster.md`, "Counters"; ADR 0102): `cluster.leader_member_id`, the member
//! this one leads as or follows and −1 while it has none, and
//! `cluster.election_cause`, why its last election began. The causes of the
//! elections a leader begins, a term above its own, a quorum lost and an election of
//! its own that failed, are asserted where those elections are made, in
//! `step_down.rs`, with a follower's cause 4; a follower's failed election, cause 5,
//! is here.

mod common;

use cavalry_proto::cluster::Message;
use cavalry_raft::{Consensus, ElectionCause, ElectionState, Role};
use common::{
    Stub, at, canvass_position, follow_term, leadership_term, lose_the_leader, new_leadership_term,
};

/// A member that started has no leader and cause 0, in init and in canvass.
#[test]
fn a_member_that_started_has_no_leader_and_the_cause_is_its_start() {
    let mut member = Consensus::with_election(&[0, 1, 2], 1, 7).expect("a member");
    assert_eq!(member.leader_member_id(), -1);
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

/// A follower names its leader from the new leadership term it takes, through its
/// follower states and while its election is closed, and names none from the init of
/// its next election. The cause of that election is the leader heartbeat timeout,
/// code 1, and stays the cause while the election runs and after it closes.
#[test]
fn a_follower_names_its_leader_until_its_next_election_and_the_cause_is_the_heartbeat() {
    let mut member = Consensus::with_election(&[0, 1, 2], 1, 7).expect("a member");
    let mut io = Stub::new();
    member.do_work(at(1), &mut io).expect("no fault");
    member
        .on_message(at(2), &new_leadership_term(0, 0), &mut io)
        .expect("no fault");
    assert_eq!(member.election_state(), ElectionState::FollowerReplay);
    assert_eq!(member.leader_gauge(), 0, "from the term it takes");
    // Through each follower's state it passes on its way to closed.
    let mut passed = vec![member.election_state()];
    for ms in 3..=6 {
        if ms == 5 {
            io.image = Some(0);
        }
        member.do_work(at(ms), &mut io).expect("no fault");
        assert_eq!(member.leader_gauge(), 0, "in {:?}", member.election_state());
        if passed.last() != Some(&member.election_state()) {
            passed.push(member.election_state());
        }
    }
    assert_eq!(
        passed,
        [
            ElectionState::FollowerReplay,
            ElectionState::FollowerLogInit,
            ElectionState::FollowerLogAwait,
            ElectionState::FollowerReady,
            ElectionState::Closed,
        ]
    );
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

/// A follower whose image of its leader's log is no longer available begins again
/// with cause 2.
#[test]
fn a_follower_that_lost_the_image_of_the_log_has_cause_two() {
    let mut member = Consensus::with_election(&[0, 1, 2], 1, 7).expect("a member");
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

/// A follower's election that failed is the cause of its next, code 5. Here the
/// image of its leader's log joins past the end of its own log, in follower log
/// await: the member counts the mismatch, fails its election and names no leader.
#[test]
fn a_followers_election_that_failed_is_the_cause_of_its_next() {
    let mut io = Stub::new();
    let mut member = Consensus::with_election(&[0, 1, 2], 1, 7).expect("a member");
    member.do_work(at(1), &mut io).expect("no fault");
    member
        .on_message(at(2), &new_leadership_term(0, 0), &mut io)
        .expect("no fault");
    member.do_work(at(3), &mut io).expect("no fault");
    member.do_work(at(4), &mut io).expect("no fault");
    assert_eq!(member.election_state(), ElectionState::FollowerLogAwait);
    assert_eq!(member.leader_gauge(), 0);
    io.image = Some(64);
    member.do_work(at(5), &mut io).expect("no fault");
    assert_eq!(member.election_state(), ElectionState::Init);
    assert_eq!(member.counts().join_position_mismatches, 1);
    assert_eq!(member.counts().elections_failed, 1);
    assert_eq!(member.election_cause(), ElectionCause::ElectionFailed);
    assert_eq!(member.election_cause().code(), 5);
    assert_eq!(member.leader_gauge(), -1);
}

/// A member that replicates a term it missed names its leader while it replicates,
/// in follower log replication, and names none in the canvass it goes back to when
/// the replication has ended. That canvass begins no election: the cause stays the
/// one its election began with, and no election is counted. The next new leadership
/// term names the leader again.
#[test]
fn a_member_names_its_leader_while_it_replicates_and_none_in_the_canvass_after() {
    let mut io = Stub::new();
    let mut member = Consensus::with_election(&[0, 1, 2], 0, 1).expect("a member");
    member.do_work(at(1), &mut io).expect("no fault");
    let closed = follow_term(&mut member, &mut io, 2, &new_leadership_term(1, 0), 0);
    let ms = lose_the_leader(&mut member, &mut io, closed, 32);
    assert_eq!(member.leader_gauge(), -1);
    assert_eq!(
        member.election_cause(),
        ElectionCause::LeaderHeartbeatTimeout
    );
    let elections = member.counts().elections;

    // Member 2 leads term 1 from 64, and this member lacks 32 bytes of term 0.
    member
        .on_message(
            at(ms),
            &leadership_term(2, 1, 64, 64, 0, [1, 64, -1]),
            &mut io,
        )
        .expect("no fault");
    assert_eq!(
        member.election_state(),
        ElectionState::FollowerLogReplication
    );
    assert_eq!(member.leader_gauge(), 2, "the leader it replicates from");
    member.do_work(at(ms + 1), &mut io).expect("no fault");
    io.catchup_image = Some(32);
    io.append_position = 48;
    member.do_work(at(ms + 2), &mut io).expect("no fault");
    assert_eq!(
        member.election_state(),
        ElectionState::FollowerLogReplication
    );
    assert_eq!(member.leader_gauge(), 2);

    // The replay reaches the stop, the leader's base: the member canvasses again.
    io.append_position = 64;
    member.do_work(at(ms + 3), &mut io).expect("no fault");
    assert_eq!(member.election_state(), ElectionState::Canvass);
    assert_eq!(member.leader_gauge(), -1, "none in the canvass after it");
    assert_eq!(
        member.election_cause(),
        ElectionCause::LeaderHeartbeatTimeout,
        "the election it was in"
    );
    assert_eq!(member.counts().elections, elections);
    assert_eq!(member.counts().elections_failed, 0);

    // Level with the leader's base: it takes the term and names the leader again.
    member
        .on_message(
            at(ms + 4),
            &leadership_term(2, 1, 64, 64, 1, [-1; 3]),
            &mut io,
        )
        .expect("no fault");
    assert_eq!(member.election_state(), ElectionState::FollowerReplay);
    assert_eq!(member.leader_gauge(), 2);
}
