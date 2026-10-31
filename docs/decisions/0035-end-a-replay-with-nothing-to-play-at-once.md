# 0035. End a replay with nothing to play at once, without a segment file

- **Status:** Accepted
- **Date:** 2026-10-31
- **Invariants affected:** 1, 6
- **Supersedes:** none; amends ADR 0030's start of a replay in one case

## Context

The recorder keeps no segment file for a recording that stopped at its start position:
`stop` removes the file it never wrote to, and recovery reads a recording with no file as
an empty one, not as damage (`spec/durable-log.md`, "The driver"; ADR 0025). The
quickstart program on the Linux runner stopped its recording before the recorder had
written a block (pull request 67, run 40946433350, 2026-10-29), so the recording stopped
at 0 and its file went. `cavalry replay --recording 0 --from 0` was then refused with
error code 12, storage space: `cannot open the segment file .../0-0.rec of recording 0
at position 0: No such file or directory`. The replayer opened the segment file at the
start position before it asked whether there was a byte to read.

An empty recording replays as an empty replay. Code 12 says the log directory lost a
file and sends the operator to a backup, which is not true, and the replay's subscriber
never learns that the stream it asked for is complete and holds nothing. Invariant 1
says the absence is reported, not misreported.

## Decision

A replay of a stopped recording whose position is its end position or the recording's
extent has nothing to play. The replayer opens no segment file for it, answers `replay
ready` as for any replay, and on its first visit once the publication exists ends it
with reason 0 at that position, so the subscriber gets the publication, its end of
stream and `replay stopped`, and `cavalry replay` prints `0 messages`. A replay of a live
recording from its extent keeps waiting for bytes, and its file exists. A replay with
bytes to play opens its file at the start as before, so a missing file is still code 12
at the start and not reason 3 after it. The spec's `start replay` and replayer
paragraphs say so.

## Alternatives considered

- **The conductor ends the replay itself, without handing it to the replayer.** Lost:
  the conductor never touches the disk or the catalog (ADR 0030), so it cannot know
  the stop position without the replayer's slot read, and the replay's publication and
  events would take a second path through the conductor for one case.
- **Keep the empty segment file at stop.** Lost: it changes the on-disk format and its
  recovery rule (ADR 0025) for a reader's convenience, and the first segment of a
  recording that stopped at a later segment base would still be the file the replayer
  opened for nothing.
- **Open the segment file lazily for every replay, at the first block.** Lost: a missing
  file would then end a replay with reason 3 after `replay ready`, where today it is
  refused at the start with code 12 and a message that names the file; the spec's early
  answer is the better one, and only the empty case has no file to find.

## Evidence

- `mod replay` in `crates/cavalry-driver/src/conductor.rs`, the empty case before the
  full one: a recording stopped at 0 with its file gone; the replay is `replay ready`
  with start position 0, `replay stopped` at 0 with reason 0, no error event,
  `replay.start_failed` 0, `replay.ended` 1, nothing in the replay log, end of stream 0.
- The quickstart failure, run 40946433350 of pull request 67.
- Histograms before and after the change, on the laptop, are in the commit; the hosts'
  jobs compare the pull request against their baselines.

## Consequences

Easier: `just quickstart` cannot fail on a recording the recorder had not reached, and a
replay's subscriber can trust `replay stopped` to mean the end of what there was.
Harder: the replayer's segment file is now optional in its state, `None` for a replay
that reads nothing, and every read goes through that option; nothing else changes.
Revisit: when ADR 0031's truncate lands, since a recording truncated to its start
position is this case again and must replay the same way.

## References

- `crates/cavalry-driver/src/replayer.rs`, `start` and `play`;
  `crates/cavalry-driver/src/conductor.rs`, `mod replay`
- `spec/client-protocol.md`, "start replay" and the replayer paragraphs;
  `spec/durable-log.md`, "The driver"
- `docs/how-to/replay-a-recording.md`; ADR 0025, ADR 0030, ADR 0031; card 165
