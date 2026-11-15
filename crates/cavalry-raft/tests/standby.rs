//! Standbys (`spec/cluster.md`, "Standbys" and "Changing a standby's mark"; ADR 0134):
//! a member that the members string marks `standby` follows the log and has no vote.
//! First one core at a time, driven by hand: what a standby does with a canvass and
//! with a request vote, and what a voter counts. Then clusters in memory: a standby
//! follows and delays no election, its append position commits nothing, a standby
//! asked for a vote by a member that runs another string answers nothing, a standby
//! promoted leads with every committed entry, a lost voter is replaced in two
//! changes, and two marks changed at once elect a leader that lacks a committed
//! entry, which is what the rules of a change are for.

mod common;
mod elected_cluster;

use cavalry_proto::cluster::{Message, Template};
use cavalry_raft::memory::Cluster;
use cavalry_raft::{Consensus, ElectionState, MembersRefused, Role};
use common::{Call, Stub, at, canvass_position, follow_term, new_leadership_term};
use elected_cluster::{Safety, holds, settled};

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

/// `voter`'s vote on `candidate`'s ballot for `term`.
fn vote(voter: i32, candidate: i32, term: i64, vote: bool) -> Message<'static> {
    Message::Vote {
        candidate_term_id: term,
        log_leadership_term_id: -1,
        log_position: 0,
        candidate_member_id: candidate,
        follower_member_id: voter,
        vote,
    }
}

fn sends(io: &Stub, template: Template) -> usize {
    io.count(|call| matches!(call, Call::Send { template: sent, .. } if *sent == template))
}

fn sends_to(io: &Stub, to: i32, template: Template) -> usize {
    io.count(
        |call| matches!(call, Call::Send { to: sent_to, template: sent } if *sent_to == to && *sent == template),
    )
}

// ---- one core, by hand ------------------------------------------------------------

/// The members a core is given are checked in the page's order: grammar, duplicate,
/// count, voters, self. A member with no voter beside it is refused by a check of its
/// own, after the count and before the member's own id.
#[test]
fn members_with_no_voter_are_refused_after_the_count_and_before_the_members_own_id() {
    let refused = |voters: &[i32], standbys: &[i32], own: i32| {
        Consensus::with_standbys(voters, standbys, own, 1).expect_err("refused")
    };
    let none = refused(&[], &[0, 1], 0);
    assert_eq!(none, MembersRefused::NoVoter { standbys: 2 });
    assert_eq!(none.reason(), "members-voters");
    assert!(none.to_string().contains("take `,standby` off"), "{none}");
    // A duplicate is one id twice, in either list or across the two.
    assert_eq!(
        refused(&[0, 1], &[1], 0),
        MembersRefused::Duplicate { id: 1 }
    );
    assert_eq!(
        refused(&[0], &[2, 2], 0),
        MembersRefused::Duplicate { id: 2 }
    );
    // The count is of the voters and the standbys together, and is checked first.
    assert_eq!(
        refused(&[0, 1, 2, 3, 4], &[5, 6, 7, 8], 0),
        MembersRefused::Count { found: 9 }
    );
    assert_eq!(
        refused(&[], &[0, 1, 2, 3, 4, 5, 6, 7, 8], 0),
        MembersRefused::Count { found: 9 }
    );
    // No voter is found before an id that is no member's.
    assert_eq!(
        refused(&[], &[0], 7),
        MembersRefused::NoVoter { standbys: 1 }
    );
    assert_eq!(refused(&[0], &[1], 7), MembersRefused::NotAMember { id: 7 });
}

/// What `cluster.voters` and `cluster.standby` hold. The voters are one bit each, by
/// the rising order of the members' ids and not by their order in the string, so two
/// members whose strings list the same marks in another order hold one value.
#[test]
fn a_member_says_which_members_vote_by_the_rising_order_of_their_ids() {
    let standby = Consensus::with_standbys(&[5, 1], &[3], 3, 1).expect("a standby");
    assert!(standby.is_standby());
    assert_eq!(standby.voters(), 0b101);
    let voter = Consensus::with_standbys(&[1, 5], &[3], 5, 1).expect("a voter");
    assert!(!voter.is_standby());
    assert_eq!(voter.voters(), 0b101);
    // No standby: every member's bit, as `with_election` makes a member.
    let all = Consensus::with_election(&[0, 1, 2], 0, 1).expect("a voter");
    assert!(!all.is_standby());
    assert_eq!(all.voters(), 0b111);
    // Eight members, the two with the highest ids standbys.
    let eight = Consensus::with_standbys(&[0, 1, 2, 3, 4, 5], &[6, 7], 6, 1).expect("a standby");
    assert_eq!(eight.voters(), 0b0011_1111);
}

/// A standby canvasses and never nominates itself. Both voters report, neither ahead
/// of it: a voter in its place is a unanimous candidate at once. The standby sends
/// its canvass position to both, stays in canvass past the startup canvass timeout,
/// stores no term and asks nobody for a vote.
#[test]
fn a_standby_canvasses_and_never_nominates_itself() {
    let mut io = Stub::new();
    let mut member = Consensus::with_standbys(&[0, 1], &[2], 2, 1).expect("a standby");
    member.do_work(at(1), &mut io).expect("no fault");
    assert_eq!(member.election_state(), ElectionState::Canvass);
    for voter in [0, 1] {
        member
            .on_message(at(2), &canvass_position(voter, -1, 0), &mut io)
            .expect("no fault");
    }
    for ms in [3, 600, 60_002, 61_000, 75_000] {
        member.do_work(at(ms), &mut io).expect("no fault");
        assert_eq!(
            member.election_state(),
            ElectionState::Canvass,
            "at {ms} ms"
        );
        assert_eq!(member.role(), Role::Follower, "at {ms} ms");
    }
    // One canvass position to each voter at each of the five cycles, which are
    // more than an election status interval apart.
    for voter in [0, 1] {
        assert_eq!(
            sends_to(&io, voter, Template::CanvassPosition),
            5,
            "to {voter}"
        );
    }
    assert_eq!(sends(&io, Template::RequestVote), 0);
    assert_eq!(
        io.count(|call| matches!(call, Call::StoreCandidateTerm(_))),
        0
    );
    let counts = member.counts();
    assert_eq!((counts.candidacies, counts.terms_led), (0, 0), "{counts:?}");

    // A voter with the same two reports nominates itself on its next cycle.
    let mut io = Stub::new();
    let mut voter = Consensus::with_election(&[0, 1, 2], 2, 1).expect("a voter");
    voter.do_work(at(1), &mut io).expect("no fault");
    for other in [0, 1] {
        voter
            .on_message(at(2), &canvass_position(other, -1, 0), &mut io)
            .expect("no fault");
    }
    voter.do_work(at(3), &mut io).expect("no fault");
    assert_eq!(voter.election_state(), ElectionState::Nominate);
}

