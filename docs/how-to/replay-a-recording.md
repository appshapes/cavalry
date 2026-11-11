# Replay a recording into a subscription

A how-to page: one task, play a recording of the durable log back from a position or
an instant, read it as a stream, and see where it ended. What the commands mean is
[`spec/client-protocol.md`](../reference/client-protocol.md), "Commands"; how an instant
becomes a position is [`spec/durable-log.md`](../reference/durable-log.md), "Time index".
In Aeron, this is Archive's `startReplay` ([glossary](../glossary.md)).

## Prerequisites

- A driver running on this host ([Run a driver and read doctor](run-the-driver.md)),
  on the log directory that holds the recording; `--log-dir <path>` or
  `CAVALRY_LOG_DIR` names another than the default.
- A recording, live or stopped ([Record a stream to the durable log](record-a-stream.md)).
  `just doctor` lists the live ones by id; the catalog holds every one, and the id of a
  stopped recording is the one `cavalry record` printed when it started.

## Steps

1. Play the whole recording back, from its start to its extent:

   ```sh
   cargo run --release -p cavalry -- replay --recording 0 --from 0
   ```

   You will see one line per message, the message's position in the recording and its
   body, as text when the body is UTF-8 with no control character but a tab and as
   lowercase hex otherwise:

   ```text
   0 hello
   64 world
   replay 7 of recording 0 ended at position 128: 2 messages from position 0
   ```

   The last line says where the replay started and ended and how many messages it
   printed. A message longer than the MTU arrives in fragments, one line each, the
   first marked `fragment begin`, the last `fragment end` and those between `fragment`.
   A position is read from the frame's header, and feeding one back as `--from` plays
   from that message. A padding frame, which fills the end of a term or covers a claim
   that was aborted or unblocked, is not a message: it prints nothing, and the position
   after it is past its length. The command subscribes to a replay stream of its own,
   so two commands on one recording never mix their lines; `--stream <id>` names the
   stream instead, for a program that is already subscribed to it, and only the
   replay's frames are printed when other publications share that stream.

2. Play a part, from an instant to an instant:

   ```sh
   cargo run --release -p cavalry -- replay --recording 0 \
       --from 2026-10-29T09:36:00Z --to 2026-10-29T09:37:00Z
   ```

   An instant is an RFC 3339 time, with an optional fraction of a second and `Z` or an
   offset, or `@<nanoseconds since the Unix epoch>`; a whole number is a position, a
   multiple of 32. `--from <instant>` resolves to the position of the last time index
   entry at or before the instant, or the start position when the instant is before the
   first; `--to <instant>` to the position of the first entry after the instant, or the
   last entry's when there is none; the index has at most one entry per millisecond, so
   that is the granularity. The last line reports the positions resolved. A frame that
   begins before `--to` is played whole.

3. Follow a live recording:

   ```sh
   cargo run --release -p cavalry -- replay --recording 0 --from 0 --follow
   ```

   The command prints what the recorder has synced, waits for more, and ends when the
   recording stops, at its stop position. Without `--follow` or `--to`, a replay ends at
   the recording's extent as it was when the replay started: its stop position, or the
   synced position of a live one.

4. Play a recording to a subscriber on another host:

   ```sh
   cargo run --release -p cavalry -- replay --recording 0 --from start \
       --channel cavalry:udp?endpoint=192.0.2.10:40123 --stream 7
   ```

   The subscriber goes first, on the other host, on its own address and that port, on
   the stream the command names, in a program there:
   `client.add_subscription("cavalry:udp?endpoint=192.0.2.10:40123", 7)?`. The command
   then subscribes to nothing and prints no message, since the messages go to the
   other host; it ends with the replay and prints the summary alone:

   ```text
   replay 7 of recording 0 ended at position 128: 128 bytes from position 0 on cavalry:udp?endpoint=192.0.2.10:40123
   ```

   The exit code is 0 when the replay `ended`, and 1 when it `ended for want of a
   subscriber`, with `0 bytes` when nothing was played, so a script sees a replay that
   delivered nothing fail; the bullet under "When it does not work" says what to check.
   `--stream` is required with a channel other than `cavalry:ipc`: the subscriber reads
   the stream it chose, and the command cannot pick one of its own. The replay's
   publication goes on the wire from the driver's sender as a `cavalry:udp` publication
   does, with the recording's term length and MTU, so the subscriber's image carries the
   recording's positions
   ([ADR 0043](../decisions/0043-replay-a-recording-over-a-udp-channel-through-the-sender.md)).

