#!/usr/bin/env python3
"""The Python client's snapshot round (spec/c-abi.md, version 6, "A service's snapshot
and its start"; ADR 0146, ADR 0147, ADR 0149): a service beside each of three cluster
members that `conformance/python/run.sh` started with `cavalry run` takes a snapshot of
its program's state that its member lists, and a service that starts again starts from
it.

    snapshot.py <cluster directory> <the cavalry binary>

The members' drivers are <cluster directory>/member-0, -1 and -2. The round runs after
the cluster round, on the log that round left.

The program's state is what it has read of the log: how many entries, and a hash of
each entry's type, position, term, time and payload, in order, written out 77 times so
that a snapshot holds it in several messages. It is the same beside every member,
since every member's service is handed the same entries.

The round:

1. Each service asks where it starts, before its first poll: `restore()` answers
   CAVALRY_CLUSTER_RESTORED_FROM_START and no state, and a second `restore()` is
   refused with CAVALRY_ERR_CLUSTER_RESTORE_TOO_LATE.
2. Each service reads an entry of the log. No snapshot is due, and `snapshot()` is
   refused with CAVALRY_ERR_CLUSTER_NO_SNAPSHOT_DUE.
3. `cavalry snapshot` is run beside each member. Beside a follower it exits 1 and names
   the leader; beside the leader it exits 0 and prints the snapshot's log position.
4. Each service's poll ends at the leader's request, handed over as
   CAVALRY_CLUSTER_ENTRY_SNAPSHOT, and `snapshot_due()` is the position the command
   printed; the three services hold one state there. From inside the handler at the
   request, `snapshot_due()` is None with the thread's error at CAVALRY_ERR_POLLING, and
   `snapshot()` and `restore()` are refused with it.
5. Each service gives its state: `snapshot(state)` is True. No snapshot is due after,
   and a second `snapshot()` is refused. The three services are then closed: one
   service beside a member takes snapshots.
6. Beside each member a new service starts from the snapshot: `restore()` answers
   CAVALRY_CLUSTER_RESTORED_STATE and the state as it was given, which only a member
   that lists the snapshot names, and its position is the snapshot's.
7. `cavalry snapshot` is run beside the leader again. Each new service's first entry is
   the request, at the first snapshot's position and no entry below it; its poll ends
   there, and it gives its state: `snapshot(state)` is True. A start beside each member
   is then given that state, at the second snapshot's position.
8. A service that never asks where it starts reads the log from position 0. It meets
   the first request with the state the first services gave, and the second with the
   state the restarted ones hold: a start from a snapshot and a read from the start
   reach one state. Its `snapshot()` is False both times: its member lists one there.

Every wait is bounded at 60 s. It exits 1 with what it found when a check fails. Python
3.9 standard library only (ADR 0003).
"""

import hashlib
import os
import struct
import subprocess
import sys
import time

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "clients", "python"))
import cavalry  # noqa: E402
from cavalry import _abi  # noqa: E402

MEMBERS = 3
WAIT_SECONDS = 60.0
IDLE_SECONDS = 0.0002  # between duty cycles: the members run in processes of their own
STATE_LENGTH = 8 + 32 * 77  # 2,472 bytes: three messages of a snapshot, 1,024 bytes each at the most
SNAPSHOT = _abi.CAVALRY_CLUSTER_ENTRY_SNAPSHOT
FROM_START = _abi.CAVALRY_CLUSTER_RESTORED_FROM_START
NO_MEMBER = _abi.CAVALRY_CLUSTER_RESTORED_NO_MEMBER
STATE = _abi.CAVALRY_CLUSTER_RESTORED_STATE
POLLING = _abi.CAVALRY_ERR_POLLING
NONE_DUE = ("raised", _abi.CAVALRY_ERR_CLUSTER_NO_SNAPSHOT_DUE)
TOO_LATE = ("raised", _abi.CAVALRY_ERR_CLUSTER_RESTORE_TOO_LATE)


def fail(what):
    print(f"conformance/python: snapshot round: {what}", file=sys.stderr)
    sys.exit(1)


