#!/usr/bin/env python3
"""Write spec/vectors/durable-log.json: the test vectors for spec/durable-log.md,
version 3. The reference encoder, standard library only. Run from the repository root:

    python3 spec/vectors/durable-log.py

Catalog header vectors carry the 128-byte header and the file length the reader sees;
descriptor vectors carry one 1024-byte slot and its index; index vectors carry the
128-byte index header and the file length, or one 16-byte entry and the entry before it;
position vectors are arithmetic: from a descriptor's fields and a position, the segment
file, the offset in it and the frame header fields a reader checks. Log recordings
vectors carry the file's 64-byte header and the file length; one slot's fields and
entries; or both slots of a file, for the choice between them. See spec/README.md,
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
               channel="cavalry:ipc", source_identity="cavalry:ipc", form=0, retained_position=None):
    """A slot's fields. `form` is 0 or 1 (spec/durable-log.md, "Descriptor"; ADR 0031): in form 1
    the retained group holds `retained_position`, the start position unless given, and the
    creation checksum skips bytes 472 to 484."""
    d = {"recording_id": recording_id, "state": state, "start_timestamp": start_timestamp,
         "stop_timestamp": stop_timestamp, "start_position": start_position, "stop_position": stop_position,
         "initial_term_id": initial_term_id, "segment_file_length": segment_file_length,
         "term_length": term_length, "mtu_length": mtu_length, "session_id": session_id,
         "stream_id": stream_id, "channel": channel, "source_identity": source_identity, "form": form}
    d["retained_position"] = start_position if retained_position is None else retained_position
    return d


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


def encode_descriptor(d, body_checksum=None, stop_checksum=None, retained_checksum=None):
    body, stop = descriptor_body(d), descriptor_stop(d)
    creation = struct.pack("<i", 0) + body  # bytes 28 to 1024
    if d.get("form", 0) == 1:
        # Bytes 472 to 484 are the retained group: the position and its own CRC32; the
        # creation checksum covers bytes 28 to 472 and 484 to 1024.
        position = struct.pack("<q", d["retained_position"])
        group_crc = zlib.crc32(position) & 0xFFFFFFFF if retained_checksum is None else retained_checksum
        creation = creation[:444] + position + struct.pack("<I", group_crc) + creation[456:]
        signed = creation[:444] + creation[456:]
    else:
        signed = creation
    body_crc = zlib.crc32(signed) & 0xFFFFFFFF if body_checksum is None else body_checksum
    stop_crc = zlib.crc32(stop) & 0xFFFFFFFF if stop_checksum is None else stop_checksum
    return struct.pack("<iI", d["state"], stop_crc) + stop + struct.pack("<I", body_crc) + creation


TERMS_MAGIC = b"CVLYTERM"
TERMS_HEADER_LENGTH = 64
TERMS_ENTRY_LENGTH = 32
# magic, version, entry length, then the two candidate slots, each a term, its checksum and reserved bytes
TERMS_HEADER = struct.Struct("<8siiqI4sqI20s")
TERMS_ENTRY = struct.Struct("<iIqqq")  # state, checksum, leadership term id, term base log position, recording id


def candidate_checksum(candidate_term):
    """A candidate slot's checksum: the zlib CRC32 of the candidate term's eight bytes."""
    return zlib.crc32(struct.pack("<q", candidate_term)) & 0xFFFFFFFF


def terms_header(slots=(-1, -1), version=0, entry_length=TERMS_ENTRY_LENGTH, magic=TERMS_MAGIC, checksums=(None, None)):
    """The terms file's header (spec/durable-log.md, "Terms"): the two candidate slots, each
    under the zlib CRC32 of its eight bytes unless a checksum is given. `written` is what
    the encoder packs; the fields a reader decodes are the candidate term each slot holds,
    None for a slot whose checksum does not match, and the stored candidate term, the
    higher of the slots that hold one."""
    sums = [candidate_checksum(term) if given is None else given for term, given in zip(slots, checksums)]
    held = [term if checksum == candidate_checksum(term) else None for term, checksum in zip(slots, sums)]
    stored = max((term for term in held if term is not None), default=None)
    return {"magic": magic.hex(), "version": version, "entry_length": entry_length,
            "candidate_terms": held, "candidate_term": stored,
            "written": {"candidate_terms": list(slots), "candidate_checksums": sums}}


def terms_header_fields(h):
    """The fields of a valid terms header vector: what a reader decodes, less what the
    encoder alone needs."""
    return {name: value for name, value in h.items() if name != "written"}


def encode_terms_header(h):
    terms, sums = h["written"]["candidate_terms"], h["written"]["candidate_checksums"]
    return TERMS_HEADER.pack(bytes.fromhex(h["magic"]), h["version"], h["entry_length"], terms[0], sums[0], b"",
                             terms[1], sums[1], b"")


def terms_entry(leadership_term_id, term_base_log_position, recording_id=-1, state=VALID, checksum=None):
    """One entry of the terms file: the checksum is the zlib CRC32 of bytes 8 to 32 unless given."""
    if checksum is None:
        checksum = zlib.crc32(struct.pack("<qqq", leadership_term_id, term_base_log_position, recording_id)) & 0xFFFFFFFF
    return {"state": state, "checksum": checksum, "leadership_term_id": leadership_term_id,
            "term_base_log_position": term_base_log_position, "recording_id": recording_id}


def encode_terms_entry(e):
    return TERMS_ENTRY.pack(e["state"], e["checksum"], e["leadership_term_id"], e["term_base_log_position"],
                            e["recording_id"])


RECORDINGS_MAGIC = b"CVLYLOGR"
RECORDINGS_HEADER = struct.Struct("<8siii44s")  # magic, version, slot length, member id, reserved
RECORDINGS_HEADER_LENGTH, RECORDINGS_MAX_ENTRIES = 64, 1026
RECORDINGS_SLOT_LENGTH = 24 + RECORDINGS_MAX_ENTRIES * 16
RECORDINGS_FILE_LENGTH = RECORDINGS_HEADER_LENGTH + 2 * RECORDINGS_SLOT_LENGTH
assert RECORDINGS_HEADER.size == RECORDINGS_HEADER_LENGTH
assert (RECORDINGS_SLOT_LENGTH, RECORDINGS_FILE_LENGTH) == (16440, 32944)


def recordings_header(member_id=1, version=0, slot_length=RECORDINGS_SLOT_LENGTH, magic=RECORDINGS_MAGIC):
    return {"magic": magic.hex(), "version": version, "slot_length": slot_length, "member_id": member_id}


def encode_recordings_header(h):
    return RECORDINGS_HEADER.pack(bytes.fromhex(h["magic"]), h["version"], h["slot_length"], h["member_id"], b"")


def recordings_slot(sequence, floor_position, entries, count=None, checksum=None):
    """One slot of the log recordings file, its fields and its entries and no byte after
    them: `entries` is (recording id, start position) pairs. The entry count is the
    number of entries and the checksum the zlib CRC32 of everything after it, unless
    given."""
    body = struct.pack("<iqq", len(entries) if count is None else count, sequence, floor_position)
    body += b"".join(struct.pack("<qq", recording_id, start) for recording_id, start in entries)
    if checksum is None:
        checksum = zlib.crc32(body) & 0xFFFFFFFF
    return struct.pack("<I", checksum) + body


def recordings_fields(sequence, floor_position, entries, **extra):
    return {"sequence": sequence, "floor_position": floor_position,
            "entries": [list(entry) for entry in entries], **extra}


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
    h1 = catalog_header(version=1)
    vectors.append(valid("catalog-header-version-1", "catalog-header", encode_catalog_header(h1), h1, file_length=HEADER_LENGTH + 2 * DESCRIPTOR_LENGTH, slots=2))
    vectors.append(invalid("catalog-version", "catalog-header", "version", encode_catalog_header(catalog_header(version=2)), file_length=HEADER_LENGTH))
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
    # Version 1's form (ADR 0031): the retained group under its checksum, the creation checksum
    # over bytes 28 to 472 and 484 to 1024; a reader tries this range first, then version 0's.
    live_v1 = descriptor(recording_id=4, form=1)
    vectors.append(valid("descriptor-form-1-live", "descriptor", encode_descriptor(live_v1), live_v1, slot=4))
    purged = descriptor(recording_id=5, form=1, stop_timestamp=1_760_000_001_000_000_000,
                        stop_position=3 * 128 * 1024 * 1024, retained_position=128 * 1024 * 1024)
    vectors.append(valid("descriptor-form-1-purged", "descriptor", encode_descriptor(purged), purged, slot=5))
    purged_to_the_stop = descriptor(recording_id=6, form=1, stop_timestamp=1_760_000_001_000_000_000,
                                    stop_position=2 * 128 * 1024 * 1024, retained_position=2 * 128 * 1024 * 1024)
    vectors.append(valid("descriptor-form-1-purged-to-the-stop", "descriptor", encode_descriptor(purged_to_the_stop), purged_to_the_stop, slot=6))
    vectors.append(invalid("descriptor-form-1-retained-checksum", "descriptor", "checksum", encode_descriptor(live_v1, retained_checksum=0xDEADBEEF), slot=4))
    vectors.append(invalid("descriptor-form-1-body-checksum", "descriptor", "checksum", encode_descriptor(live_v1, body_checksum=0xDEADBEEF), slot=4))
    vectors.append(invalid("descriptor-form-1-retained-below-start", "descriptor", "position", encode_descriptor(descriptor(recording_id=4, form=1, start_position=4096, retained_position=0)), slot=4))
    vectors.append(invalid("descriptor-form-1-retained-above-stop", "descriptor", "position", encode_descriptor(descriptor(recording_id=4, form=1, stop_timestamp=1_760_000_001_000_000_000, stop_position=4096, retained_position=128 * 1024 * 1024)), slot=4))
    vectors.append(invalid("descriptor-form-1-retained-not-a-segment-base", "descriptor", "position", encode_descriptor(descriptor(recording_id=4, form=1, stop_timestamp=1_760_000_001_000_000_000, stop_position=3 * 128 * 1024 * 1024, retained_position=4096)), slot=4))
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
    vectors.append(invalid("catalog-order-version-before-descriptor-length", "catalog-header", "version", encode_catalog_header(catalog_header(version=2, descriptor_length=512)), file_length=HEADER_LENGTH))
    vectors.append(invalid("catalog-order-magic-before-version", "catalog-header", "magic", encode_catalog_header(catalog_header(magic=b"CVLYCNTR", version=2)), file_length=HEADER_LENGTH))
    vectors.append(invalid("descriptor-order-state-before-checksum", "descriptor", "state", struct.pack("<i", 4) + encode_descriptor(descriptor(), body_checksum=1)[4:], slot=0))
    vectors.append(invalid("catalog-order-checksum-kind-before-timestamp", "catalog-header", "checksum-kind", encode_catalog_header(catalog_header(checksum_kind=1, created=-1)), file_length=HEADER_LENGTH))
    vectors.append(invalid("catalog-order-timestamp-before-length", "catalog-header", "timestamp", encode_catalog_header(catalog_header(created=-1)), file_length=HEADER_LENGTH + (MAX_SLOTS + 1) * DESCRIPTOR_LENGTH))
    # Time index: valid header, rejections, then entries.
    ih = index_header(recording_id=1)
    vectors.append(valid("index-header", "index-header", encode_index_header(ih), ih, file_length=HEADER_LENGTH + 3 * ENTRY_LENGTH, entries=3))
    vectors.append(valid("index-header-part-written-last-entry-ignored", "index-header", encode_index_header(ih), ih, file_length=HEADER_LENGTH + 3 * ENTRY_LENGTH + 7, entries=3))
    vectors.append(invalid("index-too-short", "index-header", "too-short", encode_index_header(ih)[:100], file_length=100))
    vectors.append(invalid("index-magic", "index-header", "magic", encode_index_header(index_header(magic=CATALOG_MAGIC)), file_length=HEADER_LENGTH))
    vectors.append(invalid("index-not-ready-torn-header", "index-header", "not-ready", encode_index_header(index_header(magic=bytes(8))), file_length=HEADER_LENGTH))
    vectors.append(invalid("index-order-not-ready-before-version", "index-header", "not-ready", encode_index_header(index_header(magic=bytes(8), version=1)), file_length=HEADER_LENGTH))
    vectors.append(invalid("index-order-version-before-recording-id", "index-header", "version", encode_index_header(index_header(version=1, recording_id=-1)), file_length=HEADER_LENGTH))
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
    # Terms file (version 2): the header with the file length, then entries with their
    # index and the valid entry before them, each rejection in the order the checks run.
    # A created file holds -1 in both slots; its first raise, to 4, writes slot 1; the
    # next, to 6, writes slot 0, the slot the stored candidate term is not read from.
    two = TERMS_HEADER_LENGTH + 2 * TERMS_ENTRY_LENGTH
    over = TERMS_HEADER_LENGTH + (1048576 + 1) * TERMS_ENTRY_LENGTH

    def valid_terms_header(name, h, file_length=two):
        vectors.append(valid(name, "terms-header", encode_terms_header(h), terms_header_fields(h), file_length=file_length))

    def invalid_terms_header(name, error, raw, file_length=two):
        vectors.append(invalid(name, "terms-header", error, raw, file_length=file_length))

    th = terms_header(slots=(-1, 4))
    valid_terms_header("terms-header", th)
    valid_terms_header("terms-header-no-candidate", terms_header(), file_length=TERMS_HEADER_LENGTH)
    valid_terms_header("terms-header-higher-in-slot-0", terms_header(slots=(6, 4)))
    # A raise torn by a crash: the slot's value written and its checksum not. The stored
    # candidate term is the other slot's, the one stored before the raise.
    valid_terms_header("terms-header-slot-0-torn", terms_header(slots=(6, 4), checksums=(candidate_checksum(-1), None)))
    valid_terms_header("terms-header-slot-1-torn", terms_header(slots=(6, 9), checksums=(None, candidate_checksum(4))))
    invalid_terms_header("terms-too-short", "too-short", encode_terms_header(th)[:40], file_length=40)
    invalid_terms_header("terms-not-ready", "not-ready", encode_terms_header(terms_header(magic=bytes(8))),
                         file_length=TERMS_HEADER_LENGTH)
    invalid_terms_header("terms-magic", "magic", encode_terms_header(terms_header(magic=b"CVLYTIDX")))
    invalid_terms_header("terms-version", "version", encode_terms_header(terms_header(version=1)))
    invalid_terms_header("terms-entry-length", "entry-length", encode_terms_header(terms_header(entry_length=64)))
    invalid_terms_header("terms-length", "length", encode_terms_header(th), file_length=over)
    neither = terms_header(slots=(6, 4), checksums=(0, 0))
    invalid_terms_header("terms-candidate-neither-checksum", "candidate", encode_terms_header(neither))
    invalid_terms_header("terms-candidate-below-none", "candidate", encode_terms_header(terms_header(slots=(-5, 4))))
    invalid_terms_header("terms-order-magic-before-version", "magic",
                         encode_terms_header(terms_header(magic=b"CVLYTIDX", version=1)))
    invalid_terms_header("terms-order-length-before-candidate", "length", encode_terms_header(neither), file_length=over)
    first = terms_entry(2, 0, 0)
    vectors.append(valid("terms-entry-first", "terms-entry", encode_terms_entry(first), first, index=0, previous=None))
    second = terms_entry(3, 4096, 1)
    vectors.append(valid("terms-entry-after-another", "terms-entry", encode_terms_entry(second), second, index=1,
                         previous=first))
    deleted = terms_entry(4, 8192, -1, state=DELETED)
    vectors.append(valid("terms-entry-deleted", "terms-entry", encode_terms_entry(deleted), deleted, index=2,
                         previous=second))
    empty = terms_entry(0, 0, 0, state=EMPTY, checksum=0)
    vectors.append(valid("terms-entry-empty-ends-the-file", "terms-entry", encode_terms_entry(empty), empty, index=2,
                         previous=second))
    vectors.append(invalid("terms-entry-state", "terms-entry", "state", encode_terms_entry(terms_entry(3, 4096, 1, state=2)),
                           index=1, previous=first))
    vectors.append(invalid("terms-entry-checksum", "terms-entry", "checksum",
                           encode_terms_entry(terms_entry(3, 4096, 1, checksum=1)), index=1, previous=first))
    vectors.append(invalid("terms-entry-term-negative", "terms-entry", "term", encode_terms_entry(terms_entry(-1, 0, 0)),
                           index=0, previous=None))
    vectors.append(invalid("terms-entry-term-not-above", "terms-entry", "term", encode_terms_entry(terms_entry(2, 4096, 1)),
                           index=1, previous=first))
    vectors.append(invalid("terms-entry-position-not-aligned", "terms-entry", "position",
                           encode_terms_entry(terms_entry(3, 100, 1)), index=1, previous=first))
    vectors.append(invalid("terms-entry-position-below", "terms-entry", "position",
                           encode_terms_entry(terms_entry(4, 0, 1)), index=2, previous=second))
    vectors.append(invalid("terms-entry-recording", "terms-entry", "recording",
                           encode_terms_entry(terms_entry(3, 4096, -2)), index=1, previous=first))
    vectors.append(invalid("terms-entry-order-state-before-term", "terms-entry", "state",
                           encode_terms_entry(terms_entry(-1, 0, 0, state=2)), index=0, previous=None))
    # Log recordings file (version 3): the header with the file length, then one slot,
    # then both slots of a file and which of them holds the list.
    whole = RECORDINGS_FILE_LENGTH
    rh = recordings_header()

    def invalid_recordings_header(name, error, h=rh, file_length=whole):
        vectors.append(invalid(name, "recordings-header", error, encode_recordings_header(h), file_length=file_length))

    vectors.append(valid("recordings-header", "recordings-header", encode_recordings_header(rh), rh, file_length=whole))
    first_member = recordings_header(member_id=0)
    vectors.append(valid("recordings-header-member-0", "recordings-header", encode_recordings_header(first_member),
                         first_member, file_length=whole))
    invalid_recordings_header("recordings-too-short", "too-short", file_length=whole - 1)
    invalid_recordings_header("recordings-length", "length", file_length=whole + 1)
    invalid_recordings_header("recordings-not-ready", "not-ready", recordings_header(magic=bytes(8)))
    invalid_recordings_header("recordings-magic", "magic", recordings_header(magic=TERMS_MAGIC))
    invalid_recordings_header("recordings-version", "version", recordings_header(version=1))
    invalid_recordings_header("recordings-slot-length", "slot-length", recordings_header(slot_length=16432))
    invalid_recordings_header("recordings-member", "member", recordings_header(member_id=-1))
    invalid_recordings_header("recordings-order-length-before-magic", "length",
                              recordings_header(magic=TERMS_MAGIC), file_length=whole + 16)
    invalid_recordings_header("recordings-order-not-ready-before-version", "not-ready",
                              recordings_header(magic=bytes(8), version=1))
    invalid_recordings_header("recordings-order-version-before-slot-length", "version",
                              recordings_header(version=1, slot_length=16432))
    invalid_recordings_header("recordings-order-slot-length-before-member", "slot-length",
                              recordings_header(slot_length=16432, member_id=-1))

    def valid_slot(name, sequence, floor_position, entries):
        vectors.append(valid(name, "recordings-slot", recordings_slot(sequence, floor_position, entries),
                             recordings_fields(sequence, floor_position, entries)))

    def invalid_slot(name, error, raw):
        vectors.append(invalid(name, "recordings-slot", error, raw))

    # The list a creation writes; a member's first recording; a catch-up, whose replay's
    # recording and live recording overlap; a list whose table forgot the recordings
    # below 320, with two recordings that begin at one position; and a list that a
    # truncation left with no recording, whose floor says where the log ends.
    catchup = [(0, 0), (2, 96), (3, 192)]
    forgot = [(7, 320), (8, 320), (9, 416)]
    valid_slot("recordings-slot-created", 0, 0, [])
    valid_slot("recordings-slot-one-recording", 1, 0, [(0, 0)])
    valid_slot("recordings-slot-a-catch-up", 5, 0, catchup)
    valid_slot("recordings-slot-floor-above-0", 12, 320, forgot)
    valid_slot("recordings-slot-no-recording-over-a-floor", 13, 256, [])
    valid_slot("recordings-slot-full", 2000, 0, [(i, i * 32) for i in range(RECORDINGS_MAX_ENTRIES)])
    invalid_slot("recordings-slot-checksum", "checksum", recordings_slot(5, 0, catchup, checksum=0xDEADBEEF))
    # A slot no write has reached is zeros, and the CRC32 of twenty zero bytes is not 0.
    invalid_slot("recordings-slot-never-written", "checksum", bytes(24))
    invalid_slot("recordings-slot-count-negative", "checksum", recordings_slot(5, 0, [], count=-1))
    invalid_slot("recordings-slot-count-over-the-bound", "checksum",
                 recordings_slot(5, 0, [(i, i * 32) for i in range(RECORDINGS_MAX_ENTRIES + 1)]))
    # A write a crash tore after its count: the entries the count names are not there.
    invalid_slot("recordings-slot-shorter-than-its-count", "checksum", recordings_slot(5, 0, [(0, 0)], count=2))
    invalid_slot("recordings-slot-sequence", "sequence", recordings_slot(-1, 0, catchup))
    invalid_slot("recordings-slot-floor-negative", "position", recordings_slot(5, -32, []))
    invalid_slot("recordings-slot-floor-not-aligned", "position", recordings_slot(5, 48, []))
    invalid_slot("recordings-slot-recording", "recording", recordings_slot(5, 0, [(0, 0), (-1, 96)]))
    invalid_slot("recordings-slot-start-not-aligned", "position", recordings_slot(5, 0, [(0, 0), (2, 100)]))
    invalid_slot("recordings-slot-start-below-the-one-before", "position", recordings_slot(5, 0, [(0, 0), (2, 96), (3, 64)]))
    invalid_slot("recordings-slot-first-above-the-floor", "position", recordings_slot(5, 0, [(0, 96)]))
    invalid_slot("recordings-slot-first-below-the-floor", "position", recordings_slot(5, 320, [(7, 288)]))
    invalid_slot("recordings-slot-order-checksum-before-sequence", "checksum",
                 recordings_slot(-1, 0, catchup, checksum=0xDEADBEEF))
    invalid_slot("recordings-slot-order-sequence-before-floor", "sequence", recordings_slot(-1, 48, []))
    invalid_slot("recordings-slot-order-floor-before-recording", "position", recordings_slot(5, 48, [(-1, 48)]))
    invalid_slot("recordings-slot-order-recording-before-position", "recording", recordings_slot(5, 0, [(-1, 100)]))
    invalid_slot("recordings-slot-order-an-entry-before-the-next", "position",
                 recordings_slot(5, 0, [(0, 0), (2, 100), (-1, 192)]))

    def file_vector(name, slots, chosen=None, error=None):
        vector = {"name": name, "region": "recordings-file", "slots": [slot.hex() for slot in slots]}
        if error is None:
            slot, sequence, floor_position, entries = chosen
            vector["fields"] = recordings_fields(sequence, floor_position, entries, slot=slot)
        else:
            vector["error"] = error
        vectors.append(vector)

    created = recordings_slot(0, 0, [])
    one = recordings_slot(1, 0, [(0, 0)])
    two = recordings_slot(2, 0, [(0, 0), (1, 96)])
    torn = recordings_slot(3, 0, [(0, 0), (1, 96), (2, 96)], checksum=0)
    file_vector("recordings-file-created", [created, bytes(24)], chosen=(0, 0, 0, []))
    file_vector("recordings-file-first-write", [created, one], chosen=(1, 1, 0, [(0, 0)]))
    file_vector("recordings-file-second-write", [two, one], chosen=(0, 2, 0, [(0, 0), (1, 96)]))
    # A write a crash tore: the list is the one before it, in the other slot.
    file_vector("recordings-file-slot-1-torn", [two, torn], chosen=(0, 2, 0, [(0, 0), (1, 96)]))
    file_vector("recordings-file-slot-0-torn", [torn, one], chosen=(1, 1, 0, [(0, 0)]))
    file_vector("recordings-file-neither-slot", [torn, bytes(24)], error="slots")
    file_vector("recordings-file-one-sequence-twice", [one, recordings_slot(1, 0, [(4, 0)])], error="sequence")
    # A slot whose checksum matches and whose fields fail a check refuses the file,
    # beside a whole slot, beside a torn one, and slot 0's check first when both fail.
    bad_start = recordings_slot(3, 0, [(0, 0), (2, 100)])
    bad_sequence = recordings_slot(-1, 0, [(0, 0)])
    file_vector("recordings-file-a-slot-that-fails-a-check", [two, bad_start], error="position")
    file_vector("recordings-file-a-torn-slot-and-one-that-fails-a-check", [torn, bad_sequence], error="sequence")
    file_vector("recordings-file-order-slot-0-before-slot-1", [bad_start, bad_sequence], error="position")
    doc = {"spec": "durable-log", "version": 3, "generated_by": "spec/vectors/durable-log.py", "vectors": vectors}
    out = Path(__file__).resolve().parent / "durable-log.json"
    out.write_text(json.dumps(doc, indent=2) + "\n")
    print(f"durable-log.py: wrote {len(vectors)} vectors to {out}")


if __name__ == "__main__":
    main()
