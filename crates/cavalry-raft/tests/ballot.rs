//! A member that elects, one core at a time, driven by hand through an [`Io`] that
//! remembers what it was asked (`spec/cluster.md`, "Roles and the election", states 1
//! to 4 and "Messages during an election"; ADR 0077, ADR 0081): the order of a term's
//! store and the vote or the request it allows, one vote in a term, and when a ballot
//! closes.
//!
//! [`Io`]: cavalry_raft::Io

mod common;

use cavalry_proto::cluster::{Message, Template};
use cavalry_raft::{Consensus, ElectionState, Role};
use common::{Call, Stub, at, canvass_position, new_leadership_term};

/// Member 0 of `[0, 1, 2]`, electing.
fn elector(seed: u64) -> Consensus {
    Consensus::with_election(&[0, 1, 2], 0, seed).expect("a member")
}

/// `candidate`'s request vote for `term`, from a log that holds no term and ends at
/// `position`.
fn request_vote(candidate: i32, term: i64, position: i64) -> Message<'static> {
    Message::RequestVote {
        log_leadership_term_id: -1,
        log_position: position,
        candidate_term_id: term,
        candidate_member_id: candidate,
        protocol_version: 0,
    }
}

/// `voter`'s vote on member 0's ballot for `term`.
fn vote(voter: i32, term: i64, vote: bool) -> Message<'static> {
    Message::Vote {
        candidate_term_id: term,
        log_leadership_term_id: -1,
        log_position: 0,
        candidate_member_id: 0,
        follower_member_id: voter,
        vote,
    }
}

fn sends(io: &Stub, template: Template) -> usize {
    io.count(|call| matches!(call, Call::Send { template: sent, .. } if *sent == template))
}

/// Takes `member` from init to candidate ballot at `ms`, hearing the members of
/// `heard` in canvass, none ahead of it. With both heard it is a candidate at once;
/// with one, at the startup canvass timeout, 60 s. Returns the millisecond its ballot
/// began at.
fn to_candidate_ballot(member: &mut Consensus, io: &mut Stub, heard: &[i32]) -> u64 {
    member.do_work(at(1), io).expect("no fault");
    assert_eq!(member.election_state(), ElectionState::Canvass);
    for &other in heard {
        member
            .on_message(at(2), &canvass_position(other, -1, 0), io)
            .expect("no fault");
    }
    let mut ms = 3;
    member.do_work(at(ms), io).expect("no fault");
    if heard.len() < 2 {
        assert_eq!(member.election_state(), ElectionState::Canvass);
        ms = 60_002;
        member.do_work(at(ms), io).expect("no fault");
    }
    assert_eq!(member.election_state(), ElectionState::Nominate);
    assert_eq!(member.role(), Role::Follower);
    // The delay is below half the election timeout.
    ms += 500;
    member.do_work(at(ms), io).expect("no fault");
    assert_eq!(member.election_state(), ElectionState::CandidateBallot);
    assert_eq!(member.role(), Role::Candidate);
    ms
}

/// A candidate stores the term it proposes before it asks any member for a vote, and
/// asks each member once.
#[test]
fn a_candidate_stores_its_term_before_it_asks_for_a_vote() {
    let mut io = Stub::new();
    let mut member = elector(1);
    assert_eq!(member.leader_member_id(), -1);
    let ms = to_candidate_ballot(&mut member, &mut io, &[1, 2]);
    // The cycle that ends nominate stores the term; the ballot's first cycle asks.
    assert_eq!(sends(&io, Template::RequestVote), 0);
    member.do_work(at(ms + 1), &mut io).expect("no fault");
    let stored = io
        .first(|call| matches!(call, Call::StoreCandidateTerm(0)))
        .expect("term 0 is stored");
    let asked = io
        .first(|call| {
            matches!(
                call,
                Call::Send {
                    template: Template::RequestVote,
                    ..
                }
            )
        })
        .expect("a request vote is sent");
    assert!(stored < asked, "stored at call {stored}, asked at {asked}");
    assert_eq!(
        io.count(|call| matches!(call, Call::StoreCandidateTerm(_))),
        1
    );
    assert_eq!(sends(&io, Template::RequestVote), 2);
    for to in [1, 2] {
        assert_eq!(
            io.count(|call| matches!(call, Call::Send { to: sent, template: Template::RequestVote } if *sent == to)),
            1,
            "member {to}"
        );
    }
    // Later cycles ask nobody again.
    for later in 2..=6 {
        member.do_work(at(ms + later), &mut io).expect("no fault");
    }
    assert_eq!(sends(&io, Template::RequestVote), 2);
    let counts = member.counts();
    assert_eq!((counts.candidacies, counts.terms_led), (1, 0));
}

