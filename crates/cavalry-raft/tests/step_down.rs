//! The elections a member holds once it leads or follows, one core at a
//! time, driven by hand through an [`Io`] that remembers what it was asked
//! (`spec/cluster.md`, "The commit position", states 5 and 8, "Messages during an
//! election" and "Messages outside an election"; ADR 0082): a leader closes its
//! election on a quorum, steps down on a term above its own, fails its election on
//! one before it has closed, holds an election when its active members fall below a
//! quorum, and gives its log up in init; a follower holds an election on a term above
//! its own too.
//!
//! [`Io`]: cavalry_raft::Io

mod common;

use cavalry_proto::cluster::{Message, Template};
use cavalry_raft::{Consensus, ElectionCause, ElectionState, Refused, Role};
use common::{Call, Stub, append_position, at, canvass_position, new_leadership_term};

/// Member 0 of `[0, 1, 2]`, electing.
fn elector(seed: u64) -> Consensus {
    Consensus::with_election(&[0, 1, 2], 0, seed).expect("a member")
}

fn work(member: &mut Consensus, io: &mut Stub, ms: u64) {
    member.do_work(at(ms), io).expect("no fault");
}

fn read(member: &mut Consensus, io: &mut Stub, ms: u64, message: &Message<'_>) {
    member.on_message(at(ms), message, io).expect("no fault");
}

fn sends(io: &Stub, template: Template) -> usize {
    io.count(|call| matches!(call, Call::Send { template: sent, .. } if *sent == template))
}

/// Takes `member` from init to leader log replication as the leader of term 0, its log
/// added: both other members report in canvass, neither ahead, and both vote for it.
/// Returns the next millisecond.
fn to_leader_log_replication(member: &mut Consensus, io: &mut Stub) -> u64 {
    work(member, io, 1);
    for other in [1, 2] {
        read(member, io, 2, &canvass_position(other, -1, 0));
    }
    work(member, io, 3);
    assert_eq!(member.election_state(), ElectionState::Nominate);
    // The delay is below half the election timeout.
    work(member, io, 503);
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
        read(member, io, 504, &vote);
    }
    work(member, io, 505);
    assert_eq!(member.election_state(), ElectionState::LeaderLogReplication);
    assert_eq!(member.role(), Role::Leader);
    // The log publication of term 0, at base 0.
    work(member, io, 506);
    assert_eq!(io.count(|call| *call == Call::AddLog(0, 0)), 1);
    507
}

/// Takes the leader on to leader ready and holds it there: member 1 joined its log,
/// which took the leader past leader log replication, and then canvassed again, as a
/// member whose election failed does, so no member but the leader counts for the term.
/// Returns the next millisecond.
fn to_leader_ready(member: &mut Consensus, io: &mut Stub) -> u64 {
    let ms = to_leader_log_replication(member, io);
    read(member, io, ms, &append_position(1, 0));
    work(member, io, ms);
    assert_eq!(member.election_state(), ElectionState::LeaderReplay);
    read(member, io, ms, &canvass_position(1, -1, 0));
    for step in 1..4 {
        work(member, io, ms + step);
    }
    assert_eq!(member.election_state(), ElectionState::LeaderReady);
    ms + 4
}

/// Takes the leader on to closed: both members have joined its log at position 0.
/// Returns the next millisecond.
fn to_closed_leader(member: &mut Consensus, io: &mut Stub) -> u64 {
    let ms = to_leader_log_replication(member, io);
    for step in 0..6 {
        for other in [1, 2] {
            read(member, io, ms + step, &append_position(other, 0));
        }
        work(member, io, ms + step);
    }
    assert_eq!(member.election_state(), ElectionState::Closed);
    assert_eq!(member.role(), Role::Leader);
    assert_eq!(member.leadership_term_id(), 0);
    ms + 6
}

/// Takes `member` from init to closed as a follower of member 1's term 0. Returns the
/// next millisecond.
fn to_closed_follower(member: &mut Consensus, io: &mut Stub) -> u64 {
    work(member, io, 1);
    read(member, io, 2, &new_leadership_term(1, 0));
    work(member, io, 3);
    work(member, io, 4);
    io.image = Some(0);
    work(member, io, 5);
    work(member, io, 6);
    assert_eq!(member.election_state(), ElectionState::Closed);
    assert_eq!(member.role(), Role::Follower);
    assert_eq!(member.leader_member_id(), 1);
    7
}

