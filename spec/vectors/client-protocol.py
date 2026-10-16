#!/usr/bin/env python3
"""Write spec/vectors/client-protocol.json: the test vectors for spec/client-protocol.md,
version 1.

This script is the spec's reference encoder, independent of the Rust code: it packs
each command's and event's fields with the standard library and nothing else, so an
implementation that agrees with it agrees with the field tables. Run it from the
repository root after changing the vectors below:

    python3 spec/vectors/client-protocol.py

Each vector has a region, `command` or `event`, the record's type id, the message bytes
as hex, and either the fields a conforming implementation decodes or the error it
reports. See spec/README.md, "Conventions".
"""

import json
import struct
from pathlib import Path

MAX_STRING = 4096

# Type ids: spec/client-protocol.md, "Commands" and "Events".
ADD_PUBLICATION, REMOVE_PUBLICATION, ADD_EXCLUSIVE_PUBLICATION = 1, 2, 3
ADD_SUBSCRIPTION, REMOVE_SUBSCRIPTION, CLIENT_KEEPALIVE, CLIENT_CLOSE = 4, 5, 6, 11
ON_ERROR, ON_AVAILABLE_IMAGE, ON_PUBLICATION_READY = 0x0F01, 0x0F02, 0x0F03
ON_OPERATION_SUCCESS, ON_UNAVAILABLE_IMAGE = 0x0F04, 0x0F05
ON_EXCLUSIVE_PUBLICATION_READY, ON_SUBSCRIPTION_READY, ON_CLIENT_TIMEOUT = 0x0F06, 0x0F07, 0x0F0A
# Cavalry's own ranges, beside Aeron's (ADR 0025): commands from 0x0101, events from 0x1001.
START_RECORDING, STOP_RECORDING = 0x0101, 0x0102
ON_RECORDING_READY, ON_RECORDING_STOPPED = 0x1001, 0x1002

# The fields of each type, in order, with their encodings: "q" i64, "i" i32, "s" string.
COMMANDS = {
    ADD_PUBLICATION: ("add_publication", (("client_id", "q"), ("correlation_id", "q"),
                                          ("stream_id", "i"), ("channel", "s"))),
    REMOVE_PUBLICATION: ("remove_publication", (("client_id", "q"), ("correlation_id", "q"),
                                                ("registration_id", "q"))),
    ADD_EXCLUSIVE_PUBLICATION: ("add_exclusive_publication",
                                (("client_id", "q"), ("correlation_id", "q"),
                                 ("stream_id", "i"), ("channel", "s"))),
    ADD_SUBSCRIPTION: ("add_subscription", (("client_id", "q"), ("correlation_id", "q"),
                                            ("stream_id", "i"), ("channel", "s"))),
    REMOVE_SUBSCRIPTION: ("remove_subscription", (("client_id", "q"), ("correlation_id", "q"),
                                                  ("registration_id", "q"))),
    CLIENT_KEEPALIVE: ("client_keepalive", (("client_id", "q"), ("correlation_id", "q"))),
    CLIENT_CLOSE: ("client_close", (("client_id", "q"), ("correlation_id", "q"))),
    START_RECORDING: ("start_recording", (("client_id", "q"), ("correlation_id", "q"),
                                          ("stream_id", "i"), ("channel", "s"))),
    STOP_RECORDING: ("stop_recording", (("client_id", "q"), ("correlation_id", "q"),
                                        ("recording_id", "q"))),
}
EVENTS = {
    ON_ERROR: ("error", (("offending_correlation_id", "q"), ("error_code", "i"),
                         ("error_message", "s"))),
    ON_AVAILABLE_IMAGE: ("available_image", (("correlation_id", "q"), ("session_id", "i"),
                                             ("stream_id", "i"),
                                             ("subscriber_registration_id", "q"),
                                             ("subscriber_position_counter_id", "i"),
                                             ("log_file_name", "s"), ("source_identity", "s"))),
    ON_PUBLICATION_READY: ("publication_ready", (("correlation_id", "q"), ("registration_id", "q"),
                                                 ("session_id", "i"), ("stream_id", "i"),
                                                 ("publication_limit_counter_id", "i"),
                                                 ("log_file_name", "s"))),
    ON_OPERATION_SUCCESS: ("operation_success", (("correlation_id", "q"),)),
    ON_UNAVAILABLE_IMAGE: ("unavailable_image", (("correlation_id", "q"),
                                                 ("subscriber_registration_id", "q"),
                                                 ("stream_id", "i"), ("channel", "s"))),
    ON_EXCLUSIVE_PUBLICATION_READY: ("exclusive_publication_ready",
                                     (("correlation_id", "q"), ("registration_id", "q"),
                                      ("session_id", "i"), ("stream_id", "i"),
                                      ("publication_limit_counter_id", "i"),
                                      ("log_file_name", "s"))),
    ON_SUBSCRIPTION_READY: ("subscription_ready", (("correlation_id", "q"),)),
    ON_CLIENT_TIMEOUT: ("client_timeout", (("client_id", "q"),)),
    ON_RECORDING_READY: ("recording_ready", (("correlation_id", "q"), ("recording_id", "q"),
                                             ("session_id", "i"),
                                             ("recording_position_counter_id", "i"),
                                             ("recording_synced_position_counter_id", "i"))),
    ON_RECORDING_STOPPED: ("recording_stopped", (("recording_id", "q"), ("stop_position", "q"))),
}


