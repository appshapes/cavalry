# 0078. Keep an announced image's log for the counter reuse timeout, and report one that is gone as a fault

- **Status:** Accepted
- **Date:** 2026-11-11
- **Invariants affected:** 1, 3, 6
- **Supersedes:** none. Amends `spec/log-buffer.md`, "Closing", "The receiver" and
  "What a reader rejects", within version 1; `spec/client-protocol.md`, "Events" and
  "Liveness and timeouts", within version 8; `spec/udp.md`, "The receiver", step 5,
  within version 2; and `spec/c-abi.md`, the committed log's row and the row of the
  faults a poll stops on, within version 2. They gain a time the pages did not state
  and a rule for a case they did not specify. Nothing on the wire changes, no file's
  layout, and no function, code or layout of the C ABI.

## Context

The driver tells a client that an image exists with `available image`, which names
the image's log file and the `subscription.position` counter the client owns for
it. The client's conductor opens the file and takes the counter when it reads the
event. Nothing said how long the two stay. The driver removed a closed
publication's log "once every subscriber has reached that position"
(`spec/log-buffer.md`, "Closing"), and a subscriber that has just joined a
publication with nothing published is already there. So for a publication added and
removed with nothing published, the driver sent `available image`, then
`unavailable image`, freed the counter and unlinked the file, all before a client's
conductor had run once. The conductor then read `available image`, found no file,
took that for "the driver saying one thing and the directory another", and closed
the whole client as `corrupt` (`cavalry-client`, `Conductor::image_available`).

Card 241 found it with `cavalry replay --from 32`, a position inside a frame: the
replay fails at its first frame, its publication ends where it started, and the
command printed `the client is closed` where it should have printed that the replay
failed. Card 227 then met it three more ways. A service that opens the committed
log at a position where no entry begins lost its client, three runs of three. A
committed log closed straight after its open lost its client 8 times in 60 through
the Python client, and once in four runs of the Go runner. And the line that closed
the client there was not the file: the file opened, and the position counter was
already freed.

The client's own leaving does it too. A subscription added and removed with no poll
between has its counter freed by the driver on the removal, and the conductor may
read the `available image` after that.

Two things were unspecified, and by the rule that unspecified behaviour is a bug in
the spec, both are decided here: how long the driver keeps what an `available
image` names, and what a client does when it finds them gone.

## Decision

**The driver keeps the log and the position counter that an `available image` names
for the counter reuse timeout, 1 s, after it sends the event, unless the image's
subscription is removed first or the subscriber has moved the position.** The log
of a publication that is closed with a subscriber still at its join position, of a
replay's publication that ended so, and of an image over UDP that is retired with
such a subscription remaining, stays for what is left of that time, with that
subscription's counter. The counter reuse timeout is the time the protocol already
gives a client's conductor to act on an event before a freed record is used again;
this gives the same time to the event that names the record first.

`unavailable image` does not wait. It is sent when it was before: for a
publication, when every subscriber has reached the end of stream or the linger has
passed, and over UDP when the receiver retires the image. A client that reads both
events in turn opens the image and then retires it, as it does today when it is
quick.

Two things end the wait early, because each shows that no client is still to open
what the event named. A subscription's own removal frees its counter at once, as
before: its client knows. And a position that has moved from where the image
joined is the subscriber's own store, made through the counter and after reading
the log, so a subscriber that has read a frame makes nothing wait: a publication's
log goes as it did before for every subscriber but the one that has read nothing.
The rule is one function in the driver, used for a publication's images and for
each subscription of an image over UDP.

**A client that finds the log or the counter gone reports the image to its
subscription and stays open.** The log is not there, or the counter is not
allocated, or its key names another image: the reason is `gone`, a new row of
`spec/log-buffer.md`, "What a reader rejects". The conductor does not add the
image. The subscription's next poll returns the fault `image` with the reason
`gone` and no position, once. It means that the client's own `remove subscription`
was on its way, in which case nobody polls, or that the client read the event more
than the counter reuse timeout late; frames the publisher published after the
join, if any, were not delivered to that subscription, and the fault is their
report. A log that is there and fails another check closes the client as `corrupt`,
as before.

The fault is the existing `Fault::Image`, so the C ABI's codes do not change: 120,
`FAULT_IMAGE`, carries it, with the null position.

## Alternatives considered

- **Aeron's rule as the card first read it: linger a closed publication's log the
  whole publication linger, 5 s, drained or not.** Lost twice. It is not Aeron's
  rule: at commit 54a40fae47 an IPC publication whose subscribers are drained is
  removed on the next time event, with no linger. And it would hold every ended
  replay's publication for 5 s, against a bound of 128, where 1 s is the time the
  protocol already names for "a client's conductor has acted".
