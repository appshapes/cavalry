#!/usr/bin/env python3
"""Runs the program of docs/how-to/schedule-a-services-timer.md as the page's steps run it;
`run.sh` calls it after the snapshot page's program, so the page's program is a test
(CLAUDE.md, "Documentation"; .claude/rules/docs.md, "Code in docs"; ADR 0157).

    howto_timers.py <the cavalry binary> <a directory of its own> <first port>

It takes the page's one Python block and the lines the page says the reader will see,
starts a cluster of its own, three `cavalry run` members on the loopback with fifteen
ports counted from <first port> in 10000 to 19999, as run.sh counts its own, and follows
the page:

1. The program runs beside each of the three members. Each prints lines of the page's
   shape, a deadline, the position it was handed over at and how late; the first is the
   page's first, a tick at the page's position; and the three programs print the same
   lines, each deadline at one position.
2. The program beside member 0 is stopped with the signal Ctrl-C sends and run again:
   it prints its earlier lines again, unchanged, from the log's start.

It exits 1 with what it found otherwise. The cluster is its own because the page's first
position is that of a cluster that has committed nothing else. What differs from the
page: the directories and the ports, and the wait for the election by `cavalry doctor`.
Every wait is bounded at 60 s, and every process it started is stopped by its own
process id when it ends. Python 3.9 standard library only (ADR 0003).
"""

import os
import re
import signal
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
PAGE = os.path.join("docs", "how-to", "schedule-a-services-timer.md")
MEMBERS = 3
WAIT_SECONDS = 60.0
LINES = 3   # how many lines each program is read for: a tick, a tock and a tick, four seconds of the page
LINE = re.compile(r"(tick|tock) at position (\d+), (\d+) ms after its deadline")


def fail(what):
    print(f"conformance/python: {PAGE}: {what}", file=sys.stderr)
    sys.exit(1)


def page():
    """The page's program, and the lines it says the reader will see."""
    with open(os.path.join(ROOT, PAGE), encoding="utf-8") as f:
        text = f.read()
    blocks = re.findall(r"^   ```python\n(.*?)^   ```$", text, flags=re.S | re.M)
    said = [match.group(0) for match in LINE.finditer(" ".join(text.split()))]
    if len(blocks) != 1 or len(said) < LINES or not said[0].startswith("tick at position "):
        fail(f"the page has not one Python block indented by three spaces, or not {LINES} lines of the shape "
             "`tick at position P, N ms after its deadline`, the first a tick; keep these shapes so this test "
             "can find them")
    program = "\n".join(line[3:] if line.startswith("   ") else line for line in blocks[0].split("\n"))
    return program, said


def wait_for_lines(process, path, count, what):
    """The first `count` lines of the file `process` prints to, once it has printed them. A
    program that ended before them fails at once, with what it printed."""
    deadline = time.monotonic() + WAIT_SECONDS
    while True:
        with open(path, encoding="utf-8") as f:
            lines = f.read().split("\n")
        if len(lines) > count:
            return lines[:count]
        if process.poll() is not None:
            fail(f"{what}: the program ended with code {process.returncode} before its line {count}; "
                 f"it printed {lines}")
        if time.monotonic() > deadline:
            fail(f"{what}: no line {count} after {WAIT_SECONDS:.0f} s; the program printed {lines}")
        time.sleep(0.02)


def stop(process, sent):
    """Stops a process this program started, by its own id, and waits for it."""
    if process.poll() is None:
        process.send_signal(sent)
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()


