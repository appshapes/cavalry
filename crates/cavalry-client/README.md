# cavalry-client

The client API implementation, re-exported by the `cavalry` crate. Data plane.

## What this does

- Attaches a process to the driver through shared memory and creates publications (a
  handle for writing one stream) and subscriptions (a handle for reading one stream,
  possibly from several publishers) on channels such as `cavalry:udp?endpoint=host:port`
  and `cavalry:ipc`. IPC and UDP are the same API.
- Waits for a driver that is still starting. `Cavalry::connect` on a directory whose
  command ring is there while one of the driver's three files is missing, empty or
  `not-ready` reads the directory again every 10 ms, for up to the driver timeout,
  10 s, so a client started right after its driver connects with no retry of its own.
  A driver that died while it created its files is `driver-gone` at that bound. The
  wait is for an unfinished file alone: a directory with no command ring is
  `not-found` at once, and one a stopped driver left is `driver-gone` at once (ADR
  0072).
- Publishes by claim and commit: claim a slice of the log buffer, write into it in
  place, commit it. Nothing is allocated per message, and a codec encodes straight into
  the claimed slice.
- Delivers to the subscriber's fragment handler as a view into mapped memory, one image
  per connected publisher. `Subscription::poll` reads every image in turn;
  `Subscription::bounded_poll` reads one image, the one of a session, and no frame
  that begins at or past a limit position, for a stream that may be read only as far
  as something else has got, as a cluster's service reads its log no further than the
  commit position (ADR 0066).
- Lets a handler leave a fragment unread: `Subscription::controlled_poll` hands each
  fragment to a handler that returns an `Action`. `Action::Continue` takes the fragment
  and goes on, `Action::Break` takes it and ends the read of that image, and
  `Action::Abort` leaves it at the image's position, so the next read hands the same
  fragment over again. A reader that must offer what it reads on a publication that
  is back-pressured aborts, and its own publisher is then held at its window: nothing
  is buffered between the two (ADR 0114).
- Returns back-pressure as a result code when the receiver window or the log buffer is
  full. The caller decides what to do; the driver never buffers more.
- Says when its client is gone. Once the client is closed, by `close` or because the
  driver went, every claim and offer on a publication is `Offer::Closed` before
  anything is reserved, so no frame lands past the stream's end, and every poll of a
  subscription is `Fault::Closed`. A subscription the client cannot hold is refused with
  `subscriptions-full`; it is never left open without its images.
- Stays open when an image comes and goes before its conductor has read the image's
  announcement. The driver keeps an announced image's log and position counter for
  1 s, so a publication added and removed at once, or a replay that fails at its
  first frame, reaches the subscription as an image that came and ended. A client
  that reads the announcement later than that does not add the image: the
  subscription's next poll returns `Fault::Image` with the reason `Fault::GONE` and
  `NULL_POSITION`, once, which says that frames published after the join, if any,
  were not delivered, and the subscription and the client go on (ADR 0078).
- Lets a `cavalry:udp` publication choose the term length of its log:
  `|term-length=<length>` on its channel, a power of two from 65536 to 67108864 written
  in bytes or with `k`, `m` or `g`, as `|term-length=1m`. The log is three terms,
  resident from its creation, so a channel that names none holds 48 MiB and one that
  names `64k` holds 196 KiB; `Publication::max_message_length` is the term ÷ 8 and the
  publication window is half the term. Name a term that holds what the publisher
  writes in 16 milliseconds at its fastest: 64 KiB for 4 MiB a second, 1 MiB for 64 MiB
  a second. A subscription's channel names none, since an image takes its sender's
  (ADR 0099).
- Gives a publication on a control channel,
  `cavalry:udp?control=<host>:<port>|control-mode=manual`, its destinations:
  `Cavalry::add_destination(publication.registration_id(), "cavalry:udp?endpoint=<host>:<port>")`
  and `remove_destination` with the same string, each answered by the driver, so that
  the sender sends every datagram to each destination, multi-destination cast (ADR
  0053); `|fc=min` on the control channel lets the slowest destination's receiver pace
  the sender.
- Appends to a recorded `cavalry:udp` stream that nobody receives yet, when the
  publication's channel names `|ssc=true`: a live recording of the publication then
  connects it, so `offer` and `try_claim` succeed with no receiver, and are back
  pressured by the recorder alone. A receiver that joins later is given the stream
  from where the sender stands, and what came before is in the recording, for
  `Cavalry::start_replay`. Without the key a `cavalry:udp` publication is connected by
  a receiver, recorded or not (ADR 0098).