def code_of(call):
    """The code of the `CavalryError` that `call` raises, or what it returned."""
    try:
        return ("returned", call())
    except cavalry.CavalryError as error:
        return ("raised", error.code)


class Program:
    """A service beside one member and its program: the state is how many entries it
    has read and a hash over them, which every member's service computes alike."""

    def __init__(self, abi, cluster, member):
        self.abi = abi
        self.member = member
        self.client = cavalry.Client(os.path.join(cluster, f"member-{member}"))
        self.service = self.client.cluster_service()
        self.count = 0
        self.digest = b"\0" * 32
        self.entries = []       # the type and the position of every entry taken since this service began
        self.inside = None      # what the three calls answered from inside the handler

    def state(self):
        """The program's state as bytes: the count, eight bytes, and the hash 77 times."""
        return struct.pack("<q", self.count) + self.digest * 77

    def take(self, state):
        """Starts the program from `state`, as its snapshot held it."""
        if len(state) != STATE_LENGTH or state[8:] != state[8:40] * 77:
            fail(f"a state of {len(state)} bytes that is not one the program gave")
        (self.count,) = struct.unpack("<q", state[:8])
        self.digest = state[8:40]

    def on_entry(self, body, entry, service):
        self.count += 1
        self.digest = hashlib.sha256(
            self.digest
            + struct.pack("<iqqq", entry.type, entry.position, entry.leadership_term_id, entry.timestamp)
            + bytes(body)
        ).digest()
        self.entries.append((entry.type, entry.position))
        if entry.type == SNAPSHOT and self.inside is None:
            # The poll holds the service and its client. The read answers None for a
            # refusal as for "none due", so the thread's error is set to another code
            # first, as the cluster round does, and must be CAVALRY_ERR_POLLING after.
            if self.abi.cavalry_committed_log_entry_position(None) != -1:
                fail("the position of a null header did not answer -1")
            due = service.snapshot_due()
            self.inside = (
                due,
                self.abi.cavalry_errcode(),
                code_of(lambda: service.snapshot(self.state())),
                code_of(service.restore),
            )

    def poll(self):
        count = self.service.poll(self.on_entry, 16)
        if count < 0:
            fail(f"the poll of the service beside member {self.member} stopped on fault {self.service.fault()}")
        return count

    def restore(self):
        """Asks where the service starts until its driver hosts a member with a log."""
        deadline = time.monotonic() + WAIT_SECONDS
        while True:
            restored, state = self.service.restore()
            if restored != NO_MEMBER:
                return restored, state
            if time.monotonic() > deadline:
                fail(f"the driver of member {self.member} hosts no member with a log after {WAIT_SECONDS:.0f} s")
            time.sleep(0.001)

    def give(self):
        """Gives the program's state to the service's snapshot: what it answered. A
        snapshot that a step of the driver's gave up is still due, and is given again
        (spec/c-abi.md, "A service's snapshot and its start")."""
        deadline = time.monotonic() + WAIT_SECONDS
        while True:
            answered = code_of(lambda: self.service.snapshot(self.state()))
            if answered != ("raised", _abi.CAVALRY_ERR_CLUSTER_SNAPSHOT_FAILED) or time.monotonic() > deadline:
                return answered
            time.sleep(0.01)

    def close(self):
        self.service.close()
        self.client.close()


def wait(what, done, programs):
    """Polls every program's service that has no snapshot to give until `done`."""
    deadline = time.monotonic() + WAIT_SECONDS
    while not done():
        if time.monotonic() > deadline:
            fail(f"{what}, after {WAIT_SECONDS:.0f} s")
        for program in programs:
            if program.service.snapshot_due() is None:
                program.poll()
        time.sleep(IDLE_SECONDS)


def ask(binary, cluster, member):
    """`cavalry snapshot` beside member `member`: its exit code and what it printed."""
    try:
        ran = subprocess.run(
            [binary, "snapshot", "--dir", os.path.join(cluster, f"member-{member}")],
            capture_output=True, text=True, timeout=WAIT_SECONDS, check=False)
    except subprocess.TimeoutExpired:
        fail(f"`cavalry snapshot` beside member {member} did not end in {WAIT_SECONDS:.0f} s")
    return ran


