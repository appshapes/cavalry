#!/usr/bin/env python3
"""The Python client's conformance runner (clients/README.md; ADR 0046, card 205): the
ten-minute path's shape through `clients/python`, as `conformance/c/quickstart.c` runs it
through the header. Against a driver at `CAVALRY_DIR` it checks the ABI version both ways,
records stream 1001 on `cavalry:ipc`, publishes 1,000 messages of 64 bytes by claim and
commit, each carrying its index and a pattern, and reads them back through the fragment
handler once and in order, with the header's type, flags, version, frame length, term
offset and session id checked against spec/frame.md; waits for the recording to sync to
the stream's end and stops it; checks where a handler's exception goes and that `offer`
refuses what is not bytes, on stream 1003 (`edges`); does the same 1,000 over
`cavalry:udp` on the loopback; and replays the recording onto stream 2001, each frame at
the position the recording holds it at, with the end reported. Every wait is bounded at
10 s, and a wait that runs out says how far it got. It exits 1 with the function, the code
and `cavalry_errmsg` when a call failed, or with the code's name when a result code did.

Then the report, printed and never gated, from two rounds that check nothing else. The
latency round (`latency`): 100,000 messages over `cavalry:ipc` on stream 1004, one at a
time on this thread, each carrying the publisher's `time.perf_counter_ns` from before its
commit, which the handler subtracts from the clock it reads first; the one-way p50, p99,
p99.99 and max, nearest rank, so ten samples lie above the p99.99. The allocation round
(`allocations`): 10,000 messages on stream 1002 whose handler only counts and whose
publisher writes one constant, with `tracemalloc` started for this round alone; the
allocations per message on this side of the boundary, the client's code and the
runtime's together, counted with what the runtime counts: `sys.getallocatedblocks` read
before and after the round with nothing else between, and `tracemalloc`'s blocks and
bytes, its own snapshots left out. Both are net of what was freed: an object made and
freed within its message is not seen, and clients/python/README.md lists those by
construction. Python 3.9 standard library only (ADR 0003).
"""

import ctypes
import os
import socket
import sys
import time
import tracemalloc

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "clients", "python"))
import cavalry  # noqa: E402
from cavalry import _abi  # noqa: E402

MESSAGES = 1000
LATENCY_MESSAGES = 100_000
ALLOCATION_MESSAGES = 10_000
BODY = 64
STREAM = 1001
REPLAY_STREAM = 2001
WAIT_SECONDS = 10.0
CODE_NAMES = {
    cavalry.NOT_CONNECTED: "NOT_CONNECTED", cavalry.BACK_PRESSURED: "BACK_PRESSURED",
    cavalry.ADMIN_ACTION: "ADMIN_ACTION", cavalry.CLOSED: "CLOSED",
    cavalry.MAX_POSITION_EXCEEDED: "MAX_POSITION_EXCEEDED", cavalry.ERROR: "ERROR",
}


def fail(what):
    print(f"conformance/python: {what}", file=sys.stderr)
    sys.exit(1)


def deadline():
    return time.monotonic() + WAIT_SECONDS


