//! The consensus module: one member's [`Consensus`] run over a driver, as a client of
//! it (ADR 0063). It gives the core its [`Io`] through the commands every program
//! uses: the consensus messages on a publication to each peer and a subscription of
//! its own, the leader's log as a publication with every other member as a
//! destination, created at the term's base log position, and a recording of it that
//! begins there, a follower's log as a subscription with a recording armed on it, and
//! the record of terms in the `terms` file of the member's log directory. What the
//! core counts it stores in counters it adds to the driver's counters file, where
//! `cavalry doctor` reads them.
//!
//! One thread drives a module: [`ConsensusModule::do_work`] once per duty cycle. A
//! cycle reads the clock once, allocates nothing and sends no command; the commands, a
//! round trip each, come at the member's start, where its election adds, leaves or
//! gives up the log, where a leader starts or stops a replay for a member that is
//! behind (ADR 0091), where a member that is behind subscribes on its `catchup`
//! endpoint for a replay or removes that subscription (ADR 0094), and where a member
//! truncates its log (ADR 0095). One cycle more leaves the rule: the one in which a
//! member names a recording of its log writes the file of the log's recordings in the
//! driver directory, for the member's service, before the counter holds the id (ADR
//! 0103).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use cavalry_client::{
    ArmedRecording, Cavalry, ClientError, Counter, Fault, Flags, Offer, Publication, Recording,
    Replay, Subscription,
};
use cavalry_log::catalog::{CatalogError, CatalogReader};
use cavalry_log::layout::State;
use cavalry_log::recordings::{ListedRecording, RecordingsError, RecordingsFile};
use cavalry_log::terms::{NO_RECORDING, TermsError, TermsFile};
use cavalry_proto::client_protocol::{ErrorCode, FOLLOW_LENGTH, ReplayStopReason};
use cavalry_proto::cluster::{
    CONSENSUS_STREAM_ID, EMPTY_MEMBER, Endpoint, LOG_RECORDINGS_FILE_PREFIX,
    LOG_RECORDINGS_TMP_SUFFIX, LOG_STREAM_ID, LogRecording, MAX_LOG_RECORDINGS_LENGTH, MAX_MEMBERS,
    MembersError, Message, Stream, parse_members, write_log_recordings,
};
use cavalry_proto::udp::UDP_TERM_LENGTH;
use cavalry_shm::counters::CounterId;
use cavalry_shm::counters::generated::{
    CLUSTER_APPEND_POSITION, CLUSTER_COMMIT_POSITION, CLUSTER_ELECTION_CAUSE,
    CLUSTER_ELECTION_STATE, CLUSTER_LEADER_MEMBER_ID, CLUSTER_LEADERSHIP_TERM_ID,
    CLUSTER_LOG_RECORDING_ID, CLUSTER_LOG_RECORDINGS_UNWRITTEN, CLUSTER_MESSAGES_REJECTED,
    CLUSTER_ROLE, COUNTERS,
};

use crate::consensus::{
    Clock, Consensus, Counts, ElectionState, Io, Log, MembersRefused, Refused, ReplayStopped, Role,
    base_above, term_at_or_below, terms_above,
};

/// The longest payload [`ConsensusModule::offer`] takes: what one frame of the log
/// carries after the frame header and the session message header, at the UDP
/// transport's MTU length of 1,408 bytes. An entry is one frame in this version, so
/// that no entry lies across the commit position.
pub const MAX_PAYLOAD_LENGTH: usize = 1408 - 32 - 32;

/// How many consensus messages one duty cycle reads.
const CONSENSUS_POLL_LIMIT: usize = 16;

/// How many fragments of the log one duty cycle of a follower's module reads past.
const LOG_POLL_LIMIT: usize = 64;

/// How long a member leaving the log waits for its recording's stop position.
const STOP_TIMEOUT: Duration = Duration::from_secs(1);
/// How many of the recordings of the log it has stopped a member keeps for replays:
/// the last this many, the oldest forgotten when one more stops (ADR 0091).
const KEPT_RECORDINGS: usize = 1024;
/// No sync asked for yet: no position a recording has written.
const NO_SYNC_ASKED: i64 = -1;

const DOC: &str = "docs/reference/cluster.md#roles-and-the-election";
const DOC_MEMBERS: &str = "docs/reference/cluster.md#members-and-streams";
const DOC_LOG_RECORDINGS: &str = "docs/reference/cluster.md#the-logs-recordings";
const DOC_KEPT_LOG: &str = "docs/reference/durable-log.md#log-recordings";

/// Why a consensus module did not start, or stopped.
#[derive(Debug)]
pub enum RaftError {
    /// The members string was refused.
    Members(MembersError),
    /// The member id is not one of the string's.
    Member(MembersRefused),
    /// The member's record of its log's recordings, the `log-recordings` file in its
    /// log directory, could not be read or written (`spec/durable-log.md`, "Log
    /// recordings"; ADR 0108). At a start the member does not start. In a duty cycle
    /// the recording that was to join the member's log is no part of it yet, and the
    /// next cycle writes the record again.
    Recordings(RecordingsError),
    /// The member's log directory holds a log and a record of terms that holds no
    /// term (`spec/cluster.md`, "A member's start"; ADR 0108): the `terms` file was
    /// removed or replaced, and with it went the terms of the log and the votes the
    /// member cast. A member records a term before its record of the log's
    /// recordings names a recording of that term, and a truncation that deletes
    /// every term has cut the log to nothing first, so no stop leaves this.
    TermsLost {
        /// Where the log the directory holds ends.
        log_end: i64,
    },
    /// The catalog in the member's log directory does not hold the member's log as
    /// its record of the log's recordings names it (`spec/cluster.md`, "A member's
    /// start"; ADR 0108): the directory is not as the member left it, and the member
    /// does not start on a log it cannot vouch for.
    ListedRecording {
        /// The recording the record names.
        recording_id: i64,
        /// Where the record says it begins.
        listed_start: i64,
        /// What the catalog holds instead.
        found: ListedFound,
    },
    /// The `terms` file could not be read or written.
    Terms(TermsError),
    /// The driver's catalog could not be read in the member's log directory, at the
    /// member's start or when a recording of the log was handed over, or its slot for
    /// that recording is empty or holds a recording of another session or stream,
    /// which is `unknown`: the member takes where a recording begins from it, and a
    /// catalog that is not its driver's would give another recording's (ADR 0093).
    Catalog(CatalogError),
    /// The driver refused a command, or the client is closed.
    Client(ClientError),
    /// A poll of the consensus stream or of the log stopped: an image that failed a
    /// check, or a client that is closed.
    Poll(Fault),
    /// A step this version does not take.
    Unsupported {
        /// What was asked.
        what: &'static str,
    },
    /// The payload offered is longer than [`MAX_PAYLOAD_LENGTH`].
    PayloadTooLong {
        /// Its length.
        length: usize,
    },
    /// The offer was refused by the member's state; offer again as the reason says.
    Offer(Refused),
    /// A member leaving the log stopped the recording it held, and 1 s later had not
    /// read where the recording stopped. It holds the recording still, in init, and the
    /// next duty cycle stops it again and waits for its stop.
    StopTimeout {
        /// The recording held.
        recording_id: i64,
    },
    /// The file of the member's log's recordings could not be written in the driver
    /// directory (`spec/cluster.md`, "The log's recordings"; ADR 0103). The member
    /// names no recording to its service until a later duty cycle has written it,
    /// and every cycle until then returns this error after it has done its work.
    /// What calls `do_work` decides how long that may last: `cavalry run` tries a
    /// cycle that fails again every 100 ms and gives the member up after 10 s.
    LogRecordings {
        /// The file, or the name it is written under before it is renamed.
        path: PathBuf,
        /// The error.
        source: std::io::Error,
    },
}

/// What a member's start found in the catalog in place of a recording its record of
/// the log's recordings names ([`RaftError::ListedRecording`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListedFound {
    /// A recording under that id that begins elsewhere or is of another stream: the
    /// catalog is not the one the record was written beside.
    Other {
        /// Where the catalog says it begins.
        start_position: i64,
        /// The stream the catalog says it is of.
        stream_id: i32,
    },
    /// A recording the driver found damaged, state 2 in the catalog.
    Invalid,
    /// No recording, while the catalog holds one the record names after it. A
    /// truncation purges a member's recordings from the last one back, so the ones
    /// a stop in the middle of it left missing are the record's last.
    Missing,
    /// A recording that begins past where the ones before it end.
    Gap {
        /// Where the recordings before it end.
        end_position: i64,
    },
}

impl RaftError {
    /// The reason's name: `members`, `member`, `recordings`, `terms-lost`,
    /// `listed-recording`, `terms`, `catalog`, `client`, `poll`, `unsupported`, `payload-too-long`, `offer`,
    /// `stop-timeout` or `log-recordings`.
    pub const fn reason(&self) -> &'static str {
        match self {
            RaftError::Members(_) => "members",
            RaftError::Member(_) => "member",
            RaftError::Recordings(_) => "recordings",
            RaftError::TermsLost { .. } => "terms-lost",
            RaftError::ListedRecording { .. } => "listed-recording",
            RaftError::Terms(_) => "terms",
            RaftError::Catalog(_) => "catalog",
            RaftError::Client(_) => "client",
            RaftError::Poll(_) => "poll",
            RaftError::Unsupported { .. } => "unsupported",
            RaftError::PayloadTooLong { .. } => "payload-too-long",
            RaftError::Offer(_) => "offer",
            RaftError::StopTimeout { .. } => "stop-timeout",
            RaftError::LogRecordings { .. } => "log-recordings",
        }
    }
}