/// A standby answers a request vote with nothing and keeps its candidate term: it
/// sends no vote, for or against, stores no term, stays where it is, and counts the
/// request. A vote against would end the candidate's ballot.
#[test]
fn a_standby_answers_a_request_vote_with_nothing_and_counts_it() {
    let mut io = Stub::new();
    let mut member = Consensus::with_standbys(&[0, 1], &[2], 2, 1).expect("a standby");
    member.do_work(at(1), &mut io).expect("no fault");
    assert_eq!(member.election_state(), ElectionState::Canvass);
    // A candidate whose log is at least the standby's: a voter votes for it.
    member
        .on_message(at(2), &request_vote(0, 0, 0), &mut io)
        .expect("no fault");
    // A candidate whose log is behind the standby's own, were it to hold one: a
    // voter votes against. The standby's log is empty, so this is the same request
    // for the term after, which a voter that voted in term 0 would also answer.
    member
        .on_message(at(3), &request_vote(1, 1, 0), &mut io)
        .expect("no fault");
    assert_eq!(sends(&io, Template::Vote), 0);
    assert_eq!(
        io.count(|call| matches!(call, Call::StoreCandidateTerm(_))),
        0
    );
    assert_eq!(member.election_state(), ElectionState::Canvass);
    let counts = member.counts();
    assert_eq!(counts.standby_vote_requests, 2, "{counts:?}");
    assert_eq!(
        (counts.votes_for, counts.votes_against),
        (0, 0),
        "{counts:?}"
    );
    // A request vote of a member the string does not name is no member's, and is
    // not counted.
    member
        .on_message(at(4), &request_vote(9, 2, 0), &mut io)
        .expect("no fault");
    assert_eq!(member.counts().standby_vote_requests, 2);
}

/// Once its election has closed, a standby does with a request vote's term what a
/// voter does: one above its own term is a ballot the cluster holds without its
/// leader, and it holds an election, to find the leader that ballot makes. One for
/// the term it is in moves nothing. It answers neither.
#[test]
fn a_standby_whose_election_closed_holds_an_election_on_a_request_vote_for_a_higher_term() {
    let mut io = Stub::new();
    let mut member = Consensus::with_standbys(&[0, 1], &[2], 2, 1).expect("a standby");
    member.do_work(at(1), &mut io).expect("no fault");
    let closed = follow_term(&mut member, &mut io, 2, &new_leadership_term(0, 0), 0);
    assert_eq!(member.leader_member_id(), 0);
    assert_eq!(member.counts().elections_closed, 1);

    member
        .on_message(at(closed + 1), &request_vote(1, 0, 0), &mut io)
        .expect("no fault");
    assert_eq!(member.election_state(), ElectionState::Closed);
    member
        .on_message(at(closed + 2), &request_vote(1, 1, 0), &mut io)
        .expect("no fault");
    assert_eq!(member.election_state(), ElectionState::Init);
    assert_eq!(sends(&io, Template::Vote), 0);
    let counts = member.counts();
    assert_eq!(counts.standby_vote_requests, 2, "{counts:?}");
    assert_eq!(counts.higher_terms_seen, 1, "{counts:?}");
}

/// A voter counts the voters alone in its canvass and in its ballot. It is a
/// unanimous candidate when the two other voters have reported, with the standby
/// silent. It asks the voters for their votes and not the standby. It leads at once
/// on the voters' votes although it has heard the standby, which casts none: a voter
/// it had heard that had cast none would hold it for the election timeout. And a
/// vote against from the standby, which a member that runs another string could
/// send, is not kept and loses it nothing.
#[test]
fn a_candidate_counts_the_voters_alone_and_waits_for_no_standby() {
    let mut io = Stub::new();
    let mut member = Consensus::with_standbys(&[0, 1, 2], &[3], 0, 1).expect("a voter");
    member.do_work(at(1), &mut io).expect("no fault");
    for voter in [1, 2] {
        member
            .on_message(at(2), &canvass_position(voter, -1, 0), &mut io)
            .expect("no fault");
    }
    member.do_work(at(3), &mut io).expect("no fault");
    assert_eq!(member.election_state(), ElectionState::Nominate);
    // Its canvass position went to every member, the standby among them.
    for other in [1, 2, 3] {
        assert_eq!(
            sends_to(&io, other, Template::CanvassPosition),
            1,
            "to {other}"
        );
    }
    // The delay is below half the election timeout.
    member.do_work(at(503), &mut io).expect("no fault");
    assert_eq!(member.election_state(), ElectionState::CandidateBallot);
    member.do_work(at(504), &mut io).expect("no fault");
    assert_eq!(sends(&io, Template::RequestVote), 2);
    assert_eq!(sends_to(&io, 3, Template::RequestVote), 0);

    // The standby is heard, and votes against: neither holds or loses the ballot.
    member
        .on_message(at(505), &canvass_position(3, -1, 0), &mut io)
        .expect("no fault");
    member
        .on_message(at(505), &vote(3, 0, 0, false), &mut io)
        .expect("no fault");
    member.do_work(at(506), &mut io).expect("no fault");
    assert_eq!(member.election_state(), ElectionState::CandidateBallot);
    for voter in [1, 2] {
        member
            .on_message(at(507), &vote(voter, 0, 0, true), &mut io)
            .expect("no fault");
    }
    member.do_work(at(508), &mut io).expect("no fault");
    assert_eq!(member.election_state(), ElectionState::LeaderLogReplication);
    assert_eq!(member.role(), Role::Leader);
    // The leader's new leadership term goes to every member, the standby among them,
    // once its log publication is added.
    io.clear();
    for ms in 509..520 {
        member.do_work(at(ms), &mut io).expect("no fault");
    }
    for other in [1, 2, 3] {
        assert!(
            sends_to(&io, other, Template::NewLeadershipTerm) >= 1,
            "to {other}"
        );
    }
}

