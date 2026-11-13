//! A member's sessions allocate nothing once they are made: not for a message of the
//! ingress stream, an entry of the log or a duty cycle, and not for a session that
//! connects, is refused, times out or closes (`CLAUDE.md`, invariant 2; ADR 0115). A
//! program of its own with no test harness, for the reason `tests/allocations.rs`
//! gives: the counting allocator is the whole process's, and the process must have
//! one thread. It prints one line when the check has passed.

use std::alloc::System;
use std::convert::Infallible;

use cavalry_proto::cluster::{CloseReason, EGRESS_STREAM_ID, MAX_SESSIONS, Message};
use cavalry_raft::sessions::EGRESS_SLOTS;
use cavalry_raft::{Clock, Ingress, Leadership, SessionIo, Sessions};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

const ENDPOINTS: &[u8] = b"0=10.0.0.1:9010,1=10.0.0.2:9010,2=10.0.0.3:9010";
const CHANNEL: &[u8] = b"cavalry:udp?endpoint=10.0.1.1:20000";
const ROUNDS: u64 = 10_000;

/// A world that holds nothing but counts: every publication takes every message and
/// the log takes every entry.
struct Counting {
    held: [bool; EGRESS_SLOTS],
    sent: u64,
    appended: u64,
}

impl SessionIo for Counting {
    type Fault = Infallible;

    fn add_egress(&mut self, slot: usize, _channel: &[u8]) -> Result<bool, Infallible> {
        if let Some(held) = self.held.get_mut(slot) {
            *held = true;
        }
        Ok(true)
    }

    fn send_egress(&mut self, slot: usize, _message: &Message<'_>) -> bool {
        let held = self.held.get(slot).copied().unwrap_or(false);
        if held {
            self.sent += 1;
        }
        held
    }

    fn remove_egress(&mut self, slot: usize) -> Result<(), Infallible> {
        if let Some(held) = self.held.get_mut(slot) {
            *held = false;
        }
        Ok(())
    }

    fn append(&mut self, _entry: &Message<'_>) -> Result<bool, Infallible> {
        self.appended += 1;
        Ok(true)
    }
}

fn at(ms: u64) -> Clock {
    Clock {
        now_ns: ms * 1_000_000,
        time_ms: 1_700_000_000_000 + ms as i64,
    }
}

const LEAD: Leadership = Leadership {
    leads: true,
    leadership_term_id: 1,
    leader_member_id: 0,
};

/// A connect, and the committed session open event the leader wrote for it.
fn open(sessions: &mut Sessions, io: &mut Counting, ms: u64, id: i64) {
    let connect = Message::SessionConnect {
        correlation_id: id,
        response_stream_id: EGRESS_STREAM_ID,
        version: 0,
        response_channel: CHANNEL,
        encoded_credentials: &[],
        client_info: &[],
    };
    let Ok(taken) = sessions.on_ingress(at(ms), LEAD, &connect, io);
    assert_eq!(taken, Ingress::Taken);
    let event = Message::SessionOpenEvent {
        leadership_term_id: 1,
        correlation_id: id,
        cluster_session_id: id,
        timestamp: 0,
        response_stream_id: EGRESS_STREAM_ID,
        response_channel: CHANNEL,
        encoded_principal: &[],
    };
    let Ok(known) = sessions.on_entry(at(ms), &event, io);
    assert!(known, "session {id} opens");
}

fn main() {
    let mut io = Counting {
        held: [false; EGRESS_SLOTS],
        sent: 0,
        appended: 0,
    };
    let mut sessions = Sessions::new(0, ENDPOINTS);
    let Ok(_) = sessions.do_work(at(0), LEAD, &mut io);
    for id in 0..MAX_SESSIONS as i64 {
        open(&mut sessions, &mut io, 0, id);
    }
    let mut next_id = MAX_SESSIONS as i64;
    // The session each slot holds, so that a round names an open one.
    let mut ids: [i64; MAX_SESSIONS] = core::array::from_fn(|slot| slot as i64);

    let region = Region::new(GLOBAL);
    for round in 0..ROUNDS {
        let ms = 1 + round;
        let slot = (round % MAX_SESSIONS as u64) as usize;
        let id = ids[slot];
        let message = Message::SessionMessage {
            leadership_term_id: 1,
            cluster_session_id: id,
            timestamp: 0,
            payload: b"0123456789abcdef0123456789abcdef",
        };
        let Ok(taken) = sessions.on_ingress(at(ms), LEAD, &message, &mut io);
        assert_eq!(taken, Ingress::Taken);
        let keepalive = Message::SessionKeepalive {
            leadership_term_id: 1,
            cluster_session_id: id,
        };
        let Ok(_) = sessions.on_ingress(at(ms), LEAD, &keepalive, &mut io);
        // Every hundredth round: an eleventh connect, refused and answered; then the
        // round's session closes, by its client or by an entry that says timeout,
        // and another connects in its place.
        if round % 100 == 99 {
            let refused = Message::SessionConnect {
                correlation_id: -2,
                response_stream_id: EGRESS_STREAM_ID,
                version: 0,
                response_channel: CHANNEL,
                encoded_credentials: &[],
                client_info: &[],
            };
            let Ok(_) = sessions.on_ingress(at(ms), LEAD, &refused, &mut io);
            let close = Message::SessionClose {
                leadership_term_id: 1,
                cluster_session_id: id,
            };
            let Ok(_) = sessions.on_ingress(at(ms), LEAD, &close, &mut io);
            let closed = Message::SessionCloseEvent {
                leadership_term_id: 1,
                cluster_session_id: id,
                timestamp: 0,
                close_reason: if round % 200 == 99 {
                    CloseReason::ClientAction
                } else {
                    CloseReason::Timeout
                },
            };
            let Ok(known) = sessions.on_entry(at(ms), &closed, &mut io);
            assert!(known);
            open(&mut sessions, &mut io, ms, next_id);
            ids[slot] = next_id;
            next_id += 1;
        }
        let Ok(_) = sessions.do_work(at(ms), LEAD, &mut io);
    }
    let change = region.change();
    assert_eq!(
        change.allocations + change.reallocations,
        0,
        "a member's sessions allocated or reallocated over {ROUNDS} rounds: {change:?}"
    );
    let counts = sessions.counts();
    assert_eq!(sessions.open(), MAX_SESSIONS);
    assert_eq!(
        (counts.refused, counts.closed),
        (ROUNDS / 100, ROUNDS / 100)
    );
    assert_eq!(counts.ingress_ignored, 0);
    println!(
        "sessions_allocations: ok, {ROUNDS} rounds, {} entries appended, {} events sent, \
         {} sessions opened, no allocation",
        io.appended, io.sent, counts.opened
    );
}
