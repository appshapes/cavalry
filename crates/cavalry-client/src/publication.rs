//! [`Publication`]: a client's handle for writing one stream (`spec/log-buffer.md`,
//! "Claim, commit and abort"; ADR 0021). In Aeron, this is `Publication` and
//! `ExclusivePublication`, with `tryClaim` and `offer` (`docs/glossary.md`).

use std::fmt;
use std::sync::Arc;

use cavalry_proto::client_protocol::Command;
use cavalry_shm::command_ring::Producer;
use cavalry_shm::counters::CounterValue;
use cavalry_shm::log::{BufferClaim, ClaimError, LogBuffers, TermAppender};

use crate::conductor::{Shared, now_nanos};
use crate::error::ClientError;

/// Why a claim or an offer did not write: Aeron's result codes, as a type the caller
/// cannot drop by mistake. Nothing was written. The caller decides: wait on
/// `BackPressured` and `NotConnected`, try again at once on `AdminAction`, close on
/// `MaxPositionExceeded`, and stop on `Closed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Offer {
    /// No subscriber has a position on this stream: nothing is lost, and the message
    /// would have nobody to reach.
    NotConnected,
    /// The slowest subscriber is a publication window behind: back-pressure, returned to
    /// the caller and never absorbed (invariant 3).
    BackPressured,
    /// The term rotated under the claim: try again at once.
    AdminAction,
    /// The stream reached the largest position its term length allows.
    MaxPositionExceeded,
    /// The client this publication belongs to is closed, by [`crate::Cavalry::close`]
    /// or by what its conductor found, and [`crate::Cavalry::closed`] says which. The
    /// driver removes the client's publications, so the limit will not move again: the
    /// first claim or offer the limit refuses after the close says so, as Aeron's
    /// `CLOSED` does.
    Closed,
    /// The message is longer than this log allows.
    TooLong {
        /// The length asked for.
        found: usize,
        /// The longest this publication allows for the call made.
        limit: usize,
    },
}

impl Offer {
    /// The result's name: `not-connected`, `back-pressured`, `admin-action`,
    /// `max-position-exceeded`, `closed` or `too-long`.
    pub const fn reason(self) -> &'static str {
        match self {
            Offer::NotConnected => "not-connected",
            Offer::BackPressured => "back-pressured",
            Offer::AdminAction => "admin-action",
            Offer::MaxPositionExceeded => "max-position-exceeded",
            Offer::Closed => "closed",
            Offer::TooLong { .. } => "too-long",
        }
    }
}

impl From<ClaimError> for Offer {
    fn from(error: ClaimError) -> Self {
        match error {
            ClaimError::NotConnected => Offer::NotConnected,
            ClaimError::BackPressured => Offer::BackPressured,
            ClaimError::AdminAction => Offer::AdminAction,
            ClaimError::MaxPositionExceeded => Offer::MaxPositionExceeded,
            ClaimError::TooLong { found, limit } => Offer::TooLong { found, limit },
        }
    }
}

impl fmt::Display for Offer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const DOC: &str = "docs/reference/log-buffer.md#claim-commit-and-abort";
        let reason = self.reason();
        match self {
            Offer::Closed => write!(
                f,
                "offer refused ({reason}): the client is closed, and Cavalry::closed says why; \
                 connect a new client and add the publication again; see {DOC}"
            ),
            Offer::TooLong { found, limit } => write!(
                f,
                "offer refused ({reason}): a message of {found} bytes, this publication allows \
                 {limit}; split the message, or use offer, which fragments up to the term \
                 length ÷ 8; see {DOC}"
            ),
            other => write!(
                f,
                "offer refused ({reason}): {}; see {DOC}",
                match other {
                    Offer::NotConnected =>
                        "no subscriber has a position on this stream; nothing is lost, try \
                         again when one connects",
                    Offer::BackPressured =>
                        "the slowest subscriber is a window behind; try again later, and read \
                         subscription.position to find it",
                    Offer::AdminAction => "the term rotated; try again at once",
                    Offer::MaxPositionExceeded =>
                        "the stream reached the largest position its term length allows; \
                         close the publication and add a new one",
                    Offer::Closed | Offer::TooLong { .. } => unreachable!(),
                }
            ),
        }
    }
}

impl std::error::Error for Offer {}

/// What a refusal of the appender is to the caller. A refusal that waits on the
/// subscribers, `not-connected` or `back-pressured`, is `closed` once the client is,
/// since the limit will not move again. The client's state is read on the refusal path
/// alone, so a claim the limit allows stays one acquire load (invariant 2); a claim
/// within the last limit still writes after the close, as in Aeron between two checks.
fn refusal(shared: &Shared, error: ClaimError) -> Offer {
    match error {
        ClaimError::NotConnected | ClaimError::BackPressured if shared.closed().is_some() => {
            Offer::Closed
        }
        error => Offer::from(error),
    }
}

