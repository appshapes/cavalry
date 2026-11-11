//! An [`Io`] that does nothing and remembers what it was asked, in arrays: for the
//! tests that drive one core by hand, and for the allocation test, which it must not
//! disturb.

#![allow(dead_code)]

use cavalry_proto::cluster::{Message, Template};
use cavalry_raft::consensus::{base_above, terms_above};
use cavalry_raft::{Clock, Consensus, ElectionState, Io, Log, ReplayStopped, Role};

/// One call the core made, in the order it made it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Call {
    Send {
        to: i32,
        template: Template,
    },
    StoreCandidateTerm(i64),
    RecordTerm(i64, i64),
    AddLog(i64, i64),
    JoinLog,
    LeaveLog,
    RemoveLog,
    TruncateLog(i64),
    Append(Template),
    /// A replay started: the member it is to, its position and its end, none for a
    /// catch-up's.
    StartReplay(i32, i64, Option<i64>),
    StopReplay(i32),
    AddCatchup,
    RemoveCatchup,
    /// A catch-up position sent: the leader it is to, the term and the position.
    CatchupPosition(i32, i64, i64),
}

/// The world of one core, as the test sets it.
pub struct Stub {
    /// The calls made, the first [`Stub::CALLS`] of them.
    pub calls: [Option<Call>; Stub::CALLS],
    pub made: usize,
    /// The member's append position.
    pub append_position: i64,
    /// The join position of the log's image, once the test has given it one.
    pub image: Option<i64>,
    /// Whether `record_term` fails.
    pub fail_record: bool,
    /// Whether `remove_log` fails.
    pub fail_remove: bool,
    /// Whether `truncate_log` is still under way: it cuts nothing and returns `None`.
    pub truncating: bool,
    /// Whether `append` is back-pressured.
    pub back_pressured: bool,
    /// Whether `send` reports a message not sent, as a publication that is
    /// back-pressured or not connected does.
    pub refuse_send: bool,
    /// The terms recorded, each with its base: the first [`Stub::TERMS`] of them.
    pub terms: [Option<(i64, i64)>; Stub::TERMS],
    /// The last new leadership term sent: who it was for, the log leadership term id
    /// it answers, and its three next fields.
    pub last_term: Option<(i32, i64, [i64; 3])>,
    /// Whether the member's recordings hold no position a replay is asked from, as
    /// after a purge: `start_replay` starts nothing.
    pub purged: bool,
    /// Whether `start_replay` fails.
    pub fail_start: bool,
    /// Whether `stop_replay` fails.
    pub fail_stop: bool,
    /// How the replay to each member ended, by the member's id, once the test has
    /// said so: `replay_stopped` hands it over once.
    pub stopped: [Option<ReplayStopped>; Stub::MEMBERS],
    /// The join position of the image on the catch-up subscription, once the test
    /// has given it one.
    pub catchup_image: Option<i64>,
    /// The last append position sent: who it was for, the term it carried and the
    /// position.
    pub last_append: Option<(i32, i64, i64)>,
    /// The flags of that append position.
    pub last_append_flags: u8,
}

impl Stub {
    pub const CALLS: usize = 64;
    pub const TERMS: usize = 8;
    pub const MEMBERS: usize = 8;

    pub fn new() -> Self {
        Self {
            calls: [None; Stub::CALLS],
            made: 0,
            append_position: 0,
            image: None,
            fail_record: false,
            fail_remove: false,
            truncating: false,
            back_pressured: false,
            refuse_send: false,
            terms: [None; Stub::TERMS],
            last_term: None,
            purged: false,
            fail_start: false,
            fail_stop: false,
            stopped: [None; Stub::MEMBERS],
            catchup_image: None,
            last_append: None,
            last_append_flags: 0,
        }
    }

    fn call(&mut self, call: Call) {
        if let Some(slot) = self.calls.get_mut(self.made) {
            *slot = Some(call);
        }
        self.made += 1;
    }

