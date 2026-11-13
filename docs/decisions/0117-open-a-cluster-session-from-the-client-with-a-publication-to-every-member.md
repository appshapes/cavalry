# 0117. Open a cluster session from the client, with a publication to every member

- **Status:** Accepted
- **Date:** 2026-11-13
- **Invariants affected:** 1, 2, 3, 5, 6
- **Supersedes:** none. It is the client's part of work item 229, the cluster client
  (ADR 0052, feature 5). `spec/cluster.md` is not changed: the page says what a
  client sends and reads, and this decides the call a program makes.

## Context

`spec/cluster.md`, "The session protocol" and "Sessions", says what a client of a
cluster does. It subscribes to the egress stream on a response channel, sends a
session connect to a member, follows a redirect, and holds a session once the leader
answers ok. It sends session messages with its term and session id, a keepalive
when the keepalive interval, 500 ms, has passed since it last sent anything, and a
session close. It follows a new leader event to the new leader, a keepalive first.
A connect with no answer in the session connect timeout, 5 s, is lost. The members'
side is built in `cavalry-raft` (ADR 0115, ADR 0116). No program can use it without
writing the messages itself.

ADR 0052 asks for "a cluster session that connects over the ingress channel to the
leader with an egress channel for the cluster's replies and events, keeps alive, is
redirected to a new leader by the new leader event, and closes" in `cavalry-client`,
and for the C ABI and the five clients over it. So the call's shape is the shape
five languages get: what it takes, what blocks, and what a poll may do.

Three things the page leaves to the client. How long a program waits for a session
when the member it asks is down or an election runs. Where the session's messages go
when the leader changes: a publication to the new leader's `ingress` endpoint is a
command to the driver, which a poll on the data plane may not send. And who sends
the keepalive: the client's conductor thread, or the program's own calls.

Aeron, read on 2026-11-13 at `master`, version 1.55.0-SNAPSHOT, in
`aeron-cluster/src/main/java/io/aeron/cluster/client/AeronCluster.java`:
`AeronCluster.connect` blocks until the session is open or a message timeout
passes, and an asynchronous form steps the same states; with `ingressEndpoints` set
the client holds a publication to each member; `offer` and `tryClaim` send with the
session's header; `pollEgress` reads the replies and the events, and on a new
leader event closes and adds publications; `sendKeepAlive` is the caller's to call.

## Decision

`Cavalry::connect_cluster(members, response_channel)` opens a session and returns a
`ClusterSession`. `members` is the members string the cluster was started with,
which the page says a client holds. The call blocks, as `Cavalry::connect` and every
command of the client block. It subscribes to the response channel, adds an
exclusive publication to **every** member's `ingress` endpoint, and sends a session
connect to one member after another, the first in the string first. A redirect
moves it to the leader the redirect names; a redirect that names none moves it to
the next member after 100 ms; a connect with no answer in 5 s moves it to the next
member. It gives up after 20 s, the leader heartbeat timeout and the session connect
timeout twice: long enough for a cluster that has just lost its leader to elect
another and answer.

The session holds its publication to every member for its life. A new leader event
then changes which publication the session sends on, and its term, and nothing is
added or removed: `poll` sends no command to the driver and allocates nothing.

`ClusterSession::poll(handler, fragment_limit)` is the session's read and its duty
cycle. It hands the handler each of the cluster's messages to this session as its
payload and the cluster's time. It follows a new leader event of this session with
a term not below its own. It closes the session on a session event of code closed,
and `closed()` gives the cluster's reason. And it sends a keepalive when 500 ms
have passed since the session last sent anything, at once after a new leader event.
A program calls it at least that often.

`ClusterSession::offer(message)` sends a session message to the leader with the
session's term and id, written into a claim with the message. Its refusals are
`Offer`'s. A message is one frame, as an entry of the log is (ADR 0066, ADR 0116):
`max_message_length()` is 1,344 bytes on the default MTU, and a longer one is
`Offer::TooLong`.

`ClusterSession::close()` sends a session close and closes the publications and the
subscription.

The errors are a type of the session's own, `ClusterError`: a members string that
fails a check, the client's own error, the leader's refusal with its reason, a
cluster of another major version, the 20 s, and a fault of the response channel.
Each says what to do next.

The C ABI and the five clients get the same four calls in the next change of the
work item, with its own ADR, since the ABI's version moves.

## Alternatives considered