/// A ballot closes at once on the members heard (ADR 0077). The candidate heard member
/// 1 alone: member 2 has said nothing since the candidate's election began, as a leader
/// that was killed says nothing. Member 1's vote makes a quorum with the candidate's
/// own, every member heard has voted for it, and it leads on the next cycle, long
/// before the election timeout.
#[test]
fn a_ballot_closes_at_once_when_every_member_heard_has_voted_for_the_candidate() {
    let mut io = Stub::new();
    let mut member = elector(2);
    let ms = to_candidate_ballot(&mut member, &mut io, &[1]);
    member.do_work(at(ms + 1), &mut io).expect("no fault");
    assert_eq!(member.election_state(), ElectionState::CandidateBallot);
    member
        .on_message(at(ms + 2), &vote(1, 0, true), &mut io)
        .expect("no fault");
    member.do_work(at(ms + 3), &mut io).expect("no fault");
    assert_eq!(member.election_state(), ElectionState::LeaderLogReplication);
    assert_eq!(member.role(), Role::Leader);
    assert_eq!(member.leadership_term_id(), 0);
    assert_eq!(member.leader_member_id(), 0);
    // The term is recorded at the member's append position before it leads it.
    assert!(
        io.first(|call| matches!(call, Call::RecordTerm(0, 0)))
            .is_some()
    );
}

/// A member that was heard is waited for. With both members heard and one vote for,
/// the candidate has a quorum and does not lead: member 2 may hold a log ahead of it
/// and vote against. At the election timeout, with no vote against, it leads.
#[test]
fn a_ballot_waits_the_election_timeout_for_a_member_heard_that_has_not_voted() {
    let mut io = Stub::new();
    let mut member = elector(3);
    let ms = to_candidate_ballot(&mut member, &mut io, &[1, 2]);
    member
        .on_message(at(ms + 1), &vote(1, 0, true), &mut io)
        .expect("no fault");
    for later in [2, 500, 999] {
        member.do_work(at(ms + later), &mut io).expect("no fault");
        assert_eq!(
            member.election_state(),
            ElectionState::CandidateBallot,
            "{later} ms into the ballot"
        );
    }
    member.do_work(at(ms + 1_000), &mut io).expect("no fault");
    assert_eq!(member.election_state(), ElectionState::LeaderLogReplication);
    assert_eq!(member.counts().ballots_lost, 0);
}

/// A vote against ends the ballot with no leader, whatever the votes for: at the
/// election timeout the candidate canvasses again, a follower, and counts the ballot.
#[test]
fn a_vote_against_sends_the_candidate_back_to_canvass_at_the_election_timeout() {
    let mut io = Stub::new();
    let mut member = elector(4);
    let ms = to_candidate_ballot(&mut member, &mut io, &[1, 2]);
    member
        .on_message(at(ms + 1), &vote(1, 0, true), &mut io)
        .expect("no fault");
    member
        .on_message(at(ms + 2), &vote(2, 0, false), &mut io)
        .expect("no fault");
    member.do_work(at(ms + 999), &mut io).expect("no fault");
    assert_eq!(member.election_state(), ElectionState::CandidateBallot);
    member.do_work(at(ms + 1_000), &mut io).expect("no fault");
    assert_eq!(member.election_state(), ElectionState::Canvass);
    assert_eq!(member.role(), Role::Follower);
    let counts = member.counts();
    assert_eq!((counts.ballots_lost, counts.terms_led), (1, 0));
    assert!(
        io.first(|call| matches!(call, Call::RecordTerm(..)))
            .is_none()
    );
}

