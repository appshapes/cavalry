//! The client conductor: the thread [`crate::Cavalry::connect`] starts, which keeps the
//! client alive and reads what the driver says (ADR 0021; `spec/client-protocol.md`,
//! "Liveness and timeouts"). Every 500 ms with nothing else sent it writes `client
//! keepalive`; on every cycle it loads the consumer heartbeat, which closes the client
//! on `driver-closed` or `driver-timeout`, and receives every event, which closes the
//! client on its own `client timeout`, on an event that fails a decoding check, or on
//! a lap of the broadcast buffer. A ring or a buffer that fails a check of its page
//! closes the client too. It shares
//! two atomics with the user's thread and nothing else: the state and the time of the
//! last command sent. It idles with a backoff, so a quiet client costs a parked thread.
//! On macOS the client has a second thread, its watch, and each of the two ends a park
//! of the other that outlived its timeout (ADR 0177; `watch.rs`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, AtomicI64, Ordering};
use std::time::Duration;

use cavalry_proto::client_protocol::{Command, Event};
use cavalry_shm::broadcast::{BroadcastError, Receiver};
use cavalry_shm::command_ring::layout::DRIVER_CLOSED;
use cavalry_shm::command_ring::{CommandRing, CommandRingError, Producer};
use cavalry_shm::counters::CountersWriter;
use cavalry_shm::counters::generated::{CLIENT_CONDUCTOR_PARKS_OVERDUE, SUBSCRIPTION_POSITION};

use crate::owned::Owned as Counter;
use cavalry_shm::log::{LogBuffers, LogDirectory, LogError};
use cavalry_shm::spsc;

use crate::error::Closed;
use crate::recording::RecordingStop;
use crate::replay::ReplayEnd;
use crate::response::Response;
use crate::subscription::{ImageEvent, Registration};
use crate::watch::{Pair, WakeDue};

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

/// How many replays one client follows for their `replay stopped` at once: the
/// driver's own bound on replays, starting or live (`spec/client-protocol.md`, "Sizes";
/// ADR 0030), the one definition of which is `cavalry_proto`'s (ADR 0036).
/// [`ReplayRegistry`] forgets a replay once its end is stored or its handle
/// is gone, so what it holds is bounded by what the driver holds, and its capacity is
/// one more than this for the command in flight. Full even so, a registration is
/// refused, never dropped in silence: `start_replay` stops the replay and returns
/// [`crate::ClientError::ReplaysFull`].
pub(crate) const MAX_REPLAYS: usize = cavalry_proto::client_protocol::MAX_REPLAYS;

/// How many replay registrations may wait for the conductor between two of its cycles.
/// One command is in flight per client, so one waits at most; the rest is room.
pub(crate) const REPLAY_QUEUE: usize = 16;

/// What `start replay` registers with the conductor before it sends the command: where
/// the conductor stores the replay's `replay stopped` when it arrives. The conductor
/// forgets a registration once the end is stored or the handle is gone.
pub(crate) struct ReplayRegistration {
    pub(crate) replay_session_id: i64,
    pub(crate) end: Arc<ReplayEnd>,
}

/// How many recordings the driver holds, starting or live
/// (`spec/client-protocol.md`, "Sizes"), and so how many this client can have started
/// that have not stopped. The registry of the stops this client follows is one more
/// than this, for the command in flight, as [`MAX_REPLAYS`] is for replays.
pub(crate) const MAX_RECORDINGS: usize = cavalry_proto::client_protocol::MAX_RECORDINGS;

/// How many recording registrations may wait for the conductor between two of its
/// cycles. One command is in flight per client, so one waits at most; the rest is room.
pub(crate) const RECORDING_QUEUE: usize = 16;

/// What `start recording` registers with the conductor before it sends the command:
/// where the conductor stores the recording's `recording stopped` when it arrives. The
/// conductor learns the recording id from `recording ready`, and forgets a registration
/// once the stop is stored or the handle is gone.
pub(crate) struct RecordingRegistration {
    pub(crate) correlation_id: i64,
    pub(crate) stop: Arc<RecordingStop>,
    /// Where the conductor puts the command's second answer when the driver arms the
    /// recording instead of starting it (`spec/client-protocol.md`, "Commands", start
    /// recording): the queue of one an [`ArmedRecording`](crate::ArmedRecording)
    /// polls. `None` for `start_recording`, which holds no armed recording.
    pub(crate) armed: Option<spsc::Producer<Response>>,
}

/// How many registrations the conductor's recordings registry holds: the recordings
/// the driver holds, starting or live, the recordings it holds armed, at most one for
/// each stream and endpoint a live subscription holds and so at most its subscriptions
/// registry, and one for the command in flight.
pub(crate) const RECORDING_REGISTRY: usize = MAX_RECORDINGS + MAX_SUBSCRIPTIONS + 1;

/// The scratch buffer every command is encoded into: the longest command is a channel
/// of 4096 bytes and its header, and the ring's own limit is far above.
pub(crate) const COMMAND_SCRATCH: usize = 4096 + 64;

/// What the two threads share. The state is 0 while the client is open and a
/// [`Closed`] code after; the first thread to close wins, by compare-and-swap.
pub(crate) struct Shared {
    /// Loaded by every claim and offer before the limit, so in a slot of its own,
    /// apart from `last_sent`, which the conductor stores at each keepalive.
    state: StateSlot,
    /// The client's nano clock when a command was last sent by either thread.
    last_sent: AtomicI64,
    /// The correlation id of the command the user's thread waits on, or [`NO_PENDING`].
    /// Stored before the command is written and loaded after its answer is read, so the
    /// ring and the broadcast buffer order the two: see [`Conductor::events`].
    pending: AtomicI64,
    /// The client's lock, where it could take one. Everything of the client that can
    /// store into a counter holds this state, so the lock is let go when the last of
    /// them is dropped (`spec/client-protocol.md`, "The records a client writes").
    _lock: Option<crate::owned::ClientLock>,
}

/// The client's state word in a 128-byte slot of its own: a publisher loads it on
/// every claim and offer in its hot loop, and a store to a word beside it would make
/// that load fetch the line back from the storing core (ADR 0011;
/// `.claude/rules/data-plane.md`).
#[repr(C, align(128))]
struct StateSlot(AtomicI32);

impl Shared {
    pub(crate) fn new(now: i64, lock: Option<crate::owned::ClientLock>) -> Self {
        Self {
            state: StateSlot(AtomicI32::new(0)),
            last_sent: AtomicI64::new(now),
            pending: AtomicI64::new(NO_PENDING),
            _lock: lock,
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
        Closed::from_code(self.state.0.load(Ordering::Acquire))
    }

    /// Closes with `reason` unless already closed; returns whether this call closed it.
    pub(crate) fn close(&self, reason: Closed) -> bool {
        self.state
            .0
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

// The client's two clocks (`spec/client-protocol.md`, "Liveness and timeouts"; ADR
// 0109). The epoch clock is for the consumer heartbeat's age, here and in every claim
// on the command ring: the driver stored its own epoch clock there. The nano clock is
// for the keepalive interval, which a step of the host's clock must not move.
pub(crate) use cavalry_shm::clock::{epoch_time, nano_time};

pub(crate) const fn nanos(duration: Duration) -> i64 {
    let nanos = duration.as_nanos();
    if nanos > i64::MAX as u128 {
        i64::MAX
    } else {
        nanos as i64
    }
}

/// Aeron's backoff: spins, then yields, then parks from a microsecond to a millisecond.
///
/// The park is [`std::thread::park_timeout`], not a sleep, so that the client's close
/// can end it: an unpark of the conductor's thread makes the park return at once. The
/// close stores its reason and then unparks, and does not depend on the park's own
/// timeout, which is not sure on every machine: on one Mac a wait of 1 ms in about 40
/// million did not return until its thread was woken (ADR 0171).
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

    /// `due` is where the conductor says when its park is to end, for the client's
    /// watch (ADR 0177): two stores and a read of the clock around the park, and none
    /// for a client that has no watch.
    fn idle(&mut self, due: Option<&WakeDue>) {
        if self.spins < 10 {
            self.spins += 1;
            std::hint::spin_loop();
        } else if self.yields < 20 {
            self.yields += 1;
            std::thread::yield_now();
        } else {
            #[cfg(test)]
            let park = {
                let held = PARK_FOR_TEST.get();
                if PARK_FOR_TEST_ONCE.get() {
                    PARK_FOR_TEST.set(None);
                }
                held.unwrap_or(self.park)
            };
            #[cfg(not(test))]
            let park = self.park;
            match due {
                Some(due) => {
                    // What was asked, not what a test makes of it: the watch ends a
                    // park that outlived the timeout it was given.
                    due.parking(nano_time(), self.park);
                    std::thread::park_timeout(park);
                    due.woke();
                }
                None => std::thread::park_timeout(park),
            }
            self.park = (self.park * 2).min(Duration::from_millis(1));
        }
    }

    const fn reset(&mut self) {
        self.spins = 0;
        self.yields = 0;
        self.park = Duration::from_micros(1);
    }
}

#[cfg(test)]
thread_local! {
    /// For a test: how long this thread's backoff parks, in place of at most 1 ms. A
    /// client takes the value of the thread that connects it for its conductor's
    /// thread. An hour stands for the park whose timeout never comes, so that a test
    /// passes only when something unparks the conductor.
    pub(crate) static PARK_FOR_TEST: std::cell::Cell<Option<Duration>> =
        const { std::cell::Cell::new(None) };
    /// For a test: [`PARK_FOR_TEST`] is for the conductor's next park alone, so that it
    /// parks as it should once that park has been ended.
    pub(crate) static PARK_FOR_TEST_ONCE: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    /// For a test: whether a client that this thread connects has a watch, in place of
    /// what its system gives, and with it how long the watch's first park lasts in
    /// place of a period. `Some(None)` is no watch; `Some(Some(None))` a watch;
    /// `Some(Some(Some(hour)))` a watch whose first park is held.
    pub(crate) static WATCH_FOR_TEST: std::cell::Cell<Option<Option<Option<Duration>>>> =
        const { std::cell::Cell::new(None) };
    /// For a test: a flag that holds the conductor of a client this thread connects.
    /// While it is set the conductor does nothing at the top of its pass, as a thread
    /// that its system does not run does nothing, and the application's thread goes
    /// on. A client takes the flag of the thread that connects it for its conductor's
    /// thread.
    pub(crate) static HOLD_FOR_TEST: std::cell::RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>> =
        const { std::cell::RefCell::new(None) };
}

/// The count of overdue parks as the conductor has published it (ADR 0177): nothing
/// until the first one, then a counter of the client's own that the conductor adds
/// with `add counter`, as any client adds one (ADR 0062), and stores the count in.
enum OverduePublished {
    /// No park was overdue yet: the client has no counter for it.
    None,
    /// `add counter` was sent with this correlation id, and its answer has not come.
    Asked(i64),
    /// The counter, and the count last stored in it.
    Counter(Counter, u64),
    /// The driver refused the counter, most likely for want of a record, or named a
    /// record this client cannot write: the count stays in the process, and the
    /// conductor does not ask again. A record the driver gave and this client could
    /// not open stays the client's until its close, as every counter of a client does.
    Refused,
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
    replays: ReplayRegistry,
    recording_registrations: spsc::Consumer<RecordingRegistration>,
    recordings: RecordingRegistry,
    client_id: i64,
    scratch: Vec<u8>,
    /// What this conductor shares with the client's watch, where the client has one.
    pair: Option<Arc<Pair>>,
    /// The count of overdue parks as published.
    overdue: OverduePublished,
}

/// Takes the driver's answer to the conductor's own `add counter`, when the event is
/// that answer: the counter, in which the count is stored at the next pass, or a
/// refusal, after which the count stays in the process. Every other event is left as
/// it is, and is decoded here only while that answer is awaited.
fn own_answer(
    overdue: &mut OverduePublished,
    counters: &CountersWriter,
    shared: &Arc<Shared>,
    type_id: i32,
    message: &[u8],
) {
    let OverduePublished::Asked(asked) = *overdue else {
        return;
    };
    match Event::parse(type_id, message) {
        Ok(Event::CounterReady {
            correlation_id,
            counter_id,
        }) if correlation_id == asked => {
            *overdue = match u32::try_from(counter_id)
                .ok()
                .and_then(|id| counters.counter(id).ok())
            {
                Some(counter) => {
                    OverduePublished::Counter(Counter::new(counter, Arc::clone(shared)), 0)
                }
                None => OverduePublished::Refused,
            };
        }
        Ok(Event::Error {
            offending_correlation_id,
            ..
        }) if offending_correlation_id == asked => {
            *overdue = OverduePublished::Refused;
        }
        _ => {}
    }
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
        recording_registrations: spsc::Consumer<RecordingRegistration>,
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
            replays: ReplayRegistry::with_capacity(MAX_REPLAYS + 1),
            recording_registrations,
            recordings: RecordingRegistry::with_capacity(RECORDING_REGISTRY),
            client_id,
            scratch: vec![0u8; COMMAND_SCRATCH],
            pair: None,
            overdue: OverduePublished::None,
        }
    }

