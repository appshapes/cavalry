#!/usr/bin/env python3
"""Write spec/vectors/durable-log.json: the test vectors for spec/durable-log.md,
version 0. The reference encoder, standard library only. Run from the repository root:

    python3 spec/vectors/durable-log.py

Catalog header vectors carry the 128-byte header and the file length the reader sees;
descriptor vectors carry one 1024-byte slot and its index; index vectors carry the
128-byte index header and the file length, or one 16-byte entry and the entry before it;
position vectors are arithmetic: from a descriptor's fields and a position, the segment
file, the offset in it and the frame header fields a reader checks. See spec/README.md,
"Conventions".
"""

import json
import struct
import zlib
from pathlib import Path

CATALOG_MAGIC = b"CVLYCTLG"
INDEX_MAGIC = b"CVLYTIDX"
CATALOG_HEADER = struct.Struct("<8siiiiq96s")  # magic, version, descriptor length, reserved, checksum kind, created, reserved
DESCRIPTOR = struct.Struct("<iIqqIiqqqiiiiiii256si128s552s")  # state, stop crc, stop ts, stop pos, creation crc, reserved, creation fields
INDEX_HEADER = struct.Struct("<8siiq104s")  # magic, version, reserved, recording id, reserved
ENTRY = struct.Struct("<qq")
HEADER_LENGTH, DESCRIPTOR_LENGTH, ENTRY_LENGTH = 128, 1024, 16
CHANNEL_LENGTH, SOURCE_LENGTH = 256, 128
assert CATALOG_HEADER.size == HEADER_LENGTH and DESCRIPTOR.size == DESCRIPTOR_LENGTH
assert INDEX_HEADER.size == HEADER_LENGTH and ENTRY.size == ENTRY_LENGTH
FRAME_ALIGNMENT = 32
EMPTY, VALID, INVALID, DELETED = 0, 1, 2, 3


MAX_SLOTS = 1 << 20  # the catalog's bound, fixed by the format: 1 GiB of slots


def catalog_header(version=0, descriptor_length=DESCRIPTOR_LENGTH, checksum_kind=0,
                   created=1_760_000_000_000_000_000, magic=CATALOG_MAGIC):
    return {"magic": magic.hex(), "version": version, "descriptor_length": descriptor_length,
            "data_checksum_kind": checksum_kind, "created": created}


def encode_catalog_header(h):
    return CATALOG_HEADER.pack(bytes.fromhex(h["magic"]), h["version"], h["descriptor_length"],
                               0, h["data_checksum_kind"], h["created"], b"")


def descriptor(recording_id=0, state=VALID, start_timestamp=1_760_000_000_000_000_000, stop_timestamp=-1,
               start_position=0, stop_position=-1, initial_term_id=7, segment_file_length=128 * 1024 * 1024,
               term_length=64 * 1024 * 1024, mtu_length=1408, session_id=1, stream_id=1001,
               channel="cavalry:ipc", source_identity="cavalry:ipc"):
    return {"recording_id": recording_id, "state": state, "start_timestamp": start_timestamp,
            "stop_timestamp": stop_timestamp, "start_position": start_position, "stop_position": stop_position,
            "initial_term_id": initial_term_id, "segment_file_length": segment_file_length,
            "term_length": term_length, "mtu_length": mtu_length, "session_id": session_id,
            "stream_id": stream_id, "channel": channel, "source_identity": source_identity}


def descriptor_body(d):
    """Bytes 32 to 1024: the fields written once at creation; the creation checksum covers
    bytes 28 to 1024, the reserved word before them included."""
    channel, source = d["channel"].encode(), d["source_identity"].encode()
    return struct.pack("<qqqiiiiiii256si128s552s", d["recording_id"], d["start_timestamp"],
                       d["start_position"], d["initial_term_id"], d["segment_file_length"],
                       d["term_length"], d["mtu_length"], d["session_id"], d["stream_id"],
                       len(channel), channel, len(source), source, b"")


def descriptor_stop(d):
    """Bytes 8 to 24: the stop fields, under the stop checksum."""
    return struct.pack("<qq", d["stop_timestamp"], d["stop_position"])


def encode_descriptor(d, body_checksum=None, stop_checksum=None):
    body, stop = descriptor_body(d), descriptor_stop(d)
    creation = struct.pack("<i", 0) + body  # bytes 28 to 1024
    body_crc = zlib.crc32(creation) & 0xFFFFFFFF if body_checksum is None else body_checksum
    stop_crc = zlib.crc32(stop) & 0xFFFFFFFF if stop_checksum is None else stop_checksum
    return struct.pack("<iI", d["state"], stop_crc) + stop + struct.pack("<I", body_crc) + creation


def index_header(recording_id=0, version=0, magic=INDEX_MAGIC):
    return {"magic": magic.hex(), "version": version, "recording_id": recording_id}


