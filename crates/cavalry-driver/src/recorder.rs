//! The recorder: the driver's second thread, which writes every live recording's
//! committed frames from the publication's log buffer into the durable log
//! (`spec/client-protocol.md`, "Commands"; `spec/durable-log.md`, "The driver"; ADR
//! 0025). The conductor never touches the disk: it hands the recorder each start, attach
//! and stop through a bounded queue, the recorder answers through a second one, and the
//! conductor sends the client's event when the answer arrives. On each duty cycle the
//! recorder reads its clock once, handles a bounded number of requests, visits every
//! live recording, takes one block of whole committed frames with
//! [`TermReader::block`], writes it with one positional write, stores the recording's
//! position with a release store, syncs once per second, or at once when a `sync
//! recording` command asks (ADR 0069), and stops a recording whose
//! publication reached its end of stream, whose write failed, or whose block scan found
//! a frame that fails a check. A block is taken exactly when the writer's position moved,
//! `cavalry-log`'s rule, so a failed write leaves the reader where it was. Nothing here
//! allocates per block, locks, or logs; the syscalls are the recorder's own writes and
//! syncs.
//!
//! One recording is not visited every duty cycle (ADR 0084). A block scan ends by
//! reading the word of the first frame that is not committed, which is the cache line
//! its publisher writes next, and a recorder that keeps up with a publisher would read
//! it every duty cycle. So after a block that ended there the recorder leaves the
//! recording alone for [`spec::RECORDER_TAIL_INTERVAL`]. A stop, a sync that was asked
//! for, and the driver's close write what is committed first, within
//! [`spec::RECORDER_REQUEST_BLOCKS`] blocks and one block's limit of bytes, so none of
//! them leaves behind what the interval was holding back.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};

use cavalry_log::catalog::CatalogError;
use cavalry_log::dir::{LogDir, Recovery, SlotWrite};
use cavalry_log::layout::Recording;
use cavalry_log::recorder::{RecorderError, RecordingWriter};
use cavalry_proto::frame::FRAME_ALIGNMENT;
use cavalry_shm::counters::Counter;
use cavalry_shm::log::{LogError, TermReader};
use cavalry_shm::spsc::{Consumer, Producer};

use crate::clock::EpochClock;
use crate::conductor::now_nanos;
use crate::idle::{IdleStrategy, Idler};
use crate::spec;

/// How many replies the recorder holds back when the reply queue is full: one per live
/// recording, which may stop on its own, and one per request handled in a cycle. With
/// the pending replies at this bound the recorder handles no request and stops no
/// recording until the conductor takes replies again; it keeps writing.
pub(crate) const PENDING_REPLIES: usize =
    spec::MAX_RECORDINGS + spec::MAX_COMMANDS_IN_PROGRESS + spec::RECORDER_REQUESTS_PER_CYCLE;

/// What a `start recording` command fixed about the recording, carried to the recorder
/// once: the descriptor's fields and a reader on the log at the start position.
pub(crate) struct Start {
    /// The publication's registration id, which names its log.
    pub publication_id: i64,
    /// The command's correlation id, which the reply carries back.
    pub correlation_id: i64,
    /// When the command was handled: the descriptor's start timestamp.
    pub start_timestamp: i64,
    /// The producer position when the command was handled: the start position.
    pub start_position: i64,
    /// The log buffer's initial term id.
    pub initial_term_id: i32,
    /// The log buffer's term length.
    pub term_length: i32,
    /// The log buffer's MTU length.
    pub mtu_length: i32,
    /// The publication's session id.
    pub session_id: i32,
    /// The publication's stream id.
    pub stream_id: i32,
    /// The channel, `cavalry:ipc` in this version.
    pub channel: String,
    /// The publisher's identity, `cavalry:ipc` in this version.
    pub source_identity: String,
    /// A reader on the publication's log at the start position; it keeps the mapping
    /// alive on the recorder's thread.
    pub reader: TermReader,
}

