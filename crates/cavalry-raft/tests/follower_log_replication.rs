//! Follower log replication, state 9, on one core driven by hand (`spec/cluster.md`,
//! "Roles and the election", row 9, and "Messages during an election"; ADR 0086). A
//! member whose append position is below its leader's term base replicates one term
//! it lacks from the leader's recordings, to that term's end, and canvasses again.
//!
//! The member of every test here is member 0 of three. It followed term 0, led by
//! member 1 from position 0, and lost that leader with some bytes of the term in its
//! log. The leader that tells it what to replicate is member 2.

mod common;

use cavalry_proto::cluster::{LEADER_HEARTBEAT_TIMEOUT_NS, Message, Template, replication_record};
use cavalry_raft::{Consensus, ElectionState, Role};
use common::{Call, Stub, at, follow_term, lose_the_leader, new_leadership_term};

/// The leader heartbeat timeout in the tests' milliseconds.
const TIMEOUT_MS: u64 = LEADER_HEARTBEAT_TIMEOUT_NS / 1_000_000;

fn work(member: &mut Consensus, io: &mut Stub, ms: u64) {
    member.do_work(at(ms), io).expect("no fault");
}

fn read(member: &mut Consensus, io: &mut Stub, ms: u64, message: &Message<'_>) {
    member.on_message(at(ms), message, io).expect("no fault");
}

/// Member 0 after term 0: in canvass, its log of term 0 ending at `appended`, its
/// record of terms (0, 0), the calls of its first election forgotten. Returns the
/// member and the next millisecond.
fn after_term_zero(io: &mut Stub, appended: i64) -> (Consensus, u64) {
    let mut member = Consensus::with_election(&[0, 1, 2], 0, 1).expect("a member");
    work(&mut member, io, 1);
    let closed = follow_term(&mut member, io, 2, &new_leadership_term(1, 0), 0);
    let ms = lose_the_leader(&mut member, io, closed, appended);
    io.clear();
    io.last_append = None;
    (member, ms)
}

/// Member 2's new leadership term for `term`, which begins at `base`, to a member
/// whose log is of `log_term`, with `next` as its three next fields and `commit` as
/// the leader's commit position.
fn told(term: i64, base: i64, log_term: i64, next: [i64; 3], commit: i64) -> Message<'static> {
    let [
        next_leadership_term_id,
        next_term_base_log_position,
        next_log_position,
    ] = next;
    Message::NewLeadershipTerm {
        log_leadership_term_id: log_term,
        next_leadership_term_id,
        next_term_base_log_position,
        next_log_position,
        leadership_term_id: term,
        term_base_log_position: base,
        log_position: base,
        leader_recording_id: 5,
        timestamp: 1_700_000_000_000,
        leader_member_id: 2,
        log_session_id: 78,
        app_version: 0,
        is_startup: false,
        commit_position: commit,
    }
}

fn commit_position(term: i64, position: i64) -> Message<'static> {
    Message::CommitPosition {
        leadership_term_id: term,
        log_position: position,
        leader_member_id: 2,
    }
}

fn recorded(io: &Stub) -> Vec<(i64, i64)> {
    io.terms.iter().flatten().copied().collect()
}

fn place(io: &Stub, call: Call) -> usize {
    io.first(|made| *made == call)
        .unwrap_or_else(|| panic!("{call:?} was not made"))
}

