#!/usr/bin/env python3
"""Runs the program of docs/how-to/schedule-a-services-timer.md as the page's steps run it;
`run.sh` calls it after the snapshot page's program, so the page's program is a test
(CLAUDE.md, "Documentation"; .claude/rules/docs.md, "Code in docs"; ADR 0157).

    howto_timers.py <the cavalry binary> <a directory of its own> <first port>

It takes the page's one Python block and the lines the page says the reader will see,
starts a cluster of its own, three `cavalry run` members on the loopback with fifteen
ports counted from <first port> in 10000 to 19999, as run.sh counts its own, and follows
the page:

1. As a reader's cluster has, this one runs for more than three seconds before the
   program starts, so that both of the program's first deadlines are past.
2. The program runs beside each of the three members. Each prints the page's opening:
   a tick and a tock at one position, the log's end then, the tick 1,000 ms later than
   the tock; and then a tick at a position some timer events above. The three programs
   print the same lines. The page's own example lines are held to the same shape.
3. The program beside the member that leads is stopped with the signal Ctrl-C sends and
   run again. It prints every line it printed before, unchanged, from the log's start,
   and then two lines that it had not printed, which another member's program prints
   too. The leader appends one event at the most while no service runs beside it, the
   timer it was last told of, so the second of the two came after the restart: the
   program goes on with the others.

It exits 1 with what it found otherwise. The cluster is its own so that the log holds
nothing but its first entry when the program starts. What differs from the page: the
directories and the ports, the wait for the election by `cavalry doctor`, the wait of
step 1 where a reader's cluster is minutes old, and the choice of the leader's program
for the restart, the case in which the other programs wait. That no line comes before
its deadline is not checked here: the program prints a deadline only once the cluster's
time has passed it. Every wait is bounded at 60 s, and every process it started is
stopped by its own process id when it ends. Python 3.9 standard library only (ADR 0003).
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
PAST_SECONDS = 3.5    # how long the cluster runs before the program: past its deadlines of 2 s and 3 s
LINES = 3             # the page's opening: a tick and a tock at once, then a tick
FIRST_POSITION = 96   # where a log that holds one new leadership term ends
EVENT_BYTES = 64      # a timer event in the log
LINE = re.compile(r"(tick|tock) at position (\d+), (\d+) ms after its deadline")


def fail(what):
    print(f"conformance/python: {PAGE}: {what}", file=sys.stderr)
    sys.exit(1)


def opening(lines, whose):
    """Fails unless `lines` open as the page says: a tick and a tock at one position, the
    tick 1,000 ms later than the tock, since both deadlines were counted from one entry
    and handed over with one event; then a tick, some timer events above. Returns the
    first position."""
    read = []
    for line in lines[:LINES]:
        match = LINE.fullmatch(line)
        if match is None:
            fail(f"{whose} `{line}`, which is no line of the shape `tick at position P, N ms after its deadline`")
        read.append((match.group(1), int(match.group(2)), int(match.group(3))))
    (first, at, late), (second, at_too, late_too), (third, later, _) = read
    if (first, second, third) != ("tick", "tock", "tick") or at != at_too or late != late_too + 1000:
        fail(f"{whose} {lines[:LINES]}; the opening is a tick and a tock at one position, the tick 1,000 ms later "
             f"than the tock, and then a tick")
    if later <= at or (later - at) % EVENT_BYTES != 0:
        fail(f"{whose} {lines[:LINES]}; the third line's position is not one or more timer events, of "
             f"{EVENT_BYTES} bytes each, above the first's")
    return at


def page():
    """The page's program, and the lines it says the reader will see."""
    with open(os.path.join(ROOT, PAGE), encoding="utf-8") as f:
        text = f.read()
    blocks = re.findall(r"^   ```python\n(.*?)^   ```$", text, flags=re.S | re.M)
    said = [match.group(0) for match in LINE.finditer(" ".join(text.split()))]
    if len(blocks) != 1 or len(said) < LINES:
        fail(f"the page has not one Python block indented by three spaces, or not {LINES} lines of the shape "
             "`tick at position P, N ms after its deadline`; keep these shapes so this test can find them")
    opening(said, "the page says the reader will see")
    program = "\n".join(line[3:] if line.startswith("   ") else line for line in blocks[0].split("\n"))
    return program, said


