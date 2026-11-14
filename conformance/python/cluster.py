#!/usr/bin/env python3
"""The Python client's cluster round (spec/c-abi.md, versions 4 and 5; ADR 0122, ADR 0123,
ADR 0124): `cavalry.Client.cluster_service` and `cavalry.ClusterService` beside each of
three cluster members that `conformance/python/run.sh` started with `cavalry run`, and
`cavalry.Client.connect_cluster` and `cavalry.ClusterSession` on the runner's first
driver, the one at CAVALRY_DIR.

    cluster.py <cluster directory> <members string> <response channel>

The members' drivers are <cluster directory>/member-0, -1 and -2. The round is the C
runner's, `conformance/c/cluster.c`, through the client, and then the client's own edges.

The C runner's checks: a reply from outside the handler is refused; each service reads an
entry before the session connects, since a service answers no entry that its member held
when the service first opened the member's log; one message is handed to all three
services as one entry, with one position, term, time and session; one service answers it,
the one beside the leader, and the two others are answered
CAVALRY_CLUSTER_REPLY_NOT_ANSWERING; the session reads one reply, that service's, with
the entry's term and the cluster's time; each service holds one session, and its position
is past the entry.

The client's own edges: what `offer` refuses; the calls the ABI refuses from inside a
service's handler, on the service and on the client its poll holds, and from inside the
session's, each made after the thread's error was set to another code, so that a refusal
that answers what an open handle answers is told from an answer; a service's handler that
raises after it replied, and one that returns what is no action, whose entry is aborted,
handed over again by the next poll and answered once; a reply too long for a frame and a
reply to a session that is not open; a handler that returns BREAK with two entries
waiting; a session's handler that raises, whose reply is not handed over again; the
session's close as an entry every service is handed; a session and a service closed
twice, and a service closed after its client.

Then a report, printed and never gated: how many polls the three services made, how many
took over a millisecond, and the longest, which is how long a poll held the GIL
(clients/python/README.md, "The call").

Every wait is bounded at 60 s, the time a cluster may take to commit on a busy host. It
exits 1 with what it found when a check fails. Python 3.9 standard library only (ADR
0003).
"""

import ctypes
import os
import sys
import time

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "clients", "python"))
import cavalry  # noqa: E402
from cavalry import _abi  # noqa: E402

MEMBERS = 3
WAIT_SECONDS = 60.0
IDLE_SECONDS = 0.0002  # between duty cycles: the members run in processes of their own
SETTLE_SECONDS = 0.3   # the time in which a second reply, or a second entry, would come
MESSAGE = _abi.CAVALRY_CLUSTER_ENTRY_SESSION_MESSAGE
SENT = _abi.CAVALRY_CLUSTER_REPLY_SENT
POLLING = _abi.CAVALRY_ERR_POLLING


def fail(what):
    print(f"conformance/python: cluster round: {what}", file=sys.stderr)
    sys.exit(1)


def set_another_error(abi):
    """Sets the thread's error to one no refusal gives, CAVALRY_ERR_NULL, by asking the ABI
    for the position of a null header, as the committed log round does: a success clears
    nothing (spec/c-abi.md, "Error codes"), so a refusal that answers what an open handle
    answers is told from an answer by the code alone, which must have changed from this
    one to CAVALRY_ERR_POLLING."""
    if abi.cavalry_committed_log_entry_position(None) != -1 or abi.cavalry_errcode() != _abi.CAVALRY_ERR_NULL:
        fail(f"the position of a null header did not answer -1 with CAVALRY_ERR_NULL: error {abi.cavalry_errcode()}")


def refusals_of(abi, calls):
    """What each call answers, with the thread's error after it: a `CavalryError` is -1
    with its code."""
    out = []
    for name, call in calls:
        set_another_error(abi)
        try:
            out.append((name, call(), abi.cavalry_errcode()))
        except cavalry.CavalryError as error:
            out.append((name, -1, error.code))
    return out


