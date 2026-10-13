//! The client conductor: the thread [`crate::Cavalry::connect`] starts, which keeps the
//! client alive and reads what the driver says (ADR 0021; `spec/client-protocol.md`,
//! "Liveness and timeouts"). Every 500 ms with nothing else sent it writes `client
//! keepalive`; on every cycle it loads the consumer heartbeat, which closes the client
//! on `driver-closed` or `driver-timeout`, and receives every event, which closes the
//! client on its own `client timeout` or on a lap of the broadcast buffer. It shares
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
            let mut work = 0;
            work += self.keepalive(now);
            match self.events() {
                Ok(received) => work += received,
                Err(reason) => {
                    self.shared.close(reason);
                    break;
                }
            }
            if work == 0 {
                backoff.idle();
            } else {
                backoff.reset();
            }
        }
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

    /// `client keepalive` when nothing was sent for the keepalive interval. A ring that
    /// refuses the write is left to the next cycle: a full ring clears, and a gone
    /// driver is found by the heartbeat.
    fn keepalive(&mut self, now: i64) -> usize {
        if now.saturating_sub(self.shared.last_sent()) < nanos(KEEPALIVE_INTERVAL) {
            return 0;
        }
        let command = Command::ClientKeepalive {
            client_id: self.client_id,
            correlation_id: self.producer.next_correlation_id(),
        };
        let Ok(length) = command.write(&mut self.scratch) else {
            return 0;
        };
        match self
            .producer
            .write(command.command_type() as i32, &self.scratch[..length], now)
        {
            Ok(()) => {
                self.shared.sent(now);
                1
            }
            Err(CommandRingError::Full { .. } | CommandRingError::Unblocked { .. }) => 0,
            Err(_) => 0,
        }
    }

    /// Up to a batch of events; `client timeout` with this client's id closes the
    /// client, and a lap or a corrupt buffer closes it too.
    fn events(&mut self) -> Result<usize, Closed> {
        let mut received = 0;
        let client_id = self.client_id;
        while received < 16 {
            let mut timed_out = false;
            let got = self.receiver.receive(|type_id, message| {
                if let Ok(Event::ClientTimeout { client_id: id }) = Event::parse(type_id, message)
                    && id == client_id
                {
                    timed_out = true;
                }
            });
            match got {
                Ok(true) => {
                    received += 1;
                    if timed_out {
                        return Err(Closed::ClientTimeout);
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
