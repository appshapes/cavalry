//! [`Subscription`] and [`Image`]: a client's handle for reading one stream, one image per
//! connected publication (`spec/log-buffer.md`, "Reading"; ADR 0021). In Aeron, these are
//! `Subscription`, `Image` and the `FragmentHandler` (`docs/glossary.md`).
//!
//! The conductor opens an image's log and its position counter when the driver announces
//! it and hands them over through a bounded queue; the subscription applies the queue at
//! the start of each poll, and hands a mapping back through a second queue when its image
//! goes, so that the poll never opens or unmaps a file.

use std::fmt;
use std::sync::Arc;

use cavalry_proto::client_protocol::{Command, NULL_POSITION};
use cavalry_proto::frame::DataFrameHeader;
use cavalry_shm::command_ring::Producer;
use cavalry_shm::counters::Counter;
use cavalry_shm::log::{Action, LogBuffers, LogError, Positions, TermReader};
use cavalry_shm::spsc;

use crate::conductor::{Shared, epoch_time};
use crate::error::ClientError;

/// How many image events may wait for a subscription between two polls.
pub(crate) const IMAGE_QUEUE: usize = 64;

/// How many retired mappings may wait for the conductor to drop them.
pub(crate) const RETIRE_QUEUE: usize = 64;

/// How many images one subscription holds: the publications on one stream a process
/// reads at once. The driver holds 2048 images in all (`spec/client-protocol.md`,
/// "Sizes"); an image past this bound is reported as [`Fault::ImagesFull`] and retired
/// unread.
pub const MAX_IMAGES: usize = 256;

/// What the conductor hands a subscription: an image the driver announced, opened, or
/// one it withdrew.
pub(crate) enum ImageEvent {
    /// `available image`: the log and the position counter the driver allocated.
    Available {
        correlation_id: i64,
        session_id: i32,
        log: LogBuffers,
        position: Counter,
        source_identity: String,
    },
    /// `unavailable image`, by the image's correlation id.
    Unavailable { correlation_id: i64 },
    /// `available image` for an image whose log or position counter was gone when the
    /// conductor read the event (ADR 0078): nothing to map, and a fault to report.
    Gone { session_id: i32 },
}

/// A mapping the subscription no longer reads, for the conductor to drop.
pub(crate) struct Retired {
    _log: LogBuffers,
    _position: Counter,
}

/// What a subscription registers with the conductor when it is added, and what the
/// conductor sweeps when the subscription is gone.
pub(crate) struct Registration {
    pub(crate) registration_id: i64,
    pub(crate) images: spsc::Producer<ImageEvent>,
    pub(crate) retired: spsc::Consumer<Retired>,
}

/// Why a poll could not go on. A fault names the image it retired; the subscription
/// keeps its other images and the next poll continues with them. A poll that faults
/// returns the fault and not the count: the fragments it handed over before the fault
/// were delivered, and the handler is the place that counts them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// A frame of the image failed a check of `spec/frame.md`, or the join position was
    /// outside the log: nothing from it on is delivered, and the image is retired. A
    /// stream that cannot be read is not guessed at (invariant 1).
    ///
    /// With the reason [`Fault::GONE`] the image was never read: the driver had removed
    /// its log, or freed its position counter, before this client's conductor read the
    /// image's announcement. The driver keeps both for the counter reuse timeout, 1 s,
    /// after the announcement, so the conductor was more than that behind
    /// (`spec/client-protocol.md`, "Events"; ADR 0078). Frames its publisher published
    /// after the join, if any, were not delivered to this subscription, and this fault
    /// is their report. The subscription and its client go on.
    Image {
        /// The image's session id.
        session_id: i32,
        /// The reader's position when the check failed; `NULL_POSITION`, −1, for an
        /// image that was [`Fault::GONE`], which had no reader.
        position: i64,
        /// The check that failed, as the log page names it.
        reason: &'static str,
    },
    /// The subscription holds [`MAX_IMAGES`] images and the driver announced another; it
    /// is retired unread, and its publisher sees a subscriber that never moves.
    ImagesFull {
        /// The image's session id.
        session_id: i32,
    },
    /// The client this subscription belongs to is closed, by [`crate::Cavalry::close`]
    /// or by what its conductor found, and [`crate::Cavalry::closed`] says which: no
    /// image comes or goes from here, so nothing more is read, as Aeron's closed
    /// subscription reads nothing.
    Closed,
}

