#!/usr/bin/env python3
"""Write spec/vectors/udp.json: the test vectors for spec/udp.md, version 0.
The reference encoder, standard library only. Run from the repository root:

    python3 spec/vectors/udp.py

Four kinds (spec/udp.md, "Vectors"): `frame` vectors carry one control frame or
heartbeat; `datagram` vectors a datagram of data frames with the term length the reader
checks it against; `channel` vectors a channel string and what it parses to; `arithmetic`
vectors inputs and the fields that follow. See spec/README.md, "Conventions".
"""

import json
import struct
from pathlib import Path

FRAME_HEADER = struct.Struct("<ibBH")  # frame length, version, flags, type: every frame's first eight
DATA_HEADER = struct.Struct("<ibBHiiiiq")  # spec/frame.md
SETUP = struct.Struct("<ibBHiiiiiiii")  # + term offset, session id, stream id, initial term id,
# active term id, term length, mtu length, ttl
STATUS = struct.Struct("<ibBHiiiiiq")  # + session id, stream id, consumption term id,
# consumption term offset, receiver window length, receiver id
STATUS_TAGGED = struct.Struct("<ibBHiiiiiqq")  # + group tag
NAK = struct.Struct("<ibBHiiiii")  # + session id, stream id, term id, term offset, length
RTTM = struct.Struct("<ibBHiiqqq")  # + session id, stream id, echo timestamp, reception delta, receiver id
assert (SETUP.size, STATUS.size, STATUS_TAGGED.size, NAK.size, RTTM.size, DATA_HEADER.size) == (40, 36, 44, 28, 40, 32)

PAD, DATA, NAK_TYPE, SM_TYPE, SETUP_TYPE, RTTM_TYPE = 0, 1, 2, 3, 5, 6
BEGIN, END, EOS = 0x80, 0x40, 0x20
SM_SEND_SETUP, SM_EOS, RTTM_REPLY = 0x80, 0x40, 0x80
FRAME_ALIGNMENT, HEADER_LENGTH = 32, 32
MIN_TERM, MAX_TERM, MIN_MTU, MAX_MTU = 65536, 1 << 30, 64, 65504
MAX_MESSAGE_CAP = 16 * 1024 * 1024
SESSION_ID, STREAM_ID, INITIAL_TERM_ID, RECEIVER_ID = 7, 1001, -5, 0x1122334455667788
TERM_LENGTH = 65536


def i32(value):
    """Two's complement wrap to a signed 32-bit integer."""
    value &= 0xFFFF_FFFF
    return value - (1 << 32) if value >= 1 << 31 else value


def position(term_id, term_offset, initial_term_id, term_length):
    """spec/log-buffer.md, "Positions": the term count wraps in 32 bits."""
    return i32(term_id - initial_term_id) * term_length + term_offset