def main():
    if len(sys.argv) != 4:
        print("usage: howto_timers.py <the cavalry binary> <a directory of its own> <first port>", file=sys.stderr)
        sys.exit(2)
    binary, directory, first_port = os.path.abspath(sys.argv[1]), sys.argv[2], int(sys.argv[3])
    program, said = page()
    os.makedirs(directory, exist_ok=True)
    source = os.path.join(directory, "tick_service.py")
    with open(source, "w", encoding="utf-8") as f:
        f.write(program)

    members = "|".join(
        ",".join([str(member)] + [f"127.0.0.1:{10000 + (first_port + member * 5 + i) % 10000}" for i in range(5)])
        for member in range(MEMBERS))
    environment = dict(os.environ, CAVALRY_CLUSTER_MEMBERS=members,
                       PYTHONPATH=os.path.join(ROOT, "clients", "python"))
    started = []

    def member_dir(member):
        return os.path.join(directory, f"member-{member}")

    def run_service(member, name):
        """The page's step 2: the program beside one member, its output in a file."""
        path = os.path.join(directory, name)
        with open(path, "w", encoding="utf-8") as out:
            # A program started from a shell with no terminal can inherit an ignored
            # SIGINT, and Python then leaves it ignored: the page's Ctrl-C needs it.
            process = subprocess.Popen(
                [sys.executable, source, member_dir(member)], stdout=out, stderr=subprocess.STDOUT, env=environment,
                preexec_fn=lambda: signal.signal(signal.SIGINT, signal.SIG_DFL))
        started.append(process)
        return process, path

    try:
        for member in range(MEMBERS):
            with open(os.path.join(directory, f"member-{member}.out"), "w", encoding="utf-8") as out:
                started.append(subprocess.Popen(
                    [binary, "run", "--dir", member_dir(member), "--log-dir", member_dir(member) + "-log"],
                    stdout=out, stderr=subprocess.STDOUT,
                    env=dict(environment, CAVALRY_CLUSTER_MEMBER_ID=str(member))))
        deadline = time.monotonic() + WAIT_SECONDS
        while True:
            doctor = subprocess.run(
                [binary, "doctor", "--dir", member_dir(1), "--log-dir", member_dir(1) + "-log"],
                capture_output=True, text=True, timeout=WAIT_SECONDS, check=False,
                env=dict(environment, CAVALRY_CLUSTER_MEMBER_ID="1"))
            if "election closed" in doctor.stdout:
                break
            if time.monotonic() > deadline:
                fail(f"member 1 did not close its election in {WAIT_SECONDS:.0f} s; doctor said: {doctor.stdout}")
            time.sleep(0.1)

        # 1. The program beside each member: the same lines, of the page's shape.
        services = [run_service(member, f"service-{member}.out") for member in range(MEMBERS)]
        printed = [wait_for_lines(process, path, LINES, f"the program beside member {member}")
                   for member, (process, path) in enumerate(services)]
        read = []
        for line in printed[0]:
            match = LINE.fullmatch(line)
            if match is None:
                fail(f"the program beside member 0 printed `{line}`, which is no line of the page's shape, "
                     f"`{said[0]}`: a deadline handed over before its time prints a number below 0")
            read.append((match.group(1), int(match.group(2)), int(match.group(3))))
        if printed[0][0].split(",")[0] != said[0].split(",")[0]:
            fail(f"the program's first line is `{printed[0][0]}`, and the page says `{said[0]}` for a cluster "
                 f"that has committed nothing else: another deadline, or another position")
        if [position for _, position, _ in read] != sorted(position for _, position, _ in read):
            fail(f"the program beside member 0 was handed its deadlines at positions that fall: {printed[0]}")
        for member in range(1, MEMBERS):
            if printed[member] != printed[0]:
                fail(f"the program beside member {member} printed {printed[member]} and the one beside member 0 "
                     f"{printed[0]}; the page says the same lines beside every member")

        # 2. Ctrl-C, and the program again: its earlier lines, from the log's start.
        stop(services[0][0], signal.SIGINT)
        again, path = run_service(0, "service-0-again.out")
        replayed = wait_for_lines(again, path, LINES, "the program's second run beside member 0")
        if replayed != printed[0]:
            fail(f"the program's second run printed {replayed}, and its first {printed[0]}; the page says the "
                 f"lines it printed before, unchanged")
    finally:
        for process in reversed(started):
            stop(process, signal.SIGINT if process.args[0] == sys.executable else signal.SIGTERM)

    print(f"conformance/python: {PAGE}: its program printed {printed[0]} beside each of three members of its "
          "own, and the same again when the one beside member 0 was run again")
    print(f"conformance/python: {PAGE} ok")


if __name__ == "__main__":
    main()
