#!/usr/bin/env python3
"""Write spec/vectors/log-buffer.json: the test vectors for spec/log-buffer.md, version 1.
The reference encoder, standard library only. Run from the repository root:

    python3 spec/vectors/log-buffer.py

Five regions (spec/log-buffer.md, "Vectors"): `header` vectors carry the first 128 bytes
of the metadata page and the file length the reader sees; `tail` vectors one tail
counter; `position` and `claim` vectors are arithmetic, with inputs and no bytes;
`claim-header` vectors the 32 bytes a publisher writes when it claims, and the same bytes
after commit and after abort. See spec/README.md, "Conventions".
"""

import json
import struct
from pathlib import Path

MAGIC = b"CVLYLOGB"
HEADER = struct.Struct("<8siiqiiiii20s32s32s")  # magic, version, type, correlation id, initial term id,
# term length, mtu length, publication window length, receiver window length (version 1), reserved,
# default frame header, reserved
FRAME_HEADER = struct.Struct("<ibBHiiiiq")  # frame length, version, flags, type, term offset, session id,
# stream id, term id, reserved value
HEADER_LENGTH, META_DATA_LENGTH, PARTITIONS = 128, 4096, 3
FRAME_ALIGNMENT, FRAME_HEADER_LENGTH = 32, 32
MAX_POSITION_SHIFT = 31  # the largest position is term length << 31
assert HEADER.size == HEADER_LENGTH and FRAME_HEADER.size == FRAME_HEADER_LENGTH

SESSION_ID, STREAM_ID = 1, 1001
DATA, PAD, UNFRAGMENTED = 1, 0, 0xC0


def i32(value):
    """Two's complement wrap to a signed 32-bit integer."""
    value &= 0xFFFF_FFFF
    return value - (1 << 32) if value >= 1 << 31 else value


def align(value, alignment):
    return (value + alignment - 1) & ~(alignment - 1)


# ---- header -----------------------------------------------------------------------

def default_frame_header(initial_term_id, session_id=SESSION_ID, stream_id=STREAM_ID,
                         frame_length=0, version=0, flags=UNFRAGMENTED, frame_type=DATA,
                         term_offset=0, reserved_value=0):
    return FRAME_HEADER.pack(frame_length, version, flags, frame_type, term_offset, session_id,
                             stream_id, initial_term_id, reserved_value)


def header(term_length=65536, type_=0, correlation_id=42, initial_term_id=-5, mtu_length=1408,
           window=None, version=0, magic=MAGIC, frame_header=None, receiver_window=0):
    window = term_length // 2 if window is None else window
    frame_header = default_frame_header(initial_term_id) if frame_header is None else frame_header
    return {"magic": magic.hex(), "version": version, "type": type_, "correlation_id": correlation_id,
            "initial_term_id": initial_term_id, "term_length": term_length, "mtu_length": mtu_length,
            "publication_window_length": window, "receiver_window_length": receiver_window,
            "default_frame_header": frame_header.hex(),
            "session_id": struct.unpack_from("<i", frame_header, 12)[0],
            "stream_id": struct.unpack_from("<i", frame_header, 16)[0]}


def encode_header(h):
    return HEADER.pack(bytes.fromhex(h["magic"]), h["version"], h["type"], h["correlation_id"],
                       h["initial_term_id"], h["term_length"], h["mtu_length"],
                       h["publication_window_length"], h["receiver_window_length"], b"",
                       bytes.fromhex(h["default_frame_header"]), b"")


def file_length(term_length):
    return PARTITIONS * term_length + META_DATA_LENGTH


def valid_header(name, h, length=None):
    return {"name": name, "region": "header", "bytes": encode_header(h).hex(),
            "file_length": file_length(h["term_length"]) if length is None else length, "fields": h}


def invalid_header(name, error, raw, length):
    return {"name": name, "region": "header", "bytes": raw.hex(), "file_length": length, "error": error}


# ---- tails and positions ----------------------------------------------------------

def pack_tail(term_id, term_offset):
    return struct.pack("<q", (term_id << 32) | (term_offset & 0xFFFF_FFFF))


def unpack_tail(raw, term_length):
    (value,) = struct.unpack("<q", raw)
    return i32(value >> 32), min(value & 0xFFFF_FFFF, term_length)


