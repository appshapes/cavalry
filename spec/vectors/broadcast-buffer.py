#!/usr/bin/env python3
"""Write spec/vectors/broadcast-buffer.json: the test vectors for spec/broadcast-buffer.md,
version 0. The reference encoder, standard library only. Run from the repository root:

    python3 spec/vectors/broadcast-buffer.py

Four regions (spec/broadcast-buffer.md, "Vectors"): `header` vectors carry the 128-byte
header at the start of the trailer and the file length the reader sees; `record` vectors
one record's bytes with the ring index and capacity it sits at; `transmit` and `receive`
vectors are arithmetic, with inputs and no bytes. See spec/README.md, "Conventions".
"""

import json
import struct
from pathlib import Path

MAGIC = b"CVLYBCST"
HEADER = struct.Struct("<8siiqq96s")  # magic, version, capacity, creator pid, start timestamp, reserved
RECORD_HEADER = struct.Struct("<ii")  # length, type
HEADER_LENGTH, TRAILER_LENGTH = 128, 512
RECORD_HEADER_LENGTH = RECORD_ALIGNMENT = 8
MIN_CAPACITY, MAX_CAPACITY = 4096, 1 << 30
PADDING = -1
assert HEADER.size == HEADER_LENGTH and RECORD_HEADER.size == RECORD_HEADER_LENGTH


def align(value, alignment):
    return (value + alignment - 1) & ~(alignment - 1)


def max_message_length(capacity):
    return capacity // 8


# ---- header -----------------------------------------------------------------------

def header(capacity=MIN_CAPACITY, version=0, creator_pid=4242, start_timestamp=1_760_000_000_000_000_000,
           magic=MAGIC):
    return {"magic": magic.hex(), "version": version, "capacity": capacity,
            "creator_pid": creator_pid, "start_timestamp": start_timestamp}


def encode_header(h):
    return HEADER.pack(bytes.fromhex(h["magic"]), h["version"], h["capacity"], h["creator_pid"],
                       h["start_timestamp"], b"")


def file_length(capacity):
    return capacity + TRAILER_LENGTH


def valid_header(name, h, length=None):
    return {"name": name, "region": "header", "bytes": encode_header(h).hex(),
            "file_length": file_length(h["capacity"]) if length is None else length, "fields": h}


def invalid_header(name, error, raw, length):
    return {"name": name, "region": "header", "bytes": raw.hex(), "file_length": length, "error": error}


# ---- records ----------------------------------------------------------------------

def encode_record(length, type_, message=b""):
    aligned = align(length, RECORD_ALIGNMENT) if length > 0 else RECORD_HEADER_LENGTH
    raw = RECORD_HEADER.pack(length, type_) + message
    return raw + bytes(max(0, aligned - len(raw)))


def record_error(length, type_, capacity, index):
    """None when the record header at `index` is within the bounds of
    spec/broadcast-buffer.md, "Record", else the reason."""
    if length < RECORD_HEADER_LENGTH or align(length, RECORD_ALIGNMENT) > capacity - index:
        return "record"
    if type_ == PADDING:
        return None if length == capacity - index else "record"
    if type_ < 1 or length - RECORD_HEADER_LENGTH > max_message_length(capacity):
        return "record"
    return None


def record_fields(length, type_):
    aligned = align(length, RECORD_ALIGNMENT)
    if type_ == PADDING:
        return {"length": length, "type": type_, "state": "padding", "aligned_length": aligned}
    return {"length": length, "type": type_, "state": "message", "aligned_length": aligned,
            "message_length": length - RECORD_HEADER_LENGTH}


def record(name, length, type_, message=b"", capacity=MIN_CAPACITY, index=0):
    raw = encode_record(length, type_, message)
    vector = {"name": name, "region": "record", "bytes": raw.hex(), "capacity": capacity, "index": index}
    error = record_error(length, type_, capacity, index)
    if error:
        vector["error"] = error
    else:
        vector["fields"] = record_fields(length, type_)
    return vector


# ---- transmit ---------------------------------------------------------------------

