//! A member that elects starts on the log directory it kept, one core at a time,
//! driven by hand through an [`Io`] that remembers what it was asked
//! (`spec/cluster.md`, "The log" and "Roles and the election", states 0 and 1; ADR
//! 0089): it reads its stored candidate term and the highest term of its record once,
//! in its first init, canvasses with them, votes once in a term across a restart, and
//! as a leader says whether its term is of its first election since it started.
//!
//! [`Io`]: cavalry_raft::Io

mod common;

use cavalry_proto::cluster::{Message, Template};
use cavalry_raft::{Consensus, ElectionState, Role};
use common::{Call, Stub, at, canvass_position, follow_term, leadership_term, lose_the_leader};

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

/// `voter`'s vote for member 0 on its ballot for `term`.
fn vote_for(voter: i32, term: i64) -> Message<'static> {
    Message::Vote {
        candidate_term_id: term,
        log_leadership_term_id: -1,
        log_position: 0,
        candidate_member_id: 0,
        follower_member_id: voter,
        vote: true,
    }
}

/// Takes `member` from canvass at `ms` to the lead of `term`, its election closed:
/// both other members report the log (`log_term`, 0) in canvass, vote for it and join
/// its log at position 0. Returns the next millisecond.
fn lead(member: &mut Consensus, io: &mut Stub, ms: u64, log_term: i64, term: i64) -> u64 {
    assert_eq!(member.election_state(), ElectionState::Canvass);
    for other in [1, 2] {
        read(member, io, ms, &canvass_position(other, log_term, 0));
    }
    work(member, io, ms + 1);
    assert_eq!(member.election_state(), ElectionState::Nominate);
    // The delay is below half the election timeout.
    work(member, io, ms + 501);
    assert_eq!(member.election_state(), ElectionState::CandidateBallot);
    for voter in [1, 2] {
        read(member, io, ms + 502, &vote_for(voter, term));
    }
    work(member, io, ms + 503);
    assert_eq!(member.role(), Role::Leader);
    assert_eq!(member.leadership_term_id(), term);
    let joined = Message::AppendPosition {
        leadership_term_id: term,
        log_position: 0,
        follower_member_id: 1,
        flags: 0,
    };
    for step in 504..510 {
        read(member, io, ms + step, &joined);
        work(member, io, ms + step);
    }
    assert_eq!(member.election_state(), ElectionState::Closed);
    ms + 510
}

/// A member that elects reads what its log directory holds in its first init, and
/// only there: the candidate term it stored and the highest term of its record. It
/// canvasses with them and with the append position its recordings hold. The term it
/// followed, its leader and the commit position it knew are not kept: it follows no
/// term and knows no commit until a leader tells it. An election that begins again at
/// init reads nothing.
#[test]
fn a_member_starts_with_the_candidate_term_and_the_log_term_its_directory_holds() {
    // The directory of a member that recorded term 5, holds 96 bytes and voted in 7.
    let mut io = Stub::new();
    io.candidate_term = 7;
    io.log_term = 5;
    io.append_position = 96;
    let mut member = elector(3);
    assert_eq!(io.reads, [0, 0]);
    work(&mut member, &mut io, 1);
    assert_eq!(member.election_state(), ElectionState::Canvass);
    assert_eq!(io.reads, [1, 1]);
    work(&mut member, &mut io, 2);
    assert_eq!(io.last_canvass, Some((5, 96, 7)));
    assert_eq!(member.append_position(), 96);
    assert_eq!(member.leadership_term_id(), -1);
    assert_eq!(member.leader_member_id(), -1);
    assert_eq!(member.commit_position(), 0);

    // It follows member 1's term 8, which begins where its log ends, loses that
    // leader and begins again at init: nothing is read a second time.
    let term = leadership_term(1, 8, 96, 96, 5, [8, 96, -1]);
    let closed = follow_term(&mut member, &mut io, 3, &term, 96);
    assert_eq!(member.leadership_term_id(), 8);
    lose_the_leader(&mut member, &mut io, closed, 96);
    assert_eq!(io.reads, [1, 1]);
    work(&mut member, &mut io, closed + 10_100);
    assert_eq!(io.last_canvass, Some((8, 96, 8)));
}