/// `member`'s canvass position: a log of term 0 that ends at 0, and `candidate_term`
/// as the term it would propose from.
fn canvass_with(member: i32, log_term: i64, candidate_term: i64) -> Message<'static> {
    Message::CanvassPosition {
        log_leadership_term_id: log_term,
        log_position: 0,
        leadership_term_id: candidate_term,
        follower_member_id: member,
        protocol_version: 0,
    }
}

/// `candidate`'s request vote for `term`, from a log of term 0 that ends at 0.
fn request_vote(candidate: i32, term: i64) -> Message<'static> {
    Message::RequestVote {
        log_leadership_term_id: 0,
        log_position: 0,
        candidate_term_id: term,
        candidate_member_id: candidate,
        protocol_version: 0,
    }
}

/// `leader`'s commit position for `term`.
fn commit_position(leader: i32, term: i64) -> Message<'static> {
    Message::CommitPosition {
        leadership_term_id: term,
        log_position: 0,
        leader_member_id: leader,
    }
}

/// A leader whose election has closed answers a canvass position with neither term
/// above its own, and steps down on one with either term above it: the sender's
/// candidate term, a ballot the leader never saw, or its log's term.
#[test]
fn a_leader_steps_down_on_a_canvass_position_with_a_term_above_its_own() {
    // Neither term above: the leader answers with its term, and leads.
    let mut io = Stub::new();
    let mut leader = elector(1);
    let ms = to_closed_leader(&mut leader, &mut io);
    io.clear();
    read(&mut leader, &mut io, ms, &canvass_with(2, 0, 0));
    assert_eq!(sends(&io, Template::NewLeadershipTerm), 1);
    assert_eq!(leader.role(), Role::Leader);
    assert_eq!(leader.counts().leader_step_downs, 0);

    for (log_term, candidate_term) in [(0, 1), (1, 1)] {
        let mut io = Stub::new();
        let mut leader = elector(1);
        let ms = to_closed_leader(&mut leader, &mut io);
        let before = leader.counts();
        io.clear();
        read(
            &mut leader,
            &mut io,
            ms,
            &canvass_with(2, log_term, candidate_term),
        );
        // A follower in init from the same call, counted once, with no answer sent.
        assert_eq!(leader.election_state(), ElectionState::Init);
        assert_eq!(leader.role(), Role::Follower);
        let counts = leader.counts();
        assert_eq!(counts.leader_step_downs, 1);
        assert_eq!(counts.elections_failed, before.elections_failed);
        assert_eq!(counts.higher_terms_seen, 0);
        assert_eq!(io.made, 0, "nothing is sent: {:?}", io.calls);
        // The cause of the election it holds is the term above its own, code 4, and
        // it names no leader from the same call (ADR 0102).
        assert_eq!(leader.election_cause(), ElectionCause::HigherTerm);
        assert_eq!(leader.election_cause().code(), 4);
        assert_eq!(leader.leader_gauge(), -1);
        // Init gives the log up, and the member canvasses with no leader.
        work(&mut leader, &mut io, ms + 1);
        assert_eq!(io.count(|call| *call == Call::RemoveLog), 1);
        assert_eq!(leader.election_state(), ElectionState::Canvass);
        assert_eq!(leader.leader_member_id(), -1);
        assert_eq!(leader.counts().elections, before.elections + 1);
        // It keeps the term it led and takes no other: a canvass position raises no
        // candidate term.
        assert_eq!(
            io.count(|call| matches!(call, Call::StoreCandidateTerm(_))),
            0
        );
    }
}

/// A leader whose election has closed holds an election on a request vote whose
/// candidate term id, or a commit position whose leadership term id, is above its own
/// term. It answers the request with nothing and keeps its candidate term. One at or
/// below its term, and one from an id no other member has, moves nothing.
#[test]
fn a_leader_steps_down_on_a_request_vote_or_a_commit_position_of_a_higher_term() {
    for message in [request_vote(1, 1), commit_position(2, 1)] {
        let mut io = Stub::new();
        let mut leader = elector(2);
        let ms = to_closed_leader(&mut leader, &mut io);
        io.clear();
        // Its own term, and a higher one from no member: nothing.
        for ignored in [
            request_vote(1, 0),
            commit_position(2, 0),
            request_vote(9, 5),
            commit_position(9, 5),
            commit_position(0, 5),
        ] {
            read(&mut leader, &mut io, ms, &ignored);
            assert_eq!(leader.election_state(), ElectionState::Closed);
            assert_eq!(leader.role(), Role::Leader);
        }
        assert_eq!(io.made, 0, "{:?}", io.calls);

        read(&mut leader, &mut io, ms + 1, &message);
        assert_eq!(leader.election_state(), ElectionState::Init);
        assert_eq!(leader.role(), Role::Follower);
        assert_eq!(leader.counts().leader_step_downs, 1);
        // The election that begins is for a term above its own, by either message.
        assert_eq!(leader.election_cause(), ElectionCause::HigherTerm);
        assert_eq!(leader.leader_gauge(), -1);
        assert_eq!(leader.counts().higher_terms_seen, 0);
        // No vote, and no term stored: the request is answered by nothing.
        assert_eq!(io.made, 0, "{:?}", io.calls);
    }
}

