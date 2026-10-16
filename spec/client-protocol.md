# Client protocol, version 1

The client protocol is how a client and the driver on one host talk: the driver's
directory, which a client opens to find the driver; the commands a client sends through
the command ring (`spec/command-ring.md`); the events the driver answers with through the
broadcast buffer (`spec/broadcast-buffer.md`); and the timeouts by which each side tells
the other is gone. This page is the record types and message encodings both ring pages
defer to, and it says what the driver does with a command and what it rejects, which is
what bounds the late message write of ADR 0018. It is control plane: a command per
publication or subscription, never a message per frame. In Aeron, this is the control
protocol of `io.aeron.command`, its type ids and flyweights (ADR 0019;
`docs/glossary.md`). Version 1 adds the recording commands and events, the first of
Cavalry's own, in type id ranges of its own beside Aeron's, and the error codes they
answer with (ADR 0025); every version 0 encoding is unchanged, so a version 0 client's
commands are version 1 commands.

## The driver directory

One driver per directory, and the directory is how a client finds a driver. The default
is `/dev/shm/cavalry-<user>` on Linux when `/dev/shm` exists, and `<tmp>/cavalry-<user>`
otherwise, where `<tmp>` is the system's temporary directory (`/tmp` on macOS) and
`<user>` the name of the user running the driver. The directory holds:

| Path | Page | Created |
| --- | --- | --- |
| `counters` | `spec/counters-file.md` | when the driver starts, first |
| `command-ring` | `spec/command-ring.md` | when the driver starts, second |
| `broadcast-buffer` | `spec/broadcast-buffer.md` | when the driver starts, last |
| `publications/<registration id>.logbuffer` | `spec/log-buffer.md` | when a publication is added, named by the publication's registration id |

A driver that starts and finds the directory loads the command ring's consumer heartbeat
with an acquire load: a heartbeat fresher than the driver timeout is `driver-active`, and
the driver refuses to start, naming the directory and the creator pid; a heartbeat of −1
or older than the driver timeout is a dead driver, and the starting driver removes the
directory and creates it anew. A directory whose `command-ring` is missing, or fails a
check of its page other than `not-ready`, is a dead driver's too: one that stopped while
creating it. A `command-ring` that is `not-ready` is a driver creating it now: the
starter waits up to the driver timeout for the magic, then treats the file as a dead
driver's. The driver creates the directory and each file with create-new semantics, so
that of two drivers starting at the same moment the second finds the first's directory
and waits or refuses as above; it never removes a file whose creator is alive. A driver
that stops stores −1 in the consumer heartbeat and leaves the directory in place, so
that `doctor` can read the counters of a driver that has gone; the next driver removes it.

A client attaches by opening the command ring writable and loading the consumer
heartbeat, which is `driver-closed` or `driver-timeout` as the ring page says; then the
broadcast buffer read-only, joining at the latest record; then the counters file
read-only. It takes its client id from the ring's correlation id counter, so that no
two clients of one driver share an id. The driver learns of a client from the first
command that names its client id and forgets it on client close or timeout; it keeps no
record of the clients it has forgotten, so a command that arrives from a timed-out
client is a new client's first command, as it is in Aeron, and it is the client that
acts on `client timeout`: a client that receives one with its own id closes, since its
publications and subscriptions are gone. A first command that finds the client registry
full is answered with error code 10 and registers nothing.

## Encoding

A command is a command ring record whose type is the command's type id and whose message
is the fields in the command's table, in order, with no gaps; an event is a broadcast
buffer record the same way. Integers are little-endian (`spec/README.md`). A string field
is an i32 length, from 0 to 4096, the string's bytes in UTF-8, and zero bytes to the next
multiple of 4, so that the field after it is 4-byte aligned. The message's length is
exactly the length of its fields: a record whose message is shorter is `too-short`, one
whose message is longer is `length`, and a string whose length is below 0, above 4096 or
whose bytes or padding run past the message is `string`.

## Commands