/// A member whose append position is below the next term's base lacks the rest of its
/// own log term. It subscribes, then asks once, from its append position; reports its
/// position as the replay moves it, with its own term; and at the stop, which is the
/// leader's base here, records the leader's term before it reports the stop. The
/// commit position it was told is the stop, so it waits for nothing: the subscription
/// goes and it canvasses again, two steps of its election. The next new leadership
/// term finds it level with the leader's base, and it joins.
#[test]
fn a_member_below_the_next_base_replicates_the_rest_of_its_own_term() {
    let mut io = Stub::new();
    let (mut member, ms) = after_term_zero(&mut io, 32);
    let steps = member.counts().election_steps;
    // Term 1 begins at 64: the member lacks 32 bytes of term 0.
    read(&mut member, &mut io, ms, &told(1, 64, 0, [1, 64, -1], 64));
    assert_eq!(
        member.election_state(),
        ElectionState::FollowerLogReplication
    );
    assert_eq!(member.leader_member_id(), 2);
    assert_eq!(member.leadership_term_id(), 1);
    assert_eq!(member.role(), Role::Follower);
    assert_eq!(io.made, 0, "nothing is asked for by the message");

    work(&mut member, &mut io, ms + 1);
    assert!(place(&io, Call::AddCatchup) < place(&io, Call::CatchupPosition(2, 1, 32)));
    assert_eq!(member.counts().log_replications, 1);
    assert_eq!(
        io.last_append,
        Some((2, 0, 32)),
        "its own term, where it is"
    );
    assert_eq!(recorded(&io), vec![(0, 0)], "its term is in its record");

    // The replay's image joins at 32 and brings 16 bytes, then the rest.
    io.catchup_image = Some(32);
    io.append_position = 48;
    work(&mut member, &mut io, ms + 2);
    assert_eq!(io.last_append, Some((2, 0, 48)));
    assert_eq!(
        member.election_state(),
        ElectionState::FollowerLogReplication
    );
    io.clear();
    io.append_position = 64;
    work(&mut member, &mut io, ms + 3);
    // What it records at the stop is what the page's arithmetic gives.
    assert_eq!(replication_record(0, 0, 64, 1, 64).leader, Some((1, 64)));
    let record = place(&io, Call::RecordTerm(1, 64));
    let report = place(
        &io,
        Call::Send {
            to: 2,
            template: Template::AppendPosition,
        },
    );
    assert!(record < report, "recorded before the stop is reported");
    assert_eq!(io.last_append, Some((2, 1, 64)), "with the leader's term");
    assert_eq!(io.count(|call| *call == Call::RemoveCatchup), 1);
    assert_eq!(member.election_state(), ElectionState::Canvass);
    assert_eq!(member.counts().election_steps, steps + 2);
    assert_eq!(member.counts().log_replications, 1, "it asked once");
    assert_eq!(member.counts().elections_failed, 0);

    // Level with the leader's base, with the term in its record: it joins.
    io.clear();
    read(&mut member, &mut io, ms + 4, &told(1, 64, 1, [-1; 3], 64));
    assert_eq!(member.election_state(), ElectionState::FollowerReplay);
    assert_eq!(io.count(|call| matches!(call, Call::RecordTerm(..))), 0);
}

