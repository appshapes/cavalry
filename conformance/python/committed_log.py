#!/usr/bin/env python3
"""The Python client's committed log round (spec/c-abi.md, version 2; ADR 0070):
`cavalry.Client.committed_log` and `cavalry.CommittedLog` beside a cluster member that
`conformance/python/run.sh` started with `cavalry run`, one of three on the loopback.

    committed_log.py <the member's driver directory> <the member's id>

A cluster started with `cavalry run` holds one entry, the first of its leader's term, 56
bytes in a frame of 88 at position 0, so the log's next position is 96. The round reads
that entry through the handler, with its position, its header and the handle's reads;
makes each of the eight calls the ABI refuses from inside the log's own handler, each
after it has set the thread's error to another code, so that a refusal that answers what
a live log answers is told from an answer; opens a log and closes it with no poll
between, which leaves its client open (ADR 0078); checks where a handler's exception
goes and that its entry is not delivered again; opens the log again at 96, where nothing
is committed yet,
beside a log at 0 that it reads the entry through meanwhile, so that the log at 96 has
had the time a replay's image takes to join; closes twice; and closes a log after its
client. Every wait is bounded at 30 s and sleeps a millisecond after a poll that
delivered nothing. It exits 1 with what it found when a check fails. Python 3.9 standard
library only (ADR 0003).
"""

import os
import sys
import time

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "clients", "python"))
import cavalry  # noqa: E402
from cavalry import _abi  # noqa: E402

WAIT_SECONDS = 30.0
IDLE_SECONDS = 0.001  # after a poll that delivered nothing: three drivers share the host
ENTRY_FRAME = 88   # the term's first entry: 32 bytes of header and 56 of message
NEXT = 96          # the frame, rounded up to a multiple of 32


def fail(what):
    print(f"conformance/python: committed log: {what}", file=sys.stderr)
    sys.exit(1)


def open_log(client, position):
    """The member's committed log at `position`, asked for until the member has one."""
    until = time.monotonic() + WAIT_SECONDS
    while True:
        log = client.committed_log(position)
        if log is not None:
            return log
        if time.monotonic() > until:
            fail(f"the driver at {client.dir} hosted no member with a log within {WAIT_SECONDS:.0f} s")
        time.sleep(0.05)


def poll_until(log, handler, done, what):
    until = time.monotonic() + WAIT_SECONDS
    while not done():
        count = log.poll(handler, 16)
        if count < 0:
            fail(f"{what}: the poll stopped on fault {log.fault()}")
        if time.monotonic() > until:
            fail(f"{what}: not within {WAIT_SECONDS:.0f} s; position {log.position()}, commit position "
                 f"{log.commit_position()}, ended {log.is_ended}")
        if count == 0:
            time.sleep(IDLE_SECONDS)