| Type id | Command | Fields |
| --- | --- | --- |
| 1 | add publication | client id, correlation id, stream id, channel |
| 2 | remove publication | client id, correlation id, registration id |
| 3 | add exclusive publication | client id, correlation id, stream id, channel |
| 4 | add subscription | client id, correlation id, stream id, channel |
| 5 | remove subscription | client id, correlation id, registration id |
| 6 | client keepalive | client id, correlation id |
| 11 | client close | client id, correlation id |
| 257 (`0x0101`) | start recording | client id, correlation id, stream id, channel |
| 258 (`0x0102`) | stop recording | client id, correlation id, recording id |

Type ids 1 to 255 (`0x01` to `0xFF`) are Aeron's: 7 to 10 and 12 to 255 are reserved for
the commands Aeron's control protocol numbers, present and future, and a later version
takes one with its feature under Aeron's number. Type ids 257 to 511 (`0x0101` to
`0x01FF`) are Cavalry's own, taken in order from 257, and 259 to 511 are reserved for
later versions; 256, and 512 and above, lie outside both ranges and are reserved too. The
driver answers any reserved id with an `error` event whose code is 6. Every command
begins with the same 16 bytes:

| Offset | Size | Field | Type | Bound |
| --- | --- | --- | --- | --- |
| 0 | 8 | client id | i64 | 0 or more: the id the client took from the correlation id counter |
| 8 | 8 | correlation id | i64 | 0 or more: the id the client took from the correlation id counter for this command; the event that answers it names it |

Then, by command:

| Command | Offset | Size | Field | Type | Bound |
| --- | --- | --- | --- | --- | --- |
| add publication, add exclusive publication, add subscription, start recording | 16 | 4 | stream id | i32 | any |
| add publication, add exclusive publication, add subscription, start recording | 20 | 4 + n + pad | channel | string | in this version exactly `cavalry:ipc`; any other channel is answered with error code 1 |
| remove publication, remove subscription | 16 | 8 | registration id | i64 | 0 or more: the correlation id of the add command that registered it |
| stop recording | 16 | 8 | recording id | i64 | 0 or more: the recording id that `recording ready` carried |

What the driver does with each, after the checks of "What the driver rejects":

- **add publication.** If a concurrent publication with the same stream id and channel is
  live on this driver, the command joins it: the client's registration is the command's
  correlation id, and the publication, its registration id, session id, log and counters
  are the shared ones, so `publication ready` carries a registration id that differs
  from its correlation id. Otherwise the driver picks a session id, creates the log in
  `publications/` as `spec/log-buffer.md` says, type 0, with the sizes of "Sizes",
  allocates its `publication.position` and `publication.limit` counters
  (`counters.toml`), and registers the publication under the command's correlation id,
  which is then its registration id and names its log. It answers with `publication
  ready`, then sends `available image` to every subscription whose stream id and
  channel match, as under add subscription. A registry at capacity, or a counters file
  with no free record, is error code 10; a log that cannot be created or sized is error
  code 12.
- **add exclusive publication.** As add publication, with a new session and a log of
  type 1 every time, never shared; the answer is `exclusive publication ready`.
- **remove publication.** The command names the client's registration, the correlation
  id of its add command. The driver removes it and answers `operation success`; once the
  last registration sharing the publication is removed, it stores the end of stream
  position and lingers as `spec/log-buffer.md`, "Closing" says. A registration the
  driver does not know, or that belongs to another client, is error code 3.
- **add subscription.** The driver registers the subscription under the command's
  correlation id and answers `subscription ready`. Then, for every live publication whose
  stream id and channel match, now and whenever one is added later, it allocates a
  `subscription.position` counter for the client, owned by the client (ADR 0013), with
  the publication's producer position as its value, and sends `available image`: the
  subscription joins the stream at the producer's position. A registry at capacity, or a
  counters file with no free record, is error code 10.
- **remove subscription.** The driver removes the subscription and every image it has,
  frees their position counters, and answers `operation success`; no `unavailable image`
  is sent for a subscription its client removed. An unknown registration id, or one that
  belongs to another client, is error code 2.
- **client keepalive.** The driver refreshes the client's liveness. No answer.
- **client close.** The driver removes the client's publications and subscriptions as if
  each had been removed, and forgets the client. No answer.
