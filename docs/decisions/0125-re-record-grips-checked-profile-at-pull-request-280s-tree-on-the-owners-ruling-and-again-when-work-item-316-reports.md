# 0125. Re-record grip's checked profile at pull request 280's tree on the owner's ruling, and again when work item 316 reports

- **Status:** Accepted
- **Date:** 2026-11-14
- **Invariants affected:** 2
- **Supersedes:** none. Makes one re-record of `bench/baseline.json`, on the owner's
  ruling, that departs from two sentences. ADR 0033: "A run of a pull request that
  changes the data plane is judged and never recorded"; this entry is recorded from
  such runs alone. ADR 0110: "It is made only in three cases"; this is none of them.
  The rest of ADR 0033, ADR 0050's rule that a red at a pull request's own tree is a
  change, and ADR 0110's three cases for a session's own re-record stand. ADR 0110's
  Revisit does not fire (Consequences).

## Context

Pull request 280 (work item 229, ADR 0118) lets a cluster's service answer a
session. It adds a module to `cavalry-client` that no bench row runs. On the host
`grip`, in the checked profile `grip-linux`, two rows read slower at each of its six
heads than at any tree of `master` the same day:

| Row, p99.99 | `master`'s trees | 280's heads | The gate failed above |
| --- | --- | --- | --- |
| `claim_commit_32_client` | 117 to 120 ns | 126 to 132 ns | 129 ns |
| `claim_commit_32_recorded` | 129 to 136 ns | 147 to 153 ns | 150 ns |

At the head, 8fafb561, the first run failed on the first row, and the one re-run
that `docs/how-to/run-the-bench.md` gives failed on both. `grip`'s unchecked profile
and both profiles of the host `bench` did not move at any head.

By ADR 0050 a red at a pull request's own tree is a change, and by the how-to
("When it does not work", `REGRESSION`) such a change "does not merge until it is
not, or the baseline is raised in a commit whose message says why". That commit is
not a session's to make on its own: it is none of the three cases of ADR 0110.

What is known. The x86-64 Linux assembly of the bench and of every crate it links
was built with the release profile for `master` at 10985ab3 and for the branch at
453c0b4e, and every function's instructions were hashed. Every function a message
passes through has the same instructions in both trees: all 906 functions of
`cavalry-bench`, all 230 of `cavalry-log`, all 66 of `cavalry-proto`, and
`Publication::try_claim` and `offer` in `cavalry-client`. One function of
`cavalry-shm` and one of `cavalry-driver` differ, both on the control plane. What
moved is the crate's partition into codegen units, and with it the addresses of the
same functions in the linked binary. And this row's tail is known to follow where
memory lies on this host: work item 198 measured on 2026-11-11 that about one
process in ten reads `claim_commit_32_client` a tenth higher for its whole life,
and that the kernel's address-space randomisation decides which one. That is a
process's draw and not a build's. At 280's head every process of the checked
profile reads the row there.

What is not known. Why the same instructions at other addresses cost the recorded
row 6 ns at p50, 21 or 22 ns on `master`'s trees and 27 or 28 ns at 280's heads, in
that one profile of that one host. The assembly was built on a laptop and was not
read from `grip`'s own binaries. Work item 316 holds the question.

Work item 317 put four ways to land 280 to the owner: find the cause on `grip`
first, re-record at this tree, make the bench's build stable against placement, or
hold. He asked what 280 lands, whether it is better to land it first and find the
cause after, and what Cavalry's purpose says about it. He ruled on 2026-11-14, and
his words are on work item 317: land 280 by a re-record, with work item 316 as the
condition for tightening the baseline again.

## Decision

`grip-linux` is re-recorded in pull request 280, at its head's tree, from every
process `grip` has run there: eight, which are the two that failed and six taken
for this, each by a re-run of the job `bench (grip)`. A person's session makes it,
the one the owner ruled in, after it told him the tree and the time on the host.
The commit's message lists every gated row's slowest run before and after and names
the cause of each row that loosens by more than 5 percent.

ADR 0033 never records a run of a pull request that changes the data plane, since
such a run is what the gate judges. This entry is recorded from eight of them. The
owner ruled on what they read. And they are of the tree `master` has after the
squash: the result files name cc827dcf, the merge of 8fafb561 with `master` at
10985ab3, so the entry and the tree it was read at reach `master` in one commit.

The whole profile is re-recorded and not two of its rows, since an entry is of one
tree (ADR 0033). It is recorded from eight processes and not from the three the
rule asks for at the least, since the entry it replaces was of eight: from the
first two alone, nine other gated rows would have tightened by 1.4 to 6.2 percent,
by what the host's band happened to show, and the next change would be judged
against that. Every process is taken, the two that passed and the four with a
spike on the UDP rows among them (Evidence).

`grip-linux` is re-recorded again when work item 316's answer is on `master`, at
that answer's tree, from its first three processes and every one after them, so
that the rows gate at what the answer leaves them. Until then the gate fails
`claim_commit_32_client` above 145 ns and `claim_commit_32_recorded` above 168 ns.

