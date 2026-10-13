//! The client conductor: the thread [`crate::Cavalry::connect`] starts, which keeps the
//! client alive and reads what the driver says (ADR 0021; `spec/client-protocol.md`,
//! "Liveness and timeouts"). Every 500 ms with nothing else sent it writes `client
//! keepalive`; on every cycle it loads the consumer heartbeat, which closes the client
//! on `driver-closed` or `driver-timeout`, and receives every event, which closes the
//! client on its own `client timeout`, on an event that fails a decoding check, or on
//! a lap of the broadcast buffer. A ring or a buffer that fails a check of its page
//! closes the client too. It shares
//! two atomics with the user's thread and nothing else: the state and the time of the
//! last command sent. It idles with a backoff, so a quiet client costs a sleeping thread.

use std::sync::Arc;
use std::sync::atomic::{AtomicI32, AtomicI64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cavalry_proto::client_protocol::{Command, Event};
use cavalry_shm::broadcast::{BroadcastError, Receiver};
use cavalry_shm::command_ring::layout::DRIVER_CLOSED;
use cavalry_shm::command_ring::{CommandRing, CommandRingError, Producer};

use crate::error::Closed;

/// How often a client sends `client keepalive` when it has sent nothing else
/// (`spec/client-protocol.md`, "Liveness and timeouts").
pub(crate) const KEEPALIVE_INTERVAL: Duration = Duration::from_millis(500);

/// A consumer heartbeat older than this is `driver-timeout`.
pub(crate) const DRIVER_TIMEOUT: Duration = Duration::from_secs(10);

/// The scratch buffer every command is encoded into: the longest command is a channel
/// of 4096 bytes and its header, and the ring's own limit is far above.
pub(crate) const COMMAND_SCRATCH: usize = 4096 + 64;

/// What the two threads share. The state is 0 while the client is open and a
/// [`Closed`] code after; the first thread to close wins, by compare-and-swap.
pub(crate) struct Shared {
    state: AtomicI32,
    /// The driver's clock, nanoseconds since the Unix epoch, when a command was last
    /// sent by either thread.
    last_sent: AtomicI64,
}

impl Shared {
    pub(crate) fn new(now: i64) -> Self {
        Self {
            state: AtomicI32::new(0),
            last_sent: AtomicI64::new(now),
        }
    }

    /// Why the client is closed, or `None` while it is open.
    pub(crate) fn closed(&self) -> Option<Closed> {
        Closed::from_code(self.state.load(Ordering::Acquire))
    }

    /// Closes with `reason` unless already closed; returns whether this call closed it.
    pub(crate) fn close(&self, reason: Closed) -> bool {
        self.state
            .compare_exchange(0, reason.code(), Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub(crate) fn sent(&self, now: i64) {
        self.last_sent.store(now, Ordering::Release);
    }

    fn last_sent(&self) -> i64 {
        self.last_sent.load(Ordering::Acquire)
    }
}

/// The driver's clock: nanoseconds since the Unix epoch, as the ring page measures
/// every timeout.
pub(crate) fn now_nanos() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_nanos()).unwrap_or(i64::MAX)
        })
}

pub(crate) const fn nanos(duration: Duration) -> i64 {
    let nanos = duration.as_nanos();
    if nanos > i64::MAX as u128 {
        i64::MAX
    } else {
        nanos as i64
    }
}

/// Aeron's backoff: spins, then yields, then sleeps from a microsecond to a millisecond.
struct Backoff {
    spins: u32,
    yields: u32,
    park: Duration,
}

impl Backoff {
    const fn new() -> Self {
        Self {
            spins: 0,
            yields: 0,
            park: Duration::from_micros(1),
        }
    }

    fn idle(&mut self) {
        if self.spins < 10 {
            self.spins += 1;
            std::hint::spin_loop();
        } else if self.yields < 20 {
            self.yields += 1;
            std::thread::yield_now();
        } else {
            std::thread::sleep(self.park);
            self.park = (self.park * 2).min(Duration::from_millis(1));
        }
    }

    const fn reset(&mut self) {
        self.spins = 0;
        self.yields = 0;
        self.park = Duration::from_micros(1);
    }
}

/// The conductor's state: its own producer on the ring, the one receiver on the
/// broadcast buffer, and what it shares with the user's thread.
pub(crate) struct Conductor {
    ring: CommandRing,
    producer: Producer,
    receiver: Receiver,
    shared: Arc<Shared>,
    client_id: i64,
    scratch: Vec<u8>,
}

impl Conductor {
    pub(crate) fn new(
        ring: CommandRing,
        producer: Producer,
        receiver: Receiver,
        shared: Arc<Shared>,
        client_id: i64,
    ) -> Self {
        Self {
            ring,
            producer,
            receiver,
            shared,
            client_id,
            scratch: vec![0u8; COMMAND_SCRATCH],
        }
    }

    /// The loop: until the client is closed by either thread.
    pub(crate) fn run(mut self) {
        let mut backoff = Backoff::new();
        while self.shared.closed().is_none() {
            let now = now_nanos();
            if let Some(reason) = self.liveness(now) {
                self.shared.close(reason);
                break;
            }
            let work = match self.duty_cycle(now) {
                Ok(work) => work,
                Err(reason) => {
                    self.shared.close(reason);
                    break;
                }
            };
            if work == 0 {
                backoff.idle();
            } else {
                backoff.reset();
            }
        }
    }