class Receiver:
    """What a subscription has received: the next index expected, and the frames' term ids
    and offsets kept from the handler for the positions."""

    def __init__(self, name, subscription):
        self.name = name
        self.subscription = subscription
        self.next = 0
        self.session_id = None
        self.term_ids = []
        self.term_offsets = []
        self.positions = []
        self.constants = None

    def on_fragment(self, body, header):
        if self.next >= MESSAGES:
            fail(f"{self.name}: a fragment after all {MESSAGES} messages")
        index = int.from_bytes(body[:8], "little")
        if index != self.next:
            fail(f"{self.name}: expected message {self.next}, got {index}")
        if len(body) != BODY:
            fail(f"{self.name}: message {index} has {len(body)} bytes, not {BODY}")
        for b in range(8, BODY):
            if body[b] != (index + b) & 0xFF:
                fail(f"{self.name}: message {index} byte {b} is {body[b]}, not {(index + b) & 0xFF}")
        frame = header.values()
        if frame is None:
            fail(f"{self.name}: cavalry_header_values refused message {index}'s header inside the handler")
        # The header against spec/frame.md: a data frame, unfragmented, version 0, the
        # body plus the 32-byte header, aligned to 32 in its term.
        if (frame.type != _abi.CAVALRY_FRAME_TYPE_DATA or frame.flags != _abi.CAVALRY_FLAGS_UNFRAGMENTED
                or frame.version != 0 or frame.frame_length != 32 + BODY or frame.term_offset % 32 != 0):
            fail(f"{self.name}: message {index}'s header reads type {frame.type}, flags {frame.flags}, version "
                 f"{frame.version}, frame length {frame.frame_length}, term offset {frame.term_offset}")
        if self.session_id is None:
            self.session_id = frame.session_id
        elif frame.session_id != self.session_id:
            fail(f"{self.name}: message {index} from session {frame.session_id}, the stream's is {self.session_id}")
        self.term_ids.append(frame.term_id)
        self.term_offsets.append(frame.term_offset)
        self.next += 1

    def take_constants(self):
        """The constants of the image the frames came from, once a poll has read one, for
        the positions (spec/c-abi.md, "A frame's position"); the string is copied now."""
        if self.constants is not None or self.session_id is None:
            return
        for image in self.subscription.images():
            if image.session_id == self.session_id:
                self.constants = image
                return

    def compute_positions(self):
        if self.constants is None:
            fail(f"{self.name}: no image carried session {self.session_id} after {self.next} messages")
        c = self.constants
        for term_id, term_offset in zip(self.term_ids, self.term_offsets):
            term_count = ((term_id - c.initial_term_id) + 2**31) % 2**32 - 2**31  # 32-bit two's complement
            self.positions.append((term_count << c.position_bits_to_shift) + term_offset)

    def expect_no_more(self):
        count = self.subscription.poll(self.on_fragment, 16)
        if count != 0:
            fail(f"{self.name}: {count} more fragment(s) after all {MESSAGES} messages")


def refused(name, position, sent, until, total=MESSAGES):
    """A claim refused with `position`, below zero: fail at once on a code that does not
    pass, ERROR with the ABI's error, CLOSED or MAX_POSITION_EXCEEDED, or once the round's
    10 s have passed, saying how far the round got; else return, to claim again."""
    if position == cavalry.ERROR:
        abi = cavalry.library()
        raise cavalry.CavalryError("cavalry_publication_try_claim", abi.cavalry_errcode(),
                                   abi.cavalry_errmsg().decode("utf-8", "replace"))
    if position in (cavalry.CLOSED, cavalry.MAX_POSITION_EXCEEDED):
        fail(f"{name}: try_claim returned {CODE_NAMES[position]} ({position}) after {sent} of {total} messages")
    if time.monotonic() > until:
        fail(f"{name}: published {sent} of {total} in {WAIT_SECONDS:.0f} s; the last claim returned "
             f"{CODE_NAMES.get(position, position)} ({position})")


def poll_or_fail(name, subscription, handler, received, total=MESSAGES):
    """One poll of up to 16 fragments: the count, or a failure with the subscription's
    fault, saying how far the round got."""
    count = subscription.poll(handler, 16)
    if count < 0:
        code, session_id, position = subscription.fault()
        fail(f"{name}: poll stopped on fault {code} of session {session_id} at position {position} after {received} of "
             f"{total} messages; {cavalry.library().cavalry_errmsg().decode('utf-8', 'replace')}")
    return count


def publish_and_receive(publication, receiver):
    until = deadline()
    i = 0
    while i < MESSAGES:
        # The image joins on a poll; take its constants once a frame from it is read.
        poll_or_fail(receiver.name, receiver.subscription, receiver.on_fragment, receiver.next)
        receiver.take_constants()
        position = publication.try_claim(BODY)
        if position < 0:
            refused(receiver.name, position, i, until)
            continue
        claim = publication.claim
        claim[:8] = i.to_bytes(8, "little")
        claim[8:] = bytes((i + b) & 0xFF for b in range(8, BODY))
        if publication.commit() != position:
            fail(f"{receiver.name}: commit did not return the claim's position {position}")
        i += 1
    until = deadline()
    while receiver.next < MESSAGES:
        poll_or_fail(receiver.name, receiver.subscription, receiver.on_fragment, receiver.next)
        receiver.take_constants()
        if time.monotonic() > until:
            fail(f"{receiver.name}: received {receiver.next} of {MESSAGES} in {WAIT_SECONDS:.0f} s")
    receiver.expect_no_more()
    receiver.compute_positions()


