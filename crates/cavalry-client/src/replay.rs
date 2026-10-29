//! A replay the driver plays for a client: `start replay` and `stop replay` of
//! `spec/client-protocol.md` (ADR 0030). The replay belongs to the driver, not to the
//! client that started it: any client may stop it, and dropping a [`Replay`] stops
//! nothing. The handle reads the replay's position from the counters file, as `cavalry
//! doctor` does, and learns how the replay ended from the `replay stopped` event the
//! client's conductor keeps for it.

use std::sync::Arc;
use std::sync::atomic::{AtomicI32, AtomicI64, Ordering};

use cavalry_proto::client_protocol::ReplayStopReason;
use cavalry_shm::counters::CounterValue;

use crate::{Cavalry, ClientError};

/// How a replay ended, as `replay stopped` said it: shared between the conductor, which
/// stores it once, and the handle, which reads it. A position of −1 means not ended.
pub(crate) struct ReplayEnd {
    position: AtomicI64,
    reason: AtomicI32,
}

impl ReplayEnd {
    pub(crate) fn new() -> Self {
        Self {
            position: AtomicI64::new(-1),
            reason: AtomicI32::new(-1),
        }
    }

    /// The conductor's store, once, from the event: the reason first, then the
    /// position with a release store, which the handle's acquire load orders after.
    pub(crate) fn set(&self, position: i64, reason: i32) {
        self.reason.store(reason, Ordering::Relaxed);
        self.position.store(position, Ordering::Release);
    }

    fn get(&self) -> Option<(i64, i32)> {
        let position = self.position.load(Ordering::Acquire);
        if position < 0 {
            return None;
        }
        Some((position, self.reason.load(Ordering::Relaxed)))
    }
}

/// A replay the driver started: its replay session id, the recording played, where it
/// started, the session and stream of its publication, and its position as the
/// replayer stores it (`spec/client-protocol.md`, "Events", `replay ready`). The
/// replay's frames are read through a [`crate::Subscription`] on the replay stream,
/// which sees the replay's publication as an image whose positions are the recording's.
pub struct Replay {
    replay_session_id: i64,
    recording_id: i64,
    start_position: i64,
    session_id: i32,
    stream_id: i32,
    channel: String,
    position: Option<CounterValue>,
    end: Arc<ReplayEnd>,
}

impl Replay {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        replay_session_id: i64,
        recording_id: i64,
        start_position: i64,
        session_id: i32,
        stream_id: i32,
        channel: String,
        position: Option<CounterValue>,
        end: Arc<ReplayEnd>,
    ) -> Self {
        Self {
            replay_session_id,
            recording_id,
            start_position,
            session_id,
            stream_id,
            channel,
            position,
            end,
        }
    }

    /// The replay session id: the `start replay` command's correlation id, which names
    /// the replay's log and which `stop replay` names.
    pub fn replay_session_id(&self) -> i64 {
        self.replay_session_id
    }

    /// The recording being played.
    pub fn recording_id(&self) -> i64 {
        self.recording_id
    }

    /// Where the replay started: the recording's start position when the command asked
    /// for −1, else the position asked for.
    pub fn start_position(&self) -> i64 {
        self.start_position
    }

    /// The replay publication's session id, which the subscription's image carries.
    pub fn session_id(&self) -> i32 {
        self.session_id
    }

    /// The replay stream id.
    pub fn stream_id(&self) -> i32 {
        self.stream_id
    }

    /// The replay publication's channel, `cavalry:ipc` in this version.
    pub fn channel(&self) -> &str {
        &self.channel
    }

    /// How far the replayer has written the recording into the replay's log: the
    /// `replay.position` counter, read with an acquire load, while the replay lives; the
    /// end position once [`ended`](Replay::ended) says it ended, since the driver frees
    /// the counter then; and the start position between the two, for a replay that
    /// ended before its client found the counter and whose `replay stopped` is still on
    /// its way. It moves no faster than the slowest subscriber's position plus the
    /// publication window, and not at all with no subscriber.
    pub fn position(&self) -> i64 {
        if let Some((end, _)) = self.ended() {
            return end;
        }
        self.position
            .as_ref()
            .map_or(self.start_position, CounterValue::load_acquire)
    }

    /// How the replay ended, once `replay stopped` has arrived: the position after the
    /// last frame written, where the subscription's image ends, and the reason;
    /// `None` while the replay lives. The frames up to the position may still be
    /// unread: poll the subscription until its image's position reaches it.
    pub fn ended(&self) -> Option<(i64, Option<ReplayStopReason>)> {
        self.end
            .get()
            .map(|(position, reason)| (position, ReplayStopReason::from_code(reason)))
    }

    /// Stops this replay through `client`, which need not be the client that started
    /// it: [`Cavalry::stop_replay`] with [`replay_session_id`], and the same answers.
    ///
    /// [`replay_session_id`]: Replay::replay_session_id
    pub fn stop(&self, client: &mut Cavalry) -> Result<(), ClientError> {
        client.stop_replay(self.replay_session_id)
    }
}

impl std::fmt::Debug for Replay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Replay")
            .field("replay_session_id", &self.replay_session_id)
            .field("recording_id", &self.recording_id)
            .field("start_position", &self.start_position)
            .field("session_id", &self.session_id)
            .field("stream_id", &self.stream_id)
            .field("position", &self.position())
            .field("ended", &self.ended())
            .finish()
    }
}