def encode_index_header(h):
    return INDEX_HEADER.pack(bytes.fromhex(h["magic"]), h["version"], 0, h["recording_id"], b"")


def entry(timestamp, position):
    return {"timestamp": timestamp, "position": position}


def encode_entry(e):
    return ENTRY.pack(e["timestamp"], e["position"])


def position_vector(name, d, position):
    """The arithmetic a reader does to find a position's bytes (spec/log-buffer.md, "Positions")."""
    term_length, segment_length = d["term_length"], d["segment_file_length"]
    position_bits = term_length.bit_length() - 1
    term_count = position >> position_bits
    term_id = (d["initial_term_id"] + term_count) & 0xFFFFFFFF
    term_id = term_id - (1 << 32) if term_id >= (1 << 31) else term_id
    # Aeron's rule: segments start at the start position's term base, then whole segments.
    start_term_base = d["start_position"] - (d["start_position"] % term_length)
    from_base = position - start_term_base
    base = start_term_base + from_base - (from_base % segment_length)
    return {"name": name, "region": "position", "inputs": {"descriptor": d, "position": position},
            "fields": {"segment_file": f"{d['recording_id']}-{base}.rec", "segment_base_position": base,
                       "offset_in_segment": position - base, "term_id": term_id,
                       "term_offset": position % term_length}}


def valid(name, region, raw, fields, **extra):
    return {"name": name, "region": region, "bytes": raw.hex(), "fields": fields, **extra}


def invalid(name, region, error, raw, **extra):
    return {"name": name, "region": region, "bytes": raw.hex(), "error": error, **extra}


