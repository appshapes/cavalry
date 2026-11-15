//! A member's snapshot of its consensus module: the member's sessions as they are at
//! one position of its log, written to a recording and listed in the member's
//! snapshots file (`spec/cluster.md`, "Snapshots"; ADR 0129).
//!
//! The leader logs a cluster action that asks for a snapshot, and every member takes
//! one when its read of its own committed log reaches that entry. The module's read
//! stops at the entry, so the table of sessions is then what every entry below the
//! snapshot's log position made it, on every member alike. The member holds one
//! exclusive publication on the snapshot stream from its start, since adding one
//! takes the driver the longest of a snapshot's commands. [`ModuleSnapshot::begin`]
//! starts a recording of it, from the publication's position.
//! [`ModuleSnapshot::do_work`] writes the snapshot's messages, a marker, the open
//! sessions in the order of their ids, the module's own state and a marker; stops the
//! recording; and lists the snapshot once the driver has said where the recording
//! stopped and that position is the end of what was written. A snapshot counts from
//! that listing on, and never before (`spec/durable-log.md`, "Snapshots").
//!
//! Nothing here waits. Each step is one duty cycle's work: a command to the driver
//! that answers within its response timeout, an offer, or a load of the recording's
//! stop. A step the driver refuses, a publication that takes no message, or a stop
//! that does not come within [`SNAPSHOT_TIMEOUT`] ends the snapshot, counted in
//! `cluster.snapshots_failed`, and the member goes on: its log is whole, and the next
//! action asks again. While the messages are being written the module does not read
//! its committed log ([`ModuleSnapshot::is_writing`]), so the table they are written
//! from does not change under them. That is one cycle unless the publication is
//! back-pressured.
//!
//! Nothing here runs per message of the log: a snapshot is asked for once in a
//! snapshot interval.
//!
//! The service beside the member takes a snapshot of its own for the same action,
//! when its program gives it its state, and tells the member with a service ack on
//! the consensus module stream. [`ModuleSnapshot::read_acks`] reads that stream and
//! [`ModuleSnapshot::list_service`] lists the service's snapshot in the same file
//! (`spec/cluster.md`, "The member's list of a service's snapshots"; ADR 0131). The
//! member answers no ack and waits for none.

use std::time::{Duration, Instant};

use cavalry_client::{
    Cavalry, ClientError, Counter, Fault, NULL_LENGTH, NULL_POSITION, Offer, Publication,
    Recording, Subscription,
};
use cavalry_log::snapshots::{CONSENSUS_MODULE, ListedSnapshot, SnapshotsError, SnapshotsFile};
use cavalry_proto::cluster::{
    CLOSE_REASON_NONE, CONSENSUS_MODULE_STREAM_ID, EGRESS_STREAM_ID, LOG_REPLAY_STREAM_ID,
    MAX_SESSIONS, Message, NULL_VALUE, SERVICE_ID, SNAPSHOT_STREAM_ID, SNAPSHOT_TIMEOUT_NS,
    SNAPSHOT_TYPE_ID, SnapshotMark, Stream, TimeUnit,
};
use cavalry_shm::counters::CounterId;
use cavalry_shm::counters::generated::{
    CLUSTER_LOG_PURGE_POSITION, CLUSTER_SERVICE_ACKS_REJECTED, CLUSTER_SERVICE_SNAPSHOT_POSITION,
    CLUSTER_SERVICE_SNAPSHOT_RECORDING_ID, CLUSTER_SERVICE_SNAPSHOTS_LISTED,
    CLUSTER_SNAPSHOT_ACTIONS, CLUSTER_SNAPSHOT_POSITION, CLUSTER_SNAPSHOT_START_POSITION,
    CLUSTER_SNAPSHOT_STEPS, CLUSTER_SNAPSHOTS_BEGUN, CLUSTER_SNAPSHOTS_FAILED,
    CLUSTER_SNAPSHOTS_PASSED_OVER, CLUSTER_SNAPSHOTS_TAKEN,
};

use crate::sessions::Sessions;

/// The channel a snapshot is written on: a stream of this driver alone, which its
/// recorder records.
const SNAPSHOT_CHANNEL: &str = "cavalry:ipc";

/// How long a snapshot may go without its next step: a message the publication does
/// not take, or a recording whose stop the driver does not announce. Past it the
/// snapshot is given up and counted. It is the page's snapshot timeout, 1 s, the one
/// a service's snapshot is under as well.
pub const SNAPSHOT_TIMEOUT: Duration = Duration::from_nanos(SNAPSHOT_TIMEOUT_NS);

/// The longest message of a snapshot: a cluster session with the longest response
/// channel a leader accepts, 8 + 40 + 4 + 280 bytes, which one frame carries.
const SCRATCH_LENGTH: usize = 512;

/// How many messages of the consensus module stream a member reads in one duty
/// cycle (`spec/cluster.md`, "The member's list of a service's snapshots").
pub(crate) const SERVICE_ACK_LIMIT: usize = 10;

/// A service's word of a snapshot it took: what its service ack carries, less the
/// field a member does not read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ServiceAcked {
    /// The log position of the service's snapshot.
    pub(crate) log_position: i64,
    /// The cluster's time of the cluster action that asked for it.
    pub(crate) timestamp: i64,
    /// The snapshot's recording in the catalog of the member's driver: the ack's
    /// relevant id.
    pub(crate) recording_id: i64,
    /// The service's id.
    pub(crate) service_id: i32,
}

/// Where in the log a snapshot is: what the cluster action that asked for it gives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SnapshotDue {
    /// The position after the action's entry: every entry below it is in the state.
    pub log_position: i64,
    /// The leadership term of the action's entry.
    pub leadership_term_id: i64,
    /// Where that term begins in the log.
    pub term_base_log_position: i64,
    /// The cluster's time of the action's entry.
    pub timestamp: i64,
}

/// What a member counts of its snapshots (`counters.toml`, `cluster.snapshot_*` and
/// `cluster.snapshots_*`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SnapshotCounts {
    /// `cluster.snapshot_actions`: a cluster action asking for a snapshot that the
    /// member appended while it led.
    pub actions: u64,
    /// `cluster.snapshots_begun`: a snapshot the member began in the cycle that read
    /// the action, with a recording of its one publication on the snapshot stream.
    /// The member added that publication at its start and keeps it for its life.
    pub begun: u64,
    /// `cluster.snapshot_steps`: a snapshot whose messages were all written, its
    /// recording's stop asked.
    pub steps: u64,
    /// `cluster.snapshots_taken`: a snapshot listed in the member's snapshots file.
    pub taken: u64,
    /// `cluster.snapshots_failed`: a snapshot given up, at any step.
    pub failed: u64,
}

/// What a member counts of the snapshots of the service beside it (`counters.toml`,
/// `cluster.service_snapshots_listed` and `cluster.service_acks_rejected`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ServiceSnapshotCounts {
    /// `cluster.service_snapshots_listed`: a service's snapshot listed for its
    /// service ack.
    pub listed: u64,
    /// `cluster.service_acks_rejected`: a service ack for which nothing was listed.
    pub rejected: u64,
}