- **start recording.** The driver finds the live publications whose stream id and channel
  match, those whose end of stream position is not stored, and takes the one with the
  lowest registration id that is not yet being recorded, so that a second command records
  the next one. None live: error code 20. Every one being recorded, or its recording
  starting: error code 22, and the message names the recording id once the recorder has
  given it. The recordings registry at capacity, or the recorder's request queue full or
  holding requests that are still waiting for room in it: code 10, adding one to
  `driver.registry_full`. Otherwise the recording starts at the publication's producer
  position, loaded afresh as a join loads it (ADR 0022): from this command on the
  publication's limit and cleaning respect that position (`spec/log-buffer.md`, "The
  driver"), and the driver asks its recorder for a catalog slot. The recorder takes the
  slot and creates the time index and the first segment file as `spec/durable-log.md`,
  "The driver" says; the driver then allocates the recording's `recording.position` and
  `recording.synced_position` counters (`counters.toml`), keyed by the recording id and
  set to the start position, adds one to `recording.started`, and answers `recording
  ready` with the recording id, the session id and the two counter ids, within the
  client's response timeout. The catalog's slots used up is code 10 and adds one to
  `recording.catalog_full`; a time index or segment file that cannot be created is code 12
  and adds one to `recording.start_failed`; either frees the publication's limit from the
  recording. A counters file with no free record for the two counters, once the recorder
  has taken the slot, is code 10 and adds one to `driver.registry_full`. The slot is taken
  and the recorder is already writing, so the driver stops the recording as a `stop
  recording` would, at the position written, and the limit and the cleaning respect the
  start position until that stop is done. The stop adds one to `recording.stopped` and is
  announced to every client with `recording stopped`, for an id the starting client was
  never given; the catalog keeps the stopped recording. A recording belongs to the driver,
  not to the client that started it: it outlives that client, and any client may stop it.
- **stop recording.** The driver stops the recording at the position the recorder has
  written and answers `operation success` once the catalog's stop fields are written and
  synced (`spec/durable-log.md`, "The driver"); then every client gets `recording
  stopped`. A recording id below 0, or at or past the next recording id, the number of
  slots the catalog holds, is error code 21; one below the next id that no live recording
  holds is already stopped, and the answer is `operation success` with nothing written,
  since a recording id is never reused.

Session ids are the driver's to choose: it starts at a random i32 and adds 1 per new
publication, skipping any id a live publication of the same stream id holds, as Aeron's
driver does. Stream ids are the client's: any i32. A client's registration of a
publication or subscription is the correlation id of the command that added it, and
`remove publication` and `remove subscription` name it. A publication's registration id
is the first such registration, the one that created its log: it names the log file, it
is what `publication ready` and `exclusive publication ready` carry as registration id,
and it is the correlation id of the publication's images in `available image` and
`unavailable image`. For an exclusive publication, or a concurrent one with one
registration, the two are the same number.

The driver stops a recording by itself when its publication closes: at the end of stream
position, once the recorder has written up to it. It also stops a recording whose write
failed, at the position written, adding one to `recording.write_failed`; and one whose
block scan found a committed frame that fails a check of `spec/frame.md`, at that frame,
adding one to `log_buffer.rejected_frame`, as a subscriber that found it would stop
delivering, so the recording ends at the last frame that checked. Every stop, by command,
by close or by failure, adds one to `recording.stopped`, is announced to every client with
`recording stopped`, and frees the recording's two counters after the counter reuse
timeout; the log of a recorded publication is removed only once its recording is stopped
(`spec/log-buffer.md`, "Closing"). The recorder's lag behind the publisher is bounded by
the publication window, since the limit respects its position.

## Events

