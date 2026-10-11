#!/usr/bin/env python3
"""Write spec/vectors/command-ring.json: the test vectors for spec/command-ring.md, version 0.
The reference encoder, standard library only. Run from the repository root:

    python3 spec/vectors/command-ring.py

Five regions (spec/command-ring.md, "Vectors"): `header` vectors carry the 128-byte header
at the start of the trailer and the file length the reader sees; `record` vectors one
record's bytes with the ring index and capacity it sits at; `claim`, `unblock` and `read`
vectors are arithmetic, with inputs and no bytes, except that a `read` vector starts from
a ring image. See spec/README.md, "Conventions".
"""

import json
import struct
from pathlib import Path

MAGIC = b"CVLYCMDR"
HEADER = struct.Struct("<8siiqq96s")  # magic, version, capacity, creator pid, start timestamp, reserved
RECORD_HEADER = struct.Struct("<ii")  # length, type
HEADER_LENGTH, TRAILER_LENGTH = 128, 768
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
    """The bytes of a record whose header says `length` and `type_`, padded to the alignment
    when the message is shorter than the aligned length."""
    aligned = align(abs(length), RECORD_ALIGNMENT) if length else RECORD_HEADER_LENGTH
    raw = RECORD_HEADER.pack(length, type_) + message
    return raw + bytes(max(0, aligned - len(raw)))


def record_error(length, type_, capacity, index):
    """None when the record header at `index` is within the bounds of spec/command-ring.md,
    "Record", else the reason."""
    if length == 0:
        return None
    magnitude = abs(length)
    if magnitude < RECORD_HEADER_LENGTH or align(magnitude, RECORD_ALIGNMENT) > capacity - index:
        return "record"
    if type_ == PADDING:
        return None
    if type_ < 1 or magnitude - RECORD_HEADER_LENGTH > max_message_length(capacity):
        return "record"
    return None