    /// The place of the first call that matches, among the calls kept.
    pub fn first(&self, matches: impl Fn(&Call) -> bool) -> Option<usize> {
        self.calls
            .iter()
            .position(|call| call.as_ref().is_some_and(&matches))
    }

    /// How many of the calls kept match.
    pub fn count(&self, matches: impl Fn(&Call) -> bool) -> usize {
        self.calls
            .iter()
            .filter(|call| call.as_ref().is_some_and(&matches))
            .count()
    }

    /// Forgets the calls, for the next part of a test.
    pub fn clear(&mut self) {
        self.calls = [None; Stub::CALLS];
        self.made = 0;
    }
}

impl Io for Stub {
    type Fault = &'static str;

    fn send(&mut self, member: i32, message: &Message<'_>) -> bool {
        self.call(Call::Send {
            to: member,
            template: message.template(),
        });
        if let Message::NewLeadershipTerm {
            log_leadership_term_id,
            next_leadership_term_id,
            next_term_base_log_position,
            next_log_position,
            ..
        } = *message
        {
            self.last_term = Some((
                member,
                log_leadership_term_id,
                [
                    next_leadership_term_id,
                    next_term_base_log_position,
                    next_log_position,
                ],
            ));
        }
        if let Message::AppendPosition {
            leadership_term_id,
            log_position,
            flags,
            ..
        } = *message
            && !self.refuse_send
        {
            self.last_append = Some((member, leadership_term_id, log_position));
            self.last_append_flags = flags;
        }
        !self.refuse_send
    }

    fn store_candidate_term(&mut self, term: i64) -> Result<(), Self::Fault> {
        self.call(Call::StoreCandidateTerm(term));
        Ok(())
    }

    fn record_term(&mut self, term: i64, base: i64) -> Result<(), Self::Fault> {
        self.call(Call::RecordTerm(term, base));
        if self.fail_record {
            return Err("the terms file cannot be written");
        }
        if let Some(slot) = self.terms.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some((term, base));
        }
        Ok(())
    }

    fn add_log(&mut self, term: i64, base: i64) -> Result<Option<Log>, Self::Fault> {
        self.call(Call::AddLog(term, base));
        Ok(Some(Log {
            session_id: 77,
            recording_id: 5,
        }))
    }

    fn join_log(&mut self) -> Result<(), Self::Fault> {
        self.call(Call::JoinLog);
        Ok(())
    }

    fn log_image(&mut self, _session_id: i32) -> Result<Option<i64>, Self::Fault> {
        Ok(self.image)
    }

    fn leave_log(&mut self) -> Result<(), Self::Fault> {
        self.call(Call::LeaveLog);
        Ok(())
    }

    fn remove_log(&mut self) -> Result<(), Self::Fault> {
        self.call(Call::RemoveLog);
        if self.fail_remove {
            return Err("the recording cannot be stopped");
        }
        Ok(())
    }

    fn truncate_log(&mut self, position: i64) -> Result<Option<i64>, Self::Fault> {
        self.call(Call::TruncateLog(position));
        if self.truncating {
            return Ok(None);
        }
        self.append_position = self.append_position.min(position);
        for slot in &mut self.terms {
            if slot.is_some_and(|(_, base)| base > position) {
                *slot = None;
            }
        }
        Ok(Some(
            self.terms
                .iter()
                .flatten()
                .map(|&(term, _)| term)
                .max()
                .unwrap_or(-1),
        ))
    }

    fn append_position(&mut self) -> i64 {
        self.append_position
    }

    fn append(&mut self, entry: &Message<'_>) -> Result<bool, Self::Fault> {
        if self.back_pressured {
            return Ok(false);
        }
        self.call(Call::Append(entry.template()));
        Ok(true)
    }

    fn terms_after(&mut self, term: i64) -> [Option<(i64, i64)>; 2] {
        terms_above(self.terms.iter().flatten().copied(), term)
    }

    fn term_base_above(&mut self, position: i64) -> Option<i64> {
        base_above(self.terms.iter().flatten().copied(), position)
    }

    fn start_replay(
        &mut self,
        member: i32,
        position: i64,
        end: Option<i64>,
    ) -> Result<bool, Self::Fault> {
        self.call(Call::StartReplay(member, position, end));
        if self.fail_start {
            return Err("the replay cannot be started");
        }
        Ok(!self.purged)
    }

    fn stop_replay(&mut self, member: i32) -> Result<(), Self::Fault> {
        self.call(Call::StopReplay(member));
        if self.fail_stop {
            return Err("the replay cannot be stopped");
        }
        Ok(())
    }

    fn replay_stopped(&mut self, member: i32) -> Result<Option<ReplayStopped>, Self::Fault> {
        let slot = usize::try_from(member)
            .ok()
            .and_then(|index| self.stopped.get_mut(index));
        Ok(slot.and_then(Option::take))
    }

    fn add_catchup(&mut self) -> Result<(), Self::Fault> {
        self.call(Call::AddCatchup);
        Ok(())
    }

    fn send_catchup_position(&mut self, leader: i32, term: i64, position: i64) -> bool {
        self.call(Call::CatchupPosition(leader, term, position));
        !self.refuse_send
    }

    fn catchup_image(&mut self) -> Result<Option<i64>, Self::Fault> {
        Ok(self.catchup_image)
    }

    fn remove_catchup(&mut self) -> Result<(), Self::Fault> {
        self.call(Call::RemoveCatchup);
        Ok(())
    }
}