| Type id | Event | Fields |
| --- | --- | --- |
| 3841 (`0x0F01`) | error | offending correlation id, error code, error message |
| 3842 (`0x0F02`) | available image | correlation id, session id, stream id, subscriber registration id, subscriber position counter id, log file name, source identity |
| 3843 (`0x0F03`) | publication ready | correlation id, registration id, session id, stream id, publication limit counter id, log file name |
| 3844 (`0x0F04`) | operation success | correlation id |
| 3845 (`0x0F05`) | unavailable image | correlation id, subscriber registration id, stream id, channel |
| 3846 (`0x0F06`) | exclusive publication ready | as publication ready |
| 3847 (`0x0F07`) | subscription ready | correlation id |
| 3850 (`0x0F0A`) | client timeout | client id |
| 4097 (`0x1001`) | recording ready | correlation id, recording id, session id, recording position counter id, recording synced position counter id |
| 4098 (`0x1002`) | recording stopped | recording id, stop position |

Type ids 3841 to 4095 (`0x0F01` to `0x0FFF`) are Aeron's: 3848, 3849 and 3851 to 4095 are
reserved for the events Aeron's control protocol numbers, present and future. Type ids
4097 to 4351 (`0x1001` to `0x10FF`) are Cavalry's own, taken in order from 4097, and 4099
to 4351 are reserved for later versions; 4096, and 4352 and above, lie outside both ranges
and are reserved too. A client that receives any reserved id reports `type` and closes,
since an event it cannot decode may be the one it needed. A version 0 client therefore
closes on the first recording event a version 1 driver sends, so an upgrade restarts the
clients before the driver; a version 1 client that sends a recording command to a version
0 driver is answered with code 6 and keeps running.

| Event | Offset | Size | Field | Type | Bound |
| --- | --- | --- | --- | --- | --- |
| error | 0 | 8 | offending correlation id | i64 | 0 or more: the correlation id of the command it answers |
| error | 8 | 4 | error code | i32 | one of "Error codes" |
| error | 12 | 4 + n + pad | error message | string | what the operator does next, ending with the page and section that explains it |
| available image | 0 | 8 | correlation id | i64 | 0 or more: the publication's registration id, the one that created its log, which names the image |
| available image | 8 | 4 | session id | i32 | any |
| available image | 12 | 4 | stream id | i32 | any |
| available image | 16 | 8 | subscriber registration id | i64 | 0 or more: the subscription this image belongs to |
| available image | 24 | 4 | subscriber position counter id | i32 | 0 to the counters file's capacity − 1: the `subscription.position` counter the client owns for this image |
| available image | 28 | 4 + n + pad | log file name | string | the log's path relative to the driver's directory, `publications/<registration id>.logbuffer`, 1 to 4096 bytes |
| available image | after log file name | 4 + n + pad | source identity | string | the publisher's channel, `cavalry:ipc` in this version |
| publication ready, exclusive publication ready | 0 | 8 | correlation id | i64 | 0 or more: the add command's |
| publication ready, exclusive publication ready | 8 | 8 | registration id | i64 | 0 or more: the publication's registration id; equal to the correlation id unless the command joined a live concurrent publication |
| publication ready, exclusive publication ready | 16 | 4 | session id | i32 | any |
| publication ready, exclusive publication ready | 20 | 4 | stream id | i32 | any |
| publication ready, exclusive publication ready | 24 | 4 | publication limit counter id | i32 | 0 to the counters file's capacity − 1: the `publication.limit` counter the publisher loads |
| publication ready, exclusive publication ready | 28 | 4 + n + pad | log file name | string | as available image's |
| operation success | 0 | 8 | correlation id | i64 | 0 or more: the command's |
| unavailable image | 0 | 8 | correlation id | i64 | 0 or more: the image's, as available image named it |
| unavailable image | 8 | 8 | subscriber registration id | i64 | 0 or more |
| unavailable image | 16 | 4 | stream id | i32 | any |
| unavailable image | 20 | 4 + n + pad | channel | string | the publisher's channel |
| subscription ready | 0 | 8 | correlation id | i64 | 0 or more: the add command's, which is the subscription's registration id |
| client timeout | 0 | 8 | client id | i64 | 0 or more: the client the driver timed out |
| recording ready | 0 | 8 | correlation id | i64 | 0 or more: the start recording command's |
| recording ready | 8 | 8 | recording id | i64 | 0 or more: the recording's slot in the catalog (`spec/durable-log.md`) |
| recording ready | 16 | 4 | session id | i32 | any: the session of the publication being recorded |
| recording ready | 20 | 4 | recording position counter id | i32 | 0 to the counters file's capacity − 1: the `recording.position` counter, what the operating system holds |
| recording ready | 24 | 4 | recording synced position counter id | i32 | 0 to the counters file's capacity − 1: the `recording.synced_position` counter, what a sync has made durable |
| recording stopped | 0 | 8 | recording id | i64 | 0 or more |
| recording stopped | 8 | 8 | stop position | i64 | 0 or more, a multiple of 32: the catalog's stop position |