/// A follower whose election has closed holds an election on the same two messages,
/// counted in `cluster.higher_terms_seen`: the cluster began a term without it. It
/// leaves the log in init, as a follower that begins again does.
#[test]
fn a_follower_holds_an_election_on_a_higher_term_and_counts_it() {
    for message in [request_vote(2, 1), commit_position(2, 1)] {
        let mut io = Stub::new();
        let mut follower = elector(3);
        let ms = to_closed_follower(&mut follower, &mut io);
        io.clear();
        for ignored in [
            request_vote(2, 0),
            commit_position(2, 0),
            request_vote(9, 5),
            commit_position(9, 5),
        ] {
            read(&mut follower, &mut io, ms, &ignored);
            assert_eq!(follower.election_state(), ElectionState::Closed);
        }
        assert_eq!(io.made, 0, "{:?}", io.calls);

        read(&mut follower, &mut io, ms + 1, &message);
        assert_eq!(follower.election_state(), ElectionState::Init);
        assert_eq!(follower.role(), Role::Follower);
        let counts = follower.counts();
        assert_eq!(counts.higher_terms_seen, 1);
        assert_eq!(counts.leader_step_downs, 0);
        assert_eq!(counts.leader_heartbeat_timeouts, 0);
        assert_eq!(io.made, 0, "{:?}", io.calls);
        assert_eq!(follower.election_cause(), ElectionCause::HigherTerm);
        assert_eq!(follower.leader_gauge(), -1);
        work(&mut follower, &mut io, ms + 2);
        assert_eq!(io.count(|call| *call == Call::LeaveLog), 1);
        assert_eq!(io.count(|call| *call == Call::RemoveLog), 0);
        assert_eq!(follower.election_state(), ElectionState::Canvass);
    }
}

