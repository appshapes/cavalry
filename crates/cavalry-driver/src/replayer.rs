//! The replayer: the driver's third thread, which plays a recording's frames from its
//! segment files into a replay's log buffer, where a subscription reads them as any
//! stream (`spec/client-protocol.md`, "Commands"; ADR 0030). The conductor never
//! touches the disk: it hands the replayer each start, attach and stop through a
//! bounded queue, the replayer answers through a second one, and the conductor creates
//! the replay's publication, sends the client's events and closes the publication when
//! the replay ends. On each duty cycle the replayer reads its clock once, handles a
//! bounded number of requests and visits every live replay: it reads one block of the
//! recording from its segment file, bounded by the recording's extent, the term, the
//! segment, the publication's limit plus an MTU, the replay's end plus an MTU and
//! 1 MiB, writes its frames into the replay log through a [`TermWriter`], which checks
//! each one and stops at the lower of the limit and the end, so that a frame that
//! begins below the end is played whole and the replay ends at the frame boundary after
//! it, and stores `replay.position` with a release store. A block holds whole every
//! frame a recorder could have written at its start, since each of its bounds is a
//! frame boundary or an MTU past the limit or the end, so a frame whose length runs
//! past the block, over the MTU or past the extent, is damage, `spec/frame.md`'s
//! `length` reason, and ends the replay with reason 3 as a frame that fails any check
//! does, rather than being read again for ever. A replay reads no byte the
//! recorder has not synced, so what it plays is what a restart would find (invariant
//! 1); a replay of a live recording waits for the synced position to move, and learns
//! the recording's stop from its catalog slot, read again once per second and whenever
//! a frame length of 0 is found below the extent. While it holds a replay the replayer
//! idles with the conductor's idle strategy; while it holds none, and no reply, it
//! parks until the conductor hands it a request and unparks it, or for a second, so a
//! replayer with nothing to play wakes no CPU. Nothing here allocates per block, locks
//! or logs; the syscalls are the replayer's own reads and opens, and a segment file is
//! opened on a path the replayer formats in place.

use std::collections::VecDeque;
use std::fs::File;
use std::io::Write as _;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use cavalry_log::catalog::{CatalogError, CatalogReader};
use cavalry_log::layout::{
    Descriptor, FRAME_ALIGNMENT, SEGMENT_SUFFIX, State, locate, segment_file_name,
};
use cavalry_proto::client_protocol::{NULL_LENGTH, NULL_POSITION, ReplayStopReason};
use cavalry_proto::frame::FrameError;
use cavalry_shm::counters::Counter;
use cavalry_shm::log::{LogError, TermWriter};
use cavalry_shm::spsc::{Consumer, Producer};

use crate::conductor::now_nanos;
use crate::idle::{IdleStrategy, Idler};
use crate::spec;

const DOC: &str = "docs/reference/client-protocol.md#error-codes";

/// How many replies the replayer holds back when the reply queue is full: one per live
/// replay, which may end on its own, and one per request handled in a cycle. At this
/// bound the replayer handles no request and ends no replay until the conductor takes
/// replies again; it keeps writing.
pub(crate) const PENDING_REPLIES: usize =
    spec::MAX_REPLAYS + spec::MAX_COMMANDS_IN_PROGRESS + spec::REPLAYER_REQUESTS_PER_CYCLE;

/// The longest segment file name, with room to spare, so a roll formats the name on
/// the stack (`cavalry_log::recorder`).
const SEGMENT_NAME_CAPACITY: usize = 64;

/// How long the replayer parks at a time while it holds no replay and no reply. The
/// conductor unparks it with every request it offers and `Driver::close` when it
/// stops, so this bounds a wake-up both missed; it is not a cadence. The backoff
/// strategy's longest park is a millisecond, and a thread that woke that often on the
/// housekeeping CPU the recorder runs on moved `claim_commit_32_recorded` at p99.99 on
/// the bench host (ADR 0030, Evidence).
const IDLE_PARK: Duration = Duration::from_secs(1);

/// What a `start replay` command asked, carried to the replayer once.
pub(crate) struct Start {
    /// The command's correlation id: the replay session id.
    pub replay_session_id: i64,
    /// The recording to play.
    pub recording_id: i64,
    /// The position asked for, [`NULL_POSITION`] for the recording's start position.
    pub position: i64,
    /// The length asked for: −1 or more, as the conductor checked.
    pub length: i64,
    /// The live recording's `recording.synced_position` counter, when the conductor
    /// holds one: the extent of a live recording.
    pub synced: Option<Counter>,
}