def offer_or_fail(name, publication, message, sent):
    """Offer `message`, retrying a refusal that passes, NOT_CONNECTED, BACK_PRESSURED or
    ADMIN_ACTION, for up to 10 s; the position."""
    until = deadline()
    while True:
        position = publication.offer(message)
        if position >= 0:
            return position
        if position in (cavalry.CLOSED, cavalry.MAX_POSITION_EXCEEDED, cavalry.ERROR) or time.monotonic() > until:
            fail(f"{name}: offer returned {CODE_NAMES.get(position, position)} ({position}) after {sent} messages")


def edges(client):
    """The client's edges, on stream 1003. Where a handler's exception goes
    (clients/python/README.md, "The exception"): ten messages offered, then one poll of
    ten whose handler raises on message 3. The ABI's poll consumes all ten, so every one
    must reach the handler, once and in order, and poll must raise HandlerError with
    `fragments` 10, `raised` 1 and the ValueError as its cause. Then a poll whose handler
    raises a ValueError on one message and SystemExit(2) on the next: poll raises the
    SystemExit as itself. Then what `offer` refuses: a `str`, a `bytearray` and a
    `memoryview` are refused with `ctypes.ArgumentError` and nothing is published."""
    name = "handler exceptions"
    subscription = client.add_subscription("cavalry:ipc", STREAM + 2)
    publication = client.add_publication("cavalry:ipc", STREAM + 2)
    seen = []

    def unexpected(body, header):
        fail(f"{name}: a fragment the test did not expect, after {seen}")

    def message(i):
        return i.to_bytes(8, "little") + bytes(BODY - 8)

    until = deadline()
    while True:
        subscription.poll(unexpected, 10)  # nothing is offered yet; the image joins on a poll
        position = publication.offer(message(0))
        if position >= 0:
            break
        if position in (cavalry.CLOSED, cavalry.MAX_POSITION_EXCEEDED, cavalry.ERROR) or time.monotonic() > until:
            fail(f"{name}: the first offer returned {CODE_NAMES.get(position, position)} ({position})")
    for i in range(1, 10):
        offer_or_fail(name, publication, message(i), i)

    def raises_on_3(body, header):
        index = int.from_bytes(body[:8], "little")
        seen.append(index)
        if index == 3:
            raise ValueError("message 3")

    try:
        subscription.poll(raises_on_3, 10)
        error = None
    except cavalry.HandlerError as raised:
        error = raised
    if error is None:
        fail(f"{name}: a handler that raised on message 3 did not make poll raise HandlerError; it saw {seen}")
    if seen != list(range(10)) or error.fragments != 10 or error.raised != 1 or not isinstance(error.__cause__, ValueError):
        fail(f"{name}: the handler saw {seen}; HandlerError has fragments {error.fragments}, raised {error.raised}, "
             f"cause {error.__cause__!r}; expected messages 0 to 9 once and in order, 10, 1 and the ValueError")

    offer_or_fail(name, publication, message(10), 10)
    offer_or_fail(name, publication, message(11), 11)

    def raises_then_exits(body, header):
        index = int.from_bytes(body[:8], "little")
        seen.append(index)
        if index == 10:
            raise ValueError("message 10")
        sys.exit(2)

    exited = None
    try:
        subscription.poll(raises_then_exits, 10)
    except SystemExit as exit_:
        exited = exit_
    except cavalry.HandlerError as wrapped:
        fail(f"{name}: poll raised HandlerError for a handler's SystemExit: {wrapped}")
    if exited is None or exited.code != 2 or seen != list(range(12)):
        fail(f"{name}: poll raised {exited!r} and the handler saw {seen}; expected SystemExit(2) and messages 0 to 11")
    if subscription.poll(unexpected, 10) != 0:
        fail(f"{name}: a fragment after all 12 messages")
    print(f"conformance/python: {name}: a handler that raised on message 3 of a poll of 10 saw all 10 once, in order, "
          f"and poll raised HandlerError (fragments 10, raised 1, the ValueError as its cause); a SystemExit after a "
          f"ValueError in one poll was raised as itself")

    # What offer refuses: a str, whose wchar_t bytes a `c_void_p` buffer published, and
    # the other forms that are not bytes.
    name = "offer"
    for refused in ("hello", bytearray(b"hello"), memoryview(b"hello")):
        try:
            position = publication.offer(refused)
        except ctypes.ArgumentError:
            continue
        fail(f"{name}: took a {type(refused).__name__} and returned {position}; it takes bytes only")
    if subscription.poll(unexpected, 10) != 0:
        fail(f"{name}: a refused offer published a fragment")
    publication.close()
    subscription.close()
    print(f"conformance/python: {name}: a str, a bytearray and a memoryview were refused with ctypes.ArgumentError and "
          f"nothing was published")