def tail(name, term_id, term_offset, term_length=65536):
    raw = pack_tail(term_id, term_offset)
    tid, offset = unpack_tail(raw, term_length)
    return {"name": name, "region": "tail", "bytes": raw.hex(), "term_length": term_length,
            "fields": {"term_id": tid, "term_offset": offset}}


def position_bits(term_length):
    return term_length.bit_length() - 1


def compute_position(initial_term_id, term_length, term_id, term_offset):
    term_count = i32(term_id - initial_term_id)  # wrapping 32-bit difference, 0 to 2^31 - 1
    return (term_count << position_bits(term_length)) + term_offset


def position_fields(initial_term_id, term_length, term_id=None, term_offset=None, position=None):
    if position is None:
        position = compute_position(initial_term_id, term_length, term_id, term_offset)
    bits = position_bits(term_length)
    term_count = position >> bits
    return {"term_count": term_count, "term_id": i32(initial_term_id + term_count),
            "term_offset": position & (term_length - 1), "position": position,
            "partition": term_count % PARTITIONS}


def position_vector(name, initial_term_id, term_length, **given):
    fields = position_fields(initial_term_id, term_length, **given)
    inputs = {"initial_term_id": initial_term_id, "term_length": term_length, **given}
    return {"name": name, "region": "position", "inputs": inputs, "fields": fields}


# ---- claims -----------------------------------------------------------------------

def claim(name, term_length, initial_term_id, active_term_count, tail_term_id, tail_offset,
          frame_length, limit, connected=1, next_tail=None):
    """One claim as spec/log-buffer.md, "Claim, commit and abort", steps 1 to 3 compute it.
    `next_tail` is the (term id, offset) of the partition rotated into, when a rotation
    matters; by default it holds the term three back with any offset."""
    tail_before = pack_tail(tail_term_id, tail_offset)
    bits = position_bits(term_length)
    max_position = term_length << MAX_POSITION_SHIFT
    inputs = {"term_length": term_length, "initial_term_id": initial_term_id,
              "active_term_count": active_term_count, "tail": tail_before.hex(),
              "frame_length": frame_length, "limit": limit, "is_connected": connected}
    fields = {}
    term_id, term_offset = unpack_tail(tail_before, term_length)
    if i32(term_id - initial_term_id) != active_term_count:
        fields["result"] = "admin-action"
        fields["reason"] = "tail"
        return {"name": name, "region": "claim", "inputs": inputs, "fields": fields}
    position = compute_position(initial_term_id, term_length, term_id, term_offset)
    aligned = align(frame_length, FRAME_ALIGNMENT)
    fields["position_before"] = position
    fields["aligned_length"] = aligned
    if position >= limit:
        if position + aligned >= max_position:
            fields["result"] = "max-position-exceeded"
        else:
            fields["result"] = "back-pressured" if connected else "not-connected"
        return {"name": name, "region": "claim", "inputs": inputs, "fields": fields}
    resulting = term_offset + aligned
    tail_after = pack_tail(term_id, tail_offset + aligned)  # the raw add, unclamped
    fields["tail_after"] = tail_after.hex()
    if resulting > term_length:
        padding = None if term_offset >= term_length else {"term_offset": term_offset,
                                                              "frame_length": term_length - term_offset}
        fields["padding"] = padding
        position_after = compute_position(initial_term_id, term_length, term_id, resulting)
        if position_after >= max_position:
            fields["result"] = "max-position-exceeded"
            return {"name": name, "region": "claim", "inputs": inputs, "fields": fields}
        fields["result"] = "admin-action"
        fields["reason"] = "end-of-term"
        next_index = (active_term_count + 1) % PARTITIONS
        expected = i32(term_id + 1 - PARTITIONS)
        current = next_tail if next_tail is not None else (expected, 0)
        inputs["next_tail"] = pack_tail(*current).hex()
        rotated = current[0] == expected
        fields["rotation"] = {"partition": next_index,
                              "tail": pack_tail(i32(term_id + 1), 0).hex() if rotated else pack_tail(*current).hex(),
                              "active_term_count": active_term_count + 1}
        return {"name": name, "region": "claim", "inputs": inputs, "fields": fields}
    fields["result"] = "claimed"
    fields["term_id"] = term_id
    fields["term_offset"] = term_offset
    fields["position_after"] = position + aligned
    return {"name": name, "region": "claim", "inputs": inputs, "fields": fields}


