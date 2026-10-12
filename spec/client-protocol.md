# Client protocol, version 0

The client protocol is how a client and the driver on one host talk: the driver's
directory, which a client opens to find the driver; the commands a client sends through
the command ring (`spec/command-ring.md`); the events the driver answers with through the
broadcast buffer (`spec/broadcast-buffer.md`); and the timeouts by which each side tells
the other is gone. This page is the record types and message encodings both ring pages
defer to, and it says what the driver does with a command and what it rejects, which is
what bounds the late message write of ADR 0018. It is control plane: a command per
publication or subscription, never a message per frame. In Aeron, this is the control
protocol of `io.aeron.command`, its type ids and flyweights (ADR 0019;
`docs/glossary.md`).

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
| `publications/<correlation id>.logbuffer` | `spec/log-buffer.md` | when a publication is added, named by its registration id |

A driver that starts and finds the directory loads the command ring's consumer heartbeat
with an acquire load: a heartbeat fresher than the driver timeout is `driver-active`, and
the driver refuses to start, naming the directory and the creator pid; a heartbeat of −1
or older than the driver timeout is a dead driver, and the starting driver removes the
directory and creates it anew. A driver that stops stores −1 in the consumer heartbeat and
leaves the directory in place, so that `doctor` can read the counters of a driver that
has gone; the next driver removes it.

A client attaches by opening the command ring writable and loading the consumer
heartbeat, which is `driver-closed` or `driver-timeout` as the ring page says; then the
broadcast buffer read-only, joining at the latest record; then the counters file
read-only. It takes its client id from the ring's correlation id counter, so that no
two clients of one driver share an id. The driver learns of a client from the first
command that names its client id and forgets it on client close or timeout.

## Encoding

A command is a command ring record whose type is the command's type id and whose message
is the fields in the command's table, in order, with no gaps; an event is a broadcast
buffer record the same way. Integers are little-endian (`spec/README.md`). A string field
is an i32 length, from 0 to 4096, the string's bytes in UTF-8, and zero bytes to the next
multiple of 4, so that the field after it is 4-byte aligned. The message's length is
exactly the length of its fields: a record whose message is shorter is `too-short`, one
whose message is longer is `length`, and a string whose length is below 0, above 4096 or
runs past the message is `string`.

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

Type ids 7 to 10 and 12 and above are reserved for the commands later versions specify
with their features; the driver answers one with an `error` event whose code is 6.
Every command begins with the same 16 bytes:

| Offset | Size | Field | Type | Bound |
| --- | --- | --- | --- | --- |
| 0 | 8 | client id | i64 | 0 or more: the id the client took from the correlation id counter |
| 8 | 8 | correlation id | i64 | 0 or more: the id the client took from the correlation id counter for this command; the event that answers it names it |

Then, by command:

| Command | Offset | Size | Field | Type | Bound |
| --- | --- | --- | --- | --- | --- |
| add publication, add exclusive publication, add subscription | 16 | 4 | stream id | i32 | any |
| add publication, add exclusive publication, add subscription | 20 | 4 + n + pad | channel | string | in this version exactly `cavalry:ipc`; any other channel is answered with error code 1 |
| remove publication, remove subscription | 16 | 8 | registration id | i64 | 0 or more: the correlation id of the add command that registered it |

What the driver does with each, after the checks of "What the driver rejects":

- **add publication.** If a concurrent publication with the same stream id and channel is
  live on this driver, the command joins it: the registration id is the command's
  correlation id, and the session id, the log and the counters are the shared ones.
  Otherwise the driver picks a session id, creates the log in `publications/` as
  `spec/log-buffer.md` says, type 0, with the sizes of "Sizes", allocates its
  `publication.position` and `publication.limit` counters (`counters.toml`), and
  registers the publication under the command's correlation id. It answers with
  `publication ready`, then sends `available image` to every subscription whose stream
  id and channel match, as under add subscription. A registry at capacity, or a counters
  file with no free record, is error code 10; a log that cannot be created is error
  code 12.
- **add exclusive publication.** As add publication, with a new session and a log of
  type 1 every time, never shared; the answer is `exclusive publication ready`.
- **remove publication.** The driver stores the end of stream position and lingers as
  `spec/log-buffer.md`, "Closing" says, once the last registration sharing the log is
  removed, and answers `operation success`. A registration id the driver does not know,
  or that belongs to another client, is error code 3.
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

Session ids are the driver's to choose: it starts at a random i32 and adds 1 per new
publication, skipping any id a live publication of the same stream id holds, as Aeron's
driver does. Stream ids are the client's: any i32. A registration id is the correlation id
of the command that registered the publication or subscription.

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