/// A leader closes its election in leader ready when a quorum counts for its term at
/// its log position, itself among them, and waits for no other member: member 2,
/// heard in canvass, has not joined its log. The term's first entry is appended once,
/// in the cycle that closes.
///
/// A leader held in leader ready with no member counted stays there, and its process
/// is refused an offer. A member counts by an append position at the leader's log
/// position alone. One at another position counts for nothing. One under an id no
/// member has, or under the leader's own id, counts for nothing. A canvass position
/// that names the leader's term at its position counts for nothing: its sender
/// recorded the term and went back to canvass, and has not joined the log. Back
/// pressure on the term's first entry keeps the election open, and the entry is
/// appended once. Then the leader's process appends.
#[test]
fn a_leader_closes_its_election_on_a_quorum_and_waits_for_no_other_member() {
    let first_entries =
        |io: &Stub| io.count(|call| *call == Call::Append(Template::NewLeadershipTermEvent));
    let mut io = Stub::new();
    let mut leader = elector(4);
    let ms = to_leader_log_replication(&mut leader, &mut io);
    read(&mut leader, &mut io, ms, &append_position(1, 0));
    for step in 0..3 {
        work(&mut leader, &mut io, ms + step);
    }
    assert_eq!(leader.election_state(), ElectionState::LeaderReady);
    assert_eq!(first_entries(&io), 0);
    work(&mut leader, &mut io, ms + 3);
    assert_eq!(leader.election_state(), ElectionState::Closed);
    assert_eq!(leader.role(), Role::Leader);
    assert_eq!(first_entries(&io), 1);
    let counts = leader.counts();
    assert_eq!(counts.elections_closed, 1);
    assert_eq!(counts.elections_failed, 0);

    // Held in leader ready: the leader alone counts, one of three.
    let mut io = Stub::new();
    let mut leader = elector(4);
    let ms = to_leader_ready(&mut leader, &mut io);
    work(&mut leader, &mut io, ms);
    assert_eq!(leader.election_state(), ElectionState::LeaderReady);
    // A position past the leader's own is of entries its log does not hold.
    read(&mut leader, &mut io, ms + 1, &append_position(2, 32));
    work(&mut leader, &mut io, ms + 1);
    assert_eq!(leader.election_state(), ElectionState::LeaderReady);
    assert_eq!(first_entries(&io), 0);
    // A report under an id no member has, and one under the leader's own id.
    read(&mut leader, &mut io, ms + 2, &append_position(7, 0));
    read(&mut leader, &mut io, ms + 2, &append_position(0, 0));
    work(&mut leader, &mut io, ms + 2);
    assert_eq!(leader.election_state(), ElectionState::LeaderReady);
    // Member 1 names the leader's term at the leader's position in a canvass
    // position: it has not joined the log.
    read(&mut leader, &mut io, ms + 3, &canvass_position(1, 0, 0));
    work(&mut leader, &mut io, ms + 3);
    assert_eq!(leader.election_state(), ElectionState::LeaderReady);
    assert_eq!(first_entries(&io), 0);
    assert_eq!(
        leader.offer(at(ms + 3), b"early", &mut io),
        Ok(Err(Refused::Election))
    );

    // Member 2 joins at the leader's position: a quorum. The first entry is
    // back-pressured, and the election stays open until it is appended.
    read(&mut leader, &mut io, ms + 4, &append_position(2, 0));
    io.back_pressured = true;
    work(&mut leader, &mut io, ms + 4);
    assert_eq!(leader.election_state(), ElectionState::LeaderReady);
    assert_eq!(first_entries(&io), 0);
    io.back_pressured = false;
    work(&mut leader, &mut io, ms + 5);
    work(&mut leader, &mut io, ms + 6);
    assert_eq!(leader.election_state(), ElectionState::Closed);
    assert_eq!(first_entries(&io), 1);
    assert_eq!(leader.offer(at(ms + 7), b"now", &mut io), Ok(Ok(())));
    assert_eq!(
        io.count(|call| *call == Call::Append(Template::SessionMessage)),
        1
    );
}

/// A leader whose election has not closed fails it on a canvass position with a term
/// above its own, in leader log replication and in leader ready, and is a follower in
/// init from the same call. In leader replay and leader init the message updates its
/// table alone.
#[test]
fn a_leader_fails_its_election_on_a_canvass_position_with_a_higher_term() {
    // Leader log replication.
    let mut io = Stub::new();
    let mut leader = elector(4);
    let ms = to_leader_log_replication(&mut leader, &mut io);
    read(&mut leader, &mut io, ms, &canvass_with(2, 0, 1));
    assert_eq!(leader.election_state(), ElectionState::Init);
    assert_eq!(leader.role(), Role::Follower);
    assert_eq!(leader.counts().elections_failed, 1);
    assert_eq!(leader.counts().leader_step_downs, 0);
    // The election it begins has the failed one as its cause, code 5 (ADR 0102).
    assert_eq!(leader.election_cause(), ElectionCause::ElectionFailed);
    assert_eq!(leader.election_cause().code(), 5);
    assert_eq!(leader.leader_gauge(), -1);
    work(&mut leader, &mut io, ms + 1);
    assert_eq!(io.count(|call| *call == Call::RemoveLog), 1);
    assert_eq!(leader.election_state(), ElectionState::Canvass);

    // Leader ready.
    let mut io = Stub::new();
    let mut leader = elector(4);
    let ms = to_leader_ready(&mut leader, &mut io);
    read(&mut leader, &mut io, ms, &canvass_with(2, 1, 1));
    assert_eq!(leader.election_state(), ElectionState::Init);
    assert_eq!(leader.role(), Role::Follower);
    assert_eq!(leader.counts().elections_failed, 1);
    assert_eq!(leader.counts().leader_step_downs, 0);

    // Leader replay and leader init: the table alone.
    let mut io = Stub::new();
    let mut leader = elector(4);
    let ms = to_leader_log_replication(&mut leader, &mut io);
    read(&mut leader, &mut io, ms, &append_position(1, 0));
    work(&mut leader, &mut io, ms);
    assert_eq!(leader.election_state(), ElectionState::LeaderReplay);
    read(&mut leader, &mut io, ms + 1, &canvass_with(2, 0, 1));
    assert_eq!(leader.election_state(), ElectionState::LeaderReplay);
    work(&mut leader, &mut io, ms + 1);
    assert_eq!(leader.election_state(), ElectionState::LeaderInit);
    read(&mut leader, &mut io, ms + 2, &canvass_with(2, 0, 1));
    assert_eq!(leader.election_state(), ElectionState::LeaderInit);
    assert_eq!(leader.role(), Role::Leader);
    assert_eq!(leader.counts().elections_failed, 0);
}