- **One publication, to the leader, replaced on a new leader event.** Lost: adding
  a publication is a command to the driver and an allocation, and the session's
  poll is on the data plane. A poll that takes the client to send a command, or an
  event the program must answer with a call of its own, puts the cluster's election
  into every program's loop in five languages. Three publications where one sends
  cost a log buffer each in the driver; the cluster's size bounds them at eight.
- **A connect that does not block, stepped by the caller.** Aeron has both. Lost
  for now: every command of this client blocks, a session is opened once, and a
  stepped connect is a state machine in each of five languages for a program that
  cannot wait. The Revisit clause names it.
- **The keepalive from the client's conductor thread.** Lost: the conductor holds
  no publication, and a keepalive it sent would keep open a session whose program
  has stopped reading. A session that its program does not poll is one the cluster
  should close.
- **A session that takes the ingress endpoints alone,** as Aeron's
  `ingressEndpoints`. Lost: the page says the members string is "the same string on
  every member and every client", and one string to copy is one less to get wrong.
- **A longer wait, or the caller's timeout.** Lost for the default: 20 s covers the
  one case a program cannot see, a leader just lost, and a caller that wants less
  time calls again or not at all. A parameter with a default is a knob (`CLAUDE.md`,
  Configuration and defaults).
- **`ClientError` for the session's errors.** Lost: `cavalry-ffi` gives every
  variant of `ClientError` a code of the C ABI, so a variant added here is a change
  to the ABI in a change that is not the ABI's. The session's errors get their codes
  with the session's functions.
- **Reassemble a reply longer than one frame.** Lost with ADR 0116's reason: a
  service's reply is one frame in this version, and a fragment is passed over.

## Evidence

- `crates/cavalry-client/tests/cluster_session.rs`, against a driver in the test's
  process with three members played by a second client in a thread: a session opens
  with the leader's session id and term, sends a message with them and a time of 0,
  reads the reply with the cluster's time, is refused a message of one byte more
  than a frame, sends a keepalive no sooner than 450 ms after its last message, and
  sends a session close; a member with no leader is passed and a redirect followed,
  the connects going to the first, the second and the third member; a refusal, a
  cluster of another major version and a members string of one bad member are
  reported, each with what to do; a new leader event turns the session, with a
  keepalive to the new leader inside 400 ms and the new term on the next message,
  and events of another session or an older term change nothing; a closed event
  closes the session with `TIMEOUT`, after which an offer is `Offer::Closed` and
  nothing more is sent; and a cluster that does not answer is given up after 20 s,
  four connects sent, to the first, second and third member and the first again.
- `crates/cavalry-client/tests/cluster_session_allocations.rs`: 5,000 messages
  offered and their replies read, two new leader events followed and a keepalive
  sent, under the counting allocator. No allocation and no reallocation.
- Aeron's source named in Context.

## Consequences

Easier: a program holds a session with three calls and one poll, and the same
shape reaches five languages; a leader's loss costs a program nothing but replies
it must ask for again; a session's poll is a subscriber's poll, with nothing of the
control plane in it.

Harder: a session costs the driver a log buffer for each member, 48 MiB each on the
default term length, most of it never touched; a program must poll at least every
500 ms or lose its session after 10 s; `connect_cluster` holds its caller for up to
20 s; a message accepted by `offer` is not committed, and the session does not say
when it is: a program that must know asks its service to reply.

Revisit: when a program cannot block for a connect, for a stepped connect; when a
session's log buffers are too much for a client of many clusters, for a term length
on the ingress publications (ADR 0099) or one publication replaced on an event
from the conductor; when a service's reply is longer than a frame, for reassembly;
when the session challenge is built, for credentials on the connect.

## References

- `CLAUDE.md`: invariants 1, 2, 3, 5 and 6; Data plane; Clients and developer
  experience; Configuration and defaults
- ADR 0046, ADR 0052, ADR 0058, ADR 0066, ADR 0099
- `spec/cluster.md`, "Members and streams", "The session protocol", "Sessions" and
  "Values"; `crates/cavalry-client/README.md`; `docs/glossary.md`, cluster session
- Work item 229 of the Brigade Board
- Aeron, read 2026-11-13 at `master` (1.55.0-SNAPSHOT):
  `aeron-cluster/src/main/java/io/aeron/cluster/client/AeronCluster.java`
  (<https://github.com/aeron-io/aeron/blob/master/aeron-cluster/src/main/java/io/aeron/cluster/client/AeronCluster.java>)