/// A directory that holds a record and no stored candidate term above it: the member
/// takes the higher of the two as its candidate term, as init does at any election.
#[test]
fn a_member_takes_the_higher_of_its_stored_candidate_term_and_its_log_term() {
    let mut io = Stub::new();
    io.candidate_term = 2;
    io.log_term = 4;
    io.append_position = 64;
    let mut member = elector(3);
    work(&mut member, &mut io, 1);
    work(&mut member, &mut io, 2);
    assert_eq!(io.last_canvass, Some((4, 64, 4)));
}

/// A member votes once in a term across a restart. In its first life it votes for
/// member 1 in term 3, which it stores before the vote. Started again on that
/// directory, it answers member 2's request for term 3 against, with nothing stored,
/// and a request for term 4 for. A member started on an empty directory answers the
/// request for term 3 for: the stored term is what tells the two apart.
#[test]
fn a_member_votes_once_in_a_term_across_a_restart() {
    let mut io = Stub::new();
    io.append_position = 64;
    let mut first = elector(5);
    work(&mut first, &mut io, 1);
    read(&mut first, &mut io, 2, &request_vote(1, 3, 64));
    let counts = first.counts();
    assert_eq!((counts.votes_for, counts.votes_against), (1, 0));
    assert_eq!(io.candidate_term, 3);

    // The process ends; the directory is what `io` holds.
    io.clear();
    let mut second = elector(5);
    work(&mut second, &mut io, 1);
    assert_eq!(second.election_state(), ElectionState::Canvass);
    read(&mut second, &mut io, 2, &request_vote(2, 3, 128));
    let counts = second.counts();
    assert_eq!((counts.votes_for, counts.votes_against), (0, 1));
    assert_eq!(
        io.count(|call| matches!(call, Call::StoreCandidateTerm(_))),
        0
    );
    assert_eq!(second.election_state(), ElectionState::Canvass);
    io.clear();
    read(&mut second, &mut io, 3, &request_vote(2, 4, 128));
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
    let counts = second.counts();
    assert_eq!((counts.votes_for, counts.votes_against), (1, 1));
    assert_eq!(second.election_state(), ElectionState::FollowerBallot);

    // On an empty directory, the same request for term 3 is answered for.
    let mut io = Stub::new();
    io.append_position = 64;
    let mut new = elector(5);
    work(&mut new, &mut io, 1);
    read(&mut new, &mut io, 2, &request_vote(2, 3, 128));
    let counts = new.counts();
    assert_eq!((counts.votes_for, counts.votes_against), (1, 0));
}

/// A leader that elects says in `is startup` whether the term it leads is of its
/// first election since it started, in its election and after it has closed. The
/// term it leads after that election, here once it lost its quorum, says 0.
#[test]
fn a_leader_says_whether_its_term_is_of_its_first_election_since_it_started() {
    let mut io = Stub::new();
    let mut member = elector(4);
    work(&mut member, &mut io, 1);
    let ms = lead(&mut member, &mut io, 2, -1, 0);
    assert_eq!(io.last_startup, Some(true));
    // Closed, it answers a member that canvasses with the same word.
    io.last_startup = None;
    read(&mut member, &mut io, ms, &canvass_position(2, -1, 0));
    assert_eq!(io.last_startup, Some(true));

    // No member reports for the leader heartbeat timeout: it holds an election, and
    // leads term 1 from it.
    work(&mut member, &mut io, ms + 10_000);
    assert_eq!(member.election_state(), ElectionState::Init);
    work(&mut member, &mut io, ms + 10_001);
    io.last_startup = None;
    let ms = lead(&mut member, &mut io, ms + 10_002, 0, 1);
    assert_eq!(io.last_startup, Some(false));
    io.last_startup = None;
    read(&mut member, &mut io, ms, &canvass_position(2, 0, 0));
    assert_eq!(io.last_startup, Some(false));
}
