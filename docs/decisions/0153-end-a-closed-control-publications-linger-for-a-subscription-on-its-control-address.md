# 0153. End a closed control publication's linger for a subscription on its control address

- **Status:** Accepted
- **Date:** 2026-11-24
- **Invariants affected:** 1, 3, 4, 6
- **Supersedes:** none. Amends `spec/udp.md`, "The sender" and "The receiver", within
  version 4, and `spec/client-protocol.md`, "Commands", the error codes, "Liveness and
  timeouts" and the sizes table, within version 10. Nothing on the wire changes.

## Context

`master`'s `ci` run on 0c68d788 failed `gates (linux)` in `cavalry-raft`'s
`tests/snapshot.rs`. A cluster member's duty cycle returned code 4: "the channel's
endpoint 127.0.0.1:24626: the socket could not be opened or bound: Address already in
use".

A cluster member's `log` endpoint is two things (`spec/cluster.md`, "The log"). While
the member leads, it is the control address of its log publication,
`cavalry:udp?control=<log>|control-mode=manual|fc=min`. While it follows, it is the
endpoint of its log subscription, `cavalry:udp?endpoint=<log>`. A leader that steps
down closes its publication and, once it follows another leader, subscribes on the
same address.

A closed publication lingers. On a control channel the linger is always the full
linger timeout, 5 s, since one destination's end of stream says nothing of another's
(ADR 0054). The send endpoint, and its socket bound to the control address, closes
only when its last publication's linger has ended and the publication is removed.

ADR 0049 holds an `add subscription` while the receive endpoint on its address closes.
Its bound is the closing endpoint wait, 5 s from the add, half the client's 10 s
response timeout. Nothing held an add behind a send endpoint, so the subscription was
refused at once. A module test makes a leader give up its log and join as a follower
at once, and it is refused with that same text on its own `log` endpoint. The CI log
cannot name the endpoint 24626 was, because the ports rule hands ports out one at a
time across threads. `cavalry run` retries a failing duty cycle every 100 ms, so in
production the member joined about 5 s late, with errors printed. A test panics on the
first error.

## Decision

**An add on a control address that only closed publications hold waits.** Take an
`add subscription` whose endpoint is the control address of a send endpoint on the
same driver, where every publication on that send endpoint is closed. It waits as ADR
0049's add waits: nothing registered, a slot of the subscriptions registry held,
counted in `subscription.add_deferred`, and bounded by the closing endpoint wait. It
runs, as if it had just arrived, when the sender answers that it has dropped the
endpoint's socket.

**The driver ends those publications' linger.** The conductor asks the sender to end
the linger of each closed publication on that send endpoint now, once for each, and
counts each in a new counter, `subscription.lingers_ended`, id 189. The sender answers
the close on its next sweep, the publication is removed, the endpoint closes, and the
add binds the address. On a sender that keeps up, that is a few cycles. In the module
test the join takes under 0.25 s, where it was refused before. A wait alone could not
fit: a linger that began just before the add ends up to 5 s later, and the sender's
drop comes after that, past ADR 0049's bound of 5 s from the add.

**An address that an open publication holds is refused.** When a publication on that
send endpoint is still open, the add is code 4 at once, as before. Its message now
says that the address is the control address of an open publication on this driver,
and names that publication's channel.

**What a subscriber of the cut publication sees.** A destination's receiver that had
not read the publication's tail gets no retransmission of it. Its image sees no more
frames. It reports that absence as it does when the publisher's driver stops: by the
image's liveness timeout and an `unavailable image`, never as a stream that ended
whole. The absence is reported, never silent (invariant 1).

Any client of the driver can cause this, by subscribing on that address, to another
client's closed publication. The port is the operator's, and only one socket can hold
it. The driver gives it to the live subscription over the courtesy the linger extends
to remote receivers.

**Why the cluster loses nothing.** A member that led closes its log publication when
it gives up its log. What a follower lacks of that publication's tail is either in the
next leader's log, which the follower takes from that leader, or it is no part of the
log the cluster committed, and the next leader's term truncates it.