/// Where a snapshot is in its taking. A member takes one at a time.
// state machine: cluster_snapshot
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum SnapshotState {
    /// No snapshot is being taken.
    Idle,
    /// The recording of the member's publication has started, and the messages are
    /// being written.
    Writing,
    /// Every message is written and the recording's stop is asked.
    Stopping,
}

/// The snapshot a member's consensus module takes of itself, one at a time, and the
/// file that lists the ones it took.
#[derive(Debug)]
pub(crate) struct ModuleSnapshot {
    file: SnapshotsFile,
    state: SnapshotState,
    due: SnapshotDue,
    /// The member's publication on the snapshot stream, added at its start and kept
    /// for its life: each snapshot is a recording of a span of it.
    publication: Publication,
    recording: Option<Recording>,
    /// Which message is written next: 0 the first marker, then the open sessions in
    /// the order of their ids, then the module's own state, then the last marker.
    next: usize,
    /// Where what was written ends: the publication's position after the last
    /// message it took.
    end: i64,
    /// Whether the recording's stop has been asked.
    stop_asked: bool,
    /// When the step the snapshot is at must have moved by.
    deadline: Instant,
    scratch: Box<[u8; SCRATCH_LENGTH]>,
    counts: SnapshotCounts,
    service_counts: ServiceSnapshotCounts,
    /// The member's subscription on the consensus module stream, added at its start
    /// and kept for its life: the service acks of the services on its driver.
    acks: Subscription,
    /// The member's counters of its snapshots, each with the value it last stored:
    /// eleven in the order of [`ModuleSnapshot::values`], then the two of its start,
    /// which are stored once.
    counters: [(Counter, i64); 13],
}

/// Why a member's snapshots could not be set up at its start.
#[derive(Debug)]
pub(crate) enum OpenError {
    /// The driver refused one of the member's counters, or its publication.
    Client(ClientError),
}

impl ModuleSnapshot {
    /// Takes the member's snapshots file, which its start opened and read, and adds
    /// the member's counters of its snapshots, each holding its value from its first
    /// store, its publication on the snapshot stream, and its subscription on the
    /// consensus module stream. `started` is what the start took from the file: the
    /// position of the snapshot it began from, −1 for none, and how many listed
    /// snapshots it passed over.
    pub(crate) fn open(
        client: &mut Cavalry,
        file: SnapshotsFile,
        member_id: i32,
        started: (i64, u64),
    ) -> Result<Self, OpenError> {
        let (start_position, passed_over) = started;
        let newest = file
            .newest(CONSENSUS_MODULE)
            .map_or(NULL_VALUE, |snapshot| snapshot.log_position);
        let newest_service = file.newest(SERVICE_ID);
        let service_recording = newest_service.map_or(NULL_VALUE, |snapshot| snapshot.recording_id);
        let purge = purge_position(file.newest(CONSENSUS_MODULE), newest_service);
        let newest_service = newest_service.map_or(NULL_VALUE, |snapshot| snapshot.log_position);
        let mut add = |id: CounterId, name: &str, value: i64| {
            let counter = client.add_counter(
                id.0,
                &member_id.to_le_bytes(),
                &format!("{name} {member_id}"),
            )?;
            counter.store_release(value)?;
            Ok::<_, ClientError>((counter, value))
        };
        let counters = (|| {
            Ok([
                add(
                    CLUSTER_SNAPSHOT_POSITION,
                    "cluster.snapshot_position",
                    newest,
                )?,
                add(CLUSTER_SNAPSHOT_ACTIONS, "cluster.snapshot_actions", 0)?,
                add(CLUSTER_SNAPSHOTS_BEGUN, "cluster.snapshots_begun", 0)?,
                add(CLUSTER_SNAPSHOT_STEPS, "cluster.snapshot_steps", 0)?,
                add(CLUSTER_SNAPSHOTS_TAKEN, "cluster.snapshots_taken", 0)?,
                add(CLUSTER_SNAPSHOTS_FAILED, "cluster.snapshots_failed", 0)?,
                // What the member lists for the service beside it.
                add(
                    CLUSTER_SERVICE_SNAPSHOT_POSITION,
                    "cluster.service_snapshot_position",
                    newest_service,
                )?,
                add(
                    CLUSTER_SERVICE_SNAPSHOTS_LISTED,
                    "cluster.service_snapshots_listed",
                    0,
                )?,
                add(
                    CLUSTER_SERVICE_ACKS_REJECTED,
                    "cluster.service_acks_rejected",
                    0,
                )?,
                // What a service's start reads, and what an operator may purge.
                add(
                    CLUSTER_SERVICE_SNAPSHOT_RECORDING_ID,
                    "cluster.service_snapshot_recording_id",
                    service_recording,
                )?,
                add(
                    CLUSTER_LOG_PURGE_POSITION,
                    "cluster.log_purge_position",
                    purge,
                )?,
                // What the start did, which never changes while the member runs.
                add(
                    CLUSTER_SNAPSHOT_START_POSITION,
                    "cluster.snapshot_start_position",
                    start_position,
                )?,
                add(
                    CLUSTER_SNAPSHOTS_PASSED_OVER,
                    "cluster.snapshots_passed_over",
                    i64::try_from(passed_over).unwrap_or(i64::MAX),
                )?,
            ])
        })()
        .map_err(OpenError::Client)?;
        let publication = client
            .add_exclusive_publication(SNAPSHOT_CHANNEL, SNAPSHOT_STREAM_ID)
            .map_err(OpenError::Client)?;
        let acks = client
            .add_subscription(SNAPSHOT_CHANNEL, CONSENSUS_MODULE_STREAM_ID)
            .map_err(OpenError::Client)?;
        Ok(Self {
            file,
            state: SnapshotState::Idle,
            due: SnapshotDue {
                log_position: 0,
                leadership_term_id: 0,
                term_base_log_position: 0,
                timestamp: 0,
            },
            publication,
            recording: None,
            next: 0,
            end: 0,
            stop_asked: false,
            deadline: Instant::now(),
            scratch: Box::new([0; SCRATCH_LENGTH]),
            counts: SnapshotCounts::default(),
            service_counts: ServiceSnapshotCounts::default(),
            acks,
            counters,
        })
    }

    /// The values of the member's eleven counters of its snapshots that move while
    /// it runs, in the order they were added: its own gauge and counts, the
    /// service's gauge and counts, the recording a service starts from, and the
    /// position its log is needed from.
    fn values(&self) -> [i64; 11] {
        let count = |value: u64| i64::try_from(value).unwrap_or(i64::MAX);
        [
            self.newest_position(),
            count(self.counts.actions),
            count(self.counts.begun),
            count(self.counts.steps),
            count(self.counts.taken),
            count(self.counts.failed),
            self.newest_service_position(),
            count(self.service_counts.listed),
            count(self.service_counts.rejected),
            self.file
                .newest(SERVICE_ID)
                .map_or(NULL_VALUE, |snapshot| snapshot.recording_id),
            self.purge_position(),
        ]
    }

    /// Stores each of the member's counters of its snapshots whose value has moved:
    /// once a duty cycle, after the cycle's work.
    pub(crate) fn store(&mut self) {
        let values = self.values();
        for ((counter, last), value) in self.counters.iter_mut().zip(values) {
            if value != *last && counter.store_release(value).is_ok() {
                *last = value;
            }
        }
    }

