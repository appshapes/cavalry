# 0044. Gate UDP on the two bench hosts with one message in flight, on the loopback and across the link

- **Status:** Accepted
- **Date:** 2026-11-04
- **Invariants affected:** 2, 5, 6
- **Supersedes:** none. Settles the one decision ADR 0039 left to card 182, whether
  the cross-host round trip is held to `bench`'s baseline alone or to both hosts', and
  carries the Revisit clause card 182's comment asked for on ADR 0042's recording window

## Context

Card 182 is the fourth feature of the UDP slice (ADR 0039), its acceptance test: the
harness's UDP rows on each bench host, a cross-host row across the direct cable between
`bench` and `grip`, the UDP step in the README's ten-minute path, a how-to for a
publisher on one host and a subscriber on another, and `doctor`'s check for an
interface whose MTU is below the stream's. The three features before it are on
`master`: the transport's frames and rules (ADR 0040), the sender (ADR 0041), the
receiver (ADR 0042) and a replay over the sender (ADR 0043). Nothing has measured the
path yet; every commit under a data-plane crate since card 180 has quoted what the
existing rows show, as ADR 0039 said it would until this card.

The harness measures the shared-memory rows with a publisher that runs free: it claims
and commits as fast as the window allows, and the one-way row is the publisher's stamp
to the subscriber's handler. That works because the subscriber on its own core keeps up
with the publisher, so the gap between them stays at a few frames. Over UDP it does
not: the conductor keeps `publication.limit` at the sender position plus the publication
window, 8 MiB (`spec/udp.md`, "Values"), and the sender's own limit is the receiver's
consumption plus its 128 KiB window, so a publisher that runs free fills the 8 MiB and
every message then waits in the log for the sender to reach it. Measured on 2026-11-04
on an Apple silicon laptop with the row built that way: `one_way_32_udp` read 8.6 ms at
p50 and 11.3 ms at p99.99 at 10.7 million claims a second, the depth of the window at
the loopback's rate, and not the path at all.

The hosts have what ADR 0039 asked for. Each has four isolated cores, `bench` CPUs
8 to 15 with 8, 10, 12 and 14 online and `grip` CPUs 4 to 7, of which the gate's
jobs pin three. The cable moved from `kip` to `grip`'s `eno2` on 2026-10-31, and the
link's record is on card 182 as of 2026-11-02: `bench` `enp5s0` 10.0.0.1/30 and `grip`
`eno2` 10.0.0.2/30, MTU 1500, `bench-link` and `grip-link` in both hosts' `/etc/hosts`,
100 of 100 pings each way at 0.12 ms on average and 0.21 ms at most, hardware
timestamps on both ports, `bench` serving the clock and `grip` following it with a
standard deviation of 20 ns in `chronyc sourcestats`, `ufw` inactive on both. One
note from that check matters here: `grip`'s `chronyc tracking` named a public NTP
server as its reference and `bench-link` as combined, not selected, so the offset
between the two hosts is the `bench-link` line of `chronyc sourcestats`, not
`tracking`'s system offset. A one-way number across two hosts carries that offset; a
round trip is measured on one clock (ADR 0039).

Two decisions were left to this card. ADR 0039: whether the cross-host round trip is
held to `bench`'s baseline alone or to both hosts'. Card 182's comment of 2026-11-02,
decided with Rjae from pull request 99's review: the window between `start recording`
on a `cavalry:udp` image and the receiver's handling of its reader, one receiver cycle,
during which the recorder's floor is not yet one of the image's readers, goes in this
ADR's Revisit clause, since ADR 0042 is immutable; no `doctor` check, since one would
fire for the stall that is the only way the window matters.

Aeron's benchmarks measure latency by ping-pong: `EmbeddedPingPong` and the `Ping` and
`Pong` samples send one message and wait for its echo, and its benchmark harness paces
sends at a fixed rate rather than letting a publisher run free (read 2026-11-04 at
`master`, `aeron-samples`).

## Decision

