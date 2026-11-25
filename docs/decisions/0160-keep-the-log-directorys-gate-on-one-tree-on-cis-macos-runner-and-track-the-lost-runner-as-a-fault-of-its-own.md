# 0160. Keep the log directory's gate on one tree on CI's macOS runner, and track the lost runner as a fault of its own

- **Status:** Accepted
- **Date:** 2026-11-25
- **Invariants affected:** 3, 6
- **Supersedes:** ADR 0156 on one point, where the gate runs all seven of its writes:
  on CI's macOS runner it writes server 0.9.0's directory alone, as it did before
  work item 441. The rest of ADR 0156 stands, on Linux and in every other run.

## Context

ADR 0156 has the log directory's upgrade gate, `conformance/upgrade/log-directory.sh`,
write three directories in seven ways: 620a2e29's alone, 620a2e29's and then
d265af82's in place, and d265af82's alone, each with its last run stopped and again
killed, and the one written in turn a third time with 620a2e29's last run killed. It
landed on `master` as 2387b4be on 2026-11-25. On CI's macOS runner, `gates (macos)`,
two of its first four jobs then lost their runner partway through `just conformance`:
`master`'s run 45549149085 and the by-hand run 45536715972. GitHub's record for each says
"The self-hosted runner lost communication with the server", and the job keeps no log
by any route. In `master`'s 60 runs before, no macOS job lost its runner.

A lost runner turns `master` red, and it skips the macOS job's later steps:
`just docs`, `just quickstart`, `just replay-check` and the bench. Each landing met it
at about one chance in two. So pull request 352 (e7008334) had the script take its
one-tree form on CI's macOS runner alone. That is a gate weakened on one runner, and
until now its reason has been only in a commit message. Its pages said that work item
441 stays open "until they run on macOS again, or an ADR says why they do not". This
is that ADR.

The experiments on throwaway branches, set out under Evidence, did not find the cause,
and they spent their time box. That box was one round in the job's own order, and one
more if it lost a runner. It lost none.

## Decision

On CI's macOS runner, when no `CAVALRY_DIRECTORY_FROM` is set, the gate keeps building
620a2e29 alone and writing server 0.9.0's directory alone, stopped and killed. Linux
writes all seven, and so does every run that is not CI's macOS runner: a laptop, or a
run by hand. The servers in production run Linux, and `gates (linux)` is the check a
pull request requires (ADR 0073).

The lost runner becomes a fault of its own, work item 456, and work item 441 ends
with this decision. The script's comment, `conformance/README.md` and
`docs/explanation/what-brigades-server-relies-on.md` name that work item and this ADR,
in place of "441 stays open".

## Alternatives considered

- **All seven on macOS, and run a lost job again.** Lost: a lost runner keeps no log.
  `master` is red until someone runs the job again, and in a lost job, docs,
  quickstart, replay-check and the bench do not run for that commit. At two losses in
  seven jobs, about one landing in four would pay that.
- **Leave work item 441 in Doing until the cause is found.** Lost: a work item parked
  in Doing reads as taken to the next session, and the experiment has no next step
  that is cheap. The fault's own work item carries the findings and what was not
  tried.
- **Drop the seven from the gate on every runner.** Lost: Linux has run them in every
  job and in 15 runs in a row with no loss, and Linux is what the servers run. That
  would give up ADR 0156's hold on the directories the servers have, to spare a
  runner they do not use.
- **Keep d265af82's directories on macOS, and drop only the directory written in
  turn.** Lost: no arm named the part that loses the runner. The directory written in
  turn passed 24 runs alone, and d265af82 alone passed 12 more. 620a2e29 alone is the
  one form that has run on that runner with no loss: 60 jobs before work item 441,
  and every job since 352.

## Evidence

The runner: Blacksmith's `blacksmith-6vcpu-macos-latest`, a macOS 26.3 virtual
machine on arm64, with 6 vCPUs, 24 GiB of memory and a 409 GiB disk. Each experiment
ran on the throwaway branches `441-gate-on-macos` and `441-gate-on-macos-2`. A
watchdog sampled the runner every 15 or 30 s, and posted each sample off the runner
as a commit status. Markers, where an arm had them, were posted before each step of
the gate.