The driver sends `unavailable image` for an image once the publication's end of stream
position is stored and every subscriber of the image has reached it or the linger has
passed, then removes the log (`spec/log-buffer.md`, "Closing"). It sends `client timeout`
after it has removed a timed-out client's publications and subscriptions, so that the
client, if it is only slow, learns that it must attach again. It sends `recording stopped`
after the catalog's stop fields are synced, so the position a client reads from the event
is what the catalog holds. Every event goes to every client; a client acts on the ones
whose correlation id, registration id or client id is its own and ignores the rest.

## Error codes

| Code | Name | When |
| --- | --- | --- |
| 1 | invalid channel | the channel is not `cavalry:ipc` |
| 2 | unknown subscription | remove subscription names a registration id the driver does not know, or another client's |
| 3 | unknown publication | remove publication names a registration id the driver does not know, or another client's |
| 6 | unknown command type | the record's type is not a command of this version |
| 7 | malformed command | the message failed a decoding check of "What the driver rejects" after its first 16 bytes were read |
| 10 | resource temporarily unavailable | a registry of "Sizes" is at capacity or the counters file has no free record |
| 11 | generic error | anything else; the message says what |
| 12 | storage space | the log file, or a recording's time index or segment file, could not be created or sized |
| 20 | no publication to record | start recording names a stream id and channel no live publication has |
| 21 | unknown recording | stop recording names a recording id the catalog does not hold |
| 22 | already recording | start recording names a stream whose live publications are all being recorded; the message names the recording id |

Codes 4, 5, 8, 9, 13 and 14 are Aeron's for features outside this version, 15 to 19 are
left free for Aeron's later codes, and all are reserved; Cavalry's own codes count up
from 20.

## Liveness and timeouts

| Timeout | Value | Measured by | Meaning |
| --- | --- | --- | --- |
| heartbeat interval | 1 s | the driver | how often it stores its clock in the consumer heartbeat |
| driver timeout | 10 s | a client | a consumer heartbeat older than this is `driver-timeout`; the client closes |
| keepalive interval | 500 ms | a client | how often it sends `client keepalive` when it has sent nothing else |
| client liveness timeout | 10 s | the driver | a client whose last command is older than this is timed out |
| response timeout | 10 s | a client | an add or remove command unanswered for this long is reported to the caller as lost; the command may have been `unblocked` (`spec/command-ring.md`) or rejected before it was decoded |
| command ring unblock timeout | 10 s | the driver | the client liveness timeout, as `spec/command-ring.md` says |
| publication unblock timeout | 15 s | the driver | `spec/log-buffer.md`, "Unblocking" |
| publication linger | 5 s | the driver | `spec/log-buffer.md`, "Closing" |
| counter reuse timeout | 1 s | the driver | how long a freed counter's record stays reclaimed before it is allocated again (`spec/counters-file.md`) |

These are Aeron's values, and they are the driver's in this version. Each timeout is
measured on the clock the driver stores in the consumer heartbeat, nanoseconds since the
Unix epoch.

## Sizes

The driver creates its files with these sizes in this version:

| File | Size |
| --- | --- |
| counters file | capacity 8192 counters |
| command ring | capacity 1048576 bytes, so the longest command is 131072 bytes |
| broadcast buffer | capacity 1048576 bytes |
| log buffer of a `cavalry:ipc` publication | term length 67108864 bytes, MTU length 1408, publication window term length ÷ 2 |

