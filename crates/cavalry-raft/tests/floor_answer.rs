//! The floor answer (`spec/cluster.md`, "A member that lost its log", "The floor answer";
//! ADR 0163; CLAUDE.md, invariant 1), on one member driven through a stub: a member that
//! holds a vote floor asks with the floor flag of each append position until it has an
//! answer for the term it follows, asks again once it follows another term, keeps the
//! lowest answer it took, and clears its floor once its log reaches it. A leader answers
//! an ask it read with the session of its frame, with its append position as it is when
//! it writes the answer.

mod common;

use cavalry_proto::cluster::{APPEND_POSITION_FLAG_FLOOR, Message, Template};
use cavalry_raft::Consensus;
use common::{
    Call, Stub, at, follow_term_unanswered, leader_of_term_two, leadership_term, lose_the_leader,
    new_leadership_term,
};

/// Member 0 of `[0, 1, 2]`, electing.
fn elector(seed: u64) -> Consensus {
    Consensus::with_election(&[0, 1, 2], 0, seed).expect("a member")
}

/// Whether the member's last append position asked for a floor answer.
fn asked(io: &Stub) -> bool {
    io.last_append_flags & APPEND_POSITION_FLAG_FLOOR != 0
}

/// A floor answer from `leader` for `term` at `position`, naming the session of the
/// member's publication to it in the stub, 1000 above the leader's id.
fn answer(leader: i32, term: i64, position: i64) -> Message<'static> {
    Message::FloorAnswer {
        leadership_term_id: term,
        log_position: position,
        leader_member_id: leader,
        follower_member_id: 0,
        session_id: leader + 1000,
    }
}

/// A member that took its first leader with its log empty holds a floor of 0 and asks.
/// An answer for another session, or another member, is ignored and counted. One for
/// its own run and term stops the asking, and holds the floor while the log is below
/// it. Under the next term it asks again, takes the lower answer of that term, and
/// clears its floor at it.
#[test]
fn a_member_asks_for_a_floor_answer_under_each_term_it_holds_none_from() {
    let mut io = Stub::new();
    let mut member = elector(3);
    member.do_work(at(1), &mut io).expect("no fault");
    let closed = follow_term_unanswered(&mut member, &mut io, 2, &new_leadership_term(1, 0), 0);
    assert_eq!(
        member.vote_floor(),
        0,
        "it took its leader with its log empty"
    );

    // It reports, and asks.
    member.do_work(at(closed + 200), &mut io).expect("no fault");
    assert!(asked(&io), "it asks while it holds a floor");

    // Answers that are not for it are ignored and counted.
    let other_session = Message::FloorAnswer {
        leadership_term_id: 0,
        log_position: 1_000,
        leader_member_id: 1,
        follower_member_id: 0,
        session_id: 7,
    };
    let other_member = Message::FloorAnswer {
        leadership_term_id: 0,
        log_position: 1_000,
        leader_member_id: 1,
        follower_member_id: 2,
        session_id: 1001,
    };
    for wrong in [other_session, other_member] {
        member
            .on_message(at(closed + 201), &wrong, &mut io)
            .expect("no fault");
    }
    assert_eq!(member.floor_answer(), -1);
    assert_eq!(member.counts().floor_answers_ignored, 2);

    // Its own answer, above its log: taken, the asking stops, and the floor holds.
    member
        .on_message(at(closed + 202), &answer(1, 0, 1_000), &mut io)
        .expect("no fault");
    assert_eq!(member.floor_answer(), 1_000);
    member.do_work(at(closed + 400), &mut io).expect("no fault");
    assert!(!asked(&io), "answered for this term, it asks no more");
    assert_eq!(member.vote_floor(), 0, "its log is below the answer");

    // A later term, led by member 2 from 64: it asks again, takes the lower answer of
    // that term, and clears once its log is there.
    let ms = lose_the_leader(&mut member, &mut io, closed + 400, 64);
    let term_one = leadership_term(2, 1, 64, 64, 0, [1, 64, -1]);
    let closed = follow_term_unanswered(&mut member, &mut io, ms, &term_one, 64);
    member.do_work(at(closed + 200), &mut io).expect("no fault");
    assert!(
        asked(&io),
        "under a term it holds no answer from, it asks again"
    );
    member
        .on_message(at(closed + 201), &answer(2, 1, 64), &mut io)
        .expect("no fault");
    // A later answer of that term, above its log, changes nothing: it keeps the lowest.
    member
        .on_message(at(closed + 201), &answer(2, 1, 128), &mut io)
        .expect("no fault");
    assert_eq!(member.floor_answer(), 64, "the lowest answer it took");
    member.do_work(at(closed + 202), &mut io).expect("no fault");
    assert_eq!(member.vote_floor(), -1, "its log reached the answer");
    assert_eq!(member.counts().vote_floors_cleared, 1);
}

/// A leader answers a report with the floor flag, read with the session of its frame,
/// with its append position as it reads it when it writes the answer, and not as its
/// last duty cycle read it: a replay of its live recording reads up to the recording's
/// synced position, which can have moved since, and may have served an earlier run of
/// the member an entry there. The same report read with no session is not answered.
#[test]
fn a_leader_answers_a_floor_ask_with_its_append_position_as_it_writes_the_answer() {
    let mut io = Stub::new();
    let (mut leader, ms) = leader_of_term_two(&mut io);
    assert_eq!(leader.append_position(), 96, "as of its last duty cycle");

    // Its recording syncs past that after the cycle, before the next reads it.
    io.append_position = 160;
    io.clear();
    let ask = Message::AppendPosition {
        leadership_term_id: 2,
        log_position: 0,
        follower_member_id: 1,
        flags: APPEND_POSITION_FLAG_FLOOR,
    };
    let answers = |io: &Stub| {
        io.count(|call| {
            *call
                == Call::Send {
                    to: 1,
                    template: Template::FloorAnswer,
                }
        })
    };

    // Read with no session, the ask is not answered.
    leader.on_message(at(ms), &ask, &mut io).expect("no fault");
    assert_eq!(answers(&io), 0, "no session, no answer");
    assert_eq!(io.last_floor_answer, None);

    // Read with the session of its frame, it is answered once, with the position now.
    leader
        .on_message_in(at(ms), &ask, 4_242, &mut io)
        .expect("no fault");
    assert_eq!(answers(&io), 1, "one answer");
    assert_eq!(
        io.last_floor_answer,
        Some((
            1,
            Message::FloorAnswer {
                leadership_term_id: 2,
                log_position: 160,
                leader_member_id: 0,
                follower_member_id: 1,
                session_id: 4_242,
            }
        )),
        "its term, its append position as it writes the answer, both ids and the session"
    );
    assert_eq!(leader.counts().floor_answers_sent, 1);
}