/// What the conductor asks the recorder.
pub(crate) enum Request {
    /// Take a catalog slot and create the files; answered with [`Reply::Ready`] or
    /// [`Reply::StartFailed`].
    Start(Start),
    /// The three counters the conductor allocated once the recording id was known; the
    /// recorder stores its positions and its bytes into them from now on. A recording
    /// already stopped drops them.
    Attach {
        /// The recording.
        recording_id: i64,
        /// Its `recording.position` counter.
        position: Counter,
        /// Its `recording.synced_position` counter.
        synced: Counter,
        /// Its `recording.bytes` gauge: the segment files' bytes on disk, stored at each
        /// roll (ADR 0031).
        bytes: Counter,
    },
    /// Stop a recording at the position written, once what is committed has been
    /// written (ADR 0084); answered with [`Reply::Stopped`], or
    /// [`Reply::StopIgnored`] when the recording already stopped on its own.
    Stop {
        /// The recording.
        recording_id: i64,
        /// The `stop recording` command's correlation id, when a command asked.
        correlation_id: Option<i64>,
    },
    /// Write what is committed of a recording and sync what is written, now, a `sync
    /// recording` command's request (ADR 0069, ADR 0084); answered with
    /// [`Reply::SyncDone`].
    Sync {
        /// The recording.
        recording_id: i64,
        /// The command's correlation id, which the reply carries back.
        correlation_id: i64,
    },
    /// A retention command's catalog write, one slot and a sync, on the thread that owns
    /// the catalog writer (ADR 0031); answered with [`Reply::CatalogDone`].
    Catalog {
        /// The command's correlation id, which the conductor's registry entry carries.
        correlation_id: i64,
        /// The stopped recording.
        recording_id: i64,
        /// The write.
        write: CatalogWrite,
    },
}

/// The catalog writes a retention command makes, in the order its hops run
/// (`spec/durable-log.md`, "The driver"; ADR 0031).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CatalogWrite {
    /// A truncate's first step: the stop fields as −1 and −1, "live, or being cut".
    OpenCut,
    /// A truncate's third step: the stop recovery would rebuild for the files as cut.
    CloseCut {
        /// The position the recording was cut to.
        stop_position: i64,
        /// The timestamp of the last index entry left.
        stop_timestamp: i64,
    },
    /// A purge's write: the retained position under its checksum, the slot migrated to
    /// version 1's form first when it is in version 0's.
    Retain {
        /// The new retained position, a segment base.
        position: i64,
    },
    /// A delete's write: state 3.
    Delete,
    /// The catalog side of a slot's repair after a command's file work failed: the one
    /// write recovery's rules ended in, which the replayer computed with the files
    /// (`Request::Repair` there), synced here on the thread that owns the catalog (ADR
    /// 0031). Nothing else of the recovery pass runs on this thread.
    Repair(SlotWrite),
}

/// Why a start failed, for the error event's code.
pub(crate) enum StartFailure {
    /// Every catalog slot is used: error code 10 and `recording.catalog_full`.
    CatalogFull,
    /// The time index or the first segment file could not be created: error code 12 and
    /// `recording.start_failed`, with the message.
    Files(String),
}

/// What a `sync recording` request came to (`spec/client-protocol.md`, "Commands"; ADR
/// 0069).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SyncOutcome {
    /// Everything the recorder had written of the recording when it took the request
    /// is covered by a sync that returned success: this request's, or an earlier one
    /// when nothing was unsynced. Or the recorder no longer holds the recording, which
    /// has stopped, and there is nothing to sync: `operation success` either way.
    Done,
    /// The sync failed, with the error's text: error code 12. The recording is marked
    /// as one whose sync failed and is given up on the recorder's next visit with room
    /// for the reply (ADR 0065).
    Failed(String),
    /// A sync of the recording had failed before and the recorder has not yet given
    /// it up; no sync was made, since a sync is not asked a second time about bytes a
    /// sync has failed on: error code 12.
    AlreadyFailed,
}