def transmit(name, capacity, tail, message_length):
    """One transmit as spec/broadcast-buffer.md, "Transmitting", computes it."""
    inputs = {"capacity": capacity, "tail": tail, "message_length": message_length}
    vector = {"name": name, "region": "transmit", "inputs": inputs}
    if message_length < 0 or message_length > max_message_length(capacity):
        vector["error"] = "message"
        return vector
    record_length = RECORD_HEADER_LENGTH + message_length
    aligned = align(record_length, RECORD_ALIGNMENT)
    new_tail = tail + aligned
    index = tail & (capacity - 1)
    to_end = capacity - index
    if to_end < aligned:
        tail_intent = new_tail + to_end
        padding = {"index": index, "length": to_end}
        tail += to_end
        index = 0
    else:
        tail_intent = new_tail
        padding = None
    vector["fields"] = {"record_length": record_length, "aligned_length": aligned, "tail_intent": tail_intent,
                        "padding": padding, "index": index, "latest": tail, "tail_after": tail + aligned}
    return vector


# ---- receive ----------------------------------------------------------------------

def tail_error(capacity, next_record, tail, tail_intent, latest):
    """The `tail` check a receiver runs on the three trailer fields before it uses them."""
    if tail < 0 or tail % RECORD_ALIGNMENT or latest % RECORD_ALIGNMENT or tail_intent % RECORD_ALIGNMENT:
        return True
    if tail_intent < tail or tail_intent > tail + capacity:
        return True
    if tail == 0:
        return latest != 0
    return latest >= tail or latest < tail - capacity or next_record > tail


def receive(name, capacity, next_record, tail, tail_intent, latest, record=None, record_at_zero=None,
            tail_intent_after=None):
    """One receive as spec/broadcast-buffer.md, "Receiving", computes it. `record` is the
    length and type at the cursor's index, `record_at_zero` those at index 0 when the first
    is padding, and `tail_intent_after` the tail intent the receiver loads after it copied
    the message, when the transmitter moved meanwhile."""
    inputs = {"capacity": capacity, "next_record": next_record, "tail": tail, "tail_intent": tail_intent,
              "latest": latest, "record": record, "record_at_zero": record_at_zero,
              "tail_intent_after": tail_intent_after}
    vector = {"name": name, "region": "receive", "inputs": inputs}
    if tail_error(capacity, next_record, tail, tail_intent, latest):
        vector["error"] = "tail"
        return vector
    if tail == next_record:
        vector["fields"] = {"result": "none", "cursor": next_record, "next_record": next_record}
        return vector
    lapped = {"result": "lapped", "cursor": latest, "next_record": latest}
    cursor = next_record
    if cursor + capacity <= tail_intent:
        vector["fields"] = lapped
        return vector
    intent_after = tail_intent if tail_intent_after is None else tail_intent_after
    index = cursor & (capacity - 1)
    length, type_ = record
    if record_error(length, type_, capacity, index):
        if cursor + capacity <= intent_after:
            vector["fields"] = lapped
        else:
            vector["error"] = "record"
        return vector
    following = cursor + align(length, RECORD_ALIGNMENT)
    if type_ == PADDING:
        cursor, index = following, 0
        length, type_ = record_at_zero
        if record_error(length, type_, capacity, index):
            if cursor + capacity <= intent_after:
                vector["fields"] = lapped
            else:
                vector["error"] = "record"
            return vector
        following = cursor + align(length, RECORD_ALIGNMENT)
    if cursor + capacity <= intent_after:
        vector["fields"] = lapped
        return vector
    vector["fields"] = {"result": "message", "cursor": cursor, "index": index, "next_record": following,
                        "type": type_, "message_length": length - RECORD_HEADER_LENGTH}
    return vector


# ---- the vectors ------------------------------------------------------------------

