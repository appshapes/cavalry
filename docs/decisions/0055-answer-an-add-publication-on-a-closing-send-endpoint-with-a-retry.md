# 0055. Answer an add publication on a closing send endpoint with code 4 and a retry

- **Status:** Accepted
- **Date:** 2026-11-06
- **Invariants affected:** 3, 5, 6
- **Supersedes:** none. Answers ADR 0049's Revisit for a `cavalry:udp` publication;
  `spec/client-protocol.md` stays as it is, since it already answers a socket that
  cannot be bound with code 4

## Context

A send endpoint closes when its last publication's linger ends (`spec/udp.md`, "The
receiver", the endpoints). The conductor marks it closing, asks the sender to drop the
socket, and frees the registry slot when the sender replies `EndpointClosed`. While the
sender keeps up, that reply comes one sender duty cycle later. An `add publication` in
that window finds no active endpoint on its channel and opens a new socket.

Before ADR 0053, only a channel whose `interface` names a port bound a fixed address.
Every other send socket bound an ephemeral port, so the new socket bound beside the
closing one. A control channel's socket binds its control address. Since ADR 0053,
every control channel meets the window, and so does a replay's publication on one. The
bind failed with `EADDRINUSE`, and the code 4 message told the operator to check that
the address is this host's and that its port is free. That is the wrong next step: the
port is held by the driver's own closing socket. The review of pull request 152 on
2026-11-06 found this, blocker 4. ADR 0049 holds an `add subscription` for the same
window, and its Revisit names this case: "when a `cavalry:udp` publication on a closing
send endpoint meets the same window".

A publication's window is not a subscription's. A receive endpoint closes when its last
subscription is removed, so a client that removes and adds a subscription on one
channel meets the window every time. A send endpoint stays active through its last
publication's linger, at least 5 s after the close, and a control channel's lingers
the full linger timeout (ADR 0054). An add on the channel during the linger opens its
publication on the endpoint that is still open. A client that removes and adds a
publication does not meet the window. An add meets it only when it arrives after the
linger has ended and before the sender has dropped the socket.

## Decision

The add is answered with code 4, the bind it tried, and the message says what
happened and what to do. When the operating system refuses the socket of an `add
publication`, or of a replay's publication, and the conductor holds a closing send
endpoint on the same resolved channel, the message names the channel and the address,
says that the channel's previous send endpoint is still closing with the sender after
its last publication's linger, and says to send the command again after a moment.
Nothing is registered and nothing is held. A refusal with no closing endpoint on the
channel keeps its message, which points at the address and the port.
`spec/client-protocol.md` already answers a socket that cannot be opened, bound or
connected with code 4, so the page does not change.

## Alternatives considered

- **Hold the add until the sender's close, as ADR 0049 holds a subscription's.** Lost:
  ADR 0049's hold exists because removing and adding a subscription meets the window
  every time. A publication meets it only at the end of a linger. The hold would need
  a list bounded by the publications registry, three counters, a `doctor` check, a
  bound of its own, and a second hold for a replay, whose publication is created from
  the replayer's reply and not from the command. That is a large mechanism for a window
  of one sender duty cycle that no ordinary sequence meets.
- **Bind the new socket beside the closing one with `SO_REUSEADDR` or `SO_REUSEPORT`.**
  Lost: on Linux, two UDP sockets bound to one address with `SO_REUSEPORT` share the
  datagrams that arrive, so the closing socket could take status messages and NAKs
  meant for the new one. The options also behave differently on Linux and macOS.
- **Make the closing endpoint active again.** Lost: the remove may already be with the
  sender, which drops the socket on its next cycle. Taking it back needs a request and
  a reply of its own, and the two race.
- **Refuse before the bind whenever a closing endpoint holds the channel.** Lost: a
  channel with an endpoint and no interface port binds an ephemeral port, which
  succeeds beside the closing socket today. Refusing it would add a failure.

## Evidence

None measured: a message for a case the code already refused. The conductor's test
`an_add_publication_on_a_closing_send_endpoint_is_code_4_and_says_to_retry`: an add
during the last publication's linger opens its publication on the endpoint that is
still active; an add once the linger has ended and before the sender's close is code
4, its message saying the endpoint is closing and to add the publication again, with
nothing registered; and the same add after the sender's close binds the control
address again and is answered `publication ready`.

## Consequences

Easier: the message for this code 4 names the cause, which is the driver's own timing,
and the next step, which is to send the command again.

Harder: a client that adds a publication on a control channel just as the last
publication's linger ends gets code 4 once and must send the add again; the client API
does not do this for it.

Revisit: when a client, or the cluster's log publication (ADR 0052, feature 3), meets
this code 4 in an ordinary sequence, for a hold as ADR 0049's; when the sender takes
longer than one duty cycle to close an endpoint on a host that keeps up.

## References

- `spec/client-protocol.md`, "Commands", add publication and start replay, "Error
  codes"; `spec/udp.md`, "The receiver", the endpoints; ADR 0041, ADR 0043, ADR 0049,
  ADR 0053, ADR 0054
- `crates/cavalry-driver/src/conductor.rs`, `open_send_endpoint`,
  `release_send_endpoint`, `take_sender_replies`
- The review of pull request 152, 2026-11-06, blocker 4; AppShapes Trello card 225