/// What the recorder answers.
pub(crate) enum Reply {
    /// The catalog slot is valid and synced, and the time index and the first segment
    /// file exist.
    Ready {
        /// The publication being recorded.
        publication_id: i64,
        /// The start command's correlation id.
        correlation_id: i64,
        /// The slot the recording took.
        recording_id: i64,
        /// The catalog's slot count after the start: the next recording id.
        next_recording_id: i64,
    },
    /// The start failed; nothing records.
    StartFailed {
        /// The publication that was to be recorded.
        publication_id: i64,
        /// The start command's correlation id.
        correlation_id: i64,
        /// Why.
        failure: StartFailure,
        /// The catalog's slot count after the attempt.
        next_recording_id: i64,
    },
    /// A recording stopped: the catalog's stop fields are written and synced, or the
    /// stop itself failed and the slot stays live for the next open's recovery.
    Stopped {
        /// The recording.
        recording_id: i64,
        /// Its publication.
        publication_id: i64,
        /// The position written, which the catalog holds as the stop position; or, when
        /// the stop itself failed, the position the last sync before it made durable
        /// (ADR 0064).
        stop_position: i64,
        /// The `stop recording` command's correlation id, when a command asked.
        correlation_id: Option<i64>,
    },
    /// A stop named a recording that had already stopped on its own, whose
    /// [`Reply::Stopped`] went out before.
    StopIgnored {
        /// The command's correlation id, when a command asked.
        correlation_id: Option<i64>,
    },
    /// A `sync recording` request was handled.
    SyncDone {
        /// The command's correlation id.
        correlation_id: i64,
        /// The recording the command named.
        recording_id: i64,
        /// What the request came to.
        outcome: SyncOutcome,
    },
    /// A retention command's catalog write is done, or failed with the message.
    CatalogDone {
        /// The command.
        correlation_id: i64,
        /// The recording.
        recording_id: i64,
        /// The write that ran.
        write: CatalogWrite,
        /// `Ok`, or the error's text for the client's error event.
        result: Result<(), String>,
        /// For a repair, what the catalog side did, for the counters; nothing otherwise.
        repaired: Recovery,
    },
}

/// One recording the recorder writes.
struct Live {
    recording_id: i64,
    publication_id: i64,
    reader: TermReader,
    writer: RecordingWriter,
    position: Option<Counter>,
    synced: Option<Counter>,
    /// The `recording.bytes` gauge, stored when the segment count changes.
    bytes: Option<Counter>,
    /// The segment count last stored in `bytes`.
    segments_stored: i64,
    /// A sync of this recording failed: the periodic one, one a `sync recording`
    /// asked for, or the one a write makes of a segment it closes. Nothing more of it
    /// is written or synced, and it is given
    /// up, with no stop run, as soon as there is room for the reply: a sync asked
    /// again about bytes a sync has failed on can return success and write nothing
    /// (ADR 0065).
    sync_failed: bool,
    /// A write of this recording failed with no sync failing, a full disk's refusal
    /// for one: the recording is stopped at the position written, as ADR 0025 has it,
    /// and counted here once whether or not its stop then succeeds.
    write_failed: bool,
    /// When the recorder last wrote a block of this recording that ended at the
    /// publisher's tail: it does not scan the recording again until
    /// [`spec::RECORDER_TAIL_INTERVAL`] after it (ADR 0084). `i64::MIN` before the
    /// first.
    tail_read: i64,
}

/// What one attempt to take a block of a recording did ([`Live::take`]).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Taken {
    /// No frame is committed at the position.
    Nothing,
    /// A block was written, and its scan ended at a frame that is not committed: the
    /// recorder has reached its publisher, and has read the word the publisher writes
    /// next.
    Wrote,
    /// A block was written, and it ended for its limit, at its term's end or at a
    /// padding frame: more may be committed.
    Filled,
    /// The frame at the position is committed and fails a check of `spec/frame.md`.
    Rejected,
    /// The write failed; `sync_failed` says whether it was the sync of a segment the
    /// write closed.
    Failed,
}

