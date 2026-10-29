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

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, AtomicI64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cavalry_proto::client_protocol::{Command, Event};
use cavalry_shm::broadcast::{BroadcastError, Receiver};
use cavalry_shm::command_ring::layout::DRIVER_CLOSED;
use cavalry_shm::command_ring::{CommandRing, CommandRingError, Producer};
use cavalry_shm::counters::CountersWriter;
use cavalry_shm::log::LogBuffers;
use cavalry_shm::spsc;

use crate::error::Closed;
use crate::replay::ReplayEnd;
use crate::response::Response;
use crate::subscription::{ImageEvent, Registration};

/// How often a client sends `client keepalive` when it has sent nothing else
/// (`spec/client-protocol.md`, "Liveness and timeouts").
pub(crate) const KEEPALIVE_INTERVAL: Duration = Duration::from_millis(500);

/// A consumer heartbeat older than this is `driver-timeout`.
pub(crate) const DRIVER_TIMEOUT: Duration = Duration::from_secs(10);

/// An add or remove command unanswered for this long is lost.
pub(crate) const RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);

/// No command is waiting for an answer: correlation ids are 0 and up.
pub(crate) const NO_PENDING: i64 = -1;

/// How many answers the response queue holds: one command is in flight per client, and
/// the rest is room for an answer that arrives after its caller gave up.
pub(crate) const RESPONSE_QUEUE: usize = 4;

/// How many subscriptions one client registers with its conductor: the driver's own
/// bound on subscriptions, so the driver refuses first, with error code 10
/// (`spec/client-protocol.md`, "Sizes"). A registration the registry cannot admit even
/// after a sweep is refused, never dropped silently: see [`Registry::admit`].
pub(crate) const MAX_SUBSCRIPTIONS: usize = 1024;

/// How many registrations may wait for the conductor between two of its cycles.
pub(crate) const REGISTRATION_QUEUE: usize = 64;

/// How many replays one client follows for their `replay stopped`: the driver's own
/// bound on replays (`spec/client-protocol.md`, "Sizes"; ADR 0030). A handle past it
/// is still a replay, and `ended` never says so for it; the driver refuses first.
pub(crate) const MAX_REPLAYS: usize = 64;

/// How many replay registrations may wait for the conductor between two of its cycles.
pub(crate) const REPLAY_QUEUE: usize = 16;

/// What `start replay` registers with the conductor before it sends the command: where
/// the conductor stores the replay's `replay stopped` when it arrives. The conductor
/// forgets a registration whose handle is gone.
pub(crate) struct ReplayRegistration {
    pub(crate) replay_session_id: i64,
    pub(crate) end: Arc<ReplayEnd>,
}

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
    /// The correlation id of the command the user's thread waits on, or [`NO_PENDING`].
    /// Stored before the command is written and loaded after its answer is read, so the
    /// ring and the broadcast buffer order the two: see [`Conductor::events`].
    pending: AtomicI64,
}

impl Shared {
    pub(crate) fn new(now: i64) -> Self {
        Self {
            state: AtomicI32::new(0),
            last_sent: AtomicI64::new(now),
            pending: AtomicI64::new(NO_PENDING),
        }
    }

    /// The user's thread names the command it waits on before it sends it, and clears
    /// it after the answer or the timeout.
    pub(crate) fn set_pending(&self, correlation_id: i64) {
        self.pending.store(correlation_id, Ordering::Release);
    }