The harness measures UDP with one message in flight. The publisher claims message i
once the subscriber's handler has had message i − 1, so that a sample is the path's
latency and never the depth of a window; the same rule on the loopback and across the
link. Four rows and one peer:

1. **The loopback rows**, `claim_commit_<body>_udp` and `one_way_<body>_udp`: a
   `cavalry:udp` publication and a `cavalry:udp` subscription on one port of
   `127.0.0.1` through a real driver in the harness's process, the client rows' path
   through the driver's sender and receiver. They run in every pass of `just bench`,
   on the shared runners as a report and on both bench hosts gated against each host's
   baselines, with a tenth of `--messages` per run after a tenth of a rotation's
   warm-up: a message in flight is ten microseconds here against a tenth of one for the
   client rows, and a tenth of the messages measures the same percentile in the same
   time. `--pin` takes a fourth CPU, `P,S,C,R`: the publisher and the subscriber on
   the first two as before, the sender on the third and the receiver on the fourth,
   and the driver's conductor with the recorder, the replayer and the client's
   conductor on the housekeeping CPU, as the recorder runs for the recorded rows; three
   CPUs refuse a UDP row, since a receiver on a measured thread's CPU or with the
   housekeeping threads gives a number the baseline cannot be compared with. The
   driver's threads busy-spin under `--pin`, where each has a core, which is what
   `doctor` recommends on isolated cores, and back off otherwise, as a driver on a
   laptop does; the client rows keep backoff with the conductor pinned, as their
   baselines were recorded. The bench jobs pin `8,10,12,14` on `bench` and `4,5,6,7`
   on `grip` and run `check.sh` on the same four.
2. **The link rows**, `udp_round_trip_<body>` and `udp_one_way_<body>`: the harness
   on `bench` with `--link-here bench-link:40182 --link-peer grip-link:40183`
   subscribes on its own end of the cable and publishes to the other's, and a peer
   mode of the harness on `grip`, `--echo` with the ends reversed, claims every
   message it receives on a publication back to `bench`, with `grip`'s wall clock
   written into the message, until the stream ends. The round trip is `bench`'s
   stamp to `bench`'s handler, one clock's, and is **gated against `bench`'s baseline
   alone**, the profile with `-link` appended, `bench-linux-link`: both hosts' sender,
   receiver, socket path and port are in every sample, so the pair run once holds
   both, and a second pair with the roles swapped would cost the pair's minutes on
   every push for a number the first already carries. The one-way is `bench`'s
   wall-clock stamp to `grip`'s at its handler, carries the offset between the two
   clocks, and is reported beside the round trip and never gated, with the offset
   read from the `bench-link` line of `chronyc sourcestats` on `grip`, which the peer
   job prints after the echo, since `tracking` names whatever source `grip` selected
   at the moment; a one-way sample below zero, `grip`'s clock behind `bench`'s by
   more than the path, is recorded as 0 and counted under the table. The rows run
   alone, with `--messages` as given per run, 100,000 times 21 runs in CI at about
   100 µs a message, and each side waits up to ten minutes for the other, so the order
   of the two commands does not matter. They are the pair of jobs ADR 0039 named,
   `bench (udp)` on `bench` and `bench (udp peer)` on `grip`, started together once
   both single-host bench jobs are done, so that neither host runs a bench while the
   other measures the link, under the same guard that keeps a stranger's code off the
   hosts. The gate's rule reads `udp_round_trip_` as a message row and `udp_one_way_`
   as a report. `bench (udp)` joins `master`'s required checks by hand once it has run
   green on `master` with its baseline, as `bench (grip)` did, since a required check
   that has never run blocks every merge.
3. **The ten-minute path** gains the UDP step inside the program it already runs: after
   the recording stops, `quickstart` publishes the same 1,000 messages again on
   `cavalry:udp?endpoint=127.0.0.1:40123`, Aeron's samples' port, on the same stream
   through the same client and reads them back once and in order through a
   subscription on that channel, every message through the driver's sender and
   receiver, and prints one more line; `scripts/quickstart.sh` checks the line on
   Linux and macOS, the README's six commands stay six, and the tutorial says what the
   reader sees. The README's status paragraph says the third slice works.