/// A member at the next term's base holds none of that term. It records the term at
/// that base before it subscribes or asks, as a member that joins a leader at its
/// base records the leader's term there. A record that cannot be written is a fault:
/// the member stays, has asked for nothing, and the next cycle records again. The
/// stop is below the leader's base here, so at the stop it records nothing more,
/// waits for no commit position, and canvasses.
#[test]
fn a_member_at_the_next_base_records_that_term_before_it_asks() {
    let mut io = Stub::new();
    let (mut member, ms) = after_term_zero(&mut io, 64);
    // The leader leads term 3 from 200; term 1 is the log from 64 to 160.
    read(&mut member, &mut io, ms, &told(3, 200, 0, [1, 64, 160], 0));
    assert_eq!(
        member.election_state(),
        ElectionState::FollowerLogReplication
    );

    io.fail_record = true;
    assert_eq!(
        member.do_work(at(ms + 1), &mut io),
        Err("the terms file cannot be written")
    );
    assert_eq!(io.count(|call| *call == Call::AddCatchup), 0);
    assert_eq!(member.counts().log_replications, 0);
    io.fail_record = false;
    io.clear();
    work(&mut member, &mut io, ms + 2);
    let record = place(&io, Call::RecordTerm(1, 64));
    assert!(record < place(&io, Call::AddCatchup));
    assert!(record < place(&io, Call::CatchupPosition(2, 3, 64)));
    assert_eq!(recorded(&io), vec![(0, 0), (1, 64)]);
    assert_eq!(io.last_append, Some((2, 1, 64)), "the term it began");

    io.catchup_image = Some(64);
    io.append_position = 160;
    io.clear();
    work(&mut member, &mut io, ms + 3);
    assert_eq!(replication_record(1, 64, 160, 3, 200).leader, None);
    assert_eq!(io.count(|call| matches!(call, Call::RecordTerm(..))), 0);
    assert_eq!(io.last_append, Some((2, 1, 160)));
    assert_eq!(member.election_state(), ElectionState::Canvass);
    assert_eq!(recorded(&io), vec![(0, 0), (1, 64)]);

    // The next new leadership term carries it on to the next term, whose end is
    // the leader's base.
    read(
        &mut member,
        &mut io,
        ms + 4,
        &told(3, 200, 1, [2, 160, 200], 0),
    );
    assert_eq!(
        member.election_state(),
        ElectionState::FollowerLogReplication
    );
    work(&mut member, &mut io, ms + 5);
    assert_eq!(recorded(&io), vec![(0, 0), (1, 64), (2, 160)]);
    assert_eq!(member.counts().log_replications, 2);
}

/// The term is begun at its base and nowhere else. The message was read with the
/// append position of the cycle before. A member whose append position has risen
/// past the base by the cycle that would record the term holds bytes above the base
/// that are not of that term: it records nothing, asks for nothing, and its election
/// fails.
#[test]
fn a_member_that_moved_past_the_base_before_it_recorded_the_term_records_nothing() {
    let mut io = Stub::new();
    let (mut member, ms) = after_term_zero(&mut io, 64);
    read(&mut member, &mut io, ms, &told(3, 200, 0, [1, 64, 160], 0));
    assert_eq!(
        member.election_state(),
        ElectionState::FollowerLogReplication
    );
    // What it had received and not synced when the message came is synced now.
    io.append_position = 96;
    work(&mut member, &mut io, ms + 1);
    assert_eq!(member.election_state(), ElectionState::Init);
    assert_eq!(member.counts().elections_failed, 1);
    assert_eq!(io.count(|call| matches!(call, Call::RecordTerm(..))), 0);
    assert_eq!(io.count(|call| *call == Call::AddCatchup), 0);
    assert_eq!(recorded(&io), vec![(0, 0)]);
}

/// The reason a term is recorded when its replication begins (ADR 0086). A member
/// begins term 1 at 64, is told a commit position of 200 by its leader, and holds 32
/// bytes of the term when the replay stops coming: its election fails. It canvasses
/// with a log of term 1 that ends at 96, which is what it holds. Told the term after,
/// it carries on from 96, as a member below the next base does: nothing is truncated
/// and nothing refused. Had it recorded the term only at the stop, it would report a
/// log of term 0 that ends at 96, be told that term 1 begins at 64, and be asked to
/// truncate to 64, below the commit position it knows.
#[test]
fn a_replication_that_fails_midway_carries_on_from_where_it_stopped() {
    let mut io = Stub::new();
    let (mut member, ms) = after_term_zero(&mut io, 64);
    read(
        &mut member,
        &mut io,
        ms,
        &told(3, 200, 0, [1, 64, 160], 200),
    );
    work(&mut member, &mut io, ms + 1);
    io.catchup_image = Some(64);
    io.append_position = 96;
    work(&mut member, &mut io, ms + 2);
    assert_eq!(member.commit_position(), 200);

    // Nothing more for the leader heartbeat timeout.
    work(&mut member, &mut io, ms + 2 + TIMEOUT_MS);
    assert_eq!(member.election_state(), ElectionState::Init);
    assert_eq!(member.counts().log_replications_unavailable, 1);
    io.catchup_image = None;
    io.clear();
    work(&mut member, &mut io, ms + 3 + TIMEOUT_MS);
    assert_eq!(member.election_state(), ElectionState::Canvass);
    assert_eq!(io.count(|call| *call == Call::RemoveCatchup), 1);

    // It reports term 1 at 96, so the leader names term 2, from 160.
    let ms = ms + 4 + TIMEOUT_MS;
    read(
        &mut member,
        &mut io,
        ms,
        &told(3, 200, 1, [2, 160, 200], 200),
    );
    assert_eq!(
        member.election_state(),
        ElectionState::FollowerLogReplication
    );
    io.clear();
    work(&mut member, &mut io, ms + 1);
    assert_eq!(io.count(|call| *call == Call::CatchupPosition(2, 3, 96)), 1);
    assert_eq!(io.count(|call| matches!(call, Call::TruncateLog(_))), 0);
    assert_eq!(io.count(|call| matches!(call, Call::RecordTerm(..))), 0);
    assert_eq!(member.counts().truncations_refused, 0);
    assert_eq!(member.counts().log_truncations, 0);
    assert_eq!(recorded(&io), vec![(0, 0), (1, 64)]);
}