- **The client's side alone, which is Aeron's answer:** a log that cannot be mapped
  is an error handed to the application, and the conductor goes on. Lost as the
  whole answer: every publication added and removed with nothing published would
  then report a fault to its subscribers, a report of absence where nothing was
  absent, one time in eight on a laptop. Kept as the second half, for the client
  that is late.
- **The driver's side alone.** Lost: a client whose conductor is stopped for more
  than 1 s and less than the client liveness timeout, 10 s, would still close as
  `corrupt` on the first image that came and went meanwhile, with a name that sends
  its operator to look for corruption.
- **Drop a gone image in silence when the client can show nothing was missed,** by
  its own clock: its last duty cycle was less than the publication linger ago.
  Lost: it needs the linger as a constant both sides share and a comparison of two
  clocks, to spare a client that was a second late one fault.
- **A new fault of its own, `image-gone`.** Lost for now: it is a new code in the C
  ABI, which is being taken to version 2 as this is written, and `Fault::Image`
  already means "this image cannot be read, nothing from it is delivered, it is
  retired", with the reason beside it.
- **Hold `unavailable image` back with the log.** Built first for a publication,
  whose close was one step. Lost in its review: a subscriber that had opened the
  image and read nothing would hear it go up to a second late.
- **One time for an image over UDP, its last announcement.** Built first. Lost in
  its review: a subscription that joined and left would hold the log of an image
  that others had read for hours, and the page's "unless the subscription is
  removed" would not have been true over UDP.
- **A key of its own for each subscription's position counter,** so that a client can
  tell a record allocated again to another subscription on the same image. Not
  done: it is a change to the counters' keys, which `doctor` and the console read,
  and the case needs a client to poll a subscription it has itself removed. The
  client checks the type and the image's key, and drops an announcement for a
  subscription whose handle is gone.

## Evidence

- The card's reproduction, on a laptop, each five times: `cavalry run`, a recording of
  a hundred 32-byte messages, then `cavalry replay --recording 0 --from 32`. On
  `master` at d32c7a3, five of five: `cavalry replay: poll refused (closed): the
  client is closed`. With this change, five of five: `cavalry replay: the replay
  failed at position 32`, exit status 1, the client open.
- `crates/cavalry-client/tests/image_announced.rs`, three tests through a real driver:
  - a peer's publication added and removed with nothing published, 300 times beside
    one subscription: on `master` the subscriber's client closed as `corrupt` in
    round 1; with this change it is open after the 300;
  - the client's own subscription added and removed with no poll between, 300 times:
    on `master` the client closed as `corrupt` within the first rounds; with this
    change it is open after the 300;
  - a subscription removed behind its client's back in the driver's pass that
    announces its image, 100 times: the poll returns `Fault::Image` with the reason
    `gone` and the null position in 100 of 100 rounds, in four runs of four, then
    polls on, and the client is open.
- `crates/cavalry-client/src/conductor.rs`, one unit test of the decision against a
  real driver's directory: the image, for its log and its counter; `gone`, for a log
  that is not there, a record with no counter, and a record whose key names another
  image or another session; `corrupt`, for a record id no counters file has and for
  a file that is there and is no log.
- `crates/cavalry-driver/src/conductor.rs`, four tests on the driver's clock. A
  closed publication with an image announced in the same tick sends `unavailable
  image` at once, keeps its log and the image's counter until one millisecond short
  of the counter reuse timeout, and loses them at it; it loses them at once when the
  subscription is removed first, and at once when the subscriber has read a frame.
  An image over UDP whose stream ends at its join position tells its subscription at
  once and keeps its log and that subscription's record until the same moment; when
  the subscription is removed meanwhile its record is freed with the removal and
  the image goes on the next duty cycle. A replay of an empty recording waits the
  same way. That last test is one of the driver's 118 that were there, and the only
  one that had to change: it expected the log to go in the same tick.
- Through a cluster: `crates/cavalry-raft/tests/cluster.rs` opens the committed log
  at a position inside an entry and reads `failed_at` as that position, with the
  client open, and opens the log and closes it sixty times with no poll of it between;
  `crates/cavalry-ffi/tests/committed_log.rs` does the first through the C ABI, a
  test card 227's session wrote and could not land: on `master` it failed three
  runs of three at its first poll.