4. **`doctor`** reports `udp-interface-mtu` when an endpoint's address routes over an
   interface whose MTU is below the stream's datagram, 1408 bytes, plus its IP and UDP
   headers, 1436 for IPv4: on Linux from `/proc/net/route` and the interface's `mtu`
   file, with no key to set, since the stream's MTU is fixed in this version
   (`spec/udp.md`, "Values"); elsewhere there is nothing to read, as for the other host
   checks. The how-to `docs/how-to/publish-between-two-hosts.md` names it beside
   `udp-publication-not-connected` and `udp-subscription-no-image`, and the set-up
   page's "Further hosts" is rewritten for `bench` and `grip` with the link's record.

## Alternatives considered

- **A publisher that runs free, as the shared-memory rows.** Lost on the measurement
  in Context: 8.6 ms at p50 is the window's depth, a number that moves with the
  publication window and the loopback's throughput and says nothing about the sender's
  or the receiver's duty cycle.
- **Pacing at a fixed rate, Aeron's benchmark harness.** Lost for now: a rate is a
  number someone chooses, and at any rate below the path's a sample is the same thing
  one in flight measures, while at a rate above it the queue returns; one in flight
  needs no number and is Aeron's own ping-pong. A rate returns with a throughput row,
  which is a different question and a Revisit clause.
- **The round trip held to both hosts' baselines, the pair run twice with the roles
  swapped.** Lost: one clock, one profile; both hosts are in every sample, and a
  second pair doubles the minutes on every push for the same path.
- **The link rows inside the single-host bench jobs.** Lost: the pair must meet, and a
  bench on one host while the other measures the link would disturb both; a pair of
  jobs after the two is one rendezvous with both hosts idle.
- **The peer as a binary of its own.** Lost: it needs the same driver, the same pins
  and the same placement of the sender and the receiver, which the harness has; a
  mode of the harness is one set of rules.
- **Busy-spin for the driver's threads on every run.** Lost: unpinned on a laptop or a
  shared runner, the publisher, the subscriber, the sender, the receiver and the
  conductor are five spinning threads, and on a four-core runner they starve each
  other; the rule follows `--pin`, where each has a core.
- **Backoff for the driver's threads on every run.** Lost on the measurement below:
  the sender and the receiver yield between messages, and the one-way row's p99.99
  read three to four times busy-spin's on the laptop.
- **The one-way row's offset from `chronyc tracking`.** Lost: at the check of
  2026-11-02 `tracking` named a public NTP server, with `bench-link` combined and not
  selected, so its system offset was not the offset between the two hosts;
  `sourcestats` gives the offset against `bench-link` whatever is selected. Making
  `bench-link` the selected source on `grip`, `noselect` on the pool lines or a local
  stratum on `bench`, is a host change and a Revisit clause.
- **A second example program for the UDP step, and a seventh command.** Lost: the
  step is the same API on another channel, which is the slice's promise, and a reader
  sees it in the program they already ran; the README's six commands stay six.
- **An ephemeral port in the ten-minute path.** Lost: a reader copies a channel from
  the page, and `127.0.0.1:40123` is one they can type; a port in use refuses the
  subscription with error code 4 and the message names the port.
- **The `-unchecked` twin of the link rows in CI.** Lost for now: the pair's minutes
  on every push, for a difference the loopback rows measure on both hosts under both
  profiles; a Revisit clause.

## Evidence

Laptop, 2026-11-04: Apple silicon, macOS, 18 cores, unpinned, clock step 41 ns, the
harness at this change. The loopback rows with one message in flight, 100,000
messages after 78,643 warm-up, p50 / p99 / p99.99 / max in µs:

| Driver's idle strategy | `one_way_32_udp` | `claim_commit_32_udp` |
| --- | --- | --- |
| backoff, run 1 | 10.7 / 34.8 / 202.8 / 221.7 | 0 / 0.04 / 6.3 / 67.7 |
| backoff, run 2 | 10.6 / 33.6 / 202.0 / 336.4 | 0 / 0.04 / 7.1 / 91.2 |
| busy-spin, run 1 | 9.0 / 21.6 / 50.0 / 149.9 | 0 / 0.04 / 9.4 / 81.2 |
| busy-spin, run 2 | 9.2 / 22.2 / 68.7 / 182.7 | 0 / 0.04 / 9.6 / 100.0 |

The same row with the publisher running free, backoff: `one_way_32_udp` 8,634 / 11,231
/ 11,256 / 11,256 µs at 10.7 million claims a second, the window's depth. The link
rows on the laptop's loopback, two drivers and two clients in two processes, 20,000
messages a run after 78,643 warm-up, two runs: `udp_round_trip_32` 56.1 / 100.7 /
178.0 / 256.3 µs, `udp_one_way_32` 28.0 / 58.0 / 112.1 / 129.0 µs on the one clock, so
half the round trip, and the peer echoed 197,286 messages and ended with the stream.
Zero allocations in every measured loop. The bench hosts' numbers are the first runs
of the jobs at this change, quoted on card 182 and in the commit that records the
rows' baselines.

## Consequences

Easier: a UDP change meets the gate on both hosts and across the cable on every push,
with a number that is the path's; a reader of the ten-minute path sees the same program
publish over shared memory and over UDP; `doctor` names an interface that would
fragment the stream before the stream is tried.

Harder: the single-host bench jobs grow by the UDP rows, about a minute a profile at
ten microseconds a message; a pair of jobs that must meet, and a stall on either host
holds `master`, as a stall on one does today; every isolated core of both hosts is
taken, so a fifth measured thread needs a host change; `--pin` with three CPUs refuses
a UDP row, so a person's run on a three-core host passes `--only`; the ten-minute path
binds a fixed port; the link job's baseline is `bench`'s own and `grip`'s part of the
path is not held on its own.

Revisit: when a throughput row is wanted, for pacing at a rate beside one in flight;
when the link rows' tail shows the interrupt path, the NIC's interrupt on a
housekeeping CPU and the receiver on an isolated one, for `SO_BUSY_POLL` or an
interrupt bound to the receiver's core, a host change under `bench/host/` that ADR
0039 names; when the link rows have run for a week, for their `-unchecked` twin and
for the count per run, 100,000; when `grip`'s selected clock source matters, for
`noselect` on its pool lines or a local stratum on `bench`, so that `tracking` reads
the link; and the window ADR 0042 left: between `start recording` on a `cavalry:udp`
image and the receiver's handling of the recording's reader, one receiver cycle, the
recorder's floor is not yet one of the image's readers, and cleaning runs a term behind
the slowest subscriber, so the window matters only when the receiver handles no request
for a full term of traffic, which is a conductor that stopped taking replies for that
long, a stalled driver and not a working one; no `doctor` check, since one would fire
for the stall; revisit if a stalled driver's recording is ever found short by a term.

## References

- ADR 0016, ADR 0017, ADR 0029, ADR 0033, ADR 0034, ADR 0039, ADR 0041, ADR 0042
- `bench/src/main.rs`; `bench/README.md`; `scripts/bench-compare.py`;
  `.github/workflows/ci.yml`, the `bench`, `bench-grip`, `bench-udp` and
  `bench-udp-peer` jobs
- `docs/how-to/run-the-bench.md`; `docs/how-to/set-up-a-bench-host.md`, "Further
  hosts"; `docs/how-to/publish-between-two-hosts.md`; `docs/tutorials/publish-and-subscribe.md`;
  `crates/cavalry/examples/quickstart.rs`; `scripts/quickstart.sh`;
  `crates/cavalry-control/src/doctor.rs`
- `spec/udp.md`, "The sender", "The receiver", "Values"
- Aeron, read 2026-11-04 at `master`: `aeron-samples/src/main/java/io/aeron/samples/{EmbeddedPingPong,Ping,Pong}.java`
- AppShapes Trello card 182, its comments of 2026-10-31 and 2026-11-02; card 181's
  pull request 99