/// A member reads a message whoever sent it. A voter votes, on the logs compared,
/// for a candidate that its own string marks a standby, and follows the leader that
/// candidate becomes: the candidate runs a string that gives it a vote, as it does
/// while a promotion's string reaches the members one after another.
#[test]
fn a_voter_votes_for_and_follows_a_member_its_own_string_marks_a_standby() {
    let mut io = Stub::new();
    let mut member = Consensus::with_standbys(&[0, 1], &[2], 0, 1).expect("a voter");
    member.do_work(at(1), &mut io).expect("no fault");
    member
        .on_message(at(2), &request_vote(2, 0, 0), &mut io)
        .expect("no fault");
    assert_eq!(sends_to(&io, 2, Template::Vote), 1);
    assert_eq!(member.election_state(), ElectionState::FollowerBallot);
    let counts = member.counts();
    assert_eq!(
        (counts.votes_for, counts.votes_against),
        (1, 0),
        "{counts:?}"
    );
    member
        .on_message(at(3), &new_leadership_term(2, 0), &mut io)
        .expect("no fault");
    assert_eq!(member.election_state(), ElectionState::FollowerReplay);
    assert_eq!(member.leader_member_id(), 2);
}

// ---- clusters in memory -----------------------------------------------------------

/// The voters among `ids` other than `leader`, given the standbys.
fn following_voters(ids: &[i32], standbys: &[i32], leader: i32) -> Vec<i32> {
    ids.iter()
        .copied()
        .filter(|id| *id != leader && !standbys.contains(id))
        .collect()
}

/// Starts each member of `order` again with a string that marks `standbys`, one
/// after another, and waits after each until every member of `running` has closed
/// its election behind one leader: a change of a standby's mark as an operator makes
/// it, the leader last.
fn change_marks(
    cluster: &mut Cluster,
    safety: &mut Safety,
    order: &[i32],
    standbys: &[i32],
    running: &[i32],
) {
    for &member in order {
        safety.restart_with(cluster, member, standbys);
        safety.run_until(
            cluster,
            30_000,
            &format!("the members settled after member {member} started with {standbys:?}"),
            |cluster| settled(cluster, running),
        );
    }
}

/// Three voters and a standby, at twenty seeds. The voters elect one of themselves,
/// and the winner's ballot closes at once on the voters' votes: a candidate that
/// waited for the standby, which it has heard and which casts no vote, would wait
/// the election timeout, 1 s. The standby follows: it closes its election behind the
/// leader, records what the leader appends, knows the commit position, and has cast
/// no vote, asked for none and been asked for none. Every member says the same
/// three voters.
#[test]
fn a_standby_follows_its_leader_and_neither_votes_nor_leads_nor_delays_an_election() {
    let ids = [0, 1, 2, 3];
    for seed in 1..=20 {
        let mut cluster = Cluster::with_standbys(&ids, &[3], seed).expect("four members");
        let mut safety = Safety::new(&ids);
        // When each member last became a candidate, and how long the winner's ballot
        // took.
        let mut candidate_since = [None; 4];
        let mut ballot_ms = None;
        for step in 0..3_000u64 {
            cluster.step();
            safety.check(&cluster);
            for (index, &id) in ids.iter().enumerate() {
                match cluster.role(id) {
                    Some(Role::Candidate) => {
                        candidate_since[index].get_or_insert(step);
                    }
                    Some(Role::Leader) => {
                        if let Some(since) = candidate_since[index].take() {
                            ballot_ms = Some(step - since);
                        }
                    }
                    _ => candidate_since[index] = None,
                }
            }
            if settled(&cluster, &ids) {
                break;
            }
        }
        assert!(settled(&cluster, &ids), "seed {seed}: no leader within 3 s");
        let leader = cluster.leader().expect("a leader");
        assert_ne!(leader, 3, "seed {seed}");
        let ballot_ms = ballot_ms.expect("the leader was a candidate first");
        assert!(
            ballot_ms < 100,
            "seed {seed}: the winner's ballot took {ballot_ms} ms"
        );

        let standby = cluster.member(3).expect("a member");
        assert!(standby.is_standby(), "seed {seed}");
        assert_eq!(standby.role(), Role::Follower, "seed {seed}");
        assert_eq!(standby.leader_member_id(), leader, "seed {seed}");
        let counts = standby.counts();
        assert_eq!(counts.elections_closed, 1, "seed {seed}: {counts:?}");
        assert_eq!(
            (
                counts.candidacies,
                counts.terms_led,
                counts.votes_for,
                counts.votes_against,
                counts.standby_vote_requests
            ),
            (0, 0, 0, 0, 0),
            "seed {seed}: {counts:?}"
        );
        for id in ids {
            let member = cluster.member(id).expect("a member");
            assert_eq!(member.voters(), 0b0111, "seed {seed}, member {id}");
            assert_eq!(member.is_standby(), id == 3, "seed {seed}, member {id}");
        }

        let mut end = 0;
        for entry in 0..100u32 {
            end = cluster
                .offer(&entry.to_le_bytes())
                .expect("the leader appends");
        }
        safety.run(&mut cluster, 500);
        for id in ids {
            assert_eq!(
                cluster.commit_position(id),
                Some(end),
                "seed {seed}, member {id}"
            );
            assert_eq!(
                cluster.log(id),
                cluster.log(leader),
                "seed {seed}, member {id}"
            );
        }
    }
}