impl Live {
    /// Takes one block of at most `limit` bytes of committed frames at the position
    /// and writes it: the position, its counter and the bytes gauge move when the
    /// block was taken. A committed frame that fails a check of `spec/frame.md` fails
    /// the whole scan, so the limit is halved until the block before that frame
    /// checks, and when the frame is the next one it is [`Taken::Rejected`]
    /// (invariant 1).
    fn take(&mut self, limit: usize, stamp: i64) -> Taken {
        let mut limit = limit;
        let block = loop {
            match self.reader.block(limit) {
                Ok(block) => break block,
                // A frame within the limit fails its check: the frames before it.
                Err(LogError::Frame(_) | LogError::Mismatch { .. }) if limit > FRAME_ALIGNMENT => {
                    limit = (limit / 2).max(FRAME_ALIGNMENT);
                }
                Err(_) => return Taken::Rejected,
            }
        };
        if block.is_empty() {
            return Taken::Nothing;
        }
        let reached = block.uncommitted;
        let end = block.end();
        let before = self.writer.position();
        let written = self.writer.write(block.bytes, block.length, stamp);
        // The block was taken exactly when the position moved, whatever `write`
        // returned; a block not taken is scanned again next cycle.
        let moved = self.writer.position() != before;
        if moved {
            // `end` is the end of a block of the reader's current term, a multiple of
            // 32 at most the term's end, which `seek` accepts: it cannot fail without
            // the reader and the writer disagreeing from here on.
            let sought = self.reader.seek(end);
            debug_assert!(sought.is_ok(), "the block's end is in the reader's term");
            if let Some(position) = &self.position {
                position.store_release(self.writer.position());
            }
            let segments = self.writer.segments_written();
            if segments != self.segments_stored
                && let Some(bytes) = &self.bytes
            {
                bytes.store_release(
                    segments
                        .saturating_mul(i64::from(self.writer.descriptor().segment_file_length())),
                );
                self.segments_stored = segments;
            }
        }
        if written.is_err() {
            // A write that failed in the sync of the segment it closed is a sync that
            // failed.
            self.sync_failed = self.writer.sync_failed();
            return Taken::Failed;
        }
        match (moved, reached) {
            (false, _) => Taken::Nothing,
            (true, true) => Taken::Wrote,
            (true, false) => Taken::Filled,
        }
    }
}

/// What one visit to a live recording did.
enum Step {
    /// Nothing was committed, due or stopped.
    Idle,
    /// A block was written or a sync ran.
    Worked,
    /// The recording stopped and left the list.
    Stopped,
}

/// The recorder's state: the log directory it owns, the queues, and the live recordings.
pub(crate) struct Recorder {
    log: LogDir,
    requests: Consumer<Request>,
    replies: Producer<Reply>,
    live: Vec<Live>,
    /// Replies the reply queue had no room for, offered again next cycle.
    pending: VecDeque<Reply>,
    /// The epoch clock, for what a recording is stamped with: its start, its stop and
    /// its time index. Every interval here is measured on the `now` a duty cycle is
    /// handed, the nano clock (ADR 0109).
    epoch: EpochClock,
    write_failed: Counter,
    rejected_frame: Counter,
    idler: Idler,
}

impl Recorder {
    /// A recorder over `log`, opened and recovered by the caller, with the two static
    /// counters it moves: `recording.write_failed` and `log_buffer.rejected_frame`.
    pub(crate) fn new(
        log: LogDir,
        requests: Consumer<Request>,
        replies: Producer<Reply>,
        write_failed: Counter,
        rejected_frame: Counter,
        idle: IdleStrategy,
    ) -> Self {
        Self {
            log,
            requests,
            replies,
            live: Vec::with_capacity(spec::MAX_RECORDINGS),
            pending: VecDeque::with_capacity(PENDING_REPLIES),
            epoch: EpochClock::new(),
            write_failed,
            rejected_frame,
            idler: Idler::new(idle),
        }
    }

    /// The recorder's loop: duty cycles until `stop`, then every live recording is
    /// stopped: at the position written, with a clean stop in the catalog that the next
    /// open has nothing to repair, except a recording one of whose syncs failed or
    /// whose stop's own writes or syncs fail, which is announced at the position last
    /// synced and whose slot the next open repairs (ADR 0064, ADR 0065).
    pub(crate) fn run(mut self, stop: &AtomicBool) {
        while !stop.load(Ordering::Acquire) {
            let now = now_nanos();
            if self.duty_cycle(now) == 0 {
                self.idler.idle();
            } else {
                self.idler.reset();
            }
        }
        // The conductor stops on the same flag and takes no more replies, so a stop here
        // keeps its reply only while the pending replies have room; the catalog's stop
        // is what the next open reads.
        let now = now_nanos();
        while let Some(last) = self.live.len().checked_sub(1) {
            self.flush_replies();
            self.stop_at_tail(last, now, None);
        }
        self.flush_replies();
    }

