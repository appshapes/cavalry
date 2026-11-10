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

/// How long an `add subscription` waits for its channel's previous receive endpoint to
/// close, from the moment the conductor read it, before it is answered with error code 4
/// and registers nothing: half the response timeout, so that the answer reaches the
/// client before the client reports the add lost (ADR 0049).
pub const CLOSING_ENDPOINT_WAIT: Duration = Duration::from_secs(5);
const _: () = assert!(CLOSING_ENDPOINT_WAIT.as_nanos() < RESPONSE_TIMEOUT.as_nanos());

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

/// The counters file's capacity, in counters: sized so that the registries fill before
/// it does, 16,155 at most in client protocol version 7 (`spec/client-protocol.md`,
/// "Sizes"; ADR 0040, ADR 0053, ADR 0059, ADR 0062).
pub const COUNTERS_CAPACITY: u32 = 16384;

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

/// How many counters clients have added with `add counter` and not removed, whichever
/// client: one record of the counters file each (`spec/client-protocol.md`, "Sizes";
/// ADR 0062).
pub const MAX_CLIENT_COUNTERS: usize = 256;

/// How many subscription registrations the driver holds.
pub const MAX_SUBSCRIPTIONS: usize = 1024;

/// How many images the driver holds: pairs of a subscription and a publication that match.
pub const MAX_IMAGES: usize = 2048;

/// How many recordings the driver holds: starting or live, one per publication at most
/// (ADR 0025); the one definition is `cavalry_proto`'s, which the client shares, as it
/// shares [`MAX_REPLAYS`].
pub const MAX_RECORDINGS: usize = cavalry_proto::client_protocol::MAX_RECORDINGS;

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

/// Every publication the sender may hold at once: a client's `cavalry:udp`
/// publications and a replay's over a `cavalry:udp` channel, which is closed through
/// the sender as a client's is (ADR 0041, ADR 0043). The sender's lists, its queues and
/// the conductor's deferred requests for it are sized from this, not from
/// [`MAX_PUBLICATIONS`] alone.
pub const MAX_SENDER_PUBLICATIONS: usize = MAX_PUBLICATIONS + MAX_REPLAY_PUBLICATIONS;

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
/// replies; a start or a `sync recording` that finds the request queue full is answered
/// with error code 10, a stop or an attach waits in the conductor for room, and the
/// recorder keeps a reply the reply queue has no room for until it has.
pub(crate) const RECORDER_QUEUE_CAPACITY: usize = 2 * MAX_RECORDINGS;

/// The most bytes the recorder takes from a term in one block, 1 MiB (ADR 0025); the
/// first committed frame is always taken, so a block makes progress under any limit.
pub(crate) const RECORDER_BLOCK_LIMIT: usize = 1024 * 1024;

/// How many requests the recorder handles per duty cycle before it turns to the
/// recordings.
pub(crate) const RECORDER_REQUESTS_PER_CYCLE: usize = 16;

/// How many send endpoints the driver holds: every resolved `cavalry:udp` channel a
/// publication sends on; the one definition is `cavalry_proto`'s (`spec/udp.md`,
/// "Values"; ADR 0040).
pub const MAX_SEND_ENDPOINTS: usize = cavalry_proto::udp::MAX_SEND_ENDPOINTS;

/// The term length of a `cavalry:udp` publication's log, in bytes (`spec/udp.md`,
/// "Values").
pub const UDP_TERM_LENGTH: u32 = cavalry_proto::udp::UDP_TERM_LENGTH;

/// The MTU length of a `cavalry:udp` publication: the longest datagram (`spec/udp.md`,
/// "Values").
pub const UDP_MTU_LENGTH: u32 = cavalry_proto::udp::MTU_LENGTH_DEFAULT;

/// The publication window of a `cavalry:udp` publication: term length ÷ 2.
pub const UDP_PUBLICATION_WINDOW: u32 = UDP_TERM_LENGTH / 2;