C = MIN_CAPACITY
plain = header()
VECTORS = [
    # header
    valid_header("header-capacity-4096", plain),
    valid_header("header-capacity-1g", header(capacity=MAX_CAPACITY, creator_pid=1, start_timestamp=0)),
    invalid_header("too-short-file", "too-short", encode_header(plain), TRAILER_LENGTH - 1),
    invalid_header("too-short-bytes", "too-short", encode_header(plain)[:127], file_length(C)),
    invalid_header("not-ready", "not-ready", encode_header(header(magic=b"\0" * 8)), file_length(C)),
    invalid_header("magic-counters-file", "magic", encode_header(header(magic=b"CVLYCNTR")), file_length(C)),
    invalid_header("magic-command-ring", "magic", encode_header(header(magic=b"CVLYCMDR")), file_length(C)),
    invalid_header("version-1", "version", encode_header(header(version=1)), file_length(C)),
    invalid_header("capacity-not-power-of-two", "capacity", encode_header(header(capacity=4097)), file_length(4097)),
    invalid_header("capacity-below-minimum", "capacity", encode_header(header(capacity=2048)), file_length(2048)),
    invalid_header("capacity-above-maximum", "capacity", encode_header(header(capacity=-(1 << 31))), file_length(C)),
    invalid_header("capacity-negative", "capacity", encode_header(header(capacity=-4096)), file_length(C)),
    invalid_header("length-short", "length", encode_header(plain), file_length(C) - 1),
    invalid_header("length-long", "length", encode_header(plain), file_length(C) + 8),
    invalid_header("version-before-capacity", "version", encode_header(header(version=1, capacity=4097)),
                   file_length(4097)),
    # record
    record("record-message", 24, 1, b"publication ready"[:16]),
    record("record-message-aligns", 13, 7, b"hello"),
    record("record-message-empty", 8, 3),
    record("record-message-max", 8 + max_message_length(C), 2, b"m" * max_message_length(C)),
    record("record-padding-to-the-end", 8, PADDING, index=C - 8),
    record("record-padding-long", 2048, PADDING, index=2048),
    record("record-length-zero", 0, 0),
    record("record-length-below-header", 4, 1),
    record("record-length-past-the-end", 16, 1, b"12345678", index=C - 8),
    record("record-type-zero", 24, 0, b"publication ready"[:16]),
    record("record-type-below-padding", 24, -2, b"publication ready"[:16]),
    record("record-padding-not-to-the-end", 16, PADDING, index=2048),
    record("record-message-over-maximum", 8 + max_message_length(C) + 1, 2, b"m" * (max_message_length(C) + 1)),
    # transmit
    transmit("transmit-first-record", C, 0, 16),
    transmit("transmit-aligns-length", C, 24, 13),
    transmit("transmit-empty-message", C, 48, 0),
    transmit("transmit-max-message", C, 56, max_message_length(C)),
    transmit("transmit-message-too-long", C, 56, max_message_length(C) + 1),
    transmit("transmit-wraps-with-padding", C, C - 8, 16),
    transmit("transmit-fits-exactly-at-the-end", C, C - 24, 16),
    transmit("transmit-second-lap", C, 3 * C + 1024, 16),
    # receive
    receive("receive-nothing", C, 24, 24, 24, 0),
    receive("receive-first-record", C, 0, 24, 24, 0, record=(24, 1)),
    receive("receive-second-record", C, 24, 48, 48, 24, record=(21, 7)),
    receive("receive-joins-at-latest", C, C + 24, C + 48, C + 48, C + 24, record=(24, 1)),
    receive("receive-skips-padding", C, C - 8, C + 24, C + 24, C, record=(8, PADDING), record_at_zero=(24, 1)),
    receive("receive-lapped", C, 0, 2 * C, 2 * C, 2 * C - 24),
    receive("receive-lapped-while-copying", C, 0, 24, 24, 0, record=(24, 1), tail_intent_after=C),
    receive("receive-lapped-record-looks-corrupt", C, 0, 24, 24, 0, record=(4, 1), tail_intent_after=C + 8),
    receive("receive-record-length-below-header", C, 0, 24, 24, 0, record=(4, 1)),
    receive("receive-record-type-zero", C, 0, 24, 24, 0, record=(24, 0)),
    receive("receive-padding-not-to-the-end", C, 0, 32, 32, 0, record=(16, PADDING)),
    receive("receive-tail-behind-cursor", C, 48, 24, 24, 0),
    receive("receive-tail-intent-below-tail", C, 0, 24, 16, 0),
    receive("receive-tail-intent-too-far-ahead", C, 0, 24, C + 32, 0),
    receive("receive-latest-at-tail", C, 0, 24, 24, 24),
    receive("receive-tail-not-aligned", C, 0, 20, 24, 0),
]


def main():
    out = Path(__file__).with_suffix(".json")
    doc = {"spec": "spec/broadcast-buffer.md", "version": 0,
           "generated_by": "spec/vectors/broadcast-buffer.py; do not edit by hand", "vectors": VECTORS}
    out.write_text(json.dumps(doc, indent=2) + "\n")
    print(f"{out}: {len(VECTORS)} vectors")


if __name__ == "__main__":
    main()