/// The image of a replay must join where the member's log ends. One that joins
/// elsewhere fails the election, counted, and init removes the subscription.
#[test]
fn an_image_that_joins_elsewhere_fails_the_election() {
    let mut io = Stub::new();
    let (mut member, ms) = after_term_zero(&mut io, 32);
    read(&mut member, &mut io, ms, &told(1, 64, 0, [1, 64, -1], 64));
    work(&mut member, &mut io, ms + 1);
    io.catchup_image = Some(16);
    work(&mut member, &mut io, ms + 2);
    assert_eq!(member.election_state(), ElectionState::Init);
    assert_eq!(member.counts().join_position_mismatches, 1);
    assert_eq!(member.counts().elections_failed, 1);
    assert_eq!(io.count(|call| *call == Call::RemoveCatchup), 0);
    io.catchup_image = None;
    work(&mut member, &mut io, ms + 3);
    assert_eq!(member.election_state(), ElectionState::Canvass);
    assert_eq!(io.count(|call| *call == Call::RemoveCatchup), 1);
}

/// The calls that subscribe, ask and unsubscribe, in the order the member made them.
fn asks(io: &Stub) -> Vec<Call> {
    io.calls
        .iter()
        .flatten()
        .filter(|call| {
            matches!(
                call,
                Call::AddCatchup | Call::RemoveCatchup | Call::CatchupPosition(..)
            )
        })
        .copied()
        .collect()
}