    /// Gives the conductor the client's watch.
    pub(crate) fn watched(mut self, pair: Option<Arc<Pair>>) -> Self {
        self.pair = pair;
        self
    }

    /// The loop: until the client is closed by either thread.
    pub(crate) fn run(mut self) {
        let mut backoff = Backoff::new();
        let pair = self.pair.clone();
        if let Some(pair) = &pair {
            let _ = pair.conductor_thread.set(std::thread::current());
        }
        // The due time of the watch's park last counted (ADR 0177).
        let mut counted = 0;
        #[cfg(test)]
        let hold = HOLD_FOR_TEST.with_borrow(Clone::clone);
        while self.shared.closed().is_none() {
            #[cfg(test)]
            if let Some(hold) = &hold {
                while hold.load(std::sync::atomic::Ordering::Acquire)
                    && self.shared.closed().is_none()
                {
                    std::thread::park_timeout(Duration::from_millis(1));
                }
            }
            let epoch = epoch_time();
            if let Some(reason) = self.liveness(epoch) {
                self.shared.close(reason);
                break;
            }
            let now = nano_time();
            let work = match self.duty_cycle(epoch, now) {
                Ok(work) => work,
                Err(reason) => {
                    self.shared.close(reason);
                    break;
                }
            };
            if let Some(pair) = &pair {
                self.publish_overdue(pair, epoch);
            }
            if work == 0 {
                if let Some(pair) = &pair {
                    pair.conductor_looks(now, &mut counted);
                }
                backoff.idle(pair.as_deref().map(|pair| &pair.conductor));
            } else {
                backoff.reset();
            }
        }
    }

    /// Publishes the count of overdue parks when it has moved: one load and one
    /// compare on a pass where it has not. The first time, the conductor adds the
    /// client's counter for it, and stores the count once the driver has answered.
    fn publish_overdue(&mut self, pair: &Pair, epoch: i64) {
        let total = pair.parks_overdue.load(Ordering::Acquire);
        match &mut self.overdue {
            // Not once the client is closed: a command after its `client close` would
            // read to the driver as a new client's first.
            OverduePublished::None if total > 0 && self.shared.closed().is_none() => {
                let correlation_id = self.producer.next_correlation_id();
                let key = self.client_id.to_le_bytes();
                let mut label = [0u8; 64];
                let label = {
                    use std::io::Write;
                    let mut rest = &mut label[..];
                    let _ = write!(rest, "client.conductor_parks_overdue {}", self.client_id);
                    let used = 64 - rest.len();
                    &label[..used]
                };
                let command = Command::AddCounter {
                    client_id: self.client_id,
                    correlation_id,
                    type_id: CLIENT_CONDUCTOR_PARKS_OVERDUE.0,
                    key: &key,
                    label,
                };
                let Ok(length) = command.write(&mut self.scratch) else {
                    return;
                };
                // A full ring, or a claim that was unblocked, leaves it to a later pass.
                if self
                    .producer
                    .write(
                        command.command_type() as i32,
                        &self.scratch[..length],
                        epoch,
                    )
                    .is_ok()
                {
                    self.overdue = OverduePublished::Asked(correlation_id);
                }
            }
            OverduePublished::Counter(counter, stored) if *stored != total => {
                counter.store_release(i64::try_from(total).unwrap_or(i64::MAX));
                *stored = total;
            }
            _ => {}
        }
    }

    /// One cycle's work after the heartbeat: the keepalive, the subscriptions added or
    /// gone since the last cycle, then a batch of events.
    fn duty_cycle(&mut self, epoch: i64, now: i64) -> Result<usize, Closed> {
        Ok(self.keepalive(epoch, now)? + self.registrations() + self.events()?)
    }

    /// Drops the mappings the subscriptions retired and forgets a registration whose
    /// subscription is gone, a replay whose end is stored or whose handle is gone, and a
    /// recording whose stop is stored or whose handle is gone, then admits the
    /// subscriptions, replays and recordings the user's thread added: the sweeps first,
    /// so that one dropped before another was added leaves its place to it.
    fn registrations(&mut self) -> usize {
        self.registry.sweep() + self.replays.sweep() + self.recordings.sweep() + self.admit()
    }

    /// Admits every registration queued, each through [`Registry::admit`], every replay
    /// registration, each through [`ReplayRegistry::admit`], and every recording
    /// registration, each through [`RecordingRegistry::admit`].
    fn admit(&mut self) -> usize {
        let mut work = 0;
        while let Some(registration) = self.registrations.poll() {
            self.registry.admit(registration);
            work += 1;
        }
        while let Some(registration) = self.replay_registrations.poll() {
            self.replays.admit(registration);
            work += 1;
        }
        while let Some(registration) = self.recording_registrations.poll() {
            self.recordings.admit(registration);
            work += 1;
        }
        work
    }

