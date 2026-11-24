# 0154. Hold an add publication while a receive endpoint that holds its control address closes

- **Status:** Accepted
- **Date:** 2026-11-24
- **Invariants affected:** 1, 3, 4, 6
- **Supersedes:** none. Amends `spec/udp.md`, "The receiver", within version 4, and
  `spec/client-protocol.md`, "Commands", the error codes, "Liveness and timeouts", the
  sizes table, within version 10. Nothing on the wire changes.

## Context

A cluster member's `log` endpoint is two things (`spec/cluster.md`, "The log"). While
the member follows, it is the endpoint of its log subscription,
`cavalry:udp?endpoint=<log>`. While it leads, it is the control address of its log
publication, `cavalry:udp?control=<log>|control-mode=manual|fc=min`. ADR 0153 made the
way from leader to follower wait for the driver instead of failing. This is the way
back: a follower that wins an election leaves its log, which removes the subscription,
and then adds its leader publication on the same address.

A receive endpoint closes once its last subscription is removed and its images are
retired: one receiver cycle while the receiver keeps up, longer while a retired
image's recording catches up (ADR 0068). Until then its socket holds the address, and
an `add publication` that binds it there was refused at once with code 4. ADR 0049
holds an `add subscription` behind a closing receive endpoint, and nothing held an
`add publication`.

A module test, written for work item 445, makes a member join the log, leave it, and
add its log publication at once. It was refused 3 times in 3 on `master` at b0822607,
with "the channel's control address `<log>`: the socket could not be opened or bound:
Address already in use". In an election the two steps are milliseconds apart, so
timing hides the refusal. A receiver starved under CPU throttling shows it, and then
the member that has just been elected fails its duty cycles.

## Decision

**The add waits for the receive endpoint's close.** This applies to an `add
publication` or `add exclusive publication` on a control channel when a receive
endpoint on the same driver holds its control address (the same port, on the same
address or a wildcard) and that endpoint's last subscription has been removed. The add
waits with nothing registered and no event sent, and runs as if it had just arrived
when the receiver answers that it has closed that endpoint. The publication then binds
the address.

The wait is ADR 0049's, for the other order:

- the add holds a slot of the publications registry;
- the closing endpoint wait, 5 s from the moment the conductor read it, is its bound;
- one still waiting at the bound is code 4, with a message of its own that names both
  causes and what to do for each;
- one whose client closes or times out first goes with the client;
- one that waits again after a close keeps the moment it first arrived, and is
  counted once;
- the adds held behind one receive endpoint run at its close in the order they
  arrived, subscriptions and publications together, so that the first to ask has the
  address. A subscription held after a publication then meets an open publication's
  control address, and is refused as ADR 0153 says.

Three counters count it:

- `publication.add_deferred` (id 190), the adds held, once each;
- `publication.add_deferred_timed_out` (id 191), those refused at the bound;
- the gauge `publication.add_deferred_waiting` (id 192), those waiting now, so that
  `cavalry doctor` can tell a publications registry full of held adds from one full of
  publications, as `subscription.add_deferred_waiting` does for subscriptions.

The counting once also applies to the adds that ADR 0049 and ADR 0153 hold: an add
run again at a close and held again was counted a second time in
`subscription.add_deferred`, and is not now.

**No version moves.** A client sees the answers it saw before, `publication ready` or
code 4, and one of them later. No byte of a command, an event, a frame or a shared
layout changes. ADR 0049 kept `spec/client-protocol.md` and `spec/udp.md` at their
versions for the same kind of change, and the conformance suite, which runs every
client against the pages' bytes and answers, has nothing new to hold. The wait is the
driver's, and the driver's tests hold it.

**An address that a live subscription holds is refused at once.** While a
subscription is still on the receive endpoint, the address is that subscription's, and
waiting would not free it. The add is code 4 at once, as an `add subscription` on an
open publication's control address is (ADR 0153). Its message names the subscription's
channel. It offers another control address first, or removing that subscription, after
which the add waits.

## Alternatives considered

- **Answer with code 4 and a message to send the command again**, as ADR 0055 answers
  an `add publication` on a closing send endpoint. It lost because the member would
  then need a retry loop for a window the driver knows the length of, which ADR 0049
  rejected for subscriptions. ADR 0055's case is one sender cycle in which the add
  meets its own channel's endpoint going. This one is an address the same driver's
  receiver gives up on its own schedule, which a client cannot see.
- **Have the module wait in its election until the subscription's endpoint has
  closed.** It lost because the module has no view of a receive endpoint's state other
  than the driver's answer, and every client that changes an address from receiving
  to sending would need the same wait.
- **A leader's log publication with no control address bound.** It lost for the
  reasons in ADR 0153: `spec/udp.md` refuses `control-mode` without a control address,
  and `spec/cluster.md` names the leader's channel with its `log` endpoint as the
  control address.
- **Bind beside the receive socket with `SO_REUSEPORT` or `SO_REUSEADDR`.** It lost as
  it did in ADR 0153: two sockets on one address can each receive the other's
  datagrams.

## Evidence

- `crates/cavalry-raft/src/module/tests.rs`, `a_follower_that_leaves_its_log_leads_at_once`:
  a member joins the log (`join_log`), leaves it (`leave_log`) and adds its log
  publication (`add_log`) at once. On `master` at b0822607 it was refused 3 times in 3.
  With this change it passes 3 times in 3, in under a second each.
- `crates/cavalry-driver/src/conductor.rs`,
  `an_add_publication_on_a_leaving_receive_endpoints_address_waits_for_its_close`:
  - while a subscription is on the endpoint, the add is refused, and the message names
    the subscription's channel;
  - once the subscription is removed, the add waits with nothing registered, and
    `publication.add_deferred` counts it;
  - the add runs when the receiver closes the endpoint, and the send endpoint is active
    on the control address.

  With the wait taken out, the test fails.
- `an_add_publication_that_waits_past_the_closing_endpoint_wait_is_code_4`: with the
  receiver held, the add is code 4 at the bound and not a millisecond before. Its
  message names the address, `publication.add_deferred_timed_out` counts it, and the
  close that comes later registers nothing.
- `held_adds_behind_one_receive_endpoint_run_in_the_order_they_arrived`: a publication
  held first and a subscription held after it, behind one receive endpoint. At the
  close the publication binds, and the subscription is refused as an open
  publication's address is. Each is counted once. With subscriptions run first, as in
  the first version, the test fails.
- The agent review before the push found one defect in the first version: the held
  subscriptions ran before the held publications at a close, so a later subscription
  could take the address from a publication held first. It also found four smaller
  points, all taken: the address in the timed-out add's message, the waiting gauge,
  counting a held add once, and saying why no version moves.

## Consequences

- A member that followed and wins an election leads at once, with no failed duty
  cycle, whatever the receiver's pace inside the bound.
- The publications registry's waiting adds count against its 1,024, as the
  subscriptions registry's do against its own.
- Nothing runs per message. The conductor scans the receive endpoints once at an `add
  publication` on a control channel, and once more at each receiver close for the adds
  waiting on it.
- With ADR 0153, a member's `log` endpoint passes from subscription to publication and
  back without a refusal in either order.

## References

- Work item 447 on the Brigade Board, a part of feature 4 of the beta slice, work item
  364. It came from the development manager's review of work item 445's design.
- `spec/udp.md`, "The receiver"; `spec/client-protocol.md`, "Commands";
  `spec/cluster.md`, "The log".
- ADR 0049, ADR 0055, ADR 0068, ADR 0153.