/// A standby that does not run delays no election: the voters are unanimous among
/// themselves and one leads within a second, where a voter that did not run would
/// hold them in canvass for the startup canvass timeout, 60 s. The standby starts
/// late, catches up and follows.
#[test]
fn a_standby_that_does_not_run_holds_no_voter_in_canvass() {
    let ids = [0, 1, 2, 3];
    let voters = [0, 1, 2];
    let mut cluster = Cluster::with_standbys(&ids, &[3], 3).expect("four members");
    let mut safety = Safety::new(&ids);
    cluster.halt(3);
    let took = safety.run_until(&mut cluster, 1_500, "the voters' election", |cluster| {
        settled(cluster, &voters)
    });
    assert!(took < 1_500, "the voters settled after {took} ms");
    let leader = cluster.leader().expect("a leader");
    let end = cluster
        .offer(b"before the standby")
        .expect("the leader appends");
    safety.run(&mut cluster, 500);
    assert_eq!(cluster.commit_position(leader), Some(end));
    assert_eq!(cluster.append_position(3), Some(0));

    cluster.resume(3);
    safety.run_until(&mut cluster, 5_000, "the standby joined", |cluster| {
        settled(cluster, &ids) && cluster.append_position(3) == Some(end)
    });
    assert_eq!(cluster.log(3), cluster.log(leader));
    let late = cluster.counts(3).expect("a member");
    assert_eq!(
        (late.candidacies, late.elections_failed),
        (0, 0),
        "{late:?}"
    );
}

/// A standby's append position commits nothing, and a standby keeps no leader its
/// quorum. With the two voters that follow stopped, the leader's entry is on the
/// leader and on the standby and is not committed. After the leader heartbeat
/// timeout the leader has lost its quorum, with the standby reporting all the while.
/// Then nobody leads: one voter runs, and the standby never nominates itself. When
/// the voters run again they elect a leader, and the four hold one log.
#[test]
fn a_standbys_append_position_commits_nothing_and_keeps_no_leader_its_quorum() {
    let ids = [0, 1, 2, 3];
    let mut cluster = Cluster::with_standbys(&ids, &[3], 1).expect("four members");
    let mut safety = Safety::new(&ids);
    safety.run_until(&mut cluster, 3_000, "the first election", |cluster| {
        settled(cluster, &ids)
    });
    let leader = cluster.leader().expect("a leader");
    let committed = cluster
        .offer(b"on three voters")
        .expect("the leader appends");
    safety.run(&mut cluster, 500);
    for id in ids {
        assert_eq!(cluster.commit_position(id), Some(committed), "member {id}");
    }

    let stopped = following_voters(&ids, &[3], leader);
    for &id in &stopped {
        cluster.halt(id);
    }
    let uncommitted = cluster
        .offer(b"on the leader and the standby")
        .expect("the leader appends");
    safety.run(&mut cluster, 500);
    assert_eq!(cluster.append_position(leader), Some(uncommitted));
    assert_eq!(cluster.append_position(3), Some(uncommitted));
    assert_eq!(cluster.commit_position(leader), Some(committed));
    assert_eq!(cluster.commit_position(3), Some(committed));

    safety.run_until(
        &mut cluster,
        11_000,
        "the leader lost its quorum",
        |cluster| cluster.leader().is_none(),
    );
    let lost = cluster.counts(leader).expect("a member");
    assert_eq!(lost.leader_quorum_losses, 1, "{lost:?}");
    // One voter and the standby run: no quorum, and the standby is no candidate.
    safety.run(&mut cluster, 30_000);
    assert_eq!(cluster.leader(), None);
    assert_eq!(cluster.election_state(3), Some(ElectionState::Canvass));
    let standby = cluster.counts(3).expect("a member");
    assert_eq!(
        (standby.candidacies, standby.terms_led),
        (0, 0),
        "{standby:?}"
    );
    assert_eq!(cluster.commit_position(3), Some(committed));

    for &id in &stopped {
        cluster.resume(id);
    }
    safety.run_until(
        &mut cluster,
        30_000,
        "the voters elected a leader",
        |cluster| settled(cluster, &ids),
    );
    let leader = cluster.leader().expect("a leader");
    assert_ne!(leader, 3);
    safety.run(&mut cluster, 500);
    for id in ids {
        assert_eq!(cluster.log(id), cluster.log(leader), "member {id}");
        assert!(holds(&cluster.log(id), b"on three voters"), "member {id}");
    }
}

/// The only voter of a string leads from init, as a cluster of one member does, and
/// its standbys follow it: what it appends is committed by its own record alone,
/// and both standbys hold it.
#[test]
fn the_only_voter_of_a_string_leads_from_init_and_its_standbys_follow() {
    let ids = [0, 1, 2];
    let mut cluster = Cluster::with_standbys(&ids, &[1, 2], 1).expect("three members");
    let mut safety = Safety::new(&ids);
    safety.run_until(
        &mut cluster,
        2_000,
        "the voter led and the standbys joined",
        |cluster| settled(cluster, &ids),
    );
    assert_eq!(cluster.leader(), Some(0));
    let end = cluster.offer(b"one voter").expect("the leader appends");
    safety.run(&mut cluster, 500);
    for id in ids {
        assert_eq!(cluster.commit_position(id), Some(end), "member {id}");
        assert_eq!(
            cluster.member(id).expect("a member").voters(),
            0b001,
            "member {id}"
        );
    }
    let led = cluster.counts(0).expect("a member");
    assert_eq!((led.candidacies, led.terms_led), (0, 1), "{led:?}");
}

