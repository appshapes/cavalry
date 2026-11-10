# 0062. Add a client's counter with Aeron's counter commands, as client protocol version 7

- **Status:** Accepted
- **Date:** 2026-11-10
- **Invariants affected:** 3, 4, 6
- **Supersedes:** none. Takes `spec/client-protocol.md` to version 7; adds the `client`
  scope to `spec/counters-file.md`, whose layout and version do not change. ADR 0019's
  Revisit named this change, and this ADR is the one it asked for

## Context

Card 227 builds the replicated log (ADR 0052, feature 3). Its consensus module is
`cavalry-raft`, and it reaches the driver as every program does, as a client: it adds
the log publication, the consensus publications and the subscriptions, and arms the
recordings, through the commands of `spec/client-protocol.md`. In Aeron, the consensus
module is a client of the media driver in the same way. ADR 0052 asks that module for
counters: the commit position "published as a counter", "the crate's counters and a
`[[state_machine]]` for the member's role", and `doctor` "explaining a stalled commit
from the append positions". Invariant 4 asks the same of every state transition.

A client cannot do that today. The driver is the only writer of a metadata record
(`spec/counters-file.md`, "Metadata record"), and no command asks it to write one for a
client. `spec/counters-file.md` already says whose a value may be: "a counter's value
is written by the counter's owner, the driver or the client the driver allocated it
for". The one counter a client owns today is its subscriber position, which the
driver allocates for an image. ADR 0019 left the rest to a later version: "when a
client needs a counter of its own, which adds Aeron's add counter and remove counter
commands and their events". ADR 0046 and ADR 0051 repeat the clause for the clients
and the C ABI. `spec/client-protocol.md` reserves the numbers: command type ids 9 and
10, event type ids 3848 and 3849, and error code 5.

What Aeron does, read on 2026-11-10 at `master`, commit a62440f.
`ControlProtocolEvents`: `ADD_COUNTER` is `0x09`, `REMOVE_COUNTER` is `0x0A`,
`ON_COUNTER_READY` is `0x0F08` and `ON_UNAVAILABLE_COUNTER` is `0x0F09`.
`CounterMessageFlyweight`: the client id, the correlation id, the counter type id as
an i32, the key length and the key's bytes aligned to four, the label length and the
label. `RemoveCounterFlyweight`: the client id, the correlation id and the counter's
registration id. `CounterUpdateFlyweight`, for both events: a correlation id and the
counter id as an i32. `DriverConductor.onAddCounter` allocates the counter with the
type id, key and label the client gave, whatever they are, links it to the client and
sends counter ready. `onRemoveCounter` answers a registration it does not hold with
`UNKNOWN_COUNTER`, error code 5, and otherwise sends operation success, then
unavailable counter, then frees the counter. `CounterLink.onTimeEvent` sends
unavailable counter for each counter of a client that timed out, and frees it. Aeron
does not check that the client that removes a counter is the one that added it.

The counters file has room. `spec/client-protocol.md`, "Sizes", sizes it so that the
registries fill before it does: they come to 15,899 of its 16,384 records.

## Decision

Client protocol version 7 adds two commands and two events under Aeron's numbers and
with Aeron's fields, and takes Aeron's error code 5.

`add counter`, type id 9: the client id, the correlation id, the counter type id, the
key and the label. The driver registers the counter under the command's correlation
id, its registration id; allocates a record of the counters file with the type id,
the key and the label; and answers `counter ready`, type id 3848, with the
correlation id and the counter id. The client is then the counter's owner and the one
writer of its value, as it is of a subscriber position. `remove counter`, type id 10,
names the registration: the driver answers `operation success`, sends `unavailable
counter`, type id 3849, with the registration id and the counter id, to every client,
and frees the record, which is reused after the counter reuse timeout as any record
is. A registration the driver does not hold, or another client's, is error code 5,
`unknown counter`. A client that closes or times out loses its counters as it loses
its publications: each is freed, with its `unavailable counter`.

Three things are Cavalry's own, each for an invariant.

**The type id is one `counters.toml` gives a client.** `counters.toml` gains a third
scope, `client`: one counter per key, allocated when a client adds it. The driver
refuses an `add counter` whose type id is not a `client` entry of its `counters.toml`
with error code 11 and a message that names the id and the reference page. Every
counter in the file is then one that `counters.toml` lists and that
`docs/reference/counters.md` and `cavalry doctor` can name (invariant 4). The first
entries are the consensus module's, added with the code that writes them. A type id
of an application's own is the Revisit clause below.

**The client's counters are bounded.** A registry of 256 client counters, for the
driver and not per client; the 257th `add counter` is error code 10 and adds one to
`driver.registry_full`. With it the registries come to 16,155 records, fewer than
16,384, so the registries still fill before the counters file does (invariant 3). The
key is at most 112 bytes and the label at most 124, the record's own bounds, and a
longer one is refused when the command is decoded, with error code 7, before anything
is registered.

**Only its owner removes a counter.** A `remove counter` that names another client's
registration is answered as an unknown one, as `remove publication` and `remove
subscription` are.

The driver does not compare a counter with those it holds: two adds of one type id and
key are two counters, as in Aeron. It writes the label the client gave and checks only
its length and that it is UTF-8; `counters.toml`'s rule for an instance counter's
label, the name, a space and the key's text, is the client's to follow.

`spec/counters-file.md` keeps version 0. No byte of the file moves, the driver still
writes every metadata record, and a type id is still the `id` of a counter in
`counters.toml`; the page gains the `client` scope and says what a client's key and
label are.