# ---- claim header -----------------------------------------------------------------

def claim_header(name, term_offset, frame_length, term_id, initial_term_id=-5, flags=UNFRAGMENTED,
                 reserved_value=0):
    default = default_frame_header(initial_term_id, flags=flags)
    version, default_flags, frame_type = struct.unpack_from("<bBH", default, 4)
    session_id, stream_id = struct.unpack_from("<ii", default, 12)
    claimed = FRAME_HEADER.pack(-frame_length, version, default_flags, frame_type, term_offset,
                                session_id, stream_id, term_id, reserved_value)
    committed = FRAME_HEADER.pack(frame_length, version, default_flags, frame_type, term_offset,
                                  session_id, stream_id, term_id, reserved_value)
    aborted = FRAME_HEADER.pack(frame_length, version, default_flags, PAD, term_offset,
                                session_id, stream_id, term_id, reserved_value)
    return {"name": name, "region": "claim-header",
            "inputs": {"default_frame_header": default.hex(), "term_offset": term_offset,
                       "frame_length": frame_length, "term_id": term_id, "reserved_value": reserved_value},
            "bytes": claimed.hex(), "fields": {"committed": committed.hex(), "aborted": aborted.hex()}}


# ---- the vectors ------------------------------------------------------------------

T = 65536
plain = header()
VECTORS = [
    # header
    valid_header("header-64k-concurrent", plain),
    valid_header("header-1g-exclusive", header(term_length=1 << 30, type_=1, correlation_id=-1,
                                              initial_term_id=2**31 - 1, mtu_length=65504, window=1)),
    valid_header("header-window-one", header(window=1)),
    # version 1 (ADR 0040): a publication's log carries receiver window 0; an image, type 2,
    # carries its receiver window, 1 to term length / 2.
    valid_header("header-v1-concurrent", header(version=1)),
    valid_header("header-v1-image", header(version=1, type_=2, correlation_id=23, receiver_window=32768)),
    valid_header("header-v1-image-small-window", header(version=1, type_=2, receiver_window=32)),
    valid_header("header-v1-image-1g", header(version=1, type_=2, term_length=1 << 30, receiver_window=131072,
                                               mtu_length=65504)),
    invalid_header("too-short-file", "too-short", encode_header(plain), META_DATA_LENGTH - 1),
    invalid_header("too-short-bytes", "too-short", encode_header(plain)[:127], file_length(T)),
    invalid_header("not-ready", "not-ready", encode_header(header(magic=b"\0" * 8)), file_length(T)),
    invalid_header("magic-counters-file", "magic", encode_header(header(magic=b"CVLYCNTR")), file_length(T)),
    invalid_header("version-2", "version", encode_header(header(version=2)), file_length(T)),
    invalid_header("version-negative", "version", encode_header(header(version=-1)), file_length(T)),
    invalid_header("type-3", "type", encode_header(header(type_=3)), file_length(T)),
    invalid_header("term-length-not-power-of-two", "term-length", encode_header(header(term_length=100000)),
                   file_length(100000)),
    invalid_header("term-length-below-minimum", "term-length", encode_header(header(term_length=32768)),
                   file_length(32768)),
    invalid_header("term-length-above-maximum", "term-length", encode_header(header(term_length=-(1 << 31))),
                   file_length(T)),
    invalid_header("mtu-not-aligned", "mtu", encode_header(header(mtu_length=1400)), file_length(T)),
    invalid_header("mtu-too-small", "mtu", encode_header(header(mtu_length=32)), file_length(T)),
    invalid_header("mtu-too-large", "mtu", encode_header(header(mtu_length=65536)), file_length(T)),
    invalid_header("window-zero", "window", encode_header(header(window=0)), file_length(T)),
    invalid_header("window-over-half", "window", encode_header(header(window=T // 2 + 1)), file_length(T)),
    invalid_header("window-image-zero", "window", encode_header(header(version=1, type_=2, receiver_window=0)),
                   file_length(T)),
    invalid_header("window-image-over-half", "window",
                   encode_header(header(version=1, type_=2, receiver_window=T // 2 + 32)), file_length(T)),
    invalid_header("window-publication-receiver-nonzero", "window",
                   encode_header(header(version=1, receiver_window=32)), file_length(T)),
    invalid_header("window-v0-receiver-nonzero", "window", encode_header(header(receiver_window=32)),
                   file_length(T)),
    invalid_header("frame-header-wrong-type", "frame-header",
                   encode_header(header(frame_header=default_frame_header(-5, frame_type=PAD))), file_length(T)),
    invalid_header("frame-header-wrong-term-id", "frame-header",
                   encode_header(header(frame_header=default_frame_header(-4))), file_length(T)),
    invalid_header("frame-header-nonzero-length", "frame-header",
                   encode_header(header(frame_header=default_frame_header(-5, frame_length=32))), file_length(T)),
    invalid_header("length-short", "length", encode_header(plain), file_length(T) - 1),
    invalid_header("length-long", "length", encode_header(plain), file_length(T) + 4096),
    invalid_header("version-before-term-length", "version",
                   encode_header(header(version=2, term_length=100000)), file_length(100000)),
    invalid_header("type-before-window", "type", encode_header(header(type_=3, window=0)), file_length(T)),
    # tail
    tail("tail-start", -5, 0),
    tail("tail-mid-term", -5, 4096),
    tail("tail-at-end", -4, T),
    tail("tail-past-end-is-clamped", -4, T + 64),
    tail("tail-term-id-max", 2**31 - 1, 32),
    tail("tail-term-id-min", -(2**31), 32),
    tail("tail-offset-high-bit", 7, 0xFFFF_FFF0, term_length=1 << 30),
    # position
    position_vector("position-first-term", -5, T, term_id=-5, term_offset=0),
    position_vector("position-second-term", -5, T, term_id=-4, term_offset=96),
    position_vector("position-fourth-term-wraps-partition", -5, T, term_id=-2, term_offset=32),
    position_vector("position-term-id-wraps-past-max", 2**31 - 2, T, term_id=-(2**31), term_offset=64),
    position_vector("position-inverse-start", -5, T, position=0),
    position_vector("position-inverse-third-term", -5, T, position=2 * T + 1024),
    position_vector("position-inverse-largest-term", 0, 1 << 30, position=((1 << 30) << 31) - 32),
    # claim
    claim("claim-fits", T, -5, 0, -5, 0, 64, limit=T // 2),
    claim("claim-aligns-length", T, -5, 0, -5, 0, 33, limit=T // 2),
    claim("claim-exact-end-of-term", T, -5, 0, -5, T - 64, 64, limit=T),
    claim("claim-back-pressured", T, -5, 0, -5, T // 2, 64, limit=T // 2),
    claim("claim-not-connected", T, -5, 0, -5, T // 2, 64, limit=T // 2, connected=0),
    claim("claim-end-of-term-pads-and-rotates", T, -5, 0, -5, T - 32, 64, limit=T),
    claim("claim-end-of-term-already-full", T, -5, 0, -5, T, 64, limit=2 * T),
    claim("claim-end-of-term-already-rotated", T, -5, 0, -5, T - 32, 64, limit=T, next_tail=(-4, 96)),
    claim("claim-tail-raced-a-rotation", T, -5, 1, -5, 0, 64, limit=T),
    claim("claim-max-position-exceeded", T, 0, 2**31 - 1, 2**31 - 1, T - 32, 64, limit=(T << 31) + T),
    claim("claim-max-position-at-limit", T, 0, 2**31 - 1, 2**31 - 1, T - 32, 64, limit=T - 32 + ((2**31 - 1) << 16)),
    # claim header
    claim_header("claim-header-first-frame", 0, 64, -5),
    claim_header("claim-header-mid-term", 4096, 1440, -4, reserved_value=7),
    claim_header("claim-header-padding-length", T - 32, 32, -5),
]


def main():
    out = Path(__file__).with_suffix(".json")
    doc = {"spec": "spec/log-buffer.md", "version": 1,
           "generated_by": "spec/vectors/log-buffer.py; do not edit by hand", "vectors": VECTORS}
    out.write_text(json.dumps(doc, indent=2) + "\n")
    print(f"{out}: {len(VECTORS)} vectors")


if __name__ == "__main__":
    main()