    fn pending(&self) -> i64 {
        self.pending.load(Ordering::Acquire)
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
    dir: PathBuf,
    ring: CommandRing,
    producer: Producer,
    receiver: Receiver,
    counters: CountersWriter,
    shared: Arc<Shared>,
    responses: spsc::Producer<Response>,
    registrations: spsc::Consumer<Registration>,
    registry: Registry,
    replay_registrations: spsc::Consumer<ReplayRegistration>,
    replays: Vec<ReplayRegistration>,
    client_id: i64,
    scratch: Vec<u8>,
}

impl Conductor {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        dir: PathBuf,
        ring: CommandRing,
        producer: Producer,
        receiver: Receiver,
        counters: CountersWriter,
        shared: Arc<Shared>,
        responses: spsc::Producer<Response>,
        registrations: spsc::Consumer<Registration>,
        replay_registrations: spsc::Consumer<ReplayRegistration>,
        client_id: i64,
    ) -> Self {
        Self {
            dir,
            ring,
            producer,
            receiver,
            counters,
            shared,
            responses,
            registrations,
            registry: Registry::with_capacity(MAX_SUBSCRIPTIONS),
            replay_registrations,
            replays: Vec::with_capacity(MAX_REPLAYS),
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

    /// One cycle's work after the heartbeat: the keepalive, the subscriptions added or
    /// gone since the last cycle, then a batch of events.
    fn duty_cycle(&mut self, now: i64) -> Result<usize, Closed> {
        Ok(self.keepalive(now)? + self.registrations() + self.events()?)
    }

    /// Drops the mappings the subscriptions retired and forgets a registration whose
    /// subscription is gone, then admits the subscriptions the user's thread added: the
    /// sweep first, so that a subscription dropped before another was added leaves its
    /// place to it.
    fn registrations(&mut self) -> usize {
        self.registry.sweep() + self.admit()
    }

    /// Admits every registration queued, each through [`Registry::admit`], and every
    /// replay registration, after forgetting those whose handle is gone; a replay past
    /// the bound is dropped, and its handle never learns its end.
    fn admit(&mut self) -> usize {
        let mut work = 0;
        while let Some(registration) = self.registrations.poll() {
            self.registry.admit(registration);
            work += 1;
        }
        while let Some(registration) = self.replay_registrations.poll() {
            self.replays
                .retain(|replay| Arc::strong_count(&replay.end) > 1);
            if self.replays.len() < MAX_REPLAYS {
                self.replays.push(registration);
            }
            work += 1;
        }
        work
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

    /// Up to a batch of events; each closes the client when [`event`] says so, answers
    /// the command the user's thread waits on through the response queue when it is
    /// that command's event, and is dropped otherwise; a lap or a corrupt buffer closes
    /// the client too.
    ///
    /// The pending correlation id is loaded inside the handler, after the event is read,
    /// never before the receive. The user's thread stores it (release) before it writes
    /// the command; the driver reads the command from the ring, then writes its answer to
    /// the broadcast buffer, which this receive reads (acquire). So a load after the read
    /// sees the id of every command whose answer it is reading; a load before the
    /// receive could see [`NO_PENDING`] and drop the answer: a false `response-timeout`,
    /// with a registration left on the driver.
    ///
    /// The same chain orders the registration queue: `add subscription` queues its
    /// registration before it writes the command, so admitting the queue before an
    /// answer or an image is handled finds every registration the driver could have
    /// answered, and an image that follows its answer in one batch finds its
    /// subscription.
    fn events(&mut self) -> Result<usize, Closed> {
        let mut received = 0;
        let client_id = self.client_id;
        while received < 16 {
            let shared = &self.shared;
            let mut outcome = Ok(Outcome::Nothing);
            let got = self.receiver.receive(|type_id, message| {
                outcome = event(client_id, shared.pending(), type_id, message);
            });
            match got {
                Ok(true) => {
                    received += 1;
                    let outcome = outcome?;
                    if outcome != Outcome::Nothing {
                        self.admit();
                    }
                    match outcome {
                        Outcome::Nothing => {}
                        Outcome::Answer(response) => {
                            // Full means the caller gave up on several answers in a row and
                            // the queue holds them; the caller clears the queue before its
                            // next command, so a dropped stale answer is nothing lost.
                            let _ = self.responses.offer(response);
                        }
                        Outcome::ImageAvailable {
                            correlation_id,
                            session_id,
                            subscriber_registration_id,
                            position_counter_id,
                            log_id,
                            source_identity,
                        } => self.image_available(
                            correlation_id,
                            session_id,
                            subscriber_registration_id,
                            position_counter_id,
                            log_id,
                            source_identity,
                        )?,
                        Outcome::ImageUnavailable {
                            correlation_id,
                            subscriber_registration_id,
                        } => self.image_unavailable(correlation_id, subscriber_registration_id)?,
                        Outcome::ReplayStopped {
                            replay_session_id,
                            position,
                            reason,
                        } => {
                            if let Some(replay) = self
                                .replays
                                .iter()
                                .find(|replay| replay.replay_session_id == replay_session_id)
                            {
                                replay.end.set(position, reason);
                            }
                        }
                    }
                }
                Ok(false) => break,
                Err(BroadcastError::Lapped { .. }) => return Err(Closed::Lapped),
                Err(_) => return Err(Closed::Corrupt),
            }
        }
        Ok(received)
    }

    /// `available image`: opens the log read-only and takes the owner's handle on the
    /// position counter the driver allocated, then hands both to the subscription. An
    /// image for a subscription this client no longer holds is dropped, as the driver
    /// will withdraw it on the remove. A log that cannot be opened or a counter that is
    /// not allocated is the driver saying one thing and the directory another: corrupt.
    /// A subscription whose queue is full has not polled while the driver announced a
    /// queue's worth of images: `overrun`.
    fn image_available(
        &mut self,
        correlation_id: i64,
        session_id: i32,
        subscriber_registration_id: i64,
        position_counter_id: i32,
        log_id: i64,
        source_identity: String,
    ) -> Result<(), Closed> {
        let Some(registration) = self.registry.get(subscriber_registration_id) else {
            return Ok(());
        };
        let log = LogBuffers::open(&self.dir, log_id, false).map_err(|_| Closed::Corrupt)?;
        let position = u32::try_from(position_counter_id)
            .ok()
            .and_then(|id| self.counters.counter(id).ok())
            .ok_or(Closed::Corrupt)?;
        registration
            .images
            .offer(ImageEvent::Available {
                correlation_id,
                session_id,
                log,
                position,
                source_identity,
            })
            .map_err(|_| Closed::Overrun)
    }

    /// `unavailable image`: tells the subscription, which retires the image on its next
    /// poll and hands the mapping back here.
    fn image_unavailable(
        &mut self,
        correlation_id: i64,
        subscriber_registration_id: i64,
    ) -> Result<(), Closed> {
        let Some(registration) = self.registry.get(subscriber_registration_id) else {
            return Ok(());
        };
        registration
            .images
            .offer(ImageEvent::Unavailable { correlation_id })
            .map_err(|_| Closed::Overrun)
    }
}

/// The subscriptions one client holds, by registration id, with the queues to each:
/// bounded, and never full in silence.
pub(crate) struct Registry {
    subscriptions: Vec<Registration>,
    capacity: usize,
}

impl Registry {
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            subscriptions: Vec::with_capacity(capacity),
            capacity,
        }
    }

    /// Admits `registration`, sweeping first when the registry is full. Still full, the
    /// registration is dropped here and the call returns `false`: its subscription's
    /// halves of the two queues see their peer dropped, and `add subscription`, which
    /// looks after the answer, removes the subscription and returns
    /// [`crate::ClientError::SubscriptionsFull`]. A subscription is refused, never left
    /// without its images.
    pub(crate) fn admit(&mut self, registration: Registration) -> bool {
        if self.subscriptions.len() >= self.capacity {
            self.sweep();
        }
        if self.subscriptions.len() >= self.capacity {
            drop(registration);
            return false;
        }
        self.subscriptions.push(registration);
        true
    }

    /// Drops the mappings the subscriptions retired, and forgets a registration whose
    /// subscription is gone, which its queue's dropped half says. Returns the work done.
    pub(crate) fn sweep(&mut self) -> usize {
        let mut work = 0;
        let mut index = 0;
        while index < self.subscriptions.len() {
            while let Some(retired) = self.subscriptions[index].retired.poll() {
                drop(retired);
                work += 1;
            }
            if self.subscriptions[index].images.peer_dropped() {
                self.subscriptions.swap_remove(index);
                work += 1;
            } else {
                index += 1;
            }
        }
        work
    }

    /// The registration of the subscription with `registration_id`.
    pub(crate) fn get(&mut self, registration_id: i64) -> Option<&mut Registration> {
        self.subscriptions
            .iter_mut()
            .find(|registration| registration.registration_id == registration_id)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.subscriptions.len()
    }
}

/// What one event is to the conductor, once decoded and not a close.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// Not for this client, or for a caller that gave up.
    Nothing,
    /// The answer to the command the user's thread waits on.
    Answer(Response),
    /// `available image` for one of this client's subscriptions.
    ImageAvailable {
        correlation_id: i64,
        session_id: i32,
        subscriber_registration_id: i64,
        position_counter_id: i32,
        log_id: i64,
        source_identity: String,
    },
    /// `unavailable image` for one of this client's subscriptions.
    ImageUnavailable {
        correlation_id: i64,
        subscriber_registration_id: i64,
    },
    /// `replay stopped`, for a replay this client may hold a handle on.
    ReplayStopped {
        replay_session_id: i64,
        position: i64,
        reason: i32,
    },
}