The client API gains `Cavalry::add_counter`, which takes the type id, the key and the
label and returns a `Counter` that stores and loads its value and removes itself when
closed. The C ABI does not take the commands in this version: no ABI function sends
them, so no ABI caller meets error code 5, and the header names the code when a
version of the ABI adds counters (ADR 0051's Revisit).

The work lands in three changes, each passing the gates on its own: this ADR with the
page, the vectors and `cavalry-proto`'s encodings, the driver answering the two
commands with error code 11 until the next change; the `client` scope in
`counters.toml`, its generator and the driver's registry; and the client API.

## Alternatives considered

- **The driver allocates the cluster's counters itself when it starts as a member, and
  the consensus module writes them by type id.** No protocol change. Lost: the driver
  would hold the list of what another crate counts, and a counter would outlive the
  module that writes it, so `doctor` would read a commit position that nothing is
  moving as a stalled commit. With `add counter`, a counter is there while its owner
  is, and the three earlier ADRs that foresaw this need all named the commands.
- **Any type id, as Aeron.** Lost for this version: a counter `counters.toml` does not
  list is one the reference page and `doctor` cannot name, and no user has asked for
  one, so the rule that every counter is generated from one file holds a version
  longer. Lifting it later refuses nothing that this version accepts.
- **Cavalry's own type id range for the two commands, from 264.** Lost: the page keeps
  Aeron's numbers for Aeron's commands, as version 5 took 7 and 8, and reserved 9 and
  10 for these.
- **The key and the label left out, the driver deriving both from `counters.toml`.**
  Lost: the key is the instance, a member id for the cluster's counters, and only the
  client knows it; and Aeron's layout carries both.
- **No `unavailable counter`, the owner alone learning of a removal.** Lost: a
  service reads the commit position its member's consensus module owns, and it must
  stop reading a counter id before the record is reused. The event is how any reader
  of another client's counter learns that.
- **`unavailable counter` before `operation success`.** Lost: Aeron sends operation
  success first, and nothing in Cavalry needs the other order.
- **A registry per client, or no registry.** Lost: the counters file is one bound for
  all clients, and "Sizes" must add up under it; one registry of 256 does.
- **`spec/counters-file.md` to version 1.** Lost: a reader rejects a version it does
  not know, so every client and every `doctor` of version 0 would refuse a file in
  which no byte had changed.
- **The commands in the C ABI now.** Lost: a service does not add counters, and the
  ABI moves to its next version once, with the bounded poll card 227 adds later.

## Evidence

None measured: a protocol decision. What informed it: ADR 0019's, ADR 0046's and ADR
0051's Revisit clauses; ADR 0052's feature 3 and ADR 0058's cluster protocol;
`spec/client-protocol.md` version 6, its reserved ids and codes and its "Sizes";
`spec/counters-file.md`; and Aeron's `ControlProtocolEvents`, `ErrorCode`,
`CounterMessageFlyweight`, `CounterUpdateFlyweight`, `RemoveCounterFlyweight`,
`ClientCommandAdapter`, `DriverConductor` and `CounterLink`, read on 2026-11-10 at
commit a62440f. The numbers in card 227's hand-over notes, 12 and 13 for the two
commands, were wrong: 12 and 13 are Aeron's receive destination commands, and the
source gives 9 and 10. What the implementation proves goes in the commit messages:
the vectors decoded and encoded by `cavalry-proto`, the registry at capacity, and a
counter freed at its owner's close and timeout.

## Consequences

Easier: the consensus module counts what it does in the file `doctor` reads, with no
code of the cluster's in the driver; a counter is there exactly while its owner is; a
reader of another client's counter is told when to stop; the next client that needs a
counter needs an entry in `counters.toml` and nothing else.

Harder: a client of version 6 closes on the first `counter ready` or `unavailable
counter` a driver of version 7 sends, as the reserved-id rule says, so an upgrade
restarts the clients before the driver, as every version that added an event did; the
driver holds one more registry and frees it at a client's close and timeout; a client
that the driver timed out while it was only slow may store into a record that was
freed, the hazard a subscriber position already has, bounded the same way by the
counter reuse timeout and by the `client timeout` event; an application cannot yet
add a counter of its own.

Revisit: when a user needs a counter of an application's own, for a range of type
ids that `counters.toml` never assigns; when a reader must tell that a counter id was
reused under it, for Aeron's registration id and owner id in the value slot, which is
a new version of `spec/counters-file.md`; when the C ABI's next version is cut, for
the two functions and the name of error code 5; when the registry of 256 fills, for
its size against the counters file's.

## References

- `spec/client-protocol.md`, version 7: "Commands", "Events", "Error codes", "Sizes",
  "What the driver rejects"; `spec/counters-file.md`, "Metadata record" and
  "`counters.toml`"
- ADR 0012, ADR 0019, ADR 0046, ADR 0051, ADR 0052, ADR 0053, ADR 0058
- Aeron at commit a62440f, read 2026-11-10:
  `aeron-client/src/main/java/io/aeron/command/ControlProtocolEvents.java`,
  `CounterMessageFlyweight.java`, `CounterUpdateFlyweight.java` and
  `RemoveCounterFlyweight.java`; `aeron-client/src/main/java/io/aeron/ErrorCode.java`;
  `aeron-driver/src/main/java/io/aeron/driver/DriverConductor.java`,
  `ClientCommandAdapter.java` and `CounterLink.java`
  (<https://github.com/aeron-io/aeron>)
- AppShapes Trello card 227