def main():
    if len(sys.argv) != 3:
        fail("usage: committed_log.py <the member's driver directory> <the member's id>")
    member_dir, member_id = sys.argv[1], int(sys.argv[2])
    abi = cavalry.library()
    client = cavalry.Client(member_dir)
    log = open_log(client, 0)
    if (log.member_id, log.recording_id) != (member_id, 0):
        fail(f"the log is member {log.member_id}'s recording {log.recording_id}, not member {member_id}'s recording 0")
    if (log.position(), log.commit_position()) != (0, 0):
        fail(f"a log opened at 0 began at {log.position()} with bound {log.commit_position()}")

    # The entry, with what each call on the log answers from inside its own handler.
    entries = []
    refusals = []
    inside = []

    def set_another_error():
        """Sets the thread's error to one no refusal gives, CAVALRY_ERR_NULL, by asking the
        ABI for the position of a null header. A success clears nothing (spec/c-abi.md,
        "Error codes"), and a refused `is_ended`, `failed_at` or `withdrawn_at` answers
        what a live log answers, False or None, so the code alone tells that refusal from
        an answer: it must have changed from this one to CAVALRY_ERR_POLLING. Without
        this the code would be CAVALRY_ERR_POLLING already, left by the call before."""
        if abi.cavalry_committed_log_entry_position(None) != -1 or abi.cavalry_errcode() != _abi.CAVALRY_ERR_NULL:
            fail(f"the position of a null header did not answer -1 with CAVALRY_ERR_NULL: error {abi.cavalry_errcode()}")

    def on_entry(body, header, position):
        entries.append((position, len(body), header.frame_length, header.flags, header.type))
        if inside:
            return
        inside.append(True)
        for name, call in (("poll", lambda: log.poll(on_entry, 1)), ("position", log.position),
                           ("commit_position", log.commit_position), ("is_ended", lambda: log.is_ended),
                           ("failed_at", log.failed_at), ("withdrawn_at", log.withdrawn_at)):
            set_another_error()
            refusals.append((name, call(), abi.cavalry_errcode()))
        for name, call in (("fault", log.fault), ("close", log.close)):
            set_another_error()
            try:
                call()
                refusals.append((name, 0, abi.cavalry_errcode()))
            except cavalry.CavalryError as error:
                refusals.append((name, -1, error.code))

    poll_until(log, on_entry, lambda: entries, "the term's first entry")
    if entries != [(0, ENTRY_FRAME - 32, ENTRY_FRAME, _abi.CAVALRY_FLAGS_UNFRAGMENTED, _abi.CAVALRY_FRAME_TYPE_DATA)]:
        fail(f"expected one entry of {ENTRY_FRAME - 32} bytes at position 0 in an unfragmented data frame of "
             f"{ENTRY_FRAME}, got {entries}. One cause of more than one entry is a new term: a cluster that "
             f"elected again between its formation and this read holds that term's first entry after this one; "
             f"the members' output, which run.sh prints, says whether it did")
    polling = _abi.CAVALRY_ERR_POLLING
    expected = [("poll", -1, polling), ("position", -1, polling), ("commit_position", -1, polling),
                ("is_ended", False, polling), ("failed_at", None, polling), ("withdrawn_at", None, polling),
                ("fault", -1, polling), ("close", -1, polling)]
    if refusals != expected:
        fail(f"the calls from inside the log's own handler answered {refusals}, not {expected}")
    if (log.position(), log.commit_position()) != (NEXT, NEXT):
        fail(f"after the entry the log is at {log.position()} with bound {log.commit_position()}, not {NEXT} and {NEXT}")
    if (log.is_ended or log.failed_at() is not None or log.withdrawn_at() is not None
            or log.fault() != (0, 0, cavalry.NULL_POSITION)):
        fail(f"a live log read ended {log.is_ended}, failed at {log.failed_at()}, withdrawn at "
             f"{log.withdrawn_at()}, fault {log.fault()}")
    for _ in range(50):
        if log.poll(on_entry, 16) != 0:
            fail("an entry arrived with nothing more committed")
    if len(entries) != 1:
        fail(f"the refused poll, or a later one, delivered more: {entries}")
    print(f"conformance/python: committed log: member {log.member_id}'s entry at position 0 read once, next "
          f"position {log.position()}, commit position {log.commit_position()}; eight calls from inside the handler "
          f"refused with CAVALRY_ERR_POLLING, each made with the thread's error at CAVALRY_ERR_NULL")
    log.close()
    log.close()  # idempotent

    # A log opened and closed with no poll between: its replay's image may not have joined
    # yet, and the client's conductor then reads of an image whose log is gone. The client
    # stays open (ADR 0078; before it, card 241, this closed the client as corrupt in 8 of
    # 60 tries), which twenty in a row and a log opened after them show.
    for _ in range(20):
        open_log(client, 0).close()
    if client.is_closed:
        fail(f"a log opened and closed with no poll between closed its client: {client.closed()}")

    # A handler that raises: the entry was delivered, the exception is raised after the
    # ABI has returned, and the log's position is past the entry, so it is not delivered
    # again.
    raising = open_log(client, 0)
    seen = []

    def raises(body, header, position):
        seen.append(position)
        raise ValueError("the handler's own")

    until = time.monotonic() + WAIT_SECONDS
    raised = None
    while raised is None:
        try:
            if raising.poll(raises, 16) < 0:
                fail(f"the second log's poll stopped on fault {raising.fault()}")
        except cavalry.HandlerError as error:
            raised = error
        if raised is None and time.monotonic() > until:
            fail("the second log read no entry")
        if raised is None:
            time.sleep(IDLE_SECONDS)
    if seen != [0] or not isinstance(raised.__cause__, ValueError) or raising.position() != NEXT:
        fail(f"a raising handler saw {seen}, raised {raised!r} from {raised.__cause__!r}, and left the log at "
             f"{raising.position()}")
    # The error is the log's own: it counts entries and names no subscription.
    if (raised.fragments, raised.raised) != (1, 1) or "entry handler raised on 1 of the 1 entry" not in str(raised) \
            or "Subscription" in str(raised):
        fail(f"a committed log's HandlerError has fragments {raised.fragments}, raised {raised.raised} and says: {raised}")
    for _ in range(50):
        if raising.poll(raises, 16) != 0:
            fail("the entry whose handler raised was delivered again")
    raising.close()

    # From the next position nothing is committed yet: no entry, and no entry before it.
    # A replay's image joins a moment after the open, so polls made at once would pass
    # before the image is there, whatever the open did with the position. So a second
    # log is opened at 0 beside this one, on the same client, and both are polled until
    # that one has delivered the entry at 0 and this one has taken the member's commit
    # position as its bound, which a poll does once the log's image has joined
    # (`CommittedLog::poll`, crates/cavalry-client/src/committed_log.rs): by then this
    # log's replay, asked for first, has had the time the other's took to join and
    # deliver.
    rest = open_log(client, NEXT)
    if rest.position() != NEXT:
        fail(f"a log opened at {NEXT} began at {rest.position()}")
    beside = open_log(client, 0)
    at_next, at_start = [], []
    until = time.monotonic() + WAIT_SECONDS
    while not at_start or rest.commit_position() < NEXT:
        counts = (rest.poll(lambda body, header, position: at_next.append(position), 16),
                  beside.poll(lambda body, header, position: at_start.append(position), 16))
        if min(counts) < 0:
            fail(f"the polls of the logs at {NEXT} and at 0 returned {counts}: faults {rest.fault()} and {beside.fault()}")
        if time.monotonic() > until:
            fail(f"the log at 0 beside the log at {NEXT} delivered {at_start} and the log at {NEXT} took the bound "
                 f"{rest.commit_position()}, not the entry at 0 and {NEXT}, within {WAIT_SECONDS:.0f} s")
        if counts == (0, 0):
            time.sleep(IDLE_SECONDS)
    for _ in range(50):
        if rest.poll(lambda body, header, position: at_next.append(position), 16) < 0:
            fail(f"the poll of the log at {NEXT} stopped on fault {rest.fault()}")
    if at_start[0] != 0 or at_next or rest.position() != NEXT:
        fail(f"a log opened at {NEXT} delivered the entries at {at_next} and is at {rest.position()}, while a log "
             f"opened at 0 beside it delivered the entries at {at_start}. An entry below {NEXT} is one the open "
             f"did not start past; one at {NEXT} or above is a new term's, of a cluster that elected again, which "
             f"the members' output, printed by run.sh, shows")
    beside.close()
    # A log that outlives its client: its poll stops on the closed fault, and its close
    # frees it with no client to stop the replay through.
    client.close()
    if rest.poll(on_entry, 16) != -1 or rest.fault()[0] != _abi.CAVALRY_ERR_FAULT_CLOSED:
        fail(f"a poll after the client's close did not stop on the closed fault: {rest.fault()}")
    rest.close()
    print(f"conformance/python: committed log: twenty logs opened and closed with no poll between left the "
          f"client open; a raising handler's entry is not delivered again; nothing at position {NEXT} while a "
          f"log at 0 beside it read the entry at 0; a log closed after its client")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except cavalry.CavalryError as error:
        fail(f"{error.function} failed with error {error.code}: {error.message}")
    except cavalry.HandlerError as error:
        fail(f"the handler raised: {error.__cause__!r}")