| Where the seven-directory gate ran | Runs | Runners lost |
| --- | --- | --- |
| CI's own job: `just lint`, `just test`, then `just conformance` (runs 45536355928, 45536715972, 45549149085, and round 5b's three, run 45563204036) | 7 | 2 |
| The gate alone in a loop, no markers (test-times 45553732082; rounds 2, 3 and 4: runs 45556061823, 45558818935, 45560862496) | about 50 | 3 |
| The gate alone, with a marker before each step, waited for or sent in the background (runs 45557226906 and 45558818935) | 32 | 0 |
| The gate alone, with a pause of 10 ms before every program, or of 100 ms where d265af82 starts on what 620a2e29 left (run 45560862496) | 40 | 0 |
| The directory written in turn alone, one arm for each ending, with markers (run 45557226906) | 24 | 0 |
| 620a2e29 alone, and d265af82 alone in server 0.10.0's shape (run 45556061823) | 24 | 0 |
| Linux, the gate alone in a loop (test-times 45553732082) | 15 | 0 |
| This laptop: macOS 26.6.1 on arm64 hardware, 64 GiB, by hand and in `just conformance` | at least 10 | 0, and the machine was none the worse |

None of the counts where no loss was seen is large enough to show that its arm
prevents the loss. At the bare loop's rate, about 3 in 50, zero losses in 24 or 40
runs is within chance. So neither the markers, nor the pauses, nor a single tree, nor
the directory written in turn alone is a remedy or a cause.

Each loss was sudden. In run 45556061823 the seven lost its runner within 15 s of the
start of its fourth run. In 45558818935 it was lost in the first seconds of its
eleventh. Every sample before was flat, and the watchdog, a process of its own, went
silent at the same instant as the gate.

What the samples rule out:
- **Disk:** 147 to 151 GiB free throughout.
- **Swap:** 0.
- **Files:** at most 190 under the gate's directories.
- **The gate's own memory.** On the laptop the machine's file-backed pages rose at
  most 182 MiB during the seven, against 161 MiB during the one-tree form. The files
  alive at once peaked at 4,813 MiB in 116 files against 1,171 MiB in 29, by their
  lengths, most of them sparse 128 MiB segment files. On the runner, 12 to 15 GiB
  were free when `just conformance` began, with the compressor near 0.
- **A quiet machine:** round 4 began each arm at a load average under 3, and the bare
  seven passed 20 of 20.
- **A signal to the runner's own processes:** the gate signals only the processes it
  started, by their process ids, never a process group.

What was not tried, and goes to work item 456:
- Fresh ports at each start. Every feed of a run, in every directory, binds the same
  two 127.0.0.1 UDP ports, counted from the script's process id.
- Another size or image of the runner.
- The provider's own record of those machines.
- A watchdog that tells a machine that stopped from a machine whose network stopped.
  Every sample leaves over the network, so the two look the same here.

One more finding was not followed here, and goes to work item 457. On the macOS
runner, `just test` brings free memory down to 304 to 518 MiB of 24 GiB, with one
test process (`snapshot`) at 16 GiB resident, `image_announced` at about 8 GiB and
`cluster_service` at 4.5 GiB, swap at 0 and the job passing. The gate
starts after that, with 12 to 15 GiB free again.

## Consequences

- On every `master` commit, the macOS job holds the gate's one-tree form, as it did
  before work item 441. It does not hold that a build on macOS opens the directory
  the servers hold now. Linux holds that in every pull request and on `master`.
- A change that only macOS would break in d265af82's directories, or in the
  directory written in turn, goes unseen there. The servers run Linux, and the
  laptop's runs by hand write all seven.
- The script's comment, the README and the explanation page say where the exception
  is tracked. A stranger reading the macOS job's log sees the script's own line about
  it.
- **Revisit** when any of these happens:
  - work item 456 finds the cause;
  - the runner's image or size changes;
  - a runner is lost on macOS in a job that does not run this gate;
  - the servers in production run macOS.

  Work item 456's "Done when" decides it: the cause written with the run that shows
  it, or a new ADR that says why the exception stays. The seven return to CI's macOS
  runner only after twenty jobs in CI's own order and a hundred runs of the gate
  alone, from a throwaway branch, with no runner lost.

## References

- Work item 441, which carried this decision; work item 456, the fault, part of
  feature 8 (work item 368); and work item 457, the memory that `just test`
  holds on the macOS runner.
- ADR 0156, the seven directories; ADR 0141, the gate; ADR 0073, which jobs run where.
- Pull requests 349 (2387b4be), which added the seven, and 352 (e7008334), which put
  the one-tree form on CI's macOS runner.
- `conformance/upgrade/log-directory.sh`, the comment above "The older builds".