/// A clock at `milliseconds` since the test began.
pub fn at(milliseconds: u64) -> Clock {
    Clock {
        now_ns: milliseconds * 1_000_000,
        time_ms: 1_700_000_000_000 + i64::try_from(milliseconds).unwrap_or(0),
    }
}

/// The leader's new leadership term to a member that reported no term, for term 0 at
/// base 0 with the log at `log_position`.
pub fn new_leadership_term(leader: i32, log_position: i64) -> Message<'static> {
    Message::NewLeadershipTerm {
        log_leadership_term_id: -1,
        next_leadership_term_id: 0,
        next_term_base_log_position: 0,
        next_log_position: -1,
        leadership_term_id: 0,
        term_base_log_position: 0,
        log_position,
        leader_recording_id: 5,
        timestamp: 1_700_000_000_000,
        leader_member_id: leader,
        log_session_id: 77,
        app_version: 0,
        is_startup: true,
        commit_position: 0,
    }
}

/// A follower's append position for term 0.
pub fn append_position(follower: i32, log_position: i64) -> Message<'static> {
    Message::AppendPosition {
        leadership_term_id: 0,
        log_position,
        follower_member_id: follower,
        flags: 0,
    }
}

/// A member's canvass position, reporting the log (`term`, `position`).
pub fn canvass_position(member: i32, term: i64, position: i64) -> Message<'static> {
    Message::CanvassPosition {
        log_leadership_term_id: term,
        log_position: position,
        leadership_term_id: term,
        follower_member_id: member,
        protocol_version: 0,
    }
}

/// `leader`'s new leadership term for `term`, which begins at `base` with the leader's
/// log at `log_position`, to a member whose log is of `log_term`, with `next` as its
/// three next fields: the next leadership term id, that term's base and its end.
pub fn leadership_term(
    leader: i32,
    term: i64,
    base: i64,
    log_position: i64,
    log_term: i64,
    next: [i64; 3],
) -> Message<'static> {
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
        log_position,
        leader_recording_id: 5,
        timestamp: 1_700_000_000_000,
        leader_member_id: leader,
        log_session_id: 77,
        app_version: 0,
        is_startup: true,
        commit_position: base,
    }
}