    /// The consumer heartbeat: −1 is `driver-closed`; older than the driver timeout on
    /// the client's epoch clock, `epoch`, is `driver-timeout`.
    fn liveness(&self, epoch: i64) -> Option<Closed> {
        let heartbeat = self.ring.consumer_heartbeat();
        if heartbeat == DRIVER_CLOSED {
            Some(Closed::DriverClosed)
        } else if epoch.saturating_sub(heartbeat) > nanos(DRIVER_TIMEOUT) {
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
    /// The interval is measured on the nano clock, `now`, so that a clock set back does
    /// not silence the client for the length of the step; the claim checks the
    /// heartbeat's age against the epoch clock, `epoch`.
    fn keepalive(&mut self, epoch: i64, now: i64) -> Result<usize, Closed> {
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
        match self.producer.write(
            command.command_type() as i32,
            &self.scratch[..length],
            epoch,
        ) {
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
    /// that command's event, goes to the subscription, replay or recording it names when
    /// it is theirs, an armed recording's second answer among them, and is dropped
    /// otherwise; a lap or a corrupt buffer closes the client too.
    ///
    /// The pending correlation id is loaded inside the handler, after the event is read,
    /// never before the receive. The user's thread stores it (release) before it writes
    /// the command; the driver reads the command from the ring, then writes its answer to
    /// the broadcast buffer, which this receive reads (acquire). So a load after the read
    /// sees the id of every command whose answer it is reading; a load before the
    /// receive could see [`NO_PENDING`] and drop the answer: a false `response-timeout`,
    /// with a registration left on the driver.
    ///
    /// The same chain orders the registration queues: `add subscription` and `start
    /// recording` queue their registration before they write the command, so admitting
    /// the queues before an answer or an image is handled finds every registration the
    /// driver could have answered: an image that follows its answer in one batch finds
    /// its subscription, and the `recording ready` that binds a recording to its id, or
    /// the `operation success` that marks it armed, finds its registration.
    fn events(&mut self) -> Result<usize, Closed> {
        let mut received = 0;
        let client_id = self.client_id;
        while received < 16 {
            let shared = &self.shared;
            let recordings = &self.recordings;
            let registrations = &mut self.registrations;
            let registry = &mut self.registry;
            let overdue = &mut self.overdue;
            let counters = &self.counters;
            let mut outcome = Ok(Outcome::Nothing);
            let got = self.receiver.receive(|type_id, message| {
                // The answer to the conductor's own `add counter`, which is nobody's
                // pending command and which `event` passes over (ADR 0177).
                own_answer(overdue, counters, shared, type_id, message);
                // The driver's answer to a `remove subscription`, which is nobody's
                // pending command either: its registration can be forgotten, since
                // the driver announces nothing more for it (ADR 0179).
                if registry.removing() {
                    match Event::parse(type_id, message) {
                        Ok(Event::OperationSuccess { correlation_id }) => {
                            registry.removed(correlation_id);
                        }
                        Ok(Event::Error {
                            offending_correlation_id,
                            ..
                        }) => {
                            registry.removed(offending_correlation_id);
                        }
                        _ => {}
                    }
                }
                outcome = event(
                    client_id,
                    shared.pending(),
                    &|correlation_id| recordings.awaits(correlation_id),
                    // A subscription is registered before its command goes, so the
                    // registrations queued now are admitted before the registry is
                    // asked, and an image of this client's finds its subscription.
                    &mut |registration_id| {
                        while let Some(registration) = registrations.poll() {
                            registry.admit(registration);
                        }
                        registry.holds(registration_id)
                    },
                    type_id,
                    message,
                );
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
                            // The recording's id, before its answer goes: the stop that
                            // follows names the id, and the registration was queued under
                            // the command's correlation id.
                            if let Response::RecordingReady {
                                correlation_id,
                                recording_id,
                                ..
                            } = response
                            {
                                self.recordings.bind(correlation_id, recording_id);
                            }
                            // `operation success` to a command that registered for a
                            // second answer: the driver armed its recording, and the
                            // event that names this correlation id next is that answer,
                            // whatever command the user's thread waits on by then.
                            if let Response::Success { correlation_id } = response {
                                self.recordings.armed(correlation_id);
                            }
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
                            log_name,
                            source_identity,
                        } => self.image_available(
                            correlation_id,
                            session_id,
                            subscriber_registration_id,
                            position_counter_id,
                            log_name,
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
                            if let Some(end) = self.replays.end(replay_session_id) {
                                end.set(position, reason);
                            }
                        }
                        Outcome::RecordingStopped {
                            recording_id,
                            stop_position,
                        } => {
                            if let Some(stop) = self.recordings.stop(recording_id) {
                                stop.set(stop_position);
                            }
                        }
                        Outcome::ArmedAnswer(response) => self.recordings.answer(response),
                    }
                }
                Ok(false) => break,
                Err(BroadcastError::Lapped { .. }) => return Err(Closed::Lapped),
                Err(_) => return Err(Closed::Corrupt),
            }
        }
        Ok(received)
    }

    /// `available image`: hands the subscription what [`announced`] makes of the event,
    /// the image's log and position counter or the word that the image is gone. An
    /// image for a subscription this client no longer holds, or whose handle is gone
    /// and only waits to be swept, is dropped, as the driver will withdraw it on the
    /// remove: nothing would ever read it. A subscription whose queue is full has not
    /// polled while the driver announced a queue's worth of images: `overrun`.
    fn image_available(
        &mut self,
        correlation_id: i64,
        session_id: i32,
        subscriber_registration_id: i64,
        position_counter_id: i32,
        log_name: (LogDirectory, i64),
        source_identity: String,
    ) -> Result<(), Closed> {
        let Some(registration) = self.registry.get(subscriber_registration_id) else {
            return Ok(());
        };
        if registration.images.peer_dropped() {
            // The subscription's handle is gone and the driver has yet to answer its
            // removal: the record this event names is this client's all the same, and
            // nothing will poll it. Its release word is stored here, by its id.
            if let Ok(record) = u32::try_from(position_counter_id) {
                release_unheld(&self.counters, record, correlation_id, session_id);
            }
            return Ok(());
        }
        let event = announced(
            &self.dir,
            (&self.counters, &self.shared),
            correlation_id,
            session_id,
            position_counter_id,
            log_name,
            source_identity,
        )?;
        registration
            .images
            .offer(event)
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

/// What an `available image` is to its subscription: the image's log, opened
/// read-only, with the owner's handle on the position counter the driver allocated,
/// or the word that the image is gone.
///
/// The driver keeps the log for the counter reuse timeout after the event, unless
/// the subscription is removed first (`spec/client-protocol.md`, "Events"; ADR 0078).
/// The counter's record it gives to no other owner before this client has let it go,
/// however late this conductor reads the event (`spec/client-protocol.md`, "The
/// records a client writes"; ADR 0179): no time is in that. So a log that is not
/// there, or a record that holds no counter, is an image that is `gone`: it ended, or
/// this client's own `remove subscription` was on its way, before this conductor read
/// the event. The image is not added, the conductor stores the record's release word
/// by its id, the subscription's next poll reports the fault, and the client stays
/// open. A record that holds another image's counter is treated the same and is
/// stored into by nobody; the rule above says it is never met. A log that is there
/// and fails a check, or a record id no counters file has, is the driver saying one
/// thing and the directory another: corrupt. The client closes, and the record is
/// held until the driver has the client's lock.
fn announced(
    dir: &Path,
    owner: (&CountersWriter, &Arc<Shared>),
    correlation_id: i64,
    session_id: i32,
    position_counter_id: i32,
    log_name: (LogDirectory, i64),
    source_identity: String,
) -> Result<ImageEvent, Closed> {
    let (counters, shared) = owner;
    let (directory, log_id) = log_name;
    let log = match LogBuffers::open_named(dir, directory, log_id, false) {
        Ok(log) => Some(log),
        Err(LogError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return Err(Closed::Corrupt),
    };
    let record = u32::try_from(position_counter_id).map_err(|_| Closed::Corrupt)?;
    // The record is this image's while its key begins with the image's correlation id
    // and session id (`spec/log-buffer.md`, "Counters"): freed, it holds no counter,
    // and allocated again to another image it holds that image's key. The key is the
    // image's and not the subscription's, so a record allocated again to another
    // subscription on this same image is not told apart here. The driver frees a
    // record while its image lives only when the record's subscription is removed, so
    // the event then names a subscription this client has removed, whose handle is
    // gone or on its way out: `image_available` drops the event for one that is gone,
    // and nothing polls, or stores a position through, one on its way out.
    let named = match counters.record(record) {
        Ok(Some(fields)) => {
            let key = fields.key();
            fields.type_id() == SUBSCRIPTION_POSITION.0
                && key[..8] == correlation_id.to_le_bytes()
                && key[8..12] == session_id.to_le_bytes()
        }
        Ok(None) => false,
        Err(_) => return Err(Closed::Corrupt),
    };
    let position = if named {
        counters
            .counter(record)
            .ok()
            .map(|cell| Counter::new(cell, Arc::clone(shared)))
    } else {
        None
    };
    Ok(match (log, position) {
        (Some(log), Some(position)) => ImageEvent::Available {
            correlation_id,
            session_id,
            log,
            position,
            source_identity,
        },
        // The image is not added. The record the event named is this client's from the
        // event on, and the driver gives it to nobody until its release word is
        // stored: by the handle's drop when one was taken, and by the record's id
        // when none was (`spec/client-protocol.md`, "The records a client writes").
        (_, Some(position)) => {
            drop(position);
            ImageEvent::Gone { session_id }
        }
        (_, None) => {
            release_unheld(counters, record, correlation_id, session_id);
            ImageEvent::Gone { session_id }
        }
    })
}

/// Stores, by its id, the release word of a record this client was told of in an
/// `available image` and holds no handle on (`spec/client-protocol.md`, "The records a
/// client writes"). The record is the client's from the event on, and the driver gives
/// it to nobody before the word is stored, so the record either holds no counter, the
/// driver having freed it and holding it, or is still the image's own. A record that
/// holds another counter is neither: something broke the rule, and nothing is stored
/// into it.
fn release_unheld(counters: &CountersWriter, record: u32, correlation_id: i64, session_id: i32) {
    let ours = match counters.record(record) {
        Ok(None) => true,
        Ok(Some(fields)) => {
            let key = fields.key();
            fields.type_id() == SUBSCRIPTION_POSITION.0
                && key[..8] == correlation_id.to_le_bytes()
                && key[8..12] == session_id.to_le_bytes()
        }
        Err(_) => false,
    };
    if ours {
        let _ = counters.release(record);
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
            // A registration whose subscription sent `remove subscription` stays until
            // the driver has answered it (`removed`): until then the driver may
            // announce an image for it, whose record `image_available` releases. So
            // does one whose removal could not be written, which the driver keeps
            // until the client closes. One whose halves were dropped with no removal,
            // an `add subscription` the driver refused, has nothing to wait for.
            if self.subscriptions[index].images.peer_dropped()
                && self.subscriptions[index].removal.load(Ordering::Acquire)
                    == crate::subscription::NOT_REMOVED
            {
                self.subscriptions.swap_remove(index);
                work += 1;
            } else {
                index += 1;
            }
        }
        work
    }

    /// Whether the subscription with `registration_id` is registered here, which is
    /// what makes an `available image` this client's.
    pub(crate) fn holds(&self, registration_id: i64) -> bool {
        self.subscriptions
            .iter()
            .any(|registration| registration.registration_id == registration_id)
    }

    /// Forgets the registration whose `remove subscription` had correlation id
    /// `correlation_id`, which the driver has now answered: it announces nothing more
    /// for that subscription. Returns whether one was forgotten.
    pub(crate) fn removed(&mut self, correlation_id: i64) -> bool {
        // A correlation id is 0 or more. The values below it are this field's own, for
        // a subscription that has sent no removal and for one that could not: an
        // event that carried one of them, as an error the driver could not place may,
        // must forget neither.
        if correlation_id < 0 {
            return false;
        }
        let found = self.subscriptions.iter().position(|registration| {
            registration.removal.load(Ordering::Acquire) == correlation_id
        });
        match found {
            Some(index) => {
                self.subscriptions.swap_remove(index);
                true
            }
            None => false,
        }
    }

    /// Whether a registration waits for the answer to its removal.
    pub(crate) fn removing(&self) -> bool {
        self.subscriptions
            .iter()
            .any(|registration| registration.removal.load(Ordering::Acquire) >= 0)
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

/// The replays one client follows for their `replay stopped`, by replay session id:
/// bounded, and never full in silence.
///
/// A registration is forgotten once its end is stored or its handle is gone, so the
/// list holds replays live at the driver, or whose `replay stopped` this conductor has
/// not read yet, each with a handle waiting for the end. The driver accepts `start
/// replay` only with a free place among its [`MAX_REPLAYS`], and it sends `replay
/// stopped` as it frees a place, so by the time this conductor reads a `replay ready`
/// it has read the `replay stopped` of every replay that made room for it, and at
/// most [`MAX_REPLAYS`] of its registrations have no end stored. The capacity is one
/// more than that: the registration of the command in flight is queued before the
/// command is sent and may be admitted before that `replay stopped` is read, and one
/// command is in flight per client. Full even so, [`ReplayRegistry::admit`] refuses.
pub(crate) struct ReplayRegistry {
    replays: Vec<ReplayRegistration>,
    capacity: usize,
}

impl ReplayRegistry {
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            replays: Vec::with_capacity(capacity),
            capacity,
        }
    }

    /// Admits `registration`, sweeping first when the registry is full. Still full,
    /// the registration's end is marked refused and the registration dropped here, and
    /// the call returns `false`: `start_replay`, which looks after the answer, finds
    /// the mark, stops the replay and returns [`crate::ClientError::ReplaysFull`]. A
    /// replay is refused, never left with a handle that never learns its end.
    pub(crate) fn admit(&mut self, registration: ReplayRegistration) -> bool {
        if self.replays.len() >= self.capacity {
            self.sweep();
        }
        if self.replays.len() >= self.capacity {
            registration.end.refuse();
            drop(registration);
            return false;
        }
        self.replays.push(registration);
        true
    }

    /// Forgets a registration whose end is stored, which its handle has read or will
    /// read from the shared end, or whose handle is gone, which the `Arc` count says.
    /// Returns how many were forgotten.
    pub(crate) fn sweep(&mut self) -> usize {
        let before = self.replays.len();
        self.replays
            .retain(|replay| Arc::strong_count(&replay.end) > 1 && !replay.end.is_set());
        before - self.replays.len()
    }

    /// The end shared with the handle of replay `replay_session_id`.
    pub(crate) fn end(&self, replay_session_id: i64) -> Option<&ReplayEnd> {
        self.replays
            .iter()
            .find(|replay| replay.replay_session_id == replay_session_id)
            .map(|replay| &*replay.end)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.replays.len()
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
        log_name: (LogDirectory, i64),
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
    /// `recording stopped`, for a recording this client may hold a handle on.
    RecordingStopped {
        recording_id: i64,
        stop_position: i64,
    },
    /// The second answer to an armed `start recording` of this client: `recording
    /// ready` or `error` under the arming command's correlation id.
    ArmedAnswer(Response),
}

/// The recordings whose stop this client follows: one registration per `start
/// recording` whose handle is alive and whose `recording stopped` has not been read,
/// bounded, and never full in silence.
///
/// A registration is queued under its command's correlation id, learns its recording id
/// from `recording ready`, and is forgotten once its stop is stored or its handle is
/// gone, a refused command's among them. The driver accepts `start recording` only with
/// a free place among its [`MAX_RECORDINGS`] and sends `recording stopped` as it frees
/// one, so by the time this conductor reads a `recording ready` it has read the stop of
/// every recording that made room for it, and at most [`MAX_RECORDINGS`] of its
/// registrations follow a recording with no stop stored. A registration whose command
/// the driver answered by arming the recording waits for the second answer, and the
/// driver holds at most [`MAX_SUBSCRIPTIONS`] armed. The capacity is the two together
/// and one more, for the command in flight ([`RECORDING_REGISTRY`]). Full even so,
/// [`RecordingRegistry::admit`] refuses.
pub(crate) struct RecordingRegistry {
    recordings: Vec<FollowedRecording>,
    capacity: usize,
}

/// One registration: the command's correlation id, the recording id once `recording
/// ready` gave it, −1 until then, the stop shared with the handle, and, for a command
/// that may be answered twice, where its second answer goes and whether the first,
/// `operation success`, has been read.
struct FollowedRecording {
    correlation_id: i64,
    recording_id: i64,
    stop: Arc<RecordingStop>,
    armed: Option<spsc::Producer<Response>>,
    armed_answered: bool,
}

impl RecordingRegistry {
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            recordings: Vec::with_capacity(capacity),
            capacity,
        }
    }

    /// Admits `registration`, sweeping first when the registry is full. Still full, the
    /// registration's stop is marked refused and the registration dropped here, and the
    /// call returns `false`: `start_recording`, which looks after the answer, finds the
    /// mark and returns an error, never a handle that would never learn its stop.
    pub(crate) fn admit(&mut self, registration: RecordingRegistration) -> bool {
        if self.recordings.len() >= self.capacity {
            self.sweep();
        }
        if self.recordings.len() >= self.capacity {
            registration.stop.refuse();
            return false;
        }
        self.recordings.push(FollowedRecording {
            correlation_id: registration.correlation_id,
            recording_id: -1,
            stop: registration.stop,
            armed: registration.armed,
            armed_answered: false,
        });
        true
    }

    /// `operation success` for the command `correlation_id`: when its registration
    /// holds a place for a second answer, the driver armed the recording, and the next
    /// event that names the correlation id is that answer.
    pub(crate) fn armed(&mut self, correlation_id: i64) {
        if let Some(recording) = self.recordings.iter_mut().find(|recording| {
            recording.correlation_id == correlation_id && recording.armed.is_some()
        }) {
            recording.armed_answered = true;
        }
    }

    /// Whether an event that names `correlation_id` is the second answer to an armed
    /// `start recording` of this client: its registration read `operation success` and
    /// still waits. Correlation ids come from the driver's one counter, so no other
    /// client's event names one of this client's.
    pub(crate) fn awaits(&self, correlation_id: i64) -> bool {
        self.recordings.iter().any(|recording| {
            recording.correlation_id == correlation_id
                && recording.armed_answered
                && recording.armed.is_some()
        })
    }

    /// The second answer to an armed `start recording`: handed to the handle through
    /// its queue of one, which holds nothing before it. `recording ready` binds the
    /// registration to its recording, whose stop it follows from here; `error`, the
    /// armed recording dropped or its start failed, ends the registration.
    pub(crate) fn answer(&mut self, response: Response) {
        let correlation_id = response.correlation_id();
        let Some(index) = self
            .recordings
            .iter()
            .position(|recording| recording.correlation_id == correlation_id)
        else {
            return;
        };
        let Some(mut answers) = self.recordings[index].armed.take() else {
            return;
        };
        if let Response::RecordingReady { recording_id, .. } = response {
            self.recordings[index].recording_id = recording_id;
        } else {
            self.recordings.swap_remove(index);
        }
        // One answer into a queue of one that nothing else is offered to.
        let _ = answers.offer(response);
    }

    /// Forgets a registration whose stop is stored, which its handle has read or will
    /// read from the shared stop, or whose handle is gone, which the `Arc` count says.
    /// Returns how many were forgotten.
    pub(crate) fn sweep(&mut self) -> usize {
        let before = self.recordings.len();
        self.recordings.retain(|recording| {
            Arc::strong_count(&recording.stop) > 1 && recording.stop.get().is_none()
        });
        before - self.recordings.len()
    }

    /// `recording ready` for the command `correlation_id`: its registration follows
    /// recording `recording_id` from here.
    pub(crate) fn bind(&mut self, correlation_id: i64, recording_id: i64) {
        if let Some(recording) = self
            .recordings
            .iter_mut()
            .find(|recording| recording.correlation_id == correlation_id)
        {
            recording.recording_id = recording_id;
        }
    }

    /// The stop shared with the handle of recording `recording_id`, when this client
    /// follows it.
    pub(crate) fn stop(&self, recording_id: i64) -> Option<&RecordingStop> {
        self.recordings
            .iter()
            .find(|recording| recording_id >= 0 && recording.recording_id == recording_id)
            .map(|recording| &*recording.stop)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.recordings.len()
    }
}

/// The kind and registration id a log file name carries: `publications/<id>.logbuffer`
/// or `images/<id>.logbuffer`, as `spec/client-protocol.md`, "The driver directory"
/// names them; anything else is a corrupt event (ADR 0010).
fn log_name(log_file_name: &[u8]) -> Option<(LogDirectory, i64)> {
    cavalry_shm::log::layout::parse_file_name(log_file_name)
}

/// What one event means to client `client_id` while it waits on the command with
/// correlation id `pending` ([`NO_PENDING`] for none), and while `awaits` accepts the
/// correlation id of each armed `start recording` of this client whose second answer
/// has not been read ([`RecordingRegistry::awaits`]): `client timeout` with the client's
/// id closes it with [`Closed::ClientTimeout`]; an event that fails a decoding check,
/// `too-short`, `type`, `string` or `length`, closes it with [`Closed::Corrupt`], since
/// an event it cannot decode may be the one it needed (`spec/client-protocol.md`, "What
/// the driver rejects"); `available image` and `unavailable image` are for the
/// subscription they name, whatever is pending, and an `available image` is this
/// client's only when `subscribes` accepts the subscriber registration id it names
/// ([`Registry::holds`], asked once the queued registrations are admitted): the driver
/// announces every image to every client, and one that is another client's is nothing
/// to this one, with nothing copied from it, so that a conductor allocates nothing for
/// its neighbours' images (card 253); `replay stopped` is for the replay
/// handle it names, and `recording stopped` is for the recording handle it names; a
/// `recording ready` or an `error` that names a correlation id `awaits` accepts is the
/// second answer to an armed `start recording`, for that recording's handle, and is
/// checked before the pending command's answer, since the image may arrive while the
/// user's thread still names the arming command's id as pending; `publication ready`,
/// `exclusive publication ready`, `subscription ready`, `operation success`, `counter
/// ready`, `recording ready`, `replay ready` and `error` with the pending correlation id
/// are the answer; every other event leaves the client open and is nothing to it. An
/// `available image` whose log file name is not the page's shape is `corrupt`.
fn event(
    client_id: i64,
    pending: i64,
    awaits: &dyn Fn(i64) -> bool,
    subscribes: &mut dyn FnMut(i64) -> bool,
    type_id: i32,
    message: &[u8],
) -> Result<Outcome, Closed> {
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
            let log_name = log_name(log_file_name).ok_or(Closed::Corrupt)?;
            // The driver announces an image to every client. Nothing is copied from
            // one that is another client's: its source identity, `cavalry:ipc` or the
            // sender's address, is made a `String` for this client's subscription
            // alone, so a client's conductor allocates nothing for its neighbours.
            if !subscribes(subscriber_registration_id) {
                return Ok(Outcome::Nothing);
            }
            return Ok(Outcome::ImageAvailable {
                correlation_id,
                session_id,
                subscriber_registration_id,
                position_counter_id: subscriber_position_counter_id,
                log_name,
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
        Event::RecordingStopped {
            recording_id,
            stop_position,
        } => {
            return Ok(Outcome::RecordingStopped {
                recording_id,
                stop_position,
            });
        }
        // An armed recording's second answer, looked for before the pending command's:
        // the image may arrive while the user's thread has not yet cleared the arming
        // command's correlation id, and the answer is the handle's, not the command's.
        Event::RecordingReady {
            correlation_id,
            recording_id,
            session_id,
            recording_position_counter_id,
            recording_synced_position_counter_id,
        } if awaits(correlation_id) => {
            return Ok(Outcome::ArmedAnswer(Response::RecordingReady {
                correlation_id,
                recording_id,
                session_id,
                position_counter_id: recording_position_counter_id,
                synced_position_counter_id: recording_synced_position_counter_id,
            }));
        }
        Event::Error {
            offending_correlation_id,
            error_code,
            error_message,
        } if awaits(offending_correlation_id) => {
            return Ok(Outcome::ArmedAnswer(Response::Error {
                correlation_id: offending_correlation_id,
                code: error_code,
                message: String::from_utf8_lossy(error_message).into_owned(),
            }));
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
            ..
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
            ..
        } if correlation_id == pending => Outcome::Answer(Response::PublicationReady {
            correlation_id,
            registration_id,
            session_id,
            stream_id,
            limit_counter_id: publication_limit_counter_id,
            log_file_name: String::from_utf8_lossy(log_file_name).into_owned(),
            exclusive: true,
        }),
        Event::SubscriptionReady { correlation_id, .. } if correlation_id == pending => {
            Outcome::Answer(Response::SubscriptionReady { correlation_id })
        }
        Event::OperationSuccess { correlation_id } if correlation_id == pending => {
            Outcome::Answer(Response::Success { correlation_id })
        }
        Event::CounterReady {
            correlation_id,
            counter_id,
        } if correlation_id == pending => Outcome::Answer(Response::CounterReady {
            correlation_id,
            counter_id,
        }),
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
    use std::sync::Arc;

    use cavalry_shm::spsc;

    use super::{
        CountersWriter, Event, LogDirectory, MAX_REPLAYS, MAX_SUBSCRIPTIONS, NO_PENDING, Outcome,
        RECORDING_REGISTRY, RecordingRegistration, RecordingRegistry, Registry, ReplayRegistration,
        ReplayRegistry, SUBSCRIPTION_POSITION, announced, event, log_name,
    };
    use crate::error::Closed;
    use crate::recording::RecordingStop;
    use crate::replay::ReplayEnd;
    use crate::response::Response;
    use crate::subscription::{ImageEvent, Registration, Retired};

    const HOUR: std::time::Duration = std::time::Duration::from_secs(3600);

    /// A driver on a directory of its own for a test of the client's watch.
    fn watch_test_driver(test: &str) -> (std::path::PathBuf, cavalry_driver::Driver) {
        let dir = std::env::temp_dir().join(format!(
            "cavalry-client-conductor-{test}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let driver = cavalry_driver::Driver::start(&dir, dir.join("log")).expect("the driver");
        (dir, driver)
    }

    /// The value of the counter `client.conductor_parks_overdue` that client
    /// `client_id` added in the driver's counters file, when it has added one.
    fn conductor_parks_overdue(dir: &std::path::Path, client_id: i64) -> Option<i64> {
        let counters = cavalry_shm::counters::CountersReader::open(dir).expect("the counters");
        (0..counters.capacity()).find_map(|id| match counters.record(id) {
            Ok(Some(fields))
                if fields.type_id()
                    == cavalry_shm::counters::generated::CLIENT_CONDUCTOR_PARKS_OVERDUE.0
                    && fields.key()[..8] == client_id.to_le_bytes() =>
            {
                Some(counters.read(id).expect("its value"))
            }
            _ => None,
        })
    }

    /// Waits for `what`, or panics with `failure` after 20 s.
    fn wait_within<T>(failure: &str, mut what: impl FnMut() -> Option<T>) -> T {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            if let Some(found) = what() {
                return found;
            }
            assert!(std::time::Instant::now() < deadline, "{failure}");
            std::thread::park_timeout(std::time::Duration::from_millis(10));
        }
    }

    /// A client whose conductor is slow must not store an ended image's position into
    /// the record of the image that came next (work item 506; invariant 1). The driver
    /// frees an image's position record when it sends `unavailable image`, and gives
    /// the record out again after the counter reuse timeout, 1 s. The subscriber's
    /// thread stores its position after each poll until its conductor has handed it
    /// the event. Here the conductor is held across one publication's end and the next
    /// one's start, for longer than that timeout, while the subscriber's thread polls
    /// as an application does. The next image's position must stay where that image
    /// joined, 0, until the subscriber reads it, and its publisher must be held at its
    /// window: a position ahead of the reader lets the publisher write over what the
    /// reader has not read.
    #[test]
    fn a_slow_conductor_does_not_store_an_ended_images_position_into_the_next_images_record() {
        use cavalry_shm::counters::generated::{PUBLICATION_LIMIT, SUBSCRIPTION_POSITION};
        const STREAM: i32 = 5062;
        const CHANNEL: &str = "cavalry:ipc";
        // Four terms of the driver's 64 MiB, in messages of 1,024 bytes with their
        // headers: an end position that is two terms and more past a new image's 0.
        const FIRST: usize = 4 * 64 * 1024;
        let (dir, driver) = watch_test_driver("stray-store-next-image");
        let counters = cavalry_shm::counters::CountersReader::open(&dir).expect("the counters");
        let records_of = |type_id: i32| -> Vec<u32> {
            (0..counters.capacity())
                .filter(|id| {
                    counters
                        .record(*id)
                        .ok()
                        .flatten()
                        .is_some_and(|fields| fields.type_id() == type_id)
                })
                .collect()
        };
        let offer_all = |publication: &mut crate::Publication, count: usize| -> usize {
            let mut offered = 0;
            for _ in 0..count {
                let deadline = std::time::Instant::now() + std::time::Duration::from_millis(200);
                loop {
                    match publication.offer(&[7u8; 992]) {
                        Ok(_) => {
                            offered += 1;
                            break;
                        }
                        Err(crate::Offer::AdminAction) => {}
                        // Held at its limit for 200 ms with nobody reading: that is
                        // as far as the publisher is let write.
                        Err(crate::Offer::BackPressured) => {
                            if std::time::Instant::now() >= deadline {
                                return offered;
                            }
                            std::hint::spin_loop();
                        }
                        Err(other) => panic!("{other}"),
                    }
                }
            }
            offered
        };

        super::WATCH_FOR_TEST.set(Some(None));
        let mut publisher = crate::Cavalry::connect_to(&dir).expect("the publisher connects");
        let hold = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        super::HOLD_FOR_TEST.set(Some(std::sync::Arc::clone(&hold)));
        let mut subscriber = crate::Cavalry::connect_to(&dir).expect("the subscriber connects");
        super::HOLD_FOR_TEST.set(None);
        super::WATCH_FOR_TEST.set(None);
        let mut subscription = subscriber
            .add_subscription("cavalry:ipc", STREAM)
            .expect("the subscription");

        // The first publication, all read.
        let mut first = publisher
            .add_publication(CHANNEL, STREAM)
            .expect("the first");
        let mut read = 0usize;
        let mut offered = 0usize;
        while offered < FIRST {
            offered += offer_all(&mut first, 64);
            read += subscription.poll(|_, _| {}, 256).expect("polls");
        }
        wait_within("the subscriber did not read the first publication", || {
            read += subscription.poll(|_, _| {}, 256).expect("polls");
            (read == FIRST).then_some(())
        });
        let record = *records_of(SUBSCRIPTION_POSITION.0)
            .first()
            .expect("the image's position");
        let end = counters.read(record).expect("the position");
        assert_eq!(
            usize::try_from(end),
            Ok(FIRST * 1024),
            "the first image's position at its end"
        );

        // The conductor is held; the first publication ends; the driver frees the
        // record; the subscriber's thread polls on, as it would.
        hold.store(true, std::sync::atomic::Ordering::Release);
        first.close().expect("the first publication closes");
        wait_within("the driver did not free the first image's position", || {
            subscription.poll(|_, _| {}, 16).expect("polls");
            counters
                .record(record)
                .ok()
                .flatten()
                .is_none()
                .then_some(())
        });
        let freed = std::time::Instant::now();
        while freed.elapsed()
            < cavalry_driver::spec::COUNTER_REUSE_TIMEOUT + std::time::Duration::from_millis(200)
        {
            subscription.poll(|_, _| {}, 16).expect("polls");
        }

        // The next publication on the stream. The record that was freed is held for the
        // subscriber, whose thread has not said that it is done with it: the next
        // image's position is in another record, and the held one is no counter to a
        // reader and is counted in `counters.held`.
        let held = || {
            counters
                .read(cavalry_shm::counters::generated::COUNTERS_HELD.record())
                .expect("counters.held")
        };
        assert_eq!(
            held(),
            1,
            "the ended image's position is held for its writer"
        );
        let mut second = publisher
            .add_publication(CHANNEL, STREAM)
            .expect("the second");
        let next = wait_within("the next image's position was not allocated", || {
            let positions = records_of(SUBSCRIPTION_POSITION.0);
            (positions.len() == 1).then(|| positions[0])
        });
        assert_ne!(
            next, record,
            "the record the subscriber's thread still stores into went to the next image"
        );
        let record = next;
        let limit = *records_of(PUBLICATION_LIMIT.0).first().expect("its limit");
        // The subscriber's thread polls its old image on: its conductor is held, so it
        // has not been told that the image ended.
        let mut seen = vec![counters.read(record).expect("the position")];
        let polled = std::time::Instant::now();
        while polled.elapsed() < std::time::Duration::from_millis(300) {
            subscription.poll(|_, _| {}, 16).expect("polls");
            let value = counters.read(record).expect("the position");
            if seen.last() != Some(&value) {
                seen.push(value);
            }
        }
        let window_limit = counters.read(limit).expect("the limit");
        // What the publisher is let write while the subscriber has read nothing of the
        // new image.
        let written = offer_all(&mut second, 2 * FIRST);

        // The conductor goes on: the subscriber takes the new image and reads it.
        hold.store(false, std::sync::atomic::Ordering::Release);
        let mut delivered = 0usize;
        let mut fault = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while delivered < written && fault.is_none() && std::time::Instant::now() < deadline {
            match subscription.poll(|_, _| {}, 256) {
                Ok(count) => delivered += count,
                Err(error) => fault = Some(format!("{error:?}")),
            }
        }
        let report = format!(
            "the next image's position record {record} read {seen:?} while the subscriber had \
             read nothing of it (the first image ended at {end}); its publisher's limit was \
             {window_limit} and it wrote {written} messages of 1,024 bytes into terms of 64 MiB; \
             the subscriber then read {delivered} of them, fault {fault:?}"
        );
        assert_eq!(seen, vec![0], "{report}");
        assert_eq!(
            window_limit,
            i64::from(cavalry_driver::spec::IPC_PUBLICATION_WINDOW),
            "the publisher is held at its window while nothing is read: {report}"
        );
        assert_eq!(written * 1024, 32 * 1024 * 1024, "{report}");
        assert_eq!(delivered, written, "{report}");
        assert!(fault.is_none(), "{report}");
        // The subscriber's thread has polled since its conductor went on: it took the
        // ended image out and stored its release word, and the driver let the record go.
        wait_within("the ended image's record was not let go", || {
            subscription.poll(|_, _| {}, 16).expect("polls");
            (held() == 0).then_some(())
        });

        drop(second);
        drop(subscription);
        subscriber.close().expect("the subscriber closes");
        publisher.close().expect("the publisher closes");
        driver.close().expect("the driver closes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A client's watch ends a park of its conductor that outlived its timeout, and
    /// the client counts it where `doctor` reads it (ADR 0177). The conductor's first
    /// park lasts an hour here, which stands for the park whose timeout never comes.
    /// Until it is ended the client has no counter for it. The watch unparks the
    /// conductor, which adds the client's counter and stores 1 in it: only a conductor
    /// that went on can do either. The count stays 1, and the client takes a command
    /// as before.
    #[test]
    fn the_watch_ends_the_conductors_park_and_the_conductor_counts_it_in_its_own_counter() {
        let (dir, driver) = watch_test_driver("watch-ends-conductor");
        super::WATCH_FOR_TEST.set(Some(Some(None)));
        super::PARK_FOR_TEST.set(Some(HOUR));
        super::PARK_FOR_TEST_ONCE.set(true);
        let mut client = crate::Cavalry::connect_to(&dir).expect("connects");
        super::WATCH_FOR_TEST.set(None);
        super::PARK_FOR_TEST.set(None);
        super::PARK_FOR_TEST_ONCE.set(false);
        let client_id = client.client_id();
        assert_eq!(
            conductor_parks_overdue(&dir, client_id),
            None,
            "no counter before a park was overdue"
        );
        wait_within(
            "the watch did not end the conductor's park, or the conductor did not count it",
            || (conductor_parks_overdue(&dir, client_id) == Some(1)).then_some(()),
        );
        // The conductor parks as it should from here on, so the count stands: a count
        // taken at every look, or a conductor still held, would move it.
        std::thread::park_timeout(std::time::Duration::from_millis(500));
        assert_eq!(
            conductor_parks_overdue(&dir, client_id),
            Some(1),
            "one park, counted once"
        );
        let publication = client
            .add_publication("cavalry:ipc", 2413)
            .expect("the client takes a command after its conductor was woken");
        drop(publication);
        client.close().expect("closes");
        // The driver frees it when it reads the client's close, a duty cycle later.
        wait_within(
            "the driver did not free the client's counter at its close",
            || {
                conductor_parks_overdue(&dir, client_id)
                    .is_none()
                    .then_some(())
            },
        );
        driver.close().expect("the driver closes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A client's conductor ends the park of its watch when that outlived its timeout,
    /// and counts it once (ADR 0177). The watch's first park lasts an hour here, and
    /// the conductor is not held: a watch that is parked counts nothing, so the count
    /// is the conductor's.
    #[test]
    fn the_conductor_ends_its_watchs_park_and_counts_it_once() {
        let (dir, driver) = watch_test_driver("conductor-ends-watch");
        super::WATCH_FOR_TEST.set(Some(Some(Some(HOUR))));
        let client = crate::Cavalry::connect_to(&dir).expect("connects");
        super::WATCH_FOR_TEST.set(None);
        let client_id = client.client_id();
        wait_within("the conductor did not end its watch's park", || {
            (conductor_parks_overdue(&dir, client_id) == Some(1)).then_some(())
        });
        std::thread::park_timeout(std::time::Duration::from_millis(500));
        assert_eq!(conductor_parks_overdue(&dir, client_id), Some(1));
        client.close().expect("closes");
        driver.close().expect("the driver closes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A driver with no counter left refuses the conductor's own (ADR 0177). The park
    /// was ended all the same, and the conductor asks once: it does not ask again, and
    /// it does not spin or wait on the refusal. A second client first adds counters
    /// until the driver refuses one. Then a client whose conductor's first park lasts
    /// an hour connects; its watch ends the park, and its conductor's `add counter` is
    /// refused. The driver's two counters of a refused add move by one between them
    /// and stay there, the client has no counter, and it takes a command as before.
    #[test]
    fn a_driver_with_no_counter_left_refuses_the_conductors_own_and_is_asked_once() {
        let (dir, driver) = watch_test_driver("watch-no-counter-left");
        let refusals = |dir: &std::path::Path| {
            let counters = cavalry_shm::counters::CountersReader::open(dir).expect("counters");
            [
                cavalry_shm::counters::generated::DRIVER_REGISTRY_FULL,
                cavalry_shm::counters::generated::COUNTERS_ALLOCATION_FAILED,
            ]
            .into_iter()
            .map(|id| counters.read(id.record()).expect("a static counter"))
            .sum::<i64>()
        };
        super::WATCH_FOR_TEST.set(Some(None));
        let mut filler = crate::Cavalry::connect_to(&dir).expect("the filling client");
        let mut held = Vec::new();
        for key in 0i32.. {
            match filler.add_counter(
                cavalry_shm::counters::generated::CLUSTER_COMMIT_POSITION.0,
                &key.to_le_bytes(),
                "cluster.commit_position filler",
            ) {
                Ok(counter) => held.push(counter),
                Err(_) => break,
            }
            assert!(key < 100_000, "the driver refuses a counter at some count");
        }
        let before = refusals(&dir);

        super::WATCH_FOR_TEST.set(Some(Some(None)));
        super::PARK_FOR_TEST.set(Some(HOUR));
        super::PARK_FOR_TEST_ONCE.set(true);
        let mut client = crate::Cavalry::connect_to(&dir).expect("connects");
        super::WATCH_FOR_TEST.set(None);
        super::PARK_FOR_TEST.set(None);
        super::PARK_FOR_TEST_ONCE.set(false);
        let client_id = client.client_id();
        wait_within("the conductor did not ask for its counter", || {
            (refusals(&dir) == before + 1).then_some(())
        });
        std::thread::park_timeout(std::time::Duration::from_millis(500));
        assert_eq!(refusals(&dir), before + 1, "asked once, and not again");
        assert_eq!(conductor_parks_overdue(&dir, client_id), None);
        let publication = client
            .add_publication("cavalry:ipc", 2414)
            .expect("the client takes a command after the refusal");
        drop(publication);
        client.close().expect("closes");
        drop(held);
        filler.close().expect("the filling client closes");
        driver.close().expect("the driver closes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A close does not wait on the conductor's park to time out (ADR 0171). The
    /// conductor's backoff parks for an hour here, which stands for the park whose
    /// timeout never comes, and the client has no watch, so `close` returns in time
    /// only if it unparks the conductor's thread after it stores the closed state.
    /// A client with no watch adds no counter.
    #[test]
    fn close_ends_the_conductor_though_its_park_would_not_time_out() {
        let dir = std::env::temp_dir().join(format!(
            "cavalry-client-conductor-close-unparks-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let driver = cavalry_driver::Driver::start(&dir, dir.join("log")).expect("the driver");
        super::WATCH_FOR_TEST.set(Some(None));
        super::PARK_FOR_TEST.set(Some(std::time::Duration::from_secs(3600)));
        let client = crate::Cavalry::connect_to(&dir).expect("connects");
        super::WATCH_FOR_TEST.set(None);
        super::PARK_FOR_TEST.set(None);
        let client_id = client.client_id();
        // Long enough for the conductor to spend its spins and yields and park, and for
        // a watch, had there been one, to have ended that park.
        std::thread::park_timeout(std::time::Duration::from_millis(600));
        assert_eq!(conductor_parks_overdue(&dir, client_id), None);
        let (closed, close) = std::sync::mpsc::channel();
        let closer = std::thread::spawn(move || {
            let _ = closed.send(client.close());
        });
        let result = close
            .recv_timeout(std::time::Duration::from_secs(20))
            .expect("close returned: the conductor stayed parked, so the close did not unpark it");
        result.expect("closes");
        closer.join().expect("the closing thread ended");
        driver.close().expect("the driver closes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The conductor stores a release word by a record's id only into a record that is
    /// the client's to release: one that holds no counter, which the driver has freed
    /// and holds, or one that is still the image's own. Into a record that holds
    /// another counter it stores nothing: that record is another owner's, and its
    /// word would let the driver give it away under its writer (ADR 0179).
    #[test]
    fn the_conductor_releases_by_id_only_a_record_that_is_the_images_or_holds_no_counter() {
        use cavalry_shm::counters::CountersFile;
        use cavalry_shm::counters::generated::PUBLICATION_LIMIT;
        let dir = std::env::temp_dir().join(format!(
            "cavalry-client-conductor-release-by-id-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the directory");
        let mut file = CountersFile::create(&dir, 256).expect("the counters file");
        let writer = CountersWriter::open(&dir).expect("a client's mapping");
        let (image, session) = (41i64, 7i32);
        let mut key = [0u8; 16];
        key[..8].copy_from_slice(&image.to_le_bytes());
        key[8..12].copy_from_slice(&session.to_le_bytes());
        let mut allocate = |type_id, key: &[u8]| {
            file.allocate(type_id, key, "a counter")
                .expect("a record")
                .id()
        };
        let own = allocate(SUBSCRIPTION_POSITION, &key);
        let held = allocate(SUBSCRIPTION_POSITION, &key);
        let other_image = allocate(SUBSCRIPTION_POSITION, &[9u8; 16]);
        let other_type = allocate(PUBLICATION_LIMIT, &key);
        file.hold(held).expect("held");

        for record in [own, held, other_image, other_type] {
            assert!(!file.released(record).expect("a record"));
            super::release_unheld(&writer, record, image, session);
        }
        assert!(file.released(own).expect("a record"), "the image's own");
        assert!(
            file.released(held).expect("a record"),
            "freed and held for this client"
        );
        assert!(
            !file.released(other_image).expect("a record"),
            "another image's position is another owner's"
        );
        assert!(
            !file.released(other_type).expect("a record"),
            "and so is a counter of another kind"
        );
        super::release_unheld(&writer, 100_000, image, session);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What [`announced`] makes of an `available image`, against a real driver's
    /// directory: the image when its log and its position counter are there; `gone`
    /// when the log is not there, or the record holds no counter, or another image's
    /// by its key; and `corrupt` only when a check fails, a record id no counters file
    /// has or a log that is there and is no log (ADR 0078).
    #[test]
    fn an_announced_image_is_its_log_and_counter_or_gone_and_corrupt_only_on_a_failed_check() {
        let dir = std::env::temp_dir().join(format!(
            "cavalry-client-conductor-announced-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let driver = cavalry_driver::Driver::start(&dir, dir.join("log")).expect("the driver");
        let mut client = crate::Cavalry::connect_to(&dir).expect("connects");
        let subscription = client
            .add_subscription("cavalry:ipc", 2412)
            .expect("a subscription");
        let publication = client
            .add_publication("cavalry:ipc", 2412)
            .expect("a publication");
        let (image, session) = (publication.registration_id(), publication.session_id());
        let counters = CountersWriter::open(&dir).expect("the counters file opens");
        let capacity = counters.capacity();
        // The image's position counter, found by its type and its key's first field.
        let position = |counters: &CountersWriter| {
            (0..capacity).find(|id| {
                matches!(counters.record(*id), Ok(Some(fields))
                    if fields.type_id() == SUBSCRIPTION_POSITION.0
                        && fields.key()[..8] == image.to_le_bytes())
            })
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let record = loop {
            if let Some(record) = position(&counters) {
                break i32::try_from(record).expect("a counter id");
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the driver made the image"
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        };
        let free = (0..capacity)
            .rev()
            .find(|id| matches!(counters.record(*id), Ok(None)))
            .and_then(|id| i32::try_from(id).ok())
            .expect("a record with no counter");
        let log = (LogDirectory::Publications, image);
        let of = |correlation_id, session_id, record, log| {
            announced(
                &dir,
                (&counters, &Arc::new(super::Shared::new(0, None))),
                correlation_id,
                session_id,
                record,
                log,
                String::new(),
            )
        };
        let gone = |made: Result<ImageEvent, Closed>| matches!(made, Ok(ImageEvent::Gone { session_id }) if session_id == session);

        assert!(matches!(
            of(image, session, record, log),
            Ok(ImageEvent::Available { correlation_id, session_id, .. })
                if correlation_id == image && session_id == session
        ));
        // Gone: no log by that name; a record that holds no counter; and a record
        // that holds a counter whose key names another image, or another session.
        let no_log = (LogDirectory::Publications, image + 1_000_000);
        assert!(gone(of(image, session, record, no_log)));
        assert!(gone(of(image, session, free, log)));
        assert!(gone(of(image + 1, session, record, log)));
        assert!(matches!(
            of(image, session.wrapping_add(1), record, log),
            Ok(ImageEvent::Gone { .. })
        ));
        // Corrupt: a record id no counters file has, and a log that is there and is
        // no log.
        assert_eq!(of(image, session, -1, log).err(), Some(Closed::Corrupt));
        let past = i32::try_from(capacity).expect("a capacity");
        assert_eq!(of(image, session, past, log).err(), Some(Closed::Corrupt));
        std::fs::write(dir.join("publications/999999.logbuffer"), b"not a log").expect("a file");
        let not_a_log = (LogDirectory::Publications, 999_999);
        assert_eq!(
            of(image, session, record, not_a_log).err(),
            Some(Closed::Corrupt)
        );

        drop(subscription);
        drop(publication);
        drop(client);
        drop(driver);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A replay registration and the handle's half of its end.
    fn replay(replay_session_id: i64) -> (ReplayRegistration, Arc<ReplayEnd>) {
        let end = Arc::new(ReplayEnd::new());
        (
            ReplayRegistration {
                replay_session_id,
                end: Arc::clone(&end),
            },
            end,
        )
    }

    /// The full case first (CLAUDE.md, gate 3): the registry at its capacity with every
    /// replay live and every handle held refuses the next and marks it, and an end
    /// stored or a handle dropped frees a place at the next admission, so that a client
    /// that keeps the handles of ended replays is never refused.
    #[test]
    fn a_full_replay_registry_sweeps_then_refuses_and_an_end_frees_a_place() {
        let capacity = MAX_REPLAYS + 1;
        let mut registry = ReplayRegistry::with_capacity(capacity);
        let mut held = Vec::with_capacity(capacity);
        for id in 0..capacity as i64 {
            let (registration, end) = replay(id);
            assert!(registry.admit(registration));
            held.push(end);
        }
        assert_eq!(registry.len(), capacity);

        // Full, every replay live and every handle held: the next is refused and
        // marked, which `start_replay` turns into `replays-full`.
        let over = capacity as i64;
        let (refused, end) = replay(over);
        assert!(!registry.admit(refused));
        assert!(end.refused());
        assert!(registry.end(over).is_none());
        assert_eq!(registry.len(), capacity);

        // One replay ended, its handle kept: the admission sweeps it and the new one
        // takes its place, admitted and not marked.
        registry.end(0).expect("registered").set(640, 0);
        let (admitted, end) = replay(over + 1);
        assert!(registry.admit(admitted));
        assert!(!end.refused());
        assert!(registry.end(0).is_none());
        assert!(registry.end(over + 1).is_some());
        assert_eq!(registry.len(), capacity);

        // One handle dropped: the same.
        drop(held.swap_remove(1));
        let (admitted, end) = replay(over + 2);
        assert!(registry.admit(admitted));
        assert!(!end.refused());
        assert!(registry.end(1).is_none());
        assert_eq!(registry.len(), capacity);

        // The cycle's sweep forgets an ended replay without waiting for an admission.
        registry.end(2).expect("registered").set(64, 1);
        assert_eq!(registry.sweep(), 1);
        assert!(registry.end(2).is_none());
        assert_eq!(registry.sweep(), 0);
    }

    /// A recording registration and the handle's half of its stop.
    fn recording(correlation_id: i64) -> (RecordingRegistration, Arc<RecordingStop>) {
        let stop = Arc::new(RecordingStop::new());
        (
            RecordingRegistration {
                correlation_id,
                stop: Arc::clone(&stop),
                armed: None,
            },
            stop,
        )
    }

    /// The second answer to an armed `start recording` is its handle's, not the pending
    /// command's: the registration that read `operation success` takes the `recording
    /// ready` or the `error` that names its correlation id next, even while the user's
    /// thread still names that id as pending, and a `start_recording` registration,
    /// with no place for a second answer, never does.
    #[test]
    fn an_armed_recordings_second_answer_goes_to_its_handle() {
        let mut registry = RecordingRegistry::with_capacity(4);
        let encode = |event: Event<'_>| {
            let mut buffer = [0u8; 256];
            let length = event.write(&mut buffer).expect("the event encodes");
            (event.event_type() as i32, buffer[..length].to_vec())
        };
        let ready = |correlation_id| {
            encode(Event::RecordingReady {
                correlation_id,
                recording_id: 7,
                session_id: 3,
                recording_position_counter_id: 200,
                recording_synced_position_counter_id: 201,
            })
        };

        let stop = Arc::new(RecordingStop::new());
        let (producer, mut answers) = spsc::queue::<Response>(1);
        assert!(registry.admit(RecordingRegistration {
            correlation_id: 40,
            stop: Arc::clone(&stop),
            armed: Some(producer),
        }));
        // Before `operation success`, `recording ready` under the pending id is the
        // command's one answer: the driver started the recording at once.
        let (type_id, message) = ready(40);
        assert!(!registry.awaits(40));
        assert!(matches!(
            event(
                5,
                40,
                &|id| registry.awaits(id),
                &mut |_| true,
                type_id,
                &message
            ),
            Ok(Outcome::Answer(Response::RecordingReady { .. }))
        ));
        // `operation success` arms it. The image arrives before the user's thread has
        // cleared its pending id: the answer is the handle's all the same.
        registry.armed(40);
        assert!(registry.awaits(40));
        let outcome = event(
            5,
            40,
            &|id| registry.awaits(id),
            &mut |_| true,
            type_id,
            &message,
        );
        let Ok(Outcome::ArmedAnswer(response)) = outcome else {
            panic!("the second answer was read as {outcome:?}");
        };
        registry.answer(response.clone());
        assert_eq!(answers.poll(), Some(response));
        assert!(!registry.awaits(40), "answered twice, and no third time");
        // The registration follows the recording the second answer named.
        registry
            .stop(7)
            .expect("bound by the second answer")
            .set(4096);
        assert_eq!(stop.get(), Some(4096));

        // An armed recording dropped, or one whose start failed: `error` under the
        // arming id goes to the handle and ends the registration, with nothing pending.
        let dropped_stop = Arc::new(RecordingStop::new());
        let (producer, mut answers) = spsc::queue::<Response>(1);
        assert!(registry.admit(RecordingRegistration {
            correlation_id: 41,
            stop: Arc::clone(&dropped_stop),
            armed: Some(producer),
        }));
        registry.armed(41);
        let before = registry.len();
        let (type_id, message) = encode(Event::Error {
            offending_correlation_id: 41,
            error_code: 20,
            error_message: b"the stream's last subscription is gone",
        });
        let outcome = event(
            5,
            NO_PENDING,
            &|id| registry.awaits(id),
            &mut |_| true,
            type_id,
            &message,
        );
        let Ok(Outcome::ArmedAnswer(response)) = outcome else {
            panic!("the second answer was read as {outcome:?}");
        };
        registry.answer(response);
        assert!(matches!(
            answers.poll(),
            Some(Response::Error {
                correlation_id: 41,
                code: 20,
                ..
            })
        ));
        assert_eq!(registry.len(), before - 1);

        // `start_recording` registers no place for a second answer: `operation
        // success` under its id arms nothing here, and a later event that names the id
        // is nobody's.
        let (plain, _plain_stop) = recording(42);
        assert!(registry.admit(plain));
        registry.armed(42);
        assert!(!registry.awaits(42));
        let (type_id, message) = ready(42);
        assert_eq!(
            event(
                5,
                NO_PENDING,
                &|id| registry.awaits(id),
                &mut |_| true,
                type_id,
                &message
            ),
            Ok(Outcome::Nothing)
        );
    }

    /// The full case first (CLAUDE.md, gate 3): the registry at its capacity with every
    /// recording live and every handle held refuses the next and marks it; a stop
    /// stored or a handle dropped frees a place at the next admission; a registration
    /// follows a recording only once `recording ready` has bound its id, and a stop for
    /// a recording this client does not follow finds nothing.
    #[test]
    fn a_full_recording_registry_sweeps_then_refuses_and_a_stop_frees_a_place() {
        let capacity = RECORDING_REGISTRY;
        let mut registry = RecordingRegistry::with_capacity(capacity);
        let mut held = Vec::with_capacity(capacity);
        for id in 0..capacity as i64 {
            let (registration, stop) = recording(1_000 + id);
            assert!(registry.admit(registration));
            // `recording ready` gives the command's registration its recording id.
            registry.bind(1_000 + id, id);
            held.push(stop);
        }
        assert_eq!(registry.len(), capacity);

        // Full, every recording live and every handle held: the next is refused and
        // marked, which `start_recording` turns into an error.
        let over = capacity as i64;
        let (refused, stop) = recording(1_000 + over);
        assert!(!registry.admit(refused));
        assert!(stop.refused());
        assert_eq!(registry.len(), capacity);

        // A stop is stored for the recording it names and for no other; one this
        // client does not follow, and the id of none, find nothing.
        registry.stop(0).expect("followed").set(640);
        assert_eq!(held[0].get(), Some(640));
        assert_eq!(held[1].get(), None);
        assert!(registry.stop(over + 7).is_none());
        assert!(registry.stop(-1).is_none());

        // The stopped recording's place goes to the next admission, its handle kept.
        let (admitted, stop) = recording(1_000 + over + 1);
        assert!(registry.admit(admitted));
        assert!(!stop.refused());
        assert!(registry.stop(0).is_none());
        assert_eq!(registry.len(), capacity);
        // Not bound yet, it follows no recording: a stop that names another's id, or
        // an id it has not been given, is not stored in it.
        assert!(registry.stop(over + 1).is_none());
        registry.bind(1_000 + over + 1, over + 1);
        registry.stop(over + 1).expect("bound").set(64);
        assert_eq!(stop.get(), Some(64));

        // A handle dropped, a refused command's among them: the cycle's sweep forgets
        // it without waiting for an admission, with the stop just stored.
        drop(held.swap_remove(1));
        assert_eq!(registry.sweep(), 2);
        assert!(registry.stop(1).is_none());
        assert_eq!(registry.sweep(), 0);
        assert_eq!(registry.len(), capacity - 2);
    }

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
                removal: Arc::new(std::sync::atomic::AtomicI64::new(
                    crate::subscription::NOT_REMOVED,
                )),
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

    /// A registration whose subscription sent `remove subscription` is kept until the
    /// driver has answered that command: until then the driver may announce an image
    /// for the subscription, and the conductor must still know the registration to
    /// store that image's release word (ADR 0179). One whose removal could not be
    /// written is kept, since the driver keeps the subscription. Only one whose halves
    /// went with no removal at all, an add the driver refused, is forgotten at a sweep.
    #[test]
    fn a_removed_subscriptions_registration_is_kept_until_the_driver_has_answered() {
        use crate::subscription::REMOVAL_NOT_SENT;
        use std::sync::atomic::Ordering;
        let mut registry = Registry::with_capacity(4);
        let (removed, removed_incoming, removed_retired) = registration(1);
        let (unsent, unsent_incoming, unsent_retired) = registration(2);
        let (refused, refused_incoming, refused_retired) = registration(3);
        // What `Subscription::remove` stores before it writes its command.
        removed.removal.store(77, Ordering::Release);
        unsent.removal.store(REMOVAL_NOT_SENT, Ordering::Release);
        for registration in [removed, unsent, refused] {
            assert!(registry.admit(registration));
        }
        assert!(registry.removing(), "one waits for its answer");
        drop((removed_incoming, removed_retired));
        drop((unsent_incoming, unsent_retired));
        drop((refused_incoming, refused_retired));
        assert_eq!(registry.sweep(), 1, "the add the driver refused, alone");
        assert!(registry.get(3).is_none());
        let kept = registry.get(1).expect("kept until the driver answers");
        assert!(
            kept.images.peer_dropped(),
            "and known as a subscription whose handle is gone, so that an image \
             announced for it is released and not queued"
        );
        assert!(registry.get(2).is_some());
        assert_eq!(registry.sweep(), 0);
        // Another command's answer forgets nothing; the removal's own does, once.
        assert!(!registry.removed(76));
        assert!(!registry.removed(REMOVAL_NOT_SENT));
        assert!(
            !registry.removed(crate::subscription::NOT_REMOVED),
            "nor a subscription that has sent no removal: an event with no correlation \
             id of its own must not take a live subscription's images away"
        );
        assert!(registry.removed(77));
        assert!(registry.get(1).is_none());
        assert!(!registry.removed(77));
        assert!(!registry.removing());
        assert!(
            registry.get(2).is_some(),
            "the driver was never told of this one"
        );
    }

    // Vectors from `spec/vectors/client-protocol.json`, by name.

    /// `client-timeout`: type 3850, client id 5.
    const CLIENT_TIMEOUT: (i32, &[u8]) = (3850, &[5, 0, 0, 0, 0, 0, 0, 0]);
    /// `operation-success`: type 3844, correlation id 3.
    const OPERATION_SUCCESS: (i32, &[u8]) = (3844, &[3, 0, 0, 0, 0, 0, 0, 0]);
    /// The vectors whose `error` is set, with that error: `event-too-short`,
    /// `event-unknown-type`, `event-string-runs-past-message` and `event-length-trailing`.
    const MALFORMED: [(&str, i32, &[u8]); 4] = [
        ("too-short", 3844, &[3, 0, 0, 0]),
        ("type", 3851, &[3, 0, 0, 0, 0, 0, 0, 0]),
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
            event(5, NO_PENDING, &|_| false, &mut |_| true, type_id, message),
            Err(Closed::ClientTimeout)
        );
        assert_eq!(
            event(5, 3, &|_| false, &mut |_| true, type_id, message),
            Err(Closed::ClientTimeout)
        );
    }

    #[test]
    fn client_timeout_for_another_client_leaves_it_open() {
        let (type_id, message) = CLIENT_TIMEOUT;
        assert_eq!(
            event(6, NO_PENDING, &|_| false, &mut |_| true, type_id, message),
            Ok(Outcome::Nothing)
        );
    }

    #[test]
    fn a_decoded_event_nobody_waits_for_leaves_it_open() {
        let (type_id, message) = OPERATION_SUCCESS;
        assert_eq!(
            event(5, NO_PENDING, &|_| false, &mut |_| true, type_id, message),
            Ok(Outcome::Nothing)
        );
        assert_eq!(
            event(5, 4, &|_| false, &mut |_| true, type_id, message),
            Ok(Outcome::Nothing),
            "another command's"
        );
    }

    #[test]
    fn the_pending_commands_event_is_its_answer() {
        let (type_id, message) = OPERATION_SUCCESS;
        assert_eq!(
            event(5, 3, &|_| false, &mut |_| true, type_id, message),
            Ok(Outcome::Answer(Response::Success { correlation_id: 3 }))
        );
        // `error`: type 3841, offending correlation id 4, code 1, message "abc".
        let error: &[u8] = &[
            4, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 3, 0, 0, 0, 0x61, 0x62, 0x63, 0,
        ];
        assert_eq!(
            event(5, 4, &|_| false, &mut |_| true, 3841, error),
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
            event(
                5,
                NO_PENDING,
                &|_| false,
                &mut |id| id == 6,
                0x0F02,
                &message
            ),
            Ok(Outcome::ImageAvailable {
                correlation_id: 7,
                session_id: -2,
                subscriber_registration_id: 6,
                position_counter_id: 14,
                log_name: (LogDirectory::Publications, 7),
                source_identity: "cavalry:ipc".to_string(),
            })
        );
        // The same image, announced to a client that does not hold subscription 6: it
        // is nothing to that client, whatever it waits on, and the check is asked of
        // the registration id the event names.
        let mut asked = Vec::new();
        assert_eq!(
            event(
                5,
                NO_PENDING,
                &|_| false,
                &mut |id| {
                    asked.push(id);
                    false
                },
                0x0F02,
                &message
            ),
            Ok(Outcome::Nothing)
        );
        assert_eq!(asked, [6]);
        assert_eq!(
            event(5, 7, &|_| false, &mut |_| false, 0x0F02, &message),
            Ok(Outcome::Nothing)
        );
        // An image whose log file name is not the page's shape is corrupt before the
        // subscription is asked for: an event this client cannot decode closes it,
        // whoever the event was for.
        let mut misnamed = message.clone();
        // The log file name starts after 28 bytes of fields and its length: the slash
        // of `publications/7.logbuffer` is its thirteenth byte.
        assert_eq!(misnamed[32 + 12], b'/');
        misnamed[32 + 12] = b'-';
        assert_eq!(
            event(5, NO_PENDING, &|_| false, &mut |_| false, 0x0F02, &misnamed),
            Err(Closed::Corrupt)
        );
        assert_eq!(
            log_name(b"publications/42.logbuffer"),
            Some((LogDirectory::Publications, 42))
        );
        assert_eq!(
            log_name(b"images/42.logbuffer"),
            Some((LogDirectory::Images, 42))
        );
        assert_eq!(log_name(b"42.logbuffer"), None);
        assert_eq!(log_name(b"publications/x.logbuffer"), None);
    }

    #[test]
    fn an_event_that_fails_a_decoding_check_closes_it() {
        for (error, type_id, message) in MALFORMED {
            // The fixture fails the check it is named for, so that one which drifts from
            // the vectors fails here rather than passing on another check.
            assert_eq!(
                Event::parse(type_id, message).map_err(|e| e.reason()),
                Err(error),
                "the `{error}` vector"
            );
            assert_eq!(
                event(5, NO_PENDING, &|_| false, &mut |_| true, type_id, message),
                Err(Closed::Corrupt),
                "the `{error}` vector"
            );
        }
    }
}