def latency(client):
    """The latency round, for the report: 100,000 messages over cavalry:ipc on stream 1004,
    one at a time on this thread. The publisher writes `perf_counter_ns` into the claim
    just before its commit; the handler reads the clock first and keeps the difference.
    `tracemalloc` is off. The latencies, in order."""
    name = "the latency round"
    subscription = client.add_subscription("cavalry:ipc", STREAM + 3)
    publication = client.add_publication("cavalry:ipc", STREAM + 3)
    latencies = [0] * LATENCY_MESSAGES
    received = [0]

    def on_fragment(body, header):
        now = time.perf_counter_ns()
        latencies[received[0]] = now - int.from_bytes(body[:8], "little")
        received[0] += 1

    until = deadline()
    for i in range(LATENCY_MESSAGES):
        while True:
            position = publication.try_claim(BODY)
            if position >= 0:
                break
            refused(name, position, i, until, LATENCY_MESSAGES)
            poll_or_fail(name, subscription, on_fragment, received[0], LATENCY_MESSAGES)  # the image joins on a poll
        publication.claim[:8] = time.perf_counter_ns().to_bytes(8, "little")
        publication.commit()
        while received[0] <= i:
            poll_or_fail(name, subscription, on_fragment, received[0], LATENCY_MESSAGES)
            if time.monotonic() > until:
                fail(f"{name}: received {received[0]} of {LATENCY_MESSAGES}; message {i} did not arrive in "
                     f"{WAIT_SECONDS:.0f} s")
        until = deadline()  # 10 s for each message, outside the interval it measures
    if subscription.poll(on_fragment, 16) != 0:
        fail(f"{name}: a fragment after all {LATENCY_MESSAGES:,} messages")
    publication.close()
    subscription.close()
    return latencies


def allocations(client):
    """The allocation round, for the report: 10,000 messages over cavalry:ipc on stream
    1002 whose handler only counts and whose publisher writes one constant, so that the
    checks' objects are not in the count, with `tracemalloc` on for this round alone. The
    counts are read in this order, so that neither reads the other's objects: the
    snapshot, the traced memory, the live blocks; the round; the live blocks, the traced
    memory, the snapshot. (messages, live blocks, traced blocks, traced bytes, the peak
    traced above the round's start)."""
    name = "the allocation round"
    subscription = client.add_subscription("cavalry:ipc", STREAM + 1)
    publication = client.add_publication("cavalry:ipc", STREAM + 1)
    received = [0]

    def count(body, header):
        received[0] += 1

    constant = bytes(range(BODY))
    until = deadline()
    while True:
        poll_or_fail(name, subscription, count, received[0], ALLOCATION_MESSAGES)
        position = publication.try_claim(BODY)
        if position >= 0:
            break
        refused(name, position, 0, until, ALLOCATION_MESSAGES)
    publication.claim[:] = constant
    publication.commit()
    sent = 1
    poll_or_fail(name, subscription, count, received[0], ALLOCATION_MESSAGES)

    tracemalloc.start()
    snapshot_before = tracemalloc.take_snapshot()
    tracemalloc.reset_peak()
    traced_before = tracemalloc.get_traced_memory()[0]
    received_before = received[0]
    blocks_before = sys.getallocatedblocks()
    until = deadline()
    while sent < ALLOCATION_MESSAGES:
        position = publication.try_claim(BODY)
        if position >= 0:
            publication.claim[:] = constant
            publication.commit()
            sent += 1
        else:
            refused(name, position, sent, until, ALLOCATION_MESSAGES)
        poll_or_fail(name, subscription, count, received[0], ALLOCATION_MESSAGES)
    while received[0] < ALLOCATION_MESSAGES:
        poll_or_fail(name, subscription, count, received[0], ALLOCATION_MESSAGES)
        if time.monotonic() > until:
            fail(f"{name}: received {received[0]} of {ALLOCATION_MESSAGES} in {WAIT_SECONDS:.0f} s")
    blocks_after = sys.getallocatedblocks()
    traced_after, traced_peak = tracemalloc.get_traced_memory()
    snapshot_after = tracemalloc.take_snapshot()
    tracemalloc.stop()

    measured = ALLOCATION_MESSAGES - received_before
    own = (tracemalloc.Filter(False, tracemalloc.__file__),)  # the snapshots' own objects
    live = snapshot_after.filter_traces(own).compare_to(snapshot_before.filter_traces(own), "lineno")
    if subscription.poll(count, 16) != 0:
        fail(f"{name}: a fragment after all {ALLOCATION_MESSAGES:,} messages")
    publication.close()
    subscription.close()
    return (measured, (blocks_after - blocks_before) / measured, sum(s.count_diff for s in live) / measured,
            (traced_after - traced_before) / measured, traced_peak - traced_before)


