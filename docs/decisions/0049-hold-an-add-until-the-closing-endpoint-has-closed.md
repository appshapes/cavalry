# 0049. Hold an add subscription until its channel's closing endpoint has closed

- **Status:** Accepted
- **Date:** 2026-11-05
- **Invariants affected:** 3, 5, 6
- **Supersedes:** none. Amends `spec/client-protocol.md`, "Commands", within version
  4 with a rule for a case the page answered with an error: an `add subscription` on a
  `cavalry:udp` channel whose previous receive endpoint is still closing

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
registered ones and one more is code 10, the same error as before. A waiting add
whose client closes or times out first goes with the client. The wait is one receiver
cycle, far inside the client's 10 s response timeout. `subscription.add_deferred`
counts the adds held. The page says so under `add subscription` and in the sizes
table; nothing on the wire changes and the page stays at version 4.

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

## Evidence

The conductor's tests: `an_add_on_a_channel_whose_endpoint_is_still_closing_waits_for_the_close`,
two adds in the window, nothing registered and no event until the receiver's close,
then both registered in order with `subscription ready` for each, `channel.opened` at 2
and no error; `waiting_adds_hold_registry_slots_and_go_with_their_client`, the
registry full of waiting adds, one more refused with code 10, and the waiting adds gone
with their client before the close registers anything. The client's test
`a_subscription_removed_and_added_on_one_udp_channel_needs_no_retry` over the public
API: remove then add at once, `subscription ready` with no refusal. Nothing on a
message's path changes; `just bench` before and after is in the commit message.

## Consequences

Easier: a client removes and adds on one channel and needs no retry; the one
driver-timing refusal of code 4 is gone, so code 4 means a socket the host would not
give.

Harder: the conductor holds the waiting add's channel bytes, one allocation per add
on the control plane; a waiting add is a registry slot a client cannot see as a
subscription until the close, which `subscription.add_deferred` explains.

Revisit: when a client needs the add's wait reported as an event of its own; when a
`cavalry:udp` publication on a closing send endpoint meets the same window.

## References

- `spec/client-protocol.md`, "Commands" and "Sizes"; `spec/udp.md`, "The receiver";
  ADR 0040, ADR 0042, ADR 0046
- `crates/cavalry-driver/src/conductor.rs`, `add_udp_subscription`,
  `take_receiver_replies`, `remove_client`
- AppShapes Trello card 194; card 181's pull request 98