    /// One duty cycle; returns how much work it found, 0 when the recorder may idle. The
    /// nano clock is read once by the caller and passed down as `now`; what is stamped
    /// takes the epoch clock at that `now`.
    pub(crate) fn duty_cycle(&mut self, now: i64) -> usize {
        let mut work = self.flush_replies();
        for _ in 0..spec::RECORDER_REQUESTS_PER_CYCLE {
            if self.pending.len() >= PENDING_REPLIES {
                break;
            }
            let Some(request) = self.requests.poll() else {
                break;
            };
            self.handle(request, now);
            work += 1;
        }
        let mut index = 0;
        while index < self.live.len() {
            let may_stop = self.pending.len() < PENDING_REPLIES;
            match self.record(index, now, may_stop) {
                Step::Stopped => work += 1,
                Step::Worked => {
                    work += 1;
                    index += 1;
                }
                Step::Idle => index += 1,
            }
        }
        work + self.flush_replies()
    }

    fn handle(&mut self, request: Request, now: i64) {
        match request {
            Request::Start(start) => self.start(start, now),
            Request::Attach {
                recording_id,
                position,
                synced,
                bytes,
            } => {
                if let Some(live) = self
                    .live
                    .iter_mut()
                    .find(|live| live.recording_id == recording_id)
                {
                    position.store_release(live.writer.position());
                    synced.store_release(live.writer.synced_position());
                    let segments = live.writer.segments_written();
                    bytes.store_release(
                        segments.saturating_mul(i64::from(
                            live.writer.descriptor().segment_file_length(),
                        )),
                    );
                    live.segments_stored = segments;
                    live.position = Some(position);
                    live.synced = Some(synced);
                    live.bytes = Some(bytes);
                }
            }
            Request::Catalog {
                correlation_id,
                recording_id,
                write,
            } => {
                let mut repaired = Recovery::default();
                let result = match write {
                    CatalogWrite::OpenCut => self
                        .log
                        .catalog_mut()
                        .open_cut(recording_id)
                        .map_err(|error| error.to_string()),
                    CatalogWrite::CloseCut {
                        stop_position,
                        stop_timestamp,
                    } => self
                        .log
                        .catalog_mut()
                        .stop(recording_id, stop_position, stop_timestamp)
                        .map_err(|error| error.to_string()),
                    CatalogWrite::Retain { position } => self
                        .log
                        .catalog_mut()
                        .retain(recording_id, position)
                        .map_err(|error| error.to_string()),
                    CatalogWrite::Delete => self
                        .log
                        .catalog_mut()
                        .delete(recording_id)
                        .map_err(|error| error.to_string()),
                    CatalogWrite::Repair(ref slot_write) => self
                        .log
                        .apply_repair(recording_id, slot_write)
                        .map(|recovery| repaired = recovery)
                        .map_err(|error| error.to_string()),
                };
                self.pending.push_back(Reply::CatalogDone {
                    correlation_id,
                    recording_id,
                    write,
                    result,
                    repaired,
                });
            }
            Request::Sync {
                recording_id,
                correlation_id,
            } => {
                // What is committed is written first (ADR 0084). A write that fails
                // there stops the recording on its next visit, as one in `record`
                // does; a frame that fails its check is met and counted there too.
                if let Some(index) = self.index_of(recording_id) {
                    self.write_blocks(index, now, spec::RECORDER_REQUEST_BLOCKS);
                }
                let outcome = self.sync_now(recording_id, now);
                self.pending.push_back(Reply::SyncDone {
                    correlation_id,
                    recording_id,
                    outcome,
                });
            }
            Request::Stop {
                recording_id,
                correlation_id,
            } => match self.index_of(recording_id) {
                Some(index) => {
                    self.stop_at_tail(index, now, correlation_id);
                }
                None => self
                    .pending
                    .push_back(Reply::StopIgnored { correlation_id }),
            },
        }
    }

    /// Where the live recording `recording_id` is in the list.
    fn index_of(&self, recording_id: i64) -> Option<usize> {
        self.live
            .iter()
            .position(|live| live.recording_id == recording_id)
    }