Its registries have these capacities, and a command that would exceed one is answered
with error code 10 and registers nothing:

| Registry | Capacity | Counts |
| --- | --- | --- |
| clients | 256 | every client id the driver has not forgotten |
| publications | 1024 | every registration of a publication, shared or not |
| subscriptions | 1024 | every registration of a subscription |
| images | 2048 | every pair of a subscription and a publication that match |
| recordings | 1024 | every recording that is starting or live, one per publication at most |

The counters file is sized so that the registries fill before it does: the static
counters of `counters.toml`, 31 in this version, two per publication (2048), one per
image (2048) and two per recording (2048) come to 6175, fewer than 8192.

## What the driver rejects

The driver decodes every command the ring delivers with these checks, in this order, and
the first that fails is the result; each adds one to `client_protocol.rejected_command`,
and each answer adds one to `client_protocol.errors`.

| Reason | Check | Answer |
| --- | --- | --- |
| `too-short` | the message is shorter than 16 bytes | none: no correlation id to answer |
| `type` | the type is not a command of this version | `error`, code 6 |
| `too-short` | the message is shorter than the command's fields | `error`, code 7 |
| `string` | a string's length is below 0, above 4096, or its bytes or padding run past the message | `error`, code 7 |
| `length` | the message is longer than the command's fields | `error`, code 7 |

The vectors cover these: they are checks on bytes. A command that decodes is then
checked by the driver against its state, which no vector holds; the checks and their
codes are the ones "Commands" and "Error codes" name: a channel that is not
`cavalry:ipc` is `channel`, code 1, and adds one to `client_protocol.rejected_command`;
an unknown or another client's registration is code 2 or 3; a registry or the counters
file at capacity is code 10 and adds one to `driver.registry_full`; a log that could not
be created or sized is code 12 and adds one to `driver.log_create_failed`; a stream id and
channel no live publication has is code 20; a stream whose live publications are all being
recorded is code 22; a recording id the catalog does not hold is code 21; the catalog's
slots used up is code 10 and adds one to `recording.catalog_full`; a recording's files
that could not be created is code 12 and adds one to `recording.start_failed`. Every
`error` event sent adds one to `client_protocol.errors`.

A record the ring itself rejects (`spec/command-ring.md`, "Reading") never reaches these
checks and gets no answer: the client's response timeout reports it. A client runs the
same decoding checks, `too-short`, `type`, `string` and `length`, on every event, and
reports the first that fails and closes, as the broadcast buffer page says of `record`.

## Counters

Static counters in `counters.toml` (`spec/counters-file.md`), written by the driver:
`client.timed_out`, `client_protocol.rejected_command`, `client_protocol.errors`,
`driver.registry_full`, `driver.log_create_failed`, `recording.started`,
`recording.stopped`, `recording.write_failed`, `recording.start_failed`,
`recording.catalog_full`, and the recovery pass's `recording.recovered`,
`recording.invalidated`, `recording.index_rewritten`, `recording.catalog_truncated`,
`recording.slot_dropped` and `recording.files_removed` (`spec/durable-log.md`, "The
driver"). Instance gauges `recording.position` and `recording.synced_position`, one pair
per recording: the key is the recording id as an i64, little-endian, 8 bytes; the label is
the counter's name, a space, and the recording id, the session id and the stream id in
decimal separated by spaces. The driver stores both with release stores, and loads
`recording.position` with an acquire load before it cleans, as it loads a subscriber's
position (`spec/log-buffer.md`, "The driver").

## Vectors

`spec/vectors/client-protocol.json`, written by `spec/vectors/client-protocol.py`, the
reference encoder. Each vector has a `region`, `command` or `event`, the record's `type`
id, the message `bytes`, and either the `fields` a conforming implementation decodes,
which it encodes back to the same bytes, or the `error` it reports, by the name of a
decoding check in "What the driver rejects". A conforming implementation decodes every
valid vector to its fields, encodes those fields to the same bytes, and reports the named
reason for every invalid one. A vector whose channel is not `cavalry:ipc` is valid: the
`channel` check is the driver's, after decoding, and the driver's tests cover it.