/// Where the term lies in two of the leader's recordings the member asks twice (ADR
/// 0092). The first replay ends at its recording's end, 32, below the stop, and its
/// image goes. The member removes its subscription. On the next cycle its log ends
/// further on than where it asked from, so it subscribes again, which arms a recording
/// for the next image, and asks from where its log ends. Each catch-up position is
/// counted. The second image must join where the member asked from: the check is
/// against that position and not the append position of the cycle that first sees
/// the image, which the image's own bytes may have moved already. An image that joins
/// anywhere else fails.
#[test]
fn a_member_asks_again_when_its_replays_image_has_gone_and_its_log_has_moved() {
    let mut io = Stub::new();
    let (mut member, ms) = after_term_zero(&mut io, 0);
    read(&mut member, &mut io, ms, &told(1, 64, 0, [1, 64, -1], 64));
    work(&mut member, &mut io, ms + 1);
    io.catchup_image = Some(0);
    io.append_position = 32;
    work(&mut member, &mut io, ms + 2);
    assert_eq!(io.last_append, Some((2, 0, 32)));
    // The image is there: the member has reported its end and asks for nothing.
    work(&mut member, &mut io, ms + 3);
    assert_eq!(
        asks(&io),
        vec![Call::AddCatchup, Call::CatchupPosition(2, 1, 0)]
    );

    // The replay ended at its recording's end, and its image goes.
    io.catchup_image = None;
    work(&mut member, &mut io, ms + 4);
    assert_eq!(
        asks(&io),
        vec![
            Call::AddCatchup,
            Call::CatchupPosition(2, 1, 0),
            Call::RemoveCatchup
        ],
        "the cycle that removes the subscription asks for nothing"
    );
    work(&mut member, &mut io, ms + 5);
    assert_eq!(
        asks(&io),
        vec![
            Call::AddCatchup,
            Call::CatchupPosition(2, 1, 0),
            Call::RemoveCatchup,
            Call::AddCatchup,
            Call::CatchupPosition(2, 1, 32),
        ]
    );
    assert_eq!(member.counts().log_replications, 2);

    // The second replay's image joined at 32 and its first 16 bytes are synced
    // before this cycle looks.
    io.catchup_image = Some(32);
    io.append_position = 48;
    work(&mut member, &mut io, ms + 6);
    assert_eq!(
        member.election_state(),
        ElectionState::FollowerLogReplication
    );
    assert_eq!(member.counts().join_position_mismatches, 0);
    assert_eq!(io.last_append, Some((2, 0, 48)));

    // That image goes too, and the member asks from 48. An image that joins at 40
    // is not the answer to it.
    io.catchup_image = None;
    work(&mut member, &mut io, ms + 7);
    work(&mut member, &mut io, ms + 8);
    assert_eq!(io.count(|call| *call == Call::CatchupPosition(2, 1, 48)), 1);
    assert_eq!(member.counts().log_replications, 3);
    io.catchup_image = Some(40);
    work(&mut member, &mut io, ms + 9);
    assert_eq!(member.election_state(), ElectionState::Init);
    assert_eq!(member.counts().join_position_mismatches, 1);
}

/// A member asks from a position once (ADR 0092). A replay whose image went and
/// left the member's log where it asked from brought nothing. The member removes its
/// subscription, asks no more and subscribes to nothing, however many cycles pass,
/// and the progress timer fails its election, counted as a replication that is
/// unavailable: a replay that ends at once is not asked for again and again.
#[test]
fn a_member_whose_replay_brought_nothing_does_not_ask_again() {
    let mut io = Stub::new();
    let (mut member, ms) = after_term_zero(&mut io, 32);
    read(&mut member, &mut io, ms, &told(1, 64, 0, [1, 64, -1], 64));
    work(&mut member, &mut io, ms + 1);
    io.catchup_image = Some(32);
    work(&mut member, &mut io, ms + 2);
    io.catchup_image = None;
    for step in 3..200 {
        work(&mut member, &mut io, ms + step);
    }
    assert_eq!(
        asks(&io),
        vec![
            Call::AddCatchup,
            Call::CatchupPosition(2, 1, 32),
            Call::RemoveCatchup
        ]
    );
    assert_eq!(
        member.election_state(),
        ElectionState::FollowerLogReplication
    );
    work(&mut member, &mut io, ms + TIMEOUT_MS);
    assert_eq!(member.election_state(), ElectionState::Init);
    assert_eq!(member.counts().log_replications, 1);
    assert_eq!(member.counts().log_replications_unavailable, 1);
    assert_eq!(member.counts().elections_failed, 1);
    // Init has no subscription to remove.
    work(&mut member, &mut io, ms + TIMEOUT_MS + 1);
    assert_eq!(io.count(|call| *call == Call::RemoveCatchup), 1);
}