- `just bench` on the laptop, Apple M5 Pro, macOS, unpinned, 1,000,000 messages of 32
  bytes a row, on `master` at e86e721 and with this change, in turn. In nanoseconds,
  p50 / p99 / p99.99 / max:

  | Row | `master`, run 1 | this change, run 1 | `master`, run 2 | this change, run 2 |
  | --- | --- | --- | --- | --- |
  | `claim_commit_32_client` | 42 / 125 / 167 / 5127 | 42 / 125 / 125 / 3917 | 42 / 84 / 125 / 4795 | 42 / 84 / 125 / 2375 |
  | `claim_commit_32_exclusive` | 42 / 42 / 125 / 12839 | 42 / 42 / 167 / 11607 | 42 / 42 / 167 / 4295 | 42 / 42 / 167 / 9647 |
  | `one_way_32_client` | 83 / 333 / 8375 / 9631 | 83 / 209 / 6503 / 7519 | 83 / 375 / 11959 / 12919 | 83 / 250 / 9503 / 9751 |
  | `one_way_32_exclusive` | 83 / 291 / 1542 / 13463 | 125 / 250 / 1083 / 11935 | 125 / 375 / 9271 / 10439 | 125 / 541 / 6127 / 9687 |
  | `one_way_32_ffi` | 83 / 250 / 10351 / 11479 | 83 / 334 / 13775 / 61343 | 83 / 292 / 10879 / 11799 | 83 / 250 / 4875 / 5919 |
  | `one_way_32_recorded` | 125 / 250 / 7751 / 9967 | 125 / 375 / 8375 / 9799 | 125 / 209 / 4627 / 7503 | 125 / 375 / 17567 / 18143 |
  | `one_way_32_udp` | 11023 / 49919 / 132991 / 429311 | 10375 / 50079 / 131327 / 145919 | 9335 / 41535 / 129087 / 140543 | 10839 / 69439 / 134783 / 176383 |

  The p50s of the two builds are within one step of the laptop's clock, 41 ns, in
  every row but the one over UDP, where `master`'s two runs, 11,023 and 9,335 ns,
  lie either side of the change's, 10,375 and 10,839 ns. At p99.99 two runs of one
  build differ by more than the two builds do, with nothing pinned.
  One row looked worse with the change in several earlier runs, `one_way_32_recorded`
  at p99.99, so that row was run alone twelve times on each build, in the order
  `master`, change, change, `master`: the median p99.99 is 5,813 ns on `master`,
  from 3,251 to 18,095, and 5,045 ns with the change, from 2,833 to 20,559. Both
  builds have runs near 18 µs and the medians do not differ. The laptop shows no
  difference and cannot show a small one: the gate hosts' rows in CI are the
  judgement. Allocations per message: 0 in 5,200,000, in every run. The change is
  not on a message's path.

## Consequences

Easier: a client no longer closes on a race it did not cause, a replay that fails
at its first frame says so, and a test can now fail a replay through a cluster. The
word `corrupt` is left for a file or an event that fails a check.

Harder: a closed publication with a subscriber that joined less than 1 s before and
has read nothing holds its log and its counters for the rest of that second. The
images registry, 2048 pairs, bounds what waits, as it bounds the publications that
linger unread: a program that adds and removes publications with nothing published
beside a subscriber, more than two thousand in a second, has the next image refused
with error code 10. Replays that play nothing, an empty recording or a position
inside a frame, hold their place among the 128 replay publications for that second,
so more than 128 of those in a second beside their subscriber are refused with error
code 10; a replay that delivered a frame closes as before. One of the driver's
tests expected the log of a replay that played nothing to go in the same clock tick,
and advances its clock.

No counter is added. The driver makes no transition it did not make before: a
publication whose log waits is a publication closing, as one that lingers unread
is, and that state has no counter today; an image whose log waits is an image
retired. A refusal the wait causes is counted where every refusal is, in
`driver.registry_full`. The client has no counter of its own in this version, for
this fault as for `images-full`: the fault is reported where its subscription
polls.

Revisit if `doctor` has to explain an error code 10 from publications that wait
here: then the wait gets a counter. Revisit the fault's name when the C ABI next
gains a code for another reason.

## References

- `spec/log-buffer.md`, "Closing", "The receiver", "What a reader rejects";
  `spec/client-protocol.md`, "Events", "Liveness and timeouts"; `spec/udp.md`, "The
  receiver", step 5.
- `crates/cavalry-driver/src/conductor.rs`, `close_if_done`, `on_image_retired`,
  `remove_retired_images`; `crates/cavalry-client/src/conductor.rs`,
  `image_available`; `crates/cavalry-client/tests/image_announced.rs`.
- ADR 0021 (the subscription's image queue), ADR 0036 (the replay publications'
  bound), ADR 0068 (an image being retired), ADR 0010 (a reader's checks).
- Aeron at commit 54a40fae47: `IpcPublication.onTimeEvent`,
  `DriverConductor.transitionToLinger`, `ClientConductor.onAvailableImage`.
- Card 241, with card 227's three reports.