def position_of(ran, member):
    """The log position that `cavalry snapshot` printed beside member `member`."""
    prefix = f"member {member} lists a snapshot of log position "
    if ran.returncode != 0 or not ran.stdout.startswith(prefix):
        fail(f"`cavalry snapshot` beside member {member}, exit {ran.returncode}: {(ran.stdout + ran.stderr).strip()}")
    return int(ran.stdout[len(prefix):].split(":")[0].split(",")[0])


def main():
    if len(sys.argv) != 3:
        print("usage: snapshot.py <cluster directory> <the cavalry binary>", file=sys.stderr)
        sys.exit(2)
    cluster, binary = sys.argv[1:]
    abi = cavalry.library()

    # 1. Where each service starts: no snapshot of a service is listed yet.
    programs = [Program(abi, cluster, member) for member in range(MEMBERS)]
    for program in programs:
        if program.service.snapshot_due() is not None:
            fail(f"a snapshot is due beside member {program.member} before its service's first poll")
        restored = program.restore()
        if restored != (FROM_START, None):
            fail(f"the service beside member {program.member} starts with {restored}, not from the log's start")
        again = code_of(program.service.restore)
        if again != TOO_LATE:
            fail(f"a second restore beside member {program.member} answered {again}")

    # 2. Every service reads an entry of the log the cluster round left; none is due.
    wait("every service read an entry", lambda: all(program.count > 0 for program in programs), programs)
    for program in programs:
        refused = code_of(lambda: program.service.snapshot(program.state()))
        if refused != NONE_DUE:
            fail(f"a snapshot where none is due answered {refused} beside member {program.member}")

    # 3. `cavalry snapshot` beside each member: the leader's is taken, a follower's
    # names the leader.
    asked = [ask(binary, cluster, member) for member in range(MEMBERS)]
    leaders = [member for member, ran in enumerate(asked) if ran.returncode == 0]
    if len(leaders) != 1:
        fail("`cavalry snapshot` did not exit 0 beside exactly one member: "
             + "; ".join(f"member {m}: exit {r.returncode}: {(r.stdout or r.stderr).strip()}" for m, r in enumerate(asked)))
    leader = leaders[0]
    position = position_of(asked[leader], leader)
    for member, ran in enumerate(asked):
        if member != leader and (ran.returncode != 1 or f"member {leader} leads" not in ran.stderr):
            fail(f"`cavalry snapshot` beside member {member}, a follower, exit {ran.returncode}: {ran.stderr.strip()}")

    # 4. Every service's poll ends at the request, and the snapshot is due at the
    # position the command printed, with one state beside the three members.
    wait("every service's poll ended at the leader's request",
         lambda: all(program.service.snapshot_due() is not None for program in programs), programs)
    refused = ("raised", POLLING)
    for program in programs:
        due = program.service.snapshot_due()
        if due != position or program.service.position() != position or program.entries[-1][0] != SNAPSHOT:
            fail(f"beside member {program.member} the snapshot is due at {due}, the service is at "
                 f"{program.service.position()} and its last entry is {program.entries[-1]}: "
                 f"the command printed {position}")
        if program.inside != (None, POLLING, refused, refused):
            fail(f"from inside the handler at the request, beside member {program.member}: {program.inside}")
    given = programs[0].state()
    if any(program.state() != given for program in programs):
        fail("the three services hold different states at the snapshot's position")
    if len(given) != STATE_LENGTH:
        fail(f"a state of {len(given)} bytes")

    # 5. Each service gives its state, once, and is closed: the service that starts
    # from the snapshot is the one that takes the next.
    for program in programs:
        written = program.give()
        if written != ("returned", True):
            fail(f"the service beside member {program.member} gave its state and was answered {written}")
        if program.service.snapshot_due() is not None:
            fail(f"a snapshot is still due beside member {program.member} after it was taken")
        again = code_of(lambda: program.service.snapshot(program.state()))
        if again != NONE_DUE:
            fail(f"a second snapshot beside member {program.member} answered {again}")
        program.close()

    # 6. The member lists it: a new service beside each member starts from it. A member
    # lists the snapshot when it reads the service's word of it, a duty cycle or so
    # later, so a start that finds none is made again with a new service.
    restarted = []
    for member in range(MEMBERS):
        deadline = time.monotonic() + WAIT_SECONDS
        while True:
            program = Program(abi, cluster, member)
            restored, state = program.restore()
            if restored == STATE:
                break
            program.close()
            if time.monotonic() > deadline:
                fail(f"member {member} lists no snapshot of its service after {WAIT_SECONDS:.0f} s: "
                     f"a new service starts with {restored}")
            time.sleep(0.01)
        if state != given:
            fail(f"the service beside member {member} starts from {len(state)} bytes that are not the state given")
        program.take(state)
        if program.service.position() != position:
            fail(f"the service beside member {member} starts at {program.service.position()}, not at {position}")
        again = code_of(program.service.restore)
        if again != TOO_LATE:
            fail(f"a second restore beside member {member} answered {again}")
        restarted.append(program)

    # 7. A second request, beside the leader: each restarted service's first entry is
    # the request, at the first snapshot's position, and it gives its state.
    second = position_of(ask(binary, cluster, leader), leader)
    if second <= position:
        fail(f"a second snapshot at {second}, not past the first at {position}")
    wait("every restarted service's poll ended at the second request",
         lambda: all(program.service.snapshot_due() is not None for program in restarted), restarted)
    for program in restarted:
        if program.entries != [(SNAPSHOT, position)] or program.service.snapshot_due() != second:
            fail(f"the service beside member {program.member} that started from its snapshot at {position} was "
                 f"handed {program.entries}, with a snapshot due at {program.service.snapshot_due()}: the second "
                 f"request is the one entry at {position}, and asks for a snapshot at {second}")
    after = restarted[0].state()
    if after == given or any(program.state() != after for program in restarted):
        fail("the restarted services do not hold one new state at the second snapshot's position")
    for program in restarted:
        written = program.give()
        if written != ("returned", True):
            fail(f"the restarted service beside member {program.member} gave its state and was answered {written}")
    # Each member lists the second snapshot too: a start beside it is given the new
    # state, once the member has read the service's word of it.
    for member in range(MEMBERS):
        deadline = time.monotonic() + WAIT_SECONDS
        while True:
            probe = Program(abi, cluster, member)
            restored, state = probe.restore()
            at = probe.service.position()
            probe.close()
            if (restored, state, at) == (STATE, after, second):
                break
            if time.monotonic() > deadline:
                fail(f"member {member} does not list its service's second snapshot after {WAIT_SECONDS:.0f} s: "
                     f"a new service starts with {restored} at {at}")
            time.sleep(0.01)

    # 8. A service that never asks reads from position 0 and meets both requests, each
    # with the state the others held there: a start from a snapshot and a read from
    # the start reach one state. Its member lists a snapshot at each, or past it.
    late = Program(abi, cluster, leader)
    for at, held in ((position, given), (second, after)):
        wait(f"a service that reads from the start meets the request for {at}",
             lambda: late.service.snapshot_due() is not None, [late])
        if late.service.snapshot_due() != at or late.state() != held:
            fail(f"a service that read from the start is due at {late.service.snapshot_due()}, not at {at}, "
                 f"or holds another state than the services held there")
        listed = late.give()
        if listed != ("returned", False):
            fail(f"a snapshot of position {at}, which the member already lists or is past, answered {listed}")

    for program in restarted + [late]:
        program.close()
    print(f"conformance/python: snapshot round: three services gave a state of {len(given)} bytes at log "
          f"position {position}, asked for by `cavalry snapshot` beside member {leader}; each member listed "
          f"its service's snapshot, a new service beside each started from it, read the next request at "
          f"{position} and gave its state at {second}")
    print("conformance/python: snapshot round ok")


if __name__ == "__main__":
    main()