/// Two strings side by side, one mark apart. The three voters are started again with
/// a string that gives member 3 a vote, and member 3 still runs the string that
/// marks it a standby. A candidate then asks member 3 for its vote: member 3 answers
/// nothing and counts the request, and the candidate, which has heard it, waits the
/// election timeout and leads on the votes of three of its four voters. Nothing
/// committed is lost on the way. When member 3 is started with the new string, the
/// four say the same four voters.
#[test]
fn a_standby_asked_for_a_vote_by_a_member_that_runs_another_string_answers_nothing() {
    let ids = [0, 1, 2, 3];
    let mut cluster = Cluster::with_standbys(&ids, &[3], 1).expect("four members");
    let mut safety = Safety::new(&ids);
    safety.run_until(&mut cluster, 3_000, "the first election", |cluster| {
        settled(cluster, &ids)
    });
    let leader = cluster.leader().expect("a leader");
    let before = cluster
        .offer(b"before the change")
        .expect("the leader appends");
    safety.run(&mut cluster, 500);

    // The voters take the new string, the leader last. The leader's start is an
    // election, in which every candidate asks member 3.
    let mut order = following_voters(&ids, &[3], leader);
    order.push(leader);
    change_marks(&mut cluster, &mut safety, &order, &[], &ids);
    let asked = cluster.counts(3).expect("a member");
    assert!(asked.standby_vote_requests >= 1, "{asked:?}");
    assert_eq!((asked.votes_for, asked.votes_against), (0, 0), "{asked:?}");
    for id in [0, 1, 2] {
        assert_eq!(
            cluster.member(id).expect("a member").voters(),
            0b1111,
            "member {id}"
        );
    }
    let standby = cluster.member(3).expect("a member");
    assert_eq!(standby.voters(), 0b0111);
    assert!(standby.is_standby());
    let leader = cluster.leader().expect("a leader");
    assert_ne!(leader, 3);
    safety.run(&mut cluster, 500);
    let commit = cluster.commit_position(leader).expect("the leader");
    assert!(commit >= before, "commit position {commit}, below {before}");

    // Member 3 takes the new string: the change has reached every member.
    change_marks(&mut cluster, &mut safety, &[3], &[], &ids);
    for id in ids {
        let member = cluster.member(id).expect("a member");
        assert_eq!(member.voters(), 0b1111, "member {id}");
        assert!(!member.is_standby(), "member {id}");
    }
    assert_eq!(
        cluster.counts(3).expect("a member").standby_vote_requests,
        0
    );
    let after = cluster
        .offer(b"after the change")
        .expect("the leader appends");
    safety.run(&mut cluster, 500);
    for id in ids {
        assert_eq!(cluster.commit_position(id), Some(after), "member {id}");
        assert!(holds(&cluster.log(id), b"before the change"), "member {id}");
    }
}

/// A standby promoted leads, with nothing committed lost. Two voters and a standby
/// commit a hundred entries. The standby is promoted: each member is started again
/// with a string that gives it a vote, the leader last. Then one voter stops, and
/// the other starts again, so that it canvasses for the startup canvass timeout and
/// nominates nothing. The promoted member is the one that can be a candidate: it
/// wins the other's vote, leads, and commits with it. Every entry committed before
/// is in its log, which the check holds at every step.
#[test]
fn a_standby_promoted_leads_and_holds_every_committed_entry() {
    let ids = [0, 1, 2];
    for seed in 1..=5 {
        let mut cluster = Cluster::with_standbys(&ids, &[2], seed).expect("three members");
        let mut safety = Safety::new(&ids);
        safety.run_until(&mut cluster, 3_000, "the first election", |cluster| {
            settled(cluster, &ids)
        });
        let first = cluster.leader().expect("a leader");
        assert_ne!(first, 2, "seed {seed}");
        let mut before = 0;
        for entry in 0..100u32 {
            before = cluster
                .offer(&entry.to_le_bytes())
                .expect("the leader appends");
        }
        safety.run(&mut cluster, 500);
        for id in ids {
            assert_eq!(
                cluster.commit_position(id),
                Some(before),
                "seed {seed}, member {id}"
            );
        }

        // The promotion: the standby and the voter that follows, then the leader.
        let other = following_voters(&ids, &[2], first);
        change_marks(&mut cluster, &mut safety, &[2, other[0], first], &[], &ids);
        for id in ids {
            let member = cluster.member(id).expect("a member");
            assert_eq!(member.voters(), 0b111, "seed {seed}, member {id}");
            assert!(!member.is_standby(), "seed {seed}, member {id}");
        }
        let between = cluster
            .offer(b"under three voters")
            .expect("the leader appends");
        safety.run(&mut cluster, 500);
        for id in ids {
            assert_eq!(
                cluster.commit_position(id),
                Some(between),
                "seed {seed}, member {id}"
            );
        }

        // Member 0 stops. Member 1 starts again and canvasses, waiting for every
        // member. Member 2 leads: it led already, and keeps its quorum with member
        // 1's reports, or it is the one member that can be a candidate.
        cluster.halt(0);
        safety.restart(&mut cluster, 1);
        safety.run_until(&mut cluster, 15_000, "the promoted member led", |cluster| {
            settled(cluster, &[1, 2]) && cluster.leader() == Some(2)
        });
        assert_eq!(cluster.role(2), Some(Role::Leader), "seed {seed}");
        let promoted = cluster.counts(2).expect("a member");
        assert!(promoted.terms_led >= 1, "seed {seed}: {promoted:?}");
        assert_eq!(
            cluster.counts(1).expect("a member").candidacies,
            0,
            "seed {seed}"
        );
        let led = cluster
            .offer(b"led by the promoted standby")
            .expect("the promoted member appends");
        safety.run(&mut cluster, 500);
        for id in [1, 2] {
            assert_eq!(
                cluster.commit_position(id),
                Some(led),
                "seed {seed}, member {id}"
            );
        }
        let log = cluster.log(2);
        assert!(holds(&log, b"under three voters"), "seed {seed}");
        assert!(holds(&log, &99u32.to_le_bytes()), "seed {seed}");

        // Member 0 runs again, finds the later term and follows the promoted member.
        cluster.resume(0);
        safety.run_until(&mut cluster, 30_000, "member 0 followed", |cluster| {
            settled(cluster, &ids) && cluster.append_position(0) == Some(led)
        });
        assert_eq!(cluster.log(0), cluster.log(2), "seed {seed}");
    }
}

