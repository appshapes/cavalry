//! The values `spec/client-protocol.md` fixes for this version: the timeouts of
//! "Liveness and timeouts", the file sizes and the registries' capacities of "Sizes";
//! and the cadence `spec/log-buffer.md`, "The driver", fixes for the producer position.
//! None is configurable in this version (ADR 0019, ADR 0022, ADR 0030).

use std::time::Duration;

/// How often the conductor stores its clock in the consumer heartbeat.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);

/// A consumer heartbeat older than this is `driver-timeout` to a client.
pub const DRIVER_TIMEOUT: Duration = Duration::from_secs(10);

/// How often a client sends `client keepalive` when it has sent nothing else.
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_millis(500);

/// A client whose last command is older than this is timed out.
pub const CLIENT_LIVENESS_TIMEOUT: Duration = Duration::from_secs(10);

/// An add or remove command unanswered for this long is reported to the caller as lost.
pub const RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the command ring's head may stand still with the tail beyond it before the
/// claim at the head is treated as a dead client's: the client liveness timeout.
pub const COMMAND_RING_UNBLOCK_TIMEOUT: Duration = CLIENT_LIVENESS_TIMEOUT;

/// How long a publication's consumer position may stand still with the producer beyond
/// it before the claim there is treated as a dead publisher's.
pub const PUBLICATION_UNBLOCK_TIMEOUT: Duration = Duration::from_secs(15);

/// The least time between two loads of one publication's producer position by the
/// conductor: the publisher stores to that line on every commit, and a load on every
/// duty cycle kept the line moving between two caches, which the publisher paid at p50
/// and p99.99 (`spec/log-buffer.md`, "The driver", Positions; ADR 0022). Two
/// milliseconds, twice the backoff strategy's longest park: at one, a publisher at ten
/// million claims a second paid the transfer that follows a load on one claim in ten
/// thousand, the gate's own percentile, so the exclusive claim row read one of two
/// values by the process (ADR 0032).
pub const PRODUCER_POSITION_INTERVAL: Duration = Duration::from_millis(2);

/// How long a closed publication's log stays for its subscribers to drain.
pub const PUBLICATION_LINGER: Duration = Duration::from_secs(5);

/// How long a freed counter's record stays reclaimed before it is allocated again.
pub const COUNTER_REUSE_TIMEOUT: Duration = Duration::from_secs(1);

/// The counters file's capacity, in counters.
pub const COUNTERS_CAPACITY: u32 = 8192;

/// The command ring's capacity, in bytes.
pub const COMMAND_RING_CAPACITY: u32 = 1024 * 1024;

/// The broadcast buffer's capacity, in bytes.
pub const BROADCAST_CAPACITY: u32 = 1024 * 1024;

/// The term length of a `cavalry:ipc` publication's log, in bytes.
pub const IPC_TERM_LENGTH: u32 = 64 * 1024 * 1024;

/// The MTU length of a `cavalry:ipc` publication's log, in bytes.
pub const IPC_MTU_LENGTH: u32 = 1408;

/// The publication window of a `cavalry:ipc` publication: term length ÷ 2.
pub const IPC_PUBLICATION_WINDOW: u32 = IPC_TERM_LENGTH / 2;

/// How many clients the driver tracks: every client id it has not forgotten.
pub const MAX_CLIENTS: usize = 256;

/// How many publication registrations the driver holds, shared or not.
pub const MAX_PUBLICATIONS: usize = 1024;

/// How many subscription registrations the driver holds.
pub const MAX_SUBSCRIPTIONS: usize = 1024;

/// How many images the driver holds: pairs of a subscription and a publication that match.
pub const MAX_IMAGES: usize = 2048;

/// How many recordings the driver holds: starting or live, one per publication at most
/// (ADR 0025).
pub const MAX_RECORDINGS: usize = 1024;

/// How many replays the driver holds: starting or live (ADR 0030); the one definition
/// is `cavalry_proto`'s, which the client shares (ADR 0036).
pub const MAX_REPLAYS: usize = cavalry_proto::client_protocol::MAX_REPLAYS;

/// How many replay publications the driver holds: a live replay's, and one that lingers
/// after its replay ended, until every subscriber has reached its end of stream or the
/// publication linger has passed. A replay leaves the replays registry when it ends,
/// so its publication outlives its registry slot, and new replays could start and end
/// under the linger with no bound on the publications they leave behind; this is that
/// bound, two per replay slot. `start replay` is refused with error code 10 when the
/// starting replays and the replay publications together have reached it (ADR 0036).
pub const MAX_REPLAY_PUBLICATIONS: usize = 2 * MAX_REPLAYS;

/// How many retention commands the driver holds in progress at once: truncate recording,
/// purge segments and purge recording, from admission to the answer, the replays
/// registry's capacity (`spec/client-protocol.md`, "Sizes"; ADR 0031).
pub const MAX_COMMANDS_IN_PROGRESS: usize = MAX_REPLAYS;

/// How long a replay's publication may have no subscriber before the replay ends with
/// reason 2, from its start or from the moment its last subscriber went: Aeron's replay
/// connect timeout.
pub const REPLAY_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The capacity of each queue between the conductor and the replayer, in requests or
/// replies; a start that finds the request queue full is answered with error code 10,
/// and the replayer keeps a reply the reply queue has no room for until it has.
pub(crate) const REPLAYER_QUEUE_CAPACITY: usize = 2 * MAX_REPLAYS;

/// The most bytes the replayer reads from a segment file for one replay in one duty
/// cycle, 1 MiB (ADR 0030); a read is bounded by the term, the replay's end, the
/// recording's extent and the publication's limit as well.
pub(crate) const REPLAYER_BLOCK_LIMIT: usize = 1024 * 1024;

/// How many requests the replayer handles per duty cycle before it turns to the replays.
pub(crate) const REPLAYER_REQUESTS_PER_CYCLE: usize = 16;

/// How often the replayer reads a live recording's catalog slot again, to learn its
/// stop: once per second, the recorder's sync cadence.
pub(crate) const REPLAYER_CATALOG_INTERVAL: Duration = Duration::from_secs(1);

/// The capacity of each queue between the conductor and the recorder, in requests or
/// replies; a start or stop that finds the request queue full is answered with error
/// code 10, and the recorder keeps a reply the reply queue has no room for until it has.
pub(crate) const RECORDER_QUEUE_CAPACITY: usize = 2 * MAX_RECORDINGS;

/// The most bytes the recorder takes from a term in one block, 1 MiB (ADR 0025); the
/// first committed frame is always taken, so a block makes progress under any limit.
pub(crate) const RECORDER_BLOCK_LIMIT: usize = 1024 * 1024;

/// How many requests the recorder handles per duty cycle before it turns to the
/// recordings.
pub(crate) const RECORDER_REQUESTS_PER_CYCLE: usize = 16;

/// A duration as the nanoseconds the driver's clock counts in.
pub(crate) const fn nanos(duration: Duration) -> i64 {
    let nanos = duration.as_nanos();
    if nanos > i64::MAX as u128 {
        i64::MAX
    } else {
        nanos as i64
    }
}