impl Fault {
    /// The reason of a [`Fault::Image`] for an image whose log or position counter was
    /// gone when the conductor read its announcement (`spec/log-buffer.md`, "What a
    /// reader rejects").
    pub const GONE: &'static str = "gone";

    /// The fault's name: `image`, `images-full` or `closed`.
    pub const fn reason(&self) -> &'static str {
        match self {
            Fault::Image { .. } => "image",
            Fault::ImagesFull { .. } => "images-full",
            Fault::Closed => "closed",
        }
    }
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const DOC: &str = "docs/reference/log-buffer.md#reading";
        let reason = self.reason();
        match self {
            Fault::Image {
                session_id,
                reason: Fault::GONE,
                ..
            } => write!(
                f,
                "poll stopped ({reason}): the image of session {session_id} was gone before \
                 this client read its announcement: the driver had removed its log or freed \
                 its position counter, so nothing from it is delivered, and frames published \
                 after the join, if any, were missed; the subscription and the client go on. \
                 The driver keeps both for the counter reuse timeout after it announces an \
                 image, so this client's conductor read the announcement later than that: \
                 look for what stopped this process, and if the stream was a replay, start \
                 it again from the position you hold; see \
                 docs/reference/client-protocol.md#liveness-and-timeouts"
            ),
            Fault::Image {
                session_id,
                position,
                reason: check,
            } => write!(
                f,
                "poll stopped ({reason}): the image of session {session_id} failed the `{check}` \
                 check at position {position}; nothing from it on is delivered and the image is \
                 retired; the publisher wrote what the log page forbids, or the log is corrupt; \
                 run `cavalry doctor`; see {DOC}"
            ),
            Fault::ImagesFull { session_id } => write!(
                f,
                "poll stopped ({reason}): this subscription holds {MAX_IMAGES} images and the \
                 driver announced session {session_id}; it is retired unread; split the stream's \
                 readers across subscriptions; see {DOC}"
            ),
            Fault::Closed => write!(
                f,
                "poll refused ({reason}): the client is closed, and Cavalry::closed says why; \
                 connect a new client and add the subscription again; see {DOC}"
            ),
        }
    }
}

impl std::error::Error for Fault {}

/// One publisher's stream as this subscription sees it.
pub struct Image {
    correlation_id: i64,
    session_id: i32,
    source_identity: String,
    reader: TermReader,
    position: Counter,
    log: LogBuffers,
}

impl Image {
    /// The id the driver gave this image: the `available image` event's correlation id.
    pub fn correlation_id(&self) -> i64 {
        self.correlation_id
    }

    /// The publication's session id.
    pub fn session_id(&self) -> i32 {
        self.session_id
    }

    /// Where the stream came from: `cavalry:ipc` in this version.
    pub fn source_identity(&self) -> &str {
        &self.source_identity
    }

    /// The reader's position: the start of the next frame it will read.
    pub fn position(&self) -> i64 {
        self.reader.position()
    }

    /// Whether the publication closed and this image has read everything it wrote.
    pub fn is_end_of_stream(&self) -> bool {
        self.reader.is_end_of_stream()
    }