/// A lost voter is replaced by a standby in two changes, one mark each, with an
/// election closed in each string before the next. Three voters and a standby; a
/// voter that follows is lost. First that voter is marked a standby: two voters,
/// both needed, as they are with the third lost. Then the standby is promoted: three
/// voters. The promoted member then leads, by the way of the promotion's test, and
/// its log holds what was committed in each of the three strings.
#[test]
fn a_lost_voter_is_replaced_by_a_standby_in_two_changes_of_one_mark_each() {
    let ids = [0, 1, 2, 3];
    for seed in 1..=5 {
        let mut cluster = Cluster::with_standbys(&ids, &[3], seed).expect("four members");
        let mut safety = Safety::new(&ids);
        safety.run_until(&mut cluster, 3_000, "the first election", |cluster| {
            settled(cluster, &ids)
        });
        let leader = cluster.leader().expect("a leader");
        cluster
            .offer(b"committed by three voters")
            .expect("the leader appends");
        safety.run(&mut cluster, 500);

        let following = following_voters(&ids, &[3], leader);
        let (lost, kept) = (following[0], following[1]);
        cluster.halt(lost);
        let running = [leader, kept, 3];

        // The first change: the lost voter is a standby. The leader starts last.
        change_marks(
            &mut cluster,
            &mut safety,
            &[3, kept, leader],
            &[lost, 3],
            &running,
        );
        let mask = cluster.member(leader).expect("a member").voters();
        assert_eq!(mask.count_ones(), 2, "seed {seed}");
        for id in running {
            assert_eq!(
                cluster.member(id).expect("a member").voters(),
                mask,
                "seed {seed}, member {id}"
            );
        }
        let leader = cluster.leader().expect("a leader");
        assert_ne!(leader, 3, "seed {seed}");
        cluster
            .offer(b"committed by two voters")
            .expect("the leader appends");
        safety.run(&mut cluster, 500);

        // The second change: the standby has a vote.
        let follower = if leader == kept { running[0] } else { kept };
        change_marks(
            &mut cluster,
            &mut safety,
            &[3, follower, leader],
            &[lost],
            &running,
        );
        for id in running {
            let member = cluster.member(id).expect("a member");
            assert_eq!(member.voters().count_ones(), 3, "seed {seed}, member {id}");
            assert!(!member.is_standby(), "seed {seed}, member {id}");
        }
        let after = cluster
            .offer(b"committed with the promoted member")
            .expect("the leader appends");
        safety.run(&mut cluster, 500);
        for id in running {
            assert_eq!(
                cluster.commit_position(id),
                Some(after),
                "seed {seed}, member {id}"
            );
        }

        // One of the two older voters stops and the other starts again: the
        // promoted member leads.
        let (stops, restarts) = (running[0], running[1]);
        cluster.halt(stops);
        safety.restart(&mut cluster, restarts);
        safety.run_until(&mut cluster, 15_000, "the promoted member led", |cluster| {
            settled(cluster, &[restarts, 3]) && cluster.leader() == Some(3)
        });
        let log = cluster.log(3);
        for payload in [
            &b"committed by three voters"[..],
            b"committed by two voters",
            b"committed with the promoted member",
        ] {
            assert!(holds(&log, payload), "seed {seed}");
        }
    }
}

