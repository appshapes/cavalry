#!/usr/bin/env python3
"""Write spec/vectors/counters-file.json: the test vectors for spec/counters-file.md,
version 0. The reference encoder, standard library only. Run from the repository root:

    python3 spec/vectors/counters-file.py

Header vectors carry the 128-byte header and the file length the reader sees; record
vectors carry one 256-byte metadata record. See spec/README.md, "Conventions".
"""

import json
import struct
from pathlib import Path

MAGIC = b"CVLYCNTR"
HEADER = struct.Struct("<8siiqq96s")  # magic, version, capacity, creator pid, start timestamp, reserved
RECORD = struct.Struct("<iiq112si124s")  # state, type id, reclaim deadline, key, label length, label
HEADER_LENGTH, RECORD_LENGTH, SLOT_LENGTH = 128, 256, 128
assert HEADER.size == HEADER_LENGTH and RECORD.size == RECORD_LENGTH


def header(capacity=64, version=0, creator_pid=4242, start_timestamp=1_760_000_000_000_000_000, magic=MAGIC):
    return {"magic": magic.hex(), "version": version, "capacity": capacity,
            "creator_pid": creator_pid, "start_timestamp": start_timestamp}


def encode_header(h):
    return HEADER.pack(bytes.fromhex(h["magic"]), h["version"], h["capacity"], h["creator_pid"],
                       h["start_timestamp"], b"")


def file_length(capacity):
    return HEADER_LENGTH + capacity * (RECORD_LENGTH + SLOT_LENGTH)


def record(state=1, type_id=0, reclaim_deadline=0, key=b"", label=b"counters.allocated"):
    return {"state": state, "type_id": type_id, "reclaim_deadline": reclaim_deadline,
            "key": key.hex(), "label_length": len(label), "label": label.decode()}


def encode_record(r):
    return RECORD.pack(r["state"], r["type_id"], r["reclaim_deadline"], bytes.fromhex(r["key"]),
                       r["label_length"], r["label"].encode())


def valid_header(name, h, length=None):
    return {"name": name, "region": "header", "bytes": encode_header(h).hex(),
            "file_length": file_length(h["capacity"]) if length is None else length, "fields": h}


def invalid_header(name, error, raw, length):
    return {"name": name, "region": "header", "bytes": raw.hex(), "file_length": length, "error": error}


def valid_record(name, r):
    return {"name": name, "region": "record", "bytes": encode_record(r).hex(), "fields": r}


def invalid_record(name, error, raw):
    return {"name": name, "region": "record", "bytes": raw.hex(), "error": error}


plain = header()
VECTORS = [
    valid_header("header-capacity-64", plain),
    valid_header("header-capacity-65536", header(capacity=65536, creator_pid=1, start_timestamp=0)),
    valid_header("header-longer-file-is-fine", plain, length=file_length(64) + 4096),
    invalid_header("too-short", "too-short", encode_header(plain)[:127], 127),
    invalid_header("not-ready", "not-ready", encode_header(header(magic=b"\0" * 8)), file_length(64)),
    invalid_header("magic", "magic", encode_header(header(magic=b"CVLYLOGB")), file_length(64)),
    invalid_header("version-1", "version", encode_header(header(version=1)), file_length(64)),
    invalid_header("capacity-not-power-of-two", "capacity", encode_header(header(capacity=100)), file_length(100)),
    invalid_header("capacity-below-minimum", "capacity", encode_header(header(capacity=32)), file_length(32)),
    invalid_header("capacity-above-maximum", "capacity", encode_header(header(capacity=131072)), file_length(131072)),
    invalid_header("capacity-negative", "capacity", encode_header(header(capacity=-64)), file_length(64)),
    invalid_header("length", "length", encode_header(plain), file_length(64) - 1),
    invalid_header("version-before-capacity", "version", encode_header(header(version=1, capacity=100)), file_length(100)),
    valid_record("record-static-allocated", record()),
    valid_record("record-instance-with-key",
                 record(type_id=7, key=struct.pack("<ii", 1, 1001), label=b"publication.back_pressure 1 1001")),
    valid_record("record-unused", record(state=0, type_id=0, label=b"")),
    valid_record("record-reclaimed", record(state=-1, reclaim_deadline=1_760_000_005_000_000_000)),
    valid_record("record-label-maximum", record(label=b"x" * 124)),
    invalid_record("record-state-2", "state", encode_record(record(state=2))),
    invalid_record("record-label-too-long", "label",
                   RECORD.pack(1, 0, 0, b"", 125, b"y" * 124)),
    invalid_record("record-label-negative", "label", RECORD.pack(1, 0, 0, b"", -1, b"")),
]


def main():
    out = Path(__file__).with_suffix(".json")
    doc = {"spec": "spec/counters-file.md", "version": 0,
           "generated_by": "spec/vectors/counters-file.py; do not edit by hand", "vectors": VECTORS}
    out.write_text(json.dumps(doc, indent=2) + "\n")
    print(f"{out}: {len(VECTORS)} vectors")


if __name__ == "__main__":
    main()