impl core::fmt::Display for RaftError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let reason = self.reason();
        match self {
            RaftError::Members(error) => {
                write!(f, "member refused ({reason}): {error}; see {DOC_MEMBERS}")
            }
            RaftError::Member(error) => write!(f, "member refused ({reason}): {error}"),
            RaftError::Recordings(error) => write!(f, "member stopped ({reason}): {error}"),
            RaftError::TermsLost { log_end } => write!(
                f,
                "member refused ({reason}): the member's log directory holds its log to \
                 position {log_end}, and its record of terms, the `terms` file, holds no \
                 term: that file was removed or replaced. The terms of the log and the \
                 votes the member cast were in it, and the member does not start without \
                 them. Start it with the log directory it had, whole, as its driver left \
                 it. If the file is lost, leave the member stopped: the cluster goes on \
                 while a quorum of the others runs, and a cluster is brought back to its \
                 full count by starting it again as a new cluster, every member on a new \
                 log directory. Do not start this member alone on a new log directory; \
                 see {DOC}"
            ),
            RaftError::ListedRecording {
                recording_id,
                listed_start,
                found,
            } => {
                write!(
                    f,
                    "member refused ({reason}): the member's record of its log's \
                     recordings, the `log-recordings` file in its log directory, names \
                     recording {recording_id} as part of its log from position \
                     {listed_start}, and the `catalog` beside it holds "
                )?;
                match found {
                    ListedFound::Other {
                        start_position,
                        stream_id,
                    } => write!(
                        f,
                        "under that id a recording of stream {stream_id} that begins at \
                         {start_position}: the two files are not of one directory"
                    )?,
                    ListedFound::Invalid => write!(
                        f,
                        "that recording as damaged, state 2, which `cavalry doctor` names"
                    )?,
                    ListedFound::Missing => write!(
                        f,
                        "no such recording, and holds one the record names after it: \
                         a recording of the member's log was removed"
                    )?,
                    ListedFound::Gap { end_position } => write!(
                        f,
                        "the recordings before it ending at {end_position}, below where \
                         it begins: part of the member's log is gone"
                    )?,
                }
                write!(
                    f,
                    ". The member does not start on a log it cannot vouch for. Start it \
                     with the log directory it had, whole, as its driver left it. If that \
                     directory is lost or damaged, leave the member stopped: the cluster \
                     goes on while a quorum of the others runs, and a cluster is brought \
                     back to its full count by starting it again as a new cluster, every \
                     member on a new log directory. Do not start this member alone on a \
                     new log directory; see {DOC_KEPT_LOG}"
                )
            }
            RaftError::Terms(error) => write!(f, "member stopped ({reason}): {error}"),
            RaftError::Catalog(error) => write!(
                f,
                "member stopped ({reason}): {error}. The member reads where each of its \
                 recordings of the log begins from the catalog in its log directory, \
                 which must be its driver's. Start the member with the log directory its \
                 driver was started with. If that is the directory it has, the catalog \
                 there could not be read, and the error above names the file and the \
                 cause; see {DOC_MEMBERS}"
            ),
            RaftError::Client(error) => write!(f, "member stopped ({reason}): {error}"),
            RaftError::Poll(fault) => write!(f, "member stopped ({reason}): {fault}"),
            RaftError::Unsupported { what } => write!(
                f,
                "member stopped ({reason}): {what} is not in this version; start the cluster \
                 as a new cluster, every member on a new log directory; see {DOC}"
            ),
            RaftError::PayloadTooLong { length } => write!(
                f,
                "offer refused ({reason}): the payload is {length} bytes and an entry carries \
                 at most {MAX_PAYLOAD_LENGTH}; send it in parts of that length; see {DOC}"
            ),
            RaftError::Offer(refused) => write!(f, "{refused}"),
            RaftError::StopTimeout { recording_id } => write!(
                f,
                "member stopped ({reason}): the member stopped recording {recording_id} as \
                 it left the log, and 1 s later the driver had not said where it stopped, so \
                 the member does not know where its log ends; it stays in init holding the \
                 recording. Call do_work again, which stops it again and waits; if this \
                 repeats, run `cavalry doctor`: the driver or this process is starved of CPU; \
                 see {DOC}"
            ),
            RaftError::LogRecordings { path, source } => {
                // The module's own writer refuses a list only if the module made one
                // the page forbids: nothing an operator can put right.
                let remedy = if source.kind() == std::io::ErrorKind::InvalidData {
                    "This is a fault of the consensus module and not of the directory: \
                     report it with this message"
                } else {
                    "Make the driver directory writable by the member's user and give it \
                     room"
                };
                write!(
                    f,
                    "member stopped ({reason}): {} could not be written: {source}. The \
                     member lists the recordings of its log there for its service, and \
                     names no recording in `cluster.log_recording_id` until the file is \
                     written: in each duty cycle that fails so it still records and \
                     replicates, and its service reads nothing. {remedy}; do_work writes \
                     the file again on its next call, and `cavalry run` makes that call \
                     every 100 ms and gives the member up after 10 s of cycles that fail; \
                     see {DOC_LOG_RECORDINGS}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for RaftError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RaftError::Members(error) => Some(error),
            RaftError::Member(error) => Some(error),
            RaftError::Terms(error) => Some(error),
            RaftError::Recordings(error) => Some(error),
            RaftError::Catalog(error) => Some(error),
            RaftError::Client(error) => Some(error),
            RaftError::Poll(fault) => Some(fault),
            RaftError::Offer(error) => Some(error),
            RaftError::LogRecordings { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<ClientError> for RaftError {
    fn from(error: ClientError) -> Self {
        RaftError::Client(error)
    }
}

impl From<TermsError> for RaftError {
    fn from(error: TermsError) -> Self {
        RaftError::Terms(error)
    }
}

impl From<RecordingsError> for RaftError {
    fn from(error: RecordingsError) -> Self {
        RaftError::Recordings(error)
    }
}

impl From<CatalogError> for RaftError {
    fn from(error: CatalogError) -> Self {
        RaftError::Catalog(error)
    }
}

/// The two clocks of a duty cycle: a monotonic one since the module started, and the
/// wall clock's milliseconds, the cluster's time.
fn read_clock(started: Instant) -> Clock {
    Clock {
        now_ns: u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
        time_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| {
                i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
            }),
    }
}

/// `cavalry:udp?endpoint=<host>:<port>` for an endpoint of the members string.
fn endpoint_channel(endpoint: &Endpoint<'_>) -> String {
    format!("cavalry:udp?endpoint={}", host_port(endpoint))
}

/// `<host>:<port>`, an IPv6 literal in its square brackets again.
fn host_port(endpoint: &Endpoint<'_>) -> String {
    let host = String::from_utf8_lossy(endpoint.host);
    if host.contains(':') {
        format!("[{host}]:{}", endpoint.port)
    } else {
        format!("{host}:{}", endpoint.port)
    }
}

/// Another member as this one reaches it.
struct Peer {
    id: i32,
    /// The publication on the consensus stream to the peer's `consensus` endpoint.
    consensus: Publication,
    /// The peer's `log` endpoint as a destination of the leader's log.
    log_destination: String,
    /// The peer's `catchup` endpoint as the channel of a replay to it.
    catchup_channel: String,
}

/// A recording of the log this member made and stopped, by where it lies in the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Kept {
    recording_id: i64,
    /// The position its first frame is at.
    start: i64,
    /// The position it stopped at.
    stop: i64,
}

/// A recording of the log this member is making, with where it begins.
struct Held {
    recording: Recording,
    /// The recording's start position as the catalog holds it: where its first frame
    /// is. It is the image's join position when the recording was armed before the
    /// image arrived, and past it when the image came first (ADR 0093).
    start: i64,
    /// Where the member's log ended when the member subscribed for this recording.
    /// A recording of the live log that began below it is no part of the log: its
    /// image joined below the member's append position (ADR 0094). It is taken at
    /// the subscription and not when the driver hands the recording over: in a
    /// catch-up the replay goes on meanwhile and may carry the log past where the
    /// live recording begins before the module has the recording in hand. A
    /// recording of a replay's image is given the end of the recordings the member
    /// stopped, at the hand-over, and nothing reads it: a replay's recording counts
    /// by where it begins alone ([`ClientIo::end_below_live`]).
    floor: i64,
    /// Whether the election has been told this recording's image: [`Io::log_image`]
    /// has returned its start for the image's session, or [`Io::catchup_image`] has
    /// returned it. Only then can the recording become part of the member's log.
    checked: bool,
    /// Whether the member's record of its log's recordings names this recording (ADR
    /// 0108). Only then is it part of the member's log: the member reports no
    /// position this recording holds before the record that names it is synced, so
    /// that a member that starts again finds every recording it reported from.
    listed: bool,
}

impl Held {
    /// Whether this recording of the live log carries the member's log on from
    /// `end`: the election was told its image, it began at or past where the log
    /// ended when the member subscribed, and the log has since reached where it
    /// begins.
    /// With no catch-up that is a recording that began where the log ended (ADR
    /// 0093). In follower catch-up it is the live recording, which begins ahead of
    /// the log and counts once the replay's recording has reached its start (ADR
    /// 0094).
    fn carries_on_from(&self, end: i64) -> bool {
        self.listed && self.would_carry_on_from(end)
    }

    /// [`Self::carries_on_from`] but for the record of the log's recordings: what
    /// makes this recording one to list.
    fn would_carry_on_from(&self, end: i64) -> bool {
        self.checked && self.start >= self.floor && self.start <= end
    }

    /// This recording as the record of the log's recordings names it.
    fn as_listed(&self) -> ListedRecording {
        ListedRecording {
            recording_id: self.recording.recording_id(),
            start_position: self.start,
        }
    }
}

/// The image of the log on a follower's subscription, as the module noted it.
#[derive(Clone, Copy)]
enum LogImage {
    /// No image has joined the subscription yet.
    Awaited,
    /// The first image that joined the subscription, noted after the poll that applied
    /// it. Where this member's recording of it begins is the catalog's to say, not the
    /// image's (ADR 0093).
    Joined {
        /// The id the driver gave the image, which tells it from a later image of the
        /// same session.
        correlation_id: i64,
        session_id: i32,
    },
    /// The image noted is no longer available. No other image of the subscription is
    /// noted or read until the member leaves the log, so that the core sees the loss.
    Gone,
}

/// The core's [`Io`] over the client.
struct ClientIo {
    client: Cavalry,
    terms: TermsFile,
    peers: Vec<Peer>,
    /// The leader's log channel: the control channel on this member's `log` endpoint.
    log_control_channel: String,
    /// A follower's log channel: this member's `log` endpoint.
    log_endpoint_channel: String,
    scratch: Box<[u8; 1408]>,
    // The leader's log.
    log_publication: Option<Publication>,
    /// The term `log_publication` was created for.
    log_term: Option<i64>,
    destinations_added: usize,
    log_recording: Option<Recording>,
    /// The base `log_publication` was created at, where `log_recording` begins.
    log_base: i64,
    /// The recordings of the log this member stopped, in position order: the last
    /// [`KEPT_RECORDINGS`], allocated at the member's start.
    kept: VecDeque<Kept>,
    /// The member's record of its log's recordings, in its log directory: the ones
    /// kept and the ones being made that are part of the log (ADR 0108).
    recordings: RecordingsFile,
    /// Whether that record names `log_recording`, as [`Held::listed`].
    log_listed: bool,
    /// The replay this member started for each peer and has not yet said the end of,
    /// by the peer's place in `peers`.
    replays: Vec<Option<Replay>>,
    // A follower's log.
    log_subscription: Option<Subscription>,
    armed: Option<ArmedRecording>,
    /// A recording the driver has started and handed over whose start position has
    /// not yet been read from the catalog.
    starting: Option<Recording>,
    /// The image on the log subscription and where it joined.
    image: LogImage,
    held: Option<Held>,
    /// Where the member's log ended when it subscribed on its `log` endpoint: the
    /// floor of the recording armed there ([`Held::floor`]).
    log_floor: i64,
    /// The driver's catalog, in the member's log directory: where a recording of the
    /// log begins is read from it, once for each recording handed over (ADR 0093).
    catalog: CatalogReader,
    // A member's catch-up: its subscription on its `catchup` endpoint (ADR 0094).
    /// This member's id, which its catch-up position carries.
    member_id: i32,
    /// This member's `catchup` endpoint as a channel, and as the `host:port` its
    /// catch-up position carries.
    catchup_endpoint_channel: String,
    catchup_endpoint: String,
    catchup_subscription: Option<Subscription>,
    catchup_armed: Option<ArmedRecording>,
    /// As `starting`, for the recording armed on the catch-up subscription.
    catchup_starting: Option<Recording>,
    /// The recording of the replay's image the armed recording took.
    catchup_held: Option<Held>,
    /// The end of the contiguous span the recordings this member has stopped hold.
    stopped_at: i64,
    /// The position its recording of the log had written when this member last asked
    /// the recorder to sync it ([`ask_sync`]), `NO_SYNC_ASKED` from the moment the
    /// member takes a recording in hand: the mark is one recording's, and the next
    /// may write a position the one before it was asked at.
    sync_asked_at: i64,
    /// The same for its recording of a replay.
    catchup_sync_asked_at: i64,
    // The file of the log's recordings, for the member's service (ADR 0103).
    /// `log-recordings-<member id>` in the driver directory.
    recordings_path: PathBuf,
    /// The name the file is written under before it is renamed.
    recordings_tmp_path: PathBuf,
    /// The file's bytes, the longest file's length, allocated at the member's start.
    recordings_file: Vec<u8>,
    /// The id `cluster.log_recording_id` holds, as the last duty cycle left it: −1
    /// while the member names no recording. A truncation waits for −1 (ADR 0103).
    named: i64,
}

/// Asks the driver to sync `recording`, this member's recording of the log, when the
/// recorder has written past what it has synced, without waiting for the answer
/// (`spec/client-protocol.md`, "Commands", sync recording; ADR 0060, ADR 0069). The
/// member's append position is the recording's synced position, so without the
/// request an entry counts toward a commit at the recorder's next sync of its own,
/// up to a second later. It asks once for each position written that it sees, never
/// once per duty cycle: while the sync runs the recorder writes nothing, the next
/// block it takes holds everything that arrived meanwhile, and one request then
/// covers that block, so the syncs group the entries by themselves. A request the
/// driver refuses, its recorder's queue full, is not asked again until the position
/// written moves; the recorder's own sync covers what it left within a second.
fn ask_sync(client: &mut Cavalry, recording: &Recording, asked_at: &mut i64) {
    let written = recording.position();
    if written > recording.synced_position()
        && written != *asked_at
        && recording.request_sync(client).is_ok()
    {
        *asked_at = written;
    }
}

/// Stops `recording` and waits for its stop position, within [`STOP_TIMEOUT`]: where
/// this member's log ends when the recording began where the log ended before it. A
/// stop already read, the driver's at the stream's end or this member's own on a cycle
/// before, needs no command.
fn stop_recording(client: &mut Cavalry, recording: &Recording) -> Result<i64, RaftError> {
    if recording.stop_position().is_none() {
        client.stop_recording(recording.recording_id())?;
    }
    let deadline = Instant::now() + STOP_TIMEOUT;
    loop {
        if let Some(stop) = recording.stop_position() {
            return Ok(stop);
        }
        if Instant::now() >= deadline {
            return Err(RaftError::StopTimeout {
                recording_id: recording.recording_id(),
            });
        }
        std::thread::sleep(Duration::from_micros(50));
    }
}

/// Takes the recording the driver has started for an armed `start recording`, and
/// reads where it begins. That is the catalog's word, never the image's: an image
/// that arrived between this member's `add subscription` and its `start recording`
/// is recorded from what it had received by then, past its join position (ADR 0093).
/// The driver writes the descriptor before it says the recording started, so its
/// slot in the driver's catalog holds this recording by now. A slot that is empty, or
/// that holds a recording of another session or stream, is in a catalog that is not
/// the driver's, and its start position would be another recording's: it is
/// `unknown`, as a slot the catalog does not have is. A read that fails leaves the
/// recording waiting in `starting`, and the next cycle reads again. `floor` is where
/// the member's log ended when it subscribed.
fn take_started(
    armed: &mut Option<ArmedRecording>,
    starting: &mut Option<Recording>,
    catalog: &CatalogReader,
    floor: i64,
) -> Result<Option<Held>, RaftError> {
    if let Some(waiting) = armed.as_mut()
        && let Some(recording) = waiting.poll()?
    {
        *starting = Some(recording);
        *armed = None;
    }
    let Some(recording) = starting.take() else {
        return Ok(None);
    };
    let recording_id = recording.recording_id();
    let found = catalog.get(recording_id).and_then(|descriptor| {
        descriptor
            .filter(|descriptor| {
                descriptor.session_id() == recording.session_id()
                    && descriptor.stream_id() == recording.stream_id()
            })
            .ok_or(CatalogError::Unknown { recording_id })
    });
    match found {
        Ok(descriptor) => Ok(Some(Held {
            recording,
            start: descriptor.start_position(),
            floor,
            checked: false,
            listed: false,
        })),
        Err(error) => {
            *starting = Some(recording);
            Err(error.into())
        }
    }
}

/// Writes the member's record of its log's recordings (`spec/durable-log.md`, "Log
/// recordings"; ADR 0108): the recordings it keeps, in the order they begin, and after
/// them the ones it is making that are part of its log or are to be, `open`, its
/// recording of a replay and then its recording of the live log or of the log it
/// leads. With them goes the floor position: where the first of them begins, and
/// `end`, where the member's log ends, when there is none, which is a log wholly in
/// recordings the table has forgotten, or no log. One write and one sync, when a
/// recording joins the log or a truncation has cut it, and no allocation.
fn write_list(
    recordings: &mut RecordingsFile,
    kept: &VecDeque<Kept>,
    open: [Option<ListedRecording>; 2],
    end: i64,
) -> Result<(), RaftError> {
    let entries = kept
        .iter()
        .map(|kept| ListedRecording {
            recording_id: kept.recording_id,
            start_position: kept.start,
        })
        .chain(open.into_iter().flatten());
    let floor = entries
        .clone()
        .next()
        .map_or(end, |first| first.start_position);
    Ok(recordings.write(floor, entries)?)
}

/// What the catalog holds under the id of a recording the member's record of its
/// log's recordings names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cataloged {
    /// A valid recording: where it begins, its stream, and where it stopped, `None`
    /// for one still being made.
    Held {
        start: i64,
        stream_id: i32,
        stop: Option<i64>,
    },
    /// A recording the driver found damaged, state 2.
    Invalid,
    /// No recording: a slot the catalog does not have, an empty one, or one whose
    /// recording was purged, state 3.
    Missing,
}

/// The log a member kept, read back at its start (`spec/cluster.md`, "A member's
/// start"; ADR 0108): the recordings its record names, each carrying the log on from
/// where the ones before it end, the first from `floor`, the record's floor position.
/// Returns the table of them and where the log ends. `cataloged` says what the
/// catalog holds under a recording id, and `stop` stops a recording still being
/// made, the driver having run on while the member was away, and returns where it
/// stopped, so that a stop is read once and is final.
///
/// The catalog may lack the record's last recordings and no others: a truncation
/// purges a member's recordings from the last one back and writes the record after
/// them, so a member that stopped in the middle of one finds them missing, and its
/// log ends where the recordings the catalog holds end. With all of them missing
/// that is the floor. Everything else the catalog can hold in place of a recording
/// the record names is [`RaftError::ListedRecording`], and the member does not
/// start: a recording missing before one that is held, one the driver found
/// damaged, one that begins elsewhere or is of another stream, and one that begins
/// past where the ones before it end. A log read back shorter than the member
/// reported it would let the member vote for a candidate that lacks a committed
/// entry, so nothing here shortens the log but that one truncation.
///
/// Every recording is checked before any is stopped, so a start that is refused
/// for what a recording is has changed nothing. A recording that holds nothing is
/// left out of the table, as a truncation leaves it out. The table keeps the last
/// [`KEPT_RECORDINGS`], as [`ClientIo::keep`] does.
fn kept_log(
    floor: i64,
    listed: &[ListedRecording],
    mut cataloged: impl FnMut(i64) -> Result<Cataloged, RaftError>,
    mut stop: impl FnMut(i64) -> Result<i64, RaftError>,
) -> Result<(VecDeque<Kept>, i64), RaftError> {
    let refused = |entry: &ListedRecording, found| RaftError::ListedRecording {
        recording_id: entry.recording_id,
        listed_start: entry.start_position,
        found,
    };
    // What each recording is, before anything is stopped: `None` from the first
    // one missing.
    let mut stops: Vec<Option<i64>> = Vec::with_capacity(listed.len());
    let mut missing: Option<&ListedRecording> = None;
    for entry in listed {
        match cataloged(entry.recording_id)? {
            Cataloged::Missing => {
                missing.get_or_insert(entry);
            }
            Cataloged::Invalid => return Err(refused(entry, ListedFound::Invalid)),
            Cataloged::Held {
                start,
                stream_id,
                stop,
            } => {
                if let Some(missing) = missing {
                    return Err(refused(missing, ListedFound::Missing));
                }
                if start != entry.start_position || stream_id != LOG_STREAM_ID {
                    return Err(refused(
                        entry,
                        ListedFound::Other {
                            start_position: start,
                            stream_id,
                        },
                    ));
                }
                stops.push(stop);
            }
        }
    }
    let mut kept = VecDeque::with_capacity(KEPT_RECORDINGS);
    let mut end = floor;
    for (entry, stopped) in listed.iter().zip(stops) {
        if entry.start_position > end {
            return Err(refused(entry, ListedFound::Gap { end_position: end }));
        }
        let stop = match stopped {
            Some(stop) => stop,
            None => stop(entry.recording_id)?,
        };
        end = end.max(stop);
        if stop > entry.start_position {
            if kept.len() >= KEPT_RECORDINGS {
                kept.pop_front();
            }
            kept.push_back(Kept {
                recording_id: entry.recording_id,
                start: entry.start_position,
                stop,
            });
        }
    }
    Ok((kept, end))
}

/// What the driver's catalog holds under `recording_id`, for [`kept_log`].
fn cataloged(catalog: &CatalogReader, recording_id: i64) -> Result<Cataloged, RaftError> {
    match catalog.get(recording_id) {
        Ok(Some(descriptor)) => Ok(match descriptor.state() {
            State::Valid => Cataloged::Held {
                start: descriptor.start_position(),
                stream_id: descriptor.stream_id(),
                stop: (!descriptor.is_live()).then(|| descriptor.stop_position()),
            },
            State::Invalid => Cataloged::Invalid,
            State::Empty | State::Deleted => Cataloged::Missing,
        }),
        Ok(None) | Err(CatalogError::Unknown { .. }) => Ok(Cataloged::Missing),
        Err(error) => Err(error.into()),
    }
}

/// Stops a recording of the member's log that is still being made at the member's
/// start, and waits for its stop position in the catalog, within [`STOP_TIMEOUT`].
fn stop_listed(
    client: &mut Cavalry,
    catalog: &CatalogReader,
    recording_id: i64,
) -> Result<i64, RaftError> {
    match client.stop_recording(recording_id) {
        Ok(()) => {}
        // Stopped between the catalog's read and the command.
        Err(ClientError::Refused {
            code: Some(ErrorCode::UnknownRecording),
            ..
        }) => {}
        Err(error) => return Err(error.into()),
    }
    let deadline = Instant::now() + STOP_TIMEOUT;
    loop {
        if let Some(stop) = catalog.stop_position(recording_id)? {
            return Ok(stop);
        }
        if Instant::now() >= deadline {
            return Err(RaftError::StopTimeout { recording_id });
        }
        std::thread::sleep(Duration::from_micros(50));
    }
}

impl ClientIo {
    /// Makes the recordings that have come to carry the member's log on part of it
    /// (ADR 0108): its recording of a replay, once the election was told its image
    /// and it begins where the log ends, and its recording of the live log, once the
    /// election was told its image and the log has reached where it begins. The
    /// member's record of its log's recordings is written with them, synced, and only
    /// then do they count: [`Io::append_position`] reports nothing either holds
    /// before. A record that cannot be written leaves them out of the log, and the
    /// next duty cycle writes it again. Nothing is written in a cycle in which no
    /// recording joins.
    fn list_counted(&mut self) -> Result<(), RaftError> {
        let end = self.stopped_at;
        let joins = |held: &&Held| held.checked && held.start == end;
        let replay = self.catchup_held.as_ref().filter(joins);
        let reached = replay.map_or(end, |held| end.max(held.recording.synced_position()));
        let live = self
            .held
            .as_ref()
            .filter(|held| held.would_carry_on_from(reached));
        if !replay.is_some_and(|held| !held.listed) && !live.is_some_and(|held| !held.listed) {
            return Ok(());
        }
        let open = [replay.map(Held::as_listed), live.map(Held::as_listed)];
        let (replay, live) = (replay.is_some(), live.is_some());
        write_list(&mut self.recordings, &self.kept, open, end)?;
        if let Some(held) = self.catchup_held.as_mut().filter(|_| replay) {
            held.listed = true;
        }
        if let Some(held) = self.held.as_mut().filter(|_| live) {
            held.listed = true;
        }
        Ok(())
    }

    /// Where this member's log ends below its recording of the live log: the end of
    /// the recordings it has stopped, and past it what its recording of a replay has
    /// synced, once the election was told the replay's image and when that recording
    /// begins where the stopped ones end. Nothing is asked of the driver.
    fn end_below_live(&self) -> i64 {
        let end = self.stopped_at;
        self.catchup_held
            .as_ref()
            .filter(|held| held.checked && held.listed && held.start == end)
            .map_or(end, |held| end.max(held.recording.synced_position()))
    }

    /// The recording of the leader's image that is part of this member's log: the one
    /// held, once the election has been told its image and when it carries the log
    /// on from where the log ends below it (ADR 0093, ADR 0094).
    fn counted(&self) -> Option<&Held> {
        let end = self.end_below_live();
        self.held.as_ref().filter(|held| held.carries_on_from(end))
    }

    /// Stops this member's recording of a replay and takes its stop as the log's end
    /// when the election was told its image and it began where the log ended; the
    /// recording is then kept for a replay of this member's own. One the election was
    /// never told, or that began elsewhere, is stopped and is no part of the log. The
    /// recording stays held until its stop has been read, as in [`Io::leave_log`].
    fn settle_catchup(&mut self) -> Result<(), RaftError> {
        if let Some(held) = self.catchup_held.as_ref() {
            let stop = stop_recording(&mut self.client, &held.recording)?;
            let kept = Kept {
                recording_id: held.recording.recording_id(),
                start: held.start,
                stop,
            };
            if held.checked && held.listed && kept.start == self.stopped_at {
                self.stopped_at = stop;
                self.keep(kept);
            }
            self.catchup_held = None;
        }
        if let Some(recording) = self.catchup_starting.as_ref() {
            stop_recording(&mut self.client, recording)?;
            self.catchup_starting = None;
        }
        Ok(())
    }

    /// Takes what arrived on the catch-up subscription: applies the images the driver
    /// announced or withdrew, reads past what has arrived of the image this member's
    /// recording holds, so that the replay is not held back and its image can go when
    /// it has ended, and takes the armed recording once the driver has started it.
    /// The module acts on no entry in this version: the recording is the member's
    /// log. An image of another session, a replay this member's recording did not
    /// take, is never read.
    fn observe_catchup(&mut self) -> Result<(), RaftError> {
        let Some(subscription) = self.catchup_subscription.as_mut() else {
            return Ok(());
        };
        subscription.poll(|_, _| {}, 0).map_err(RaftError::Poll)?;
        if let Some(held) = self.catchup_held.as_ref() {
            subscription
                .bounded_poll(
                    held.recording.session_id(),
                    |_, _| {},
                    i64::MAX,
                    LOG_POLL_LIMIT,
                )
                .map_err(RaftError::Poll)?;
        }
        let floor = self.stopped_at;
        if let Some(held) = take_started(
            &mut self.catchup_armed,
            &mut self.catchup_starting,
            &self.catalog,
            floor,
        )? {
            self.catchup_held = Some(held);
            // The mark is of the recording before this one: a position this one
            // writes may equal it, and its sync must still be asked.
            self.catchup_sync_asked_at = NO_SYNC_ASKED;
        }
        Ok(())
    }

    /// Keeps a recording this member stopped, for the replays a leader serves. A full
    /// table forgets its oldest: that recording stays in the driver's catalog, and
    /// this member no longer replays it (ADR 0091). The record of the log's
    /// recordings names it until its next write, which is of the table as it then
    /// is; a member that starts meanwhile keeps the last [`KEPT_RECORDINGS`] the
    /// record names, as here (ADR 0108).
    fn keep(&mut self, kept: Kept) {
        if self.kept.len() >= KEPT_RECORDINGS {
            self.kept.pop_front();
        }
        self.kept.push_back(kept);
    }

    /// The recording this member records the live log into, with where it begins: the
    /// leader's of its publication, from the base it was created at, or a follower's
    /// of the leader's image once it is part of the member's log ([`Self::counted`]).
    fn live_recording(&self) -> Option<(i64, i64)> {
        match &self.log_recording {
            Some(recording) => Some((recording.recording_id(), self.log_base)),
            None => self
                .counted()
                .map(|held| (held.recording.recording_id(), held.start)),
        }
    }

    /// Writes the file of this member's log's recordings for the live recording
    /// `live_recording_id`, which begins at `live_start_position` (`spec/cluster.md`,
    /// "The log's recordings"; ADR 0103): the recordings the member stopped and
    /// keeps, in the order they begin, and after them its recording of a catch-up's
    /// replay while it still makes it, with what that recording has synced. One that
    /// holds nothing has no entry. The file is written whole under its other name
    /// and renamed, so that a service reads this file or the one before it. A write
    /// and a rename, four system calls, in the duty cycle in which the member names a
    /// recording, and again in each cycle while one of them fails. A write that
    /// succeeds allocates nothing: the bytes and both paths were made at the member's
    /// start. One that fails clones the path into its error.
    fn write_log_recordings(
        &mut self,
        live_recording_id: i64,
        live_start_position: i64,
    ) -> Result<(), RaftError> {
        let kept = self.kept.iter().map(|kept| LogRecording {
            recording_id: kept.recording_id,
            start_position: kept.start,
            stop_position: kept.stop,
        });
        let end = self.stopped_at;
        let replayed = self
            .catchup_held
            .as_ref()
            .filter(|held| held.checked && held.listed && held.start == end)
            .map(|held| LogRecording {
                recording_id: held.recording.recording_id(),
                start_position: held.start,
                stop_position: held.recording.synced_position(),
            });
        let entries = kept
            .chain(replayed)
            .filter(|entry| entry.stop_position > entry.start_position);
        let length = write_log_recordings(
            &mut self.recordings_file,
            self.member_id,
            live_recording_id,
            live_start_position,
            entries,
        )
        .map_err(|error| RaftError::LogRecordings {
            path: self.recordings_path.clone(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidData, error),
        })?;
        let bytes = self.recordings_file.get(..length).unwrap_or(&[]);
        std::fs::write(&self.recordings_tmp_path, bytes).map_err(|source| {
            RaftError::LogRecordings {
                path: self.recordings_tmp_path.clone(),
                source,
            }
        })?;
        std::fs::rename(&self.recordings_tmp_path, &self.recordings_path).map_err(|source| {
            RaftError::LogRecordings {
                path: self.recordings_path.clone(),
                source,
            }
        })
    }

    /// The recording that holds `position`, and where it stopped, `None` for the
    /// leader's live one: the recording kept that begins at or below the position and
    /// stopped above it, else the live recording of the log this member leads, when
    /// the position is at or above the base its publication was created at.
    fn recording_at(&self, position: i64) -> Option<(i64, Option<i64>)> {
        self.kept
            .iter()
            .find(|kept| kept.start <= position && position < kept.stop)
            .map(|kept| (kept.recording_id, Some(kept.stop)))
            .or_else(|| {
                self.log_recording
                    .as_ref()
                    .filter(|_| position >= self.log_base)
                    .map(|recording| (recording.recording_id(), None))
            })
    }

    /// Takes the images the driver announced or withdrew on the log subscription, notes
    /// a new image's join position before a frame of it is read, hands over an armed
    /// recording the driver has started, and reads past what has arrived of the image
    /// noted, so that the stream is not held back by a subscriber that stands still:
    /// the module acts on no entry in this version, and the recording is the member's
    /// log.
    ///
    /// An image is never read in the poll that applies it. The client's conductor runs
    /// on a thread of its own and hands an image over whenever it has read the driver's
    /// `available image`, between any two polls, and every poll applies what was handed
    /// over before it reads. A poll that read every image would read up to
    /// `LOG_POLL_LIMIT` frames of an image it had just applied, and the position
    /// noted on the next cycle would be past the image's join: a join position mismatch
    /// counted for a follower whose log had none, and a recording never counted, since
    /// it would not begin where it was noted to. So the poll with a fragment limit of 0
    /// applies and reads nothing, the image is noted after it, and the read is a bounded
    /// poll of the noted session alone, which reads no image of another session that
    /// the conductor hands over meanwhile. An image of the same session that the
    /// conductor hands over in place of the noted one, which that read may read, is
    /// never noted: the noted image is gone by the next cycle, and nothing is noted on
    /// the subscription after it.
    fn observe_log(&mut self) -> Result<(), RaftError> {
        let Some(subscription) = self.log_subscription.as_mut() else {
            return Ok(());
        };
        subscription.poll(|_, _| {}, 0).map_err(RaftError::Poll)?;
        self.image = match self.image {
            LogImage::Awaited => subscription
                .images()
                .first()
                .map_or(LogImage::Awaited, |image| LogImage::Joined {
                    correlation_id: image.correlation_id(),
                    session_id: image.session_id(),
                }),
            LogImage::Joined { correlation_id, .. }
                if subscription
                    .images()
                    .iter()
                    .any(|image| image.correlation_id() == correlation_id) =>
            {
                self.image
            }
            LogImage::Joined { .. } | LogImage::Gone => LogImage::Gone,
        };
        let LogImage::Joined { session_id, .. } = self.image else {
            return Ok(());
        };
        subscription
            .bounded_poll(session_id, |_, _| {}, i64::MAX, LOG_POLL_LIMIT)
            .map_err(RaftError::Poll)?;
        if let Some(held) = take_started(
            &mut self.armed,
            &mut self.starting,
            &self.catalog,
            self.log_floor,
        )? {
            self.held = Some(held);
            // The mark is of the recording before this one: a position this one
            // writes may equal it, and its sync must still be asked.
            self.sync_asked_at = NO_SYNC_ASKED;
        }
        Ok(())
    }
}

impl Io for ClientIo {
    type Fault = RaftError;

    fn send(&mut self, member: i32, message: &Message<'_>) -> bool {
        let Some(peer) = self.peers.iter_mut().find(|peer| peer.id == member) else {
            return false;
        };
        let mut buffer = [0u8; 128];
        let Ok(length) = message.write(&mut buffer) else {
            return false;
        };
        buffer
            .get(..length)
            .is_some_and(|bytes| peer.consensus.offer(bytes).is_ok())
    }

    fn store_candidate_term(&mut self, term: i64) -> Result<(), Self::Fault> {
        Ok(self.terms.set_candidate_term(term)?)
    }

    fn record_term(&mut self, term: i64, base: i64) -> Result<(), Self::Fault> {
        Ok(self.terms.append(term, base, NO_RECORDING)?)
    }

    /// The `terms` file's stored candidate term as the module holds it in memory. The
    /// core reads it once, in its first init (ADR 0089): −1 on a new log directory,
    /// and what the member stored on one it kept (ADR 0108).
    fn candidate_term(&mut self) -> i64 {
        self.terms.candidate_term()
    }

    /// The highest term of the `terms` file, from memory, as [`Io::candidate_term`].
    fn log_leadership_term_id(&mut self) -> i64 {
        self.terms.log_leadership_term_id()
    }

    fn add_log(&mut self, term: i64, base: i64) -> Result<Option<Log>, Self::Fault> {
        // The publication held is the one of the term it was created for, at that
        // term's base: a call for the same term is a step tried again, and one for
        // another term would append that term's entries where the earlier term's
        // publication stands. A member that led an earlier term in this process gives
        // its publication and its recording up before it leads again, in init, by
        // `remove_log` (ADR 0082, ADR 0091).
        if self.log_term.is_some_and(|held| held != term) {
            return Err(RaftError::Unsupported {
                what: "a term led by a member that still holds the log publication of an \
                       earlier term",
            });
        }
        if self.log_publication.is_none() {
            // A term's publication is created at the term's base, so that every frame
            // of the term lies at its position in the cluster's log, and the recording
            // started below begins there (`spec/cluster.md`, state 5; ADR 0059).
            let Some(channel) = log_channel_at(&self.log_control_channel, base) else {
                return Err(RaftError::Unsupported {
                    what: "a term whose base no publication's position keys can name",
                });
            };
            self.log_publication = Some(
                self.client
                    .add_exclusive_publication(&channel, LOG_STREAM_ID)?,
            );
            self.log_term = Some(term);
            self.log_base = base;
        }
        let Some(publication) = self.log_publication.as_ref() else {
            return Ok(None);
        };
        while let Some(peer) = self.peers.get(self.destinations_added) {
            self.client
                .add_destination(publication.registration_id(), &peer.log_destination)?;
            self.destinations_added += 1;
        }
        if self.log_recording.is_none() {
            self.log_recording = Some(
                self.client
                    .start_recording(&self.log_control_channel, LOG_STREAM_ID)?,
            );
            self.sync_asked_at = NO_SYNC_ASKED;
            self.log_listed = false;
        }
        // The recording is part of the member's log once the record of the log's
        // recordings names it, and the leader appends nothing before (ADR 0108). A
        // record that could not be written returns here, and the step is tried again.
        if !self.log_listed
            && let Some(recording) = self.log_recording.as_ref()
        {
            let own = ListedRecording {
                recording_id: recording.recording_id(),
                start_position: self.log_base,
            };
            write_list(
                &mut self.recordings,
                &self.kept,
                [None, Some(own)],
                self.stopped_at,
            )?;
            self.log_listed = true;
        }
        Ok(self.log_recording.as_ref().map(|recording| Log {
            session_id: publication.session_id(),
            recording_id: recording.recording_id(),
        }))
    }

    fn join_log(&mut self) -> Result<(), Self::Fault> {
        if self.log_subscription.is_none() {
            // Where the log ends now: the image that joins this subscription joins
            // at or past it, and its recording must not begin below it.
            self.log_floor = self.end_below_live();
            self.log_subscription = Some(
                self.client
                    .add_subscription(&self.log_endpoint_channel, LOG_STREAM_ID)?,
            );
        }
        if self.armed.is_none() && self.starting.is_none() && self.held.is_none() {
            self.armed = Some(
                self.client
                    .arm_recording(&self.log_endpoint_channel, LOG_STREAM_ID)?,
            );
        }
        Ok(())
    }

    /// Where this member's recording of the image with `session_id` begins, which is
    /// the image's join position as the election is to check it (ADR 0093): `None`
    /// until that image is on the subscription and the driver has started this
    /// member's recording of it, and again once the image is no longer available. A
    /// recording that began past the image's join, the image having arrived before
    /// the recording was armed, is reported where it begins, so that the election
    /// finds an image that joined past the member's append position. A recording of
    /// another session's image is never reported. Reporting a recording is what makes
    /// it part of the member's log: [`Io::append_position`] counts none before.
    fn log_image(&mut self, session_id: i32) -> Result<Option<i64>, Self::Fault> {
        let LogImage::Joined {
            session_id: noted, ..
        } = self.image
        else {
            return Ok(None);
        };
        if noted != session_id {
            return Ok(None);
        }
        Ok(self
            .held
            .as_mut()
            .filter(|held| held.recording.session_id() == session_id)
            .map(|held| {
                held.checked = true;
                held.start
            }))
    }

    /// Stops the recording held and takes its stop as the log's end, then drops the
    /// subscription. The recording stays held until its stop has been read: a stop the
    /// driver refused or did not answer, or a stop position not read within
    /// `STOP_TIMEOUT`, returns with nothing else changed. The core then stays in init
    /// with the log joined and calls again on the next cycle, which stops the recording
    /// again, a success with nothing written for one already stopped, and waits again.
    /// So a leave that fails never loses the recording whose stop is the log's end, and
    /// it never takes a synced position short of the stop.
    fn leave_log(&mut self) -> Result<(), Self::Fault> {
        // A member that leaves in follower catch-up holds two recordings, and the
        // live one begins inside what the replay's holds: the replay's recording is
        // stopped first, so that the live one is kept after it and the log's end is
        // the further of the two stops (ADR 0094). Init removes the catch-up
        // subscription next.
        self.settle_catchup()?;
        if let Some(held) = self.held.as_ref() {
            let stop = stop_recording(&mut self.client, &held.recording)?;
            let kept = Kept {
                recording_id: held.recording.recording_id(),
                start: held.start,
                stop,
            };
            // The recording's stop is where this member's log ends when the election
            // was told its image and the recording carries the log on: it began where
            // the log ended, or, in a catch-up, ahead of it and no further than the
            // replay's recording reached. One that began past that, or whose image
            // the election never asked for, is no part of the member's contiguous
            // log, and is not kept for a replay.
            if held.carries_on_from(self.stopped_at) {
                self.stopped_at = self.stopped_at.max(stop);
                self.keep(kept);
            }
            self.held = None;
        }
        // A recording handed over whose start was never read is stopped the same way,
        // and is no part of the log: the election was never told its image.
        if let Some(recording) = self.starting.as_ref() {
            stop_recording(&mut self.client, recording)?;
            self.starting = None;
        }
        // The driver drops a recording still armed with the stream's last subscription.
        self.armed = None;
        self.log_subscription = None;
        self.image = LogImage::Awaited;
        Ok(())
    }

    /// Gives up the log of the term this member led (ADR 0082, ADR 0091): stops its
    /// recording and takes the stop as the log's end, as a follower that leaves the
    /// log does, then closes the publication and forgets the term it was for, so that
    /// the next term the member leads has a publication of its own at that term's
    /// base. What the leader appended and its recorder had not written is not in its
    /// log: it never reported it. The recording stays held until its stop has been
    /// read, so a stop the driver refused or did not answer in time returns with
    /// nothing else changed, and init calls again on the next cycle. The core asks
    /// it of a member that led and holds an election, in init, and not again once it
    /// returned `Ok`.
    fn remove_log(&mut self) -> Result<(), Self::Fault> {
        if let Some(recording) = self.log_recording.as_ref() {
            let stop = stop_recording(&mut self.client, recording)?;
            let kept = Kept {
                recording_id: recording.recording_id(),
                start: self.log_base,
                stop,
            };
            self.log_recording = None;
            // A leader's recording begins at its term's base, which was its append
            // position, the end of its log, when it began to lead. One the record of
            // the log's recordings never named is no part of the log: nothing was
            // appended to it.
            if kept.start == self.stopped_at && self.log_listed {
                self.stopped_at = stop;
                self.keep(kept);
            }
            self.log_listed = false;
        }
        if let Some(publication) = self.log_publication.take() {
            publication.close()?;
        }
        self.log_term = None;
        self.destinations_added = 0;
        Ok(())
    }

    /// Truncates the member's log to `position` (ADR 0083, ADR 0095): of the
    /// recordings it keeps, the ones it stopped, `purge recording` on each that
    /// begins above the position and `truncate recording` on each that begins at or
    /// below it and stopped above it, which with a catch-up can be two that overlap
    /// (ADR 0094); then the `terms` file's `truncate_above`, each write synced. The
    /// kept table is cut with each command, so that no replay of this member's own
    /// brings entries its log no longer holds, and a recording left holding nothing
    /// is forgotten. Then the log ends at the position, and the highest term the
    /// record holds is returned.
    ///
    /// Every command waits for the driver's answer, so the truncation is one step
    /// and `Ok(None)` is returned in two cases alone. The member still holds a
    /// recording of the log or of a replay, which the core never asks of. Or
    /// `cluster.log_recording_id` still holds the id of the recording the member
    /// left, which the duty cycle that left it stores −1 over: a service takes the
    /// member's commit position as its bound while the counter names a recording, and
    /// no recording of the log is cut under that bound (ADR 0103). A command
    /// that faults returns with what was cut so far cut in the table too, and the
    /// leader's next new leadership term asks again: a purge of a recording already
    /// purged, or a truncation to where a recording already stops, changes nothing.
    /// The core asks it of a member that is out of the log, with a position below its
    /// append position and at or above the commit position it knows (ADR 0083).
    fn truncate_log(&mut self, position: i64) -> Result<Option<i64>, Self::Fault> {
        if self.log_recording.is_some()
            || self.held.is_some()
            || self.starting.is_some()
            || self.catchup_held.is_some()
            || self.catchup_starting.is_some()
            || self.named != NO_RECORDING
        {
            return Ok(None);
        }
        // The table is in the order its recordings begin, so the ones that begin
        // above the position are at its end.
        while let Some(last) = self.kept.back().copied()
            && last.start > position
        {
            match self.client.purge_recording(last.recording_id) {
                Ok(()) => {}
                // Purged by a call before this one whose answer was not read.
                Err(ClientError::Refused {
                    code: Some(ErrorCode::UnknownRecording),
                    ..
                }) => {}
                Err(error) => return Err(error.into()),
            }
            self.kept.pop_back();
        }
        for index in 0..self.kept.len() {
            let Some(kept) = self.kept.get(index).copied() else {
                break;
            };
            if kept.stop > position {
                self.client
                    .truncate_recording(kept.recording_id, position)?;
                if let Some(slot) = self.kept.get_mut(index) {
                    slot.stop = position;
                }
            }
        }
        // A recording cut to where it begins holds nothing to replay.
        self.kept.retain(|kept| kept.stop > kept.start);
        // The record of the log's recordings, without what was purged, before the
        // record of terms: a member that stops between the two starts again with a
        // term based above the end of its log, which it then deletes (ADR 0108). With
        // no recording left in the table, the record's floor says where the log ends.
        let end = self.stopped_at.min(position);
        write_list(&mut self.recordings, &self.kept, [None, None], end)?;
        self.terms.truncate_above(position)?;
        self.stopped_at = end;
        Ok(Some(self.terms.log_leadership_term_id()))
    }

    fn append_position(&mut self) -> i64 {
        if let Some(recording) = self.log_recording.as_ref().filter(|_| self.log_listed) {
            ask_sync(&mut self.client, recording, &mut self.sync_asked_at);
            return recording.synced_position();
        }
        // The contiguous end of what this member's recordings hold: the ones it has
        // stopped, then its recording of a replay, then its recording of the live
        // log once the log has reached where that one begins (ADR 0094).
        let mut end = self.stopped_at;
        if let Some(held) = self
            .catchup_held
            .as_ref()
            .filter(|held| held.checked && held.listed && held.start == end)
        {
            ask_sync(
                &mut self.client,
                &held.recording,
                &mut self.catchup_sync_asked_at,
            );
            end = end.max(held.recording.synced_position());
        }
        if let Some(held) = self.held.as_ref().filter(|held| held.carries_on_from(end)) {
            ask_sync(&mut self.client, &held.recording, &mut self.sync_asked_at);
            end = end.max(held.recording.synced_position());
        }
        end
    }

    fn terms_after(&mut self, term: i64) -> [Option<(i64, i64)>; 2] {
        // The `terms` file's entries as it holds them in memory: nothing is read from
        // the disk here.
        terms_above(
            self.terms
                .terms()
                .map(|entry| (entry.leadership_term_id, entry.term_base_log_position)),
            term,
        )
    }

    /// From the `terms` file's entries in memory, as [`Io::terms_after`] reads them.
    fn term_at_or_below(&mut self, term: i64) -> i64 {
        term_at_or_below(
            self.terms
                .terms()
                .map(|entry| (entry.leadership_term_id, entry.term_base_log_position)),
            term,
        )
    }

    /// Deletes every term above `term` from the `terms` file, each write synced
    /// (`spec/durable-log.md`; ADR 0096). The core asks it of a member that is out of
    /// the log, after it has truncated what it held above the base of the lowest of
    /// those terms.
    fn drop_terms_above(&mut self, term: i64) -> Result<i64, Self::Fault> {
        Ok(self.terms.drop_above_term(term)?)
    }

    fn term_base_above(&mut self, position: i64) -> Option<i64> {
        // From the same entries in memory, as `terms_after` reads them.
        base_above(
            self.terms
                .terms()
                .map(|entry| (entry.leadership_term_id, entry.term_base_log_position)),
            position,
        )
    }

    /// Replays the recording that holds `position` to `member`, on stream 100 at the
    /// member's `catchup` endpoint as this member's members string names it (ADR 0085,
    /// ADR 0091). With `end`, a replication's, it runs to `end` or to the recording's
    /// stop, whichever comes first. With none, a catch-up's, it follows the live
    /// recording of the term this member leads for as long as it grows, and a
    /// position in a recording that has stopped starts nothing: a catch-up is within
    /// the leader's term (ADR 0087). `false`, with nothing started, when no recording
    /// this member keeps holds the position, or the driver no longer holds the
    /// recording or the position.
    ///
    /// A replay still held for the member is stopped first, and the new one starts
    /// once it has stopped, the order [`Io::start_replay`] leaves to the I/O: the
    /// driver answers `stop replay` once its replayer has written the replay's last
    /// frame (`spec/client-protocol.md`, "Commands"), and this waits for that answer,
    /// as the commands of [`Io::add_log`] wait for theirs. How the earlier replay
    /// ended is forgotten. The core asks it of a leader that has read a member's
    /// catch-up position, and holds at most one replay a member (ADR 0085).
    fn start_replay(
        &mut self,
        member: i32,
        position: i64,
        end: Option<i64>,
    ) -> Result<bool, Self::Fault> {
        let Some(index) = self.peers.iter().position(|peer| peer.id == member) else {
            return Ok(false);
        };
        let Some((recording_id, stop)) = self.recording_at(position) else {
            return Ok(false);
        };
        let length = match (end, stop) {
            (Some(end), Some(stop)) => end.min(stop).saturating_sub(position),
            (Some(end), None) => end.saturating_sub(position),
            // A catch-up follows the recording of the term this member leads. One
            // that has stopped is of an earlier term, and a member behind it
            // replicates, with an end.
            (None, Some(_)) => return Ok(false),
            (None, None) => FOLLOW_LENGTH,
        };
        if length <= 0 {
            return Ok(false);
        }
        self.stop_replay(member)?;
        let (Some(peer), Some(slot)) = (self.peers.get(index), self.replays.get_mut(index)) else {
            return Ok(false);
        };
        match self.client.start_replay(
            recording_id,
            position,
            length,
            LOG_STREAM_ID,
            &peer.catchup_channel,
        ) {
            Ok(replay) => {
                *slot = Some(replay);
                Ok(true)
            }
            // The catalog no longer holds the recording, or the recording no longer
            // holds the position, purged by retention: nothing to replay, as for a
            // recording this member has forgotten (`spec/client-protocol.md`,
            // "Commands", codes 21 and 23).
            Err(ClientError::Refused {
                code: Some(ErrorCode::UnknownRecording | ErrorCode::PositionOutOfRange),
                ..
            }) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    /// Stops the replay held for `member`, when it has not ended: `stop replay`, which
    /// waits for the driver's answer, sent once the replayer has written the replay's
    /// last frame. A second stop of one replay is answered at once. Nothing for a
    /// member with no replay.
    fn stop_replay(&mut self, member: i32) -> Result<(), Self::Fault> {
        let Some(index) = self.peers.iter().position(|peer| peer.id == member) else {
            return Ok(());
        };
        if let Some(Some(replay)) = self.replays.get(index)
            && replay.ended().is_none()
        {
            replay.stop(&mut self.client)?;
        }
        Ok(())
    }

    /// How the replay held for `member` ended, once the driver has said so, handed
    /// over once: the position it reached, and whether it ended by itself at its end.
    /// One that had no subscriber for the replay connect timeout, or failed, did not.
    fn replay_stopped(&mut self, member: i32) -> Result<Option<ReplayStopped>, Self::Fault> {
        let Some(slot) = self
            .peers
            .iter()
            .position(|peer| peer.id == member)
            .and_then(|index| self.replays.get_mut(index))
        else {
            return Ok(None);
        };
        let Some((position, reason)) = slot.as_ref().and_then(Replay::ended) else {
            return Ok(None);
        };
        *slot = None;
        Ok(Some(ReplayStopped {
            position,
            ended: reason == Some(ReplayStopReason::Ended),
        }))
    }

    /// Subscribes to stream 100 on this member's `catchup` endpoint and arms a
    /// recording on it (ADR 0086, ADR 0094). The core calls it with no catch-up
    /// subscription held and before it asks its leader for a replay (ADR 0092), so
    /// the stream has no image when the recording is armed, and the replay's image
    /// is recorded from its join position. A call that failed at its second command
    /// is tried again and sends that one alone. The core asks it of a member behind
    /// its leader: once for each replay of a replication, and once for a catch-up.
    fn add_catchup(&mut self) -> Result<(), Self::Fault> {
        if self.catchup_subscription.is_none() {
            self.catchup_subscription = Some(
                self.client
                    .add_subscription(&self.catchup_endpoint_channel, LOG_STREAM_ID)?,
            );
        }
        if self.catchup_armed.is_none()
            && self.catchup_starting.is_none()
            && self.catchup_held.is_none()
        {
            self.catchup_armed = Some(
                self.client
                    .arm_recording(&self.catchup_endpoint_channel, LOG_STREAM_ID)?,
            );
        }
        Ok(())
    }

    /// Sends catch-up position to `leader` with this member's id and its own
    /// `catchup` endpoint, `host:port` as its members string has it. The message is
    /// encoded into the module's scratch, 1,408 bytes, since an endpoint with a long
    /// host name does not fit the 128 bytes the other consensus messages are encoded
    /// into. `false` when the leader is no member, or its publication did not take
    /// the message: the core sends again on a later cycle.
    fn send_catchup_position(&mut self, leader: i32, term: i64, position: i64) -> bool {
        let message = Message::CatchupPosition {
            leadership_term_id: term,
            log_position: position,
            follower_member_id: self.member_id,
            catchup_endpoint: self.catchup_endpoint.as_bytes(),
        };
        let Some(peer) = self.peers.iter_mut().find(|peer| peer.id == leader) else {
            return false;
        };
        let Ok(length) = message.write(self.scratch.as_mut_slice()) else {
            return false;
        };
        self.scratch
            .get(..length)
            .is_some_and(|bytes| peer.consensus.offer(bytes).is_ok())
    }

    /// Where this member's recording of the replay's image begins, which is the
    /// image's join position as the election is to check it (ADR 0093, ADR 0094):
    /// `None` before the driver has started the recording armed on the catch-up
    /// subscription, and again once that recording has stopped, which it does when
    /// its image goes, the replay having ended or been stopped. A recording is
    /// reported once at least, so that a replay short enough to come and go between
    /// two duty cycles is still checked, counted and seen to have gone. Reporting it
    /// is what makes it part of the member's log.
    fn catchup_image(&mut self) -> Result<Option<i64>, Self::Fault> {
        let Some(held) = self.catchup_held.as_mut() else {
            return Ok(None);
        };
        if held.checked && held.recording.stop_position().is_some() {
            return Ok(None);
        }
        held.checked = true;
        Ok(Some(held.start))
    }

    /// Stops this member's recording of the replay, whose stop is where its log ends
    /// when the election was told its image and it began where the log ended, then
    /// removes the catch-up subscription. A stop the driver did not answer in time
    /// returns with the recording still held, and the core calls again.
    fn remove_catchup(&mut self) -> Result<(), Self::Fault> {
        self.settle_catchup()?;
        // The driver drops a recording still armed with the stream's last subscription.
        self.catchup_armed = None;
        self.catchup_subscription = None;
        Ok(())
    }

    fn append(&mut self, entry: &Message<'_>) -> Result<bool, Self::Fault> {
        let Some(publication) = self.log_publication.as_mut() else {
            return Ok(false);
        };
        let Ok(length) = entry.write(self.scratch.as_mut_slice()) else {
            return Ok(false);
        };
        let Some(bytes) = self.scratch.get(..length) else {
            return Ok(false);
        };
        match publication.offer(bytes) {
            Ok(_) => Ok(true),
            Err(Offer::BackPressured | Offer::NotConnected | Offer::AdminAction) => Ok(false),
            Err(other) => Err(RaftError::Client(ClientError::Driver {
                what: format!("the log publication refused an entry: {other}"),
            })),
        }
    }
}

/// The channel of a log publication whose first frame lies at `base`: `control`, the
/// member's log control channel, as it is for a base of 0, where a publication's log
/// begins unless its channel says otherwise, and for any other base `control` with
/// the three position keys, an `init-term-id` of 0, the whole terms below `base` as
/// `term-id` and the rest of `base` as `term-offset` (`spec/udp.md`, "The sender";
/// ADR 0059). `None` for a base below 0 and for one whose terms are more than a
/// `term-id` holds, 2,147,483,647 terms of 16 MiB.
fn log_channel_at(control: &str, base: i64) -> Option<String> {
    if base == 0 {
        return Some(control.to_owned());
    }
    if base < 0 {
        return None;
    }
    let term_length = i64::from(UDP_TERM_LENGTH);
    let term_id = i32::try_from(base.checked_div(term_length)?).ok()?;
    let term_offset = base.checked_rem(term_length)?;
    Some(format!(
        "{control}|init-term-id=0|term-id={term_id}|term-offset={term_offset}"
    ))
}

/// One of the module's counters with the value it last stored.
struct Stored {
    counter: Counter,
    last: i64,
}

impl Stored {
    /// Adds the counter of type `id` for `member_id` and stores `value` in it at once,
    /// so that the 0 a counter holds between its allocation and its first store is
    /// never read as a value: term 0 is a real term, and recording 0 a real recording.
    fn add(
        client: &mut Cavalry,
        id: CounterId,
        name: &str,
        member_id: i32,
        value: i64,
    ) -> Result<Self, ClientError> {
        let counter = client.add_counter(
            id.0,
            &member_id.to_le_bytes(),
            &format!("{name} {member_id}"),
        )?;
        counter.store_release(value)?;
        Ok(Self {
            counter,
            last: value,
        })
    }

    /// Stores `value` when it is not the value last stored: a release store, which a
    /// reader of a position loads with acquire. Whether the counter holds `value`
    /// now: not when the client is closed, which stores nothing.
    fn store(&mut self, value: i64) -> bool {
        if value != self.last && self.counter.store_release(value).is_ok() {
            self.last = value;
        }
        self.last == value
    }
}

/// A count as a counter's value.
fn count_value(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// The member's role as `cluster.role` holds it: 0 follower, 1 candidate, 2 leader.
fn role_code(core: &Consensus) -> i64 {
    i64::from(core.role() as u8)
}

/// The member's counters in the counters file (`spec/cluster.md`, "Counters").
struct Counters {
    role: Stored,
    election_state: Stored,
    leadership_term_id: Stored,
    /// `cluster.leader_member_id`: the member this one leads as or follows, −1 while
    /// it has none (ADR 0102).
    leader_member_id: Stored,
    /// `cluster.election_cause`: why its last election began (ADR 0102).
    election_cause: Stored,
    append_position: Stored,
    /// `cluster.log_recording_id`, added before `cluster.commit_position` and stored
    /// before it (`spec/cluster.md`, "The service").
    log_recording_id: Stored,
    commit_position: Stored,
    /// The core's counts, one counter each, in the order of [`Counts::by_counter`]:
    /// [`Counts::LEN`] of them, added once when the module starts.
    counts: Vec<Stored>,
    messages_rejected: Stored,
    /// `cluster.log_recordings_unwritten`: the duty cycles in which the file of the
    /// log's recordings could not be written (ADR 0103).
    log_recordings_unwritten: Stored,
}

impl Counters {
    /// Adds the member's counters, each holding what `core` holds from its first
    /// store, which follows its add at once. `cluster.log_recording_id` is added and
    /// holds −1 before `cluster.commit_position` is added: a service reads the
    /// recording id only of a member whose commit position it has found
    /// (`spec/cluster.md`, "The service").
    fn add(client: &mut Cavalry, member_id: i32, core: &Consensus) -> Result<Self, ClientError> {
        let mut add = |id, name, value| Stored::add(client, id, name, member_id, value);
        Ok(Self {
            role: add(CLUSTER_ROLE, "cluster.role", role_code(core))?,
            election_state: add(
                CLUSTER_ELECTION_STATE,
                "cluster.election_state",
                i64::from(core.election_state().code()),
            )?,
            leadership_term_id: add(
                CLUSTER_LEADERSHIP_TERM_ID,
                "cluster.leadership_term_id",
                core.leadership_term_id(),
            )?,
            leader_member_id: add(
                CLUSTER_LEADER_MEMBER_ID,
                "cluster.leader_member_id",
                core.leader_gauge(),
            )?,
            election_cause: add(
                CLUSTER_ELECTION_CAUSE,
                "cluster.election_cause",
                i64::from(core.election_cause().code()),
            )?,
            append_position: add(
                CLUSTER_APPEND_POSITION,
                "cluster.append_position",
                core.append_position(),
            )?,
            log_recording_id: add(
                CLUSTER_LOG_RECORDING_ID,
                "cluster.log_recording_id",
                NO_RECORDING,
            )?,
            commit_position: add(
                CLUSTER_COMMIT_POSITION,
                "cluster.commit_position",
                core.commit_position(),
            )?,
            counts: {
                // Each count's counter, by the id the core pairs it with and under the
                // name `counters.toml` gives that id (ADR 0067). Every id of
                // `by_counter` is an entry of that table, which its doctest checks,
                // so the other name is never a label.
                let mut counts = Vec::with_capacity(Counts::LEN);
                for (id, value) in core.counts().by_counter() {
                    let name = COUNTERS
                        .iter()
                        .find(|entry| entry.id == id)
                        .map_or("cluster.count", |entry| entry.name);
                    counts.push(add(id, name, count_value(value))?);
                }
                counts
            },
            messages_rejected: add(CLUSTER_MESSAGES_REJECTED, "cluster.messages_rejected", 0)?,
            log_recordings_unwritten: add(
                CLUSTER_LOG_RECORDINGS_UNWRITTEN,
                "cluster.log_recordings_unwritten",
                0,
            )?,
        })
    }

    /// Stores what the core holds, each value only when it has moved, and
    /// `log_recording_id`, the recording the member records the live log into or −1.
    /// The recording id is stored before the commit position, and a commit position
    /// only once the counter holds that id, so that no commit position is stored
    /// beside a recording id that does not hold it (`spec/cluster.md`, "The service").
    /// Returns the recording id the counter holds after it. `module` is the module's
    /// own two counts: the consensus messages it rejected, and the duty cycles in
    /// which it could not write the file of its log's recordings.
    fn store(&mut self, core: &Consensus, module: [u64; 2], log_recording_id: i64) -> i64 {
        let [messages_rejected, log_recordings_unwritten] = module;
        let counts: Counts = core.counts();
        self.role.store(role_code(core));
        self.election_state
            .store(i64::from(core.election_state().code()));
        self.leadership_term_id.store(core.leadership_term_id());
        self.leader_member_id.store(core.leader_gauge());
        self.election_cause
            .store(i64::from(core.election_cause().code()));
        self.append_position.store(core.append_position());
        if self.log_recording_id.store(log_recording_id) {
            self.commit_position.store(core.commit_position());
        }
        for (stored, (_, value)) in self.counts.iter_mut().zip(counts.by_counter()) {
            stored.store(count_value(value));
        }
        self.messages_rejected.store(count_value(messages_rejected));
        self.log_recordings_unwritten
            .store(count_value(log_recordings_unwritten));
        self.log_recording_id.last
    }
}

/// One cluster member's consensus module, a client of the driver in its directory.
pub struct ConsensusModule {
    core: Consensus,
    consensus: Subscription,
    io: ClientIo,
    counters: Counters,
    messages_rejected: u64,
    /// The duty cycles in which the file of the log's recordings could not be
    /// written, `cluster.log_recordings_unwritten` (ADR 0103).
    log_recordings_unwritten: u64,
    started: Instant,
    /// The clock of the last duty cycle, which an offer between two cycles is stamped
    /// with: the clock is read once a cycle and never per entry.
    clock: Clock,
}

impl ConsensusModule {
    /// Starts the member with id `member_id` of the cluster `members` names, the
    /// members string of `spec/cluster.md`, "Members and streams", as a client of the
    /// driver in `dir`, with its record of terms in `log_dir`, the driver's log
    /// directory. It subscribes to the consensus stream on its `consensus` endpoint,
    /// adds a publication to every other member's, and adds its counters; the log comes
    /// with its election.
    ///
    /// A log directory with no catalog is [`RaftError::Catalog`], and nothing is
    /// written into it: `log_dir` must be the directory the driver in `dir` was started
    /// with, whose catalog says where each of the member's recordings of the log
    /// begins (ADR 0093).
    ///
    /// On the log directory it kept the member takes up its log (`spec/cluster.md`, "A
    /// member's start"; ADR 0089, ADR 0108): the candidate term it stored, its record
    /// of terms, and the recordings its record of the log's recordings names, each
    /// with the stop the catalog holds for it, which is where its log ends. A term
    /// the record of terms holds above that end is deleted: a truncation the member's
    /// stop cut short. Nothing else shortens the log a member starts on. A directory
    /// of another member's, a record of terms that holds a term beside no record of
    /// the log's recordings, which a version before ADR 0108 left
    /// ([`RaftError::Recordings`]), a log beside a record of terms that holds no
    /// term ([`RaftError::TermsLost`]), and a catalog that does not hold the
    /// recordings the record names ([`RaftError::ListedRecording`]) are each refused
    /// with what to do, and the member does not start.
    pub fn start(
        dir: impl AsRef<Path>,
        log_dir: impl AsRef<Path>,
        members: &str,
        member_id: i32,
    ) -> Result<Self, RaftError> {
        let mut parsed = [EMPTY_MEMBER; MAX_MEMBERS];
        let count = parse_members(members.as_bytes(), &mut parsed, Some(member_id))
            .map_err(RaftError::Members)?;
        let parsed = parsed.get(..count).unwrap_or(&[]);
        let mut ids = [0i32; MAX_MEMBERS];
        for (slot, member) in ids.iter_mut().zip(parsed) {
            *slot = member.id;
        }
        // The seed of the member's nomination delays: the operating system's, through
        // the standard library's `RandomState`, mixed with the member id, so that two
        // members never draw one sequence (ADR 0077).
        let seed = {
            use std::hash::{BuildHasher, Hasher};
            let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
            hasher.write_i32(member_id);
            hasher.finish()
        };
        let core = Consensus::with_election(ids.get(..count).unwrap_or(&[]), member_id, seed)
            .map_err(RaftError::Member)?;
        let Some(own) = parsed.iter().find(|member| member.id == member_id) else {
            return Err(RaftError::Member(MembersRefused::NotAMember {
                id: member_id,
            }));
        };

        // The driver's catalog, which the driver created when it started on this
        // directory: a directory without one is not the driver's. It is opened before
        // the `terms` file, which is created when it is not there, so that a
        // directory the module refuses is left as it was.
        let catalog = CatalogReader::open(log_dir.as_ref())?;
        let mut terms = TermsFile::open(log_dir.as_ref())?;
        // A member whose record holds a term holds a log, and is given no new record
        // of its recordings: an empty one would say that it holds none, and the
        // member would start with a log shorter than it reported (ADR 0108).
        let mut recordings = if terms.log_leadership_term_id() == -1 {
            RecordingsFile::open(log_dir.as_ref(), member_id)?
        } else {
            RecordingsFile::open_kept(log_dir.as_ref(), member_id)?
        };

        let recordings_path = dir
            .as_ref()
            .join(format!("{LOG_RECORDINGS_FILE_PREFIX}{member_id}"));
        let recordings_tmp_path = dir.as_ref().join(format!(
            "{LOG_RECORDINGS_FILE_PREFIX}{member_id}{LOG_RECORDINGS_TMP_SUFFIX}"
        ));
        let mut client = Cavalry::connect_to(dir)?;
        // The log the member kept, before it reads a message: its recordings and
        // where they end. Then, in a truncation's order, the record of them as the
        // catalog holds them, and a record of terms that holds no term above that
        // end: a truncation the member's stop cut short ends here.
        let (kept, stopped_at) = kept_log(
            recordings.floor_position(),
            recordings.entries(),
            |recording_id| cataloged(&catalog, recording_id),
            |recording_id| stop_listed(&mut client, &catalog, recording_id),
        )?;
        // A log and no term of it: the record of terms is not the one this log was
        // recorded beside, and the member's votes went with it.
        if stopped_at > 0 && terms.log_leadership_term_id() == -1 {
            return Err(RaftError::TermsLost {
                log_end: stopped_at,
            });
        }
        let floor = kept.front().map_or(stopped_at, |first| first.start);
        let unchanged = recordings.floor_position() == floor
            && recordings.entries().len() == kept.len()
            && recordings
                .entries()
                .iter()
                .zip(&kept)
                .all(|(listed, kept)| listed.recording_id == kept.recording_id);
        if !unchanged {
            write_list(&mut recordings, &kept, [None, None], stopped_at)?;
        }
        terms.truncate_above(stopped_at)?;
        let consensus =
            client.add_subscription(&endpoint_channel(&own.consensus), CONSENSUS_STREAM_ID)?;
        let mut peers = Vec::with_capacity(MAX_MEMBERS);
        for member in parsed.iter().filter(|member| member.id != member_id) {
            peers.push(Peer {
                id: member.id,
                consensus: client.add_exclusive_publication(
                    &endpoint_channel(&member.consensus),
                    CONSENSUS_STREAM_ID,
                )?,
                log_destination: endpoint_channel(&member.log),
                catchup_channel: endpoint_channel(&member.catchup),
            });
        }
        let replays = peers.iter().map(|_| None).collect();
        // Each counter holds the core's value from its add: −1 for the term and the
        // recording, before a duty cycle has run.
        let counters = Counters::add(&mut client, member_id, &core)?;
        let started = Instant::now();
        Ok(Self {
            core,
            consensus,
            io: ClientIo {
                client,
                terms,
                peers,
                log_control_channel: format!(
                    "cavalry:udp?control={}|control-mode=manual|fc=min",
                    host_port(&own.log)
                ),
                log_endpoint_channel: endpoint_channel(&own.log),
                member_id,
                catchup_endpoint_channel: endpoint_channel(&own.catchup),
                catchup_endpoint: host_port(&own.catchup),
                catchup_subscription: None,
                catchup_armed: None,
                catchup_starting: None,
                catchup_held: None,
                catchup_sync_asked_at: NO_SYNC_ASKED,
                scratch: Box::new([0u8; 1408]),
                log_publication: None,
                log_term: None,
                destinations_added: 0,
                log_recording: None,
                log_base: 0,
                kept,
                recordings,
                log_listed: false,
                replays,
                log_subscription: None,
                armed: None,
                starting: None,
                image: LogImage::Awaited,
                held: None,
                log_floor: 0,
                catalog,
                stopped_at,
                sync_asked_at: NO_SYNC_ASKED,
                recordings_path,
                recordings_tmp_path,
                recordings_file: vec![0; MAX_LOG_RECORDINGS_LENGTH],
                named: NO_RECORDING,
            },
            counters,
            messages_rejected: 0,
            log_recordings_unwritten: 0,
            started,
            clock: read_clock(started),
        })
    }

    /// One duty cycle: the consensus messages that have arrived, at most 16, what a
    /// follower's log subscription holds, the core's own cycle, and the counters that
    /// moved. Returns how much it did, 0 for nothing, for the caller's idle strategy.
    /// An error is a step the member cannot go on without; the member stays in the
    /// state it was in, and a later call tries the step again. An error of the core's
    /// own cycle is returned after the counters are stored (ADR 0091).
    pub fn do_work(&mut self) -> Result<usize, RaftError> {
        self.clock = read_clock(self.started);
        let clock = self.clock;
        let Self {
            core,
            consensus,
            io,
            messages_rejected,
            ..
        } = self;
        let mut fault = None;
        let read = consensus
            .poll(
                |body, header| {
                    // A consensus message is one frame; a fragment of something longer
                    // is none of this page's, and is counted as a message that failed
                    // its length.
                    let whole = header.flags().contains(Flags::UNFRAGMENTED);
                    match Message::parse(Stream::Consensus, body) {
                        Ok(message) if whole => {
                            if fault.is_none()
                                && let Err(error) = core.on_message(clock, &message, io)
                            {
                                fault = Some(error);
                            }
                        }
                        _ => *messages_rejected += 1,
                    }
                },
                CONSENSUS_POLL_LIMIT,
            )
            .map_err(RaftError::Poll)?;
        if let Some(error) = fault {
            return Err(error);
        }
        io.observe_log()?;
        io.observe_catchup()?;
        // The core's fault is returned after the counters are stored: a member that
        // faults at a step it tries again each cycle, a replay it cannot start, still
        // shows its role, its state and its counts (ADR 0091).
        let stepped = core.do_work(clock, io);
        // A recording the election was just told of joins the member's log here, with
        // the record that names it synced; the next cycle reports what it holds (ADR
        // 0108). Its error is returned after the core's own.
        let listed = io.list_counted();
        // The file of the log's recordings is written before the counter names a
        // recording it did not name (`spec/cluster.md`, "The service"; ADR 0103). A
        // file that could not be written leaves the counter at −1, and the next
        // cycle writes it again; its error is returned after the core's own.
        let mut written = Ok(());
        let log_recording_id = match self.io.live_recording() {
            Some((id, start)) if id != self.io.named => {
                match self.io.write_log_recordings(id, start) {
                    Ok(()) => id,
                    Err(error) => {
                        // Counted, so that `cavalry doctor` tells this member from
                        // one whose recording never started (invariant 4).
                        self.log_recordings_unwritten =
                            self.log_recordings_unwritten.saturating_add(1);
                        written = Err(error);
                        NO_RECORDING
                    }
                }
            }
            Some((id, _)) => id,
            None => NO_RECORDING,
        };
        self.io.named = self.counters.store(
            &self.core,
            [self.messages_rejected, self.log_recordings_unwritten],
            log_recording_id,
        );
        let work = read + stepped?;
        listed?;
        written?;
        Ok(work)
    }

    /// Appends `payload` to the cluster's log as a session message of no session, for
    /// the leader's own process (`spec/cluster.md`, "Session message header, template
    /// 1"). It is committed once a quorum has recorded it, which
    /// [`ConsensusModule::commit_position`] says. The entry carries the cluster's time
    /// of the last duty cycle. A follower, a leader whose election
    /// has not closed and a back-pressured log are [`RaftError::Offer`], each with what
    /// to do; a payload over [`MAX_PAYLOAD_LENGTH`] is [`RaftError::PayloadTooLong`].
    pub fn offer(&mut self, payload: &[u8]) -> Result<(), RaftError> {
        if payload.len() > MAX_PAYLOAD_LENGTH {
            return Err(RaftError::PayloadTooLong {
                length: payload.len(),
            });
        }
        self.core
            .offer(self.clock, payload, &mut self.io)?
            .map_err(RaftError::Offer)
    }

    /// This member's id.
    pub fn member_id(&self) -> i32 {
        self.core.member_id()
    }

    /// This member's role.
    pub fn role(&self) -> Role {
        self.core.role()
    }

    /// The state of this member's election, closed when none runs.
    pub fn election_state(&self) -> ElectionState {
        self.core.election_state()
    }

    /// The term this member leads or follows, −1 before the first.
    pub fn leadership_term_id(&self) -> i64 {
        self.core.leadership_term_id()
    }

    /// This member's append position: what its recording of the log has synced.
    pub fn append_position(&self) -> i64 {
        self.core.append_position()
    }

    /// The commit position this member knows, which never falls.
    pub fn commit_position(&self) -> i64 {
        self.core.commit_position()
    }

    /// What this member has counted, as its counters in the counters file say it.
    pub fn counts(&self) -> Counts {
        self.core.counts()
    }

    /// The id of this member's recording of the log, once it has one: the leader's of
    /// its publication, a follower's of the leader's image once its election has been
    /// told where that recording begins and it begins where the member's log ended
    /// (ADR 0093); `None` before the first and
    /// from when the member has left the log, its recording's stop read, until it has
    /// joined it again. A leave that failed holds the recording, named, while the next
    /// cycle tries it again. Each duty
    /// cycle stores it in `cluster.log_recording_id`, −1 for `None`, before the commit
    /// position (`spec/cluster.md`, "The service").
    pub fn log_recording_id(&self) -> Option<i64> {
        self.io.live_recording().map(|(id, _)| id)
    }

    /// Stops the module: closes its client, which removes its publications, its
    /// subscriptions and its counters, and then removes its file of the log's
    /// recordings, which no counter names any more (ADR 0103). Its recordings are the
    /// driver's and stay; the driver stops each when its publication or its image
    /// ends.
    pub fn close(self) -> Result<(), RaftError> {
        let Self {
            consensus,
            io,
            counters,
            ..
        } = self;
        drop(counters);
        drop(consensus);
        let ClientIo {
            client,
            peers,
            log_publication,
            log_subscription,
            recordings_path,
            recordings_tmp_path,
            ..
        } = io;
        drop(peers);
        drop(log_publication);
        drop(log_subscription);
        let closed = client.close();
        // The file, and what a write that failed left under its other name. A file
        // that is not there was never written: the member named no recording. Any
        // other error is ignored too: a file left behind names a recording that no
        // counter names once the client is closed, so no service reads it
        // (`spec/cluster.md`, "The service"), and the driver removes it with the rest
        // of its directory when it next starts.
        let _ = std::fs::remove_file(recordings_path);
        let _ = std::fs::remove_file(recordings_tmp_path);
        Ok(closed?)
    }
}

impl core::fmt::Debug for ConsensusModule {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ConsensusModule")
            .field("member_id", &self.core.member_id())
            .field("role", &self.core.role())
            .field("election_state", &self.core.election_state())
            .field("leadership_term_id", &self.core.leadership_term_id())
            .field("append_position", &self.core.append_position())
            .field("commit_position", &self.core.commit_position())
            .finish()
    }
}

#[cfg(test)]
mod tests;
