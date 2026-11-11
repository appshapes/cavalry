# 0084. Let the recorder trail a publisher it is keeping up with

- **Status:** Accepted
- **Date:** 2026-11-11
- **Invariants affected:** 1, 2, 3, 5
- **Supersedes:** none. Amends ADR 0025's recorder: when it reads, not what it writes

## Context

A recorded stream's publisher is about twice as slow as an unrecorded one on `bench`,
a Ryzen 7 2700X, and no slower on `grip`, a Core i7-10700 (card 252). On `bench` the
recorded row's claim reads 169 ns at p50 and 319 ns at p99.99 where the client row's
reads 79 and 159, and the stream carries 4.93 million messages a second where it
could carry over 8 million. The recorded rows are gate rows on both hosts.

The recorder takes blocks of committed frames from the publication's log
(`TermReader::block`, ADR 0025). The scan reads each frame's length word in turn and
stops at the first that is not committed. A recorder that keeps up with its publisher
is therefore reading, every duty cycle, the word of the frame the publisher is about
to write: the cache line the publisher needs next. On `bench` the recorder runs on the
housekeeping CPUs, which are the other of the part's two groups of four cores, each
with a third-level cache of its own, so each such read takes the line across. `grip`
has one third-level cache for all its cores.

The measurement that names this is in "Evidence": a recorder that spins exactly as
hard but reads a word further ahead costs the publisher less the further ahead it
reads, and nothing at 1 MiB.

Two things the recorder must go on doing. A stream that goes quiet is recorded to its
end without waiting for more of it. And the cluster's commit waits on a recording: a
member's append position is its recording's synced position, and a member that leaves
the log stops its recording and takes the stop position as where its log ends (ADR
0060, ADR 0069).

## Decision

After the recorder has written a block that ended at a publisher's tail, it does not
scan that recording again for 50 microseconds. A publisher that never pauses then
meets its recorder once in 50 µs and not once a message.

In full, for each live recording, each duty cycle:

1. If 50 µs or more have passed since the recorder last wrote a block of the
   recording that ended at the tail, it scans at its position as ADR 0025 has it, a
   block of at most 1 MiB. Otherwise it leaves the recording alone, and its idle
   strategy decides when it looks again.
2. Only a block that reached the tail starts the 50 µs: one whose scan ended at a
   frame that is not committed, which `TermReader::block` now reports
   (`Block::uncommitted`). A scan that found nothing committed starts none, so the
   first frame of a quiet stream is written on the next duty cycle, as it is today. A
   block that ended for its limit, at its term's end or at a padding frame starts
   none either: more may be committed, and the recorder scans again on its next duty
   cycle.
3. A `sync recording` and a `stop recording` are served to the tail. When the recorder
   takes the request it first writes what is committed at its position, block after
   block, until a scan finds no committed frame, four blocks or 1 MiB are written, a
   frame fails its check or a write fails; then it syncs or stops as before. The
   same is done for every live recording when the driver closes. Four blocks, since
   a block ends with its term and a padding frame is a block of its own; 1 MiB, since
   that is what one duty cycle writes of a recording.

So `recording.position` trails a publisher that does not pause by what the publisher
writes in 50 µs, and by what the recorder's idle strategy adds while it waits: under
the default, the backoff strategy, the recorder parks once the 50 µs find it with
nothing to do, and a park of a microsecond is some tens of microseconds on Linux. A
stop leaves no committed frame unwritten that the recorder could reach in four blocks
and 1 MiB, where before it left what one duty cycle had not reached.

The two numbers are constants in `cavalry-driver`'s `spec.rs`, `RECORDER_TAIL_INTERVAL`
and `RECORDER_REQUEST_BLOCKS`, with no configuration key (invariant 5). The recorder's
clock is the wall's, as every timeout of the driver's is today, so a difference below
zero, a clock set back, counts as the 50 µs passed. Card 266 asks which clock the
driver's timeouts should run on, and this interval follows what it decides.

## Alternatives considered

- **Look ahead in the term.** The recorder reads one word 16 KiB ahead of its
  position and takes a block only when something is written there, and only as far as
  that. This is the best measurement in "Evidence": the recorder never reads the
  publisher's line at all, and the recorded row reads as the client row does, claim
  p99.99 170 ns. Lost, for now. The word 16 KiB ahead is a frame's length word only
  when frames happen to divide the distance. Otherwise it is inside a message's body,
  which a publisher writes with plain stores before it commits the frame, and a read
  of it from another thread is outside the rule that makes the log buffer's reads
  sound (`cavalry_shm`'s `FrameCells`: a reader reads a frame after it has acquired
  the frame's positive length at the frame's start). It would also depend on the
  message: a body of zeros reads as nothing written.