- Records a stream to the durable log: `Cavalry::start_recording(channel, stream_id)`
  asks the driver to record the stream's live publication and returns a `Recording`
  whose `position()` and `synced_position()` read the recorder's two counters;
  `Cavalry::stop_recording(id)`, or `Recording::stop(&mut client)`, stops it from any
  client. `Cavalry::sync_recording(id)`, or `Recording::sync(&mut client)`, has the
  recorder sync what it has written of the recording at once, where its own sync runs
  once a second, and returns when the sync has: a program that acknowledges what it
  recorded waits for one sync, not for the next second, and acknowledges up to
  `synced_position()`, which a failed sync leaves below what was written.
  `Recording::request_sync(&mut client)` asks for the same sync and does not wait for
  the answer, for a caller that must not block; it allocates nothing (ADR 0069). The
  recording is the driver's:
  it outlives the client, and dropping the handle stops nothing (ADR 0025).
  The client follows the stop of each recording it started, whoever stopped it:
  `Recording::stop_position()` is `None` while the recording is live and its stop
  position after the driver's `recording stopped`, and from then on both positions are
  that position. A sync that returned success covered it: the catalog's stop, or,
  when the stop's own writes or syncs failed or a sync of the recording failed
  before it, the position last synced (ADR 0064, ADR 0065). The
  driver frees a stopped recording's counters and gives their
  records to other counters a second later, so each read checks that the record is
  still the recording's, and a position is never another counter's value. A client
  follows 1,024 recordings at most, the driver's own bound.
- Reads a member's committed log on a stream the caller names:
  `Cavalry::committed_log_on(stream_id, position)` is `committed_log` below on another
  `cavalry:ipc` stream. A cluster member's consensus module reads its own log so, on
  the log replay stream, `LOG_REPLAY_STREAM_ID`, 103, so that its replay and a
  service's never reach each other's subscription (ADR 0116).
- Reads a cluster's committed log for a service: `Cavalry::committed_log(position)`
  returns a `CommittedLog` once the driver hosts a cluster member that has a recording
  of its log, and `CommittedLog::poll` hands over every entry a quorum of members
  holds, in order, each once, with its position. The log is read from the member's
  own recording through a replay that follows it, on the leader and on a follower
  alike, and no further than the member's commit position, which is taken only while
  the member's counter still names the live recording it named when the log was
  opened (ADR 0066, ADR 0103). A member's
  log is one recording or several in a row, which the member lists in a file in its
  driver directory: a handle replays the one that holds the position it was opened
  at, is ended at that recording's stop, and the log opened again at
  `CommittedLog::position()` is the next, so a service reads every entry once, in
  order, across them. With no file the call loads the member's counter again, and
  opens the live recording only when the counter still names it: a member that
  writes no file is of an earlier version, and one that has closed has stored −1
  there before it removed its file (ADR 0103). A service may
  start before its member, with it or after it, at position 0 or where it stopped.
  It reads until `CommittedLog::is_ended()`, and then three calls say whether it was
  given every entry below its bound: it was when all three return `None`. If the
  driver's replay of the recording failed, `CommittedLog::failed_at()` gives the
  position it failed at, and the entries from there on were not delivered. If the
  driver removed the replay's image, 5 s after the replay ended, before the service
  read it, `CommittedLog::withdrawn_at()` gives the position where the undelivered
  entries begin. If the client's own read of the replay's image stopped on a fault,
  the poll that met it returned the fault, and `CommittedLog::faulted()` gives it
  from then on, for as long as the handle is held (ADR 0080).