/// A member votes once in a term, and stores a term before the vote that raises it. A
/// request for a higher term from a log at least its own is answered for; a second
/// request for that term, and one for a lower term, are answered against with nothing
/// stored; a request for a higher term from a log behind its own is answered against,
/// and the term is taken and stored all the same.
#[test]
fn a_member_votes_once_in_a_term_and_stores_the_term_before_the_vote() {
    let mut io = Stub::new();
    io.append_position = 64;
    let mut member = elector(5);
    member.do_work(at(1), &mut io).expect("no fault");
    io.clear();

    member
        .on_message(at(2), &request_vote(1, 3, 64), &mut io)
        .expect("no fault");
    let stored = io
        .first(|call| matches!(call, Call::StoreCandidateTerm(3)))
        .expect("term 3 is stored");
    let voted = io
        .first(|call| {
            matches!(
                call,
                Call::Send {
                    to: 1,
                    template: Template::Vote
                }
            )
        })
        .expect("a vote is sent to member 1");
    assert!(stored < voted);
    assert_eq!(member.election_state(), ElectionState::FollowerBallot);
    let counts = member.counts();
    assert_eq!((counts.votes_for, counts.votes_against), (1, 0));

    // The same term from another candidate, then a lower one: against, nothing stored.
    io.clear();
    member
        .on_message(at(3), &request_vote(2, 3, 128), &mut io)
        .expect("no fault");
    member
        .on_message(at(4), &request_vote(2, 2, 128), &mut io)
        .expect("no fault");
    assert_eq!(
        io.count(|call| matches!(call, Call::StoreCandidateTerm(_))),
        0
    );
    assert_eq!(
        io.count(|call| matches!(
            call,
            Call::Send {
                to: 2,
                template: Template::Vote
            }
        )),
        2
    );
    let counts = member.counts();
    assert_eq!((counts.votes_for, counts.votes_against), (1, 2));
    assert_eq!(member.election_state(), ElectionState::FollowerBallot);

    // A higher term from a log behind this member's: against, and the term is taken.
    io.clear();
    member
        .on_message(at(5), &request_vote(2, 4, 0), &mut io)
        .expect("no fault");
    let stored = io
        .first(|call| matches!(call, Call::StoreCandidateTerm(4)))
        .expect("term 4 is stored");
    let voted = io
        .first(|call| {
            matches!(
                call,
                Call::Send {
                    to: 2,
                    template: Template::Vote
                }
            )
        })
        .expect("a vote is sent to member 2");
    assert!(stored < voted);
    let counts = member.counts();
    assert_eq!((counts.votes_for, counts.votes_against), (1, 3));
}

/// A vote that cannot be sent is no vote: the term it would have raised is stored, so
/// the member votes for nobody else in it, and the member stays where it was and
/// counts nothing.
#[test]
fn a_vote_that_is_not_sent_moves_the_term_and_nothing_else() {
    let mut io = Stub::new();
    let mut member = elector(6);
    member.do_work(at(1), &mut io).expect("no fault");
    io.refuse_send = true;
    member
        .on_message(at(2), &request_vote(1, 3, 0), &mut io)
        .expect("no fault");
    assert_eq!(
        io.count(|call| matches!(call, Call::StoreCandidateTerm(3))),
        1
    );
    assert_eq!(member.election_state(), ElectionState::Canvass);
    let counts = member.counts();
    assert_eq!((counts.votes_for, counts.votes_against), (0, 0));
    // The term is taken: the same request again is one for a term it has voted in.
    io.refuse_send = false;
    member
        .on_message(at(3), &request_vote(1, 3, 0), &mut io)
        .expect("no fault");
    assert_eq!(member.counts().votes_against, 1);
    assert_eq!(member.election_state(), ElectionState::Canvass);
}