From a program, the same is `client.add_subscription("cavalry:ipc", stream_id)?`
first, then `client.start_replay(recording_id, position, length, stream_id,
"cavalry:ipc")?`, which returns a `Replay` with `start_position()`, `position()` from
the replayer's counter and `ended()`, the end position and the reason once `replay
stopped` has arrived; `position` is `NULL_POSITION` for the start, `length` is
`NULL_LENGTH` for the extent or `FOLLOW_LENGTH` to follow. The subscription's image
of the replay, the one whose `session_id()` is `replay.session_id()`, carries the
recording's positions: `image.position_of(&header)` is a frame's position in the
recording, and `image.positions()` is the same arithmetic as a value the fragment
handler holds, `positions.position(header.term_id(), header.term_offset() as u32)`,
since the handler cannot reach the image while the subscription polls. The image
joins at a poll; `subscription.poll(|_, _| {}, 0)?` applies the images the driver
announced and reads no frame, so a program takes the positions before its first
message. Poll until `image.position()` reaches the end `ended()` reports: an image
that goes first took the rest with it, and the program has read to where it has.
Once every subscriber of the image has read to the end of stream position, or the
publication linger, 5 s, has passed, the driver retires the image as it does a closed
publication's (`spec/client-protocol.md`, "Commands" and "Events"), so a program that
needs the image's fields after the end takes them from the poll that read it there; a
later poll may find the image gone.
`cavalry::index::position_at(log_dir, recording_id, instant)` and `position_after`
resolve instants as the command does. The replay is the driver's: dropping the handle
stops nothing, and `client.stop_replay(id)?`, or `replay.stop(&mut client)?`, ends it
from any client. The client follows the end of every replay it started while the
handle lives, as many as the driver allows live at once, 64; one more is
`replays-full`, and stopped again.

To play a recording to a subscriber on another host, pass a `cavalry:udp` channel:
`client.start_replay(recording_id, position, length, stream_id,
"cavalry:udp?endpoint=<host>:<port>")?`, where the host and port are the ones a
program on the other host subscribed on with
`client.add_subscription("cavalry:udp?endpoint=<its address>:<port>", stream_id)?`.
The driver's sender puts the replay's publication on the wire as it puts a client's
`cavalry:udp` publication, with the recording's term length and MTU, so the
subscriber's image carries the recording's positions as over `cavalry:ipc`; the
subscriber's driver creates the image from the sender's setup frame, and a
`cavalry:ipc` subscription on the replay stream sees nothing of it. Subscribe on the
other host first: a replay no receiver answers ends after the replay connect timeout,
5 s, with `ended for want of a subscriber`, and `cavalry doctor` on the replaying host
names `replay.not_connected`. The replay ends and lingers as over `cavalry:ipc`, and
its send endpoint stays open for the publication linger after it
([ADR 0043](../decisions/0043-replay-a-recording-over-a-udp-channel-through-the-sender.md)).

## How to verify it worked

- The lines' positions start at the position the last line reports and rise by each
  message's aligned length, its body and 32-byte header rounded up to a multiple of
  32, and by more over a padding frame, which prints nothing; the last line says
  `ended`, with no `read to position` after the count.
- `just doctor` after the replay reports no `replay-*` finding; the `replay.ended`
  counter grew by one.
- Over a `cavalry:udp` channel, the summary line's byte count is the end position less
  the start, and on the receiving host `image.created` grew by one and the
  subscription's image reached that end position.
- Running the command twice prints the same lines: a replay of the same bytes is the
  same stream (invariant 1). `just replay-check` is that check as a gate: a recording
  of 100,000 hash-chained messages with a padding frame a quarter of the way, the live
  subscriber's lines and two replays' lines hashed and compared, and a replay from an
  instant checked against the index, which a position summed over the lines would fail
  past the padding (`scripts/replay-check.sh`). The same gate replays the recordings
  of the three members of a cluster
  ([Run a cluster of three members](run-a-cluster.md), "How to verify it worked").

## When it does not work

- `error 21 (unknown recording)`: the catalog does not hold that id as a valid
  recording; `doctor` lists the live recordings, and the id a stopped one had is the
  one `cavalry record` printed.
- `error 23 (position out of range)`: the position is not a multiple of 32 from the
  recording's start position to its extent; the message names both. Resolve an instant
  instead, or pass `--from start`, which names the recording's start position whatever
  it is and plays to the extent or, with `--follow`, to the stop; `--from 0` is the
  start only of a recording that started at 0, and a recording that started later
  refuses a position before its start. `--from start` takes no `--to`: the length to
  `--to` is counted from the start position, which only the driver knows, so the
  command refuses the pair before anything is sent; pass the start position error 23's
  message names to `--from` instead.
- `cavalry replay: the replay failed at position N`, with exit status 1 and no message
  printed, where N is the `--from` you passed: N is a multiple of 32 inside the
  recording at which no frame begins, so the bytes there are no frame header. Error 23
  cannot refuse it, since the driver does not know where frames begin until it reads
  them. Pass a position a subscriber took from a frame, `image.position_of(&header)`,
  or resolve an instant with `--at`, or pass `--from start`. The same message at a
  position you did not pass is a recording whose file is damaged there: `cavalry
  doctor` warns of `replay.failed` and names the recording.
- `error 7 (malformed command)`: a length below −1; `--to` before `--from` is refused
  before the command is sent.
- `error 12 (storage space)`: a segment file of the recording is missing from the log
  directory, or the replay's log could not be created in the driver directory; the
  message names the file. A recording that stopped at its start position has no
  segment file and is not this error: a replay of it from that position ends at once
  there with `0 messages`
  ([ADR 0035](../decisions/0035-end-a-replay-with-nothing-to-play-at-once.md)).
- `error 10 (resource temporarily unavailable)`: the replays registry (64), the replay
  publications (128: every replay's publication, live or lingering after its replay
  ended until its subscribers read to the end or five seconds pass), the replayer's
  queue or the counters file is full, or, on a `cavalry:udp` channel, the send
  endpoints registry (64) or the sender's request queue; the message names which. Stop
  replays that are not needed, or wait for the lingering ones to close.
- `error 9 (unknown host)` or `error 4 (channel endpoint error)` on a `cavalry:udp`
  channel: the endpoint's host does not resolve, or its socket could not be opened,
  bound or connected; the message names the host or the operating system's reason.
  Check the name, or pass an address, and that an `interface` named is this host's.
- The replay prints nothing and ends after five seconds with `ended for want of a
  subscriber`: a program started the replay before adding its subscription. Subscribe
  first, as the command does. Over a `cavalry:udp` channel the command prints the
  summary line, `0 bytes` when nothing was played, exits 1, and stderr names the
  endpoint and the stream: no receiver answered the replay's setup frames at the
  endpoint. Check that a driver on the receiving host has a subscription on that
  address, port and stream, and that the port is open between the hosts; `cavalry
  doctor` on this host names `replay.not_connected`, and on the receiving host reports
  `udp-subscription-no-image` while the subscription waits. A subscriber that went
  during the replay ends it the same way, after the connection timeout and then the
  replay connect timeout, with the bytes played in the summary: its image is whole as
  far as it read, and the command again with `--from` at that position plays the rest.
- The replay stops early with `failed`: a frame in a segment file failed a check, or the
  file could not be read; what was printed is whole, and `replay.failed` in `doctor`
  names the recording. The segment file needs restoring from a backup.
- The last line ends in `read to position <position>`, stderr says which positions
  `were not printed`, and the exit code is 1: the driver removed the replay's log
  before the command had read it to the end. An ended replay's log stays for its
  subscribers for the publication linger, 5 s, and the command's stdout held it up for
  longer: a pager, or a pipe into a slow program. What was printed is whole and ends at
  that position. Read faster, into a file for instance, or run the command again with
  `--from` at that position for the rest.
- `client closed (driver-closed)`, or `closed` with another reason, no summary line,
  and the exit code is 1, over a `cavalry:udp` channel: the driver went, or the
  command's client timed out, was lapped or read a corrupt event, before the replay's
  `replay stopped` arrived, so the command does not know where the replay ended and
  names no end. What reached the subscriber is whole up to the position its image
  reports. Run `cavalry doctor` on this host, start the driver if it went, and run the
  command again with `--from` at that position.
- `command refused (replays-full)` from a program: this client has 64 replays live at
  once, the driver's bound, and the new one was stopped again; wait for one to end, or
  stop one.
- `command refused (replay-queue-full)` from a program: this client has 16 `start
  replay` commands whose registrations its own conductor has not taken yet, the queue
  it keeps between two of its cycles, and the new one sent nothing to the driver; the
  bound is the client's, not one of the driver's registries. Wait for a start to be
  answered, then start the next.
- `time index refused (io)` or `rejected (empty)` on `--from <instant>`: the recording's
  index is missing, unreadable or has no entry yet; pass a position, or `--log-dir` the
  directory the driver uses.
- `replay --channel with a channel other than cavalry:ipc needs --stream <id>`, exit
  2, before anything is sent: the subscriber on that channel's endpoint reads the
  stream it chose, and the command cannot pick one of its own. Pass `--stream` with the
  stream id the program on the other host subscribed on.