def main():
    vectors = []
    # Catalog header: valid, then each rejection in the order the checks run.
    h = catalog_header()
    vectors.append(valid("catalog-header", "catalog-header", encode_catalog_header(h), h, file_length=HEADER_LENGTH + 2 * DESCRIPTOR_LENGTH, slots=2))
    vectors.append(invalid("catalog-too-short", "catalog-header", "too-short", encode_catalog_header(h)[:64], file_length=64))
    vectors.append(invalid("catalog-magic", "catalog-header", "magic", encode_catalog_header(catalog_header(magic=b"CVLYCNTR")), file_length=HEADER_LENGTH))
    vectors.append(invalid("catalog-not-ready", "catalog-header", "not-ready", encode_catalog_header(catalog_header(magic=bytes(8))), file_length=HEADER_LENGTH))
    vectors.append(invalid("catalog-version", "catalog-header", "version", encode_catalog_header(catalog_header(version=1)), file_length=HEADER_LENGTH))
    vectors.append(invalid("catalog-descriptor-length", "catalog-header", "descriptor-length", encode_catalog_header(catalog_header(descriptor_length=512)), file_length=HEADER_LENGTH))
    vectors.append(invalid("catalog-checksum-kind", "catalog-header", "checksum-kind", encode_catalog_header(catalog_header(checksum_kind=1)), file_length=HEADER_LENGTH))
    vectors.append(invalid("catalog-created", "catalog-header", "timestamp", encode_catalog_header(catalog_header(created=-1)), file_length=HEADER_LENGTH))
    vectors.append(valid("catalog-partial-trailing-slot-ignored", "catalog-header", encode_catalog_header(h), h, file_length=HEADER_LENGTH + DESCRIPTOR_LENGTH + 100, slots=1))
    vectors.append(invalid("catalog-length-over-bound", "catalog-header", "length", encode_catalog_header(h), file_length=HEADER_LENGTH + (MAX_SLOTS + 1) * DESCRIPTOR_LENGTH))
    # Descriptors: valid live, valid stopped, every state, then each rejection in order.
    live = descriptor()
    vectors.append(valid("descriptor-live", "descriptor", encode_descriptor(live), live, slot=0))
    stopped = descriptor(recording_id=1, stop_timestamp=1_760_000_001_000_000_000, stop_position=4096)
    vectors.append(valid("descriptor-stopped", "descriptor", encode_descriptor(stopped), stopped, slot=1))
    empty = descriptor(recording_id=2, state=EMPTY, start_timestamp=0, stop_timestamp=-1, initial_term_id=0,
                       segment_file_length=0, term_length=0, mtu_length=0, session_id=0, stream_id=0,
                       channel="", source_identity="")
    vectors.append({"name": "descriptor-empty", "region": "descriptor", "bytes": (bytes(DESCRIPTOR_LENGTH)).hex(),
                    "fields": {"state": EMPTY}, "slot": 2})
    # An empty slot whose other bytes are not zero: a crash during creation; still empty.
    torn = bytearray(encode_descriptor(descriptor(recording_id=2)))
    torn[0:4] = struct.pack("<i", EMPTY)
    vectors.append({"name": "descriptor-empty-after-torn-creation", "region": "descriptor", "bytes": bytes(torn).hex(),
                    "fields": {"state": EMPTY}, "slot": 2})
    deleted = descriptor(recording_id=3, state=DELETED, stop_timestamp=1_760_000_001_000_000_000, stop_position=4096)
    vectors.append(valid("descriptor-deleted", "descriptor", encode_descriptor(deleted), deleted, slot=3))
    # A slot the driver set to state 2: reported invalid before anything else is read, zeros or not.
    vectors.append(invalid("descriptor-invalid", "descriptor", "invalid", struct.pack("<i", INVALID) + bytes(DESCRIPTOR_LENGTH - 4), slot=3))
    vectors.append(invalid("descriptor-invalid-with-fields", "descriptor", "invalid", encode_descriptor(descriptor(recording_id=3, state=INVALID)), slot=3))
    vectors.append(invalid("descriptor-state", "descriptor", "state", encode_descriptor(descriptor(state=4)), slot=0))
    vectors.append(invalid("descriptor-body-checksum", "descriptor", "checksum", encode_descriptor(live, body_checksum=0xDEADBEEF), slot=0))
    vectors.append(invalid("descriptor-stop-checksum", "descriptor", "checksum", encode_descriptor(stopped, stop_checksum=0xDEADBEEF), slot=1))
    vectors.append(invalid("descriptor-recording-id", "descriptor", "recording-id", encode_descriptor(descriptor(recording_id=5)), slot=0))
    vectors.append(invalid("descriptor-start-timestamp", "descriptor", "timestamp", encode_descriptor(descriptor(start_timestamp=-1)), slot=0))
    vectors.append(invalid("descriptor-stop-before-start", "descriptor", "timestamp", encode_descriptor(descriptor(start_timestamp=10, stop_timestamp=5)), slot=0))
    vectors.append(invalid("descriptor-start-position-unaligned", "descriptor", "position", encode_descriptor(descriptor(start_position=16)), slot=0))
    vectors.append(invalid("descriptor-stop-position-below-start", "descriptor", "position", encode_descriptor(descriptor(start_position=4096, stop_position=2048, stop_timestamp=1_760_000_001_000_000_000)), slot=0))
    vectors.append(invalid("descriptor-term-length", "descriptor", "term-length", encode_descriptor(descriptor(term_length=3 * 1024 * 1024)), slot=0))
    vectors.append(invalid("descriptor-segment-length-below-term", "descriptor", "segment-length", encode_descriptor(descriptor(segment_file_length=32 * 1024 * 1024)), slot=0))
    vectors.append(invalid("descriptor-segment-length-not-power", "descriptor", "segment-length", encode_descriptor(descriptor(segment_file_length=192 * 1024 * 1024)), slot=0))
    vectors.append(invalid("descriptor-mtu", "descriptor", "mtu", encode_descriptor(descriptor(mtu_length=1400)), slot=0))
    # A channel length over the field's size, with a checksum that matches, so the string check is reached.
    d = descriptor()
    body = descriptor_body(d)
    body = body[:48] + struct.pack("<i", CHANNEL_LENGTH + 1) + body[52:]
    creation = struct.pack("<i", 0) + body
    stop = descriptor_stop(d)
    long_channel = struct.pack("<iI", VALID, zlib.crc32(stop) & 0xFFFFFFFF) + stop + struct.pack("<I", zlib.crc32(creation) & 0xFFFFFFFF) + creation
    vectors.append(invalid("descriptor-channel-length", "descriptor", "string", long_channel, slot=0))
    # Two faults each: the first in the page's order is the one reported.
    vectors.append(invalid("descriptor-order-checksum-before-id", "descriptor", "checksum", encode_descriptor(descriptor(recording_id=5), body_checksum=1), slot=0))
    vectors.append(invalid("descriptor-order-timestamp-before-id", "descriptor", "timestamp", encode_descriptor(descriptor(recording_id=5, start_timestamp=-1)), slot=0))
    vectors.append(invalid("descriptor-order-id-before-position", "descriptor", "recording-id", encode_descriptor(descriptor(recording_id=5, start_position=16)), slot=0))
    vectors.append(invalid("descriptor-order-position-before-mtu", "descriptor", "position", encode_descriptor(descriptor(start_position=16, mtu_length=1400)), slot=0))
    bad_mtu = descriptor(mtu_length=1400)
    body = descriptor_body(bad_mtu)
    body = body[:48] + struct.pack("<i", CHANNEL_LENGTH + 1) + body[52:]
    creation = struct.pack("<i", 0) + body
    stop = descriptor_stop(bad_mtu)
    mtu_and_long_channel = struct.pack("<iI", VALID, zlib.crc32(stop) & 0xFFFFFFFF) + stop + struct.pack("<I", zlib.crc32(creation) & 0xFFFFFFFF) + creation
    vectors.append(invalid("descriptor-order-mtu-before-string", "descriptor", "mtu", mtu_and_long_channel, slot=0))
    vectors.append(invalid("descriptor-order-invalid-before-checksum", "descriptor", "invalid", struct.pack("<i", INVALID) + encode_descriptor(descriptor(), body_checksum=1)[4:], slot=0))
    vectors.append(invalid("descriptor-stop-timestamp-live-position-set", "descriptor", "position", encode_descriptor(descriptor(stop_position=4096)), slot=0))
    vectors.append(invalid("descriptor-stop-position-live-timestamp-set", "descriptor", "position", encode_descriptor(descriptor(stop_timestamp=1_760_000_001_000_000_000)), slot=0))
    vectors.append(invalid("catalog-order-version-before-descriptor-length", "catalog-header", "version", encode_catalog_header(catalog_header(version=1, descriptor_length=512)), file_length=HEADER_LENGTH))
    vectors.append(invalid("catalog-order-magic-before-version", "catalog-header", "magic", encode_catalog_header(catalog_header(magic=b"CVLYCNTR", version=1)), file_length=HEADER_LENGTH))
    # Time index: valid header, rejections, then entries.
    ih = index_header(recording_id=1)
    vectors.append(valid("index-header", "index-header", encode_index_header(ih), ih, file_length=HEADER_LENGTH + 3 * ENTRY_LENGTH, entries=3))
    vectors.append(valid("index-header-part-written-last-entry-ignored", "index-header", encode_index_header(ih), ih, file_length=HEADER_LENGTH + 3 * ENTRY_LENGTH + 7, entries=3))
    vectors.append(invalid("index-too-short", "index-header", "too-short", encode_index_header(ih)[:100], file_length=100))
    vectors.append(invalid("index-magic", "index-header", "magic", encode_index_header(index_header(magic=CATALOG_MAGIC)), file_length=HEADER_LENGTH))
    vectors.append(invalid("index-not-ready-torn-header", "index-header", "not-ready", encode_index_header(index_header(magic=bytes(8))), file_length=HEADER_LENGTH))
    vectors.append(invalid("index-version", "index-header", "version", encode_index_header(index_header(version=1)), file_length=HEADER_LENGTH))
    vectors.append(invalid("index-recording-id", "index-header", "recording-id", encode_index_header(index_header(recording_id=-1)), file_length=HEADER_LENGTH))
    first = entry(1_760_000_000_000_000_000, 0)
    second = entry(1_760_000_000_001_000_000, 4096)
    vectors.append(valid("index-entry-first", "index-entry", encode_entry(first), first))
    vectors.append(valid("index-entry-second", "index-entry", encode_entry(second), second, previous=first))
    vectors.append(invalid("index-entry-timestamp-backwards", "index-entry", "order", encode_entry(entry(first["timestamp"] - 1, 4096)), previous=first))
    vectors.append(invalid("index-entry-position-backwards", "index-entry", "order", encode_entry(entry(second["timestamp"], 0)), previous=second))
    vectors.append(invalid("index-entry-position-unaligned", "index-entry", "position", encode_entry(entry(second["timestamp"], 4100)), previous=first))
    vectors.append(invalid("index-entry-negative", "index-entry", "position", encode_entry(entry(second["timestamp"], -32)), previous=first))
    vectors.append(invalid("index-entry-order-timestamp-before-position", "index-entry", "timestamp", encode_entry(entry(-1, 4100)), previous=first))
    vectors.append(invalid("index-entry-order-position-before-order", "index-entry", "position", encode_entry(entry(first["timestamp"] - 1, 4100)), previous=first))
    # Positions: the arithmetic from a position to its bytes.
    d = descriptor()
    vectors.append(position_vector("position-start", d, 0))
    vectors.append(position_vector("position-in-first-term", d, 4096))
    vectors.append(position_vector("position-second-term-same-segment", d, 64 * 1024 * 1024 + 96))
    vectors.append(position_vector("position-second-segment", d, 128 * 1024 * 1024 + 32))
    vectors.append(position_vector("position-term-id-wraps", descriptor(initial_term_id=2**31 - 1), 64 * 1024 * 1024))
    late = descriptor(start_position=64 * 1024 * 1024 + 4096)
    vectors.append(position_vector("position-start-inside-second-term", late, late["start_position"]))
    vectors.append(position_vector("position-second-segment-of-late-start", late, 64 * 1024 * 1024 + 128 * 1024 * 1024 + 64))
    doc = {"spec": "durable-log", "version": 0, "generated_by": "spec/vectors/durable-log.py", "vectors": vectors}
    out = Path(__file__).resolve().parent / "durable-log.json"
    out.write_text(json.dumps(doc, indent=2) + "\n")
    print(f"durable-log.py: wrote {len(vectors)} vectors to {out}")


if __name__ == "__main__":
    main()