- **Leave the recorder at the publisher's heels and say where to run it.** Lost. The
  cost is the line moving between two caches, and a recorder pinned into the
  publisher's group of cores would not pay it. But that is a core taken from the
  application on the part with the fewest to spare, it is a setting an operator must
  know to make (invariant 5), and it does nothing on a host with two sockets.
- **Read the publisher's position counter and stay behind it.** Lost. The publisher
  stores that counter each message too, so it is the same cost on another line.
- **A longer interval.** The tail still pays once an interval: at 8.5 million messages
  a second, one message in 425 at 50 µs. A p99.99 free of the recorder needs one in
  more than ten thousand, an interval over a millisecond, and a recording, a replay
  that follows it and a cluster member's append position would all be a millisecond
  late.
- **Hold an empty scan for the interval too.** Lost. Under the busy-spin and yield
  strategies the first frame of a quiet stream would wait up to 50 µs where it waits
  for one duty cycle today.
- **Serve a request to the tail however far.** Lost. Up to sixteen requests are taken
  in one duty cycle, before any other recording is visited, and a recorder far behind
  is behind a slow disk. What the interval holds back is 50 µs of writing, far inside
  1 MiB; a recorder further behind stops or syncs where it is, as it does today.

## Evidence

Measured by hand on `bench` and `grip` on 2026-11-11 between 18:52 and 19:11 UTC, with
no CI job on either host, from a scratch build of f4ea51b that was not committed. Rows
`claim_commit_32_recorded` and `one_way_32_recorded`, 5,000,000 messages, clock step
10 ns on `bench` and 1 ns on `grip`, five runs unless said; p50 / p99 / p99.99 in
nanoseconds of the median run by p99.99. The scratch build's recorder looked ahead in
the term, the alternative above: with 64-byte frames the word it read was always a
frame's.

On `bench`, `--pin 8,10,12,14`, the recorder scanning at its position, to the tail,
only once a frame is committed this far ahead, and spinning on that one word until
then:

| Ahead | claim | one-way | messages a second |
| --- | --- | --- | --- |
| today's recorder (21 runs) | 169 / 179 / 319 | 270 / 420 / 710 | 4.93 million |
| 0 bytes | 159 / 179 / 319 | 270 / 420 / 710 | 4.94 million |
| 64 bytes | 160 / 180 / 350 | 270 / 430 / 720 | 4.98 million |
| 1 KiB | 160 / 180 / 370 | 270 / 440 / 730 | 5.11 million |
| 2 KiB | 160 / 180 / 430 | 260 / 430 / 740 | 5.55 million |
| 4 KiB (21 runs) | 80 / 180 / 450 | 190 / 390 / 710 | 6.53 million |
| 8 KiB | 80 / 170 / 420 | 170 / 360 / 680 | 7.49 million |
| 16 KiB | 80 / 170 / 360 | 160 / 300 / 660 | 7.99 million |
| 64 KiB (21 runs) | 80 / 160 / 250 | 170 / 270 / 560 | 8.28 million |
| 1 MiB | 80 / 90 / 170 | 160 / 210 / 350 | 8.61 million |

The row at 0 bytes is the control: the same spin, on the word at the tail, costs what
today's recorder costs. So the cost is the read at the tail, and it falls with how
seldom the recorder reads there. The client row, with no recording, read 79 / 89 / 159
and 170 / 230 / 290 in CI's `bench (linux)` job at the same tree that day.

The same, with the recorder waiting by its idle strategy and not by spinning. These
are the nearest to the decision, which also scans to the tail and waits idle. At
`bench`'s rate the publisher writes 16 KiB in about 30 µs and 64 KiB in about 120 µs,
so the decision's 50 µs lies between the first two rows:

| Ahead | claim | one-way | messages a second |
| --- | --- | --- | --- |
| 16 KiB | 80 / 170 / 330 | 160 / 280 / 580 | 8.30 million |
| 64 KiB | 80 / 170 / 250 | 160 / 270 / 560 | 8.42 million |
| 16 KiB, and the block ends at the frame seen ahead | 80 / 90 / 170 | 160 / 210 / 380 | 8.56 million |

The last row is the alternative that lost, for what it would be worth if a sound way
to it is found.

On `grip`, `--pin 4,5,6,7`, seven runs: today's recorder 23 / 53 / 133 and
26 / 68 / 136 in two passes, at 14.6 and 14.5 million messages a second; trailing by
16 KiB, 21 / 60 / 133 at 15.2 million. About four percent there, and no cost.