**The other order is not this decision's.** A follower that leaves its log and leads
at once adds a publication whose control address is its `log` endpoint. The receive
endpoint there closes within a receiver cycle of the subscription's removal. An `add
publication` meeting that endpoint before it closes is refused with code 4 today. In
an election the two steps are milliseconds apart, so timing hides the refusal, but a
module test that makes them back to back is refused 3 times in 3. Its wait is work
item 447: a held publication add, with counters of its own, which ADR 0049 never built
for publications.

## Alternatives considered

- **Wait for the linger, with no end of it.** It lost on the bound. A control
  channel's linger is 5 s from the close, and ADR 0049's wait is 5 s from the add. A
  bound long enough for both would reach the client's 10 s response timeout, which
  ADR 0049 rejected so that the driver's answer and the client's timeout never race.
- **Have the module retry the join until the linger has passed.** It lost for ADR
  0049's reason: a retry loop in a client for a window the driver knows the length
  of. The member would still join 5 s late after every step-down, and every other
  client that met the case would need the same loop.
- **A leader's log publication with no control address bound**, so that only a
  follower's subscription ever binds the `log` endpoint. Aeron's manual control mode
  allows that. Two pages rule it out. `spec/udp.md`, "The channel URI", says that
  `control` and `control-mode` come together, and "What a reader rejects" refuses
  `control-mode` without a control address, under `control`. `spec/cluster.md`, "The
  log", names the leader's channel as `control=<its log endpoint>`. Taking it would
  move the UDP page to version 5, change the cluster protocol's channel, and change
  where a follower's status messages go.
- **Bind the receive socket beside the send socket**, with `SO_REUSEPORT` or
  `SO_REUSEADDR`. Two UDP sockets on one address share its datagrams. Under
  `SO_REUSEPORT` the kernel splits them between the sockets, and under `SO_REUSEADDR`
  the platform gives them to one socket of its choosing. Either way the lingering
  publication's status messages and NAKs, and the subscription's data frames, can
  reach the wrong socket.
- **End every control channel's linger at the close.** It lost because it gives up
  the tail's retransmission for every publication, where this decision gives it up
  only when the same driver needs the address back.

## Evidence

- `crates/cavalry-raft/src/module/tests.rs`,
  `a_leader_that_leaves_its_log_joins_it_again_at_once_as_a_follower`: a leader gives
  up its log (`remove_log`) and joins at once as a follower (`join_log`). Before this
  change it was refused with CI's text on its own `log` endpoint, 127.0.0.1:20003.
  After it, the join succeeds in under 2 s, 3 times in 3, at 0.22 s per run.
- `crates/cavalry-driver/src/conductor.rs`,
  `a_subscription_on_a_closed_publications_control_address_ends_its_linger_and_binds`:
  - while the publication is open, the add is refused, with a message that names the
    publication's channel;
  - once it is closed, two adds wait, `subscription.lingers_ended` counts one, and both
    run in order once the sender drops the socket, under 2 s;
  - the send endpoint has closed, and the receive endpoint is active.

  With the conductor's request to end the linger taken out, the adds do not run within
  5 s, and the test fails.
- A scratch test before the change, one driver on macOS: a publication on
  `control=127.0.0.1:P` was closed, and a subscription on `endpoint=127.0.0.1:P` was
  refused from 1.3 ms after the close, then succeeded after 5.05 s, on its 91st try.
- `src/module/tests.rs`'s other order, a follower that leaves its log and leads at
  once, is refused 3 times in 3 on 127.0.0.1:29003. That test is work item 447's.

## Consequences

- A member that led and steps down follows the next leader at once, with no failed
  duty cycle and no 5 s wait.
- A remote receiver of a control channel's closed publication can lose the tail's
  retransmission when a client of the publisher's driver subscribes on that address.
  `subscription.lingers_ended` counts it, and its `doctor` text names the cause and
  what to do outside a cluster.
- The conductor gains one sender request, `EndLinger`, and one flag on a closed
  publication. Nothing runs per message.
- Revisit if a client needs the tail of a control channel's publication delivered
  while the same driver reuses its control address. That client would need another
  endpoint, or the wait of the first alternative under a longer response timeout.

## References

- Work item 445 on the Brigade Board, a part of feature 4 of the beta slice, work item
  364. Work item 447 carries the other order.
- `spec/udp.md`, "The sender" and "The receiver"; `spec/client-protocol.md`,
  "Commands"; `spec/cluster.md`, "The log".
- ADR 0049, ADR 0054, ADR 0055, ADR 0068.
- `master`'s `ci` run 45487811627, first attempt.