class Served:
    """A service beside one member: what its handler was handed and what its calls
    answered. The handler answers a session's message with its member's id, a colon and
    the message, so that the reply a client reads says whose service sent it."""

    def __init__(self, abi, cluster, member):
        self.abi = abi
        self.member = member
        self.client = cavalry.Client(os.path.join(cluster, f"member-{member}"))
        self.service = self.client.cluster_service()
        self.entries = []      # every entry taken: (type, position, term, time, session, leader, timed out)
        self.messages = {}     # a message's payload -> the entries it was taken as
        self.handed = {}       # a message's payload -> the positions it was handed over at, taken or not
        self.codes = {}        # a message's payload -> what each reply to it answered
        self.refusals = None   # what the calls from inside the handler answered
        self.raise_on = None   # a payload whose first handling replies and then raises
        self.no_action_on = None   # a payload whose first handling returns what is no action
        self.edges_on = None   # a payload before whose reply a reply too long and one to no session are made
        self.edges = None
        self.break_on = ()     # payloads whose entries are taken with BREAK
        self.polls = 0         # the report: every poll, those over a millisecond, and the longest
        self.over_a_millisecond = 0
        self.longest = (0, "no poll")

    def on_entry(self, body, entry, service):
        if service is not self.service:
            fail("the handler was handed another object than the service that was polled")
        values = (entry.type, entry.position, entry.leadership_term_id, entry.timestamp, entry.session_id,
                  entry.leader_member_id, entry.timed_out)
        if entry.type != MESSAGE or entry.session_id < 0:
            if len(body) != 0:
                fail(f"an entry that is no session's message has a payload of {len(body)} bytes: {values}")
            self.entries.append(values)
            return None
        payload = bytes(body)
        handed = self.handed.setdefault(payload, [])
        handed.append(entry.position)
        if self.no_action_on == payload and len(handed) == 1:
            return 3   # Aeron's COMMIT, which Cavalry has no action for
        if self.edges_on == payload and self.edges is None:
            too_long = service.reply(entry.session_id, bytes(1345))
            self.edges = [(too_long, self.abi.cavalry_errcode()), (service.reply(entry.session_id + 1000, b"x"), None)]
        # Reply first, then change the state: an entry that is aborted is handed over again.
        code = service.reply(entry.session_id, b"%d:" % self.member + payload)
        if code < 0:
            fail(f"a reply from inside the handler answered {code} with error {self.abi.cavalry_errcode()}")
        if code == _abi.CAVALRY_CLUSTER_REPLY_RETRY:
            handed.pop()
            return cavalry.ABORT
        if self.raise_on == payload and len(handed) == 1:
            self.codes.setdefault(payload, []).append(code)
            raise ValueError("the handler's own")
        if self.refusals is None:
            client = self.client
            self.refusals = refusals_of(self.abi, (
                ("poll", lambda: service.poll(self.on_entry, 1)), ("position", service.position),
                ("answers", service.answers), ("sessions", service.sessions), ("fault", service.fault),
                ("close", service.close), ("client.is_closed", lambda: client.is_closed),
                ("client.cluster_service", client.cluster_service),
                ("client.add_subscription", lambda: client.add_subscription("cavalry:ipc", 1229))))
        self.codes.setdefault(payload, []).append(code)
        self.messages.setdefault(payload, []).append(values)
        self.entries.append(values)
        return cavalry.BREAK if payload in self.break_on else cavalry.CONTINUE

    def poll(self, limit=16):
        """One poll, timed: the count of entries taken. A handler's failure is the
        caller's to catch."""
        before = len(self.entries)
        started = time.perf_counter_ns()
        try:
            count = self.service.poll(self.on_entry, limit)
        finally:
            took = time.perf_counter_ns() - started
            self.polls += 1
            self.over_a_millisecond += took > 1_000_000
            if took > self.longest[0]:
                self.longest = (took, f"a poll of the service beside member {self.member} that began after "
                                      f"{before} entries and took {len(self.entries) - before}")
        if count < 0:
            fail(f"the poll of the service beside member {self.member} stopped on fault {self.service.fault()}")
        return count


