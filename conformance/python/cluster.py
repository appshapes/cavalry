#!/usr/bin/env python3
"""The Python client's cluster round (spec/c-abi.md, versions 4, 5 and 7; ADR 0122, ADR
0123, ADR 0124, ADR 0157): `cavalry.Client.cluster_service` and `cavalry.ClusterService` beside each of
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
raises after it replied, and one that returns what is no action, True and then 3, whose
entry is aborted each time, handed over again by the next poll and answered once; a reply
too long for a frame and a
reply to a session that is not open; a handler that returns BREAK with two entries
waiting; a session's handler that raises, whose reply is not handed over again; the
program's timers, `schedule_timer` and `cancel_timer` from inside the handler, as the C
runner's round has one: a timer handed to every service once, as an entry of its own
type with the timer's id, at one position and no earlier than the deadline its second
schedule set, where its first was an hour away, and not again within a timer refresh
interval and more; a timer scheduled
and cancelled in one handling handed to none; the least and the greatest int64 as ids,
through ctypes, handed back with those ids; both calls refused from outside the handler, and an id or a deadline
that is no int64 refused before the ABI is called; the
session's close as an entry every service is handed; a session and a service closed
twice, and a service closed after its client.

Then a measurement, printed and gated only for what must hold: 200 messages, one at a
time, at each of which every service's program schedules one more timer, due at once.
Each is handed to every service once, at one position and no earlier than its message.
The round prints how long after its message each was stamped by the leader, in the
milliseconds of the cluster's time, as a histogram, and how long the leader's service
waited from the schedule to the timer, by this process's clock, beside what the run's
first timers took (work item 462; ADR 0168).

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
TIMER = _abi.CAVALRY_CLUSTER_ENTRY_TIMER
TIMER_MESSAGE = b"timer"   # the message at which each service's program schedules its timers
TIMER_ID = 77              # the timer that is handed over
CANCELLED_ID = 78          # the timer that is scheduled and cancelled in the same handling
# How long after the message's entry the timer is due. A second, and not the 50 ms this
# round had first: the event of a timer that is due at once is stamped tens of
# milliseconds after its message, the time an entry takes to be committed and read by a
# service on a laptop, and more on a busy host, so the event of a timer that was wrongly
# due at once is stamped about as late as a deadline of 50 ms, and that margin would let
# it pass. Before ADR 0168 a service's first timer waited for its publication to its
# member too, 49 ms in one run of this round. The round prints what it was in its own
# run, for its first timers and for 200 later ones.
TIMER_AFTER_MS = 1000
HOUR_MS = 3_600_000        # the deadline its first schedule gives it, which the second replaces
LATER_MESSAGE = b"later"   # the prefix of a message at which each program schedules one more timer, due at once
LATER = 200                # how many of them the round sends, one at a time
LATER_ID = 1000            # the id of the first of their timers; the others count on from it
INT64_MIN, INT64_MAX = -(2 ** 63), 2 ** 63 - 1
REFRESH_SECONDS = 1.2      # a timer refresh interval of spec/cluster.md, "Values", 1 s, and more
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
        self.no_action_on = None   # a payload whose first two handlings return what is no action
        self.edges_on = None   # a payload before whose reply a reply too long and one to no session are made
        self.edges = None
        self.break_on = ()     # payloads whose entries are taken with BREAK
        self.timers = []       # every timer handed over: (position, correlation id, the event's time)
        self.timer_at = None   # when the last was handed over, by this process's monotonic clock
        self.timer_codes = []  # what the schedules and the cancel at TIMER_MESSAGE answered
        self.first_at = None   # when the program scheduled at TIMER_MESSAGE, by this process's clock, in ns
        self.first_took = None  # from then to the first timer it was handed, in ns
        self.later_scheduled = {}  # a later timer's id -> (its message's time, when scheduled in ns, the answer)
        self.later = {}        # a later timer's id -> (its position, its event's time, when handed over in ns)
        self.refused_inside = "no call"  # what a schedule of an id above int64 from inside the handler did
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
            if entry.type == TIMER and LATER_ID <= entry.correlation_id < LATER_ID + LATER:
                if entry.correlation_id in self.later:
                    fail(f"the service beside member {self.member} was handed timer {entry.correlation_id} a "
                         f"second time, at position {entry.position}")
                self.later[entry.correlation_id] = (entry.position, entry.timestamp, time.perf_counter_ns())
                self.timer_at = time.monotonic()
            elif entry.type == TIMER:
                if self.first_took is None and self.first_at is not None:
                    self.first_took = time.perf_counter_ns() - self.first_at
                self.timers.append((entry.position, entry.correlation_id, entry.timestamp))
                self.timer_at = time.monotonic()
            elif entry.correlation_id != 0:
                fail(f"an entry that is no timer has the correlation id {entry.correlation_id}: {values}")
            self.entries.append(values)
            return None
        if entry.correlation_id != 0:
            fail(f"a session's message has the correlation id {entry.correlation_id}: {values}")
        payload = bytes(body)
        handed = self.handed.setdefault(payload, [])
        handed.append(entry.position)
        if self.no_action_on == payload and len(handed) <= 2:
            # First True, as a call-back that says "handled" returns, which equals 1,
            # the value of ABORT, and is no action; then 3, Aeron's COMMIT, which
            # Cavalry has no action for.
            return True if len(handed) == 1 else 3
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
        if payload == TIMER_MESSAGE:
            # The program's state changes at the entry it takes. One timer, scheduled for
            # an hour later and then again for a little later: were the second schedule
            # not to set the one timer's deadline, the round's wait would end before the
            # timer came. A second timer, due at once, cancelled again: were the cancel
            # to miss, it would be handed over. And the least and the greatest int64 as
            # ids, due at once, which each service must be handed back as they are.
            # Before them, an id above int64, which is refused here as it is outside
            # the handler, and schedules nothing. Every member's service makes the same
            # calls at the same entry.
            try:
                self.refused_inside = service.schedule_timer(INT64_MAX + 1, entry.timestamp)
            except OverflowError as refused:
                self.refused_inside = refused
            self.timer_codes = [service.schedule_timer(TIMER_ID, entry.timestamp + HOUR_MS),
                                service.schedule_timer(TIMER_ID, entry.timestamp + TIMER_AFTER_MS),
                                service.schedule_timer(CANCELLED_ID, entry.timestamp),
                                service.cancel_timer(CANCELLED_ID),
                                service.schedule_timer(INT64_MIN, entry.timestamp),
                                service.schedule_timer(INT64_MAX, entry.timestamp)]
            self.first_at = time.perf_counter_ns()
        if payload.startswith(LATER_MESSAGE):
            # One more timer, due at once, at a message of its own: what a timer costs
            # once a service has scheduled its first.
            timer_id = LATER_ID + int(payload[len(LATER_MESSAGE):])
            answer = service.schedule_timer(timer_id, entry.timestamp)
            self.later_scheduled[timer_id] = (entry.timestamp, time.perf_counter_ns(), answer)
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
    # with the ABI's code, and handed over again. Twice: True first, which equals ABORT's
    # 1 and would abort the entry at every poll with nothing raised if the client passed
    # it on, and then 3. On the third handling, before the reply: a reply too long for a
    # frame, and a reply to a session that is not open.
    leader.no_action_on = leader.edges_on = b"no action"
    offer(session, b"no action", served, answered)
    for returned in ("True", "3"):
        raised = handler_error(leader, served, answered, f"a handler that returns {returned}, which is no action")
        cause = raised.__cause__
        if raised.cluster != "service" or not isinstance(cause, cavalry.CavalryError) \
                or cause.code != _abi.CAVALRY_ERR_CLUSTER_ACTION or f"returned {returned}," not in cause.message:
            fail(f"a handler that returned {returned} raised {raised!r} from {cause!r}")
    wait("the entry with no action is handed over again and answered",
         lambda: answered.to(b"no action") and all(b"no action" in s.messages for s in served), served, answered)
    settle(served, answered)
    handed = leader.handed[b"no action"]
    if len(handed) != 3 or len(set(handed)) != 1 or len(answered.to(b"no action")) != 1:
        fail(f"the entry whose handler returned no action twice was handed over at {handed} and the session read "
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

    # 9b. The program's timers (spec/c-abi.md, version 7; spec/cluster.md, "The service",
    # "Timers"; ADR 0157). Each service's program schedules four timers at one message and
    # cancels one: the three are handed to every service once each, as entries of their
    # own type with the timers' ids, at one position each and none earlier than its
    # deadline, and the cancelled one to none. Step 10 holds the "once" for a timer
    # refresh interval and more. An argument that is no int64 never
    # reaches the ABI: the thread's error is as it was set.
    for what, call, error in (
            ("an id above int64", lambda: served[0].service.schedule_timer(INT64_MAX + 1, 1), OverflowError),
            ("a deadline below int64", lambda: served[0].service.schedule_timer(1, INT64_MIN - 1), OverflowError),
            ("a float as a deadline", lambda: served[0].service.schedule_timer(1, 1.5), TypeError),
            ("an id above int64 to cancel", lambda: served[0].service.cancel_timer(2 ** 64 + 5), OverflowError)):
        set_another_error(abi)
        before = abi.cavalry_errcode()
        try:
            answered_with = call()
        except error:
            answered_with = error
        if answered_with is not error or abi.cavalry_errcode() != before:
            fail(f"{what} was not refused with {error.__name__} before the ABI was called: it answered "
                 f"{answered_with!r}, and the thread's error went from {before} to {abi.cavalry_errcode()}")
    for function, call in (("schedule_timer", lambda: served[0].service.schedule_timer(1, 1)),
                           ("cancel_timer", lambda: served[0].service.cancel_timer(1))):
        set_another_error(abi)
        if call() != -1 or abi.cavalry_errcode() != _abi.CAVALRY_ERR_CLUSTER_NO_ENTRY:
            fail(f"{function} from outside the handler was not refused with CAVALRY_ERR_CLUSTER_NO_ENTRY: "
                 f"error {abi.cavalry_errcode()}")
    offer(session, TIMER_MESSAGE, served, answered)
    expected = sorted([INT64_MIN, INT64_MAX, TIMER_ID])
    wait("each service is handed its three timers", lambda: all(len(s.timers) >= len(expected) for s in served),
         served, answered)
    settle(served, answered)
    scheduled = served[0].messages[TIMER_MESSAGE][0]
    SCHEDULED = _abi.CAVALRY_CLUSTER_TIMER_SCHEDULED
    for s in served:
        if s.timer_codes != [SCHEDULED, SCHEDULED, SCHEDULED, 0, SCHEDULED, SCHEDULED]:
            fail(f"the service beside member {s.member} was answered {s.timer_codes} for its schedules and its "
                 f"cancel; expected CAVALRY_CLUSTER_TIMER_SCHEDULED, {SCHEDULED}, for each schedule and 0 for the "
                 f"cancel")
        if not isinstance(s.refused_inside, OverflowError):
            fail(f"the service beside member {s.member} scheduled an id above int64 from inside its handler and "
                 f"was answered {s.refused_inside!r}; expected an OverflowError before the ABI was called")
        if sorted(t[1] for t in s.timers) != expected:
            fail(f"the service beside member {s.member} was handed the timers {s.timers}, each a position, an "
                 f"id and a time; expected timer {TIMER_ID}, the least int64 and the greatest once each, and "
                 f"timer {CANCELLED_ID}, which it cancelled, never")
        if s.timers != served[0].timers:
            fail(f"the service beside member {s.member} was handed its timers as {s.timers} and the service "
                 f"beside member {served[0].member} as {served[0].timers}; expected one position and one time "
                 f"for each")
        for position, correlation_id, timestamp in s.timers:
            after = TIMER_AFTER_MS if correlation_id == TIMER_ID else 0
            if timestamp < scheduled[3] + after or position <= scheduled[1]:
                fail(f"the service beside member {s.member} was handed timer {correlation_id} at position "
                     f"{position} and time {timestamp}; it was scheduled at position {scheduled[1]} and time "
                     f"{scheduled[3]} for {after} ms later")
    timer = next(t for t in served[0].timers if t[1] == TIMER_ID)
    # What a deadline that was ignored would have read: the time of the timers that were
    # due at once, which the member was told of with timer 77. The deadline check above
    # proves something for as long as this is below the deadline.
    at_once = max(t[2] for t in served[0].timers if t[1] != TIMER_ID) - scheduled[3]
    margin = "" if at_once < TIMER_AFTER_MS else ", so the deadline's check proved nothing in this run"
    print(f"conformance/python: cluster round: timer {TIMER_ID}, scheduled by each service at a message for an "
          f"hour later and then for {TIMER_AFTER_MS} ms later, handed to three services once, at position "
          f"{timer[0]}, {timer[2] - scheduled[3]} ms after the message; the least and the greatest int64 as "
          f"ids, due at once, handed to each once with those ids, {at_once} ms after the message{margin}; timer "
          f"{CANCELLED_ID}, due at once and cancelled in the same handling, handed to none; both calls refused "
          f"from outside the handler, and what is no int64 before the ABI is called, from inside the handler too")

    # 9c. Later timers: what a timer costs a service that has scheduled one already.
    # One message at a time, and the next when every service has been handed the
    # timer of the last, so that each is measured alone.
    for n in range(LATER):
        offer(session, LATER_MESSAGE + b"%03d" % n, served, answered)
        wait(f"each service is handed the timer of later message {n}",
             lambda n=n: all(LATER_ID + n in s.later for s in served), served, answered)
    ids = range(LATER_ID, LATER_ID + LATER)
    for s in served:
        answers = {s.later_scheduled[i][2] for i in ids}
        if answers != {SCHEDULED}:
            fail(f"the service beside member {s.member} was answered {answers} for its {LATER} later schedules; "
                 f"expected {SCHEDULED} for each")
        if [s.later[i][:2] for i in ids] != [served[0].later[i][:2] for i in ids]:
            fail(f"the service beside member {s.member} was handed its later timers at other positions or times "
                 f"than the service beside member {served[0].member}")
        early = [i for i in ids if s.later[i][1] < s.later_scheduled[i][0]]
        if early:
            fail(f"the service beside member {s.member} was handed the later timers {early} with a time before "
                 f"their messages'")
    stamped = sorted(served[0].later[i][1] - served[0].later_scheduled[i][0] for i in ids)
    counts = ", ".join(f"{ms} ms: {stamped.count(ms)}" for ms in sorted(set(stamped)))
    waited = sorted((leader.later[i][2] - leader.later_scheduled[i][1]) // 1000 for i in ids)
    quantile = lambda q: waited[min(len(waited) - 1, int(q * len(waited)))]  # noqa: E731
    print(f"conformance/python: cluster round: {LATER} later timers, each due at once at a message of its own and "
          f"handed to three services once: stamped by the leader after their message's time, {counts}; the "
          f"service beside the leader was handed each {quantile(0.5):,} us after it scheduled it at the p50, "
          f"{quantile(0.99):,} us at the p99 and {waited[-1]:,} us at the most, of {LATER}, where its first timers "
          f"of this run took {leader.first_took // 1000:,} us and were stamped {at_once} ms after their message "
          f"(not gated)")

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
    # The timers of step 9b, once each: a leader gives a timer that is still pending in
    # its service another event a timer refresh interval after the first, so the services
    # are polled until that long has passed since the last of them was handed a timer.
    until = max(s.timer_at for s in served) + REFRESH_SECONDS
    while time.monotonic() < until:
        cycle(served)
    for s in served:
        if len(s.timers) != 3 or len(s.later) != LATER:
            fail(f"the service beside member {s.member} was handed the timers {s.timers} and {len(s.later)} later "
                 f"ones by {REFRESH_SECONDS} s after the last; expected three and {LATER}, once each")

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
