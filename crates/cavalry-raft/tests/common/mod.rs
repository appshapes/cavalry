//! An [`Io`] that does nothing and remembers what it was asked, in arrays: for the
//! tests that drive one core by hand, and for the allocation test, which it must not
//! disturb.

#![allow(dead_code)]

use cavalry_proto::cluster::{Message, Template};
use cavalry_raft::consensus::terms_above;
use cavalry_raft::{Clock, Io, Log};

/// One call the core made, in the order it made it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Call {
    Send { to: i32, template: Template },
    StoreCandidateTerm(i64),
    RecordTerm(i64, i64),
    AddLog(i64, i64),
    JoinLog,
    LeaveLog,
    RemoveLog,
    Append(Template),
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
}

impl Stub {
    pub const CALLS: usize = 64;
    pub const TERMS: usize = 8;

    pub fn new() -> Self {
        Self {
            calls: [None; Stub::CALLS],
            made: 0,
            append_position: 0,
            image: None,
            fail_record: false,
            fail_remove: false,
            back_pressured: false,
            refuse_send: false,
            terms: [None; Stub::TERMS],
            last_term: None,
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