    /// Counts a cluster action asking for a snapshot that the member appended as the
    /// leader: `cluster.snapshot_actions`.
    pub(crate) fn count_action(&mut self) {
        self.counts.actions = self.counts.actions.saturating_add(1);
    }

    /// What the member has counted of its snapshots.
    pub(crate) const fn counts(&self) -> SnapshotCounts {
        self.counts
    }

    /// The log position of the newest snapshot of the consensus module that the
    /// member's file lists, −1 when it lists none: `cluster.snapshot_position`.
    pub(crate) fn newest_position(&self) -> i64 {
        self.file
            .newest(CONSENSUS_MODULE)
            .map_or(NULL_VALUE, |snapshot| snapshot.log_position)
    }

    /// The position the member's log is needed from: `cluster.log_purge_position`.
    pub(crate) fn purge_position(&self) -> i64 {
        purge_position(
            self.file.newest(CONSENSUS_MODULE),
            self.file.newest(SERVICE_ID),
        )
    }

    /// What the member has counted of the snapshots of the service beside it.
    pub(crate) const fn service_counts(&self) -> ServiceSnapshotCounts {
        self.service_counts
    }

    /// The log position of the newest snapshot of a service that the member's file
    /// lists, −1 when it lists none: `cluster.service_snapshot_position`.
    pub(crate) fn newest_service_position(&self) -> i64 {
        self.file
            .newest(SERVICE_ID)
            .map_or(NULL_VALUE, |snapshot| snapshot.log_position)
    }

    /// Reads the consensus module stream, at most [`SERVICE_ACK_LIMIT`] messages: the
    /// service acks into `acks`, in the order they were read. Returns how many acks
    /// were read, and how many messages were none: a message the stream does not
    /// carry, or one a reader rejects (`spec/cluster.md`, "What a reader rejects").
    pub(crate) fn read_acks(
        &mut self,
        acks: &mut [Option<ServiceAcked>; SERVICE_ACK_LIMIT],
    ) -> Result<(usize, u64), Fault> {
        let mut read = 0usize;
        let mut rejected = 0u64;
        self.acks.poll(
            |bytes, _| match Message::parse(Stream::ConsensusModule, bytes) {
                Ok(Message::ServiceAck {
                    log_position,
                    timestamp,
                    relevant_id,
                    service_id,
                    ..
                }) => {
                    if let Some(slot) = acks.get_mut(read) {
                        *slot = Some(ServiceAcked {
                            log_position,
                            timestamp,
                            recording_id: relevant_id,
                            service_id,
                        });
                        read += 1;
                    }
                }
                _ => rejected = rejected.saturating_add(1),
            },
            SERVICE_ACK_LIMIT,
        )?;
        Ok((read, rejected))
    }