/// A candidate that takes a higher term during its ballot, voting against a candidate
/// whose log is behind its own, holds no vote for its new term, its own included:
/// votes for the term it proposed count for nothing, and it canvasses again at the
/// election timeout, never leading a term with votes cast for another.
#[test]
fn a_candidate_whose_term_rose_during_its_ballot_leads_no_term() {
    let mut io = Stub::new();
    io.append_position = 64;
    let mut member = elector(7);
    let ms = to_candidate_ballot(&mut member, &mut io, &[1, 2]);
    member
        .on_message(at(ms + 1), &request_vote(1, 5, 0), &mut io)
        .expect("no fault");
    assert_eq!(member.election_state(), ElectionState::CandidateBallot);
    assert_eq!(member.counts().votes_against, 1);
    for voter in [1, 2] {
        member
            .on_message(at(ms + 2), &vote(voter, 0, true), &mut io)
            .expect("no fault");
    }
    member.do_work(at(ms + 3), &mut io).expect("no fault");
    assert_eq!(member.election_state(), ElectionState::CandidateBallot);
    member.do_work(at(ms + 1_000), &mut io).expect("no fault");
    assert_eq!(member.election_state(), ElectionState::Canvass);
    assert_eq!(member.counts().terms_led, 0);
    assert!(
        io.first(|call| matches!(call, Call::RecordTerm(..)))
            .is_none()
    );
}

/// A member that elects takes no term from an id that is no other member's: id 7, a
/// host started with another members string, or its own id. In canvass it records no
/// term, takes no leader and canvasses on; a candidate stays in its ballot. A new
/// leadership term is read as no consensus message from such an id is read.
#[test]
fn a_member_that_elects_takes_no_term_from_an_id_that_is_no_other_members() {
    for stranger in [7, 0] {
        let mut io = Stub::new();
        let mut member = elector(9);
        member.do_work(at(1), &mut io).expect("no fault");
        member
            .on_message(at(2), &new_leadership_term(stranger, 0), &mut io)
            .expect("no fault");
        assert_eq!(
            io.first(|call| matches!(call, Call::RecordTerm(..))),
            None,
            "leader {stranger}"
        );
        assert_eq!(member.election_state(), ElectionState::Canvass);
        assert_eq!(member.leader_member_id(), -1);
        assert_eq!(member.leadership_term_id(), -1);

        // A candidate for term 0 reads it for term 0, its own candidate term.
        let mut io = Stub::new();
        let mut member = elector(10);
        let ms = to_candidate_ballot(&mut member, &mut io, &[1, 2]);
        member
            .on_message(at(ms + 1), &new_leadership_term(stranger, 0), &mut io)
            .expect("no fault");
        assert_eq!(
            io.first(|call| matches!(call, Call::RecordTerm(..))),
            None,
            "leader {stranger}"
        );
        assert_eq!(member.election_state(), ElectionState::CandidateBallot);
        assert_eq!(member.role(), Role::Candidate);
        assert_eq!(member.leader_member_id(), -1);
    }
}

/// A member that elects takes the new leadership term of whichever member sends it,
/// where a member whose leader is appointed reads the appointed leader's alone.
#[test]
fn a_member_that_elects_follows_the_member_whose_term_it_reads() {
    let mut io = Stub::new();
    let mut member = elector(8);
    member.do_work(at(1), &mut io).expect("no fault");
    assert_eq!(member.leader_member_id(), -1);
    member
        .on_message(at(2), &new_leadership_term(2, 0), &mut io)
        .expect("no fault");
    assert_eq!(member.election_state(), ElectionState::FollowerReplay);
    assert_eq!(member.leader_member_id(), 2);
    assert_eq!(member.leadership_term_id(), 0);

    let mut io = Stub::new();
    let mut appointed = Consensus::new(&[0, 1, 2], 1).expect("a member");
    appointed.do_work(at(1), &mut io).expect("no fault");
    appointed
        .on_message(at(2), &new_leadership_term(2, 0), &mut io)
        .expect("no fault");
    assert_eq!(appointed.election_state(), ElectionState::Canvass);
    assert_eq!(appointed.leader_member_id(), 0);
}