/// The registration id a log file name carries: `publications/<id>.logbuffer`, as
/// `spec/client-protocol.md`, "The driver directory" names it.
fn log_id(log_file_name: &[u8]) -> Option<i64> {
    let name = std::str::from_utf8(log_file_name).ok()?;
    name.strip_prefix("publications/")?
        .strip_suffix(".logbuffer")?
        .parse()
        .ok()
}

/// What one event means to client `client_id` while it waits on the command with
/// correlation id `pending` ([`NO_PENDING`] for none): `client timeout` with the client's
/// id closes it with [`Closed::ClientTimeout`]; an event that fails a decoding check,
/// `too-short`, `type`, `string` or `length`, closes it with [`Closed::Corrupt`], since
/// an event it cannot decode may be the one it needed (`spec/client-protocol.md`, "What
/// the driver rejects"); `available image` and `unavailable image` are for the
/// subscription they name, whatever is pending, and `replay stopped` is for the replay
/// handle it names; `publication ready`, `exclusive publication ready`, `subscription
/// ready`, `operation success`, `recording ready`, `replay ready` and `error` with the
/// pending correlation id are the answer; every other event, `recording stopped` among
/// them, leaves the client open and is nothing to it. An `available image` whose log
/// file name is not the page's shape is `corrupt`.
fn event(client_id: i64, pending: i64, type_id: i32, message: &[u8]) -> Result<Outcome, Closed> {
    let event = Event::parse(type_id, message).map_err(|_| Closed::Corrupt)?;
    if let Event::ClientTimeout { client_id: id } = event
        && id == client_id
    {
        return Err(Closed::ClientTimeout);
    }
    match event {
        Event::AvailableImage {
            correlation_id,
            session_id,
            subscriber_registration_id,
            subscriber_position_counter_id,
            log_file_name,
            source_identity,
            ..
        } => {
            return Ok(Outcome::ImageAvailable {
                correlation_id,
                session_id,
                subscriber_registration_id,
                position_counter_id: subscriber_position_counter_id,
                log_id: log_id(log_file_name).ok_or(Closed::Corrupt)?,
                source_identity: String::from_utf8_lossy(source_identity).into_owned(),
            });
        }
        Event::UnavailableImage {
            correlation_id,
            subscriber_registration_id,
            ..
        } => {
            return Ok(Outcome::ImageUnavailable {
                correlation_id,
                subscriber_registration_id,
            });
        }
        Event::ReplayStopped {
            replay_session_id,
            position,
            reason,
        } => {
            return Ok(Outcome::ReplayStopped {
                replay_session_id,
                position,
                reason,
            });
        }
        _ => {}
    }
    if pending == NO_PENDING {
        return Ok(Outcome::Nothing);
    }
    Ok(match event {
        Event::PublicationReady {
            correlation_id,
            registration_id,
            session_id,
            stream_id,
            publication_limit_counter_id,
            log_file_name,
        } if correlation_id == pending => Outcome::Answer(Response::PublicationReady {
            correlation_id,
            registration_id,
            session_id,
            stream_id,
            limit_counter_id: publication_limit_counter_id,
            log_file_name: String::from_utf8_lossy(log_file_name).into_owned(),
            exclusive: false,
        }),
        Event::ExclusivePublicationReady {
            correlation_id,
            registration_id,
            session_id,
            stream_id,
            publication_limit_counter_id,
            log_file_name,
        } if correlation_id == pending => Outcome::Answer(Response::PublicationReady {
            correlation_id,
            registration_id,
            session_id,
            stream_id,
            limit_counter_id: publication_limit_counter_id,
            log_file_name: String::from_utf8_lossy(log_file_name).into_owned(),
            exclusive: true,
        }),
        Event::SubscriptionReady { correlation_id } if correlation_id == pending => {
            Outcome::Answer(Response::SubscriptionReady { correlation_id })
        }
        Event::OperationSuccess { correlation_id } if correlation_id == pending => {
            Outcome::Answer(Response::Success { correlation_id })
        }
        Event::RecordingReady {
            correlation_id,
            recording_id,
            session_id,
            recording_position_counter_id,
            recording_synced_position_counter_id,
        } if correlation_id == pending => Outcome::Answer(Response::RecordingReady {
            correlation_id,
            recording_id,
            session_id,
            position_counter_id: recording_position_counter_id,
            synced_position_counter_id: recording_synced_position_counter_id,
        }),
        Event::ReplayReady {
            correlation_id,
            replay_session_id,
            recording_id,
            start_position,
            session_id,
            stream_id,
        } if correlation_id == pending => Outcome::Answer(Response::ReplayReady {
            correlation_id,
            replay_session_id,
            recording_id,
            start_position,
            session_id,
            stream_id,
        }),
        Event::Error {
            offending_correlation_id,
            error_code,
            error_message,
        } if offending_correlation_id == pending => Outcome::Answer(Response::Error {
            correlation_id: offending_correlation_id,
            code: error_code,
            message: String::from_utf8_lossy(error_message).into_owned(),
        }),
        _ => Outcome::Nothing,
    })
}