`perf c2c` cannot name the lines on `bench`: the part has no memory events ("no PMU
supports the memory events"). That the cost is the line crossing between the two
groups of cores is read from the part's layout and the two hosts' difference, and was
not measured by moving the recorder.

The rule as decided was not run by hand. The pull request that makes the change
carries the recorded rows from CI's bench jobs on both hosts, before and after, and
those are its measurement.

The tests, in `crates/cavalry-driver/src/conductor.rs` on a clock the test sets:

- `a_recorder_that_read_to_the_tail_reads_there_again_after_its_interval`: a frame
  published to a recorder that had written nothing is written on its next duty
  cycle; a second, one nanosecond short of 50 µs later, is not; at 50 µs it is.
- `a_recorder_that_is_behind_its_publisher_is_not_held_by_the_interval`: with 1.5 MiB
  committed, a block of the limit and then the rest, a nanosecond apart; the frame
  after that waits.
- `a_block_that_ends_with_its_term_starts_no_interval`.
- `a_clock_set_back_does_not_hold_the_recorder`.
- `a_sync_and_a_stop_inside_the_interval_write_what_is_committed_first`: the sync's
  position and the stop's are the publisher's, and the recording holds every message.
- `what_a_request_writes_first_is_bounded_in_blocks`, tried with one block where two
  are waiting across a term's end, and `what_a_request_writes_first_is_bounded_in_bytes`,
  with 1.5 MiB waiting.

And in `crates/cavalry-shm/tests/log_buffer.rs`, the two block tests say which blocks
ended at a frame not committed: an empty scan and a block that stopped at a claim do;
one that stopped for its limit, before a padding frame, or is a padding frame does not.

The driver's 124 unit tests that were there before pass unchanged: their clock moves a
millisecond a duty cycle, so their recorder is always past its interval.

## Consequences

A recorded stream's publisher on a host like `bench` meets its recorder once in 50 µs
and not once a message. Its p50 and its rate come back. Its p99.99 does not: one
message in some hundreds still meets the recorder.

A publisher slower than a message in 50 µs gains nothing: the recorder has scanned
again before its next message, as today, and that message meets it. What that costs
is one line's crossing, about 100 ns a message on `bench`, and it is the cost every
reader of the stream on another group of cores has.

`recording.position` may trail a busy publisher by 50 µs of its writing where it
trailed by a duty cycle. A replay that follows a live recording, and a cluster
member's append position, see a frame that much later under the busy-spin and yield
strategies. Under the default, the backoff strategy already parks an idle recorder
for up to 1 ms, and nothing changes that.

The cluster's commit path is unchanged for a quiet log: an empty scan starts no
interval. Under a leader that appends without pause, an entry's write, and so the
sync the consensus module asks for when it sees the position move, comes up to 50 µs
later than today; the sync itself takes far longer. A module that asked for the sync
as it appended would not wait at all, since the request is served to the tail. That
is the module's to decide.

A `sync recording` now covers more than `spec/client-protocol.md` promised, and the
page says the new, larger thing: what was committed when the recorder took the
request, within four blocks and 1 MiB.

A stop does more before it answers: up to 1 MiB of writes. A caller that waits a
fixed time for the stop can see that time pass on a slow disk. The consensus module
waits one second for a recording's stop position (`STOP_TIMEOUT` in
`crates/cavalry-raft/src/module.rs`), returns `stop-timeout`, and stops the recording
again on its next duty cycle; a stop of a recording that has stopped succeeds with
nothing written, so the second try ends it.

A committed frame that fails its check is now reached by halving the block's limit,
and not one frame a duty cycle: the frames before it are written in a few blocks.

The sender scans a `cavalry:udp` publication's log the same way and on the same
housekeeping CPUs, and the udp rows on `bench` show the same shape (claim p99.99 299 ns
against the client row's 159). This decision does not touch the sender: its frames
must leave as they are committed. Revisit it there with a measurement of its own.

Revisit this decision for the tail: a recorder whose blocks end before the
publisher's tail, by a rule that reads nothing the publisher is writing, would reach
the last row of the second table. And revisit it if the cluster's bench, when it
exists, shows the 50 µs in its commit latency, or if a part with one cache for all
its cores shows a cost from the interval.

## References

- Card 252, and its notes of 2026-11-11.
- `spec/client-protocol.md`, "Commands": stop recording, sync recording.
- `crates/cavalry-driver/src/recorder.rs`, `Recorder::record`, `Live::take`,
  `Recorder::write_blocks`; `crates/cavalry-driver/src/spec.rs`,
  `RECORDER_TAIL_INTERVAL`, `RECORDER_REQUEST_BLOCKS`;
  `cavalry_shm::log::TermReader::block` and `Block::uncommitted`.
- ADR 0025 (the recorder), ADR 0060 and ADR 0069 (the cluster's commit and the sync on
  command), ADR 0010 (the checks a block scan makes), ADR 0011 (lines that move in
  pairs).
