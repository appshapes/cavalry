#!/usr/bin/env python3
"""The Python client's closed round (spec/c-abi.md, "cluster service"; ADR 0155): a
`cavalry.ClusterService` whose driver stops. A poll of a closed client is one answer,
-1 with `CAVALRY_ERR_CLOSED` as the thread's error and as `fault()`'s code, at the
first poll that fails and at every poll after it, with the client's reason `CAVALRY_CLOSED_DRIVER_CLOSED`; and the
service's close is then no error. The driver hosts no member, so the service holds no
log and its poll has no command to send: the case in which a poll of a closed client
took nothing and returned 0 for as long as it was called. `conformance/python/run.sh`
runs it last, against the driver its quickstart ran on.

    closed.py <the driver's process id>

The client attaches to the driver at CAVALRY_DIR, and the program stops that driver
itself, with SIGTERM to the process id it was given. A failed check prints what failed
and exits 1; the wait is bounded at 60 s. Python 3.9 standard library only (ADR 0003).
"""

import os
import signal
import sys
import time

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "clients", "python"))
import cavalry  # noqa: E402
from cavalry import _abi  # noqa: E402

WAIT_SECONDS = 60.0
POLLS_AFTER = 3


def fail(what):
    print(f"conformance/python: closed round: {what}", file=sys.stderr)
    sys.exit(1)


def main():
    if len(sys.argv) != 2 or not sys.argv[1].isdigit() or int(sys.argv[1]) <= 1:
        fail("usage: closed.py <the driver's process id>")
    pid = int(sys.argv[1])
    handed = []

    def on_entry(body, entry, service):
        handed.append(entry.type)

    abi = cavalry.library()
    client = cavalry.Client()
    service = client.cluster_service()

    # 1. Beside a driver that runs and hosts no member, a poll takes nothing.
    taken = service.poll(on_entry, 10)
    if taken != 0 or client.is_closed:
        fail(f"a poll beside a driver with no member returned {taken}, and the client reads closed "
             f"{client.closed()}; expected 0 beside a driver that runs")

    # 2. The driver stops. Until the client has seen it, a poll takes nothing; the first
    # poll that fails is the close, and nothing else.
    os.kill(pid, signal.SIGTERM)
    until = time.monotonic() + WAIT_SECONDS
    polls = 0
    while True:
        taken = service.poll(on_entry, 10)
        errcode = abi.cavalry_errcode()
        polls += 1
        if taken == -1:
            break
        if taken != 0:
            fail(f"a poll beside a driver with no member returned {taken}; expected 0, or -1 after the stop")
        if time.monotonic() > until:
            fail(f"no poll failed within {WAIT_SECONDS:.0f} s of the driver's stop; the client reads closed "
                 f"{client.closed()}")
        time.sleep(0.001)
    code = service.fault()[0]
    if code != _abi.CAVALRY_ERR_CLOSED or errcode != _abi.CAVALRY_ERR_CLOSED:
        fail(f"the first poll that failed after the driver's stop stopped on code {code}, with the thread's "
             f"error {errcode}; expected CAVALRY_ERR_CLOSED, {_abi.CAVALRY_ERR_CLOSED}, as both")
    if not client.is_closed or client.closed() != _abi.CAVALRY_CLOSED_DRIVER_CLOSED:
        fail(f"the poll said closed, and the client reads closed {client.closed()}; expected "
             f"CAVALRY_CLOSED_DRIVER_CLOSED, {_abi.CAVALRY_CLOSED_DRIVER_CLOSED}")

    # 3. Every poll after it is the same answer: a loop that looks at what its poll
    # returned ends.
    for after in range(1, POLLS_AFTER + 1):
        taken = service.poll(on_entry, 10)
        errcode = abi.cavalry_errcode()
        code = service.fault()[0]
        if taken != -1 or code != _abi.CAVALRY_ERR_CLOSED or errcode != _abi.CAVALRY_ERR_CLOSED:
            fail(f"poll {after} after the first that failed returned {taken} with code {code} and the thread's "
                 f"error {errcode}; expected -1 with CAVALRY_ERR_CLOSED as both")
    if handed:
        fail(f"the handler was called beside a driver with no member: {handed}")

    # 4. The service closes with no error, and its client after it: a driver that is
    # gone cannot be told.
    service.close()
    client.close()
    print(f"conformance/python: closed round: a service beside a driver with no member took nothing; after the "
          f"driver's stop, poll {polls} and the {POLLS_AFTER} after it returned -1 with CAVALRY_ERR_CLOSED, the "
          f"client's reason is CAVALRY_CLOSED_DRIVER_CLOSED, and the service closed with no error")
    print("conformance/python: closed round ok")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except cavalry.CavalryError as error:
        fail(f"{error.function} failed with error {error.code}: {error.message}")