/// What the rules of a change are for (`spec/cluster.md`, "Changing a standby's
/// mark"; ADR 0134): two marks changed with no election closed between them elect a
/// leader that lacks a committed entry. Five voters and a standby. Members 0, 2 and
/// 3, a quorum, elect a leader and commit an entry, parted from members 1, 4 and 5.
/// Then members 1, 4 and 5 are started with a string that is two marks from the one
/// the others run: member 2 a standby and member 5 a voter. They are a quorum of
/// that string's five voters, and none of them holds the entry or voted in its
/// term. One of them leads, in the same term as the first leader, with a log that
/// does not hold the committed entry.
///
/// The test holds the harness to finding that, and the page to its reason. The same
/// members started with one mark changed, member 2 a standby alone, are two of four
/// voters, and elect nobody: that is the test below.
#[test]
fn two_marks_changed_at_once_elect_a_leader_that_lacks_a_committed_entry() {
    let ids = [0, 1, 2, 3, 4, 5];
    let (one_side, other_side) = ([0, 2, 3], [1, 4, 5]);
    let mut cluster = Cluster::with_standbys(&ids, &[5], 1).expect("six members");
    for a in one_side {
        for b in other_side {
            cluster.cut_between(a, b);
        }
    }
    // Three of five voters: a quorum candidate at the startup canvass timeout.
    for _ in 0..62_000 {
        cluster.step();
        if settled(&cluster, &one_side) {
            break;
        }
    }
    assert!(
        settled(&cluster, &one_side),
        "members 0, 2 and 3 elect a leader"
    );
    let first = cluster.leader().expect("a leader");
    let end = cluster
        .offer(b"committed by members 0, 2 and 3")
        .expect("the leader appends");
    cluster.run_ms(500);
    assert_eq!(cluster.commit_position(first), Some(end));
    let first_term = cluster
        .member(first)
        .expect("a member")
        .leadership_term_id();

    // Two marks at once: member 2 a standby and member 5 a voter.
    for member in other_side {
        cluster.restart_with(member, &[2]).expect("a voter is left");
    }
    let second_leader = |cluster: &Cluster| {
        other_side
            .into_iter()
            .find(|&id| cluster.role(id) == Some(Role::Leader))
    };
    for _ in 0..62_000 {
        cluster.step();
        if second_leader(&cluster).is_some() {
            break;
        }
    }
    let second = second_leader(&cluster).expect("members 1, 4 and 5 elect a leader");
    // Two leaders of one term, and the second without the entry the first committed.
    assert_eq!(cluster.role(first), Some(Role::Leader));
    assert_eq!(
        cluster
            .member(second)
            .expect("a member")
            .leadership_term_id(),
        first_term
    );
    assert!(holds(
        &cluster.log(first),
        b"committed by members 0, 2 and 3"
    ));
    assert!(!holds(
        &cluster.log(second),
        b"committed by members 0, 2 and 3"
    ));
}

/// The same parting with one mark changed. Members 1, 4 and 5 are started with a
/// string that marks member 2 a standby and nothing else: they hold two votes of
/// four, and elect nobody for longer than the startup canvass timeout. When the
/// parting ends, the members elect a leader that holds the committed entry: the
/// string of members 0 and 3 and the string of members 1, 4 and 5 are one mark
/// apart, and a quorum of the one shares a voter with a quorum of the other.
#[test]
fn one_mark_changed_elects_no_leader_without_the_committed_entry() {
    let ids = [0, 1, 2, 3, 4, 5];
    let (one_side, other_side) = ([0, 2, 3], [1, 4, 5]);
    let mut cluster = Cluster::with_standbys(&ids, &[5], 1).expect("six members");
    let mut safety = Safety::new(&ids);
    for a in one_side {
        for b in other_side {
            cluster.cut_between(a, b);
        }
    }
    safety.run_until(
        &mut cluster,
        62_000,
        "members 0, 2 and 3 elect a leader",
        |cluster| settled(cluster, &one_side),
    );
    let first = cluster.leader().expect("a leader");
    let end = cluster
        .offer(b"committed by members 0, 2 and 3")
        .expect("the leader appends");
    safety.run(&mut cluster, 500);
    assert_eq!(cluster.commit_position(first), Some(end));

    for member in other_side {
        safety.restart_with(&mut cluster, member, &[2, 5]);
    }
    safety.run(&mut cluster, 70_000);
    for member in other_side {
        assert_eq!(
            cluster.role(member),
            Some(Role::Follower),
            "member {member}"
        );
        let counts = cluster.counts(member).expect("a member");
        assert_eq!(counts.terms_led, 0, "member {member}: {counts:?}");
    }

    for a in one_side {
        for b in other_side {
            cluster.heal_between(a, b);
        }
    }
    safety.run_until(&mut cluster, 30_000, "the six settled", |cluster| {
        settled(cluster, &ids)
    });
    let leader = cluster.leader().expect("a leader");
    safety.run(&mut cluster, 500);
    for id in ids {
        assert!(
            holds(&cluster.log(id), b"committed by members 0, 2 and 3"),
            "member {id}"
        );
        assert_eq!(cluster.log(id), cluster.log(leader), "member {id}");
    }
}

/// Steps `cluster` until `done`, for `steps` steps at the most; whether it was.
fn stepped_until(cluster: &mut Cluster, steps: usize, done: impl Fn(&Cluster) -> bool) -> bool {
    for _ in 0..steps {
        if done(cluster) {
            return true;
        }
        cluster.step();
    }
    done(cluster)
}

/// Five voters and a standby, member 5, which is being promoted: members 5, 4 and 3
/// were started with the new string, of six voters, and members 0, 1 and 2 still
/// run the string before, as they do in the middle of a change. Returns the cluster
/// at a seed at which a member that runs the new string leads, every member's
/// election closed behind it, and that leader's id.
fn half_promoted() -> (Cluster, i32) {
    let ids = [0, 1, 2, 3, 4, 5];
    for seed in 1..=16 {
        let mut cluster = Cluster::with_standbys(&ids, &[5], seed).expect("six members");
        assert!(
            stepped_until(&mut cluster, 62_000, |cluster| settled(cluster, &ids)),
            "seed {seed}: the members elect a leader"
        );
        for member in [5, 4, 3] {
            cluster.restart_with(member, &[]).expect("a voter is left");
            assert!(
                stepped_until(&mut cluster, 62_000, |cluster| settled(cluster, &ids)),
                "seed {seed}: the members follow one leader once member {member} has started"
            );
        }
        if let Some(leader) = cluster.leader().filter(|leader| *leader >= 3) {
            return (cluster, leader);
        }
    }
    panic!("no seed of sixteen at which a member that runs the new string leads");
}

