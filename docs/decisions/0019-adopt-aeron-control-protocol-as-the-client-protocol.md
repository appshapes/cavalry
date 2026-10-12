# 0019. Adopt Aeron's control protocol as the client protocol

- **Status:** Accepted
- **Date:** 2026-10-12
- **Invariants affected:** 1, 3, 4, 5, 6
- **Supersedes:** none

## Context

Features 1 to 3 of the first slice are on `master` (ADR 0007): the frame header, the
counters file, the log buffer, the command ring and the broadcast buffer, each with its
page in `spec/`. The two ring pages carry any record whose type is 1 or more and say
that the commands and events themselves, their type ids and encodings, "belong to the
client protocol's page"; ADR 0018's Revisit names that page as where the driver's
validation of a decoded command is stated, which is what bounds the late message write
of a stalled producer. Feature 4, the driver skeleton (card 86), is the conductor that
reads those commands, and feature 5, the client API, sends them. Neither can be written
until the page exists, and a wire format cannot be undone (`CLAUDE.md`, "When in
doubt"), so it is decided here, before any code.

What the driver does with a command is already written on the layout pages: the log's
positions, cleaning, unblocking and closing in `spec/log-buffer.md`, "The driver"; the
ring's creation, reading, unblocking and heartbeat in `spec/command-ring.md`; the
buffer's creation and transmit in `spec/broadcast-buffer.md`. Each of those pages leaves
a value "the driver's to specify": the driver timeout, the client liveness timeout, the
unblock timeouts, the linger, the heartbeat interval, the directory. Aeron's control
protocol, `io.aeron.command` in the client, was read from its source on 2026-10-12:
`ControlProtocolEvents` for the type ids, the flyweights for the fields and offsets,
`ErrorCode` for the codes, `CommonContext` for the directory and `Configuration` for the
timeouts and sizes.

## Decision

`spec/client-protocol.md` version 0 is Aeron's control protocol, type id for type id and
field for field, carried as records of the command ring and the broadcast buffer: the
seven commands of IPC publish/subscribe (add publication 1, remove publication 2, add
exclusive publication 3, add subscription 4, remove subscription 5, client keepalive 6,
client close 11) and the eight events (error, available image, publication ready,
operation success, unavailable image, exclusive publication ready, subscription ready,
client timeout, Aeron's `0x0F01` to `0x0F0A` less the counter events), with Aeron's ids
kept and the gaps reserved so that a later feature adds its commands under Aeron's
numbers. Integers are little-endian; a string is an i32 length from 0 to 4096, UTF-8
bytes and zero padding to a multiple of 4, Aeron's SBE-compatible alignment. Every field
has a bound (ADR 0010), a message's length is exactly its fields' and the driver rejects
a command in a stated order, answering with an `error` event and Aeron's code where the
correlation id was readable and counting `client_protocol.rejected_command`; a client runs
the same checks on events and closes on the first failure. The driver directory is
`cavalry-<user>` under `/dev/shm` on Linux and under the temporary directory elsewhere,
holding `counters`, `command-ring`, `broadcast-buffer` and `publications/<correlation
id>.logbuffer`; a starting driver refuses a directory whose heartbeat is fresh and
recreates one whose driver is gone, and a stopping driver leaves the directory with −1
in the heartbeat for `doctor`. The timeouts are Aeron's: driver timeout 10 s, client
liveness 10 s, keepalive 500 ms, heartbeat 1 s, response 10 s, command ring unblock 10 s,
publication unblock 15 s, linger 5 s, counter reuse 1 s. The sizes are Aeron's: 1 MiB
rings, 64 MiB IPC terms, MTU 1408, window term ÷ 2, and a counters file of 4096. A
concurrent add publication with the same stream id and channel joins the live
publication, as Aeron's does; a subscription's image joins at the producer's position
and owns its `subscription.position` counter (ADR 0013). Four departures from Aeron, each
for a reason the page states: the channel status indicator id is left out of
`publication ready` and `subscription ready`, since IPC has no endpoint and the UDP
version adds it; the unused registration correlation id of add subscription is left out;
log file names are relative to the driver directory, which the client already knows,
not absolute; and a message longer than its fields is rejected (`length`), where Aeron
ignores trailing bytes, because ADR 0010 validates every peer-written field and a new
field is a new version. Two static counters join `counters.toml`: `client.timed_out` and
`client_protocol.rejected_command`.

## Alternatives considered

- **A protocol of Cavalry's own.** Lost: `CLAUDE.md` copies Aeron's semantics and names
  where its behaviour is good, and this protocol is good: one correlation id per command,
  one event per answer, a client id for liveness, and a client that learns it was timed
  out. A stranger who knows Aeron reads the page in minutes (invariant 6).
- **SBE-encoded messages, as Aeron's C driver reads them.** Lost: the Java flyweights
  and the C driver agree on the bytes, which are fixed-offset integers and
  length-prefixed strings; naming the bytes in a table needs no schema language, and a
  schema compiler is a dependency on the control plane nobody asked for.
- **A text encoding, JSON or key-value.** Lost: a parser with no fixed bounds, and a
  message the driver must allocate to decode (invariant 2 does not apply, but ADR 0010's
  bounds do, and every field here has one).
- **Carry the UDP fields now: channel status ids, destinations, source identity as an
  address.** Lost: fields nobody writes are fields nobody validates; the UDP slice bumps
  the version and adds them with their feature. The source identity string stays, since
  an image names where it came from under both transports.
- **Ignore trailing bytes, as Aeron does, so that a newer client can send a longer
  message to an older driver.** Lost: a newer client talking to an older driver is a
  version the file headers reject first; inside one version, a longer message is a bug
  or an attack, and ADR 0010 says reject it.
- **Make the timeouts and sizes configurable on this page.** Lost: a configuration key
  needs a `why_not_default` and a `doctor` check (`CLAUDE.md`, invariant 5), which are
  the driver feature's to argue, key by key; the page states the values the driver uses,
  and a key, when one is justified, changes the driver, not the protocol.

## Evidence

None measured: a scope and format decision on the control plane. What informed it: the
Aeron sources named in Context, read on 2026-10-12; the three layout pages' "the
driver's to specify" clauses; ADR 0018's Revisit; and the 35 vectors of
`spec/vectors/client-protocol.json`, which encode every command and event once, the
extremes of each integer, the empty and the longest string, and one vector per rejection
in the order the checks run.

## Consequences

Easier: the driver skeleton (card 86) and the client API read one page for what a
command means, what answers it and when a side is gone; a client in another language
follows the same table and the same vectors. Harder: a change to a command is a new
version of this page with an ADR, as for every layout. To maintain: the vectors script,
the Rust encoders in `cavalry-proto` that `just conformance` runs against it, and the
two counters.

Revisit: when the UDP slice adds channel status indicators and destination commands, as
version 1; when a client needs a counter of its own, which adds Aeron's add counter and
remove counter commands and their events; when a measurement shows a command on a
latency budget, which ADR 0014's Revisit says would move the ring onto the data-plane
rules; when a configuration key for a timeout or a size is justified, which changes the
driver's reference page, not this one.

## References

- `spec/client-protocol.md`, `spec/vectors/client-protocol.py`,
  `docs/reference/client-protocol.md`
- `spec/command-ring.md`, "Record" and "Reading"; `spec/broadcast-buffer.md`, "Record";
  `spec/log-buffer.md`, "The driver"; `spec/counters-file.md`
- ADR 0007 (the slice), ADR 0010 (bounds), ADR 0013 (who owns a position counter), ADR
  0014 (the rings), ADR 0018 (the late write and the validation this page states)
- Aeron, `aeron-client/src/main/java/io/aeron/command/ControlProtocolEvents.java` and
  the flyweights beside it, `io/aeron/ErrorCode.java`, `io/aeron/CommonContext.java`,
  `aeron-driver/src/main/java/io/aeron/driver/Configuration.java`, read 2026-10-12