class Answered:
    """What the session's handler was handed."""

    def __init__(self, abi, session):
        self.abi = abi
        self.session = session
        self.replies = []      # (payload, leadership term id, timestamp)
        self.refusals = None
        self.raise_on = None   # a reply on which the handler raises

    def on_reply(self, body, reply):
        payload = bytes(body)
        self.replies.append((payload, reply.leadership_term_id, reply.timestamp))
        if self.refusals is None:
            session = self.session
            self.refusals = refusals_of(self.abi, (
                ("offer", lambda: session.offer(b"x")), ("poll", lambda: session.poll(self.on_reply, 1)),
                ("leadership_term_id", session.leadership_term_id), ("leader_member_id", session.leader_member_id),
                ("stale_replies", session.stale_replies), ("closed", session.closed), ("fault", session.fault),
                ("close", session.close)))
        if self.raise_on is not None and payload.endswith(self.raise_on):
            raise ValueError("the reply handler's own")

    def to(self, message):
        return [reply for reply in self.replies if reply[0][2:] == message]


def cycle(served, answered=None, skip=None):
    """One duty cycle: every service's poll but `skip`'s, the session's when there is one,
    and a short sleep."""
    for s in served:
        if s is not skip:
            s.poll()
    if answered is not None and answered.session.poll(answered.on_reply, 16) < 0:
        fail(f"the session's poll stopped on fault {answered.session.fault()}")
    time.sleep(IDLE_SECONDS)


def wait(what, done, served, answered=None, skip=None):
    until = time.monotonic() + WAIT_SECONDS
    while not done():
        if time.monotonic() > until:
            fail(f"{what}: not within {WAIT_SECONDS:.0f} s; the services took "
                 f"{[len(s.entries) for s in served]} entries and the session read "
                 f"{len(answered.replies) if answered is not None else 'no'} replies")
        cycle(served, answered, skip)


def settle(served, answered=None, skip=None):
    until = time.monotonic() + SETTLE_SECONDS
    while time.monotonic() < until:
        cycle(served, answered, skip)


def offer(session, message, served, answered):
    until = time.monotonic() + WAIT_SECONDS
    while True:
        offered = session.offer(message)
        if offered >= 0:
            return
        if offered == cavalry.ERROR:
            fail(f"the offer of {message!r} failed with error {cavalry.library().cavalry_errcode()}")
        if offered == cavalry.CLOSED:
            fail(f"the session closed before {message!r}: {session.closed()}")
        if time.monotonic() > until:
            fail(f"the session's publication did not take {message!r} within {WAIT_SECONDS:.0f} s: {offered}")
        cycle(served, answered)


def handler_error(leader, served, answered, what):
    """The `HandlerError` of the leader's service's next poll that raises one, with the
    other services and the session polled meanwhile."""
    until = time.monotonic() + WAIT_SECONDS
    while True:
        try:
            leader.poll()
        except cavalry.HandlerError as error:
            return error
        if time.monotonic() > until:
            fail(f"{what}: no poll of the leader's service raised HandlerError within {WAIT_SECONDS:.0f} s")
        cycle(served, answered, skip=leader)