    /// Writes what is committed at the position of the recording at `index`, block
    /// after block, until a scan finds no committed frame, `blocks` blocks or one
    /// block's limit of bytes ([`spec::RECORDER_BLOCK_LIMIT`]) are written, a frame
    /// fails its check, or a write fails: what a stop or a sync the recorder was asked
    /// for does first, with [`spec::RECORDER_REQUEST_BLOCKS`], so that neither leaves
    /// behind what the interval was holding back (ADR 0084). Several blocks, since a
    /// block ends with its term and a padding frame is a block of its own; one
    /// block's bytes, since that is what a duty cycle writes of a recording. Returns
    /// what the last attempt did. A write that fails marks the recording, which
    /// `record` or the stop that follows then stops; a recording so marked, or one of
    /// whose syncs failed, is not written again (ADR 0065). When the last scan reached
    /// the publisher the interval starts, as it does in `record`.
    pub(crate) fn write_blocks(&mut self, index: usize, now: i64, blocks: usize) -> Taken {
        let stamp = self.epoch.at(now);
        let live = &mut self.live[index];
        if live.sync_failed || live.write_failed {
            return Taken::Nothing;
        }
        let from = live.writer.position();
        let limit = i64::try_from(spec::RECORDER_BLOCK_LIMIT).unwrap_or(i64::MAX);
        let mut last = Taken::Nothing;
        for _ in 0..blocks {
            let written = live.writer.position().saturating_sub(from);
            let Some(left) = usize::try_from(limit.saturating_sub(written))
                .ok()
                .filter(|left| *left > 0)
            else {
                break;
            };
            last = live.take(left, stamp);
            match last {
                Taken::Wrote | Taken::Filled => {}
                Taken::Nothing | Taken::Rejected => break,
                Taken::Failed => {
                    live.write_failed = true;
                    break;
                }
            }
        }
        // The last scan read the word at the publisher's tail, and something was
        // written: the recorder has just been where the interval keeps it from.
        if live.writer.position() != from && matches!(last, Taken::Wrote | Taken::Nothing) {
            live.tail_read = now;
        }
        last
    }

    /// A stop of the recording at `index` that first writes what is committed
    /// ([`Recorder::write_blocks`]): a `stop recording`, and the driver's close. A
    /// frame that fails its check there is counted here, since the recording is not
    /// visited again.
    fn stop_at_tail(&mut self, index: usize, now: i64, correlation_id: Option<i64>) -> Step {
        if self.write_blocks(index, now, spec::RECORDER_REQUEST_BLOCKS) == Taken::Rejected {
            self.rejected_frame.increment();
        }
        self.stop_at(index, now, correlation_id)
    }

    /// A `sync recording` request (ADR 0069): syncs what is written of the live
    /// recording `recording_id` and not yet synced, and stores the synced position. A
    /// recording the recorder no longer holds has stopped, and nothing is done. One
    /// whose sync has already failed is not synced again (ADR 0065). A sync that fails
    /// here marks the recording as the periodic sync's failure does in `record`, which
    /// gives it up on its next visit with room for the reply.
    fn sync_now(&mut self, recording_id: i64, now: i64) -> SyncOutcome {
        let Some(live) = self
            .live
            .iter_mut()
            .find(|live| live.recording_id == recording_id)
        else {
            return SyncOutcome::Done;
        };
        if live.sync_failed {
            return SyncOutcome::AlreadyFailed;
        }
        match live.writer.sync_now(now) {
            Ok(synced) => {
                if synced && let Some(counter) = &live.synced {
                    counter.store_release(live.writer.synced_position());
                }
                SyncOutcome::Done
            }
            Err(error) => {
                live.sync_failed = true;
                SyncOutcome::Failed(error.to_string())
            }
        }
    }

