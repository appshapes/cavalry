# 0049. Hold an add subscription until its channel's closing endpoint has closed

- **Status:** Accepted
- **Date:** 2026-11-05
- **Invariants affected:** 3, 5, 6
- **Supersedes:** none. Amends `spec/client-protocol.md`, "Commands", "Error codes",
  "Liveness and timeouts" and "Sizes", within version 4, with a rule for a case the
  page answered with an error: an `add subscription` on a `cavalry:udp` channel whose
  previous receive endpoint is still closing

## Context

A `cavalry:udp` subscription receives on a receive endpoint, one socket per resolved
channel, which the conductor closes when its last subscription and image are gone: the
entry moves to `Closing`, the receiver is asked to drop the socket, and the receiver's
`EndpointClosed` reply frees the registry slot one receiver cycle later (ADR 0042). An
`add subscription` on the same channel inside that window found the port still bound.
Card 181's fifth blocker on pull request 98 gave it a truthful refusal in place of a
bind error that blamed the operator's port: code 4 with a message that said the
previous endpoint was still closing and to add the subscription again after a moment.

Remove then add on one channel in one client is an ordinary sequence, and a client
should not need a retry loop for it (card 194, decided with Rjae on 2026-11-02 from
that review note). Every other refusal of code 4 is a socket the host would not give;
this one was the driver's own timing.

## Decision

The add waits. The conductor keeps it in a bounded list of pending adds, with nothing
registered and no event sent, and when the receiver's `EndpointClosed` reply frees the
endpoint's slot it runs the waiting adds for that endpoint in the order they arrived,
each as if it had just arrived: the first binds the endpoint again, registers its
subscription and answers `subscription ready`. A waiting add holds a slot of the
subscriptions registry, so the registry fills with waiting adds as it fills with
registered ones and one more is code 10, the same error as before, whether the add is
on `cavalry:ipc` or `cavalry:udp`. A waiting add whose client closes or times out first
goes with the client.

The wait is one receiver cycle while the receiver keeps up, and it has a bound of its
own, the closing endpoint wait, 5 s from the moment the conductor read the add: half
the client's 10 s response timeout, so that the answer reaches the client before the
client reports the add lost. The receiver can fall that far behind: its thread starved
under cgroup CPU throttling, or the endpoint's remove waiting behind a full request
queue. An add still waiting at the bound is answered with code 4, the bind it would
have made, with nothing registered and a message that names the endpoint, the wait
and what to do; the close that comes later runs nothing for it.
`subscription.add_deferred` counts the adds held,
`subscription.add_deferred_timed_out` the adds refused at the bound, which `cavalry
doctor` reports, and the gauge `subscription.add_deferred_waiting` the adds waiting
now. The page says so under `add subscription`, in the code 4 row, in "Liveness and
timeouts" and in the sizes table; nothing on the wire changes and the page stays at
version 4.

## Alternatives considered

- **Keep the refusal and have the client retry.** Lost: a retry loop in every client
  for a window the driver knows the length of, and five more clients to write it in
  (ADR 0046).
- **Bind the new endpoint beside the closing one.** Lost: the port is the operator's
  and one socket holds it; a second bind fails until the first socket is gone.
- **A separate registry with a size of its own.** Lost: a waiting add becomes a
  subscription or goes, so the subscriptions registry's bound is the one it belongs
  under, and the sizes table gains no row.
- **Run the waiting adds on the conductor's next cycle rather than at the reply.**
  Lost: the reply is the moment the slot is free, and the adds are the reply's.
- **Wait with no bound of its own.** Lost: a receiver that runs no cycle for the
  client's response timeout leaves the client reporting the add lost while the driver
  still holds it, and the close that comes later registers a subscription the client
  dropped, with a registry slot and a `subscription.position` per image that nobody
  advances until the client closes (invariant 3). Review of pull request 123,
  blocker 2.
- **A bound equal to the response timeout.** Lost: the driver's answer and the
  client's own timeout would race, and the client could report the add lost and then
  be sent `subscription ready` for it.
- **Code 10 at the bound.** Lost: code 10 says a registry of "Sizes" or the counters
  file is full, and neither is; what fails is the bind the add would have made, on a
  port the closing socket still holds, which is code 4's case.

## Evidence

The conductor's tests: `an_add_on_a_channel_whose_endpoint_is_still_closing_waits_for_the_close`,
two adds in the window, nothing registered and no event until the receiver's close,
then both registered in order with `subscription ready` for each, `channel.opened` at 2
and no error; `waiting_adds_hold_registry_slots_and_go_with_their_client`, the
registry full of waiting adds, one more refused with code 10, and the waiting adds gone
with their client before the close registers anything;
`an_ipc_add_cannot_take_the_slot_a_waiting_add_holds`, 1023 subscriptions registered
and one add waiting, a `cavalry:ipc` add refused with code 10 and the close then
registering the waiting add; `an_add_that_waits_past_the_closing_endpoint_wait_is_code_4`,
the receiver held while the conductor's clock passes the bound, the add answered with
code 4 and counted, nothing registered, and the receiver's close afterwards
registering nothing. The client's test
`a_subscription_removed_and_added_on_one_udp_channel_needs_no_retry` over the public
API: remove then add at once, `subscription ready` with no refusal. Nothing on a
message's path changes; `just bench` before and after is in the commit message.

## Consequences

Easier: a client removes and adds on one channel and needs no retry; code 4 for the
driver's own timing is left only past a bound a receiver that keeps up never reaches,
and then `cavalry doctor` names it.

Harder: the conductor holds the waiting add's channel bytes, one allocation per add
on the control plane; a waiting add is a registry slot a client cannot see as a
subscription until the close, which `subscription.add_deferred_waiting` explains; the
duty cycle reads the list of waiting adds while it holds any, on the control plane.

Revisit: when a client needs the add's wait reported as an event of its own; when a
`cavalry:udp` publication on a closing send endpoint meets the same window; when
`subscription.add_deferred_timed_out` rises on a host whose receiver is not starved.

## References

- `spec/client-protocol.md`, "Commands", "Error codes", "Liveness and timeouts" and
  "Sizes"; `spec/udp.md`, "The receiver"; ADR 0040, ADR 0042, ADR 0046
- `crates/cavalry-driver/src/conductor.rs`, `subscriptions_full`,
  `add_udp_subscription`, `take_receiver_replies`, `time_out_pending_subscriptions`,
  `remove_client`; `crates/cavalry-driver/src/spec.rs`, `CLOSING_ENDPOINT_WAIT`
- Pull request 123's review, blockers 1 and 2
- AppShapes Trello card 194; card 181's pull request 98