/// A leader holds an election when its active members fall below a quorum: no other
/// member has reported for the leader heartbeat timeout, 10 s. Its election closed,
/// that is counted in `cluster.leader_quorum_losses` alone; in leader log replication
/// and in leader ready its election fails, counted in both.
#[test]
fn a_leader_that_loses_its_quorum_holds_an_election() {
    // Closed.
    let mut io = Stub::new();
    let mut leader = elector(5);
    let ms = to_closed_leader(&mut leader, &mut io);
    // One millisecond short of the timeout after the last reports, it leads.
    work(&mut leader, &mut io, ms - 1 + 9_999);
    assert_eq!(leader.role(), Role::Leader);
    work(&mut leader, &mut io, ms - 1 + 10_000);
    assert_eq!(leader.election_state(), ElectionState::Init);
    assert_eq!(leader.role(), Role::Follower);
    let counts = leader.counts();
    assert_eq!(counts.leader_quorum_losses, 1);
    assert_eq!(counts.leader_step_downs, 0);
    assert_eq!(counts.elections_failed, 0);
    // The cause of the election it holds is the quorum it lost, code 3 (ADR 0102).
    assert_eq!(leader.election_cause(), ElectionCause::QuorumLost);
    assert_eq!(leader.election_cause().code(), 3);
    assert_eq!(leader.leader_gauge(), -1);
    work(&mut leader, &mut io, ms + 10_000);
    assert_eq!(io.count(|call| *call == Call::RemoveLog), 1);
    assert_eq!(leader.election_state(), ElectionState::Canvass);

    // Leader log replication: the members reported last in canvass, at 2 ms.
    let mut io = Stub::new();
    let mut leader = elector(5);
    to_leader_log_replication(&mut leader, &mut io);
    work(&mut leader, &mut io, 2 + 10_000);
    assert_eq!(leader.election_state(), ElectionState::Init);
    assert_eq!(leader.role(), Role::Follower);
    let counts = leader.counts();
    assert_eq!(counts.leader_quorum_losses, 1);
    assert_eq!(counts.elections_failed, 1);
    // A quorum lost before its election closed fails that election: code 5, not 3.
    assert_eq!(leader.election_cause(), ElectionCause::ElectionFailed);

    // Leader ready: member 1 reported last when it canvassed again.
    let mut io = Stub::new();
    let mut leader = elector(5);
    let ms = to_leader_ready(&mut leader, &mut io);
    work(&mut leader, &mut io, ms + 10_000);
    assert_eq!(leader.election_state(), ElectionState::Init);
    assert_eq!(leader.role(), Role::Follower);
    let counts = leader.counts();
    assert_eq!(counts.leader_quorum_losses, 1);
    assert_eq!(counts.elections_failed, 1);
}