    /// Takes the catalog slot and creates the files (`LogDir::start`), or says why not.
    fn start(&mut self, start: Start, now: i64) {
        let next = |log: &LogDir| i64::try_from(log.catalog().slots()).unwrap_or(i64::MAX);
        let publication_id = start.publication_id;
        let correlation_id = start.correlation_id;
        if self.live.len() >= spec::MAX_RECORDINGS {
            // The conductor bounds its registry to the same number, so this is a bug
            // there; refused rather than grown.
            self.pending.push_back(Reply::StartFailed {
                publication_id,
                correlation_id,
                failure: StartFailure::Files(format!(
                    "the recorder holds {} recordings, its capacity",
                    spec::MAX_RECORDINGS
                )),
                next_recording_id: next(&self.log),
            });
            return;
        }
        let recording = Recording {
            recording_id: 0,
            start_timestamp: start.start_timestamp,
            start_position: start.start_position,
            initial_term_id: start.initial_term_id,
            term_length: start.term_length,
            mtu_length: start.mtu_length,
            session_id: start.session_id,
            stream_id: start.stream_id,
            channel: &start.channel,
            source_identity: &start.source_identity,
        };
        // The start timestamp is the conductor's reading of the epoch clock and what
        // follows is this thread's: never one below it, so that no entry of the time
        // index, and no stop, is stamped before the start.
        let stamp = self.epoch.at(now).max(start.start_timestamp);
        match self.log.start(&recording, stamp) {
            Ok(mut writer) => {
                // The writer was stamped with the epoch clock; its sync interval runs on
                // the nano clock, from the start.
                writer.sync_from(now);
                let recording_id = writer.recording_id();
                self.live.push(Live {
                    bytes: None,
                    segments_stored: 0,
                    sync_failed: false,
                    write_failed: false,
                    recording_id,
                    publication_id,
                    reader: start.reader,
                    writer,
                    position: None,
                    synced: None,
                    tail_read: i64::MIN,
                });
                self.pending.push_back(Reply::Ready {
                    publication_id,
                    correlation_id,
                    recording_id,
                    next_recording_id: next(&self.log),
                });
            }
            Err(error) => {
                self.pending.push_back(Reply::StartFailed {
                    publication_id,
                    correlation_id,
                    failure: start_failure(error),
                    next_recording_id: next(&self.log),
                });
            }
        }
    }

    /// One visit to the live recording at `index`: a block if one is committed, a sync
    /// if one is due, a stop if the stream ended or a write failed, and the recording
    /// given up if a sync of its own failed. With `may_stop` false a stop waits for the
    /// next cycle, since there is no room for its reply; a recording whose sync failed
    /// waits too, and is neither written nor synced again while it does.
    fn record(&mut self, index: usize, now: i64, may_stop: bool) -> Step {
        let stamp = self.epoch.at(now);
        let live = &mut self.live[index];
        // A sync that failed, or a write that failed while a `sync recording` was
        // served: the recording is given up, or stopped, once there is room for the
        // reply.
        if live.sync_failed || live.write_failed {
            if !may_stop {
                return Step::Idle;
            }
            return self.stop_at(index, now, None);
        }
        let mut worked = false;
        // ADR 0084. A scan reads the word at the publisher's tail, the cache line the
        // publisher writes next, so after a block that ended there the recorder leaves
        // the recording alone for `RECORDER_TAIL_INTERVAL`. A block that ended for its
        // limit, its term or a padding frame did not reach the tail, and an empty
        // scan wrote nothing: neither starts the interval. The clock is the nano clock, which never goes
        // back; a difference below zero counts as the interval passed all the same.
        let since = now.saturating_sub(live.tail_read);
        if !(0..spec::RECORDER_TAIL_INTERVAL).contains(&since) {
            match live.take(spec::RECORDER_BLOCK_LIMIT, stamp) {
                Taken::Wrote => {
                    live.tail_read = now;
                    worked = true;
                }
                Taken::Filled => worked = true,
                Taken::Nothing => {
                    if live.reader.is_end_of_stream() {
                        if !may_stop {
                            return Step::Idle;
                        }
                        return self.stop_at(index, now, None);
                    }
                }
                // The frame is counted as a subscriber's report is, and the recording
                // ends at the last frame that checked (invariant 1).
                Taken::Rejected => {
                    if !may_stop {
                        return Step::Idle;
                    }
                    self.rejected_frame.increment();
                    return self.stop_at(index, now, None);
                }
                // A write that failed is tried again next cycle when there is no room
                // for the stop's reply now.
                Taken::Failed => {
                    if !may_stop {
                        return Step::Idle;
                    }
                    live.write_failed = true;
                    return self.stop_at(index, now, None);
                }
            }
        }
        match live.writer.sync(now) {
            Ok(true) => {
                if let Some(synced) = &live.synced {
                    synced.store_release(live.writer.synced_position());
                }
                worked = true;
            }
            Ok(false) => {}
            Err(_) => {
                live.sync_failed = true;
                if !may_stop {
                    return Step::Idle;
                }
                return self.stop_at(index, now, None);
            }
        }
        if worked { Step::Worked } else { Step::Idle }
    }