/// Below its stop, a member whose append position has not moved for the leader
/// heartbeat timeout fails its election, counted. Each move starts the time again.
/// A catch-up position that never leaves is the same to the timer, and is not
/// counted as a replication.
#[test]
fn a_replication_that_does_not_move_for_the_heartbeat_timeout_fails() {
    let mut io = Stub::new();
    let (mut member, ms) = after_term_zero(&mut io, 0);
    read(&mut member, &mut io, ms, &told(1, 64, 0, [1, 64, -1], 64));
    work(&mut member, &mut io, ms + 1);
    io.catchup_image = Some(0);
    io.append_position = 16;
    work(&mut member, &mut io, ms + 9_000);
    work(&mut member, &mut io, ms + 9_000 + TIMEOUT_MS - 1);
    assert_eq!(
        member.election_state(),
        ElectionState::FollowerLogReplication
    );
    work(&mut member, &mut io, ms + 9_000 + TIMEOUT_MS);
    assert_eq!(member.election_state(), ElectionState::Init);
    assert_eq!(member.counts().log_replications_unavailable, 1);
    assert_eq!(member.counts().elections_failed, 1);
    assert_eq!(member.counts().log_replications, 1);

    let mut io = Stub::new();
    let (mut member, ms) = after_term_zero(&mut io, 0);
    io.refuse_send = true;
    read(&mut member, &mut io, ms, &told(1, 64, 0, [1, 64, -1], 64));
    work(&mut member, &mut io, ms + 1);
    work(&mut member, &mut io, ms + 2);
    assert_eq!(
        io.count(|call| *call == Call::CatchupPosition(2, 1, 0)),
        2,
        "sent again while it has not left"
    );
    assert_eq!(member.counts().log_replications, 0);
    work(&mut member, &mut io, ms + TIMEOUT_MS);
    assert_eq!(member.election_state(), ElectionState::Init);
    assert_eq!(member.counts().log_replications_unavailable, 1);
}

/// At a stop that is the leader's term base the member waits until the commit
/// position it was told has reached its append position, reporting every leader
/// heartbeat interval with the leader's term, so that the leader counts it. A word
/// of its leader extends the wait by the leader heartbeat timeout; a wait that runs
/// out fails the election, and counts no replication as unavailable.
#[test]
fn at_the_leaders_base_a_member_waits_for_the_commit_position() {
    let mut io = Stub::new();
    let (mut member, ms) = after_term_zero(&mut io, 0);
    // The leader's commit position is 0: no quorum has recorded term 1 yet.
    read(&mut member, &mut io, ms, &told(1, 64, 0, [1, 64, -1], 0));
    work(&mut member, &mut io, ms + 1);
    io.catchup_image = Some(0);
    io.append_position = 64;
    work(&mut member, &mut io, ms + 2);
    assert_eq!(
        member.election_state(),
        ElectionState::FollowerLogReplication
    );
    assert_eq!(recorded(&io), vec![(0, 0), (1, 64)]);
    assert_eq!(io.last_append, Some((2, 1, 64)));
    assert_eq!(io.count(|call| *call == Call::RemoveCatchup), 0);

    // It goes on reporting while it waits.
    io.clear();
    work(&mut member, &mut io, ms + 202);
    assert_eq!(
        io.count(|call| matches!(
            call,
            Call::Send {
                to: 2,
                template: Template::AppendPosition
            }
        )),
        1
    );
    // A commit position below its append position is the leader's word, not the end.
    read(&mut member, &mut io, ms + 9_000, &commit_position(1, 32));
    work(&mut member, &mut io, ms + 9_000 + TIMEOUT_MS - 1);
    assert_eq!(
        member.election_state(),
        ElectionState::FollowerLogReplication
    );
    read(
        &mut member,
        &mut io,
        ms + 9_000 + TIMEOUT_MS,
        &commit_position(1, 64),
    );
    work(&mut member, &mut io, ms + 9_001 + TIMEOUT_MS);
    assert_eq!(member.election_state(), ElectionState::Canvass);
    assert_eq!(io.count(|call| *call == Call::RemoveCatchup), 1);
    assert_eq!(member.counts().elections_failed, 0);

    // The same, with no word of the leader after the stop.
    let mut io = Stub::new();
    let (mut member, ms) = after_term_zero(&mut io, 0);
    read(&mut member, &mut io, ms, &told(1, 64, 0, [1, 64, -1], 0));
    work(&mut member, &mut io, ms + 1);
    io.catchup_image = Some(0);
    io.append_position = 64;
    work(&mut member, &mut io, ms + 2);
    work(&mut member, &mut io, ms + 1 + TIMEOUT_MS);
    assert_eq!(
        member.election_state(),
        ElectionState::FollowerLogReplication
    );
    work(&mut member, &mut io, ms + 2 + TIMEOUT_MS);
    assert_eq!(member.election_state(), ElectionState::Init);
    assert_eq!(member.counts().elections_failed, 1);
    assert_eq!(member.counts().log_replications_unavailable, 0);
}

