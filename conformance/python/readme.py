#!/usr/bin/env python3
"""Runs the example of clients/python/README.md against the runner's driver; `run.sh`
calls it after quickstart.py, so the README's program is a test (CLAUDE.md, "Clients and
developer experience"; .claude/rules/docs.md, "Code in docs"). It takes the program
between `python3 - <<'PY'` and `PY` in the README's "Example" and the line the README
promises after "You will see", runs the program with this Python and
`PYTHONPATH=clients/python` from the checkout's root, as the README's shell lines do, for
up to 10 s, and exits 1 unless the program exits 0 and prints that line. The README's
shell lines around the program, the builds and the driver, are run.sh's own steps.

The program runs without `CAVALRY_FFI_LIBRARY`, which run.sh sets for its own programs
(work item 504): the README's shell lines do not set it, so the reader's package finds
the library by its own search, up from `clients/python/cavalry` to the checkout's
`target/release`, and this is the test of that search. It finds there the library run.sh
has just built, and run.sh has stopped already where a library sits beside the module.
Python 3.9 standard library only (ADR 0003).
"""

import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
README = os.path.join("clients", "python", "README.md")
WAIT_SECONDS = 10


def fail(what):
    print(f"conformance/python: {README}'s example: {what}", file=sys.stderr)
    sys.exit(1)


def example():
    """The README's program and the line it promises, as `(program, expected)`.
    `scripts/package-python.py` runs the same two against the installed package (work
    item 469), so the shapes are read in one place."""
    with open(os.path.join(ROOT, README), encoding="utf-8") as f:
        text = f.read()
    program = re.search(r"^PYTHONPATH=clients/python python3 - <<'PY'\n(.*?)^PY$", text, flags=re.S | re.M)
    expected = re.search(r"^You will see `([^`]+)`\.$", text, flags=re.M)
    if program is None or expected is None:
        fail("no program between `PYTHONPATH=clients/python python3 - <<'PY'` and `PY`, or no line "
             "\"You will see `...`.\" after it; keep both shapes so this test can find them")
    return program.group(1), expected.group(1)


def main():
    program, expected = example()
    # The reader's environment: the README's PYTHONPATH, and no CAVALRY_FFI_LIBRARY, which
    # the README's shell lines never set (see the module's docstring).
    environment = {name: value for name, value in os.environ.items() if name != "CAVALRY_FFI_LIBRARY"}
    environment["PYTHONPATH"] = os.path.join(ROOT, "clients", "python")
    try:
        result = subprocess.run([sys.executable, "-"], input=program, capture_output=True, text=True,
                                timeout=WAIT_SECONDS, cwd=ROOT, env=environment)
    except subprocess.TimeoutExpired as timeout:
        fail(f"did not finish in {WAIT_SECONDS} s; it printed {timeout.stdout!r}")
    if result.returncode != 0:
        fail(f"exited {result.returncode}; it printed {result.stdout!r} and {result.stderr!r}")
    if expected not in result.stdout.splitlines():
        fail(f"printed {result.stdout!r}, not the line the README says the reader will see, {expected!r}")
    print(f"conformance/python: {README}'s example printed `{expected}`, as the README says")
    return 0


if __name__ == "__main__":
    sys.exit(main())