/// What the conductor asks the replayer.
pub(crate) enum Request {
    /// Read the recording's slot, check the position and open the segment file;
    /// answered with [`Reply::Ready`] or [`Reply::StartFailed`].
    Start(Start),
    /// The replay's publication exists: the writer on its log, its `publication.limit`
    /// counter, which bounds the writer, and its `replay.position` counter. A replay
    /// already stopped drops them.
    Attach {
        /// The replay.
        replay_session_id: i64,
        /// The writer on the replay's log, at the start position.
        writer: TermWriter,
        /// The publication's limit, loaded before each block.
        limit: Counter,
        /// The replay's position gauge, stored after each block.
        position: Counter,
    },
    /// Stop a replay at the position reached; answered with [`Reply::Stopped`] with the
    /// reason given, or ignored for a replay that already ended.
    Stop {
        /// The replay.
        replay_session_id: i64,
        /// The `stop replay` command's correlation id, when a command asked.
        correlation_id: Option<i64>,
        /// Why: by command, or for want of a subscriber.
        reason: ReplayStopReason,
    },
    /// A retention command's checks that need the disk, before any catalog write
    /// (ADR 0031): the slot read by the two-range rule, the position against the
    /// retained position, the stop position and the segment base rule, and for a
    /// truncate below the stop the frame header at the position; answered with
    /// [`Reply::Checked`] or [`Reply::CheckFailed`].
    Check {
        /// The command's correlation id.
        correlation_id: i64,
        /// The command.
        command: Retention,
    },
    /// A retention command's file work, after its first catalog write: one file per
    /// duty cycle, a truncate's segments past the position from the highest base down,
    /// then the cut and extension of the segment that holds the position and the index's
    /// cut, a purge's segments wholly before the position, a delete's segments and then
    /// its index; answered with [`Reply::FileWorkDone`].
    FileWork {
        /// The command's correlation id.
        correlation_id: i64,
        /// The command.
        command: Retention,
    },
}

/// A retention command as the conductor admitted it: `truncate recording`, `purge
/// segments` or `purge recording` (`spec/client-protocol.md`, "Commands"; ADR 0031).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Retention {
    /// Keep the bytes before `position`, lose the rest.
    Truncate {
        /// The stopped recording.
        recording_id: i64,
        /// The cut: a multiple of 32 from the retained position to the stop position,
        /// at which a frame begins or which is the stop position.
        position: i64,
    },
    /// Lose the segment files wholly before `position`, which becomes the retained
    /// position.
    Purge {
        /// The stopped recording.
        recording_id: i64,
        /// A segment base above the retained position and at most the stop position.
        position: i64,
    },
    /// Lose every file; the slot stays as state 3.
    Delete {
        /// The stopped recording.
        recording_id: i64,
    },
}

impl Retention {
    /// The recording the command names.
    pub(crate) const fn recording_id(self) -> i64 {
        match self {
            Retention::Truncate { recording_id, .. }
            | Retention::Purge { recording_id, .. }
            | Retention::Delete { recording_id } => recording_id,
        }
    }

    /// The command's name in an error message.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Retention::Truncate { .. } => "truncate recording",
            Retention::Purge { .. } => "purge segments",
            Retention::Delete { .. } => "purge recording",
        }
    }
}

/// Why a retention command's checks failed, for the error event's code (ADR 0031).
pub(crate) enum CheckFailure {
    /// The catalog does not hold the recording as a valid recording: code 21.
    UnknownRecording {
        /// The id asked for.
        recording_id: i64,
        /// What the slot holds instead.
        why: String,
    },
    /// The slot reads live or being cut: code 24.
    Busy {
        /// The id asked for.
        recording_id: i64,
    },
    /// The position is outside the command's rule: code 23, with the bounds.
    PositionOutOfRange {
        /// The position asked for.
        position: i64,
        /// The recording's retained position.
        retained: i64,
        /// The recording's stop position.
        stop: i64,
        /// The rule, in the message's words.
        rule: &'static str,
    },
    /// No frame begins at a truncate's position below the stop: code 23.
    NoFrame {
        /// The position asked for.
        position: i64,
    },
    /// A segment file could not be opened or read, or the catalog read: code 12.
    Files(String),
}

/// What a command's file work left: for the catalog's last write and the counters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FileWorkDone {
    /// Segment files removed, and for a delete the index too.
    pub files_removed: u64,
    /// For a truncate, the timestamp of the last index entry left: the stop timestamp
    /// recovery would rebuild.
    pub stop_timestamp: i64,
}

/// Why a start failed, for the error event's code.
pub(crate) enum StartFailure {
    /// The catalog does not hold the recording as a valid recording: error code 21.
    UnknownRecording {
        /// The id asked for.
        recording_id: i64,
        /// What the catalog holds instead.
        why: String,
    },
    /// The position is not a multiple of 32 from the start position to the extent:
    /// error code 23.
    PositionOutOfRange {
        /// The position asked for.
        position: i64,
        /// The recording's start position.
        start: i64,
        /// The recording's extent.
        extent: i64,
    },
    /// The segment file could not be opened, or the catalog could not be read: error
    /// code 12, with the message.
    Files(String),
}