    /// The position of a frame the handler was given from this image, from its
    /// header's term id and term offset (`spec/log-buffer.md`, "Positions"): for a
    /// replay's image, the position in the recording (ADR 0030). A frame's position is
    /// not the sum of the aligned lengths delivered before it: a padding frame, which
    /// fills the end of a term or covers an aborted claim, moves the position and is
    /// handed to no handler. The handler holds no borrow of the image while the
    /// subscription polls; [`Image::positions`] is the same arithmetic as a value it
    /// can hold.
    pub fn position_of(&self, header: &DataFrameHeader) -> i64 {
        self.positions()
            .position(header.term_id(), header.term_offset() as u32)
    }

    /// The position arithmetic of this image's log, the initial term id and the term
    /// length, as a copyable value: a fragment handler, which holds no borrow of the
    /// image, names the position of the frame it was given as
    /// `positions.position(header.term_id(), header.term_offset() as u32)`, which is
    /// [`Image::position_of`] of that header. The images a poll reads from are those
    /// [`Subscription::images`] lists before it; a poll with a fragment limit of 0
    /// applies the images the driver announced and reads no frame.
    pub fn positions(&self) -> Positions {
        self.log.positions()
    }

    fn retire(self) -> Retired {
        Retired {
            _log: self.log,
            _position: self.position,
        }
    }
}

impl fmt::Debug for Image {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Image")
            .field("correlation_id", &self.correlation_id)
            .field("session_id", &self.session_id)
            .field("position", &self.position())
            .finish()
    }
}

/// A client's handle for reading one stream on one channel, from every publication on
/// it, through a fragment handler. One subscription is polled from one thread at a
/// time. Closing sends `remove subscription`; dropping does the same.
pub struct Subscription {
    registration_id: i64,
    stream_id: i32,
    channel: String,
    client_id: i64,
    shared: Arc<Shared>,
    producer: Producer,
    images: Vec<Image>,
    incoming: spsc::Consumer<ImageEvent>,
    retired: spsc::Producer<Retired>,
    next: usize,
    /// Whether `remove subscription` was sent, so that `close` and `Drop` send it once.
    removed: bool,
    scratch: Vec<u8>,
}