/// A client's handle for writing one stream on one channel: claim a frame and commit
/// it, or offer a message, each against the limit the driver maintains. One
/// publication is used from one thread at a time. Closing sends `remove publication`;
/// dropping does the same.
pub struct Publication {
    log: LogBuffers,
    appender: TermAppender,
    limit: CounterValue,
    shared: Arc<Shared>,
    producer: Producer,
    client_id: i64,
    registration_id: i64,
    session_id: i32,
    stream_id: i32,
    channel: String,
    exclusive: bool,
    /// Whether `remove publication` was sent, so that `close` and `Drop` send it once.
    removed: bool,
    scratch: Vec<u8>,
}

impl Publication {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        log: LogBuffers,
        limit: CounterValue,
        shared: Arc<Shared>,
        producer: Producer,
        client_id: i64,
        registration_id: i64,
        session_id: i32,
        stream_id: i32,
        channel: String,
        exclusive: bool,
    ) -> Result<Self, ClientError> {
        let appender = log.appender()?;
        Ok(Self {
            log,
            appender,
            limit,
            shared,
            producer,
            client_id,
            registration_id,
            session_id,
            stream_id,
            channel,
            exclusive,
            removed: false,
            scratch: vec![0u8; 64],
        })
    }

    /// Claims a frame for a body of `length` bytes: write it in place through
    /// [`BufferClaim::body`], then [`BufferClaim::commit`], or [`BufferClaim::abort`].
    /// Nothing is allocated; the limit is one acquire load. The refusals are
    /// [`Offer`]'s; `length` over [`Publication::max_payload_length`] is
    /// [`Offer::TooLong`] before anything is reserved, and a refusal after the client
    /// closed is [`Offer::Closed`].
    pub fn try_claim(&mut self, length: usize) -> Result<BufferClaim<'_>, Offer> {
        let limit = self.limit.load_acquire();
        let shared = &self.shared;
        self.appender
            .try_claim(length, limit)
            .map_err(|error| refusal(shared, error))
    }

    /// Copies `message` in, as one frame or as fragments of
    /// [`Publication::max_payload_length`] bytes committed in order, and returns the
    /// position after it. A message over [`Publication::max_message_length`] is
    /// [`Offer::TooLong`] before anything is reserved, and a refusal after the client
    /// closed is [`Offer::Closed`].
    pub fn offer(&mut self, message: &[u8]) -> Result<i64, Offer> {
        let limit = self.limit.load_acquire();
        self.appender
            .offer(message, limit, 0)
            .map_err(|error| refusal(&self.shared, error))
    }

    /// The producer position: where the next frame goes.
    pub fn position(&self) -> Result<i64, ClientError> {
        Ok(self.appender.position()?)
    }

    /// The position up to which this publication may write, as the driver last set it:
    /// the slowest subscriber's position plus the publication window.
    pub fn limit(&self) -> i64 {
        self.limit.load_acquire()
    }

    /// Whether a subscriber has a position on this stream, as the driver last set it,
    /// and the client is open.
    pub fn is_connected(&self) -> bool {
        !self.is_closed() && self.log.is_connected()
    }

    /// The longest body one claim carries.
    pub fn max_payload_length(&self) -> usize {
        self.appender.max_payload_length()
    }

    /// The longest message one offer carries.
    pub fn max_message_length(&self) -> usize {
        self.appender.max_message_length()
    }

    /// The id under which the driver knows this publication: the log's name, and the
    /// first registration's id when the publication is shared.
    pub fn registration_id(&self) -> i64 {
        self.registration_id
    }

    /// This publication's identity on the stream.
    pub fn session_id(&self) -> i32 {
        self.session_id
    }

    /// The stream id.
    pub fn stream_id(&self) -> i32 {
        self.stream_id
    }

    /// The channel.
    pub fn channel(&self) -> &str {
        &self.channel
    }

    /// Whether this is an exclusive publication: one session, one publisher.
    pub fn is_exclusive(&self) -> bool {
        self.exclusive
    }

    /// Whether the client this publication belongs to is closed, so that every refusal
    /// from here is [`Offer::Closed`]. [`Publication::close`] consumes the handle, so a
    /// publication the caller still holds is closed only with its client.
    pub fn is_closed(&self) -> bool {
        self.shared.closed().is_some()
    }

    /// Sends `remove publication`: the driver closes the log when every registration
    /// of it is gone, lets subscribers drain it for the linger, and removes it. A
    /// driver that is gone cannot be told, and that is not an error here.
    pub fn close(mut self) -> Result<(), ClientError> {
        self.remove();
        Ok(())
    }

    fn remove(&mut self) {
        if self.removed {
            return;
        }
        self.removed = true;
        let command = Command::RemovePublication {
            client_id: self.client_id,
            correlation_id: self.producer.next_correlation_id(),
            registration_id: self.registration_id,
        };
        if let Ok(length) = command.write(&mut self.scratch) {
            let _ = self.producer.write(
                command.command_type() as i32,
                &self.scratch[..length],
                now_nanos(),
            );
        }
    }
}

impl Drop for Publication {
    fn drop(&mut self) {
        self.remove();
    }
}

impl fmt::Debug for Publication {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Publication")
            .field("registration_id", &self.registration_id)
            .field("session_id", &self.session_id)
            .field("stream_id", &self.stream_id)
            .field("channel", &self.channel)
            .field("exclusive", &self.exclusive)
            .field("closed", &self.is_closed())
            .finish()
    }
}
