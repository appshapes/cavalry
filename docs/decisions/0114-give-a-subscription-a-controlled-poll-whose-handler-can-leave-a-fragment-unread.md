# 0114. Give a subscription a controlled poll, whose handler can leave a fragment unread

- **Status:** Accepted
- **Date:** 2026-11-13
- **Invariants affected:** 1, 2, 3, 4, 6
- **Supersedes:** none. Amends `spec/log-buffer.md`, whose version stays 1, since no
  byte of the file changes: "Reading" says what a controlled read is. It is the
  first change of work item 229, the cluster client (ADR 0052, feature 5), and it
  makes the choice work item 282 asks for.

## Context

A subscriber reads with `Subscription::poll`: the handler is given each fragment, and
when it returns the image's position is past that fragment. A handler cannot refuse a
fragment. `Subscription::bounded_poll` (ADR 0066) stops a read at a position, which
the caller must know before the read begins. Two readers need to stop at a fragment
they learn about only by reading it.

The leader of a cluster reads its clients' session messages from the ingress stream
and appends each to the log. `spec/cluster.md`, "Sessions", says what happens when
the log publication's claim is back-pressured: "the message whose claim failed stays
unconsumed, the ingress image's position before it, and the leader reads nothing more
from its ingress images until a claim succeeds and the message is appended: it
buffers no message and drops none, so that the back pressure reaches the client's
publication and its caller". The page was written for work item 226 (ADR 0058), and
nothing in the client can do what it says: the claim fails inside the handler, after
the fragment was handed over. Work item 229 builds that path.

A cluster member's consensus module reads up to 16 consensus messages in one poll.
A message can move the member to init, where it reads nothing, and the messages of
the same poll after it are then handed to a member that acts on none of them, with
no count (work item 282; the comment in `ConsensusModule::do_work`). Work item 282
names two remedies and asks for a choice: a controlled poll in `cavalry-client`, or
a buffer of 15 messages in the module.

Aeron, read on 2026-11-13 at `master`, version 1.55.0-SNAPSHOT. A
`ControlledFragmentHandler` returns an `Action` for each fragment: `ABORT`, "Abort the
current polling operation and do not advance the position for this fragment";
`BREAK`, break and commit the position at the end of the fragment; `COMMIT`, continue
and commit the position at the end of the fragment "so that flow control is applied
to this point"; `CONTINUE`, continue with the position committed at the end of the
poll. `Image.controlledPoll` and `Image.boundedControlledPoll` run it over one
image, and `Subscription.controlledPoll` over every image in turn: an `ABORT` or a
`BREAK` ends the read of its image, and the subscription's poll goes on to the next
image. Aeron Cluster's ingress is read this way: `IngressAdapter` polls with
`controlledPoll`, and `ConsensusModuleAgent.onIngressMessage` returns `ABORT` when
the log publication does not take the message.

## Decision

The client gains a controlled poll, with Aeron's names. `cavalry-shm` gives the
reader `TermReader::controlled_poll(limit_position, limit, handler)`, and
`cavalry-client` gives the subscription `Subscription::controlled_poll(handler,
fragment_limit)`. The handler returns an `Action` for each data frame it is given.
`Action::Continue` takes the frame and the read goes on. `Action::Break` takes the
frame and ends the read of that image. `Action::Abort` does not take the frame: the
read of that image ends, the image's position stays at the frame's first byte, the
frame is not counted in the result, and the next read of the image gives the same
frame to its handler. `spec/log-buffer.md`, "Reading", says so. The subscription
reads its images in turn as `poll` does, and an action ends the read of one image:
the call goes on to the next, as Aeron's does. Each image's position is stored after
its share, as after any read.

Three of Aeron's four actions. `COMMIT` is left out: a Cavalry read stores the
position once, when it ends, and a batch is at most the fragment limit, so no reader
here needs the position stored inside a batch.

The reader's call takes a limit position, as `bounded_poll` does, and the
subscription's passes the largest. One loop then serves a bounded controlled poll
of one image, Aeron's `Image.boundedControlledPoll`, when a reader needs one; no
call for it is added now.

The call is not in the C ABI. The two readers that need it are the consensus module,
which is Rust, and a service that cannot take an entry yet, which no client has asked
for.

Work item 282's choice is the controlled poll, its first remedy. The module's change
stays that work item's.

## Alternatives considered

- **A buffer in the reader: the module keeps the messages it cannot take.** Work item
  282's second remedy. Lost for the ingress stream, which is the path that needs the
  call first: a session message the leader holds in a buffer is one the client's
  publication no longer sees, so the back pressure stops at the leader, and
  `spec/cluster.md` says "it buffers no message". With the controlled poll built for
  that path, a second mechanism for the consensus stream is one more bounded buffer
  to test at its bound for no gain.
- **Check the publication before the read, then read one fragment.** The leader would
  ask whether its log publication has room for the longest entry, and poll one
  ingress fragment only when it has. Lost: the check is not the claim. A claim is
  also refused where a term rotates and when no follower is connected, and a
  fragment read after a check that passed has nowhere to go when the claim fails.