    /// Lists a service's snapshot for its service ack, when it is one the member can
    /// hold (`spec/cluster.md`, "The member's list of a service's snapshots"; ADR
    /// 0131). The service id is the one service's. `held` says that the catalog holds
    /// the ack's recording as a stopped recording of the service snapshot stream,
    /// with bytes. The log position is above 0 and at or below `commit_position`.
    /// `term` is the newest term of the member's record of terms whose base is below
    /// the position, with that base. And the file takes the entry: its position is
    /// above the newest the file lists for the service. An ack that fails one of
    /// these is counted and nothing is listed: `Ok(false)`. `Err` is a list that
    /// could not be written.
    pub(crate) fn list_service(
        &mut self,
        ack: ServiceAcked,
        held: bool,
        commit_position: i64,
        term: Option<(i64, i64)>,
    ) -> Result<bool, SnapshotsError> {
        let listable = ack.service_id == SERVICE_ID
            && held
            && ack.log_position > 0
            && ack.log_position <= commit_position;
        let (true, Some((leadership_term_id, term_base_log_position))) = (listable, term) else {
            self.service_counts.rejected = self.service_counts.rejected.saturating_add(1);
            return Ok(false);
        };
        match self.file.list(ListedSnapshot {
            recording_id: ack.recording_id,
            leadership_term_id,
            term_base_log_position,
            log_position: ack.log_position,
            timestamp: ack.timestamp,
            service_id: ack.service_id,
        }) {
            Ok(()) => {
                self.service_counts.listed = self.service_counts.listed.saturating_add(1);
                Ok(true)
            }
            // A snapshot the list does not take: at or below the newest it holds
            // for the service, or with a field no list holds. Nothing was written.
            Err(SnapshotsError::List { .. }) => {
                self.service_counts.rejected = self.service_counts.rejected.saturating_add(1);
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    /// Whether the snapshot's messages are being written. The module does not read
    /// its committed log meanwhile: the table the messages are written from must be
    /// the table at the snapshot's position.
    pub(crate) fn is_writing(&self) -> bool {
        self.state == SnapshotState::Writing
    }

    /// Begins the snapshot `due` asks for: a recording of the member's publication on
    /// the snapshot stream, from the publication's position. A snapshot asked for
    /// while one is being taken, or at a position the file's newest snapshot already
    /// covers, is not taken; the first is counted as failed, and the second is the
    /// action a member reads again when it reads its log from the start. A recording
    /// the driver refuses ends the snapshot, counted, and the member goes on.
    pub(crate) fn begin(&mut self, client: &mut Cavalry, due: SnapshotDue) {
        if self.newest_position() >= due.log_position {
            return;
        }
        if self.state != SnapshotState::Idle {
            self.counts.failed = self.counts.failed.saturating_add(1);
            return;
        }
        // The recording the driver starts is of the stream's first publication that
        // is not yet recorded (`spec/client-protocol.md`, "Commands", start
        // recording). The module is the one writer on this stream of its driver, so
        // that is its publication; a recording of any other session is not this
        // snapshot's, and the snapshot is given up.
        let recording = match client.start_recording(SNAPSHOT_CHANNEL, SNAPSHOT_STREAM_ID) {
            Ok(recording) if recording.session_id() == self.publication.session_id() => recording,
            Ok(other) => {
                let _ = other.stop(client);
                self.counts.failed = self.counts.failed.saturating_add(1);
                return;
            }
            Err(_) => {
                self.counts.failed = self.counts.failed.saturating_add(1);
                return;
            }
        };
        self.end = recording.position();
        self.due = due;
        self.next = 0;
        self.stop_asked = false;
        self.recording = Some(recording);
        self.deadline = Instant::now() + SNAPSHOT_TIMEOUT;
        self.state = SnapshotState::Writing;
        self.counts.begun = self.counts.begun.saturating_add(1);
    }

    /// One duty cycle of the snapshot being taken: its messages while there are some
    /// to write, then the recording's stop, then the listing. Returns how much it
    /// did. `Err` is the snapshots file's alone, a list that could not be written:
    /// every other failure ends the snapshot, counted, and is no fault of the
    /// member's.
    pub(crate) fn do_work(
        &mut self,
        client: &mut Cavalry,
        sessions: &Sessions,
    ) -> Result<usize, SnapshotsError> {
        match self.state {
            SnapshotState::Idle => Ok(0),
            SnapshotState::Writing => Ok(self.write(client, sessions)),
            SnapshotState::Stopping => self.stop(client),
        }
    }

    /// Writes the messages not yet written, in order, until the publication takes no
    /// more. A message the publication will take later is tried again on the next
    /// cycle, until the deadline.
    fn write(&mut self, client: &mut Cavalry, sessions: &Sessions) -> usize {
        let open = sessions.open();
        let mut written = 0;
        // The first marker, the sessions, the module's state, the last marker.
        let last = open + 2;
        let due = self.due;
        while self.next <= last {
            let marker = |mark| Message::SnapshotMarker {
                type_id: SNAPSHOT_TYPE_ID,
                log_position: due.log_position,
                leadership_term_id: due.leadership_term_id,
                index: 0,
                mark,
                time_unit: TimeUnit::Millis,
                app_version: 0,
            };
            let message = if self.next == 0 {
                marker(SnapshotMark::Begin)
            } else if self.next == last {
                marker(SnapshotMark::End)
            } else if self.next == last - 1 {
                // The next session id as the log below the snapshot's position gives
                // it, and not the leader's own, which a connect it accepted moves
                // before the session open event is committed (ADR 0129).
                Message::ConsensusModule {
                    next_session_id: sessions.committed_next_session_id(),
                    next_service_session_id: 0,
                    log_service_session_id: 0,
                    pending_message_capacity: 0,
                }
            } else {
                let Some(session) = sessions.open_session(self.next - 1) else {
                    // The table changed under the snapshot, which the module's read
                    // of its log, paused while this writes, never lets happen.
                    self.fail(client);
                    return written;
                };
                Message::ClusterSession {
                    cluster_session_id: session.id,
                    correlation_id: session.correlation_id,
                    opened_log_position: session.opened_log_position,
                    time_of_last_activity: NULL_VALUE,
                    close_reason: CLOSE_REASON_NONE,
                    response_stream_id: EGRESS_STREAM_ID,
                    response_channel: session.response_channel,
                }
            };
            let Ok(length) = message.write(self.scratch.as_mut_slice()) else {
                self.fail(client);
                return written;
            };
            let Some(bytes) = self.scratch.get(..length) else {
                self.fail(client);
                return written;
            };
            match self.publication.offer(bytes) {
                Ok(position) => {
                    self.end = position;
                    self.next += 1;
                    written += 1;
                    self.deadline = Instant::now() + SNAPSHOT_TIMEOUT;
                }
                // The recording connects the publication and gives it its limit a
                // moment after it starts: tried again on the next cycle.
                Err(Offer::BackPressured | Offer::NotConnected | Offer::AdminAction) => {
                    if Instant::now() >= self.deadline {
                        self.fail(client);
                    }
                    return written;
                }
                Err(Offer::MaxPositionExceeded | Offer::Closed | Offer::TooLong { .. }) => {
                    self.fail(client);
                    return written;
                }
            }
        }
        self.state = SnapshotState::Stopping;
        self.deadline = Instant::now() + SNAPSHOT_TIMEOUT;
        self.counts.steps = self.counts.steps.saturating_add(1);
        written
    }

    /// Asks the recording's stop once the recorder has written every message, reads
    /// where it stopped, and lists the snapshot when that is the end of what was
    /// written. The driver answers a stop once the catalog's stop is written and
    /// synced, so a snapshot listed is one a power loss leaves whole.
    fn stop(&mut self, client: &mut Cavalry) -> Result<usize, SnapshotsError> {
        let Some(recording) = self.recording.as_ref() else {
            self.fail(client);
            return Ok(0);
        };
        let Some(stop) = recording.stop_position() else {
            if Instant::now() >= self.deadline {
                self.fail(client);
                return Ok(0);
            }
            // The stop is asked when the recorder has written to the end, so that
            // the recording stops there and not at a position before it.
            if !self.stop_asked && recording.position() >= self.end {
                match recording.stop(client) {
                    Ok(()) => self.stop_asked = true,
                    Err(_) => self.fail(client),
                }
                return Ok(1);
            }
            return Ok(0);
        };
        let recording_id = recording.recording_id();
        if stop != self.end {
            // The recording holds less than was written: a write or a sync of the
            // recorder's failed. It is no snapshot.
            self.fail(client);
            return Ok(1);
        }
        let listed = self.file.list(ListedSnapshot {
            recording_id,
            leadership_term_id: self.due.leadership_term_id,
            term_base_log_position: self.due.term_base_log_position,
            log_position: self.due.log_position,
            timestamp: self.due.timestamp,
            service_id: CONSENSUS_MODULE,
        });
        self.release();
        match listed {
            Ok(()) => {
                self.counts.taken = self.counts.taken.saturating_add(1);
                Ok(1)
            }
            Err(error) => {
                self.counts.failed = self.counts.failed.saturating_add(1);
                Err(error)
            }
        }
    }

    /// Gives the snapshot up: its recording's stop asked, when it has one that has not
    /// stopped, and the failure counted. The recording stays in the catalog, in no
    /// list, and is no snapshot. What was written of it stays in the publication's
    /// stream, below where the next snapshot's recording begins.
    fn fail(&mut self, client: &mut Cavalry) {
        if let Some(recording) = self.recording.as_ref()
            && recording.stop_position().is_none()
        {
            // Asked once and not awaited. A recording the driver does not stop is
            // still the publication's when the next snapshot begins, whose recording
            // the driver then refuses, and that snapshot fails too.
            let _ = recording.stop(client);
        }
        self.release();
        self.counts.failed = self.counts.failed.saturating_add(1);
    }

    /// Lets the recording's handle go: no snapshot is being taken.
    fn release(&mut self) {
        self.recording = None;
        self.state = SnapshotState::Idle;
    }

    /// Lists `snapshot` in the member's file, as nothing but a test does behind a
    /// snapshot that is being taken: the way to a list that refuses the snapshot's own
    /// entry, which no fault of a driver gives a test.
    #[cfg(test)]
    pub(crate) fn list_behind(&mut self, snapshot: ListedSnapshot) -> Result<(), SnapshotsError> {
        self.file.list(snapshot)
    }
}

/// The position a member's log is needed from (`spec/cluster.md`, "A member's
/// start"; ADR 0132): the lower of the log positions of the newest snapshot of its
/// consensus module and of the newest snapshot of a service that it lists, and 0
/// while it lists no snapshot of one of the two. The member's own start reads its
/// log from the first, and a service whose program calls `ClusterService::restore`
/// from the second; a service that never calls it reads from position 0, which
/// the member cannot know.
fn purge_position(own: Option<&ListedSnapshot>, service: Option<&ListedSnapshot>) -> i64 {
    match (own, service) {
        (Some(own), Some(service)) => own.log_position.min(service.log_position),
        _ => 0,
    }
}

/// How long a start waits for the replay of one snapshot's recording. A driver that
/// does not play a few hundred bytes back in this time does not play them back, and
/// the snapshot is passed over.
const LOAD_TIMEOUT: Duration = Duration::from_secs(5);

/// The most messages a consensus module's snapshot holds: two markers, a session for
/// each slot of the table, and the module's own state.
const MAX_MESSAGES: usize = MAX_SESSIONS + 3;

/// One session a start takes from a snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RestoredSession {
    pub(crate) id: i64,
    pub(crate) correlation_id: i64,
    pub(crate) opened_log_position: i64,
    pub(crate) response_channel: Vec<u8>,
}

/// What a start takes from a snapshot of the member's consensus module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Restored {
    /// The snapshot's log position: where the member's read of its log begins.
    pub(crate) log_position: i64,
    /// The sessions that were open at that position.
    pub(crate) sessions: Vec<RestoredSession>,
    /// The id the next session is given.
    pub(crate) next_session_id: i64,
}

/// The newest snapshot of the member's consensus module that `file` lists and that
/// can be read whole, and how many newer ones were passed over (`spec/cluster.md`, "A
/// member's start"; ADR 0130). `held` says whether the catalog holds a recording as a
/// snapshot's: of the snapshot stream, stopped, with bytes. A listed snapshot is
/// passed over when the catalog does not hold it so, when the driver does not play
/// it back within [`LOAD_TIMEOUT`], or when what it holds is not the snapshot the
/// list names: a begin marker of the consensus module's type with the entry's log
/// position and term, the sessions, the module's state and an end marker. `takes`
/// is the member's table of sessions: it says whether the table took the snapshot's
/// state whole, and a snapshot it refuses is passed over as the others are, so
/// that a member never begins at a snapshot's position with another state than
/// that snapshot's.
pub(crate) fn load(
    client: &mut Cavalry,
    file: &SnapshotsFile,
    mut held: impl FnMut(i64) -> bool,
    takes: impl FnMut(&Restored) -> bool,
) -> (Option<Restored>, u64) {
    newest(
        file.entries(),
        |listed| {
            held(listed.recording_id)
                .then(|| read_recording(client, listed.recording_id))
                .flatten()
        },
        takes,
    )
}

/// The rule of [`load`] on a list, with the driver left out: `read` gives the
/// messages of a listed snapshot's recording, or `None` when it cannot be read. The
/// newest entry of the consensus module whose messages are the snapshot it names,
/// and which `takes` takes, is the one a start begins from. Each newer entry of the
/// consensus module is counted as passed over. An entry of a service is not the
/// module's: it is not read and not counted.
fn newest(
    entries: &[ListedSnapshot],
    mut read: impl FnMut(&ListedSnapshot) -> Option<Vec<Vec<u8>>>,
    mut takes: impl FnMut(&Restored) -> bool,
) -> (Option<Restored>, u64) {
    let mut passed_over = 0u64;
    for listed in entries
        .iter()
        .rev()
        .filter(|listed| listed.service_id == CONSENSUS_MODULE)
    {
        let restored = read(listed)
            .and_then(|messages| restore(listed, &messages))
            .filter(|restored| takes(restored));
        match restored {
            Some(restored) => return (Some(restored), passed_over),
            None => passed_over = passed_over.saturating_add(1),
        }
    }
    (None, passed_over)
}

/// The messages of a snapshot's recording, played back whole on the log replay
/// stream, which the module's read of its log uses after it; `None` when the driver
/// refuses the replay, does not end it within [`LOAD_TIMEOUT`], or plays more
/// messages than a snapshot holds.
fn read_recording(client: &mut Cavalry, recording_id: i64) -> Option<Vec<Vec<u8>>> {
    let mut subscription = client
        .add_subscription(SNAPSHOT_CHANNEL, LOG_REPLAY_STREAM_ID)
        .ok()?;
    let replay = client
        .start_replay(
            recording_id,
            NULL_POSITION,
            NULL_LENGTH,
            LOG_REPLAY_STREAM_ID,
            SNAPSHOT_CHANNEL,
        )
        .ok()?;
    let session = replay.session_id();
    let deadline = Instant::now() + LOAD_TIMEOUT;
    let mut messages: Vec<Vec<u8>> = Vec::with_capacity(MAX_MESSAGES);
    let mut too_many = false;
    // How far the replay's image has been read, kept from each poll that still had
    // the image. The driver withdraws a replay's image once its subscriber has read
    // it to the end of stream, which can be before `replay stopped` reaches this
    // client: the image is then gone by the time the end is known
    // (`spec/client-protocol.md`, "Commands", start replay).
    let mut reached = 0i64;
    loop {
        let polled = subscription.poll(
            |bytes, header| {
                if header.session_id() != session {
                    return;
                }
                if messages.len() < MAX_MESSAGES {
                    messages.push(bytes.to_vec());
                } else {
                    too_many = true;
                }
            },
            MAX_MESSAGES,
        );
        // A poll that fails ends the read, and the replay is stopped as at the
        // deadline: the driver plays it to nobody otherwise, for as long as the
        // module's client lives.
        let Ok(read) = polled else {
            let _ = replay.stop(client);
            return None;
        };
        if let Some(image) = subscription
            .images()
            .iter()
            .find(|image| image.session_id() == session)
        {
            reached = reached.max(image.position());
        }
        // The replay has ended, and its image was read to where it ended.
        if let Some((end, _)) = replay.ended()
            && reached >= end
        {
            break;
        }
        if too_many || Instant::now() >= deadline {
            let _ = replay.stop(client);
            return None;
        }
        if read == 0 {
            std::thread::sleep(Duration::from_micros(50));
        }
    }
    (!too_many).then_some(messages)
}

/// What the messages of a snapshot's recording give a start, when they are the
/// snapshot `listed` names; `None` when they are not.
fn restore(listed: &ListedSnapshot, messages: &[Vec<u8>]) -> Option<Restored> {
    let is_marker = |bytes: &Vec<u8>, expected: SnapshotMark| {
        matches!(
            Message::parse(Stream::Snapshot, bytes),
            Ok(Message::SnapshotMarker {
                type_id: SNAPSHOT_TYPE_ID,
                log_position,
                leadership_term_id,
                index: 0,
                mark,
                ..
            }) if mark == expected
                && log_position == listed.log_position
                && leadership_term_id == listed.leadership_term_id
        )
    };
    let (first, rest) = messages.split_first()?;
    let (last, rest) = rest.split_last()?;
    let (module, sessions) = rest.split_last()?;
    if !is_marker(first, SnapshotMark::Begin) || !is_marker(last, SnapshotMark::End) {
        return None;
    }
    let Ok(Message::ConsensusModule {
        next_session_id, ..
    }) = Message::parse(Stream::Snapshot, module)
    else {
        return None;
    };
    // A member writes its open sessions in the rising order of their ids, each
    // below the id the next session is given.
    let mut restored: Vec<RestoredSession> = Vec::with_capacity(sessions.len());
    for bytes in sessions {
        let Ok(Message::ClusterSession {
            cluster_session_id,
            correlation_id,
            opened_log_position,
            response_channel,
            ..
        }) = Message::parse(Stream::Snapshot, bytes)
        else {
            return None;
        };
        let above = restored
            .last()
            .is_none_or(|before| before.id < cluster_session_id);
        if !above || cluster_session_id < 0 || cluster_session_id >= next_session_id {
            return None;
        }
        restored.push(RestoredSession {
            id: cluster_session_id,
            correlation_id,
            opened_log_position,
            response_channel: response_channel.to_vec(),
        });
    }
    Some(Restored {
        log_position: listed.log_position,
        sessions: restored,
        next_session_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One entry of a snapshots file, of the consensus module unless `service_id`
    /// says otherwise.
    fn listed(recording_id: i64, service_id: i32, log_position: i64) -> ListedSnapshot {
        ListedSnapshot {
            recording_id,
            leadership_term_id: 3,
            term_base_log_position: 0,
            log_position,
            timestamp: 1_700_000_000_000,
            service_id,
        }
    }

    fn bytes(message: &Message<'_>) -> Vec<u8> {
        let mut scratch = [0u8; SCRATCH_LENGTH];
        let length = message
            .write(&mut scratch)
            .expect("a message of a snapshot");
        scratch[..length].to_vec()
    }

    fn marker(listed: &ListedSnapshot, mark: SnapshotMark) -> Vec<u8> {
        marker_of(SNAPSHOT_TYPE_ID, listed, mark)
    }

    fn marker_of(type_id: i64, listed: &ListedSnapshot, mark: SnapshotMark) -> Vec<u8> {
        bytes(&Message::SnapshotMarker {
            type_id,
            log_position: listed.log_position,
            leadership_term_id: listed.leadership_term_id,
            index: 0,
            mark,
            time_unit: TimeUnit::Millis,
            app_version: 0,
        })
    }

    fn session(id: i64) -> Vec<u8> {
        bytes(&Message::ClusterSession {
            cluster_session_id: id,
            correlation_id: 100 + id,
            opened_log_position: 32 * id,
            time_of_last_activity: NULL_VALUE,
            close_reason: CLOSE_REASON_NONE,
            response_stream_id: EGRESS_STREAM_ID,
            response_channel: b"cavalry:udp?endpoint=127.0.0.1:9040",
        })
    }

    fn module(next_session_id: i64) -> Vec<u8> {
        bytes(&Message::ConsensusModule {
            next_session_id,
            next_service_session_id: 0,
            log_service_session_id: 0,
            pending_message_capacity: 0,
        })
    }

    /// A snapshot as a member writes it: a marker, the sessions, the module, a marker.
    fn snapshot(listed: &ListedSnapshot, ids: &[i64], next_session_id: i64) -> Vec<Vec<u8>> {
        let mut messages = vec![marker(listed, SnapshotMark::Begin)];
        messages.extend(ids.iter().map(|id| session(*id)));
        messages.push(module(next_session_id));
        messages.push(marker(listed, SnapshotMark::End));
        messages
    }

    #[test]
    fn a_snapshot_as_a_member_writes_it_gives_its_sessions_and_the_next_id() {
        let entry = listed(7, CONSENSUS_MODULE, 4096);
        let restored = restore(&entry, &snapshot(&entry, &[0, 2, 5], 6)).expect("the snapshot");
        assert_eq!(restored.log_position, 4096);
        assert_eq!(restored.next_session_id, 6);
        let ids: Vec<i64> = restored.sessions.iter().map(|session| session.id).collect();
        assert_eq!(ids, [0, 2, 5]);
        let first = restored.sessions.first().expect("a session");
        assert_eq!(first.correlation_id, 100);
        assert_eq!(first.opened_log_position, 0);
        assert_eq!(
            first.response_channel,
            b"cavalry:udp?endpoint=127.0.0.1:9040"
        );
        // No session open: the two markers and the module's state.
        let none = restore(&entry, &snapshot(&entry, &[], 0)).expect("the snapshot");
        assert!(none.sessions.is_empty());
        assert_eq!(none.next_session_id, 0);
    }

    #[test]
    fn a_recording_that_is_not_the_listed_snapshot_gives_nothing() {
        let entry = listed(7, CONSENSUS_MODULE, 4096);
        let whole = snapshot(&entry, &[0, 1], 2);
        assert!(restore(&entry, &whole).is_some());
        // Nothing, and what was written cut short at each message.
        for kept in 0..whole.len() {
            assert!(restore(&entry, &whole[..kept]).is_none(), "{kept} messages");
        }
        // Another position's snapshot, and another term's.
        let mut other = entry;
        other.log_position = 8192;
        assert!(restore(&other, &whole).is_none());
        let mut other = entry;
        other.leadership_term_id = 4;
        assert!(restore(&other, &whole).is_none());
        // The markers the wrong way round, and markers of a service's type id.
        let mut swapped = whole.clone();
        swapped.swap(0, 4);
        assert!(restore(&entry, &swapped).is_none());
        let mut services = whole.clone();
        services[0] = marker_of(2, &entry, SnapshotMark::Begin);
        services[4] = marker_of(2, &entry, SnapshotMark::End);
        assert!(restore(&entry, &services).is_none());
        // The module's state before a session, and no module's state at all.
        let mut moved = whole.clone();
        moved.swap(2, 3);
        assert!(restore(&entry, &moved).is_none());
        let mut without = whole.clone();
        without.remove(3);
        assert!(restore(&entry, &without).is_none());
        // Bytes that are no message of the snapshot stream.
        let mut damaged = whole;
        damaged[1] = vec![0xff; 12];
        assert!(restore(&entry, &damaged).is_none());
    }

    #[test]
    fn sessions_that_a_member_does_not_write_so_give_nothing() {
        let entry = listed(7, CONSENSUS_MODULE, 4096);
        // In the rising order of their ids, each below the next session id.
        assert!(restore(&entry, &snapshot(&entry, &[1, 4], 5)).is_some());
        // One session twice, and two out of order.
        assert!(restore(&entry, &snapshot(&entry, &[1, 1], 5)).is_none());
        assert!(restore(&entry, &snapshot(&entry, &[4, 1], 5)).is_none());
        // A session whose id the next session would be given, and one above it.
        assert!(restore(&entry, &snapshot(&entry, &[1, 5], 5)).is_none());
        assert!(restore(&entry, &snapshot(&entry, &[1, 9], 5)).is_none());
        // An id no leader gives.
        assert!(restore(&entry, &snapshot(&entry, &[-1, 1], 5)).is_none());
    }

    #[test]
    fn a_start_takes_the_newest_snapshot_it_can_read_and_counts_the_ones_it_passed_over() {
        let entries = [
            listed(3, CONSENSUS_MODULE, 1024),
            listed(5, CONSENSUS_MODULE, 2048),
            // A service's snapshot, newer than the module's: not the module's to read.
            listed(6, 0, 2048),
            listed(8, CONSENSUS_MODULE, 4096),
        ];
        let read = |gone: &'static [i64]| {
            move |listed: &ListedSnapshot| {
                (!gone.contains(&listed.recording_id)).then(|| snapshot(listed, &[0, 1], 2))
            }
        };
        let position = |taken: &Option<Restored>| taken.as_ref().map(|taken| taken.log_position);
        let mut asked = Vec::new();
        let (taken, passed_over) = newest(
            &entries,
            |listed| {
                asked.push(listed.recording_id);
                read(&[])(listed)
            },
            |_| true,
        );
        assert_eq!((position(&taken), passed_over), (Some(4096), 0));
        // The newest is read first and no other is read; the service's is never read.
        assert_eq!(asked, [8]);
        // The newest cannot be read: the one before it, and one passed over.
        let (taken, passed_over) = newest(&entries, read(&[8]), |_| true);
        assert_eq!((position(&taken), passed_over), (Some(2048), 1));
        // Nor that one: the oldest, and two.
        let (taken, passed_over) = newest(&entries, read(&[8, 5]), |_| true);
        assert_eq!((position(&taken), passed_over), (Some(1024), 2));
        // None can be read: no snapshot, and three passed over.
        let (taken, passed_over) = newest(&entries, read(&[8, 5, 3]), |_| true);
        assert_eq!((position(&taken), passed_over), (None, 3));
        // No list, and a list of a service's snapshots alone: nothing to pass over.
        assert_eq!(newest(&[], read(&[]), |_| true), (None, 0));
        assert_eq!(
            newest(&[listed(6, 0, 2048)], read(&[]), |_| true),
            (None, 0)
        );
    }

    #[test]
    fn a_snapshot_the_table_does_not_take_is_passed_over_for_the_one_before_it() {
        let entries = [
            listed(3, CONSENSUS_MODULE, 1024),
            listed(8, CONSENSUS_MODULE, 4096),
        ];
        let read = |listed: &ListedSnapshot| Some(snapshot(listed, &[0, 1], 2));
        // The table refuses the newest: the member begins at the older one, with
        // that one's state, and never at the newest one's position.
        let mut offered = Vec::new();
        let (taken, passed_over) = newest(&entries, read, |restored| {
            offered.push(restored.log_position);
            restored.log_position != 4096
        });
        assert_eq!(offered, [4096, 1024]);
        assert_eq!(taken.map(|taken| taken.log_position), Some(1024));
        assert_eq!(passed_over, 1);
        // The table refuses both: no snapshot, and the read begins at position 0.
        let (taken, passed_over) = newest(&entries, read, |_| false);
        assert_eq!((taken, passed_over), (None, 2));
    }
}

/// What a snapshot does at its bound, one at a time, and when a step fails
/// (`spec/cluster.md`, "A snapshot that fails"), against a driver. Each test holds
/// the member's counts and what its snapshots file lists: a snapshot that fails is
/// counted, is in no list, leaves the member with no snapshot in hand, and the next
/// action's snapshot is taken.
#[cfg(test)]
mod failures {
    use std::path::PathBuf;

    use cavalry_driver::Driver;
    use cavalry_log::catalog::CatalogReader;

    use super::*;

    /// One member's snapshots over a driver of its own, with no session open: a
    /// snapshot is a marker, the module's state and a marker.
    struct Fixture {
        dir: PathBuf,
        driver: Driver,
        client: Cavalry,
        sessions: Sessions,
        snapshot: ModuleSnapshot,
    }

    /// The snapshot an action whose entry ends at `log_position` asks for, in term 0.
    fn due(log_position: i64) -> SnapshotDue {
        SnapshotDue {
            log_position,
            leadership_term_id: 0,
            term_base_log_position: 0,
            timestamp: 1_794_700_000_000,
        }
    }

    impl Fixture {
        fn new(test: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "cavalry-raft-module-snapshot-{}-{test}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            let (driver_dir, log_dir) = (dir.join("driver"), dir.join("log"));
            let driver = Driver::start(&driver_dir, &log_dir).expect("the driver starts");
            let mut client = Cavalry::connect_to(&driver_dir).expect("a client of the driver");
            // As a member's start does on a new log directory: the file opened, and no
            // snapshot to begin from.
            let file = SnapshotsFile::open(&log_dir, 0).expect("the snapshots file is created");
            let snapshot = ModuleSnapshot::open(&mut client, file, 0, (NULL_VALUE, 0))
                .expect("the member's snapshots");
            Self {
                dir,
                driver,
                client,
                sessions: Sessions::new(0, b"0=127.0.0.1:9000"),
                snapshot,
            }
        }

        fn begin(&mut self, log_position: i64) {
            self.snapshot.begin(&mut self.client, due(log_position));
        }

        /// Duty cycles of the snapshot until `done`, for 10 s at the most, and the
        /// error of the cycle that had one.
        fn run_until(
            &mut self,
            what: &str,
            done: impl Fn(&ModuleSnapshot) -> bool,
        ) -> Result<(), SnapshotsError> {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !done(&self.snapshot) {
                assert!(
                    Instant::now() < deadline,
                    "{what}: {:?}, {:?}",
                    self.snapshot.state,
                    self.snapshot.counts
                );
                if self.snapshot.do_work(&mut self.client, &self.sessions)? == 0 {
                    std::thread::sleep(Duration::from_micros(200));
                }
            }
            Ok(())
        }

        /// The log positions of the snapshots the member's file lists, in its order.
        fn listed(&self) -> Vec<i64> {
            self.snapshot
                .file
                .entries()
                .iter()
                .map(|entry| entry.log_position)
                .collect()
        }

        /// The recording of the snapshot in hand.
        fn recording_id(&self) -> i64 {
            self.snapshot
                .recording
                .as_ref()
                .expect("a snapshot is in hand")
                .recording_id()
        }

        /// Where `recording_id` stopped, once the driver's catalog says so. A snapshot
        /// that is given up asks its recording's stop and waits for no answer, and the
        /// driver starts no second recording of the publication before that stop.
        fn stopped(&self, recording_id: i64) -> i64 {
            let catalog = CatalogReader::open(&self.dir.join("log")).expect("the catalog opens");
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Ok(Some(stop)) = catalog.stop_position(recording_id) {
                    return stop;
                }
                assert!(
                    Instant::now() < deadline,
                    "recording {recording_id} did not stop"
                );
                std::thread::sleep(Duration::from_micros(200));
            }
        }

        /// The next action's snapshot is taken and listed: the member goes on.
        fn takes(&mut self, log_position: i64) {
            let before = self.snapshot.counts;
            self.begin(log_position);
            assert_eq!(self.snapshot.counts.begun, before.begun + 1);
            self.run_until("the next snapshot is listed", |snapshot| {
                snapshot.state == SnapshotState::Idle
            })
            .expect("the list is written");
            assert_eq!(self.listed().last(), Some(&log_position));
            assert_eq!(
                self.snapshot.counts,
                SnapshotCounts {
                    begun: before.begun + 1,
                    steps: before.steps + 1,
                    taken: before.taken + 1,
                    ..before
                }
            );
        }

        fn close(self) {
            let Self {
                dir,
                driver,
                client,
                snapshot,
                ..
            } = self;
            drop(snapshot);
            client.close().expect("the client closes");
            driver.close().expect("the driver closes");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// The bound: one snapshot at a time. An action read while a snapshot is in hand,
    /// while its messages are written and while its recording stops, asks for a
    /// snapshot that is not taken and is counted as given up. The one in hand is not
    /// replaced: it is listed, at its own position. The last of those actions is
    /// read once the driver has stopped the recording and before the snapshot has
    /// read that stop: the driver would start a recording for it then, so there the
    /// member's own check is all that keeps the snapshot in hand. An action at or
    /// below the newest listed position asks for nothing and counts nothing, as when
    /// a member reads its log again. The action after that is taken.
    #[test]
    fn a_snapshot_asked_for_while_one_is_in_hand_is_counted_and_the_one_in_hand_is_listed() {
        let mut f = Fixture::new("bound");
        f.begin(320);
        assert_eq!(f.snapshot.state, SnapshotState::Writing);
        assert_eq!(
            f.snapshot.counts,
            SnapshotCounts {
                begun: 1,
                ..SnapshotCounts::default()
            }
        );
        // A second action, while the messages are being written.
        f.begin(640);
        assert_eq!(f.snapshot.state, SnapshotState::Writing);
        assert_eq!(f.snapshot.due, due(320));
        assert_eq!(
            f.snapshot.counts,
            SnapshotCounts {
                begun: 1,
                failed: 1,
                ..SnapshotCounts::default()
            }
        );
        // A third, while the recording stops.
        f.run_until("the messages are written", |snapshot| {
            snapshot.state == SnapshotState::Stopping
        })
        .expect("no list is written yet");
        f.begin(960);
        assert_eq!(f.snapshot.state, SnapshotState::Stopping);
        assert_eq!(f.snapshot.due, due(320));
        assert_eq!(
            f.snapshot.counts,
            SnapshotCounts {
                begun: 1,
                steps: 1,
                failed: 2,
                ..SnapshotCounts::default()
            }
        );
        // A fourth, once the driver has stopped the recording and before the
        // snapshot has read that: the publication is recorded by nothing, and the
        // driver would start a recording of it.
        f.run_until("the recording's stop is asked", |snapshot| {
            snapshot.stop_asked
        })
        .expect("no list is written yet");
        f.stopped(f.recording_id());
        f.begin(1280);
        assert_eq!(f.snapshot.state, SnapshotState::Stopping);
        assert_eq!(f.snapshot.due, due(320));
        assert_eq!(
            f.snapshot.counts,
            SnapshotCounts {
                begun: 1,
                steps: 1,
                failed: 3,
                ..SnapshotCounts::default()
            }
        );
        // The one in hand is listed, at its own position.
        f.run_until("the snapshot is listed", |snapshot| {
            snapshot.state == SnapshotState::Idle
        })
        .expect("the list is written");
        assert_eq!(f.listed(), [320]);
        let counts = SnapshotCounts {
            begun: 1,
            steps: 1,
            taken: 1,
            failed: 3,
            ..SnapshotCounts::default()
        };
        assert_eq!(f.snapshot.counts, counts);
        // An action the newest listed snapshot covers: nothing begun, nothing counted.
        f.begin(320);
        f.begin(32);
        assert_eq!(f.snapshot.state, SnapshotState::Idle);
        assert_eq!(f.snapshot.counts, counts);
        f.takes(640);
        assert_eq!(f.listed(), [320, 640]);
        f.close();
    }

    /// A stop that the driver does not announce by the deadline ends the snapshot.
    /// Here the deadline has passed before the stop is asked, so nothing can have
    /// announced it. Nothing is listed, one failure is counted, no snapshot is in
    /// hand, and the recording was asked to stop: once it has, the next action's
    /// snapshot is taken.
    #[test]
    fn a_stop_not_announced_by_the_deadline_ends_the_snapshot_and_lists_nothing() {
        let mut f = Fixture::new("deadline");
        f.begin(320);
        f.run_until("the messages are written", |snapshot| {
            snapshot.state == SnapshotState::Stopping
        })
        .expect("no list is written yet");
        let recording_id = f.recording_id();
        f.snapshot.deadline = Instant::now();
        let work = f
            .snapshot
            .do_work(&mut f.client, &f.sessions)
            .expect("no list is written");
        assert_eq!(work, 0);
        assert_eq!(f.snapshot.state, SnapshotState::Idle);
        assert!(f.snapshot.recording.is_none());
        assert_eq!(
            f.snapshot.counts,
            SnapshotCounts {
                begun: 1,
                steps: 1,
                failed: 1,
                ..SnapshotCounts::default()
            }
        );
        assert!(f.listed().is_empty());
        assert_eq!(f.snapshot.newest_position(), NULL_VALUE);
        // The recording is left in the catalog, stopped, and in no list.
        f.stopped(recording_id);
        f.takes(640);
        assert_eq!(f.listed(), [640]);
        f.close();
    }

    /// A recording that stopped short of what was written is no snapshot: the guard
    /// behind "a snapshot counts only once its recording has stopped at the end of
    /// what was written". The snapshot is made to hold an end 32 bytes past what its
    /// recording has, as it would when the recorder's last write had failed and the
    /// driver announced the position last synced. The deadline is moved out of the
    /// way, so that the guard alone can end the snapshot. Nothing is listed, one
    /// failure is counted, and the next action's snapshot is taken.
    #[test]
    fn a_recording_that_stopped_short_of_what_was_written_is_not_listed() {
        let mut f = Fixture::new("short");
        f.begin(320);
        f.run_until("the recording's stop is asked", |snapshot| {
            snapshot.stop_asked
        })
        .expect("no list is written yet");
        let recording_id = f.recording_id();
        let written = f.snapshot.end;
        f.snapshot.end = written + 32;
        f.snapshot.deadline = Instant::now() + Duration::from_secs(60);
        f.run_until("the snapshot is given up", |snapshot| {
            snapshot.state == SnapshotState::Idle
        })
        .expect("no list is written");
        assert_eq!(
            f.snapshot.counts,
            SnapshotCounts {
                begun: 1,
                steps: 1,
                failed: 1,
                ..SnapshotCounts::default()
            }
        );
        assert!(f.listed().is_empty());
        assert!(f.snapshot.recording.is_none());
        // The recording stopped where the messages ended, below the end the snapshot
        // held: that difference is what the guard reads.
        assert_eq!(f.stopped(recording_id), written);
        f.takes(640);
        assert_eq!(f.listed(), [640]);
        f.close();
    }

    /// A list that refuses the snapshot's entry is the member's fault: the one error
    /// `do_work` returns. Behind the snapshot in hand, the file is made to list a
    /// snapshot of the consensus module at a higher position, so the snapshot's own
    /// entry no longer carries on from the newest one listed. The cycle that would
    /// list it returns the file's error, the failure is counted, no snapshot is in
    /// hand, and the file lists what it listed before.
    #[test]
    fn a_list_that_refuses_the_snapshot_is_returned_and_counted() {
        let mut f = Fixture::new("list");
        f.begin(320);
        f.snapshot
            .list_behind(ListedSnapshot {
                recording_id: 7,
                service_id: CONSENSUS_MODULE,
                leadership_term_id: 0,
                term_base_log_position: 0,
                log_position: 640,
                timestamp: 1_794_700_000_000,
            })
            .expect("the higher snapshot is listed");
        let error = f
            .run_until("the snapshot ends", |snapshot| {
                snapshot.state == SnapshotState::Idle
            })
            .expect_err("the list refuses the snapshot's entry");
        assert!(matches!(error, SnapshotsError::List { .. }), "{error}");
        assert_eq!(f.snapshot.state, SnapshotState::Idle);
        assert!(f.snapshot.recording.is_none());
        assert_eq!(
            f.snapshot.counts,
            SnapshotCounts {
                begun: 1,
                steps: 1,
                failed: 1,
                ..SnapshotCounts::default()
            }
        );
        assert_eq!(f.listed(), [640]);
        f.close();
    }
}