/// A leader that begins again gives its log up in init before anything else, once. A
/// fault there leaves it in init, and the next cycle gives the log up again. The term
/// it leads next has a log of its own, added at that term's base.
#[test]
fn a_leader_gives_its_log_up_in_init_and_adds_a_new_one_for_its_next_term() {
    let mut io = Stub::new();
    let mut leader = elector(6);
    let ms = to_closed_leader(&mut leader, &mut io);
    read(&mut leader, &mut io, ms, &commit_position(1, 1));
    assert_eq!(leader.election_state(), ElectionState::Init);
    io.clear();

    io.fail_remove = true;
    assert!(leader.do_work(at(ms + 1), &mut io).is_err());
    assert_eq!(leader.election_state(), ElectionState::Init);
    assert_eq!(leader.counts().elections, 1, "no election begun by a fault");
    io.fail_remove = false;
    work(&mut leader, &mut io, ms + 2);
    assert_eq!(leader.election_state(), ElectionState::Canvass);
    assert_eq!(io.count(|call| *call == Call::RemoveLog), 2);
    // Not again: the member holds no log.
    read(&mut leader, &mut io, ms + 3, &canvass_position(1, 0, 0));
    read(&mut leader, &mut io, ms + 3, &canvass_position(2, 0, 0));
    work(&mut leader, &mut io, ms + 4);
    assert_eq!(leader.election_state(), ElectionState::Nominate);
    io.clear();

    // It leads term 1 on both votes, and adds that term's log at its base.
    work(&mut leader, &mut io, ms + 504);
    assert_eq!(leader.election_state(), ElectionState::CandidateBallot);
    for voter in [1, 2] {
        let vote = Message::Vote {
            candidate_term_id: 1,
            log_leadership_term_id: 0,
            log_position: 0,
            candidate_member_id: 0,
            follower_member_id: voter,
            vote: true,
        };
        read(&mut leader, &mut io, ms + 505, &vote);
    }
    work(&mut leader, &mut io, ms + 506);
    assert_eq!(leader.role(), Role::Leader);
    assert_eq!(leader.leadership_term_id(), 1);
    work(&mut leader, &mut io, ms + 507);
    assert_eq!(io.count(|call| *call == Call::RemoveLog), 0);
    assert_eq!(io.count(|call| *call == Call::AddLog(1, 0)), 1);
    let stored = io
        .first(|call| *call == Call::StoreCandidateTerm(1))
        .expect("the term stored");
    let recorded = io
        .first(|call| *call == Call::RecordTerm(1, 0))
        .expect("the term recorded");
    let added = io
        .first(|call| *call == Call::AddLog(1, 0))
        .expect("the log added");
    assert!(stored < recorded && recorded < added, "{:?}", io.calls);
}

/// A recording's stop that is asked and not yet read keeps a member in init with no
/// fault (card 256): `Io::leave_log` and `Io::remove_log` answer `false`, the cycle
/// that asked returns at once with nothing done, and each cycle after it asks again
/// and nothing else, until the stop is read. The member then canvasses, as it does
/// when the stop is read at the first ask. So the thread that runs the duty cycle is
/// never held for a stop, and a stop that takes its time is no fault.
#[test]
fn a_member_waits_in_init_for_a_stop_that_is_not_yet_read_and_faults_nothing() {
    // A follower that holds an election leaves the log in init.
    let mut io = Stub::new();
    let mut follower = elector(3);
    let ms = to_closed_follower(&mut follower, &mut io);
    read(&mut follower, &mut io, ms + 1, &request_vote(2, 1));
    assert_eq!(follower.election_state(), ElectionState::Init);
    let elections = follower.counts().elections;
    io.clear();
    io.stops_pending = 3;
    for cycle in 0..3 {
        let done = follower
            .do_work(at(ms + 2 + cycle), &mut io)
            .expect("a stop not yet read is no fault");
        assert_eq!(done, 0, "the cycle did nothing but ask");
        assert_eq!(follower.election_state(), ElectionState::Init);
    }
    assert_eq!(io.count(|call| *call == Call::LeaveLog), 3);
    assert_eq!(
        io.count(|call| *call != Call::LeaveLog),
        0,
        "{:?}",
        io.calls
    );
    work(&mut follower, &mut io, ms + 5);
    assert_eq!(io.count(|call| *call == Call::LeaveLog), 4, "and read");
    assert_eq!(follower.election_state(), ElectionState::Canvass);
    assert_eq!(follower.counts().elections, elections + 1, "one election");
    assert_eq!(follower.counts().elections_failed, 0);

    // A leader that steps down gives its log up in init, the same way.
    let mut io = Stub::new();
    let mut leader = elector(1);
    let ms = to_closed_leader(&mut leader, &mut io);
    read(&mut leader, &mut io, ms, &canvass_with(2, 1, 1));
    assert_eq!(leader.election_state(), ElectionState::Init);
    io.clear();
    io.stops_pending = 2;
    for cycle in 0..2 {
        let done = leader
            .do_work(at(ms + 1 + cycle), &mut io)
            .expect("a stop not yet read is no fault");
        assert_eq!(done, 0);
        assert_eq!(leader.election_state(), ElectionState::Init);
    }
    assert_eq!(io.count(|call| *call == Call::RemoveLog), 2);
    assert_eq!(
        io.count(|call| *call != Call::RemoveLog),
        0,
        "{:?}",
        io.calls
    );
    work(&mut leader, &mut io, ms + 3);
    assert_eq!(io.count(|call| *call == Call::RemoveLog), 3, "and read");
    assert_eq!(leader.election_state(), ElectionState::Canvass);
}