/// The capacity of each queue between the conductor and the sender, in requests or
/// replies: two per publication the sender may hold, a client's or a replay's
/// ([`MAX_SENDER_PUBLICATIONS`]), and two per endpoint, an add and a remove each; an
/// add that finds the request queue full is answered with error code 10, and the
/// sender keeps a reply the reply queue has no room for until it has (ADR 0041).
pub(crate) const SENDER_QUEUE_CAPACITY: usize = 2 * (MAX_SENDER_PUBLICATIONS + MAX_SEND_ENDPOINTS);

/// How many requests the sender handles per duty cycle before it turns to the
/// endpoints and the publications.
pub(crate) const SENDER_REQUESTS_PER_CYCLE: usize = 16;

/// How many receive endpoints the driver holds: every resolved `cavalry:udp` channel a
/// subscription receives on; the one definition is `cavalry_proto`'s (`spec/udp.md`,
/// "Values"; ADR 0040).
pub const MAX_RECEIVE_ENDPOINTS: usize = cavalry_proto::udp::MAX_RECEIVE_ENDPOINTS;

/// How many session records the receiver keeps, twice the images registry: a datagram
/// for a session past them is dropped and counted in `receiver.sessions_full`
/// (`spec/udp.md`, "Values"; ADR 0042).
pub const MAX_SESSION_RECORDS: usize = 2 * MAX_IMAGES;

/// How long a session record waits for the setup frame a status message asked for
/// before the next datagram asks again (`spec/udp.md`, "Values": pending setup timeout).
pub const PENDING_SETUP_TIMEOUT: Duration = Duration::from_secs(1);

/// How long a retired image's session record stays on cool-down, during which a setup,
/// data datagram or heartbeat for the session creates no second image: the image
/// liveness timeout (`spec/udp.md`, "Values").
pub const IMAGE_COOL_DOWN: Duration = cavalry_proto::udp::IMAGE_LIVENESS_TIMEOUT;

/// How many bytes of an image's term the receiver's rebuild scan reads per image per
/// duty cycle, 1 MiB (`spec/udp.md`, "The receiver", step 3; ADR 0042). At least a
/// frame's alignment, since a scan bounded below 32 bytes reads no word and a receiver
/// that continued from where it stopped would never move.
pub const REBUILD_SCAN_LIMIT: u32 = 1024 * 1024;
const _: () = assert!(REBUILD_SCAN_LIMIT >= cavalry_proto::frame::FRAME_ALIGNMENT as u32);

/// How often the receiver sweeps its session table for pending setups and cool-downs
/// that expired, so that the table is not walked on every cycle (ADR 0042).
pub(crate) const SESSION_SWEEP_INTERVAL: Duration = Duration::from_millis(100);

/// How many status messages with the end-of-stream flag the receiver sends once an
/// image's stream has ended, at the status message interval (`spec/udp.md`, "The
/// receiver", step 5).
pub(crate) const END_OF_STREAM_STATUS_MESSAGES: u8 = 5;

/// The capacity of each queue between the conductor and the receiver, in requests or
/// replies: two per subscription, two per endpoint, two per image and one per reader;
/// an `add subscription` that finds the request queue full is answered with error code
/// 10, and the receiver keeps a reply the reply queue has no room for until it has
/// (ADR 0042).
pub(crate) const RECEIVER_QUEUE_CAPACITY: usize =
    2 * (MAX_SUBSCRIPTIONS + MAX_RECEIVE_ENDPOINTS + MAX_IMAGES) + MAX_IMAGES + MAX_RECORDINGS;

/// How many requests the receiver handles per duty cycle before it turns to the
/// endpoints and the images.
pub(crate) const RECEIVER_REQUESTS_PER_CYCLE: usize = 16;

/// A duration as the nanoseconds the driver's clock counts in.
pub(crate) const fn nanos(duration: Duration) -> i64 {
    let nanos = duration.as_nanos();
    if nanos > i64::MAX as u128 {
        i64::MAX
    } else {
        nanos as i64
    }
}
