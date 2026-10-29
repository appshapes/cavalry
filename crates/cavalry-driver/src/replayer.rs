//! The replayer: the driver's third thread, which plays a recording's frames from its
//! segment files into a replay's log buffer, where a subscription reads them as any
//! stream (`spec/client-protocol.md`, "Commands"; ADR 0030). The conductor never
//! touches the disk: it hands the replayer each start, attach and stop through a
//! bounded queue, the replayer answers through a second one, and the conductor creates
//! the replay's publication, sends the client's events and closes the publication when
//! the replay ends. On each duty cycle the replayer reads its clock once, handles a
//! bounded number of requests and visits every live replay: it reads one block of the
//! recording from its segment file, bounded by the replay's end, the recording's extent,
//! the term, the segment, the publication's limit and 1 MiB, writes its frames into the
//! replay log through a [`TermWriter`], which checks each one and stops at the limit,
//! and stores `replay.position` with a release store. A replay reads no byte the
//! recorder has not synced, so what it plays is what a restart would find (invariant
//! 1); a replay of a live recording waits for the synced position to move, and learns
//! the recording's stop from its catalog slot, read again once per second and whenever
//! a frame length of 0 is found below the extent. Nothing here allocates per block,
//! locks or logs; the syscalls are the replayer's own reads and opens, and a segment
//! file is opened on a path the replayer formats in place.

use std::collections::VecDeque;
use std::fs::File;
use std::io::Write as _;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use cavalry_log::catalog::{CatalogError, CatalogReader};
use cavalry_log::layout::{Descriptor, FRAME_ALIGNMENT, SEGMENT_SUFFIX, State, locate};
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
pub(crate) const PENDING_REPLIES: usize = spec::MAX_REPLAYS + spec::REPLAYER_REQUESTS_PER_CYCLE;

/// The longest segment file name, with room to spare, so a roll formats the name on
/// the stack (`cavalry_log::recorder`).
const SEGMENT_NAME_CAPACITY: usize = 64;

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
    /// at the start for a length of −1.
    end: i64,
    extent: Extent,
    /// Where the replay starts, until the writer is attached.
    start_position: i64,
    segment: File,
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
            pending: VecDeque::with_capacity(PENDING_REPLIES),
            buffer: vec![0u8; spec::REPLAYER_BLOCK_LIMIT],
            rejected_frame,
            idler: Idler::new(idle),
        })
    }

    /// The replayer's loop: duty cycles until `stop`. A replay dies with the driver;
    /// nothing of it is on disk.
    pub(crate) fn run(mut self, stop: &AtomicBool) {
        while !stop.load(Ordering::Acquire) {
            let now = now_nanos();
            if self.duty_cycle(now) == 0 {
                self.idler.idle();
            } else {
                self.idler.reset();
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

    /// Reads the slot, resolves and checks the position, opens the segment file.
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
        let position = if start.position == NULL_POSITION {
            descriptor.start_position()
        } else {
            start.position
        };
        if position % FRAME_ALIGNMENT != 0
            || position < descriptor.start_position()
            || position > extent_now
        {
            self.pending
                .push_back(fail(StartFailure::PositionOutOfRange {
                    position,
                    start: descriptor.start_position(),
                    extent: extent_now,
                }));
            return;
        }
        let end = if start.length == NULL_LENGTH {
            extent_now
        } else {
            position.saturating_add(start.length.max(0))
        };
        let located = match locate(&descriptor, position) {
            Ok(located) => located,
            Err(error) => {
                self.pending
                    .push_back(fail(StartFailure::Files(format!("{error}"))));
                return;
            }
        };
        let mut segment_path =
            PathBuf::with_capacity(self.dir.as_os_str().len() + 1 + SEGMENT_NAME_CAPACITY);
        segment_path.push(&self.dir);
        segment_path.push(&located.segment_file);
        let segment = match File::open(&segment_path) {
            Ok(file) => file,
            Err(error) => {
                self.pending.push_back(fail(StartFailure::Files(format!(
                    "cannot open the segment file {} of recording {} at position {position}: \
                     {error}; the log directory lost a file, restore it from a backup; see {DOC}",
                    segment_path.display(),
                    start.recording_id
                ))));
                return;
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
            segment_base: located.segment_base_position,
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
        if (position < replay.segment_base || position >= replay.segment_base + segment_length)
            && roll(&self.dir, replay, position).is_err()
        {
            if !may_stop {
                return Step::Idle;
            }
            return self.stop_at(index, ReplayStopReason::Failed, None);
        }
        let replay = &mut self.replays[index];
        let term_length = i64::from(replay.descriptor.term_length());
        let term_end = position - position % term_length + term_length;
        let segment_end = replay.segment_base + segment_length;
        let mtu = i64::from(replay.descriptor.mtu_length());
        let read_end = replay
            .end
            .min(replay.extent.value())
            .min(term_end)
            .min(segment_end)
            .min(limit.saturating_add(mtu))
            .min(position.saturating_add(spec::REPLAYER_BLOCK_LIMIT as i64));
        let length = usize::try_from(read_end.saturating_sub(position)).unwrap_or(0);
        if length == 0 {
            return Step::Idle;
        }
        let offset = (position - replay.segment_base) as u64;
        if replay
            .segment
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
        match attached.writer.write(&self.buffer[..length], limit) {
            Ok(0) => Step::Idle,
            Ok(_) => {
                attached.position.store_release(attached.writer.position());
                Step::Worked
            }
            Err(error) => {
                // A frame length of 0 below a live recording's extent: the recording may
                // have stopped at this position and the synced counter be another's by
                // now, so the slot is read again before the frame is called damage.
                let zero = matches!(error, LogError::Frame(FrameError::Length { found: 0, .. }));
                if zero
                    && matches!(replay.extent, Extent::Live { .. })
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
        }
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
    replay.segment = File::open(&replay.segment_path)?;
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