/// What the quorum in rule 2 is for (`spec/cluster.md`, "Changing a standby's mark";
/// ADR 0134): members that run, were all started with the new string and have
/// closed their elections behind one leader among them do not make a change
/// complete when they are fewer than a quorum of that string's voters.
///
/// The cluster of [`half_promoted`]. Members 0, 1 and 2 are parted from members 3,
/// 4 and 5, whose processes stand still, the leader's among them. Members 0, 1 and
/// 2 are a quorum of their own string's five voters: they elect a leader of a later
/// term and commit an entry. Then they stop. Members 3, 4 and 5 are now every
/// member that runs. Each was started with the new string, and each still says
/// what it said when it stood still: its election closed, behind the one leader
/// among them. They are three of six voters. The next change, one mark from the
/// new string, makes member 0 a standby: members 3, 4 and 5, started with it, are a
/// quorum of its five voters, and elect a leader that lacks the committed entry.
#[test]
fn a_change_is_not_complete_on_fewer_members_than_a_quorum_of_its_voters() {
    let (old_side, new_side) = ([0, 1, 2], [3, 4, 5]);
    let (mut cluster, stale) = half_promoted();
    let stale_term = cluster
        .member(stale)
        .expect("a member")
        .leadership_term_id();
    for a in old_side {
        for b in new_side {
            cluster.cut_between(a, b);
        }
    }
    for member in new_side {
        cluster.stall(member);
    }
    // Members 0, 1 and 2 hear nothing of their leader for the leader heartbeat
    // timeout, elect one of themselves by their own string, and commit.
    assert!(
        stepped_until(&mut cluster, 62_000, |cluster| {
            settled(cluster, &old_side) && cluster.leader().is_some_and(|leader| leader < 3)
        }),
        "members 0, 1 and 2 elect a leader"
    );
    let later = cluster.leader().expect("a leader");
    let later_term = cluster
        .member(later)
        .expect("a member")
        .leadership_term_id();
    assert!(later_term > stale_term);
    let end = cluster
        .offer(b"committed by members 0, 1 and 2")
        .expect("the leader appends");
    cluster.run_ms(500);
    assert_eq!(cluster.commit_position(later), Some(end));
    for member in old_side {
        cluster.halt(member);
    }

    // What an operator reads of the members that run: each was started with the
    // new string, of six voters, and has closed its election behind one leader
    // among them. Three of six is no quorum of them.
    for member in new_side {
        let core = cluster.member(member).expect("a member");
        assert_eq!(core.voters(), 0b11_1111, "member {member}");
        assert_eq!(
            (core.election_state(), core.leader_member_id()),
            (ElectionState::Closed, stale),
            "member {member}"
        );
    }
    assert_eq!(cluster.role(stale), Some(Role::Leader));

    // The next change, one mark from the new string: member 0 a standby.
    for member in new_side {
        cluster.restart_with(member, &[0]).expect("a voter is left");
    }
    let second_leader = |cluster: &Cluster| {
        new_side
            .into_iter()
            .find(|&id| cluster.role(id) == Some(Role::Leader))
    };
    assert!(
        stepped_until(&mut cluster, 62_000, |cluster| second_leader(cluster)
            .is_some()),
        "members 3, 4 and 5 elect a leader"
    );
    let second = second_leader(&cluster).expect("a leader");
    // A leader by the rules' three strings, each one mark from the one before it,
    // that does not hold what a leader before it committed.
    assert!(holds(
        &cluster.log(later),
        b"committed by members 0, 1 and 2"
    ));
    assert!(!holds(
        &cluster.log(second),
        b"committed by members 0, 1 and 2"
    ));
    assert!(
        cluster
            .member(second)
            .expect("a member")
            .leadership_term_id()
            <= later_term,
        "the second leader's term is no later than the term it never heard of"
    );
}

/// The same cluster with the quorum that rule 2 asks for. Member 1 is started with
/// the new string as well, so that members 1, 3, 4 and 5 run it and follow one
/// leader: four of its six voters, a quorum. Members 0 and 2 are parted from them
/// and are two of the five voters of the string they run: they elect nobody, for
/// longer than the startup canvass timeout, so no leader of a later term commits
/// by the string before. The change is complete, and the next one loses nothing:
/// with member 0 made a standby, the leader that members 1, 3, 4 and 5 then have
/// holds the entry they committed.
#[test]
fn a_change_is_complete_when_a_quorum_of_its_voters_follows_one_leader() {
    let (parted, quorum) = ([0, 2], [1, 3, 4, 5]);
    let (mut cluster, _) = half_promoted();
    cluster.restart_with(1, &[]).expect("a voter is left");
    assert!(
        stepped_until(&mut cluster, 62_000, |cluster| settled(
            cluster,
            &[0, 1, 2, 3, 4, 5]
        )),
        "the members follow one leader once member 1 has started"
    );
    for a in parted {
        for b in quorum {
            cluster.cut_between(a, b);
        }
    }
    // The four elect a leader among them when the one they had is parted from
    // them, and commit by the new string: four of six.
    assert!(
        stepped_until(&mut cluster, 62_000, |cluster| {
            settled(cluster, &quorum)
                && cluster
                    .leader()
                    .is_some_and(|leader| leader != 0 && leader != 2)
        }),
        "members 1, 3, 4 and 5 follow one leader among them"
    );
    let end = cluster
        .offer(b"committed by members 1, 3, 4 and 5")
        .expect("the leader appends");
    cluster.run_ms(500);
    let leader = cluster.leader().expect("a leader");
    assert_eq!(cluster.commit_position(leader), Some(end));
    // Members 0 and 2, by the string before, are two of five: no leader.
    for _ in 0..62_000 {
        cluster.step();
        for member in parted {
            assert_ne!(cluster.role(member), Some(Role::Leader), "member {member}");
        }
    }
    // The next change, member 0 a standby, on the four that run.
    for member in quorum {
        cluster.restart_with(member, &[0]).expect("a voter is left");
        assert!(
            stepped_until(&mut cluster, 62_000, |cluster| settled(cluster, &quorum)),
            "the four follow one leader once member {member} has started"
        );
    }
    let leader = cluster.leader().expect("a leader");
    assert!(holds(
        &cluster.log(leader),
        b"committed by members 1, 3, 4 and 5"
    ));
}