This is one re-record and not a fourth case. The next red at a change's own tree
waits for the owner's ruling as this one did, and a session's own re-record stays
within ADR 0110's three cases.

## Alternatives considered

- **Find the cause on `grip` first, with 280 waiting.** It was work item 317's first
  recommendation. Lost on three things. Holding 280 protects no user: the
  instructions a message runs are the same with and without it, so whatever makes
  them slower at other addresses is on `master` already. The work of finding the
  cause is the same either way, and both trees stay buildable. And holding costs
  more each day: the other branches of work item 229 are stacked on 280, and each
  later change to `cavalry-client` can move the two rows again, either way.
- **Make the bench's build stable against placement first**, with one codegen unit
  for the data-plane crates or aligned functions. Lost: nothing yet shows that it
  removes the effect, and it re-records every profile of both hosts. If work item
  316 finds that it does, it is that work item's answer.
- **Hold 280.** Lost for the reasons of the first alternative, with nothing found
  meanwhile.
- **Re-run until a run passes, and merge.** Lost: two of the eight processes
  passed, with the client row at 129 ns, on the gate's edge, and six failed. A
  green run would be chosen, not measured.
- **Re-record on a branch of its own that lands before 280**, as ADR 0110's three
  cases do. Lost: `master` would hold an entry of a tree it does not have until 280
  lands, and a second pull request would take the hosts while 280 is open. In 280
  the entry and the tree it was read at land in one commit.
- **Join 280's processes to the entry that stands**, which would raise the two rows
  and leave the others. Lost: `--join` is for a process of the entry's own tree (ADR
  0033), and the entry would say that 280's runs were of the tree of 620a2e2.
- **Change the two rows by hand.** Lost: an entry with rows of two trees, written by
  no tool.
- **Record from three processes.** Lost: see the Decision.
- **Leave out the four processes with a spike on the UDP rows.** Lost: the how-to
  takes "every run that reached the profile, never a choice among them", and a
  process is left out only when a row's p50 and p99 moved with its p99.99 and its
  rate fell. They did not. The cost is in the Consequences.
- **Make it a fourth case that a session may take by itself.** Lost: the owner ruled
  on one pull request with its evidence in front of him. Nothing shows the next red
  at a change's own tree will be like it.

## Evidence

All of it is `grip`, an Intel Core i7-10700, profile `grip-linux`, pinned to CPUs 4
to 7, 5,000,000 messages (500,000 on the UDP rows), 21 runs a process, clock step
1 ns. A process's p99.99 is the median of its 21 runs.

The eight processes at 8fafb561's tree, run 43547701770, attempts 1 to 8. Attempts
1 and 2 are of 2026-11-14 02:41 and 02:54 UTC; attempts 3 to 8 ran from 14:23 to
15:02 UTC. p50 / p99 / p99.99 in ns, with the lowest and the highest run:

| Attempt | `claim_commit_32_client` | `claim_commit_32_recorded` | The gate |
| --- | --- | --- | --- |
| 1 | 29 / 76 / 132 (126 to 137) | 28 / 72 / 150 (136 to 160) | failed, client +7.3% |
| 2 | 29 / 76 / 132 (127 to 137) | 27 / 72 / 153 (143 to 157) | failed, client +7.3%, recorded +7.0% |
| 3 | 29 / 76 / 132 (129 to 136) | 28 / 71 / 149 (144 to 153) | failed, client +7.3% |
| 4 | 29 / 75 / 129 (127 to 134) | 28 / 72 / 148 (146 to 151) | passed |
| 5 | 29 / 75 / 133 (129 to 136) | 28 / 72 / 154 (148 to 158) | failed, client +8.1%, recorded +7.7% |
| 6 | 29 / 76 / 129 (127 to 135) | 28 / 73 / 149 (143 to 155) | passed |
| 7 | 29 / 73 / 131 (128 to 136) | 28 / 71 / 148 (145 to 154) | failed, client +6.5% |
| 8 | 29 / 76 / 132 (128 to 139) | 28 / 72 / 151 (149 to 158) | failed, client +7.3%, recorded +5.6% |

The entry that stood, of eight processes at the tree of 620a2e2, held the client
row at 29 / 70 / 119 and the recorded row at 26 / 67 / 138. Percentages are of that
entry's slowest runs, 123 ns and 143 ns.

Every gated row's slowest run, in ns at p99.99, before and after:

| Row | Before | After | Change | The gate fails above |
| --- | --- | --- | --- | --- |
| `claim_commit_32_client` | 123 | 139 | +13.0% | 145, was 129 |
| `claim_commit_32_concurrent` | 99 | 99 | 0.0% | 103 |
| `claim_commit_32_exclusive` | 92 | 90 | −2.2% | 94, was 96 |
| `claim_commit_32_ffi` | 145 | 143 | −1.4% | 150, was 152 |
| `claim_commit_32_recorded` | 143 | 160 | +11.9% | 168, was 150 |
| `claim_commit_32_udp` | 96 | 100 | +4.2% | 105, was 100 |
| `one_way_32_client` | 494 | 499 | +1.0% | 523, was 518 |
| `one_way_32_concurrent` | 426 | 426 | 0.0% | 447 |
| `one_way_32_exclusive` | 420 | 426 | +1.4% | 447, was 441 |
| `one_way_32_ffi` | 516 | 493 | −4.5% | 517, was 541 |
| `one_way_32_recorded` | 498 | 506 | +1.6% | 531, was 522 |
| `one_way_32_udp` | 5,851 | 20,639 | +252.7% | 21,670, was 6,143 |

The spikes on the UDP rows. In attempts 5 to 8, and in no other, runs 5 to 11 of
the checked profile's UDP rows hold a tail far above the others, with p50 and p99
where they were: `one_way_32_udp` 6,695 ns in attempt 5; 20,639, 20,431 and 7,259
ns in attempt 6; 6,783 ns in attempt 7; 6,675 ns in attempt 8; against 5,539 to
6,007 ns in every other run. `one_way_32_udp_mdc`, which is reported and not gated,
read 22,191 to 22,607 ns in one run of attempts 5, 6 and 7 and in four of attempt
8, against 8,103 to 8,775 ns. The unchecked profile of the same four jobs, two and
a half minutes later in each, has none. `grip`'s journal from 14:20 to 15:03 UTC
holds the runner's job lines and nothing else, its kernel log nothing, its thermal
throttle counts are zero, and no process was left running. The cause is not found.
It is recorded on work item 198, which holds the single-run spikes on this host.

In attempt 6 the unchecked profile failed, on `claim_commit_32_client` at 131 ns
(129 to 134) against 108 to 118 ns in the seven others: a whole process a tenth
up, which is work item 198's second effect. That profile is not re-recorded here.

The assembly comparison: work item 316's comment of 2026-11-14 01:49 UTC has the
method and the counts for each crate. The ruling: work item 317's comment of
2026-11-14 has what was put to the owner and his answer, whole.

## Consequences

- Until work item 316 reports, `grip-linux` passes a `claim_commit_32_client` up to
  145 ns and a `claim_commit_32_recorded` up to 168 ns at p99.99, where it failed
  them above 129 ns and 150 ns. A real regression of that size in those two rows
  passes that profile. `grip-linux-unchecked`, `bench-linux` and
  `bench-linux-unchecked` gate the same two rows at the entries they had.
- `one_way_32_udp` on `grip-linux` fails only above 21,670 ns, against medians of
  5,619 to 5,691 ns, until the profile is re-recorded: a regression of 3.8 times
  passes it. The other three profiles gate that row at the entries they had. The
  owner's ruling was on two rows; this third one loosens by the rule that every
  process is taken, and he was told of it before the commit.
- The other nine gated rows move by −4.5 to +4.2 percent.
- After the squash `master`'s tree is this tree. A later process there that reads a
  row above the entry joins it (ADR 0033).
- Until work item 316 reports, a change to `cavalry-client` can move the two rows
  again. One that puts them back at 119 ns and 130 ns passes; nothing is re-recorded
  for it.
- Work item 316 is in Doing from the day this lands. The owner has said which
  session runs on `grip` for it, and that is on work item 316.
- ADR 0110's Revisit reads "if a session's re-record ... is made outside the three
  cases: then a re-record goes back to a person's own commit". It does not fire.
  The clause is for a session that decides by itself to go outside the cases. Here
  the owner decided, on work item 317, with the evidence in front of him, and was
  told before the commit which session would make it, at which tree and from which
  processes. So this is his re-record, made by his session, and a session's own
  re-record is still held to the three cases. A session that makes one outside
  them with no such ruling fires the clause as it is written.
- To undo this, revert the commit: the entry of eight processes at the tree of
  620a2e2 comes back.
- **Revisit** if work item 316 is closed, or leaves Doing, with no report: the
  loosening has then lost its condition, and the owner is asked again.

## References

- Work item 317, the decision and the ruling. Work item 316, the cause. Work item
  198, the host's single-run spikes and its process mode. Work item 229 and pull
  request 280, the change.
- [ADR 0033](0033-record-a-bench-baseline-from-every-process-at-one-tree.md): an
  entry is of one tree, from every process at it.
- [ADR 0050](0050-re-record-at-a-tree-where-pull-requests-read-red.md): "A red at a
  pull request's own tree is a change".
- [ADR 0110](0110-let-a-persons-session-re-record-a-bench-baseline-after-telling-the-person.md):
  the three cases this is none of.
- [ADR 0118](0118-reply-to-a-session-from-the-service-beside-the-entrys-leader-and-wait-for-no-client-past-the-keepalive-interval.md):
  what pull request 280 lands.
- [Run the bench](../how-to/run-the-bench.md), step 4 and "When it does not work".