impl Subscription {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        registration_id: i64,
        stream_id: i32,
        channel: String,
        client_id: i64,
        shared: Arc<Shared>,
        producer: Producer,
        incoming: spsc::Consumer<ImageEvent>,
        retired: spsc::Producer<Retired>,
    ) -> Self {
        Self {
            registration_id,
            stream_id,
            channel,
            client_id,
            shared,
            producer,
            images: Vec::with_capacity(MAX_IMAGES),
            incoming,
            retired,
            next: 0,
            removed: false,
            scratch: vec![0u8; 64],
        }
    }

    /// Reads up to `fragment_limit` fragments across the images, each handed to
    /// `handler` as its body and its header, a view into the log; stores each image's
    /// position with a release store after its share (`spec/log-buffer.md`, "Reading").
    /// Images the driver announced since the last poll join first; the images are
    /// polled in turn, starting one further along each call. An image joins once the
    /// client's conductor has handed it over, which is after the driver made it, so a
    /// publication can accept an offer before the subscription holds the image: a poll
    /// then delivers nothing, and the frames arrive on a later poll, from the image's
    /// join position, none lost. A caller that needs the image first polls until
    /// [`image_count`](Self::image_count) is above 0. Returns how many fragments
    /// were delivered, or the [`Fault`] that stopped the poll, with what was delivered
    /// before it already handed over and not counted in the result. A client that is
    /// closed is [`Fault::Closed`], one acquire load per poll. Nothing here allocates,
    /// locks, opens or unmaps, on the fault path either.
    pub fn poll<F>(&mut self, mut handler: F, fragment_limit: usize) -> Result<usize, Fault>
    where
        F: FnMut(&[u8], &DataFrameHeader),
    {
        if self.shared.closed().is_some() {
            return Err(Fault::Closed);
        }
        self.apply_events()?;
        let count = self.images.len();
        if count == 0 {
            return Ok(0);
        }
        let start = self.next % count;
        self.next = (start + 1) % count;
        let mut delivered = 0;
        let mut faulted: Option<(usize, LogError)> = None;
        for step in 0..count {
            if delivered >= fragment_limit {
                break;
            }
            let index = (start + step) % count;
            let image = &mut self.images[index];
            match image.reader.poll(fragment_limit - delivered, &mut handler) {
                Ok(poll) => {
                    delivered += poll.fragments;
                    image.position.store_release(poll.position);
                }
                Err(error) => {
                    faulted = Some((index, error));
                    break;
                }
            }
        }
        if let Some((index, error)) = faulted {
            let image = self.images.swap_remove(index);
            let fault = Fault::Image {
                session_id: image.session_id,
                position: image.position(),
                reason: error.reason(),
            };
            self.retire(image);
            return Err(fault);
        }
        Ok(delivered)
    }

    /// Reads up to `fragment_limit` fragments of one image, the first whose session id
    /// is `session_id`, and no frame that begins at or past `limit_position`: a frame
    /// that begins below the limit is delivered whole, and a limit at or below the
    /// image's position reads nothing (`spec/log-buffer.md`, "Reading"; ADR 0066). In
    /// Aeron, this is `Image.boundedPoll`. A limit of `i64::MAX` reads the image with
    /// no bound. The subscription's other images are not read by this call.
    ///
    /// The bound is for a stream that may be read only as far as something else has
    /// got: a cluster's service reads its log no further than the commit position
    /// ([`CommittedLog`](crate::CommittedLog)). A bound cannot be applied in the
    /// handler instead, since a frame the handler was given is behind the image's
    /// position and cannot be handed back.
    ///
    /// Images the driver announced since the last poll join first, as in
    /// [`poll`](Self::poll). Returns how many fragments were delivered, 0 when the
    /// subscription holds no image of that session, or the [`Fault`] that stopped the
    /// poll. Nothing here allocates, locks, opens or unmaps, on the fault path either.
    pub fn bounded_poll<F>(
        &mut self,
        session_id: i32,
        mut handler: F,
        limit_position: i64,
        fragment_limit: usize,
    ) -> Result<usize, Fault>
    where
        F: FnMut(&[u8], &DataFrameHeader),
    {
        if self.shared.closed().is_some() {
            return Err(Fault::Closed);
        }
        self.apply_events()?;
        let Some(index) = self
            .images
            .iter()
            .position(|image| image.session_id == session_id)
        else {
            return Ok(0);
        };
        self.bounded_poll_at(index, limit_position, fragment_limit, &mut handler)
    }

    /// Reads up to `fragment_limit` fragments across the images, as [`poll`](Self::poll)
    /// does, with a handler that says for each fragment whether the read takes it
    /// (`spec/log-buffer.md`, "Reading"; ADR 0114). [`Action::Continue`] takes the
    /// fragment and goes on. [`Action::Break`] takes it and ends the read of that image.
    /// [`Action::Abort`] does not take it: the read of that image ends with the image's
    /// position at the fragment's first byte, the fragment is not counted, and the next
    /// read of the image hands the same fragment over again. In Aeron, this is
    /// `Subscription.controlledPoll` with a `ControlledFragmentHandler`.
    ///
    /// The call is for a reader that learns from a fragment whether it can take it: one
    /// it must offer on a publication that is back-pressured now, or one after which
    /// the rest of the batch must wait. A handler given a fragment by `poll` cannot hand
    /// it back, since the image's position is past it. A fragment that is not taken
    /// holds its publisher as any unread fragment does: the publication's limit follows
    /// its slowest subscriber, so the publisher's `offer` returns back pressure once its
    /// window is full, and nothing is buffered between the two.
    ///
    /// An action ends the read of the image it was given a fragment of, and the call
    /// goes on to the next image, in turn as in `poll`, until `fragment_limit`
    /// fragments are taken or every image was read once. A handler that must take
    /// nothing more in this call returns [`Action::Abort`] for each fragment it is
    /// handed after that. Each image's position is stored after its share, so an
    /// aborted fragment is never behind the position the driver reads.
    ///
    /// Returns how many fragments were taken, or the [`Fault`] that stopped the call,
    /// as `poll` does. Nothing here allocates, locks, opens or unmaps, on the fault path
    /// either.
    pub fn controlled_poll<F>(
        &mut self,
        mut handler: F,
        fragment_limit: usize,
    ) -> Result<usize, Fault>
    where
        F: FnMut(&[u8], &DataFrameHeader) -> Action,
    {
        if self.shared.closed().is_some() {
            return Err(Fault::Closed);
        }
        self.apply_events()?;
        let count = self.images.len();
        if count == 0 {
            return Ok(0);
        }
        let start = self.next % count;
        self.next = (start + 1) % count;
        let mut taken = 0;
        let mut faulted: Option<(usize, LogError)> = None;
        for step in 0..count {
            if taken >= fragment_limit {
                break;
            }
            let index = (start + step) % count;
            let image = &mut self.images[index];
            match image
                .reader
                .controlled_poll(i64::MAX, fragment_limit - taken, &mut handler)
            {
                Ok(poll) => {
                    taken += poll.fragments;
                    image.position.store_release(poll.position);
                }
                Err(error) => {
                    faulted = Some((index, error));
                    break;
                }
            }
        }
        if let Some((index, error)) = faulted {
            let image = self.images.swap_remove(index);
            let fault = Fault::Image {
                session_id: image.session_id,
                position: image.position(),
                reason: error.reason(),
            };
            self.retire(image);
            return Err(fault);
        }
        Ok(taken)
    }

    /// Reads every image but the first of `session_id` with no bound, up to
    /// `fragment_limit` fragments in all, and drops what it reads: for a reader of one
    /// session on a stream others may publish on, whose images would otherwise hold
    /// their publishers at this subscription's position (`spec/log-buffer.md`, "The
    /// driver": a publication's limit follows its slowest subscriber).
    pub(crate) fn discard_others(
        &mut self,
        session_id: i32,
        fragment_limit: usize,
    ) -> Result<usize, Fault> {
        if self.shared.closed().is_some() {
            return Err(Fault::Closed);
        }
        self.apply_events()?;
        let kept = self
            .images
            .iter()
            .position(|image| image.session_id == session_id);
        let mut dropped = 0;
        let mut index = 0;
        while index < self.images.len() && dropped < fragment_limit {
            if Some(index) == kept {
                index += 1;
                continue;
            }
            // A fault removes the image at `index` and returns: the next call goes on.
            dropped += self.bounded_poll_at(
                index,
                i64::MAX,
                fragment_limit - dropped,
                &mut |_: &[u8], _: &DataFrameHeader| {},
            )?;
            index += 1;
        }
        Ok(dropped)
    }

    /// The bounded read of the image at `index`, its position stored after it; a frame
    /// that fails a check removes the image, as in [`poll`](Self::poll).
    fn bounded_poll_at<F>(
        &mut self,
        index: usize,
        limit_position: i64,
        fragment_limit: usize,
        handler: &mut F,
    ) -> Result<usize, Fault>
    where
        F: FnMut(&[u8], &DataFrameHeader),
    {
        let Some(image) = self.images.get_mut(index) else {
            return Ok(0);
        };
        match image
            .reader
            .bounded_poll(limit_position, fragment_limit, handler)
        {
            Ok(poll) => {
                image.position.store_release(poll.position);
                Ok(poll.fragments)
            }
            Err(error) => {
                let image = self.images.swap_remove(index);
                let fault = Fault::Image {
                    session_id: image.session_id,
                    position: image.position(),
                    reason: error.reason(),
                };
                self.retire(image);
                Err(fault)
            }
        }
    }

    /// Applies what the conductor queued: new images join, withdrawn ones go.
    fn apply_events(&mut self) -> Result<(), Fault> {
        while let Some(event) = self.incoming.poll() {
            match event {
                ImageEvent::Available {
                    correlation_id,
                    session_id,
                    log,
                    position,
                    source_identity,
                } => {
                    if self.images.len() >= MAX_IMAGES {
                        self.retire_parts(log, position);
                        return Err(Fault::ImagesFull { session_id });
                    }
                    let join = position.load_acquire();
                    let reader = match log.reader(join) {
                        Ok(reader) => reader,
                        Err(error) => {
                            self.retire_parts(log, position);
                            return Err(Fault::Image {
                                session_id,
                                position: join,
                                reason: error.reason(),
                            });
                        }
                    };
                    self.images.push(Image {
                        correlation_id,
                        session_id,
                        source_identity,
                        reader,
                        position,
                        log,
                    });
                }
                ImageEvent::Unavailable { correlation_id } => {
                    if let Some(index) = self
                        .images
                        .iter()
                        .position(|image| image.correlation_id == correlation_id)
                    {
                        let image = self.images.swap_remove(index);
                        self.retire(image);
                    }
                }
                ImageEvent::Gone { session_id } => {
                    return Err(Fault::Image {
                        session_id,
                        position: NULL_POSITION,
                        reason: Fault::GONE,
                    });
                }
            }
        }
        Ok(())
    }

    /// Hands a mapping back to the conductor to drop. When the retire queue is full the
    /// mapping is dropped here, which unmaps on this thread: rare, since the conductor
    /// drains the queue on every cycle, and said here.
    fn retire(&mut self, image: Image) {
        let retired = image.retire();
        if let Err(retired) = self.retired.offer(retired) {
            drop(retired);
        }
    }

    fn retire_parts(&mut self, log: LogBuffers, position: Counter) {
        let retired = Retired {
            _log: log,
            _position: position,
        };
        if let Err(retired) = self.retired.offer(retired) {
            drop(retired);
        }
    }

    /// How many images the subscription reads from.
    pub fn image_count(&self) -> usize {
        self.images.len()
    }

    /// The images, in no particular order.
    pub fn images(&self) -> &[Image] {
        &self.images
    }

    /// Whether at least one publication is connected: an image exists.
    pub fn is_connected(&self) -> bool {
        !self.images.is_empty()
    }

    /// The id under which the driver knows this subscription: the add command's
    /// correlation id.
    pub fn registration_id(&self) -> i64 {
        self.registration_id
    }

    /// The stream id.
    pub fn stream_id(&self) -> i32 {
        self.stream_id
    }

    /// The channel.
    pub fn channel(&self) -> &str {
        &self.channel
    }

    /// Whether the client this subscription belongs to is closed, so that every poll
    /// from here is [`Fault::Closed`]. [`Subscription::close`] consumes the handle, so a
    /// subscription the caller still holds is closed only with its client.
    pub fn is_closed(&self) -> bool {
        self.shared.closed().is_some()
    }

    /// Sends `remove subscription`, so the driver withdraws this subscription's images
    /// and frees their position counters, and drops the images here, on the control
    /// plane. A driver that is gone cannot be told, and that is not an error here.
    pub fn close(mut self) -> Result<(), ClientError> {
        self.remove();
        Ok(())
    }

    fn remove(&mut self) {
        if self.removed {
            return;
        }
        self.removed = true;
        let command = Command::RemoveSubscription {
            client_id: self.client_id,
            correlation_id: self.producer.next_correlation_id(),
            registration_id: self.registration_id,
        };
        if let Ok(length) = command.write(&mut self.scratch) {
            let _ = self.producer.write(
                command.command_type() as i32,
                &self.scratch[..length],
                epoch_time(),
            );
        }
        self.images.clear();
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.remove();
    }
}

impl fmt::Debug for Subscription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Subscription")
            .field("registration_id", &self.registration_id)
            .field("stream_id", &self.stream_id)
            .field("channel", &self.channel)
            .field("images", &self.images.len())
            .field("closed", &self.is_closed())
            .finish()
    }
}
