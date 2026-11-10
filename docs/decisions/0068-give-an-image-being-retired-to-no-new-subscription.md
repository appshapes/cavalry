# 0068. Give an image being retired to no new subscription, and hold an add until its endpoint has closed

- **Status:** Accepted
- **Date:** 2026-11-10
- **Invariants affected:** 1, 3, 6
- **Supersedes:** none. Extends ADR 0049 to the moment before the one it decided: an
  `add subscription` on a `cavalry:udp` channel whose endpoint has lost its last
  subscription and whose images are still being retired. Amends `spec/udp.md`, "The
  receiver", step 5 and the endpoints, within version 2, and
  `spec/client-protocol.md`, "Commands", add subscription, within version 7. Nothing
  on the wire changes.

## Context

A subscription over UDP reads an image, and the image belongs to the receiver's
thread. When a stream's last subscription on an endpoint is removed, the conductor
asks the receiver to drop the image's last reader; the receiver retires the image and
says so; and only then does the conductor mark the image retired, unlink its log, and
close the endpoint when nothing else holds it (`spec/udp.md`, "The receiver", step 5).
Between the request and the reply the conductor's own table still lists the image as
live and the endpoint as active.

ADR 0049 decided what an `add subscription` does when it meets the endpoint closing:
it waits for the close and then binds the endpoint again. It did not decide the
moment before. An add that arrived there found an active endpoint with a live image
on its stream, joined that image as a second subscription joins a live image, and was
sent `available image` for it. The receiver's reply came a duty cycle later, the log
was unlinked, and a subscriber whose conductor had not opened the log yet read the
missing file as corruption and closed its client (`cavalry-client`, `image_available`).