/// A term with nothing in it: the member is at its base, which is also its end. It
/// records the term and is at its stop at once. It subscribes to nothing and asks
/// for nothing.
#[test]
fn a_term_of_no_length_is_recorded_and_asks_for_nothing() {
    let mut io = Stub::new();
    let (mut member, ms) = after_term_zero(&mut io, 64);
    // Term 1 began and ended at 64; the leader leads term 3 from 200.
    read(&mut member, &mut io, ms, &told(3, 200, 0, [1, 64, 64], 0));
    assert_eq!(
        member.election_state(),
        ElectionState::FollowerLogReplication
    );
    work(&mut member, &mut io, ms + 1);
    assert_eq!(member.election_state(), ElectionState::Canvass);
    assert_eq!(recorded(&io), vec![(0, 0), (1, 64)]);
    assert_eq!(io.count(|call| *call == Call::AddCatchup), 0);
    assert_eq!(
        io.count(|call| matches!(call, Call::CatchupPosition(..))),
        0
    );
    assert_eq!(member.counts().log_replications, 0);
}

/// A member carried past its stop holds bytes of no term in its record: its election
/// fails.
#[test]
fn a_member_carried_past_its_stop_fails_its_election() {
    let mut io = Stub::new();
    let (mut member, ms) = after_term_zero(&mut io, 0);
    read(&mut member, &mut io, ms, &told(1, 64, 0, [1, 64, -1], 64));
    work(&mut member, &mut io, ms + 1);
    io.catchup_image = Some(0);
    io.append_position = 96;
    work(&mut member, &mut io, ms + 2);
    assert_eq!(member.election_state(), ElectionState::Init);
    assert_eq!(member.counts().elections_failed, 1);
    assert_eq!(recorded(&io), vec![(0, 0)], "it records no term there");
}

/// While it replicates the member has a leader. That leader's commit position and its
/// new leadership term are its word. A new leadership term of another term is ignored
/// and counted, and a request vote gets no vote: the state casts none.
#[test]
fn a_member_that_replicates_follows_its_leader_alone() {
    let mut io = Stub::new();
    let (mut member, ms) = after_term_zero(&mut io, 0);
    read(&mut member, &mut io, ms, &told(1, 64, 0, [1, 64, -1], 0));
    work(&mut member, &mut io, ms + 1);
    read(&mut member, &mut io, ms + 2, &commit_position(1, 16));
    assert_eq!(member.commit_position(), 16);
    read(
        &mut member,
        &mut io,
        ms + 3,
        &told(1, 64, 0, [1, 64, -1], 32),
    );
    assert_eq!(member.commit_position(), 32);
    assert_eq!(
        member.election_state(),
        ElectionState::FollowerLogReplication
    );
    assert_eq!(member.counts().log_replications, 1, "asked once");

    let ignored = member.counts().leadership_terms_ignored;
    read(
        &mut member,
        &mut io,
        ms + 4,
        &told(2, 64, 0, [2, 64, -1], 64),
    );
    assert_eq!(member.counts().leadership_terms_ignored, ignored + 1);
    assert_eq!(member.leadership_term_id(), 1);
    io.clear();
    let request = Message::RequestVote {
        log_leadership_term_id: 0,
        log_position: 64,
        candidate_term_id: 2,
        candidate_member_id: 1,
        protocol_version: 0,
    };
    read(&mut member, &mut io, ms + 5, &request);
    assert_eq!(
        io.count(|call| matches!(
            call,
            Call::Send {
                template: Template::Vote,
                ..
            }
        )),
        0
    );
    assert_eq!(
        member.election_state(),
        ElectionState::FollowerLogReplication
    );
}