/// Takes `member` from canvass to closed as a follower of the leader whose new
/// leadership term is `term`, with the image of the leader's log joining at `join`,
/// from `ms` on; returns the millisecond its election closed at.
pub fn follow_term(
    member: &mut Consensus,
    io: &mut Stub,
    ms: u64,
    term: &Message<'_>,
    join: i64,
) -> u64 {
    assert_eq!(member.election_state(), ElectionState::Canvass);
    member.on_message(at(ms), term, io).expect("no fault");
    assert_eq!(member.election_state(), ElectionState::FollowerReplay);
    member.do_work(at(ms + 1), io).expect("no fault");
    member.do_work(at(ms + 2), io).expect("no fault");
    io.image = Some(join);
    member.do_work(at(ms + 3), io).expect("no fault");
    member.do_work(at(ms + 4), io).expect("no fault");
    assert_eq!(member.election_state(), ElectionState::Closed);
    ms + 4
}

/// The follower hears nothing for the leader heartbeat timeout after `closed_at` and
/// canvasses again, with `appended` bytes in its recordings; returns the next
/// millisecond.
pub fn lose_the_leader(
    member: &mut Consensus,
    io: &mut Stub,
    closed_at: u64,
    appended: i64,
) -> u64 {
    let ms = closed_at + 10_000;
    member.do_work(at(ms), io).expect("no fault");
    assert_eq!(member.election_state(), ElectionState::Init);
    io.image = None;
    io.append_position = appended;
    member.do_work(at(ms + 1), io).expect("no fault");
    assert_eq!(member.election_state(), ElectionState::Canvass);
    ms + 2
}

/// A member that elects, member 0 of three, taken by hand to the lead of its third
/// term: it follows term 0, led by member 1 from 0, and term 1, led by member 2 from
/// 64, each recorded at its base, then wins the ballot of term 2 and leads it from
/// 96, in leader log replication with its log publication added. Its record of terms
/// is (0, 0), (1, 64), (2, 96). Returns the member and the next millisecond.
pub fn leader_of_term_two(io: &mut Stub) -> (Consensus, u64) {
    let mut member = Consensus::with_election(&[0, 1, 2], 0, 1).expect("a member");
    // Term 0, led by member 1 from 0.
    member.do_work(at(1), io).expect("no fault");
    let closed = follow_term(&mut member, io, 2, &new_leadership_term(1, 0), 0);
    // Term 1, led by member 2 from 64, after 64 bytes of term 0.
    let ms = lose_the_leader(&mut member, io, closed, 64);
    let term_one = leadership_term(2, 1, 64, 64, 0, [1, 64, -1]);
    let closed = follow_term(&mut member, io, ms, &term_one, 64);
    assert_eq!(member.leader_member_id(), 2);
    // Term 2, its own, from 96, after 32 bytes of term 1.
    let ms = lose_the_leader(&mut member, io, closed, 96);
    for other in [1, 2] {
        member
            .on_message(at(ms), &canvass_position(other, 1, 96), io)
            .expect("no fault");
    }
    member.do_work(at(ms + 1), io).expect("no fault");
    assert_eq!(member.election_state(), ElectionState::Nominate);
    member.do_work(at(ms + 501), io).expect("no fault");
    assert_eq!(member.election_state(), ElectionState::CandidateBallot);
    for voter in [1, 2] {
        let vote = Message::Vote {
            candidate_term_id: 2,
            log_leadership_term_id: 1,
            log_position: 96,
            candidate_member_id: 0,
            follower_member_id: voter,
            vote: true,
        };
        member
            .on_message(at(ms + 502), &vote, io)
            .expect("no fault");
    }
    member.do_work(at(ms + 503), io).expect("no fault");
    assert_eq!(member.role(), Role::Leader);
    assert_eq!(member.leadership_term_id(), 2);
    member.do_work(at(ms + 504), io).expect("no fault");
    assert_eq!(
        io.terms.iter().flatten().copied().collect::<Vec<_>>(),
        vec![(0, 0), (1, 64), (2, 96)],
        "its record of terms"
    );
    assert_eq!(member.election_state(), ElectionState::LeaderLogReplication);
    (member, ms + 505)
}