#[cfg(test)]
mod tests {
    use cavalry_shm::spsc;

    use super::{MAX_SUBSCRIPTIONS, NO_PENDING, Outcome, Registry, event, log_id};
    use crate::error::Closed;
    use crate::response::Response;
    use crate::subscription::{ImageEvent, Registration, Retired};

    /// A registration and the subscription's halves of its two queues.
    fn registration(
        registration_id: i64,
    ) -> (
        Registration,
        spsc::Consumer<ImageEvent>,
        spsc::Producer<Retired>,
    ) {
        let (images, incoming) = spsc::queue::<ImageEvent>(2);
        let (retired, retired_consumer) = spsc::queue::<Retired>(2);
        (
            Registration {
                registration_id,
                images,
                retired: retired_consumer,
            },
            incoming,
            retired,
        )
    }

    #[test]
    fn a_full_registry_sweeps_then_refuses_and_its_subscription_sees_the_refusal() {
        let mut registry = Registry::with_capacity(MAX_SUBSCRIPTIONS);
        let mut held = Vec::with_capacity(MAX_SUBSCRIPTIONS);
        for id in 0..MAX_SUBSCRIPTIONS as i64 {
            let (registration, incoming, retired) = registration(id);
            assert!(registry.admit(registration));
            held.push((incoming, retired));
        }
        assert_eq!(registry.len(), MAX_SUBSCRIPTIONS);

        // Full, with every subscription alive: the next is refused, and its
        // subscription's halves see their peer dropped, which `add subscription` reports.
        let over = MAX_SUBSCRIPTIONS as i64;
        let (refused, incoming, retired) = registration(over);
        assert!(!registry.admit(refused));
        assert!(incoming.peer_dropped());
        assert!(retired.peer_dropped());
        assert!(registry.get(over).is_none());
        assert_eq!(registry.len(), MAX_SUBSCRIPTIONS);

        // A subscription dropped, then another added before any cycle swept: the
        // admission sweeps first, and the new one takes the place.
        drop(held.swap_remove(0));
        let (admitted, incoming, _retired) = registration(over + 1);
        assert!(registry.admit(admitted));
        assert!(!incoming.peer_dropped());
        assert!(registry.get(0).is_none());
        assert!(registry.get(over + 1).is_some());
        assert_eq!(registry.len(), MAX_SUBSCRIPTIONS);
    }