    /// Stops the recording at `index` at the position written: the stop entry, the
    /// syncs, the removal of a segment wholly past the stop and the catalog's stop
    /// fields (`LogDir::stop`). The reply carries the catalog's stop position, which
    /// the stop synced. A recording whose write failed with no sync failing is stopped
    /// so too (ADR 0025).
    ///
    /// Two stops announce the position last synced instead, read from the writer
    /// before anything else is done with it, and leave the slot live for the next
    /// open's recovery, whose stop is never below that position
    /// (`spec/client-protocol.md`, "Commands"). A stop whose own writes or syncs fail
    /// (ADR 0064). And the stop of a recording one of whose syncs already failed, for
    /// which `LogDir::stop` is not called at all: its syncs would run on files whose
    /// writeback has failed, where a second sync can return success and write
    /// nothing, and the catalog would then hold a stop over bytes that are not on the
    /// disk (ADR 0065). The position counter keeps the position written, what the
    /// operating system holds, and the synced counter the last sync.
    ///
    /// `recording.write_failed` counts the recording once, whichever of its writes,
    /// its syncs and its stop failed.
    fn stop_at(&mut self, index: usize, now: i64, correlation_id: Option<i64>) -> Step {
        let live = self.live.swap_remove(index);
        let synced_before = live.writer.synced_position();
        let stopped = if live.sync_failed {
            None
        } else {
            self.log
                .stop(live.writer, self.epoch.at(now))
                .ok()
                .map(|descriptor| descriptor.stop_position())
        };
        if live.sync_failed || live.write_failed || stopped.is_none() {
            self.write_failed.increment();
        }
        let stop_position = match stopped {
            Some(stopped) => {
                if let Some(synced) = &live.synced {
                    synced.store_release(stopped);
                }
                if let Some(position) = &live.position {
                    position.store_release(stopped);
                }
                stopped
            }
            None => synced_before,
        };
        // During duty cycles a stop happens only with room for its reply; only the
        // shutdown loop finds none, after the conductor has stopped reading.
        if self.pending.len() < PENDING_REPLIES {
            self.pending.push_back(Reply::Stopped {
                recording_id: live.recording_id,
                publication_id: live.publication_id,
                stop_position,
                correlation_id,
            });
        }
        Step::Stopped
    }

    /// Marks the live recording `recording_id` as one whose sync failed, as the two
    /// failure branches of `record` mark it: a test cannot make an `fdatasync` on the
    /// recorder's own descriptors fail, so it takes the branch's effect from here and
    /// lets the recorder's next visit do the rest.
    #[cfg(test)]
    pub(crate) fn fail_sync(&mut self, recording_id: i64) {
        if let Some(live) = self
            .live
            .iter_mut()
            .find(|live| live.recording_id == recording_id)
        {
            live.sync_failed = true;
        }
    }

    /// How many replies wait for room in the reply queue.
    #[cfg(test)]
    pub(crate) fn pending_replies(&self) -> usize {
        self.pending.len()
    }

    /// How many recordings the recorder writes.
    #[cfg(test)]
    pub(crate) fn live_recordings(&self) -> usize {
        self.live.len()
    }

    /// Offers the pending replies in order until the queue is full; returns how many went.
    fn flush_replies(&mut self) -> usize {
        let mut sent = 0;
        while let Some(reply) = self.pending.pop_front() {
            match self.replies.offer(reply) {
                Ok(()) => sent += 1,
                Err(reply) => {
                    self.pending.push_front(reply);
                    break;
                }
            }
        }
        sent
    }
}

/// The code a failed start is answered with: a full catalog is code 10, anything else
/// is code 12 with the error's message (`spec/client-protocol.md`, "Commands").
pub(crate) fn start_failure(error: RecorderError) -> StartFailure {
    match error {
        RecorderError::Catalog(CatalogError::Full) => StartFailure::CatalogFull,
        other => StartFailure::Files(other.to_string()),
    }
}

impl std::fmt::Debug for Recorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recorder")
            .field("log", &self.log)
            .field("live", &self.live.len())
            .field("pending_replies", &self.pending.len())
            .finish()
    }
}