def main():
    if len(sys.argv) != 4:
        fail("usage: cluster.py <cluster directory> <members string> <response channel>")
    cluster, members, response_channel = sys.argv[1:]
    abi = cavalry.library()

    # 1. A service beside each member, over a client on that member's driver.
    served = [Served(abi, cluster, member) for member in range(MEMBERS)]
    # The handler's reply is for the handler: from here there is no entry.
    if served[0].service.reply(1, b"x") != -1 or abi.cavalry_errcode() != _abi.CAVALRY_ERR_CLUSTER_NO_ENTRY:
        fail(f"a reply from outside the handler was not refused with CAVALRY_ERR_CLUSTER_NO_ENTRY: error "
             f"{abi.cavalry_errcode()}")

    # 2. Each service opens its member's log and reads the term's first entry before the
    # session connects: a service answers no entry that its member held when the service
    # first opened the log (spec/cluster.md, "Replies").
    wait("each service reads an entry of its member's log", lambda: all(s.entries for s in served), served)

    # 3. A session with the cluster, on the driver at CAVALRY_DIR. The connect returns once
    # the leader has opened the session.
    client = cavalry.Client()
    session = client.connect_cluster(members, response_channel)
    answered = Answered(abi, session)
    if session.closed() is not None or session.session_id < 0 or session.max_message_length <= 0:
        fail(f"a session just opened reads closed {session.closed()!r}, id {session.session_id} and a longest "
             f"message of {session.max_message_length} bytes")
    # What `offer` refuses: anything that is not bytes, before the ABI is called, and a
    # message longer than one frame carries.
    try:
        session.offer("hello")
        fail("the offer of a str was not refused")
    except ctypes.ArgumentError:
        pass
    if session.offer(bytes(session.max_message_length + 1)) != cavalry.ERROR \
            or abi.cavalry_errcode() != _abi.CAVALRY_ERR_TOO_LONG:
        fail(f"a message of {session.max_message_length + 1} bytes was not refused with ERROR and "
             f"CAVALRY_ERR_TOO_LONG: error {abi.cavalry_errcode()}")

    # 4. One message, taken by every service and answered by one.
    offer(session, b"hello", served, answered)
    wait("the message is taken by three services and answered",
         lambda: answered.replies and all(b"hello" in s.messages for s in served), served, answered)
    settle(served, answered)
    if len(answered.replies) != 1:
        fail(f"the session read {len(answered.replies)} replies to one message: {answered.replies}")

    # 5. The same entry beside every member, answered by the leader's service alone.
    leader = None
    first = served[0].messages[b"hello"][0]
    for s in served:
        if s.messages[b"hello"] != [first] or first[4] != session.session_id:
            fail(f"the service beside member {s.member} took the message as {s.messages[b'hello']}, and the service "
                 f"beside member 0 as {first}, of session {session.session_id}")
        if s.service.sessions() != 1:
            fail(f"the service beside member {s.member} holds {s.service.sessions()} sessions open, not one")
        if s.service.position() <= first[1]:
            fail(f"the service beside member {s.member} is at {s.service.position()}, not past the entry at {first[1]}")
        if s.service.fault()[0] != 0:
            fail(f"the service beside member {s.member} reads fault {s.service.fault()} after polls that took entries")
        expected = [("poll", -1, POLLING), ("position", -1, POLLING), ("answers", False, POLLING),
                    ("sessions", -1, POLLING), ("fault", -1, POLLING), ("close", -1, POLLING),
                    ("client.is_closed", False, POLLING), ("client.cluster_service", -1, POLLING),
                    ("client.add_subscription", -1, POLLING)]
        if s.refusals != expected:
            fail(f"the calls from inside the handler of the service beside member {s.member} answered "
                 f"{s.refusals}, not {expected}")
        if s.codes[b"hello"] == [SENT]:
            if leader is not None:
                fail(f"the services beside members {leader.member} and {s.member} both answered one entry")
            if not s.service.answers():
                fail(f"the service beside member {s.member} answered and says it does not answer")
            leader = s
        elif s.codes[b"hello"] != [_abi.CAVALRY_CLUSTER_REPLY_NOT_ANSWERING] or s.service.answers():
            fail(f"the service beside member {s.member}, a follower, answered {s.codes[b'hello']} and says it "
                 f"answers: {s.service.answers()}")
    if leader is None:
        fail("no service answered the entry")
    if answered.replies != [(b"%d:hello" % leader.member, first[2], first[3])]:
        fail(f"the reply {answered.replies} is not the leader's service's, of member {leader.member}, with the "
             f"entry's term {first[2]} and the cluster's time {first[3]}")
    expected = [("offer", cavalry.ERROR, POLLING), ("poll", -1, POLLING), ("leadership_term_id", -1, POLLING),
                ("leader_member_id", -1, POLLING), ("stale_replies", -1, POLLING), ("closed", None, POLLING),
                ("fault", -1, POLLING), ("close", -1, POLLING)]
    if answered.refusals != expected:
        fail(f"the calls from inside the session's handler answered {answered.refusals}, not {expected}")
    if (session.leader_member_id(), session.leadership_term_id()) != (leader.member, first[2]) \
            or session.closed() is not None or session.fault()[0] != 0:
        fail(f"the session sends to member {session.leader_member_id()} in term {session.leadership_term_id()}, "
             f"reads closed {session.closed()!r} and fault {session.fault()}; the entry's leader is member "
             f"{leader.member} and its term {first[2]}")
    terms = [e for e in leader.entries if e[0] == _abi.CAVALRY_CLUSTER_ENTRY_NEW_LEADERSHIP_TERM]
    opens = [e for e in leader.entries if e[0] == _abi.CAVALRY_CLUSTER_ENTRY_SESSION_OPEN]
    if not terms or terms[-1][5] != leader.member or [e[4] for e in opens] != [session.session_id]:
        fail(f"the leader's service read the new leadership term events {terms} and the session open events "
             f"{opens}; the leader is member {leader.member} and the session {session.session_id}")
    print(f"conformance/python: cluster round: session {session.session_id}'s message, the entry at position "
          f"{first[1]} of term {first[2]}, taken by three services and answered once, by the service beside member "
          f"{leader.member}; nine calls from inside a service's handler and eight from inside the session's refused "
          f"with CAVALRY_ERR_POLLING")

    # 6. A handler that replies and then raises: its entry is aborted, so the poll ends at
    # it with the entry not taken, `poll` raises, and the next poll hands the entry over
    # again; the reply the first handling sent is not sent again.
    leader.raise_on = b"raise"
    before = leader.service.position()
    offer(session, b"raise", served, answered)
    raised = handler_error(leader, served, answered, "a handler that raises")
    if raised.cluster != "service" or raised.raised != 1 or not isinstance(raised.__cause__, ValueError) \
            or raised.fragments < 0 or "Its entry was aborted" not in str(raised):
        fail(f"a service's HandlerError has cluster {raised.cluster!r}, fragments {raised.fragments}, raised "
             f"{raised.raised} and cause {raised.__cause__!r}, and says: {raised}")
    aborted = leader.handed[b"raise"][0]
    if not before <= leader.service.position() <= aborted or b"raise" in leader.messages:
        fail(f"after its handler raised on the entry at {aborted} the service is at {leader.service.position()}, "
             f"from {before}, and took {leader.messages.get(b'raise')}")
    wait("the aborted entry is handed over again and its one reply read",
         lambda: answered.to(b"raise") and all(b"raise" in s.messages for s in served), served, answered)
    settle(served, answered)
    if leader.handed[b"raise"] != [aborted, aborted] or leader.codes[b"raise"] != [SENT, SENT] \
            or len(answered.to(b"raise")) != 1:
        fail(f"the entry whose handler raised was handed over at {leader.handed[b'raise']}, its replies answered "
             f"{leader.codes[b'raise']}, and the session read {answered.to(b'raise')}")

    # 7. A handler that returns what is no action, as a raise: aborted, raised by `poll`
    # with the ABI's code, and handed over again. On the second handling, before the
    # reply: a reply too long for a frame, and a reply to a session that is not open.
    leader.no_action_on = leader.edges_on = b"no action"
    offer(session, b"no action", served, answered)
    raised = handler_error(leader, served, answered, "a handler that returns what is no action")
    cause = raised.__cause__
    if raised.cluster != "service" or not isinstance(cause, cavalry.CavalryError) \
            or cause.code != _abi.CAVALRY_ERR_CLUSTER_ACTION or "returned 3" not in cause.message:
        fail(f"a handler that returned 3 raised {raised!r} from {cause!r}")
    wait("the entry with no action is handed over again and answered",
         lambda: answered.to(b"no action") and all(b"no action" in s.messages for s in served), served, answered)
    settle(served, answered)
    handed = leader.handed[b"no action"]
    if len(handed) != 2 or handed[0] != handed[1] or len(answered.to(b"no action")) != 1:
        fail(f"the entry whose handler returned no action was handed over at {handed} and the session read "
             f"{answered.to(b'no action')}")
    expected = [(_abi.CAVALRY_CLUSTER_REPLY_TOO_LONG, _abi.CAVALRY_ERR_TOO_LONG),
                (_abi.CAVALRY_CLUSTER_REPLY_NO_SESSION, None)]
    if leader.edges != expected or leader.codes[b"no action"] != [SENT]:
        fail(f"a reply of 1,345 bytes and a reply to session {session.session_id + 1000} answered {leader.edges}, "
             f"not {expected}, and the reply after them {leader.codes[b'no action']}")

    # 8. BREAK: with two entries waiting, a poll takes one and ends. The leader's service
    # is not polled until the two others have taken both entries and a moment more, so
    # that its member has committed both.
    leader.break_on = (b"break 1", b"break 2")
    offer(session, b"break 1", served, answered)
    offer(session, b"break 2", served, answered)
    wait("the followers' services take two more entries",
         lambda: all(b"break 2" in s.messages for s in served if s is not leader), served, answered, skip=leader)
    settle(served, answered, skip=leader)
    counts = []
    until = time.monotonic() + WAIT_SECONDS
    while b"break 2" not in leader.messages:
        if time.monotonic() > until:
            fail(f"the leader's service took {counts} entries in its polls and not both entries")
        count = leader.poll()
        if count:
            counts.append(count)
        else:
            cycle(served, answered, skip=leader)
    if counts != [1, 1]:
        fail(f"with BREAK returned for each of two entries the leader's polls took {counts}, not one each")
    wait("the two replies are read", lambda: answered.to(b"break 1") and answered.to(b"break 2"), served, answered)
    if [reply[0][2:] for reply in answered.replies[-2:]] != [b"break 1", b"break 2"]:
        fail(f"the replies to two messages came out of order: {answered.replies[-2:]}")

    # 9. The session's handler raises: the reply was handed over, `poll` raises once the
    # ABI has returned, and the reply is not handed over again.
    answered.raise_on = b"reply raises"
    offer(session, b"reply raises", served, answered)
    raised = None
    until = time.monotonic() + WAIT_SECONDS
    while raised is None:
        if time.monotonic() > until:
            fail("the reply on which the session's handler raises was not read")
        try:
            cycle(served, answered)
        except cavalry.HandlerError as error:
            raised = error
    answered.raise_on = None
    if raised.cluster != "session" or (raised.fragments, raised.raised) != (1, 1) \
            or not isinstance(raised.__cause__, ValueError) or "reply handler raised on 1 of the 1 reply" not in str(raised):
        fail(f"a session's HandlerError has cluster {raised.cluster!r}, fragments {raised.fragments}, raised "
             f"{raised.raised} and cause {raised.__cause__!r}, and says: {raised}")
    settle(served, answered)
    if len(answered.to(b"reply raises")) != 1 or session.stale_replies() != 0:
        fail(f"the reply whose handler raised was read {len(answered.to(b'reply raises'))} times, and the session "
             f"passed over {session.stale_replies()} stale replies")
    print("conformance/python: cluster round: a handler that raised and one that returned no action had their "
          "entries aborted, handed over again and answered once; a reply of 1,345 bytes and a reply to no session "
          "refused; BREAK took one of two entries; a reply whose handler raised is not handed over again")

    # 10. The session before its client. Its close is an entry every service is handed,
    # after which the log holds no session open.
    session.close()
    session.close()  # idempotent
    if session.closed() is not None:
        fail(f"a session that close() freed reads closed {session.closed()!r}")
    client.close()
    closes = lambda s: [e for e in s.entries if e[0] == _abi.CAVALRY_CLUSTER_ENTRY_SESSION_CLOSE]  # noqa: E731
    wait("each service reads the session's close", lambda: all(closes(s) for s in served), served)
    for s in served:
        if [(e[4], e[6]) for e in closes(s)] != [(first[4], False)] or s.service.sessions() != 0:
            fail(f"the service beside member {s.member} read the session close events {closes(s)} and holds "
                 f"{s.service.sessions()} sessions open; the session {first[4]} was closed by its client")
    if any(s.entries != served[0].entries for s in served):
        fail(f"the three services took different entries: {[s.entries for s in served]}")

    # 11. Each service before its own client, twice; and the last after its client.
    for s in served[:-1]:
        s.service.close()
        s.service.close()  # idempotent
        s.client.close()
    served[-1].client.close()
    served[-1].service.close()
    longest = max(s.longest for s in served)
    polls, over = sum(s.polls for s in served), sum(s.over_a_millisecond for s in served)
    print(f"conformance/python: cluster round: {len(served[0].entries)} entries taken by each of three services; "
          f"the session's close read by each; a service closed after its client")
    print(f"conformance/python: cluster round: report: {polls:,} polls of the three services, each with the GIL "
          f"held for its run: {over} took over 1 ms, and the longest {longest[0] / 1000:,.0f} us, {longest[1]} "
          f"(not gated)")
    print("conformance/python: cluster round ok")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except cavalry.CavalryError as error:
        fail(f"{error.function} failed with error {error.code}: {error.message}")
    except cavalry.HandlerError as error:
        fail(f"the handler raised: {error.__cause__!r}")
