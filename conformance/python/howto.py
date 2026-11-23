#!/usr/bin/env python3
"""Runs the program of docs/how-to/take-a-services-snapshot.md as the page's steps run it;
`run.sh` calls it after the snapshot round, so the page's program is a test (CLAUDE.md,
"Documentation"; .claude/rules/docs.md, "Code in docs"; ADR 0149).

    howto.py <the cavalry binary> <a directory of its own> <first port>

It takes the page's one Python block and the three lines the page says the reader will
see, starts a cluster of its own, three `cavalry run` members on the loopback with
fifteen ports counted from <first port> in 10000 to 19999, as run.sh counts its own, and
follows the page:

1. The program runs beside member 0 and prints the page's first line, a start from
   position 0.
2. `cavalry snapshot` is run beside each member, and exits 0 beside one. The program
   prints the page's second line, with the position the command printed.
3. The program is stopped with the signal Ctrl-C sends and run again, and prints the
   page's third line: a start from the snapshot's position.

It exits 1 with what it found unless each line is the page's. The cluster is its own
because the round's cluster lists a snapshot whose state is the round's, not the page's.
What differs from the page: the directories and the ports, the wait for the election by
`cavalry doctor`, and the command run beside every member where the page names the one
that leads. Every wait is bounded at 60 s, and every process it started is stopped by its
own process id when it ends. Python 3.9 standard library only (ADR 0003).
"""

import os
import re
import signal
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
PAGE = os.path.join("docs", "how-to", "take-a-services-snapshot.md")
MEMBERS = 3
WAIT_SECONDS = 60.0


def fail(what):
    print(f"conformance/python: {PAGE}: {what}", file=sys.stderr)
    sys.exit(1)


def page():
    """The page's program, and the three lines it says the reader will see."""
    with open(os.path.join(ROOT, PAGE), encoding="utf-8") as f:
        text = f.read()
    blocks = re.findall(r"^   ```python\n(.*?)^   ```$", text, flags=re.S | re.M)
    flat = " ".join(text.split())
    first = re.search(r"`(started at position 0 with a total of \d+)`", flat)
    taken = re.search(r"`(snapshot at position (\d+): written True)`", flat)
    again = re.search(r"`(started at position ([1-9]\d*) with a total of \d+)`", flat)
    if len(blocks) != 1 or first is None or taken is None or again is None:
        fail("the page has not one Python block indented by three spaces, or not the three lines "
             "`started at position 0 with a total of N`, `snapshot at position P: written True` and "
             "`started at position P with a total of N`; keep these shapes so this test can find them")
    if taken.group(2) != again.group(2):
        fail(f"the page's snapshot is at {taken.group(2)} and its second start at {again.group(2)}")
    program = "\n".join(line[3:] if line.startswith("   ") else line for line in blocks[0].split("\n"))
    return program, first.group(1), taken.group(1), again.group(1), int(taken.group(2))


def wait_for_line(process, path, number, what):
    """Line `number` of the file `process` prints to, once it has printed it. A program
    that ended before the line fails at once, with what it printed."""
    deadline = time.monotonic() + WAIT_SECONDS
    while True:
        with open(path, encoding="utf-8") as f:
            lines = f.read().split("\n")
        if len(lines) > number:
            return lines[number - 1]
        if process.poll() is not None:
            fail(f"{what}: the program ended with code {process.returncode} before its line {number}; "
                 f"it printed {lines}")
        if time.monotonic() > deadline:
            fail(f"{what}: no line {number} after {WAIT_SECONDS:.0f} s; the program printed {lines}")
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
        print("usage: howto.py <the cavalry binary> <a directory of its own> <first port>", file=sys.stderr)
        sys.exit(2)
    binary, directory, first_port = os.path.abspath(sys.argv[1]), sys.argv[2], int(sys.argv[3])
    program, first, taken, again, at = page()
    os.makedirs(directory, exist_ok=True)
    source = os.path.join(directory, "counter_service.py")
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

    def run_service(name):
        """The page's step 2: the program beside member 0, its output in a file."""
        path = os.path.join(directory, name)
        with open(path, "w", encoding="utf-8") as out:
            # A program started from a shell with no terminal can inherit an ignored
            # SIGINT, and Python then leaves it ignored: the page's Ctrl-C needs it.
            process = subprocess.Popen(
                [sys.executable, source, member_dir(0)], stdout=out, stderr=subprocess.STDOUT, env=environment,
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

        # 1. The program beside member 0: a start from the log's start.
        service, printed = run_service("service-1.out")
        line = wait_for_line(service, printed, 1, "the program's first run")
        if line != first:
            fail(f"the program's first run printed `{line}`, and the page says `{first}`")

        # 2. `cavalry snapshot` beside each member: one leads, and the program gives its state.
        positions = []
        for member in range(MEMBERS):
            ran = subprocess.run([binary, "snapshot", "--dir", member_dir(member)], capture_output=True,
                                 text=True, timeout=WAIT_SECONDS, check=False)
            prefix = f"member {member} lists a snapshot of log position "
            if ran.returncode == 0 and ran.stdout.startswith(prefix):
                positions.append(int(ran.stdout[len(prefix):].split(":")[0].split(",")[0]))
            elif ran.returncode != 1:
                fail(f"`cavalry snapshot` beside member {member}, exit {ran.returncode}: {ran.stdout}{ran.stderr}")
        if positions != [at]:
            fail(f"`cavalry snapshot` printed the positions {positions} beside the three members, and the page "
                 f"says {at} for a cluster that has committed nothing else")
        line = wait_for_line(service, printed, 2, "the program at the leader's request")
        if line != taken:
            fail(f"the program printed `{line}` at the request, and the page says `{taken}`")

        # 3. Ctrl-C, and the program again: a start from the snapshot.
        stop(service, signal.SIGINT)
        service, printed = run_service("service-2.out")
        line = wait_for_line(service, printed, 1, "the program's second run")
        if line != again:
            fail(f"the program's second run printed `{line}`, and the page says `{again}`")
        stop(service, signal.SIGINT)
    finally:
        for process in reversed(started):
            stop(process, signal.SIGTERM)

    print(f"conformance/python: {PAGE}: its program printed `{first}`, `{taken}` and, run again, `{again}`, "
          "beside member 0 of three members of its own")
    print(f"conformance/python: {PAGE} ok")


if __name__ == "__main__":
    main()