/// At the next term's base with no end carried, the message says to wait for a later
/// one. The member stays where it is and counts nothing.
#[test]
fn a_member_told_to_wait_stays_where_it_is() {
    let mut io = Stub::new();
    let (mut member, ms) = after_term_zero(&mut io, 64);
    let counts = member.counts();
    read(&mut member, &mut io, ms, &told(3, 200, 0, [1, 64, -1], 0));
    assert_eq!(member.election_state(), ElectionState::Canvass);
    assert_eq!(member.counts(), counts);
    assert_eq!(io.made, 0);
}

/// A replication waits for the stop of its replay's recording without holding the duty
/// cycle (card 256): `Io::remove_catchup` answers `false` while the stop is asked and
/// not yet read, and the member comes back to the same place on its next cycle. At the
/// replication's stop it has recorded the leader's term once and stays in the state
/// until the removal is done, then canvasses. Where a replay's image has gone below
/// the stop, the subscription stays until the removal is done, and only the cycle
/// after that asks again.
#[test]
fn a_replication_waits_for_the_stop_of_its_recording_and_asks_again_each_cycle() {
    // At the stop.
    let mut io = Stub::new();
    let (mut member, ms) = after_term_zero(&mut io, 32);
    read(&mut member, &mut io, ms, &told(1, 64, 0, [1, 64, -1], 64));
    work(&mut member, &mut io, ms + 1);
    io.catchup_image = Some(32);
    io.append_position = 64;
    io.stops_pending = 2;
    for cycle in 0..2 {
        work(&mut member, &mut io, ms + 2 + cycle);
        assert_eq!(
            member.election_state(),
            ElectionState::FollowerLogReplication,
            "cycle {cycle}"
        );
    }
    assert_eq!(io.count(|call| *call == Call::RemoveCatchup), 2);
    work(&mut member, &mut io, ms + 4);
    assert_eq!(io.count(|call| *call == Call::RemoveCatchup), 3, "and read");
    assert_eq!(member.election_state(), ElectionState::Canvass);
    assert_eq!(
        io.count(|call| *call == Call::RecordTerm(1, 64)),
        1,
        "the leader's term is recorded once"
    );
    assert_eq!(member.counts().elections_failed, 0);

    // A replay's image gone below the stop.
    let mut io = Stub::new();
    let (mut member, ms) = after_term_zero(&mut io, 0);
    read(&mut member, &mut io, ms, &told(1, 64, 0, [1, 64, -1], 64));
    work(&mut member, &mut io, ms + 1);
    io.catchup_image = Some(0);
    io.append_position = 32;
    work(&mut member, &mut io, ms + 2);
    io.catchup_image = None;
    io.stops_pending = 2;
    work(&mut member, &mut io, ms + 3);
    work(&mut member, &mut io, ms + 4);
    assert_eq!(
        asks(&io),
        vec![
            Call::AddCatchup,
            Call::CatchupPosition(2, 1, 0),
            Call::RemoveCatchup,
            Call::RemoveCatchup,
        ],
        "asked each cycle, and nothing else while the subscription is there"
    );
    work(&mut member, &mut io, ms + 5);
    work(&mut member, &mut io, ms + 6);
    assert_eq!(
        asks(&io),
        vec![
            Call::AddCatchup,
            Call::CatchupPosition(2, 1, 0),
            Call::RemoveCatchup,
            Call::RemoveCatchup,
            Call::RemoveCatchup,
            Call::AddCatchup,
            Call::CatchupPosition(2, 1, 32),
        ],
        "the cycle after the removal asks from where the log ends"
    );
    assert_eq!(member.counts().log_replications, 2);
    assert_eq!(member.counts().elections_failed, 0);
}