Type ids 3848, 3849 and 3851 and above are reserved for the events later versions
specify; a client that receives one reports `type` and closes, since an event it cannot
decode may be the one it needed.

| Event | Offset | Size | Field | Type | Bound |
| --- | --- | --- | --- | --- | --- |
| error | 0 | 8 | offending correlation id | i64 | 0 or more: the correlation id of the command it answers |
| error | 8 | 4 | error code | i32 | one of "Error codes" |
| error | 12 | 4 + n + pad | error message | string | what the operator does next, ending with the page and section that explains it |
| available image | 0 | 8 | correlation id | i64 | 0 or more: the publication's registration id, which names the image |
| available image | 8 | 4 | session id | i32 | any |
| available image | 12 | 4 | stream id | i32 | any |
| available image | 16 | 8 | subscriber registration id | i64 | 0 or more: the subscription this image belongs to |
| available image | 24 | 4 | subscriber position counter id | i32 | 0 to the counters file's capacity − 1: the `subscription.position` counter the client owns for this image |
| available image | 28 | 4 + n + pad | log file name | string | the log's path relative to the driver's directory, `publications/<correlation id>.logbuffer`, 1 to 4096 bytes |
| available image | after log file name | 4 + n + pad | source identity | string | the publisher's channel, `cavalry:ipc` in this version |
| publication ready, exclusive publication ready | 0 | 8 | correlation id | i64 | 0 or more: the add command's |
| publication ready, exclusive publication ready | 8 | 8 | registration id | i64 | 0 or more: equal to the correlation id in this version |
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

The driver sends `unavailable image` for an image once the publication's end of stream
position is stored and every subscriber of the image has reached it or the linger has
passed, then removes the log (`spec/log-buffer.md`, "Closing"). It sends `client
timeout` after it has removed a timed-out client's publications and subscriptions, so
that the client, if it is only slow, learns that it must attach again. Every event goes
to every client; a client acts on the ones whose correlation id, registration id or
client id is its own and ignores the rest.

## Error codes

| Code | Name | When |
| --- | --- | --- |
| 1 | invalid channel | the channel is not `cavalry:ipc` |
| 2 | unknown subscription | remove subscription names a registration id the driver does not know, or another client's |
| 3 | unknown publication | remove publication names a registration id the driver does not know, or another client's |
| 6 | unknown command type | the record's type is not a command of this version |
| 7 | malformed command | the message failed a check of "What the driver rejects" after its first 16 bytes were read |
| 10 | resource temporarily unavailable | a registry is at capacity or the counters file has no free record |
| 11 | generic error | anything else; the message says what |
| 12 | storage space | the log file could not be created or sized |

Codes 4, 5, 8, 9, 13 and 14 are Aeron's for features outside this version and are
reserved.

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
| counters file | capacity 4096 counters |
| command ring | capacity 1048576 bytes, so the longest command is 131072 bytes |
| broadcast buffer | capacity 1048576 bytes |
| log buffer of a `cavalry:ipc` publication | term length 67108864 bytes, MTU length 1408, publication window term length ÷ 2 |

## What the driver rejects

The driver runs these checks in this order on every command the ring delivers, and the
first that fails is the result; each adds one to `client_protocol.rejected_command`.

| Reason | Check | Answer |
| --- | --- | --- |
| `too-short` | the message is shorter than 16 bytes | none: no correlation id to answer |
| `type` | the type is not a command of this version | `error`, code 6 |
| `too-short` | the message is shorter than the command's fields | `error`, code 7 |
| `string` | a string's length is below 0, above 4096 or runs past the message | `error`, code 7 |
| `length` | the message is longer than the command's fields | `error`, code 7 |
| `channel` | the channel is not `cavalry:ipc` | `error`, code 1 |
| `client` | the command names a client id the driver has timed out | `error`, code 11, and the client attaches again |

A record the ring itself rejects (`spec/command-ring.md`, "Reading") never reaches these
checks and gets no answer: the client's response timeout reports it. A client runs the
same decoding checks, `too-short`, `type`, `string` and `length`, on every event, and
reports the first that fails and closes, as the broadcast buffer page says of `record`.

## Counters

Two static counters in `counters.toml` (`spec/counters-file.md`), written by the driver:
`client.timed_out` and `client_protocol.rejected_command`.

## Vectors

`spec/vectors/client-protocol.json`, written by `spec/vectors/client-protocol.py`, the
reference encoder. Each vector has a `region`, `command` or `event`, the record's `type`
id, the message `bytes`, and either the `fields` a conforming implementation decodes,
which it encodes back to the same bytes, or the `error` it reports, by the name in "What
the driver rejects". A conforming implementation decodes every valid vector to its fields,
encodes those fields to the same bytes, and reports the named reason for every invalid one.
