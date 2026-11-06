# 0056. Keep an ended receiver in a closed publication's `min` table until the linger ends

- **Status:** Accepted
- **Date:** 2026-11-06
- **Invariants affected:** 4, 6
- **Supersedes:** ADR 0054 on one point: an entry whose last status message carried
  the end-of-stream flag is no longer dropped on the next duty cycle once the
  publication is closed. The rest of ADR 0054 stands: the control channel's linger,
  no entry added for the flag, and `sender.flow_control_receivers_timed_out` counting
  the timeout alone. Amends `spec/udp.md`, "The sender", within version 1. Nothing on
  the wire changes

## Context

Under `fc=min` the sender keeps a table of receivers for each publication. The gauge
`sender.flow_control_receivers` is the table's size (`spec/udp.md`, "The sender";
ADR 0053). `cavalry doctor`'s `udp-flow-control-receivers` check warns when a `min`
publication's gauge is below its endpoint's `channel.destinations`. It tells the
operator that a destination stopped answering, and to check that destination's driver
or remove the destination.

ADR 0054 made two changes. A control channel's closed publication lingers for the
full linger timeout, 5 s. An entry whose last status message carried the end-of-stream
flag is dropped on the next duty cycle and not counted. Every receiver sets that flag
once its image has ended, which happens early in the linger after every clean close
(`spec/udp.md`, "The receiver", step 5). The sweep then dropped each receiver, and the
gauge fell to 0 while `channel.destinations` kept its value. The conductor frees the
gauge only when the linger ends. So for the rest of the linger, up to 5 s, `doctor`
warned that the closed publication's destinations had stopped answering. Nothing was
wrong. Just after a publisher stops is also a likely time to run `cavalry doctor`.
Before ADR 0054, the first status message with the flag ended the linger in the same
cycle, so the window lasted one duty cycle. The second review of pull request 152 on
2026-11-06 found this.

`doctor` reads one sample of the counters file. Today no counter in that file tells a
lingering publication apart from a live one. Every counter of a `cavalry:udp`
publication stays allocated until the linger ends (`close_if_done` in
`crates/cavalry-driver/src/conductor.rs`).

## Decision

Once a publication is closed, an entry whose last status message carried the
end-of-stream flag stays in its `min` table until the linger ends. It is dropped
neither for the flag nor for the flow control receiver timeout, and it is never
counted in `sender.flow_control_receivers_timed_out`. The table, and the gauge, go
with the publication when the linger ends. So through the linger the gauge counts
every receiver that took the stream to its end, and a clean close leaves it at the
endpoint's destinations. A receiver that times out during the linger is still
dropped and counted, and the gauge falls, because that is a real fault. On a
publication that is not closed, an entry with the flag is dropped on the next duty
cycle, uncounted, as ADR 0054 says. The kept entry holds nothing back. A receiver
ends its stream only after the sender's heartbeat with EOS. That heartbeat goes only
once the sender position has reached the end of stream position, so the publication
has no data left to send. The retransmits that NAKs ask for after that meet no
sender limit. The gauge's line in `counters.toml` says so. `spec/udp.md`, "The
sender", says so within version 1.

## Alternatives considered

- **Stop updating the gauge once the sender has taken the close, so that it holds
  the count from the close.** This option lost. A publication closed before its
  receivers' first status messages would hold 0 for the whole linger, and `doctor`
  would warn even though the receivers set up and take the whole stream during the
  drain. A frozen gauge also hides a receiver that times out during the linger.
- **Allocate a counter that marks the publication closed, and have `doctor` skip
  closed publications.** This option lost. It adds a sixth counter record to every
  `cavalry:udp` publication, and the counters file is bounded. It hides a fault
  during the linger as well: a destination that never answered the publication is
  then reported by nothing. The table already holds the fact that the gauge needs.
- **Store a value outside the count in the gauge on close, such as -1.** This option
  lost. A gauge that mixes a count and a state gives a wrong sum to anything that adds
  gauges, OpenTelemetry export among them.
- **Keep ADR 0054's drop and count ended receivers apart, the gauge reading the table
  plus that count.** This option lost. The count needs a bound of its own, and the
  gauge would no longer be the table's size, which `spec/udp.md` names it. Keeping
  the entry gives both from the table, which is bounded at 64 already.
- **Let `doctor` hedge, saying to wait 5 s if a publication was just closed.** This
  option lost. A finding that may be wrong is not an explanation from counters
  (invariant 4), and the second review rejected it.

## Evidence

None measured. This rule changes when the sender drops an entry from a table that
is already bounded. `a_clean_close_of_a_min_publication_counts_no_receiver_timed_out`
in `crates/cavalry-driver/src/conductor.rs` covers the clean close. A control
channel's `min` publication has two destinations. Both receivers answer, and the
publication is closed. Each receiver sends its five status messages with the flag.
The test then checks, 1 ns before the linger ends, the counters that `doctor` reads:
the gauge at 2, `channel.destinations` at 2, the sender limit above 0, and
`sender.flow_control_receivers_timed_out` at 0. At the timeout the publication
closes, and `publication.disconnected` stays at 0. With the sweep's new branch taken
out, the test fails: the gauge reads 0. `min_flow_control_follows_the_slowest_receiver_and_drops_one_that_times_out`
still drops an ended receiver of a live publication on the next cycle, unchanged.
In `crates/cavalry-control/src/doctor.rs`,
`a_min_publication_lingering_after_a_clean_close_leaves_doctor_with_no_warning` gives
those counters to `doctor`, and `doctor` gives no `udp-` warning. A live `min`
publication beside it with one of its two destinations answering still gets the one
`udp-flow-control-receivers` warning.

## Consequences

Easier: from the moment a publisher closes, `cavalry doctor` gives no
`udp-flow-control-receivers` warning on a clean close, through the linger and after
it. `sender.flow_control_receivers` during a linger says how many receivers took the
stream.

Harder: a closed publication's table keeps up to 64 ended entries for the linger
timeout. A receiver that first answers during the linger with the table full of them
is counted in `sender.flow_control_receivers_full`. A destination added while a
closed publication lingers never answers that publication: a receiver asks no setup
for a heartbeat with EOS. So `doctor` reads that destination as not answering the
closed publication until the linger ends, as it reads any destination before its
receiver's first status message.

Revisit: when `doctor` needs to tell a lingering publication from a live one for a
check of its own, for a counter that marks the close; when a control channel's
linger gets a key of its own (ADR 0054's Revisit), since the ended entries are held
for the whole linger.

## References

- `spec/udp.md`, "The sender", "The receiver" step 5, "Counters"; ADR 0053, ADR 0054
- `crates/cavalry-driver/src/sender.rs`, `serve`, the `min` table's sweep;
  `crates/cavalry-control/src/doctor.rs`, `udp_checks`
- The second review of pull request 152, 2026-11-06; AppShapes Trello card 225