- Holds a session with a cluster: `Cavalry::connect_cluster(members,
  response_channel)` takes the members string the cluster was started with and a
  channel of this client's host, `cavalry:udp?endpoint=<host:port>`, for the cluster's
  answers. It asks one member after another, follows a redirect to the leader, and
  returns a `ClusterSession` once the leader has opened the session, or a
  `ClusterError` that says what to do: the leader's refusal with its reason, a
  cluster of another protocol version, a members string that fails a check, or no
  answer in 20 s. `ClusterSession::offer(message)` sends the leader a message of one
  frame, 1,344 bytes at most on the default MTU, with back pressure returned as
  `Offer`. `ClusterSession::poll(handler, fragment_limit)` hands over the cluster's
  replies, each with the cluster's time, and is the session's duty cycle: it sends a
  keepalive when 500 ms have passed since the session last sent anything, turns the
  session to a new leader when the cluster elects one, and closes it when the cluster
  says so, after which `closed()` gives the reason. A session whose leader is lost,
  which it knows by the leader's image going from its response channel, waits 20 s
  for the member that leads next to say so, and then closes itself with the reason
  `NO LEADER`. A program calls `poll` at least
  every 500 ms: the leader closes a session it has heard nothing from for 10 s. The
  session holds a publication to every member from the connect on, so nothing in
  `offer` or `poll` allocates or sends the driver a command. `offer` reads no clock:
  the next `poll` counts the message as sent, for the keepalive, and a poll reads the
  clock once. A message `offer` accepted is on its way, not committed: a program that
  must know asks its service to reply (ADR 0117).
- Runs a cluster's service: `Cavalry::cluster_service()` returns a `ClusterService`
  beside the cluster member on this client's driver, and
  `ClusterService::poll(client, handler, fragment_limit)` reads the member's
  committed log from its start and hands over each entry as an `Entry`, a session's
  message, a session's open or close, or a term's first entry, with its position, its
  term and the cluster's time, which is the one clock a service reads. The handler
  answers a session with `Replies::reply(session_id, payload)`, one frame, 1,344
  bytes at most. Every member's service runs the same handler on the same entries
  and one of them sends: the service beside the member that led the entry's term,
  which the log names. The others are answered `Reply::NotAnswering`, and so is a
  service for the entries its member held before the service first read its log, so
  that a service that starts again answers no client twice. A reply the client's
  channel does not take is `Reply::Retry`: the handler returns `Action::Abort` and
  is handed the entry again, so it replies before it changes its state. An entry
  handed over again makes its replies again, in the same order, and the service
  offers each to its session once: what an earlier handling sent or dropped it
  does not offer again, so no client reads a reply twice. A service
  waits so for one client no longer than 500 ms, the keepalive interval. After that
  the reply is `Reply::Dropped`, counted in `cluster.replies_dropped`, and each
  reply to that client is dropped at once until its channel takes one. Nothing in
  `poll` allocates for an entry or a reply, and its commands to the driver are at a
  session's opening, at a change of the term's leader and at a recording's end
  (ADR 0118).
- Holds a recording the driver arms: `Cavalry::arm_recording(channel, stream_id)`, on a
  `cavalry:udp` stream with a subscription and no publication or image yet, returns
  an `ArmedRecording`, and `ArmedRecording::poll()` returns the `Recording` once the
  driver has started it with the stream's next image, at the image's join position, so
  that every frame of the image is recorded (ADR 0059). This is the order a cluster
  follower needs: subscribe, arm, then the leader's first frame. The driver drops an
  armed recording when the stream's last subscription goes, and `poll()` then returns
  its error, code 20. `Cavalry::start_recording` on such a stream returns
  `ClientError::RecordingArmed`, reason `recording-armed`, and holds nothing, while
  the driver keeps the armed recording.
  `Cavalry::truncate_recording(id, position)`, `purge_segments(id, segment_base)` and
  `purge_recording(id)` cut, purge and delete a stopped recording; a live one, one a
  replay reads or one under another of the three is refused with code 24 (ADR 0031).
- Plays a recording back: `Cavalry::start_replay(recording_id, position, length,
  stream_id, channel)` asks the driver for a replay onto a stream a subscription of this
  or any client reads as any stream, its image's positions the recording's, and returns
  a `Replay` whose `position()` reads the replayer's counter and whose `ended()` says
  where and why it ended once `replay stopped` has arrived; `Cavalry::stop_replay(id)`,
  or `Replay::stop(&mut client)`, ends it from any client, and dropping the handle stops
  nothing (ADR 0030). `Image::position_of(&header)` is a frame's position in the
  recording, and `Image::positions()` the same arithmetic as a value a handler holds; a
  padding frame moves the position and is handed to no handler. The client follows the
  end of every replay it started while the handle lives, as many as the driver allows
  live at once; one more is refused with `replays-full` and stopped again, and a start
  while 16 starts wait for the client's own conductor is refused with
  `replay-queue-full` before anything reaches the driver.