def printed(path):
    """The whole lines of the file a program prints to."""
    with open(path, encoding="utf-8") as f:
        return f.read().split("\n")[:-1]


def wait_for_lines(process, path, count, what):
    """The lines of the file `process` prints to, once it has printed `count` of them. A
    program that ended before them fails at once, with what it printed."""
    deadline = time.monotonic() + WAIT_SECONDS
    while True:
        lines = printed(path)
        if len(lines) >= count:
            return lines
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

    def doctor(member):
        """What `cavalry doctor` prints beside one member."""
        return subprocess.run(
            [binary, "doctor", "--dir", member_dir(member), "--log-dir", member_dir(member) + "-log"],
            capture_output=True, text=True, timeout=WAIT_SECONDS, check=False,
            env=dict(environment, CAVALRY_CLUSTER_MEMBER_ID=str(member))).stdout

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
            said_by = [doctor(member) for member in range(MEMBERS)]
            leaders = [member for member in range(MEMBERS) if "the leader in term" in said_by[member]]
            if len(leaders) == 1 and all("election closed" in said for said in said_by):
                leader = leaders[0]
                break
            if time.monotonic() > deadline:
                fail(f"the three members did not close one election in {WAIT_SECONDS:.0f} s; doctor said: {said_by}")
            time.sleep(0.1)

        # 1. A cluster that has run for a while: past both of the program's first deadlines.
        time.sleep(PAST_SECONDS)

        # 2. The program beside each member: the page's opening, the same beside each.
        services = [run_service(member, f"service-{member}.out") for member in range(MEMBERS)]
        first = [wait_for_lines(process, path, LINES, f"the program beside member {member}")[:LINES]
                 for member, (process, path) in enumerate(services)]
        at = opening(first[0], "the program beside member 0 printed")
        if at != FIRST_POSITION:
            fail(f"the program's first lines are at position {at}: {first[0]}; a log that holds one new "
                 f"leadership term and nothing else ends at {FIRST_POSITION}")
        for member in range(1, MEMBERS):
            if first[member] != first[0]:
                fail(f"the program beside member {member} printed {first[member]} and the one beside member 0 "
                     f"{first[0]}; the page says the same lines beside every member")

        # 3. Ctrl-C beside the leader, and the program again: its earlier lines from the
        # log's start, and then two it had not printed, which another program prints too.
        # The leader's member holds one timer and nobody schedules it again while its
        # service is stopped, so one event at the most is appended meanwhile, and the
        # second new line is of an event appended once the service had told it again.
        other = (leader + 1) % MEMBERS
        more = 2
        stop(services[leader][0], signal.SIGINT)
        before = [line for line in printed(services[leader][1]) if LINE.fullmatch(line)]
        again, path = run_service(leader, f"service-{leader}-again.out")
        replayed = wait_for_lines(again, path, len(before) + more, f"the program's second run beside member {leader}")
        if replayed[:len(before)] != before:
            fail(f"the program's second run beside member {leader} printed {replayed[:len(before)]}, and its first "
                 f"{before}; the page says the lines it printed before, unchanged")
        others = wait_for_lines(services[other][0], services[other][1], len(before) + more,
                                f"the program beside member {other}, after the leader's program ran again")
        if others[:len(before) + more] != replayed[:len(before) + more]:
            fail(f"after its second run the program beside member {leader}, the leader, printed "
                 f"{replayed[:len(before) + more]} and the one beside member {other} {others[:len(before) + more]}; "
                 f"the page says it goes on with the others")
    finally:
        for process in reversed(started):
            stop(process, signal.SIGINT if process.args[0] == sys.executable else signal.SIGTERM)

    print(f"conformance/python: {PAGE}: its program printed {first[0]} beside each of three members of its own; "
          f"stopped beside member {leader}, the leader, and run again, it printed its {len(before)} lines again and "
          f"then {replayed[len(before):len(before) + more]}, as the program beside member {other} did")
    print(f"conformance/python: {PAGE} ok")


if __name__ == "__main__":
    main()