def max_message_length(term_length):
    return min(term_length // 8, MAX_MESSAGE_CAP)


# ---- frames -------------------------------------------------------------------------

def setup(term_offset=0, session_id=SESSION_ID, stream_id=STREAM_ID, initial_term_id=INITIAL_TERM_ID,
          active_term_id=INITIAL_TERM_ID, term_length=TERM_LENGTH, mtu_length=1408, ttl=0,
          frame_length=40, version=0, flags=0, type_=SETUP_TYPE):
    fields = {"frame_length": frame_length, "version": version, "flags": flags, "type": type_,
              "term_offset": term_offset, "session_id": session_id, "stream_id": stream_id,
              "initial_term_id": initial_term_id, "active_term_id": active_term_id,
              "term_length": term_length, "mtu_length": mtu_length, "ttl": ttl}
    return "setup", fields, SETUP.pack(*fields.values())


def status(consumption_term_id=INITIAL_TERM_ID, consumption_term_offset=0, receiver_window_length=131072,
           receiver_id=RECEIVER_ID, group_tag=None, session_id=SESSION_ID, stream_id=STREAM_ID,
           frame_length=None, version=0, flags=0, type_=SM_TYPE):
    frame_length = (44 if group_tag is not None else 36) if frame_length is None else frame_length
    fields = {"frame_length": frame_length, "version": version, "flags": flags, "type": type_,
              "session_id": session_id, "stream_id": stream_id,
              "consumption_term_id": consumption_term_id,
              "consumption_term_offset": consumption_term_offset,
              "receiver_window_length": receiver_window_length, "receiver_id": receiver_id}
    if group_tag is None:
        return "status-message", fields, STATUS.pack(*fields.values())
    fields["group_tag"] = group_tag
    return "status-message", fields, STATUS_TAGGED.pack(*fields.values())


def nak(term_id=INITIAL_TERM_ID, term_offset=1024, length=64, session_id=SESSION_ID, stream_id=STREAM_ID,
        frame_length=28, version=0, flags=0, type_=NAK_TYPE):
    fields = {"frame_length": frame_length, "version": version, "flags": flags, "type": type_,
              "session_id": session_id, "stream_id": stream_id, "term_id": term_id,
              "term_offset": term_offset, "length": length}
    return "nak", fields, NAK.pack(*fields.values())


def rttm(echo_timestamp=1_700_000_000_000_000_000, reception_delta=0, receiver_id=RECEIVER_ID,
         session_id=SESSION_ID, stream_id=STREAM_ID, frame_length=40, version=0, flags=RTTM_REPLY,
         type_=RTTM_TYPE):
    fields = {"frame_length": frame_length, "version": version, "flags": flags, "type": type_,
              "session_id": session_id, "stream_id": stream_id, "echo_timestamp": echo_timestamp,
              "reception_delta": reception_delta, "receiver_id": receiver_id}
    return "rtt-measurement", fields, RTTM.pack(*fields.values())


def heartbeat(term_offset=2048, term_id=INITIAL_TERM_ID + 3, flags=BEGIN | END, frame_length=0,
              session_id=SESSION_ID, stream_id=STREAM_ID, version=0, type_=DATA, reserved_value=0):
    fields = {"frame_length": frame_length, "version": version, "flags": flags, "type": type_,
              "term_offset": term_offset, "session_id": session_id, "stream_id": stream_id,
              "term_id": term_id, "reserved_value": reserved_value}
    return "heartbeat", fields, DATA_HEADER.pack(*fields.values())


def frame(name, made, term_length=TERM_LENGTH):
    kind, fields, raw = made
    return {"name": name, "kind": "frame", "frame": kind, "term_length": term_length,
            "bytes": raw.hex(), "fields": fields}


def bad_frame(name, error, raw, term_length=TERM_LENGTH):
    return {"name": name, "kind": "frame", "term_length": term_length, "bytes": raw.hex(),
            "error": error}


# ---- datagrams ----------------------------------------------------------------------

def data_frame(term_offset, body, term_id=INITIAL_TERM_ID, flags=BEGIN | END, type_=DATA, version=0,
               session_id=SESSION_ID, stream_id=STREAM_ID, reserved_value=0, frame_length=None):
    frame_length = HEADER_LENGTH + len(body) if frame_length is None else frame_length
    aligned = (frame_length + FRAME_ALIGNMENT - 1) & ~(FRAME_ALIGNMENT - 1) if frame_length > 0 else HEADER_LENGTH
    raw = DATA_HEADER.pack(frame_length, version, flags, type_, term_offset, session_id, stream_id,
                           term_id, reserved_value) + body
    return raw + b"\x00" * (aligned - len(raw)), {"term_offset": term_offset, "frame_length": frame_length}


def datagram(name, frames, term_length=TERM_LENGTH, error=None):
    raw = b"".join(r for r, _ in frames)
    doc = {"name": name, "kind": "datagram", "term_length": term_length, "bytes": raw.hex()}
    if error is None:
        doc["frames"] = [f for _, f in frames]
    else:
        doc["error"] = error
    return doc


# ---- channels -----------------------------------------------------------------------

def channel(name, string, endpoint_host, endpoint_port, interface_host=None, interface_port=None):
    return {"name": name, "kind": "channel", "string": string,
            "fields": {"endpoint_host": endpoint_host, "endpoint_port": endpoint_port,
                       "interface_host": interface_host, "interface_port": interface_port}}


def bad_channel(name, string, error, code=1):
    return {"name": name, "kind": "channel", "string": string, "error": error, "code": code}


# ---- arithmetic ---------------------------------------------------------------------

def sender_limit(name, sender_position, sender_limit, consumption_term_id, consumption_term_offset,
                 receiver_window_length, initial_term_id=INITIAL_TERM_ID, term_length=TERM_LENGTH):
    pos = position(consumption_term_id, consumption_term_offset, initial_term_id, term_length)
    accepted = sender_position - term_length // 2 <= pos <= sender_position + term_length + term_length // 2
    return {"name": name, "kind": "arithmetic", "rule": "sender-limit",
            "inputs": {"sender_position": sender_position, "sender_limit": sender_limit,
                       "consumption_term_id": consumption_term_id,
                       "consumption_term_offset": consumption_term_offset,
                       "receiver_window_length": receiver_window_length,
                       "initial_term_id": initial_term_id, "term_length": term_length},
            "fields": {"position": pos, "accepted": accepted,
                       "sender_limit": max(sender_limit, pos + receiver_window_length) if accepted else sender_limit}}


def retransmit_window(name, sender_position, term_id, term_offset, length,
                      initial_term_id=INITIAL_TERM_ID, term_length=TERM_LENGTH):
    resend = position(term_id, term_offset, initial_term_id, term_length)
    bottom = sender_position - term_length // 2 - max_message_length(term_length)
    return {"name": name, "kind": "arithmetic", "rule": "retransmit-window",
            "inputs": {"sender_position": sender_position, "term_id": term_id, "term_offset": term_offset,
                       "length": length, "initial_term_id": initial_term_id, "term_length": term_length},
            "fields": {"resend_position": resend, "bottom": bottom,
                       "in_window": bottom <= resend < sender_position}}


def receiver_bounds(name, last_sm_position, packet_position, payload_length, term_length=TERM_LENGTH):
    under = packet_position < last_sm_position
    over = packet_position + payload_length > last_sm_position + term_length // 2
    return {"name": name, "kind": "arithmetic", "rule": "receiver-bounds",
            "inputs": {"last_sm_position": last_sm_position, "packet_position": packet_position,
                       "payload_length": payload_length, "term_length": term_length},
            "fields": {"under_run": under, "over_run": over, "accepted": not under and not over}}


def threshold(name, receiver_window_length):
    return {"name": name, "kind": "arithmetic", "rule": "status-message-threshold",
            "inputs": {"receiver_window_length": receiver_window_length},
            "fields": {"threshold": receiver_window_length // 4}}


def image_start(name, active_term_id, term_offset, initial_term_id=INITIAL_TERM_ID, term_length=TERM_LENGTH):
    return {"name": name, "kind": "arithmetic", "rule": "image-start",
            "inputs": {"initial_term_id": initial_term_id, "active_term_id": active_term_id,
                       "term_offset": term_offset, "term_length": term_length},
            "fields": {"position": position(active_term_id, term_offset, initial_term_id, term_length),
                       "receiver_window_length": min(131072, term_length // 2)}}


# ---- the vectors --------------------------------------------------------------------

first_setup = setup()
VECTORS = [
    # Frames, valid: decoded to `fields`, and `fields` encodes to `bytes`.
    frame("setup-first", first_setup),
    frame("setup-mid-stream", setup(term_offset=32768, active_term_id=INITIAL_TERM_ID + 2,
                                    term_length=1 << 30, mtu_length=MAX_MTU, session_id=-1, stream_id=-1),
          term_length=1 << 30),
    frame("setup-reserved-flags-ignored", setup(flags=0xC0)),
    frame("status-message", status(consumption_term_id=INITIAL_TERM_ID + 1, consumption_term_offset=4096)),
    frame("status-message-group-tag", status(group_tag=-1)),
    frame("status-message-send-setup", status(flags=SM_SEND_SETUP, receiver_window_length=0)),
    frame("status-message-elicit-setup",
          status(flags=SM_SEND_SETUP, consumption_term_id=0, consumption_term_offset=0,
                 receiver_window_length=0)),
    frame("status-message-eos", status(flags=SM_EOS, consumption_term_offset=65504)),
    frame("status-message-window-zero", status(receiver_window_length=0)),
    frame("nak", nak()),
    frame("nak-whole-term-tail", nak(term_offset=0, length=TERM_LENGTH)),
    frame("rtt-measurement-probe", rttm()),
    frame("rtt-measurement-answer", rttm(flags=0, reception_delta=0, receiver_id=0)),
    frame("heartbeat", heartbeat()),
    frame("heartbeat-eos", heartbeat(flags=BEGIN | END | EOS, term_offset=65504)),
    # Frames, invalid: the first check that fails, in the order spec/udp.md gives.
    bad_frame("too-short-eight-bytes", "too-short", first_setup[2][:7]),
    bad_frame("too-short-setup", "too-short", first_setup[2][:39]),
    bad_frame("too-short-status-message", "too-short", status()[2][:35]),
    bad_frame("too-short-nak", "too-short", nak()[2][:27]),
    bad_frame("version-1", "version", setup(version=1)[2]),
    bad_frame("version-negative", "version", status(version=-1)[2]),
    bad_frame("type-error-frame", "type", setup(type_=4)[2]),
    bad_frame("type-resolution", "type", setup(type_=7)[2]),
    bad_frame("type-twelve", "type", setup(type_=12)[2]),
    bad_frame("length-setup-36", "length", setup(frame_length=36)[2]),
    bad_frame("length-status-message-40", "length", status(frame_length=40)[2] + b"\x00" * 4),
    bad_frame("length-nak-32", "length", nak(frame_length=32)[2] + b"\x00" * 4),
    bad_frame("length-heartbeat-not-32", "length", heartbeat()[2] + b"\x00" * 32),
    bad_frame("term-length-not-power-of-two", "term-length", setup(term_length=100000)[2]),
    bad_frame("term-length-too-small", "term-length", setup(term_length=32768)[2]),
    bad_frame("mtu-not-aligned", "mtu", setup(mtu_length=1400)[2]),
    bad_frame("mtu-too-large", "mtu", setup(mtu_length=65536)[2]),
    bad_frame("term-offset-not-aligned", "term-offset", setup(term_offset=48)[2]),
    bad_frame("term-offset-at-term-length", "term-offset", setup(term_offset=TERM_LENGTH)[2]),
    bad_frame("term-offset-negative-nak", "term-offset", nak(term_offset=-32)[2]),
    bad_frame("term-offset-status-message", "term-offset", status(consumption_term_offset=TERM_LENGTH)[2]),
    bad_frame("window-negative", "window", status(receiver_window_length=-1)[2]),
    bad_frame("nak-length-zero", "nak-length", nak(length=0)[2]),
    bad_frame("nak-length-not-aligned", "nak-length", nak(length=48)[2]),
    bad_frame("nak-length-past-term", "nak-length", nak(term_offset=TERM_LENGTH - 32, length=64)[2]),
    # The order of checks: a frame wrong in two ways reports the earlier check.
    bad_frame("version-before-type", "version", setup(version=1, type_=4)[2]),
    bad_frame("type-before-length", "type", setup(type_=4, frame_length=36)[2]),
    bad_frame("length-before-term-length", "length", setup(frame_length=36, term_length=100)[2]),
    # Datagrams of data frames: the frames a reader walks, or the first check that fails.
    datagram("datagram-two-frames", [data_frame(0, b"cavalry!"), data_frame(64, b"\x42" * 40)]),
    datagram("datagram-padding", [data_frame(65472, b"", flags=0, type_=PAD, frame_length=64)]),
    datagram("datagram-one-frame-fragment", [data_frame(1024, b"\x01" * 100, flags=BEGIN)]),
    datagram("datagram-not-aligned", [(data_frame(0, b"cavalry!")[0][:40], None)], error="length"),
    datagram("datagram-frame-version", [data_frame(0, b"cavalry!", version=1)], error="datagram"),
    datagram("datagram-frame-type", [data_frame(0, b"cavalry!", type_=2)], error="datagram"),
    datagram("datagram-term-offset-not-running", [data_frame(0, b"cavalry!"), data_frame(128, b"x")],
             error="datagram"),
    datagram("datagram-past-term-end", [data_frame(65504, b"\x00" * 40)], error="datagram"),
    # Channels: what a string parses to, or the reason it is refused and its code.
    channel("channel-ipv4", "cavalry:udp?endpoint=10.0.0.2:40123", "10.0.0.2", 40123),
    channel("channel-name", "cavalry:udp?endpoint=grip-link:40123", "grip-link", 40123),
    channel("channel-ipv6", "cavalry:udp?endpoint=[fd00::2]:40123", "fd00::2", 40123),
    channel("channel-interface", "cavalry:udp?endpoint=10.0.0.2:40123|interface=10.0.0.1",
            "10.0.0.2", 40123, "10.0.0.1", None),
    channel("channel-interface-port", "cavalry:udp?endpoint=10.0.0.2:40123|interface=10.0.0.1:5000",
            "10.0.0.2", 40123, "10.0.0.1", 5000),
    channel("channel-parameters-in-any-order", "cavalry:udp?interface=10.0.0.1|endpoint=10.0.0.2:1",
            "10.0.0.2", 1, "10.0.0.1", None),
    bad_channel("channel-no-scheme", "udp?endpoint=10.0.0.2:40123", "grammar"),
    bad_channel("channel-unknown-media", "cavalry:tcp?endpoint=10.0.0.2:40123", "grammar"),
    bad_channel("channel-no-parameters", "cavalry:udp", "grammar"),
    bad_channel("channel-empty-value", "cavalry:udp?endpoint=", "grammar"),
    bad_channel("channel-no-equals", "cavalry:udp?endpoint", "grammar"),
    bad_channel("channel-repeated-key", "cavalry:udp?endpoint=10.0.0.2:1|endpoint=10.0.0.2:2", "grammar"),
    bad_channel("channel-unknown-key", "cavalry:udp?endpoint=10.0.0.2:40123|mtu=1408", "key"),
    bad_channel("channel-no-endpoint", "cavalry:udp?interface=10.0.0.1", "endpoint"),
    bad_channel("channel-endpoint-no-port", "cavalry:udp?endpoint=10.0.0.2", "endpoint"),
    bad_channel("channel-endpoint-port-zero", "cavalry:udp?endpoint=10.0.0.2:0", "endpoint"),
    bad_channel("channel-endpoint-port-too-large", "cavalry:udp?endpoint=10.0.0.2:70000", "endpoint"),
    bad_channel("channel-endpoint-ipv6-no-brackets", "cavalry:udp?endpoint=fd00::2:40123", "endpoint"),
    bad_channel("channel-multicast-ipv4", "cavalry:udp?endpoint=224.0.1.1:40123", "multicast"),
    bad_channel("channel-multicast-ipv6", "cavalry:udp?endpoint=[ff02::1]:40123", "multicast"),
    # The order of checks: a string wrong in two ways reports the earlier check, whatever
    # the order of its parameters.
    bad_channel("channel-key-before-endpoint", "cavalry:udp?endpoint=10.0.0.2:0|mtu=1408", "key"),
    bad_channel("channel-grammar-before-key", "cavalry:udp?mtu=1|endpoint", "grammar"),
    bad_channel("channel-grammar-before-endpoint", "cavalry:udp?endpoint=10.0.0.2:0|endpoint=10.0.0.2:0",
                "grammar"),
    bad_channel("channel-endpoint-before-multicast", "cavalry:udp?endpoint=224.0.1.1:0", "endpoint"),
    # Arithmetic: the rules of "The sender" and "The receiver" on given inputs.
    sender_limit("sender-limit-first-status-message", 0, 0, INITIAL_TERM_ID, 0, 131072),
    sender_limit("sender-limit-rises", 8192, 131072, INITIAL_TERM_ID, 8192, 131072),
    sender_limit("sender-limit-never-falls", 8192, 139264, INITIAL_TERM_ID, 4096, 65536),
    sender_limit("sender-limit-below-window-rejected", 200000, 200000, INITIAL_TERM_ID, 0, 131072),
    sender_limit("sender-limit-above-window-rejected", 0, 0, INITIAL_TERM_ID + 2, 0, 131072),
    sender_limit("sender-limit-at-window-edge", 32768, 32768, INITIAL_TERM_ID, 0, 131072),
    sender_limit("sender-limit-term-id-wraps", 0, 0, i32(2**31 - 1 + 1), 0, 131072,
                 initial_term_id=2**31 - 1),
    retransmit_window("retransmit-in-window", 65536, INITIAL_TERM_ID, 32768, 64),
    retransmit_window("retransmit-at-sender-position", 65536, INITIAL_TERM_ID + 1, 0, 64),
    retransmit_window("retransmit-too-old", 65536, INITIAL_TERM_ID - 1, 0, 64),
    retransmit_window("retransmit-bottom-edge", 65536, INITIAL_TERM_ID, 65536 - 32768 - 8192, 32),
    receiver_bounds("receiver-accepts-at-sm-position", 65536, 65536, 1408),
    receiver_bounds("receiver-under-run", 65536, 65504, 32),
    receiver_bounds("receiver-over-run", 65536, 65536 + 32768 - 32, 64),
    receiver_bounds("receiver-at-over-run-edge", 65536, 65536 + 32768 - 64, 64),
    threshold("threshold-default-window", 131072),
    threshold("threshold-small-term", 32768),
    image_start("image-start-first-term", INITIAL_TERM_ID, 0),
    image_start("image-start-mid-stream", INITIAL_TERM_ID + 3, 4096),
    image_start("image-start-large-term", INITIAL_TERM_ID + 1, 32, term_length=1 << 30),
]


def main():
    out = Path(__file__).with_suffix(".json")
    names = [v["name"] for v in VECTORS]
    assert len(names) == len(set(names)), "vector names must be unique"
    doc = {
        "spec": "spec/udp.md",
        "version": 0,
        "generated_by": "spec/vectors/udp.py; do not edit by hand",
        "vectors": VECTORS,
    }
    out.write_text(json.dumps(doc, indent=2) + "\n")
    print(f"{out}: {len(VECTORS)} vectors")


if __name__ == "__main__":
    main()