- Adds a counter the client owns: `Cavalry::add_counter(type_id, key, label)` has the
  driver allocate a record of its counters file, of a type `counters.toml` gives the
  scope `client`, and returns a `Counter` that alone writes the value, with a release
  store for a position another process loads. `cavalry doctor` and every reader of
  the file see it. The driver frees it when the `Counter` is closed or dropped and
  when the client closes or is timed out, and a `Counter` stores nothing once its
  client is closed, since the record may be another counter's by then
  (`spec/client-protocol.md`, "Commands"; ADR 0062).

In Aeron, this is the client, with `tryClaim` and `commit`
([glossary](../../docs/glossary.md)).

## What it must never do

- Lie about delivery (invariant 1). A stream is ordered; a message is delivered once, or
  its absence is reported to the subscriber.
- Allocate, lock, make a syscall, await or panic per message (invariant 2). The path
  from claim to the fragment handler is judged at p99.99 and max, never at the mean.
- Allocate for another client's image. The driver announces every image to every
  client of its directory, and a client's conductor copies nothing from an `available
  image` whose subscription is not its own, so one program's subscriptions cost its
  neighbours no allocation (`tests/foreign_image_allocations.rs`).
- Absorb back-pressure. It is explicit and returned to the caller, never hidden behind a
  growing buffer (invariant 3).
- Rename a concept. Publication, subscription, image, session, position and back-
  pressure keep the names in the glossary.

## Example

One process publishes and subscribes through the driver on this host: this is the
ten-minute path's program without its recording and its UDP step, and the crate's
doctest. `cavalry run` in another terminal is the usual driver; the doctest embeds one.

```rust
use cavalry_client::{Cavalry, Offer};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // A driver of our own for the doctest; `cavalry run` is the usual shape.
    let dir = std::env::temp_dir().join(format!("cavalry-client-readme-{}", std::process::id()));
    let log_dir = dir.with_extension("log");
    let driver = cavalry_driver::Driver::start(&dir, &log_dir)?;

    // `Cavalry::connect()` reads CAVALRY_DIR or the default directory; this names one.
    let mut client = Cavalry::connect_to(&dir)?;
    let mut subscription = client.add_subscription("cavalry:ipc", 1001)?;
    let mut publication = client.add_publication("cavalry:ipc", 1001)?;

    // Publish by claim and commit: write in place, nothing copied, nothing allocated.
    // Until the driver has made the subscription's image, it answers `not-connected`.
    // The subscription takes the image on a later poll, so a poll right after the first
    // accepted claim can deliver nothing; nothing is lost, and the loop below polls
    // until the message arrives. `Subscription::poll` says how to wait for the image.
    loop {
        match publication.try_claim(5) {
            Ok(mut claim) => {
                claim.body().copy_from_slice(b"hello");
                claim.commit();
                break;
            }
            Err(Offer::NotConnected | Offer::BackPressured | Offer::AdminAction) => {
                subscription.poll(|_, _| {}, 1)?;
            }
            Err(other) => return Err(other.into()),
        }
    }

    // Subscribe by polling: each fragment is a view into the log, with its header.
    let mut received = None;
    while received.is_none() {
        subscription.poll(|body, header| received = Some((body.to_vec(), header.session_id())), 10)?;
    }
    let (body, session) = received.unwrap();
    println!("received {:?} from session {session}", std::str::from_utf8(&body)?);

    publication.close()?;
    subscription.close()?;
    client.close()?;
    driver.close()?;
    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_dir_all(&log_dir).ok();
    Ok(())
}
```

Running it prints one line, such as `received "hello" from session -1716223105`. A
directory with no driver is refused with what to do:

```text
client refused (not-found): no driver at /tmp/nowhere; start one with `cavalry run`, or set CAVALRY_DIR to the directory of the driver to use; see docs/reference/client-protocol.md#the-driver-directory
```

## What to read next

- [`spec/client-protocol.md`](../../spec/client-protocol.md): how a client attaches, the
  commands it sends, the events it reads, and the timeouts its conductor keeps; and
  [ADR 0021](../../docs/decisions/0021-shape-the-client-api-after-aerons.md), the shape
  of this API and why its two threads share no lock.
- [`spec/log-buffer.md`](../../spec/log-buffer.md), "Claim, commit and abort" and
  "Reading": what a publication and a subscription do with a log.
- [Configuration](../../docs/reference/config.md): `CAVALRY_DIR`, the one key a client
  reads; and [`cavalry-driver`](../cavalry-driver/README.md), the driver it attaches to.