def record_fields(length, type_):
    if length == 0:
        return {"length": 0, "type": type_, "state": "unclaimed"}
    aligned = align(abs(length), RECORD_ALIGNMENT)
    if length < 0:
        return {"length": length, "type": type_, "state": "claimed", "aligned_length": aligned}
    if type_ == PADDING:
        return {"length": length, "type": type_, "state": "padding", "aligned_length": aligned}
    return {"length": length, "type": type_, "state": "committed", "aligned_length": aligned,
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


# ---- claim ------------------------------------------------------------------------

def tail_error(tail, head):
    """The `tail` check a party runs against a head it has loaded: head ≤ tail ≤ head +
    capacity is the caller's to add; here the common part."""
    return tail % RECORD_ALIGNMENT != 0 or head % RECORD_ALIGNMENT != 0 or tail < head


def claim(name, capacity, tail, head, head_cache, message_length):
    """One claim as spec/command-ring.md, "Claim, commit and abort", computes it from a
    producer's loads: the tail, the head cache, and the head position when it re-reads it."""
    inputs = {"capacity": capacity, "tail": tail, "head": head, "head_cache": head_cache,
              "message_length": message_length}
    vector = {"name": name, "region": "claim", "inputs": inputs}
    if message_length < 0 or message_length > max_message_length(capacity):
        vector["error"] = "message"
        return vector
    record_length = RECORD_HEADER_LENGTH + message_length
    required = align(record_length, RECORD_ALIGNMENT)
    fields = {"record_length": record_length, "aligned_length": required}
    mask = capacity - 1
    if tail_error(tail, head_cache):
        vector["error"] = "tail"
        return vector
    cached = head_cache
    reread = False
    if required > capacity - (tail - cached):
        if tail_error(tail, head) or tail - head > capacity:
            vector["error"] = "tail"
            return vector
        reread = True
        if required > capacity - (tail - head):
            fields.update(result="full", tail_after=tail, head_cache_after=head_cache, padding=None)
            vector["fields"] = fields
            return vector
        cached = head
    new_tail = tail + required
    tail_index = tail & mask
    write_index = tail_index
    to_end = capacity - tail_index
    padding = None
    result = "claimed"
    if required > to_end:
        head_index = cached & mask
        write_index = 0
        if required > head_index:
            if not reread and (tail_error(tail, head) or tail - head > capacity):
                vector["error"] = "tail"
                return vector
            head_index = head & mask
            if required > head_index:
                result = "full"
                new_tail = tail
            cached = head
        padding = {"index": tail_index, "length": to_end}
        new_tail += to_end
    fields.update(result=result, tail_after=new_tail, head_cache_after=cached, padding=padding)
    if result == "claimed":
        fields["index"] = write_index
    vector["fields"] = fields
    return vector


# ---- unblock ----------------------------------------------------------------------

def unblock(name, capacity, head, tail, lengths):
    """One unblock as spec/command-ring.md, "Unblocking", computes it. `lengths` are the
    length words at 8-byte steps from the consumer index; words not listed are 0."""
    inputs = {"capacity": capacity, "head": head, "tail": tail, "lengths": lengths}
    vector = {"name": name, "region": "unblock", "inputs": inputs}
    if tail_error(tail, head) or tail - head > capacity:
        vector["error"] = "tail"
        return vector
    if head == tail:
        vector["fields"] = {"unblocked": False, "record": None}
        return vector
    mask = capacity - 1
    consumer_index = head & mask
    producer_index = tail & mask

    def length_at(index):
        step = (index - consumer_index) // RECORD_ALIGNMENT
        return lengths[step] if 0 <= step < len(lengths) else 0

    length = length_at(consumer_index)
    if length < 0:
        if record_error(length, PADDING, capacity, consumer_index):
            vector["error"] = "record"
            return vector
        vector["fields"] = {"unblocked": True,
                            "record": {"index": consumer_index, "length": -length, "type": PADDING}}
        return vector
    if length > 0:
        vector["fields"] = {"unblocked": False, "record": None}
        return vector
    limit = producer_index if producer_index > consumer_index else capacity
    index = consumer_index + RECORD_ALIGNMENT
    while index < limit and length_at(index) == 0:
        index += RECORD_ALIGNMENT
    # A nonzero word at `index`, or the limit reached with only zeros: either way the gap
    # from the consumer index to `index` was claimed and never written.
    vector["fields"] = {"unblocked": True,
                        "record": {"index": consumer_index, "length": index - consumer_index, "type": PADDING}}
    return vector


# ---- read -------------------------------------------------------------------------

def ring_image(capacity, records):
    """A ring of `capacity` bytes holding `records`, each (index, length, type, message)."""
    ring = bytearray(capacity)
    for index, length, type_, message in records:
        raw = encode_record(length, type_, message)
        ring[index:index + len(raw)] = raw
    return ring


def read(name, capacity, head, records, limit=2**31 - 1):
    """One read as spec/command-ring.md, "Reading", computes it from the ring image."""
    ring = ring_image(capacity, records)
    inputs = {"capacity": capacity, "head": head, "limit": limit, "ring": ring.hex()}
    mask = capacity - 1
    head_index = head & mask
    max_block = capacity - head_index
    bytes_read, messages, result = 0, [], "end-of-ring"
    while bytes_read < max_block:
        if len(messages) >= limit:
            result = "limit"
            break
        index = head_index + bytes_read
        length, type_ = RECORD_HEADER.unpack_from(ring, index)
        if length == 0:
            result = "unclaimed"
            break
        if length < 0:
            result = "claimed"
            break
        if record_error(length, type_, capacity, index):
            result = "record"
            break
        bytes_read += align(length, RECORD_ALIGNMENT)
        if type_ == PADDING:
            continue
        messages.append({"type": type_, "message": bytes(ring[index + RECORD_HEADER_LENGTH:index + length]).hex()})
    fields = {"result": result, "messages": messages, "bytes_read": bytes_read,
              "head_after": head + bytes_read, "zeroed": {"index": head_index, "length": bytes_read}}
    return {"name": name, "region": "read", "inputs": inputs, "fields": fields}


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
    invalid_header("magic-broadcast-buffer", "magic", encode_header(header(magic=b"CVLYBCST")), file_length(C)),
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
    record("record-committed", 24, 1, b"add publication!"),
    record("record-committed-aligns", 13, 7, b"hello"),
    record("record-committed-empty-message", 8, 3),
    record("record-committed-max-message", 8 + max_message_length(C), 2, b"m" * max_message_length(C)),
    record("record-padding-to-the-end", 8, PADDING, index=C - 8),
    record("record-padding-aborted-claim", 24, PADDING, b"\0" * 16, index=1024),
    record("record-claimed", -24, 1, index=2048),
    record("record-unclaimed", 0, 0),
    record("record-length-below-header", 4, 1),
    record("record-length-past-the-end", 16, 1, b"12345678", index=C - 8),
    record("record-claimed-past-the-end", -16, 1, index=C - 8),
    record("record-type-zero", 24, 0, b"add publication!"),
    record("record-type-below-padding", 24, -2, b"add publication!"),
    record("record-message-over-maximum", 8 + max_message_length(C) + 1, 2, b"m" * (max_message_length(C) + 1)),
    # claim
    claim("claim-fits", C, 0, 0, 0, 16),
    claim("claim-aligns-length", C, 24, 0, 0, 13),
    claim("claim-empty-message", C, 0, 0, 0, 0),
    claim("claim-max-message", C, 0, 0, 0, max_message_length(C)),
    claim("claim-message-too-long", C, 0, 0, 0, max_message_length(C) + 1),
    claim("claim-full", C, C, 0, 0, 16),
    claim("claim-rereads-a-stale-head-cache", C, C, 2048, 0, 16),
    claim("claim-wraps-with-padding", C, C - 8, 2048, 2048, 16),
    claim("claim-wraps-after-rereading-head", C, C - 8, 2048, 16, 16),
    claim("claim-wraps-but-the-head-blocks", C, C - 8, 16, 16, 16),
    claim("claim-tail-below-head-cache", C, 0, 0, 8, 16),
    claim("claim-tail-too-far-ahead", C, 2 * C, 0, 0, 16),
    claim("claim-tail-not-aligned", C, 12, 0, 0, 16),
    # unblock
    unblock("unblock-nothing-when-empty", C, 0, 0, []),
    unblock("unblock-nothing-when-committed", C, 0, 24, [24]),
    unblock("unblock-claimed-record", C, 0, 24, [-24]),
    unblock("unblock-gap-before-the-next-record", C, 0, 48, [0, 0, 0, -24]),
    unblock("unblock-gap-to-the-producer", C, 0, 24, [0, 0, 0]),
    unblock("unblock-gap-to-the-end", C, C - 16, C + 24, [0, 0]),
    unblock("unblock-claimed-record-past-the-end", C, C - 16, C + 24, [-32]),
    unblock("unblock-tail-behind-head", C, 24, 0, []),
    unblock("unblock-tail-too-far-ahead", C, 0, 2 * C, []),
    # read
    read("read-two-messages-then-padding-then-nothing", C, 0,
         [(0, 24, 1, b"add publication!"), (24, 13, 7, b"hello"), (40, 16, PADDING, b"")]),
    read("read-stops-at-a-claimed-record", C, 24, [(24, 13, 7, b"hello"), (40, -24, 1, b"")]),
    read("read-stops-at-the-message-limit", C, 0,
         [(0, 24, 1, b"add publication!"), (24, 13, 7, b"hello")], limit=1),
    read("read-to-the-end-of-the-ring", C, C - 16, [(C - 16, 16, 2, b"12345678"), (0, 24, 1, b"add publication!")]),
    read("read-rejects-a-record-after-delivering", C, 0, [(0, 24, 1, b"add publication!"), (24, 24, 0, b"bad type here!!!")]),
]


def main():
    out = Path(__file__).with_suffix(".json")
    doc = {"spec": "spec/command-ring.md", "version": 0,
           "generated_by": "spec/vectors/command-ring.py; do not edit by hand", "vectors": VECTORS}
    out.write_text(json.dumps(doc, indent=2) + "\n")
    print(f"{out}: {len(VECTORS)} vectors")


if __name__ == "__main__":
    main()