/// What the replayer answers.
pub(crate) enum Reply {
    /// The slot is read, the position is in range and the segment file is open: the
    /// conductor creates the replay's log from these fields and attaches a writer.
    Ready {
        /// The replay.
        replay_session_id: i64,
        /// The recording.
        recording_id: i64,
        /// Where the replay starts, resolved from the command's position.
        start_position: i64,
        /// The recording's initial term id, which the replay's log takes.
        initial_term_id: i32,
        /// The recording's term length.
        term_length: i32,
        /// The recording's MTU length.
        mtu_length: i32,
        /// The session id the recorded frames carry, which the writer replaces.
        recorded_session_id: i32,
        /// The stream id the recorded frames carry.
        recorded_stream_id: i32,
    },
    /// The start failed; nothing plays.
    StartFailed {
        /// The replay.
        replay_session_id: i64,
        /// Why.
        failure: StartFailure,
    },
    /// A replay ended: the writer wrote its last frame and the replayer forgot it.
    Stopped {
        /// The replay.
        replay_session_id: i64,
        /// The position after the last frame written.
        position: i64,
        /// Why.
        reason: ReplayStopReason,
        /// The `stop replay` command's correlation id, when a command asked.
        correlation_id: Option<i64>,
    },
    /// A retention command's checks passed: the conductor hands the first catalog write
    /// to the recorder.
    Checked {
        /// The command.
        correlation_id: i64,
    },
    /// A retention command's checks failed; nothing was written.
    CheckFailed {
        /// The command.
        correlation_id: i64,
        /// Why.
        failure: CheckFailure,
    },
    /// A retention command's file work ended, done or refused by a file.
    FileWorkDone {
        /// The command.
        correlation_id: i64,
        /// The command, for the catalog's last write.
        command: Retention,
        /// What it left, or the file error's text.
        outcome: Result<FileWorkDone, String>,
    },
}

/// One retention command's file work in progress, one file per duty cycle.
struct Work {
    correlation_id: i64,
    command: Retention,
    descriptor: Descriptor,
    /// The segment bases still to remove, in the order they go: a truncate's from the
    /// highest base down, a purge's and a delete's from the lowest up. Allocated once
    /// per command, off every message path.
    bases: Vec<i64>,
    /// Whether the segment holding a truncate's position still waits for its cut.
    cut_due: bool,
    /// Whether the index still waits for its cut (a truncate) or removal (a delete).
    index_due: bool,
    files_removed: u64,
    stop_timestamp: i64,
}

/// The bytes a replay may read of its recording: the stop position once the recording
/// is stopped, else the synced position while it is live.
enum Extent {
    /// The recording is stopped at this position.
    Final(i64),
    /// The recording is live: its synced counter, or its start position while the
    /// conductor had no counter for it, and when its catalog slot was last read.
    Live {
        synced: Option<Counter>,
        start: i64,
        checked_at: i64,
    },
}

impl Extent {
    fn value(&self) -> i64 {
        match self {
            Extent::Final(stop) => *stop,
            Extent::Live { synced, start, .. } => synced
                .as_ref()
                .map_or(*start, Counter::load_acquire)
                .max(*start),
        }
    }
}

/// The handles the conductor attaches once the replay's publication exists.
struct Attached {
    writer: TermWriter,
    limit: Counter,
    position: Counter,
}

/// One replay the replayer plays.
struct Replay {
    replay_session_id: i64,
    recording_id: i64,
    descriptor: Descriptor,
    /// Where the replay ends: the position plus the length, saturating, or the extent
    /// at the start for a length of −1. The first frame that would begin at or past it
    /// ends the replay, so the position reached is the frame boundary at or after it.
    end: i64,
    extent: Extent,
    /// Where the replay starts, until the writer is attached.
    start_position: i64,
    /// The open segment file, or `None` for a replay with nothing to play, whose
    /// position at the start is its end or its stopped recording's extent: it reads no
    /// byte, so it opens no file, and the recorder keeps none for a recording that
    /// stopped at its start (`spec/client-protocol.md`, "start replay"; ADR 0035).
    segment: Option<File>,
    segment_base: i64,
    /// The open segment's path, formatted in place at a roll.
    segment_path: PathBuf,
    attached: Option<Attached>,
}

impl Replay {
    fn position(&self) -> i64 {
        self.attached
            .as_ref()
            .map_or(self.start_position, |attached| attached.writer.position())
    }
}

/// What one visit to a replay did.
enum Step {
    /// Nothing to read, or no room to write.
    Idle,
    /// A block was written.
    Worked,
    /// The replay ended and left the list.
    Stopped,
}

/// The replayer's state: the log directory's catalog, read-only, the queues, the live
/// replays and the one block buffer they share.
pub(crate) struct Replayer {
    dir: PathBuf,
    catalog: CatalogReader,
    requests: Consumer<Request>,
    replies: Producer<Reply>,
    replays: Vec<Replay>,
    /// The retention commands whose file work is in progress, bounded as the conductor's
    /// registry of commands in progress is.
    works: Vec<Work>,
    /// Replies the reply queue had no room for, offered again next cycle.
    pending: VecDeque<Reply>,
    /// The block read from a segment file, sized once.
    buffer: Vec<u8>,
    rejected_frame: Counter,
    idler: Idler,
}

impl Replayer {
    /// A replayer over the log directory at `dir`, whose catalog it opens read-only,
    /// with the one static counter it moves: `log_buffer.rejected_frame`.
    pub(crate) fn new(
        dir: &Path,
        requests: Consumer<Request>,
        replies: Producer<Reply>,
        rejected_frame: Counter,
        idle: IdleStrategy,
    ) -> Result<Self, CatalogError> {
        Ok(Self {
            dir: dir.to_path_buf(),
            catalog: CatalogReader::open(dir)?,
            requests,
            replies,
            replays: Vec::with_capacity(spec::MAX_REPLAYS),
            works: Vec::with_capacity(spec::MAX_COMMANDS_IN_PROGRESS),
            pending: VecDeque::with_capacity(PENDING_REPLIES),
            buffer: vec![0u8; spec::REPLAYER_BLOCK_LIMIT],
            rejected_frame,
            idler: Idler::new(idle),
        })
    }