Card 227 reaches that moment by design. A cluster follower that begins its election
again leaves the log, removing its subscription on its `log` endpoint, and joins the
log again when its leader answers, a few milliseconds later. The consensus module's
own tests found it. With the tests of `cavalry-raft`'s `tests/cluster.rs` run
together on a laptop, a member's duty cycle failed now and then with `client closed
(corrupt)`: in 2 of 16 runs of the file as the card's next change has it, with
seven tests. A line added to the client for the search, and removed after it,
printed the `available image` that closed the client: the new subscription, the
registration id of the image the first subscription had left, and "No such file or
directory".

A member whose client closes is a member lost, and no counter said why.

## Decision

From the removal of its last subscription an image is being retired, whether or not
the receiver has finished. The conductor marks the image when it removes that
subscription, and until the receiver's reply it gives the image to no subscription
added on its endpoint and stream and starts no recording on it.

Between the removal of its last subscription and its close, a receive endpoint takes
no subscription. An `add subscription` on its channel waits for the close exactly as
ADR 0049 has an add wait on a closing endpoint: nothing registered, no event sent, a
slot of the subscriptions registry held, the closing endpoint wait as its bound, and
`subscription.add_deferred` counting it. When the receiver has retired the images and
closed the socket, the add runs as if it had just arrived and binds the endpoint
again. The stream's next setup then gives it an image of its own, at once: a session's
cool-down dies with its endpoint.

On an endpoint that a subscription of another stream keeps open, there is no close to
wait for. The add runs at once and is not given the image being retired; the stream's
next image comes after the session's cool-down, which is what `spec/udp.md` already
says of a subscription added back on an endpoint that stays open.

In both cases what a client sees is what it would have seen had its add arrived after
the retire was done. The endpoint's states and `channel.status` are as they were: an
endpoint is active until its last image is retired, and closing from then.

## Alternatives considered

- **Leave the driver as it is and have the client drop an `available image` whose log
  it cannot open.** Lost as the fix for this. The driver would still announce an image
  it is retiring, keep the endpoint open for the new subscription, and put the
  stream's session on cool-down there: the subscriber would read nothing of a live
  stream for the image liveness timeout, 10 s, where the same two commands a moment
  later read it at once. What a client does with a log that is gone before it opens
  it is a question of its own, with other causes, a publication that ends first
  among them (card 241); this decision removes one cause and does not answer it.
- **Let the new subscription keep the image alive.** Lost. The conductor cannot know
  whether the receiver has retired the image yet. To make the join safe it would have
  to take the retire back, a second request racing the first, with the image's
  readers, its recording's stop and its cool-down each in one of two states.
- **Do not join the image, and let the add run on the open endpoint.** Lost for an
  endpoint with no other subscription. The new subscription would hold the endpoint
  open, so the retired session's cool-down would apply: the follower of a cluster
  would wait 10 s for an image, which is its own timeout for giving the join up.
- **Mark the endpoint closing, `channel.status` 2, from the last subscription's
  removal.** Lost. The state has a counter and a meaning in `doctor`'s endpoint note,
  a socket the receiver is dropping, and a retire can be put off by a recording that
  has yet to stop. The wait needs the fact, not the state.

## Evidence

- `cavalry-driver`, `conductor::receiving`, two tests on the conductor with a real
  receiver thread and a sender on the loopback. Both remove a stream's last
  subscription and add another before the receiver has run.
  `an_add_while_the_last_subscriptions_image_is_being_retired_waits_for_the_close`:
  the add is held, with no error, no image and nothing registered; the image is
  retired, the endpoint closes and opens again, `channel.opened` 2 and
  `channel.closed` 1; the new subscription was told of no image; the sender's next
  setup gives it a new image with another registration id.
  `an_add_on_an_endpoint_another_stream_keeps_is_not_given_the_image_being_retired`:
  the add runs at once, is given no image, the image is retired and removed with no
  event to the new subscription, and the endpoint stays active.
  With the three changes to `add_subscription` taken out, both fail at the first
  check after the add: `left: ([], [0], [])`, the image being retired, registration
  id 0, made available to the new subscription.
- `cavalry-raft`, `tests/cluster.rs`, the file's tests run together on the laptop,
  16 runs each way.
  The file as the card's next change has it, seven tests, two of them with a
  follower that joins the log again: without this change 2 runs fail, both with
  `client closed (corrupt)`, one of them in
  `a_follower_that_leaves_the_log_joins_it_again_where_its_recording_stopped`, a
  test master has. With this change no run fails so.
  Master's five tests: 16 of 16 runs pass with this change and 16 of 16 without, so
  master's file does not show the fault at that count.
- One of the 16 runs with this change failed another way: a follower's `log`
  endpoint could not be bound again, `Address already in use`. The tests choose
  their ports by binding port 0 and releasing it, so between a member's uses of a
  port any socket on the host may be given it. With each test's ports taken from
  below the range the operating system gives out, and with this change, 40 of 40
  runs pass. That is the tests' fault and a change of theirs, not this decision's.
- `cavalry-client`, `tests/udp_resubscribe.rs`, unchanged, passes: remove and add at
  once with no publisher still waits for the closing endpoint and binds it again.

## Consequences

Easier. A client that removes a subscription and adds it again is given a live
stream or waits for one: it is never handed a log that is about to go. A cluster
follower leaves the log and joins it again in a few milliseconds, as often as its
elections ask, on a driver whose receiver is behind.

Harder. An add after a stream's last subscription waits for the retire as well as
for the close. A retire the receiver puts off, at its pending replies' bound, and the
log of an image whose recording has yet to stop, both lengthen that wait, and past
the closing endpoint wait, 5 s, the add is code 4 as ADR 0049 has it. A member of a
cluster stops its recording before it removes its subscription, so its own add does
not wait on one.

To maintain. The mark is the conductor's, set where a subscription is dropped and
read where a subscription joins an image and where a recording finds one. A new place
that gives out an image reads it too.

Revisit when `subscription.add_deferred_timed_out` rises on a driver whose images
wait on recordings, and with card 241, which decides what a client does with an
`available image` whose log is gone.

## References

- ADR 0049, the add that waits for a closing endpoint; ADR 0042, the image's states;
  ADR 0059, the recording armed on a subscription; ADR 0063, the follower that leaves
  the log and joins it again.
- `spec/udp.md`, "The receiver", step 5 and the endpoints; `spec/client-protocol.md`,
  "Commands", add subscription, and "Liveness and timeouts", the closing endpoint
  wait.
- Card 227; card 241.