def encode_string(s):
    """An i32 length, the UTF-8 bytes, and zero bytes to the next multiple of 4."""
    raw = s.encode("utf-8")
    return struct.pack("<i", len(raw)) + raw + b"\0" * (-len(raw) % 4)


def encode(layout, fields):
    out = b""
    for name, kind in layout:
        value = fields[name]
        out += encode_string(value) if kind == "s" else struct.pack("<" + kind, value)
    return out


def vector(region, type_id, name, fields=None, error=None, raw=None):
    table = COMMANDS if region == "command" else EVENTS
    if raw is None:
        raw = encode(table[type_id][1], fields)
    v = {"name": name, "region": region, "type": type_id, "bytes": raw.hex()}
    if error is None:
        v["fields"] = dict(fields)
        v["fields"]["name"] = table[type_id][0]
    else:
        v["error"] = error
    return v


def command(type_id, name, **fields):
    return vector("command", type_id, name, fields)


def event(type_id, name, **fields):
    return vector("event", type_id, name, fields)


IPC = "cavalry:ipc"
LOG = "publications/7.logbuffer"
VECTORS = [
    # Commands, decoded to their fields; the fields encode to the same bytes.
    command(ADD_PUBLICATION, "add-publication", client_id=0, correlation_id=1,
            stream_id=1001, channel=IPC),
    command(ADD_PUBLICATION, "add-publication-extremes", client_id=2**63 - 1,
            correlation_id=2**63 - 1, stream_id=-2**31, channel=IPC),
    command(ADD_PUBLICATION, "add-publication-other-channel", client_id=3, correlation_id=4,
            stream_id=7, channel="cavalry:udp?endpoint=localhost:40123"),
    command(ADD_PUBLICATION, "add-publication-empty-channel", client_id=3, correlation_id=4,
            stream_id=7, channel=""),
    command(ADD_PUBLICATION, "add-publication-longest-channel", client_id=3, correlation_id=5,
            stream_id=7, channel="c" * MAX_STRING),
    command(ADD_EXCLUSIVE_PUBLICATION, "add-exclusive-publication", client_id=0,
            correlation_id=2, stream_id=1001, channel=IPC),
    command(REMOVE_PUBLICATION, "remove-publication", client_id=0, correlation_id=3,
            registration_id=1),
    command(ADD_SUBSCRIPTION, "add-subscription", client_id=5, correlation_id=6,
            stream_id=1001, channel=IPC),
    command(REMOVE_SUBSCRIPTION, "remove-subscription", client_id=5, correlation_id=8,
            registration_id=6),
    command(CLIENT_KEEPALIVE, "client-keepalive", client_id=5, correlation_id=9),
    command(CLIENT_CLOSE, "client-close", client_id=5, correlation_id=10),
    command(START_RECORDING, "start-recording", client_id=5, correlation_id=11, stream_id=1001,
            channel=IPC),
    command(START_RECORDING, "start-recording-other-channel", client_id=5, correlation_id=11,
            stream_id=1001, channel="cavalry:udp?endpoint=localhost:40123"),
    command(STOP_RECORDING, "stop-recording", client_id=5, correlation_id=12, recording_id=0),
    command(STOP_RECORDING, "stop-recording-extremes", client_id=2**63 - 1,
            correlation_id=2**63 - 1, recording_id=2**63 - 1),
    # Events.
    event(ON_ERROR, "error-invalid-channel", offending_correlation_id=4, error_code=1,
          error_message="channel must be cavalry:ipc in this version (client-protocol.md, Error codes)"),
    event(ON_ERROR, "error-empty-message", offending_correlation_id=4, error_code=11,
          error_message=""),
    event(ON_AVAILABLE_IMAGE, "available-image", correlation_id=7, session_id=-1234567890,
          stream_id=1001, subscriber_registration_id=6, subscriber_position_counter_id=14,
          log_file_name=LOG, source_identity=IPC),
    event(ON_PUBLICATION_READY, "publication-ready", correlation_id=7, registration_id=7,
          session_id=-1234567890, stream_id=1001, publication_limit_counter_id=13,
          log_file_name=LOG),
    event(ON_PUBLICATION_READY, "publication-ready-joined", correlation_id=9, registration_id=7,
          session_id=-1234567890, stream_id=1001, publication_limit_counter_id=13,
          log_file_name=LOG),
    event(ON_EXCLUSIVE_PUBLICATION_READY, "exclusive-publication-ready", correlation_id=2,
          registration_id=2, session_id=99, stream_id=1001, publication_limit_counter_id=15,
          log_file_name="publications/2.logbuffer"),
    event(ON_OPERATION_SUCCESS, "operation-success", correlation_id=3),
    event(ON_UNAVAILABLE_IMAGE, "unavailable-image", correlation_id=7,
          subscriber_registration_id=6, stream_id=1001, channel=IPC),
    event(ON_SUBSCRIPTION_READY, "subscription-ready", correlation_id=6),
    event(ON_CLIENT_TIMEOUT, "client-timeout", client_id=5),
    event(ON_RECORDING_READY, "recording-ready", correlation_id=11, recording_id=0,
          session_id=-1234567890, recording_position_counter_id=16,
          recording_synced_position_counter_id=17),
    event(ON_RECORDING_STOPPED, "recording-stopped", recording_id=0, stop_position=4096),
    event(ON_RECORDING_STOPPED, "recording-stopped-at-start", recording_id=3, stop_position=0),
    event(ON_ERROR, "error-no-publication-to-record", offending_correlation_id=11, error_code=20,
          error_message="no live publication on stream 1001 and channel cavalry:ipc; add the publication first (client-protocol.md, Error codes)"),
    event(ON_ERROR, "error-unknown-recording", offending_correlation_id=12, error_code=21,
          error_message="recording 7 is not in the catalog (client-protocol.md, Error codes)"),
    event(ON_ERROR, "error-already-recording", offending_correlation_id=11, error_code=22,
          error_message="stream 1001 is already recording 0; stop recording 0 first (client-protocol.md, Error codes)"),
    # Invalid commands, by the reason a conforming implementation reports, in the order
    # the checks run.
    vector("command", ADD_PUBLICATION, "too-short-before-correlation-id", error="too-short",
           raw=struct.pack("<qi", 0, 1)),
    # The reserved ids, one per range the page names: Aeron's gap, Aeron's ids past this
    # version's, between the ranges, Cavalry's not yet taken, and above both.
    vector("command", 7, "unknown-type-reserved", error="type",
           raw=struct.pack("<qq", 0, 1)),
    vector("command", 12, "unknown-type-aeron-reserved", error="type",
           raw=struct.pack("<qq", 0, 1)),
    vector("command", 0x0100, "unknown-type-between", error="type",
           raw=struct.pack("<qq", 0, 1)),
    vector("command", 0x0103, "unknown-type-cavalry-reserved", error="type",
           raw=struct.pack("<qq", 0, 1)),
    vector("command", 0x0200, "unknown-type-above", error="type",
           raw=struct.pack("<qq", 0, 1)),
    vector("command", ADD_PUBLICATION, "too-short-no-channel", error="too-short",
           raw=struct.pack("<qqi", 0, 1, 1001)),
    vector("command", REMOVE_PUBLICATION, "too-short-no-registration-id", error="too-short",
           raw=struct.pack("<qq", 0, 1)),
    vector("command", START_RECORDING, "start-recording-too-short-no-channel", error="too-short",
           raw=struct.pack("<qqi", 5, 11, 1001)),
    vector("command", STOP_RECORDING, "stop-recording-too-short-no-recording-id",
           error="too-short", raw=struct.pack("<qq", 5, 12)),
    vector("command", ADD_PUBLICATION, "string-negative-length", error="string",
           raw=struct.pack("<qqii", 0, 1, 1001, -1)),
    vector("command", ADD_PUBLICATION, "string-over-4096", error="string",
           raw=struct.pack("<qqii", 0, 1, 1001, MAX_STRING + 1) + b"c" * (MAX_STRING + 4)),
    vector("command", ADD_PUBLICATION, "string-runs-past-message", error="string",
           raw=struct.pack("<qqii", 0, 1, 1001, 11) + b"cavalry:ip"),
    vector("command", ADD_PUBLICATION, "string-padding-missing", error="string",
           raw=struct.pack("<qqii", 0, 1, 1001, 11) + b"cavalry:ipc"),
    vector("command", START_RECORDING, "start-recording-string-runs-past-message", error="string",
           raw=struct.pack("<qqii", 5, 11, 1001, 11) + b"cavalry:ip"),
    vector("command", ADD_PUBLICATION, "length-trailing-bytes", error="length",
           raw=encode(COMMANDS[ADD_PUBLICATION][1],
                      dict(client_id=0, correlation_id=1, stream_id=1001, channel=IPC)) + b"\0\0\0\0"),
    vector("command", CLIENT_KEEPALIVE, "length-keepalive-trailing", error="length",
           raw=struct.pack("<qqi", 5, 9, 0)),
    vector("command", STOP_RECORDING, "stop-recording-length-trailing", error="length",
           raw=struct.pack("<qqqi", 5, 12, 0, 0)),
    # Invalid events, the same checks on the client's side.
    vector("event", ON_OPERATION_SUCCESS, "event-too-short", error="too-short",
           raw=struct.pack("<i", 3)),
    vector("event", 0x0F08, "event-unknown-type", error="type", raw=struct.pack("<q", 3)),
    vector("event", 0x1000, "event-unknown-type-between", error="type", raw=struct.pack("<q", 3)),
    vector("event", 0x1003, "event-unknown-type-cavalry-reserved", error="type",
           raw=struct.pack("<q", 3)),
    vector("event", 0x1100, "event-unknown-type-above", error="type", raw=struct.pack("<q", 3)),
    vector("event", ON_RECORDING_READY, "event-recording-ready-too-short", error="too-short",
           raw=struct.pack("<qqii", 11, 0, -1234567890, 16)),
    vector("event", ON_RECORDING_STOPPED, "event-recording-stopped-length-trailing", error="length",
           raw=struct.pack("<qqi", 0, 4096, 0)),
    vector("event", ON_ERROR, "event-string-runs-past-message", error="string",
           raw=struct.pack("<qii", 4, 1, 8) + b"abc\0"),
    vector("event", ON_CLIENT_TIMEOUT, "event-length-trailing", error="length",
           raw=struct.pack("<qi", 5, 0)),
]


def main():
    out = Path(__file__).with_suffix(".json")
    names = [v["name"] for v in VECTORS]
    assert len(names) == len(set(names)), "vector names must be unique"
    out.write_text(json.dumps({"spec": "spec/client-protocol.md", "version": 1,
                               "generated_by": "spec/vectors/client-protocol.py",
                               "vectors": VECTORS}, indent=1) + "\n")
    print(f"wrote {out} ({len(VECTORS)} vectors)")


if __name__ == "__main__":
    main()
