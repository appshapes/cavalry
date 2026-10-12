# 0018. Commit with a compare-and-swap in the command ring

- **Status:** Accepted
- **Date:** 2026-10-12
- **Invariants affected:** 1, 3
- **Supersedes:** the commit and abort stores of ADR 0014's command ring; the rest of
  ADR 0014 stands.

## Context

`spec/command-ring.md` version 0 copied Agrona's `ManyToOneRingBuffer` (ADR 0014): a
producer claims a record with a release store of a negative length, writes its message,
and commits with a release store of the positive length. When a producer claims and
stalls past the unblock timeout, the client liveness timeout, the driver pads the claim,
reads past it and zeroes it. The review of pull request 7 found what version 0 left
unsaid, raised as issue 8: the stalled producer wakes and commits anyway, a release
store of a positive header into a slot that is now behind the head. One lap later the
consumer reaches that slot before any producer has claimed there, loads the stale
header, delivers a command nobody sent, and releases a head past the tail; from then on
every producer's claim reports `tail`. A ghost command is a delivery nobody asked for,
invariant 1, and a ring that stops for every client is one throttled client taking the
host down, which invariant 3 names as a condition the driver survives. Agrona has the
same hole and lives with it by the liveness timeout alone.

Rjae decided on 2026-10-12, on the issue, grounded in the two forces Cavalry serves in
Aeron's domains: no dropped messages and the highest throughput. The command ring is
control traffic, a handful of commands per client per second, so a swap on its commit
costs the data plane nothing; a ghost "remove subscription" or a ring every client has
lost is a path from a paused container to dropped messages.

## Decision

Every change to a record's word that another party may be racing is a compare-and-swap
from the value its writer last loaded, with release ordering on success and acquire on
failure; only the consumer's clearing of records it has read stays a plain store, since
no producer may claim there before the head is released. So: a producer claims by
swapping its index from 0 to the claim word, the negative length and the type; it
commits by swapping the claim word to the positive length and the type, and aborts by
swapping it to the positive length and type −1; before it writes its message it loads
the word with an acquire load; and the driver unblocks by swapping the word it loaded,
a stale claim or a 0 at the head of a gap, to the padding. A failed swap or a changed
word on the producer's side is `unblocked`: the driver padded the index, and the
consumer may have read and zeroed it or another producer claimed it since. The producer
writes nothing more, reports `unblocked` to its caller, whose command was not
delivered, and may claim again. A failed swap on the driver's side writes nothing: a
producer committed, aborted or claimed there in the same instant, and the next read or
unblock finds the new word. The command ring's version is 1, so that a version 0 party,
whose writes are stores, and a version 1 party refuse each other by the header rather
than meet in the race. The late write of message bytes between the producer's load and
its commit into a slot another producer has claimed since is bounded by the unblock
timeout, as in the design this copies: a producer stalled that long between two
adjacent stores has been timed out as a client, and the driver validates every command
it decodes.

## Alternatives considered

- **Keep Agrona's behaviour and document the timeout as the bound.** Lost: it leaves a
  delivery nobody asked for and a stopped ring on a timeout, which is the thing about
  the copied design that is hard to operate; the swap costs one atomic on a control
  path.
- **Swap only the commit and the abort, as the issue proposed.** Lost in the loom
  model: with the driver's padding still a store, one interleaving had the padding land
  on a record whose commit had succeeded an instant earlier, so the producer was told
  its command was delivered and the driver skipped it as padding; and a padding over a
  gap could land after a claim word that a producer's plain store then overwrote,
  which puts a positive header behind the head again on the next lap. The unblock and
  the claim are the other two writers of the word, and the same rule closes both.
- **A generation number in the record header, checked by the consumer.** Lost: the
  consumer cannot tell a late commit from a fresh one by the header's contents
  without a per-index epoch, which is a layout change and a larger header, and the swap
  closes the same two failures with nothing added to the record.
- **The consumer rejecting a positive header behind its head.** Lost: by the time the
  consumer reaches the slot again the head has lapped, and a positive header there is
  what a fresh commit looks like.
- **Closing the late message write too.** Lost for now: the message bytes are plain
  stores and no swap covers them; a producer that checks its claim immediately before
  its stores leaves a window of nanoseconds that only a stall of the timeout's length
  can hit, and the driver validates what it decodes. Revisit below.
- **Deferring until the driver exists.** Lost: the driver is the consumer, and the rule
  is cheaper to set before any consumer code exists than to migrate afterwards.

## Evidence

- The scenario of issue 8 walked through version 0's steps: claim at index 0 with a
  negative length, a stall past the unblock timeout, the driver's padding store, read
  and zeroing, then the producer's positive store at index 0 behind the head; on the
  next lap the consumer's step 1 loads a positive length at index 0 and delivers it.
  Under this ADR the producer's swap finds 0 or another word at index 0 and fails;
  nothing is stored.
- A loom model in `crates/cavalry-shm/src/command_ring/cells.rs` runs a producer's
  claim, check, message and commit against the consumer's unblock and read in every
  interleaving: the consumer delivers the record exactly when the producer's commit
  succeeded, a producer whose index or claim was padded sees `unblocked`, and the slot
  ends cleared. With only the commit and the abort swapped, the model failed with this
  trace: the unblock padded index 0, the read skipped 16 bytes with no message, and the
  word then read as a committed record of type 1 with its message in place, because
  the commit's swap had succeeded between the driver's load and its padding store. With
  the unblock swapping from the word it loaded, the model passes.
- Cost: one `compare_exchange` where there was one store, on a path that carries
  commands, not messages; the bench's message rows are unaffected by construction and
  the host's bench job on the implementing commit is the measurement.

## Consequences

Easier: a lost command is reported to its sender, which may retry, and one stalled
client cannot stop the ring for the others; the first place where Cavalry's ring is
measurably more correct than the design it copies.

Harder: `Claim::commit`, `Claim::abort` and the message write return a result the
caller must handle, and a dropped claim aborts without one; version 0 rings are
refused.

Revisit: when the client protocol page exists, to say how the driver validates a
decoded command, which is what bounds the late message write; when a per-index epoch
is wanted to close that write as well, with a layout change and a version.

## References

- `spec/command-ring.md` version 1: "Record", "Claim, commit and abort", "Unblocking",
  "What a reader rejects", "Vectors"; `spec/vectors/command-ring.py`, the `commit`
  vectors
- `crates/cavalry-shm/src/command_ring/`: `cells.rs` (`commit`, `abort`, `check_claim`,
  the loom model), `file.rs` (`Claim`), `mod.rs` (`CommandRingError::Unblocked`)
- GitHub issue 8; ADR 0010, ADR 0014
- Agrona `ManyToOneRingBuffer`:
  <https://github.com/aeron-io/agrona/blob/master/agrona/src/main/java/org/agrona/concurrent/ringbuffer/ManyToOneRingBuffer.java>
- AppShapes Trello card 84, "Make the command ring's commit a compare-and-swap"