def percentile(sorted_values, parts, whole):
    """The nearest-rank percentile `parts / whole`: the smallest value with at least that
    share of the values at or below it, and how many values lie above it."""
    rank = max(1, -(-len(sorted_values) * parts // whole))
    return sorted_values[rank - 1], len(sorted_values) - rank


def free_udp_port():
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def main():
    abi = cavalry.library()
    # What a reader rejects, both ways: another version is refused with the code, and the
    # package's own version is not.
    context = ctypes.c_void_p()
    if (abi.cavalry_context_init_v(ctypes.byref(context), cavalry.ABI_VERSION + 1) == 0
            or abi.cavalry_errcode() != _abi.CAVALRY_ERR_ABI_VERSION):
        fail("a mismatched ABI version was not refused with CAVALRY_ERR_ABI_VERSION")
    if cavalry.abi_version() != cavalry.ABI_VERSION:
        fail(f"the library's ABI version is {cavalry.abi_version()}, the package's {cavalry.ABI_VERSION}")
    # The per-message functions keep the GIL held across the call (`ctypes.PyDLL`'s flag)
    # and every other function releases it (clients/python/README.md, "The call").
    for name in _abi.FUNCTIONS:
        held = bool(getattr(abi, name)._flags_ & ctypes._FUNCFLAG_PYTHONAPI)
        if held != (name in _abi.PER_MESSAGE):
            fail(f"{name} {'keeps' if held else 'releases'} the GIL across the call; the per-message functions "
                 f"{', '.join(_abi.PER_MESSAGE)} keep it and every other function releases it")

    with cavalry.Client() as client:
        print(f"conformance/python: client {client.client_id} on {client.dir}, library {cavalry.version()}, "
              f"ABI version {cavalry.abi_version()}")

        # 1. Record the stream, then publish 1,000 messages over cavalry:ipc and read them
        # back, once and in order.
        ipc_sub = client.add_subscription("cavalry:ipc", STREAM)
        ipc_pub = client.add_publication("cavalry:ipc", STREAM)
        recording = client.start_recording("cavalry:ipc", STREAM)  # the publication first: it records a live one
        ipc = Receiver("cavalry:ipc", ipc_sub)
        publish_and_receive(ipc_pub, ipc)
        end = ipc_pub.position()
        print(f"conformance/python: {MESSAGES} messages over cavalry:ipc by session {ipc.session_id} received once, "
              f"in order; the stream ends at {end}")
        until = deadline()
        while recording.synced_position() < end:
            if time.monotonic() > until:
                fail(f"the recording synced {recording.synced_position()} of {end} in {WAIT_SECONDS:.0f} s")
        client.stop_recording(recording.recording_id)
        recording_id = recording.recording_id
        recording.close()
        ipc_pub.close()

        edges(client)

        # The report's two rounds, which check nothing else.
        latencies = sorted(latency(client))
        measured, live_blocks, traced_blocks, traced_bytes, traced_peak = allocations(client)

        # 2. The same 1,000 over cavalry:udp on the loopback.
        udp_channel = f"cavalry:udp?endpoint=127.0.0.1:{free_udp_port()}"
        udp_sub = client.add_subscription(udp_channel, STREAM)
        udp_pub = client.add_publication(udp_channel, STREAM)
        udp = Receiver(udp_channel, udp_sub)
        publish_and_receive(udp_pub, udp)
        print(f"conformance/python: {MESSAGES} messages over {udp_channel} by session {udp.session_id} received once, "
              f"in order, from {udp.constants.source_identity}")
        udp_pub.close()
        udp_sub.close()

        # 3. Replay the recording onto stream 2001: the same checks, each frame at the
        # position the recording holds it at (ADR 0030), and the end reported.
        replay_sub = client.add_subscription("cavalry:ipc", REPLAY_STREAM)
        replay = client.start_replay(recording_id, _abi.CAVALRY_NULL_POSITION, _abi.CAVALRY_NULL_LENGTH,
                                     REPLAY_STREAM, "cavalry:ipc")
        replayed = Receiver("replay", replay_sub)
        until = deadline()
        while True:
            poll_or_fail("replay", replay_sub, replayed.on_fragment, replayed.next)
            replayed.take_constants()
            done, ended_at, reason = replay.ended()
            if replayed.next >= MESSAGES and done:
                break
            if time.monotonic() > until:
                if done:
                    fail(f"replayed {replayed.next} of {MESSAGES} in {WAIT_SECONDS:.0f} s; the replay ended at "
                         f"{ended_at} with reason {reason}")
                fail(f"replayed {replayed.next} of {MESSAGES} in {WAIT_SECONDS:.0f} s; the replay has not ended, at "
                     f"position {replay.position()}")
        replayed.expect_no_more()
        replayed.compute_positions()
        if ended_at != end or reason != _abi.CAVALRY_REPLAY_STOP_ENDED:
            fail(f"the replay ended at {ended_at} with reason {reason}, not at {end} with {_abi.CAVALRY_REPLAY_STOP_ENDED}")
        for i in range(MESSAGES):
            if replayed.positions[i] != ipc.positions[i]:
                fail(f"message {i} replayed at {replayed.positions[i]}, recorded at {ipc.positions[i]}")
        print(f"conformance/python: {MESSAGES} messages replayed on stream {REPLAY_STREAM} at the positions the "
              f"recording holds them, ended at {ended_at}")
        replay.close()
        replay_sub.close()
        ipc_sub.close()

    # The report, never gated: the laptop's or the runner's cost of this side of the
    # boundary (clients/python/README.md, "The runtime's side").
    p50, _ = percentile(latencies, 50, 100)
    p99, _ = percentile(latencies, 99, 100)
    p9999, above = percentile(latencies, 9999, 10000)
    print(f"conformance/python: report: one-way latency over {len(latencies):,} cavalry:ipc messages, one at a time on "
          f"one thread, tracemalloc off, nearest rank: p50 {p50:,} ns, p99 {p99:,} ns, p99.99 {p9999:,} ns "
          f"({above} samples above it), max {latencies[-1]:,} ns")
    print(f"conformance/python: report: allocations per message, this side of the boundary, over {measured:,} messages "
          f"whose handler counts and whose publisher writes a constant, net of what was freed, so an object made and "
          f"freed within its message is not seen: {live_blocks:+.3f} live blocks (sys.getallocatedblocks, nothing read "
          f"between), {traced_blocks:+.3f} traced blocks and {traced_bytes:+.1f} bytes (tracemalloc, its snapshots "
          f"left out); peak {traced_peak:,} bytes traced above the round's start")
    print("conformance/python: ok")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except cavalry.CavalryError as error:
        fail(f"{error.function} failed with error {error.code}: {error.message}")
    except cavalry.HandlerError as error:
        fail(f"the fragment handler raised: {error.__cause__!r}")