    /// The replayer's loop: duty cycles until `stop`, idling with the strategy while
    /// a replay is live and parked while none is. A replay dies with the driver;
    /// nothing of it is on disk.
    pub(crate) fn run(mut self, stop: &AtomicBool) {
        while !stop.load(Ordering::Acquire) {
            let now = now_nanos();
            if self.duty_cycle(now) != 0 {
                self.idler.reset();
            } else if self.replays.is_empty() && self.works.is_empty() && self.pending.is_empty() {
                // Nothing can change until the conductor offers a request, and it
                // unparks this thread when it does; an unpark that came before this
                // park is a token the park consumes at once, so no request waits for
                // the timeout. `Driver::close` unparks it too, after storing `stop`.
                std::thread::park_timeout(IDLE_PARK);
                self.idler.reset();
            } else {
                self.idler.idle();
            }
        }
    }

    /// One duty cycle; returns how much work it found, 0 when the replayer may idle.
    pub(crate) fn duty_cycle(&mut self, now: i64) -> usize {
        let mut work = self.flush_replies();
        for _ in 0..spec::REPLAYER_REQUESTS_PER_CYCLE {
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
        while index < self.replays.len() {
            let may_stop = self.pending.len() < PENDING_REPLIES;
            match self.play(index, now, may_stop) {
                Step::Stopped => work += 1,
                Step::Worked => {
                    work += 1;
                    index += 1;
                }
                Step::Idle => index += 1,
            }
        }
        work += self.file_work();
        work + self.flush_replies()
    }

    fn handle(&mut self, request: Request, now: i64) {
        match request {
            Request::Start(start) => self.start(start, now),
            Request::Attach {
                replay_session_id,
                writer,
                limit,
                position,
            } => {
                if let Some(replay) = self
                    .replays
                    .iter_mut()
                    .find(|replay| replay.replay_session_id == replay_session_id)
                {
                    position.store_release(writer.position());
                    replay.attached = Some(Attached {
                        writer,
                        limit,
                        position,
                    });
                }
            }
            Request::Check {
                correlation_id,
                command,
            } => self.check(correlation_id, command),
            Request::FileWork {
                correlation_id,
                command,
            } => self.begin_file_work(correlation_id, command),
            Request::Stop {
                replay_session_id,
                correlation_id,
                reason,
            } => {
                if let Some(index) = self
                    .replays
                    .iter()
                    .position(|replay| replay.replay_session_id == replay_session_id)
                {
                    self.stop_at(index, reason, correlation_id);
                } else if let Some(correlation_id) = correlation_id {
                    // Already ended on its own, its `Stopped` sent: the command still
                    // gets its answer, as a stop of a stopped recording does.
                    self.pending.push_back(Reply::Stopped {
                        replay_session_id,
                        position: -1,
                        reason,
                        correlation_id: Some(correlation_id),
                    });
                }
            }
        }
    }

    /// Reads the slot, resolves and checks the position, opens the segment file unless
    /// there is nothing to play.
    fn start(&mut self, start: Start, now: i64) {
        let replay_session_id = start.replay_session_id;
        let fail = |failure| Reply::StartFailed {
            replay_session_id,
            failure,
        };
        if self.replays.len() >= spec::MAX_REPLAYS {
            // The conductor bounds its registry to the same number, so this is a bug
            // there; refused rather than grown.
            self.pending.push_back(fail(StartFailure::Files(format!(
                "the replayer holds {} replays, its capacity; see {DOC}",
                spec::MAX_REPLAYS
            ))));
            return;
        }
        let unknown = |why: String| {
            fail(StartFailure::UnknownRecording {
                recording_id: start.recording_id,
                why,
            })
        };
        let descriptor = match self.catalog.get(start.recording_id) {
            Ok(Some(descriptor)) if descriptor.state() == State::Valid => descriptor,
            Ok(Some(descriptor)) => {
                let state = match descriptor.state() {
                    State::Invalid => "invalid, damaged beyond repair",
                    State::Deleted => "deleted",
                    State::Valid | State::Empty => "not valid",
                };
                self.pending
                    .push_back(unknown(format!("its slot is {state}")));
                return;
            }
            Ok(None) => {
                self.pending
                    .push_back(unknown("its slot is empty".to_string()));
                return;
            }
            Err(CatalogError::Unknown { .. }) => {
                let next = self.catalog.slots().unwrap_or(0);
                self.pending.push_back(unknown(format!(
                    "the catalog's next recording id is {next}"
                )));
                return;
            }
            Err(error) => {
                self.pending
                    .push_back(unknown(format!("its slot cannot be read: {error}")));
                return;
            }
        };
        let extent = if descriptor.is_live() {
            Extent::Live {
                synced: start.synced,
                start: descriptor.start_position(),
                checked_at: now,
            }
        } else {
            Extent::Final(descriptor.stop_position())
        };
        let extent_now = extent.value();
        // A position of −1 is the recording's retained position: its start position
        // until a purge moved it, and the lower bound of what a replay may read
        // (`spec/client-protocol.md`, "start replay"; ADR 0031).
        let retained = descriptor.retained_position();
        let position = if start.position == NULL_POSITION {
            retained
        } else {
            start.position
        };
        if position % FRAME_ALIGNMENT != 0 || position < retained || position > extent_now {
            self.pending
                .push_back(fail(StartFailure::PositionOutOfRange {
                    position,
                    start: retained,
                    extent: extent_now,
                }));
            return;
        }
        let end = if start.length == NULL_LENGTH {
            extent_now
        } else {
            position.saturating_add(start.length.max(0))
        };
        // A stopped recording's replay from its end, or from the extent, has nothing to
        // play: it ends with reason 0 on its first visit once its publication exists,
        // and opens no file, since the recorder keeps no segment file for a recording
        // that stopped at its start (spec, "start replay"; ADR 0035). A live
        // recording's replay from its extent waits for bytes, and its file exists.
        let nothing_to_play =
            matches!(extent, Extent::Final(_)) && (position >= end || position >= extent_now);
        let mut segment_path =
            PathBuf::with_capacity(self.dir.as_os_str().len() + 1 + SEGMENT_NAME_CAPACITY);
        let (segment, segment_base) = if nothing_to_play {
            (None, position)
        } else {
            let located = match locate(&descriptor, position) {
                Ok(located) => located,
                Err(error) => {
                    self.pending
                        .push_back(fail(StartFailure::Files(format!("{error}"))));
                    return;
                }
            };
            segment_path.push(&self.dir);
            segment_path.push(&located.segment_file);
            match File::open(&segment_path) {
                Ok(file) => (Some(file), located.segment_base_position),
                Err(error) => {
                    self.pending.push_back(fail(StartFailure::Files(format!(
                        "cannot open the segment file {} of recording {} at position {position}: \
                         {error}; the log directory lost a file, restore it from a backup; see {DOC}",
                        segment_path.display(),
                        start.recording_id
                    ))));
                    return;
                }
            }
        };
        self.pending.push_back(Reply::Ready {
            replay_session_id,
            recording_id: start.recording_id,
            start_position: position,
            initial_term_id: descriptor.initial_term_id(),
            term_length: descriptor.term_length(),
            mtu_length: descriptor.mtu_length(),
            recorded_session_id: descriptor.session_id(),
            recorded_stream_id: descriptor.stream_id(),
        });
        self.replays.push(Replay {
            replay_session_id,
            recording_id: start.recording_id,
            descriptor,
            end,
            extent,
            start_position: position,
            segment,
            segment_base,
            segment_path,
            attached: None,
        });
    }

    /// One visit to the replay at `index`: a block if there is one to read and room to
    /// write it, an end if the replay reached it. With `may_stop` false an end waits
    /// for the next cycle, since there is no room for its reply.
    fn play(&mut self, index: usize, now: i64, may_stop: bool) -> Step {
        if self.replays[index].attached.is_none() {
            return Step::Idle;
        }
        self.refresh_extent(index, now, false);
        let replay = &mut self.replays[index];
        let position = replay.position();
        if position >= replay.end {
            return self.end_if(index, may_stop);
        }
        if position >= replay.extent.value() {
            return match replay.extent {
                Extent::Final(_) => self.end_if(index, may_stop),
                Extent::Live { .. } => Step::Idle,
            };
        }
        let Some(attached) = replay.attached.as_mut() else {
            return Step::Idle;
        };
        let limit = attached.limit.load_acquire();
        if position >= limit {
            return Step::Idle;
        }
        let segment_length = i64::from(replay.descriptor.segment_file_length());
        if (replay.segment.is_none()
            || position < replay.segment_base
            || position >= replay.segment_base + segment_length)
            && roll(&self.dir, replay, position).is_err()
        {
            if !may_stop {
                return Step::Idle;
            }
            return self.stop_at(index, ReplayStopReason::Failed, None);
        }
        let replay = &mut self.replays[index];
        let end = replay.end;
        let term_length = i64::from(replay.descriptor.term_length());
        let term_end = position - position % term_length + term_length;
        let segment_end = replay.segment_base + segment_length;
        let mtu = i64::from(replay.descriptor.mtu_length());
        // The end and the limit bound the read by an MTU past themselves, not at
        // themselves: the writer stops at the first frame that would begin at or past
        // the lower of the two, and a frame that begins below it is written whole, so
        // the slice must hold that frame; a slice cut at the end inside a frame would
        // write nothing and move nothing, for ever (spec, "start replay").
        let read_end = replay
            .extent
            .value()
            .min(term_end)
            .min(segment_end)
            .min(limit.saturating_add(mtu))
            .min(end.saturating_add(mtu))
            .min(position.saturating_add(spec::REPLAYER_BLOCK_LIMIT as i64));
        let length = usize::try_from(read_end.saturating_sub(position)).unwrap_or(0);
        if length == 0 {
            return Step::Idle;
        }
        let offset = (position - replay.segment_base) as u64;
        let Some(segment) = replay.segment.as_ref() else {
            return Step::Idle;
        };
        if segment
            .read_exact_at(&mut self.buffer[..length], offset)
            .is_err()
        {
            if !may_stop {
                return Step::Idle;
            }
            return self.stop_at(index, ReplayStopReason::Failed, None);
        }
        let Some(attached) = replay.attached.as_mut() else {
            return Step::Idle;
        };
        match attached
            .writer
            .write(&self.buffer[..length], limit.min(end))
        {
            // The slice begins at a frame boundary below the limit and the end, and
            // ends at a frame boundary, the extent, the term's end or the segment's
            // end, or at least an MTU past the lower of the limit and the end, or
            // 1 MiB on: it holds whole every frame a recorder could have written at
            // its start, so the writer wrote nothing only because the first frame's
            // length runs past the bytes given, over the MTU or past the extent. That
            // is `spec/frame.md`'s `length` reason, damage as any other failed check
            // is, and reading the same bytes again would find it again for ever. A
            // live recording's synced counter may be another's once the recording
            // stopped, so its slot is read again first, as for a frame length of 0.
            Ok(0) => self.reject(index, now, may_stop, true),
            Ok(_) => {
                attached.position.store_release(attached.writer.position());
                Step::Worked
            }
            Err(error) => {
                // A frame length of 0 below a live recording's extent: the recording may
                // have stopped at this position and the synced counter be another's by
                // now, so the slot is read again before the frame is called damage.
                let zero = matches!(error, LogError::Frame(FrameError::Length { found: 0, .. }));
                self.reject(index, now, may_stop, zero)
            }
        }
    }

    /// Ends the replay at `index` with reason 3 for a frame that failed a check, adding
    /// one to `log_buffer.rejected_frame`, when there is room for the reply; with none
    /// the frame is read again next cycle. With `recheck`, a live recording's catalog
    /// slot is read again first, and a recording that turned out stopped is no failure:
    /// the replay goes on under its stop position next cycle, and a frame that runs
    /// past that too is damage then.
    fn reject(&mut self, index: usize, now: i64, may_stop: bool, recheck: bool) -> Step {
        if recheck
            && matches!(self.replays[index].extent, Extent::Live { .. })
            && self.refresh_extent(index, now, true)
        {
            return Step::Worked;
        }
        if !may_stop {
            return Step::Idle;
        }
        self.rejected_frame.increment();
        self.stop_at(index, ReplayStopReason::Failed, None)
    }

    /// Reads a live recording's catalog slot again when `force` or when a second has
    /// passed since the last read, and makes its stop the replay's final extent.
    /// Returns whether the recording turned out stopped.
    fn refresh_extent(&mut self, index: usize, now: i64, force: bool) -> bool {
        let replay = &mut self.replays[index];
        let due = match replay.extent {
            Extent::Final(_) => return false,
            Extent::Live { checked_at, .. } => {
                force
                    || now.saturating_sub(checked_at)
                        >= spec::nanos(spec::REPLAYER_CATALOG_INTERVAL)
            }
        };
        if !due {
            return false;
        }
        match self.catalog.get(replay.recording_id) {
            Ok(Some(descriptor)) if descriptor.state() == State::Valid && !descriptor.is_live() => {
                replay.extent = Extent::Final(descriptor.stop_position());
                true
            }
            _ => {
                if let Extent::Live { checked_at, .. } = &mut replay.extent {
                    *checked_at = now;
                }
                false
            }
        }
    }

    /// Ends the replay at `index` with reason 0 when there is room for the reply.
    fn end_if(&mut self, index: usize, may_stop: bool) -> Step {
        if may_stop {
            self.stop_at(index, ReplayStopReason::Ended, None)
        } else {
            Step::Idle
        }
    }

    /// Forgets the replay at `index` and replies with the position reached.
    fn stop_at(
        &mut self,
        index: usize,
        reason: ReplayStopReason,
        correlation_id: Option<i64>,
    ) -> Step {
        let replay = self.replays.swap_remove(index);
        let position = replay.position();
        if let Some(attached) = &replay.attached {
            attached.position.store_release(position);
        }
        // During duty cycles a stop happens only with room for its reply; a command's
        // stop with none waits in the queue, so this never drops one.
        if self.pending.len() < PENDING_REPLIES {
            self.pending.push_back(Reply::Stopped {
                replay_session_id: replay.replay_session_id,
                position,
                reason,
                correlation_id,
            });
        }
        Step::Stopped
    }

    /// How many replays the replayer plays.
    #[cfg(test)]
    pub(crate) fn live_replays(&self) -> usize {
        self.replays.len()
    }

    /// How many replies wait for room in the reply queue.
    #[cfg(test)]
    pub(crate) fn pending_replies(&self) -> usize {
        self.pending.len()
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

impl Replayer {
    /// A retention command's checks (`spec/client-protocol.md`, "Commands"; ADR 0031), in
    /// the spec's order, none of them writing: the slot, the position, the frame.
    fn check(&mut self, correlation_id: i64, command: Retention) {
        let recording_id = command.recording_id();
        let fail = |failure| Reply::CheckFailed {
            correlation_id,
            failure,
        };
        let descriptor = match self.catalog.get(recording_id) {
            Ok(Some(descriptor)) if descriptor.state() == State::Valid => descriptor,
            Ok(Some(descriptor)) => {
                let state = match descriptor.state() {
                    State::Invalid => "invalid, damaged beyond repair",
                    State::Deleted => "deleted",
                    State::Valid | State::Empty => "not valid",
                };
                self.pending.push_back(fail(CheckFailure::UnknownRecording {
                    recording_id,
                    why: format!("its slot is {state}"),
                }));
                return;
            }
            Ok(None) => {
                self.pending.push_back(fail(CheckFailure::UnknownRecording {
                    recording_id,
                    why: "its slot is empty".to_string(),
                }));
                return;
            }
            Err(CatalogError::Unknown { .. }) => {
                let next = self.catalog.slots().unwrap_or(0);
                self.pending.push_back(fail(CheckFailure::UnknownRecording {
                    recording_id,
                    why: format!("the catalog's next recording id is {next}"),
                }));
                return;
            }
            Err(error) => {
                self.pending
                    .push_back(fail(CheckFailure::Files(format!("{error}"))));
                return;
            }
        };
        if descriptor.is_live() {
            // Live, or being cut by a truncate: the conductor's registry refuses both
            // before this, so this is a slot another writer moved.
            self.pending
                .push_back(fail(CheckFailure::Busy { recording_id }));
            return;
        }
        let retained = descriptor.retained_position();
        let stop = descriptor.stop_position();
        let out_of_range = |position, rule| {
            fail(CheckFailure::PositionOutOfRange {
                position,
                retained,
                stop,
                rule,
            })
        };
        match command {
            Retention::Truncate { position, .. } => {
                if position % FRAME_ALIGNMENT != 0 || position < retained || position > stop {
                    self.pending.push_back(out_of_range(
                        position,
                        "a multiple of 32 from the retained position to the stop position",
                    ));
                    return;
                }
                if position < stop {
                    match cavalry_log::dir::frame_begins_at(&self.dir, &descriptor, position) {
                        Ok(true) => {}
                        Ok(false) => {
                            self.pending
                                .push_back(fail(CheckFailure::NoFrame { position }));
                            return;
                        }
                        Err(error) => {
                            self.pending
                                .push_back(fail(CheckFailure::Files(format!("{error}"))));
                            return;
                        }
                    }
                }
            }
            Retention::Purge { position, .. } => {
                if !segment_base(&descriptor, position) || position <= retained || position > stop {
                    self.pending.push_back(out_of_range(
                        position,
                        "a segment base of the recording above the retained position and at \
                         most the stop position",
                    ));
                    return;
                }
            }
            Retention::Delete { .. } => {}
        }
        self.pending.push_back(Reply::Checked { correlation_id });
    }

    /// Starts a command's file work: the files to remove are listed once, from the
    /// catalog slot as the first catalog write left it, and one goes per duty cycle.
    fn begin_file_work(&mut self, correlation_id: i64, command: Retention) {
        let recording_id = command.recording_id();
        let refuse = |text: String| Reply::FileWorkDone {
            correlation_id,
            command,
            outcome: Err(text),
        };
        if self.works.len() >= spec::MAX_COMMANDS_IN_PROGRESS {
            // The conductor bounds its registry to the same number, so this is a bug
            // there; refused rather than grown.
            self.pending.push_back(refuse(format!(
                "the replayer holds {} commands, its capacity; see {DOC}",
                spec::MAX_COMMANDS_IN_PROGRESS
            )));
            return;
        }
        let descriptor = match self.catalog.get(recording_id) {
            Ok(Some(descriptor)) => descriptor,
            Ok(None) => {
                self.pending
                    .push_back(refuse(format!("recording {recording_id}'s slot is empty")));
                return;
            }
            Err(error) => {
                self.pending.push_back(refuse(format!("{error}")));
                return;
            }
        };
        let segments = match cavalry_log::dir::segment_bases(&self.dir, recording_id) {
            Ok(bases) => bases,
            Err(error) => {
                self.pending.push_back(refuse(format!("{error}")));
                return;
            }
        };
        let segment_length = i64::from(descriptor.segment_file_length());
        let (mut bases, cut_due, index_due): (Vec<i64>, bool, bool) = match command {
            Retention::Truncate { position, .. } => {
                // Wholly past the position, from the highest base down; a position at
                // a segment base removes that file and cuts none.
                let mut past: Vec<i64> = segments
                    .into_iter()
                    .filter(|&base| base >= position)
                    .collect();
                past.sort_unstable_by(|a, b| b.cmp(a));
                let at_base = segment_base(&descriptor, position);
                (past, !at_base, true)
            }
            Retention::Purge { position, .. } => {
                // Wholly before the position, from the lowest up.
                let mut before: Vec<i64> = segments
                    .into_iter()
                    .filter(|&base| base.saturating_add(segment_length) <= position)
                    .collect();
                before.sort_unstable();
                (before, false, false)
            }
            Retention::Delete { .. } => {
                let mut all = segments;
                all.sort_unstable();
                (all, false, true)
            }
        };
        // Popped from the end, so the order to go is reversed here.
        bases.reverse();
        self.works.push(Work {
            correlation_id,
            command,
            descriptor,
            bases,
            cut_due,
            index_due,
            files_removed: 0,
            stop_timestamp: 0,
        });
    }

    /// One step of file work for every command in progress: one file removed, cut or
    /// truncated per command per cycle; a command whose work is done, or whose file the
    /// operating system refused, replies and leaves. Returns how many steps ran.
    fn file_work(&mut self) -> usize {
        let mut steps = 0;
        let mut index = 0;
        while index < self.works.len() {
            if self.pending.len() >= PENDING_REPLIES {
                break;
            }
            let outcome = self.file_work_step(index);
            steps += 1;
            match outcome {
                None => index += 1,
                Some(outcome) => {
                    let work = self.works.swap_remove(index);
                    self.pending.push_back(Reply::FileWorkDone {
                        correlation_id: work.correlation_id,
                        command: work.command,
                        outcome,
                    });
                }
            }
        }
        steps
    }

    /// The next step of the work at `index`: `None` while more remains, else the
    /// outcome.
    fn file_work_step(&mut self, index: usize) -> Option<Result<FileWorkDone, String>> {
        let work = &mut self.works[index];
        let recording_id = work.descriptor.recording_id();
        if let Some(base) = work.bases.pop() {
            let path = self.dir.join(segment_file_name(recording_id, base));
            return match std::fs::remove_file(&path) {
                Ok(()) => {
                    work.files_removed += 1;
                    None
                }
                // A file already gone is a file removed: the work is idempotent, as
                // recovery's is after a crash.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => Some(Err(format!(
                    "cannot remove the segment file {}: {error}",
                    path.display()
                ))),
            };
        }
        if work.cut_due
            && let Retention::Truncate { position, .. } = work.command
        {
            work.cut_due = false;
            return match cut_segment(&self.dir, &work.descriptor, position) {
                Ok(()) => None,
                Err(text) => Some(Err(text)),
            };
        }
        if work.index_due {
            work.index_due = false;
            match work.command {
                Retention::Truncate { position, .. } => {
                    return match cavalry_log::index::truncate_at(&self.dir, recording_id, position)
                    {
                        Ok(timestamp) => {
                            // A truncate to the retained position leaves recovery's
                            // empty recording, whose stop timestamp is the start
                            // timestamp whatever its first index entry carries
                            // (`spec/durable-log.md`, "The driver"; ADR 0031).
                            work.stop_timestamp = if position <= work.descriptor.retained_position()
                            {
                                work.descriptor.start_timestamp()
                            } else {
                                timestamp.unwrap_or(work.descriptor.start_timestamp())
                            };
                            None
                        }
                        Err(error) => Some(Err(format!("{error}"))),
                    };
                }
                Retention::Delete { .. } => {
                    let path = self
                        .dir
                        .join(cavalry_log::layout::index_file_name(recording_id));
                    return match std::fs::remove_file(&path) {
                        Ok(()) => {
                            work.files_removed += 1;
                            None
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                        Err(error) => Some(Err(format!(
                            "cannot remove the time index {}: {error}",
                            path.display()
                        ))),
                    };
                }
                Retention::Purge { .. } => {}
            }
        }
        Some(Ok(FileWorkDone {
            files_removed: work.files_removed,
            stop_timestamp: work.stop_timestamp,
        }))
    }
}

/// Whether `position` is one of the recording's segment bases: the start position's
/// term base plus a whole number of segment file lengths (`spec/durable-log.md`,
/// "Segment files").
fn segment_base(descriptor: &Descriptor, position: i64) -> bool {
    let term = i64::from(descriptor.term_length()).max(1);
    let segment = i64::from(descriptor.segment_file_length()).max(1);
    let start = descriptor.start_position();
    let first = start - start % term;
    position >= first && (position - first) % segment == 0
}

/// A truncate's cut of the segment that holds `position`: the file is cut to the
/// position's offset with one truncation and extended to the segment file length
/// again, so the bytes past the position are zero, and synced.
fn cut_segment(dir: &Path, descriptor: &Descriptor, position: i64) -> Result<(), String> {
    let located = locate(descriptor, position).map_err(|error| format!("{error}"))?;
    let path = dir.join(&located.segment_file);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .map_err(|error| format!("cannot open the segment file {}: {error}", path.display()))?;
    file.set_len(located.offset_in_segment as u64)
        .map_err(|error| format!("cannot cut the segment file {}: {error}", path.display()))?;
    file.set_len(descriptor.segment_file_length() as u64)
        .map_err(|error| format!("cannot extend the segment file {}: {error}", path.display()))?;
    file.sync_all()
        .map_err(|error| format!("cannot sync the segment file {}: {error}", path.display()))?;
    Ok(())
}

/// Opens the segment file that holds `position`, by Aeron's rule for segment base
/// positions (`spec/durable-log.md`, "Segment files"), on a path formatted in place.
fn roll(dir: &Path, replay: &mut Replay, position: i64) -> Result<(), std::io::Error> {
    let term_length = i64::from(replay.descriptor.term_length());
    let segment_length = i64::from(replay.descriptor.segment_file_length());
    let start = replay.descriptor.start_position();
    let start_term_base = start - start % term_length;
    let from_base = position - start_term_base;
    let base = start_term_base + from_base - from_base % segment_length;
    let mut name = [0u8; SEGMENT_NAME_CAPACITY];
    let length = {
        let mut cursor = std::io::Cursor::new(&mut name[..]);
        // Two i64s in decimal and the suffix fit the buffer by construction.
        let _ = write!(cursor, "{}-{}{}", replay.recording_id, base, SEGMENT_SUFFIX);
        cursor.position() as usize
    };
    let file_name = std::str::from_utf8(&name[..length])
        .map_err(|_| std::io::Error::other("a segment name that is not UTF-8"))?;
    replay.segment_path.clear();
    replay.segment_path.push(dir);
    replay.segment_path.push(file_name);
    replay.segment = Some(File::open(&replay.segment_path)?);
    replay.segment_base = base;
    Ok(())
}

impl std::fmt::Debug for Replayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Replayer")
            .field("dir", &self.dir)
            .field("replays", &self.replays.len())
            .field("pending_replies", &self.pending.len())
            .finish()
    }
}
