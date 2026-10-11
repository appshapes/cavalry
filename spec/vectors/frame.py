#!/usr/bin/env python3
"""Write spec/vectors/frame.json: the test vectors for spec/frame.md, version 0.

This script is the spec's reference encoder, independent of the Rust code: it packs
the nine header fields with the standard library and nothing else, so an
implementation that agrees with it agrees with the layout table. Run it from the
repository root after changing the vectors below:

    python3 spec/vectors/frame.py

Each vector is a name, the bytes as hex, and either the header a conforming reader
decodes or the error it reports. See spec/README.md, "Conventions".
"""

import json
import struct
from pathlib import Path

HEADER = struct.Struct("<ibBHiiiiq")  # the layout table in spec/frame.md, in order
HEADER_LENGTH = 32
PAD, DATA = 0, 1
BEGIN, END, EOS = 0x80, 0x40, 0x20

FIELDS = ("frame_length", "version", "flags", "type", "term_offset", "session_id",
          "stream_id", "term_id", "reserved_value")


def header(frame_length, flags, type, term_offset=0, session_id=1, stream_id=1001,
           term_id=0, reserved_value=0, version=0):
    return dict(zip(FIELDS, (frame_length, version, flags, type, term_offset,
                             session_id, stream_id, term_id, reserved_value)))


def encode(h, body=b""):
    return HEADER.pack(*(h[f] for f in FIELDS)) + body


def valid(name, h, body=b"", trailing=b""):
    return {"name": name, "bytes": (encode(h, body) + trailing).hex(), "header": h}


def invalid(name, error, raw):
    return {"name": name, "bytes": raw.hex(), "error": error}


single = header(HEADER_LENGTH + 8, BEGIN | END, DATA)
VECTORS = [
    # Valid frames: decoded to `header`, and `header` encodes to the first
    # frame-length bytes.
    valid("data-single-frame", single, b"cavalry!"),
    valid("data-begin-fragment",
          header(HEADER_LENGTH + 32, BEGIN, DATA, term_offset=64, session_id=-2,
                 stream_id=10, term_id=5, reserved_value=0x0102030405060708),
          b"\x42" * 32),
    valid("data-end-fragment-eos",
          header(HEADER_LENGTH + 1, END | EOS, DATA, term_offset=128,
                 session_id=2**31 - 1, stream_id=-1, term_id=2**31 - 1,
                 reserved_value=-1),
          b"\xff"),
    valid("data-empty-body", header(HEADER_LENGTH, BEGIN | END, DATA)),
    valid("data-reserved-flag-bits-ignored", header(HEADER_LENGTH + 8, 0xDF, DATA),
          b"cavalry!"),
    valid("data-then-alignment-padding", single, b"cavalry!", trailing=b"\x00" * 24),
    valid("padding", header(64, 0, PAD, term_offset=65472), b"\x00" * 32),
    valid("padding-minimum", header(HEADER_LENGTH, 0, PAD, term_offset=65504)),
    # Invalid frames: the reader reports `error`, the first check that fails in the
    # order spec/frame.md gives.
    invalid("too-short", "too-short", encode(single)[:31]),
    invalid("version-1", "version", encode(header(HEADER_LENGTH, BEGIN | END, DATA, version=1))),
    invalid("version-negative", "version", encode(header(HEADER_LENGTH, BEGIN | END, DATA, version=-1))),
    invalid("type-nak", "type", encode(header(HEADER_LENGTH, 0, 2))),
    invalid("type-unknown", "type", encode(header(HEADER_LENGTH, 0, 0x7FFF))),
    invalid("length-below-header", "length", encode(header(16, BEGIN | END, DATA))),
    invalid("length-negative", "length", encode(header(-40, BEGIN | END, DATA))),
    invalid("length-truncated", "length", encode(single, b"cava")),
    # The order of checks: a frame wrong in two ways reports the earlier check.
    invalid("version-before-type", "version", encode(header(HEADER_LENGTH, 0, 2, version=1))),
    invalid("type-before-length", "type", encode(header(16, 0, 2))),
]


def main():
    out = Path(__file__).with_suffix(".json")
    doc = {
        "spec": "spec/frame.md",
        "version": 0,
        "generated_by": "spec/vectors/frame.py; do not edit by hand",
        "vectors": VECTORS,
    }
    out.write_text(json.dumps(doc, indent=2) + "\n")
    print(f"{out}: {len(VECTORS)} vectors")


if __name__ == "__main__":
    main()