    #[test]
    fn a_sweep_forgets_a_registration_whose_subscription_is_gone() {
        let mut registry = Registry::with_capacity(4);
        let (first, first_incoming, first_retired) = registration(1);
        let (second, _second_incoming, _second_retired) = registration(2);
        assert!(registry.admit(first));
        assert!(registry.admit(second));
        drop((first_incoming, first_retired));
        assert_eq!(registry.sweep(), 1);
        assert!(registry.get(1).is_none());
        assert!(registry.get(2).is_some());
        assert_eq!(registry.sweep(), 0);
    }

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
        assert_eq!(
            event(5, NO_PENDING, type_id, message),
            Err(Closed::ClientTimeout)
        );
        assert_eq!(event(5, 3, type_id, message), Err(Closed::ClientTimeout));
    }

    #[test]
    fn client_timeout_for_another_client_leaves_it_open() {
        let (type_id, message) = CLIENT_TIMEOUT;
        assert_eq!(event(6, NO_PENDING, type_id, message), Ok(Outcome::Nothing));
    }

    #[test]
    fn a_decoded_event_nobody_waits_for_leaves_it_open() {
        let (type_id, message) = OPERATION_SUCCESS;
        assert_eq!(event(5, NO_PENDING, type_id, message), Ok(Outcome::Nothing));
        assert_eq!(
            event(5, 4, type_id, message),
            Ok(Outcome::Nothing),
            "another command's"
        );
    }

    #[test]
    fn the_pending_commands_event_is_its_answer() {
        let (type_id, message) = OPERATION_SUCCESS;
        assert_eq!(
            event(5, 3, type_id, message),
            Ok(Outcome::Answer(Response::Success { correlation_id: 3 }))
        );
        // `error`: type 3841, offending correlation id 4, code 1, message "abc".
        let error: &[u8] = &[
            4, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 3, 0, 0, 0, 0x61, 0x62, 0x63, 0,
        ];
        assert_eq!(
            event(5, 4, 3841, error),
            Ok(Outcome::Answer(Response::Error {
                correlation_id: 4,
                code: 1,
                message: "abc".to_string()
            }))
        );
    }

    #[test]
    fn an_image_event_is_for_its_subscription_whatever_is_pending() {
        // An `available image`: correlation 7, session -2, stream 1001, subscriber
        // registration 6, position counter 14, log "publications/7.logbuffer", source
        // "cavalry:ipc" (11 bytes, one byte of padding).
        let mut message = Vec::new();
        message.extend_from_slice(&7i64.to_le_bytes());
        message.extend_from_slice(&(-2i32).to_le_bytes());
        message.extend_from_slice(&1001i32.to_le_bytes());
        message.extend_from_slice(&6i64.to_le_bytes());
        message.extend_from_slice(&14i32.to_le_bytes());
        let log = b"publications/7.logbuffer";
        message.extend_from_slice(&(log.len() as i32).to_le_bytes());
        message.extend_from_slice(log);
        let source = b"cavalry:ipc";
        message.extend_from_slice(&(source.len() as i32).to_le_bytes());
        message.extend_from_slice(source);
        message.push(0);
        assert_eq!(
            event(5, NO_PENDING, 0x0F02, &message),
            Ok(Outcome::ImageAvailable {
                correlation_id: 7,
                session_id: -2,
                subscriber_registration_id: 6,
                position_counter_id: 14,
                log_id: 7,
                source_identity: "cavalry:ipc".to_string(),
            })
        );
        assert_eq!(log_id(b"publications/42.logbuffer"), Some(42));
        assert_eq!(log_id(b"42.logbuffer"), None);
        assert_eq!(log_id(b"publications/x.logbuffer"), None);
    }

    #[test]
    fn an_event_that_fails_a_decoding_check_closes_it() {
        for (error, type_id, message) in MALFORMED {
            assert_eq!(
                event(5, NO_PENDING, type_id, message),
                Err(Closed::Corrupt),
                "the `{error}` vector"
            );
        }
    }
}