    /// One cycle's work after the heartbeat: the keepalive, then a batch of events.
    fn duty_cycle(&mut self, now: i64) -> Result<usize, Closed> {
        Ok(self.keepalive(now)? + self.events()?)
    }

    /// The consumer heartbeat: −1 is `driver-closed`; older than the driver timeout is
    /// `driver-timeout`.
    fn liveness(&self, now: i64) -> Option<Closed> {
        let heartbeat = self.ring.consumer_heartbeat();
        if heartbeat == DRIVER_CLOSED {
            Some(Closed::DriverClosed)
        } else if now.saturating_sub(heartbeat) > nanos(DRIVER_TIMEOUT) {
            Some(Closed::DriverTimeout)
        } else {
            None
        }
    }

    /// `client keepalive` when nothing was sent for the keepalive interval. A full ring
    /// and an unblocked claim are left to the next cycle, as the command ring page says
    /// a producer retries: a full ring clears, and an unblocked claim was not delivered.
    /// A driver the write finds closed or stale closes the client as the heartbeat
    /// would; any other refusal is a ring that failed a check of its page, and closes it.
    fn keepalive(&mut self, now: i64) -> Result<usize, Closed> {
        if now.saturating_sub(self.shared.last_sent()) < nanos(KEEPALIVE_INTERVAL) {
            return Ok(0);
        }
        let command = Command::ClientKeepalive {
            client_id: self.client_id,
            correlation_id: self.producer.next_correlation_id(),
        };
        let Ok(length) = command.write(&mut self.scratch) else {
            return Ok(0);
        };
        match self
            .producer
            .write(command.command_type() as i32, &self.scratch[..length], now)
        {
            Ok(()) => {
                self.shared.sent(now);
                Ok(1)
            }
            Err(CommandRingError::Full { .. } | CommandRingError::Unblocked { .. }) => Ok(0),
            Err(CommandRingError::DriverClosed) => Err(Closed::DriverClosed),
            Err(CommandRingError::DriverTimeout { .. }) => Err(Closed::DriverTimeout),
            Err(_) => Err(Closed::Corrupt),
        }
    }

    /// Up to a batch of events; each closes the client when [`event`] says so, and a
    /// lap or a corrupt buffer closes it too.
    fn events(&mut self) -> Result<usize, Closed> {
        let mut received = 0;
        let client_id = self.client_id;
        while received < 16 {
            let mut closed = None;
            let got = self.receiver.receive(|type_id, message| {
                closed = event(client_id, type_id, message);
            });
            match got {
                Ok(true) => {
                    received += 1;
                    if let Some(reason) = closed {
                        return Err(reason);
                    }
                }
                Ok(false) => break,
                Err(BroadcastError::Lapped { .. }) => return Err(Closed::Lapped),
                Err(_) => return Err(Closed::Corrupt),
            }
        }
        Ok(received)
    }
}

/// What one event means to client `client_id`: `client timeout` with its id closes it
/// with [`Closed::ClientTimeout`]; an event that fails a decoding check, `too-short`,
/// `type`, `string` or `length`, closes it with [`Closed::Corrupt`], since an event it
/// cannot decode may be the one it needed (`spec/client-protocol.md`, "What the driver
/// rejects"); every other event leaves it open.
fn event(client_id: i64, type_id: i32, message: &[u8]) -> Option<Closed> {
    match Event::parse(type_id, message) {
        Ok(Event::ClientTimeout { client_id: id }) if id == client_id => {
            Some(Closed::ClientTimeout)
        }
        Ok(_) => None,
        Err(_) => Some(Closed::Corrupt),
    }
}

#[cfg(test)]
mod tests {
    use super::event;
    use crate::error::Closed;

    // Vectors from `spec/vectors/client-protocol.json`, by name.

    /// `client-timeout`: type 3850, client id 5.
    const CLIENT_TIMEOUT: (i32, &[u8]) = (3850, &[5, 0, 0, 0, 0, 0, 0, 0]);
    /// `operation-success`: type 3844, correlation id 3.
    const OPERATION_SUCCESS: (i32, &[u8]) = (3844, &[3, 0, 0, 0, 0, 0, 0, 0]);
    /// The vectors whose `error` is set, with that error.
    const MALFORMED: [(&str, i32, &[u8]); 4] = [
        ("too-short", 3844, &[3, 0, 0, 0]),
        ("type", 3848, &[3, 0, 0, 0, 0, 0, 0, 0]),
        (
            "string",
            3841,
            &[
                4, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 8, 0, 0, 0, 0x61, 0x62, 0x63, 0,
            ],
        ),
        ("length", 3850, &[5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
    ];

    #[test]
    fn client_timeout_for_this_client_closes_it() {
        let (type_id, message) = CLIENT_TIMEOUT;
        assert_eq!(event(5, type_id, message), Some(Closed::ClientTimeout));
    }

    #[test]
    fn client_timeout_for_another_client_leaves_it_open() {
        let (type_id, message) = CLIENT_TIMEOUT;
        assert_eq!(event(6, type_id, message), None);
    }

    #[test]
    fn a_decoded_event_leaves_it_open() {
        let (type_id, message) = OPERATION_SUCCESS;
        assert_eq!(event(5, type_id, message), None);
    }

    #[test]
    fn an_event_that_fails_a_decoding_check_closes_it() {
        for (error, type_id, message) in MALFORMED {
            assert_eq!(
                event(5, type_id, message),
                Some(Closed::Corrupt),
                "the `{error}` vector"
            );
        }
    }
}