- **A peek: one call that shows the next fragment, a second that takes it.** Lost:
  two calls for each message on the subscriber's path, and no name of Aeron's.
- **A handler that returns `bool`.** Lost: three outcomes exist. A reader that must
  stop after a message it took is the consensus module's case, and it is not the
  ingress case, where the message is not taken.
- **All four actions.** Lost: `COMMIT` would store the position in the middle of a
  read, which no reader here needs, and an action that does nothing a stranger can
  observe is a name to explain. The Revisit clause names when it returns.
- **An action that ends the whole call, not one image's read.** Lost: Aeron's
  `Subscription.controlledPoll` goes on to the next image, and a handler that must
  take nothing more returns `Abort` for each fragment it is handed, which costs one
  call for each image.
- **A parameter on `TermReader::poll`.** Lost, as it lost for the bounded poll: that
  loop is the subscriber's per-message path, its shape is measured (work item 144),
  and a reader that controls nothing must not pay for a branch on an action.
- **The call in the C ABI now.** Lost: a function through which no message passes yet
  is a surface to freeze at the first release for no user. A callback that returns a
  value also changes each runtime's trampoline, which ADR 0046 measures.

## Evidence

- `crates/cavalry-shm/tests/log_buffer.rs`,
  `a_controlled_poll_leaves_the_frame_its_handler_aborts_for_the_next_read`: five
  frames of 64 bytes, the third padding, and a claim that is not committed. An
  aborted first frame moves nothing; a frame aborted after one taken leaves the
  position at its first byte and one frame counted; the same frame is handed over
  again; a break takes its frame and hands over no other; padding below an aborted
  frame is passed; a limit position holds as in a bounded read; the fragment limit
  counts the frames taken; and with every frame taken the read gives what `poll`
  gives. `a_controlled_poll_ends_at_a_terms_end_and_reports_a_corrupt_frame`: the
  read ends at the end of a term and goes on in the next, and a frame that fails a
  check of `spec/frame.md` is reported before its handler sees it.
- `crates/cavalry-client/tests/subscription.rs`,
  `a_controlled_poll_hands_an_aborted_fragment_over_again_and_holds_its_publisher`,
  through a driver, two publications on one subscription: every fragment aborted
  leaves both images at 0; a break on one image leaves the other image's read to go
  on; each position is stored at the fragment not taken; every fragment is taken
  once. Then a handler that aborts everything, against a publisher of 1,024-byte
  messages: the publisher's `offer` returns `BackPressured`, the image's position has
  not moved, and every message that was accepted is delivered once the handler
  takes them.
- `crates/cavalry-client/tests/udp_receive_allocations.rs`: three of its ten rounds
  of a thousand messages are read with the controlled poll, each message aborted
  once and taken on the next read, every other one with a break. No allocation and
  no reallocation.
- `TermReader::poll` and `TermReader::bounded_poll` are not changed. No bench row
  calls the controlled poll; the bench jobs of the pull request are the measurement
  that the rows did not move.

## Consequences

Easier: the leader's ingress path can be written as `spec/cluster.md` says it,
with the back pressure of the log reaching the client's `offer`; work item 282 has
its remedy; a service that cannot take an entry yet has the same means when it asks.

Harder: a third read loop in `cavalry-shm` to keep in step with the two others; a
handler that aborts and never takes holds its publisher for good, which is the
caller's to bound, as a subscriber that never polls is; an aborted fragment is
handed over twice, so a handler must not act on a fragment before it decides to
take it.

Revisit: when a reader needs flow control to see its progress inside one batch, for
`Action::Commit`; when a reader needs a controlled poll of one image under a limit
position, a service that cannot take a committed entry yet, for
`Subscription::bounded_controlled_poll`, which the reader's loop already serves;
when a client in another language needs to leave a fragment unread, for the call in
the C ABI and the trampolines' cost.

## References

- `CLAUDE.md`: invariants 1, 2, 3, 4 and 6; Data plane; Protocol; When in doubt
- ADR 0046, ADR 0052, ADR 0058, ADR 0066
- `spec/log-buffer.md`, "Reading"; `spec/cluster.md`, "Sessions" and "Messages during
  an election"; `crates/cavalry-shm/README.md`; `crates/cavalry-client/README.md`;
  `docs/glossary.md`, controlled poll
- Work items 229 and 282 of the Brigade Board
- Aeron, read 2026-11-13 at `master` (1.55.0-SNAPSHOT):
  `aeron-client/src/main/java/io/aeron/logbuffer/ControlledFragmentHandler.java`
  (<https://github.com/aeron-io/aeron/blob/master/aeron-client/src/main/java/io/aeron/logbuffer/ControlledFragmentHandler.java>),
  `aeron-client/src/main/java/io/aeron/Image.java`, `Subscription.java`,
  `aeron-cluster/src/main/java/io/aeron/cluster/IngressAdapter.java` and
  `ConsensusModuleAgent.java`
