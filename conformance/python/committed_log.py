#!/usr/bin/env python3
"""The Python client's committed log round (spec/c-abi.md, version 2; ADR 0068):
`cavalry.Client.committed_log` and `cavalry.CommittedLog` beside a cluster member that
`conformance/python/run.sh` started with `cavalry run`, one of three on the loopback.

    committed_log.py <the member's driver directory> <the member's id>

A cluster started with `cavalry run` holds one entry, the first of its leader's term, 56
bytes in a frame of 88 at position 0, so the log's next position is 96. The round reads
that entry through the handler, with its position, its header and the handle's reads;
makes each call the ABI refuses from inside the log's own handler; checks where a
handler's exception goes and that its entry is not delivered again; opens the log again at
96, where nothing is committed yet; closes twice; and closes a log after its client. Every
wait is bounded at 30 s. It exits 1 with what it found when a check fails. Python 3.9
standard library only (ADR 0003).
"""

import os
import sys
import time

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "clients", "python"))
import cavalry  # noqa: E402
from cavalry import _abi  # noqa: E402

WAIT_SECONDS = 30.0
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
        if log.poll(handler, 16) < 0:
            fail(f"{what}: the poll stopped on fault {log.fault()}")
        if time.monotonic() > until:
            fail(f"{what}: not within {WAIT_SECONDS:.0f} s; position {log.position()}, commit position "
                 f"{log.commit_position()}, ended {log.is_ended}")


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

    def on_entry(body, header, position):
        entries.append((position, len(body), header.frame_length, header.flags, header.type))
        if refusals:
            return
        refusals.append(("poll", log.poll(on_entry, 1), abi.cavalry_errcode()))
        refusals.append(("position", log.position(), abi.cavalry_errcode()))
        refusals.append(("commit_position", log.commit_position(), abi.cavalry_errcode()))
        refusals.append(("is_ended", log.is_ended, abi.cavalry_errcode()))
        refusals.append(("failed_at", log.failed_at(), abi.cavalry_errcode()))
        try:
            log.close()
            refusals.append(("close", 0, 0))
        except cavalry.CavalryError as error:
            refusals.append(("close", -1, error.code))

    poll_until(log, on_entry, lambda: entries, "the term's first entry")
    if entries != [(0, ENTRY_FRAME - 32, ENTRY_FRAME, _abi.CAVALRY_FLAGS_UNFRAGMENTED, _abi.CAVALRY_FRAME_TYPE_DATA)]:
        fail(f"expected one entry of {ENTRY_FRAME - 32} bytes at position 0 in an unfragmented data frame of "
             f"{ENTRY_FRAME}, got {entries}")
    polling = _abi.CAVALRY_ERR_POLLING
    expected = [("poll", -1, polling), ("position", -1, polling), ("commit_position", -1, polling),
                ("is_ended", False, polling), ("failed_at", None, polling), ("close", -1, polling)]
    if refusals != expected:
        fail(f"the calls from inside the log's own handler answered {refusals}, not {expected}")
    if (log.position(), log.commit_position()) != (NEXT, NEXT):
        fail(f"after the entry the log is at {log.position()} with bound {log.commit_position()}, not {NEXT} and {NEXT}")
    if log.is_ended or log.failed_at() is not None or log.fault() != (0, 0, cavalry.NULL_POSITION):
        fail(f"a live log read ended {log.is_ended}, failed at {log.failed_at()}, fault {log.fault()}")
    for _ in range(50):
        if log.poll(on_entry, 16) != 0:
            fail("an entry arrived with nothing more committed")
    if len(entries) != 1:
        fail(f"the refused poll, or a later one, delivered more: {entries}")
    print(f"conformance/python: committed log: member {log.member_id}'s entry at position 0 read once, next "
          f"position {log.position()}, commit position {log.commit_position()}; six calls from inside the handler "
          f"refused with CAVALRY_ERR_POLLING")
    log.close()
    log.close()  # idempotent

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
    if seen != [0] or not isinstance(raised.__cause__, ValueError) or raising.position() != NEXT:
        fail(f"a raising handler saw {seen}, raised {raised!r} from {raised.__cause__!r}, and left the log at "
             f"{raising.position()}")
    for _ in range(50):
        if raising.poll(raises, 16) != 0:
            fail("the entry whose handler raised was delivered again")
    raising.close()

    # From the next position nothing is committed yet: no entry, and no entry before it.
    rest = open_log(client, NEXT)
    if rest.position() != NEXT:
        fail(f"a log opened at {NEXT} began at {rest.position()}")
    for _ in range(100):
        if rest.poll(on_entry, 16) != 0:
            fail(f"a log opened at {NEXT} delivered an entry: {entries}")
    # A log that outlives its client: its poll stops on the closed fault, and its close
    # frees it with no client to stop the replay through.
    client.close()
    if rest.poll(on_entry, 16) != -1 or rest.fault()[0] != _abi.CAVALRY_ERR_FAULT_CLOSED:
        fail(f"a poll after the client's close did not stop on the closed fault: {rest.fault()}")
    rest.close()
    print(f"conformance/python: committed log: a raising handler's entry is not delivered again; nothing at "
          f"position {NEXT}; a log closed after its client")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except cavalry.CavalryError as error:
        fail(f"{error.function} failed with error {error.code}: {error.message}")
    except cavalry.HandlerError as error:
        fail(f"the handler raised: {error.__cause__!r}")
