#!/usr/bin/env python3
"""Write spec/vectors/cluster.json: the test vectors for spec/cluster.md, version 0.
The reference encoder, standard library only. Run from the repository root:

    python3 spec/vectors/cluster.py

Three kinds (spec/cluster.md, "Vectors"): `message` vectors carry one message of a
stream and the fields a reader decodes or the error it reports; `members` vectors a
members string and what it parses to; `arithmetic` vectors inputs and the fields that
follow. See spec/README.md, "Conventions".
"""

import json
import struct
from pathlib import Path

SCHEMA_ID, VERSION = 1, 0
HEADER = struct.Struct("<HHHH")  # block length, template id, schema id, version
NULL = -1
MAX_MEMBERS, MAX_SESSIONS = 8, 10
LEADER_HEARTBEAT_TIMEOUT_NS = 10_000_000_000

# template id, name, block length, stream(s), fixed fields as (name, format), variable fields as
# (name, kind) with kind "ascii" or "data"; "payload" marks the session message header
TEMPLATES = {
    1: ("session-message", 24, ("log", "ingress", "egress"),
        [("leadership_term_id", "q"), ("cluster_session_id", "q"), ("timestamp", "q")], "payload"),
    2: ("session-event", 44, ("egress",),
        [("cluster_session_id", "q"), ("correlation_id", "q"), ("leadership_term_id", "q"),
         ("leader_member_id", "i"), ("code", "i"), ("version", "i"), ("leader_heartbeat_timeout_ns", "q")],
        [("detail", "ascii")]),
    3: ("session-connect", 16, ("ingress",),
        [("correlation_id", "q"), ("response_stream_id", "i"), ("version", "i")],
        [("response_channel", "ascii"), ("encoded_credentials", "data"), ("client_info", "ascii")]),
    4: ("session-close", 16, ("ingress",), [("leadership_term_id", "q"), ("cluster_session_id", "q")], []),
    5: ("session-keepalive", 16, ("ingress",), [("leadership_term_id", "q"), ("cluster_session_id", "q")], []),
    6: ("new-leader-event", 20, ("egress",),
        [("leadership_term_id", "q"), ("cluster_session_id", "q"), ("leader_member_id", "i")],
        [("ingress_endpoints", "ascii")]),
    20: ("timer-event", 24, ("log",),
         [("leadership_term_id", "q"), ("correlation_id", "q"), ("timestamp", "q")], []),
    21: ("session-open-event", 36, ("log",),
         [("leadership_term_id", "q"), ("correlation_id", "q"), ("cluster_session_id", "q"),
          ("timestamp", "q"), ("response_stream_id", "i")],
         [("response_channel", "ascii"), ("encoded_principal", "data")]),
    22: ("session-close-event", 28, ("log",),
         [("leadership_term_id", "q"), ("cluster_session_id", "q"), ("timestamp", "q"), ("close_reason", "i")],
         []),
    23: ("cluster-action", 32, ("log",),
         [("leadership_term_id", "q"), ("log_position", "q"), ("timestamp", "q"), ("action", "i"),
          ("flags", "i")], []),
    24: ("new-leadership-term-event", 48, ("log",),
         [("leadership_term_id", "q"), ("log_position", "q"), ("timestamp", "q"),
          ("term_base_log_position", "q"), ("leader_member_id", "i"), ("log_session_id", "i"),
          ("time_unit", "i"), ("app_version", "i")], []),
    25: ("membership-change-event", 40, ("log",),
         [("leadership_term_id", "q"), ("log_position", "q"), ("timestamp", "q"), ("leader_member_id", "i"),
          ("cluster_size", "i"), ("change_type", "i"), ("member_id", "i")],
         [("cluster_members", "ascii")]),
    50: ("canvass-position", 32, ("consensus",),
         [("log_leadership_term_id", "q"), ("log_position", "q"), ("leadership_term_id", "q"),
          ("follower_member_id", "i"), ("protocol_version", "i")], []),
    51: ("request-vote", 32, ("consensus",),
         [("log_leadership_term_id", "q"), ("log_position", "q"), ("candidate_term_id", "q"),
          ("candidate_member_id", "i"), ("protocol_version", "i")], []),
    52: ("vote", 36, ("consensus",),
         [("candidate_term_id", "q"), ("log_leadership_term_id", "q"), ("log_position", "q"),
          ("candidate_member_id", "i"), ("follower_member_id", "i"), ("vote", "i")], []),
    53: ("new-leadership-term", 96, ("consensus",),
         [("log_leadership_term_id", "q"), ("next_leadership_term_id", "q"),
          ("next_term_base_log_position", "q"), ("next_log_position", "q"), ("leadership_term_id", "q"),
          ("term_base_log_position", "q"), ("log_position", "q"), ("leader_recording_id", "q"),
          ("timestamp", "q"), ("leader_member_id", "i"), ("log_session_id", "i"), ("app_version", "i"),
          ("is_startup", "i"), ("commit_position", "q")], []),
    54: ("append-position", 21, ("consensus",),
         [("leadership_term_id", "q"), ("log_position", "q"), ("follower_member_id", "i"), ("flags", "B")], []),
    55: ("commit-position", 20, ("consensus",),
         [("leadership_term_id", "q"), ("log_position", "q"), ("leader_member_id", "i")], []),
    56: ("catchup-position", 20, ("consensus",),
         [("leadership_term_id", "q"), ("log_position", "q"), ("follower_member_id", "i")],
         [("catchup_endpoint", "ascii")]),
    57: ("stop-catchup", 12, ("consensus",), [("leadership_term_id", "q"), ("follower_member_id", "i")], []),
    100: ("snapshot-marker", 40, ("snapshot",),
          [("type_id", "q"), ("log_position", "q"), ("leadership_term_id", "q"), ("index", "i"), ("mark", "i"),
           ("time_unit", "i"), ("app_version", "i")], []),
}
STREAMS = {
    "log": (1, 20, 21, 22, 23, 24, 25), "consensus": tuple(range(50, 58)), "ingress": (1, 3, 4, 5),
    "egress": (1, 2, 6), "snapshot": (100,),
}
# which fields allow -1 (spec/cluster.md, each table)
ALLOW_NULL = {
    (2, "cluster_session_id"), (2, "correlation_id"), (2, "leader_member_id"),
    (50, "log_leadership_term_id"), (50, "leadership_term_id"), (51, "log_leadership_term_id"),
    (52, "log_leadership_term_id"), (53, "log_leadership_term_id"), (53, "next_leadership_term_id"),
    (53, "next_term_base_log_position"), (53, "next_log_position"), (53, "leader_recording_id"),
}
ENUMS = {
    (2, "code"): (0, 1, 2, 3, 4), (22, "close_reason"): (0, 1, 2), (23, "action"): (0, 1, 2),
    (24, "time_unit"): (0,), (25, "change_type"): (0, 1), (52, "vote"): (0, 1), (53, "is_startup"): (0, 1),
    (100, "mark"): (0, 1, 2), (100, "time_unit"): (0,),
}
MEMBER_IDS = {"leader_member_id", "follower_member_id", "candidate_member_id", "member_id"}
POSITIONS = {"log_position", "term_base_log_position", "next_term_base_log_position", "next_log_position",
             "commit_position"}
TERMS = {"leadership_term_id", "log_leadership_term_id", "next_leadership_term_id", "candidate_term_id"}


def encode(template, fields, var=None, payload=b"", block_length=None, schema_id=SCHEMA_ID, version=VERSION):
    """The bytes of one message: header, fixed fields in the template's order, then the variable
    fields each as a u32 length and its bytes, or the payload as it is."""
    name, block, _streams, fixed, variable = TEMPLATES[template]
    fmt = "<" + "".join(kind for _n, kind in fixed)
    body = struct.pack(fmt, *[fields[n] for n, _k in fixed])
    assert len(body) == block, (name, len(body), block)
    out = HEADER.pack(block if block_length is None else block_length, template, schema_id, version) + body
    if variable == "payload":
        return out + payload
    var = var or {}
    for n, kind in variable:
        value = var[n]
        raw = value.encode("ascii") if kind == "ascii" else value
        out += struct.pack("<I", len(raw)) + raw
    return out


def message(name, template, stream, fields, var=None, payload=None, error=None, bytes_=None):
    """A message vector: the stream, the bytes, and the fields a reader decodes or the error."""
    tname, _block, _streams, fixed, variable = TEMPLATES[template]
    data = bytes_ if bytes_ is not None else encode(template, fields, var, payload or b"")
    vector = {"kind": "message", "name": name, "stream": stream, "bytes": data.hex()}
    if error:
        vector["error"] = error
        return vector
    decoded = dict(fields)
    if variable == "payload":
        decoded["payload"] = (payload or b"").hex()
    else:
        for n, kind in variable:
            decoded[n] = var[n] if kind == "ascii" else var[n].hex()
    vector["template"] = tname
    vector["fields"] = decoded
    return vector


# ---- valid messages, every template once --------------------------------------------

MEMBERS = "0,10.0.0.1:20000,10.0.0.1:20001,10.0.0.1:20002,10.0.0.1:20003,10.0.0.1:20004|" \
          "1,10.0.0.2:20000,10.0.0.2:20001,10.0.0.2:20002,10.0.0.2:20003,10.0.0.2:20004|" \
          "2,10.0.0.3:20000,10.0.0.3:20001,10.0.0.3:20002,10.0.0.3:20003,10.0.0.3:20004"
INGRESS = "0=10.0.0.1:20000,1=10.0.0.2:20000,2=10.0.0.3:20000"
TS = 1_793_600_000_000  # 2026-11-06T22:13:20Z in milliseconds
CHANNEL = "cavalry:udp?endpoint=10.0.0.9:31000"

SESSION_MESSAGE = dict(leadership_term_id=3, cluster_session_id=7, timestamp=TS)
SESSION_EVENT_OK = dict(cluster_session_id=7, correlation_id=41, leadership_term_id=3, leader_member_id=0,
                        code=0, version=0, leader_heartbeat_timeout_ns=LEADER_HEARTBEAT_TIMEOUT_NS)
SESSION_CONNECT = dict(correlation_id=41, response_stream_id=102, version=0)
SESSION_CLOSE = dict(leadership_term_id=3, cluster_session_id=7)
NEW_LEADER = dict(leadership_term_id=4, cluster_session_id=7, leader_member_id=1)
TIMER = dict(leadership_term_id=3, correlation_id=99, timestamp=TS)
SESSION_OPEN = dict(leadership_term_id=3, correlation_id=41, cluster_session_id=7, timestamp=TS,
                    response_stream_id=102)
SESSION_CLOSE_EVENT = dict(leadership_term_id=3, cluster_session_id=7, timestamp=TS, close_reason=2)
CLUSTER_ACTION = dict(leadership_term_id=3, log_position=65536, timestamp=TS, action=2, flags=0)
NEW_TERM_EVENT = dict(leadership_term_id=3, log_position=65536, timestamp=TS, term_base_log_position=65536,
                      leader_member_id=0, log_session_id=1117416949, time_unit=0, app_version=0)
MEMBERSHIP = dict(leadership_term_id=3, log_position=65536, timestamp=TS, leader_member_id=0, cluster_size=3,
                  change_type=0, member_id=2)
CANVASS = dict(log_leadership_term_id=2, log_position=65536, leadership_term_id=2, follower_member_id=1,
               protocol_version=0)
REQUEST_VOTE = dict(log_leadership_term_id=2, log_position=65536, candidate_term_id=3, candidate_member_id=1,
                    protocol_version=0)
VOTE = dict(candidate_term_id=3, log_leadership_term_id=2, log_position=65536, candidate_member_id=1,
            follower_member_id=2, vote=1)
NEW_TERM = dict(log_leadership_term_id=2, next_leadership_term_id=3, next_term_base_log_position=65536,
                next_log_position=NULL, leadership_term_id=3, term_base_log_position=65536, log_position=98304,
                leader_recording_id=0, timestamp=TS, leader_member_id=0, log_session_id=1117416949,
                app_version=0, is_startup=0, commit_position=98304)
APPEND = dict(leadership_term_id=3, log_position=98304, follower_member_id=1, flags=1)
COMMIT = dict(leadership_term_id=3, log_position=98304, leader_member_id=0)
CATCHUP = dict(leadership_term_id=3, log_position=65536, follower_member_id=2)
STOP_CATCHUP = dict(leadership_term_id=3, follower_member_id=2)
SNAPSHOT = dict(type_id=1, log_position=98304, leadership_term_id=3, index=0, mark=0, time_unit=0, app_version=0)

VECTORS = [
    message("session-message-log", 1, "log", SESSION_MESSAGE, payload=b"hello"),
    message("session-message-ingress", 1, "ingress", dict(SESSION_MESSAGE, timestamp=0), payload=b"hello"),
    message("session-message-log-of-no-session", 1, "log", dict(SESSION_MESSAGE, cluster_session_id=NULL),
            payload=b"hello"),
    message("session-message-egress-empty", 1, "egress", SESSION_MESSAGE, payload=b""),
    message("session-event-ok", 2, "egress", SESSION_EVENT_OK, {"detail": ""}),
    message("session-event-redirect", 2, "egress",
            dict(SESSION_EVENT_OK, cluster_session_id=NULL, leader_member_id=1, code=2), {"detail": INGRESS}),
    message("session-event-redirect-no-leader", 2, "egress",
            dict(SESSION_EVENT_OK, cluster_session_id=NULL, leader_member_id=NULL, code=2), {"detail": INGRESS}),
    message("session-event-error-version", 2, "egress",
            dict(SESSION_EVENT_OK, cluster_session_id=NULL, code=1), {"detail": "invalid client version"}),
    message("session-event-error-limit", 2, "egress",
            dict(SESSION_EVENT_OK, cluster_session_id=NULL, code=1), {"detail": "concurrent session limit"}),
    message("session-event-error-channel", 2, "egress",
            dict(SESSION_EVENT_OK, cluster_session_id=NULL, code=1), {"detail": "invalid response channel"}),
    message("session-event-closed-timeout", 2, "egress",
            dict(SESSION_EVENT_OK, correlation_id=NULL, code=4), {"detail": "TIMEOUT"}),
    message("session-connect", 3, "ingress", SESSION_CONNECT,
            {"response_channel": CHANNEL, "encoded_credentials": b"", "client_info": "quickstart 0.1"}),
    message("session-connect-empty-info", 3, "ingress", SESSION_CONNECT,
            {"response_channel": CHANNEL, "encoded_credentials": b"", "client_info": ""}),
    message("session-connect-other-major", 3, "ingress", dict(SESSION_CONNECT, version=65536),
            {"response_channel": CHANNEL, "encoded_credentials": b"", "client_info": ""}),
    message("session-close", 4, "ingress", SESSION_CLOSE),
    message("session-keepalive", 5, "ingress", SESSION_CLOSE),
    message("new-leader-event", 6, "egress", NEW_LEADER, {"ingress_endpoints": INGRESS}),
    message("timer-event", 20, "log", TIMER),
    message("session-open-event", 21, "log", SESSION_OPEN, {"response_channel": CHANNEL, "encoded_principal": b""}),
    message("session-close-event-timeout", 22, "log", SESSION_CLOSE_EVENT),
    message("session-close-event-client", 22, "log", dict(SESSION_CLOSE_EVENT, close_reason=0)),
    message("cluster-action-snapshot", 23, "log", CLUSTER_ACTION),
    message("new-leadership-term-event", 24, "log", NEW_TERM_EVENT),
    message("membership-change-event", 25, "log", MEMBERSHIP, {"cluster_members": MEMBERS}),
    message("canvass-position", 50, "consensus", CANVASS),
    message("canvass-position-empty-log", 50, "consensus",
            dict(CANVASS, log_leadership_term_id=NULL, log_position=0, leadership_term_id=NULL)),
    message("request-vote", 51, "consensus", REQUEST_VOTE),
    message("vote-for", 52, "consensus", VOTE),
    message("vote-against", 52, "consensus", dict(VOTE, vote=0)),
    message("new-leadership-term", 53, "consensus", NEW_TERM),
    message("new-leadership-term-first", 53, "consensus",
            dict(NEW_TERM, log_leadership_term_id=NULL, next_leadership_term_id=NULL,
                 next_term_base_log_position=NULL, next_log_position=NULL, leadership_term_id=0,
                 term_base_log_position=0, log_position=0, leader_recording_id=NULL, is_startup=1,
                 commit_position=0)),
    message("append-position-catchup", 54, "consensus", APPEND),
    message("append-position", 54, "consensus", dict(APPEND, flags=0)),
    message("commit-position", 55, "consensus", COMMIT),
    message("catchup-position", 56, "consensus", CATCHUP, {"catchup_endpoint": "10.0.0.3:20003"}),
    message("stop-catchup", 57, "consensus", STOP_CATCHUP),
    message("snapshot-marker-begin", 100, "snapshot", SNAPSHOT),
    message("snapshot-marker-section", 100, "snapshot", dict(SNAPSHOT, index=2, mark=1)),
    message("snapshot-marker-end", 100, "snapshot", dict(SNAPSHOT, mark=2)),
]

# ---- rejections, in the order the checks run ----------------------------------------

commit = encode(55, COMMIT)
VECTORS += [
    message("reject-too-short-header", 55, "consensus", COMMIT, error="too-short", bytes_=commit[:7]),
    message("reject-too-short-block", 55, "consensus", COMMIT, error="too-short", bytes_=commit[:16]),
    message("reject-schema", 55, "consensus", COMMIT, error="schema", bytes_=encode(55, COMMIT, schema_id=2)),
    message("reject-version-header", 55, "consensus", COMMIT, error="version", bytes_=encode(55, COMMIT, version=1)),
    message("reject-template-reserved", 55, "consensus", COMMIT, error="template",
            bytes_=HEADER.pack(16, 7, SCHEMA_ID, VERSION) + bytes(16)),
    message("reject-template-unknown", 55, "consensus", COMMIT, error="template",
            bytes_=HEADER.pack(20, 9, SCHEMA_ID, VERSION) + bytes(20)),
    message("reject-stream", 55, "log", COMMIT, error="stream", bytes_=commit),
    message("reject-block-length", 55, "consensus", COMMIT, error="block-length",
            bytes_=encode(55, COMMIT, block_length=21) + b"\0"),
    message("reject-length-variable-field", 56, "consensus", CATCHUP, error="length",
            bytes_=encode(56, CATCHUP, {"catchup_endpoint": "10.0.0.3:20003"})[:-4]),
    message("reject-length-trailing", 4, "ingress", SESSION_CLOSE, error="length",
            bytes_=encode(4, SESSION_CLOSE) + bytes(4)),
    message("reject-version-field-major", 50, "consensus", dict(CANVASS, protocol_version=65536), error="version"),
    message("reject-version-field-negative", 51, "consensus", dict(REQUEST_VOTE, protocol_version=-65536),
            error="version"),
    message("reject-version-field-minus-one", 50, "consensus", dict(CANVASS, protocol_version=-1),
            error="version"),
    message("reject-length-before-version-field", 50, "consensus", dict(CANVASS, protocol_version=65536),
            error="length", bytes_=encode(50, dict(CANVASS, protocol_version=65536)) + bytes(4)),
    message("reject-version-field-before-member-id", 50, "consensus",
            dict(CANVASS, protocol_version=65536, follower_member_id=-2), error="version"),
    message("reject-enum-code", 2, "egress", dict(SESSION_EVENT_OK, code=5), error="enum",
            bytes_=encode(2, dict(SESSION_EVENT_OK, code=5), {"detail": ""})),
    message("reject-enum-close-reason", 22, "log", dict(SESSION_CLOSE_EVENT, close_reason=3), error="enum"),
    message("reject-enum-action", 23, "log", dict(CLUSTER_ACTION, action=3), error="enum"),
    message("reject-enum-change-type", 25, "log", dict(MEMBERSHIP, change_type=2), error="enum",
            bytes_=encode(25, dict(MEMBERSHIP, change_type=2), {"cluster_members": MEMBERS})),
    message("reject-enum-mark", 100, "snapshot", dict(SNAPSHOT, mark=3), error="enum"),
    message("reject-enum-time-unit", 24, "log", dict(NEW_TERM_EVENT, time_unit=1), error="enum"),
    message("reject-enum-vote", 52, "consensus", dict(VOTE, vote=2), error="enum"),
    message("reject-enum-is-startup", 53, "consensus", dict(NEW_TERM, is_startup=2), error="enum"),
    message("reject-ascii", 6, "egress", NEW_LEADER, error="ascii",
            bytes_=encode(6, NEW_LEADER, {"ingress_endpoints": "0=10.0.0.1:20000"}).replace(b"=", b"\x01", 1)),
    message("reject-member-id-negative", 55, "consensus", dict(COMMIT, leader_member_id=NULL), error="member-id"),
    message("reject-member-id-below-null", 2, "egress", dict(SESSION_EVENT_OK, leader_member_id=-2),
            error="member-id", bytes_=encode(2, dict(SESSION_EVENT_OK, leader_member_id=-2), {"detail": ""})),
    message("reject-position-negative", 54, "consensus", dict(APPEND, log_position=NULL), error="position"),
    message("reject-position-below-null", 53, "consensus", dict(NEW_TERM, next_log_position=-2), error="position"),
    message("reject-term", 52, "consensus", dict(VOTE, candidate_term_id=-2), error="term"),
]

# ---- members strings ----------------------------------------------------------------


def ascii_digits(text):
    return text != "" and all("0" <= c <= "9" for c in text)


def parse_endpoint(text):
    host, sep, port = text.rpartition(":")
    if not sep or not host:
        raise ValueError("members-grammar")
    if host.startswith("[") and host.endswith("]") and len(host) > 2:
        host = host[1:-1]
    elif ":" in host:
        raise ValueError("members-grammar")  # an IPv6 literal is written in brackets
    if not ascii_digits(port) or not 1 <= int(port) <= 65535:
        raise ValueError("members-grammar")
    return {"host": host, "port": int(port)}


def parse_members(text, self_id=None):
    """spec/cluster.md, "What a reader rejects": the members string's checks in order."""
    if not text:
        raise ValueError("members-grammar")
    members = []
    for item in text.split("|"):
        fields = item.split(",")
        if len(fields) != 6:
            raise ValueError("members-grammar")
        if not ascii_digits(fields[0]) or int(fields[0]) > 2**31 - 1:
            raise ValueError("members-grammar")
        member = {"id": int(fields[0])}
        for name, endpoint in zip(("ingress", "consensus", "log", "catchup", "archive"), fields[1:]):
            member[name] = parse_endpoint(endpoint)
        members.append(member)
    if len({m["id"] for m in members}) != len(members):
        raise ValueError("members-duplicate")
    if len(members) > MAX_MEMBERS:
        raise ValueError("members-count")
    if self_id is not None and all(m["id"] != self_id for m in members):
        raise ValueError("members-self")
    return members


def members(name, text, self_id=None):
    vector = {"kind": "members", "name": name, "string": text}
    if self_id is not None:
        vector["self_id"] = self_id
    try:
        vector["members"] = parse_members(text, self_id)
    except ValueError as error:
        vector["error"] = str(error)
    return vector


ONE = "0,a:1,a:2,a:3,a:4,a:5"
EIGHT = "|".join(f"{i},h{i}:1,h{i}:2,h{i}:3,h{i}:4,h{i}:5" for i in range(8))
VECTORS += [
    members("members-three", MEMBERS, self_id=1),
    members("members-one", ONE),
    members("members-ipv6", "0,[::1]:20000,[::1]:20001,[::1]:20002,[::1]:20003,[::1]:20004"),
    members("members-eight", EIGHT),
    members("members-grammar-empty", ""),
    members("members-grammar-five-fields", "0,a:1,a:2,a:3,a:4"),
    members("members-grammar-seven-fields", ONE + ",a:6"),
    members("members-grammar-id-not-integer", "x,a:1,a:2,a:3,a:4,a:5"),
    members("members-grammar-id-negative", "-1,a:1,a:2,a:3,a:4,a:5"),
    members("members-grammar-id-too-large", "2147483648,a:1,a:2,a:3,a:4,a:5"),
    members("members-grammar-no-port", "0,a,a:2,a:3,a:4,a:5"),
    members("members-grammar-empty-host", "0,:1,a:2,a:3,a:4,a:5"),
    members("members-grammar-port-zero", "0,a:0,a:2,a:3,a:4,a:5"),
    members("members-grammar-port-too-large", "0,a:65536,a:2,a:3,a:4,a:5"),
    members("members-grammar-id-not-ascii-digit", "1\u00b2,a:1,a:2,a:3,a:4,a:5"),
    members("members-grammar-port-not-ascii-digit", "0,a:\u0663,a:2,a:3,a:4,a:5"),
    members("members-grammar-ipv6-unbracketed", "0,::1:20000,[::1]:20001,[::1]:20002,[::1]:20003,[::1]:20004"),
    members("members-grammar-before-duplicate", ONE + "|0,b:1,b:2,b:3,b"),
    members("members-duplicate", ONE + "|0,b:1,b:2,b:3,b:4,b:5"),
    members("members-count", EIGHT + "|8,h8:1,h8:2,h8:3,h8:4,h8:5"),
    members("members-self", MEMBERS, self_id=3),
]

# ---- arithmetic -----------------------------------------------------------------------


def quorum(count):
    return count // 2 + 1


def commit_position(positions, leader_position, leader_term, term_base, timeout_ns=LEADER_HEARTBEAT_TIMEOUT_NS):
    """spec/cluster.md, "The commit position": a member counts when it is active, its last report was
    for the leader's term and its position is at or past the term base log position; the quorum
    position is the quorum-th highest position among those that count, 0 with fewer than a quorum,
    bounded by the leader's own (Raft's rule, not Aeron's)."""
    counted = sorted((p["position"] for p in positions
                      if p["age_ns"] < timeout_ns and p["term"] == leader_term and p["position"] >= term_base),
                     reverse=True)
    q = quorum(len(positions))
    quorum_position = counted[q - 1] if len(counted) >= q else 0
    return {"quorum_position": quorum_position, "commit_position": min(quorum_position, leader_position)}


def at_least(term, position, other_term, other_position):
    return (term, position) >= (other_term, other_position)


def ballot(count, candidate_term, votes, heard, timeout_passed):
    """spec/cluster.md, "candidate ballot" (ADR 0077): over the votes cast for the candidate's
    current candidate term alone, its own among them: leader at once when they make a quorum,
    none is against and every member heard has cast one for; else at the election timeout when a
    quorum voted for and none against, Aeron's isQuorumLeader. `heard` is the ids of the members
    the candidate has read a consensus message from since its election began."""
    cast = {v["member"]: v["vote"] for v in votes if v["term"] == candidate_term}
    quorate = all(cast.values()) and sum(1 for v in cast.values() if v) >= quorum(count)
    if quorate and all(member in cast for member in heard):
        return {"leader": True, "unanimous": True}
    if timeout_passed and quorate:
        return {"leader": True, "unanimous": False}
    return {"leader": False, "unanimous": False}


OLDER_TERM_VOTES = [{"member": 2, "vote": True, "term": 4}, {"member": 0, "vote": True, "term": 4},
                    {"member": 1, "vote": True, "term": 4}, {"member": 4, "vote": True, "term": 4}]


def candidate(count, reports, own, deadline_passed):
    """spec/cluster.md, "canvass": a unanimous candidate at once when every member has reported
    and none has a log ahead of the member's own; a quorum candidate once the deadline has
    passed when those that reported with a log not ahead, the member itself among them, make a
    quorum. `reports[i]` is member i's (term, position) or None; member 0 is the member itself."""
    reported = [r for r in reports if r is not None]
    not_ahead = [r for r in reported if at_least(own[0], own[1], r[0], r[1])]
    if len(reported) == count and len(not_ahead) == count:
        return {"nominate": True, "unanimous": True}
    if deadline_passed and len(not_ahead) >= quorum(count):
        return {"nominate": True, "unanimous": False}
    return {"nominate": False, "unanimous": False}


def appointed_leader(ids):
    """spec/cluster.md, "The appointed leader": the member with the lowest id in the members
    string leads, whatever the order the string gives the members in."""
    return {"leader": min(ids)}


def appointed_canvass(count, reports, own, deadline_passed):
    """spec/cluster.md, "The appointed leader": the appointed leader leads from canvass when
    canvass's rule makes it a candidate, unanimous or at the deadline with a quorum, and no
    member that has reported holds a log ahead of its own, whatever the deadline. `ahead` is
    how many reported members do. `reports` and `own` are `candidate`'s."""
    ahead = len([r for r in reports if r is not None and not at_least(own[0], own[1], r[0], r[1])])
    nominated = candidate(count, reports, own, deadline_passed)
    return {"leads": nominated["nominate"] and ahead == 0, "unanimous": nominated["unanimous"], "ahead": ahead}


def leader_ready(count, heard, counted, ahead, deadline_passed):
    """spec/cluster.md, "The appointed leader": in leader ready the appointed leader closes its
    election at once when every member it has heard, itself among them, counts for its term at
    its log position, and with a quorum of the members once its deadline has passed, the leader
    heartbeat timeout after it entered leader ready; never while a member's last report holds
    a log ahead of its own. `heard` is how many members it has had a canvass position or an
    append position from since its canvass began, itself among them; `counted` is how many
    count, each one of those heard, since a member counts by an append position it sent;
    `ahead` is how many are ahead. So `counted >= heard` is every member heard counting."""
    if ahead > 0 or counted < quorum(count):
        return {"closes": False}
    return {"closes": counted >= heard or deadline_passed}


def replication_stop(append_position, next_term_base_log_position, next_log_position):
    """spec/cluster.md, "follower log replication": the stop position of a follower whose append
    position is below the leader's term base log position, from the new leadership term's next
    term base log position and next log position (−1 for none)."""
    if next_term_base_log_position != NULL and append_position < next_term_base_log_position:
        return {"replicate": True, "stop_position": next_term_base_log_position}
    if next_term_base_log_position != NULL and append_position == next_term_base_log_position \
            and next_log_position != NULL:
        return {"replicate": True, "stop_position": next_log_position}
    return {"replicate": False, "stop_position": NULL}


def new_leadership_term_action(log_term, append, msg, candidate_term, in_canvass, own_base_above=NULL):
    """spec/cluster.md, "Messages during an election", new leadership term: what a reader with log
    leadership term id `log_term`, append position `append` and candidate term `candidate_term`, in
    canvass or in a ballot state, does with the message's fields, and the term it records, the
    leader's term at its base when the reader's append position, after any truncation, is at the
    leader's term base log position. A term below the candidate term is ignored, as is, in a ballot
    state, any term but the candidate term; Aeron reads any term in canvass (ADR 0058).

    `own_base_above` is the base of the lowest term in the reader's own record above the message's
    log leadership term id, −1 for none. A message that names a term below the reader's log term
    says that the leader's record holds none of the reader's terms above it. When its next
    leadership term id is above the reader's log term it answers the term the reader reported, and
    the reader drops those terms: it truncates to `own_base_above` when its append position is above
    it, and reads the message again with the log it then holds (ADR 0096). A next term at or below
    the reader's log term is an answer to an older report, and the reader canvasses."""
    none = {"truncate_to": NULL, "stop_position": NULL, "records": None}
    term = msg["leadership_term_id"]
    if term < candidate_term or (not in_canvass and term != candidate_term):
        return dict(none, action="ignore")
    if msg["log_leadership_term_id"] != log_term:
        if msg["log_leadership_term_id"] < log_term and msg["next_leadership_term_id"] > log_term \
                and own_base_above != NULL:
            return dict(none, action="drop", truncate_to=own_base_above if append > own_base_above else NULL)
        return dict(none, action="canvass")
    truncate_to = NULL
    nb = msg["next_term_base_log_position"]
    if nb != NULL and append > nb:
        truncate_to, append = nb, nb
    base = msg["term_base_log_position"]
    if append < base:
        if msg["next_leadership_term_id"] == NULL:
            return dict(none, action="fail", truncate_to=truncate_to)
        stop = replication_stop(append, nb, msg["next_log_position"])
        if stop["replicate"]:
            return dict(none, action="replicate", truncate_to=truncate_to, stop_position=stop["stop_position"])
        return dict(none, action="wait", truncate_to=truncate_to)
    records = [msg["leadership_term_id"], base] if append == base else None
    return dict(none, action="replay", truncate_to=truncate_to, records=records)


VOTING_STATES = (1, 2, 3, 4)  # canvass, nominate, candidate ballot, follower ballot


def request_vote(candidate_term, log, state, request):
    """spec/cluster.md, "Messages during an election", request vote: the vote a reader with
    candidate term `candidate_term`, log `[term, position]` and election state `state` (the states
    table's number) answers `request`, True for, False against, None for no answer, and its candidate
    term after it: against, unchanged, for a term at or below its own; against, taking the term, for a
    higher term from a log behind its own; for, taking the term, for a higher term from a log at least
    its own in canvass, nominate, candidate ballot or follower ballot; nothing in init, once the
    election has closed (17), when the member holds an election instead, or in any other state."""
    term = request["candidate_term_id"]
    if state in (0, CLOSED):
        return {"vote": None, "candidate_term": candidate_term}
    if term <= candidate_term:
        return {"vote": False, "candidate_term": candidate_term}
    if not at_least(request["log_leadership_term_id"], request["log_position"], log[0], log[1]):
        return {"vote": False, "candidate_term": term}
    if state in VOTING_STATES:
        return {"vote": True, "candidate_term": term}
    return {"vote": None, "candidate_term": candidate_term}


def vote_vector(name, candidate_term, log, state, request):
    request = {"candidate_term_id": request[0], "log_leadership_term_id": request[1], "log_position": request[2]}
    inputs = {"candidate_term": candidate_term, "log": log, "state": state, "request": request}
    return arithmetic(name, "request-vote", inputs, request_vote(candidate_term, log, state, request))


CLOSED = 17  # the election state "closed"


def canvass_position_action(leader_term, state, msg):
    """spec/cluster.md, "Messages during an election" and "Messages outside an election": what a
    leader of `leader_term` in election state `state` (5 to 8, or 17 once closed) does with a canvass
    position whose fields are `msg`: holds an election, from init, when the message's leadership
    term id, the sender's candidate term, or its log leadership term id is above the leader's term,
    in leader log replication, leader ready or closed, Raft's step-down (ADR 0058); answers with new
    leadership term when the log leadership term id is below its term, or at it once closed; else
    updates its table alone."""
    if msg["leadership_term_id"] > leader_term or msg["log_leadership_term_id"] > leader_term:
        return {"action": "election" if state in (5, 8, CLOSED) else "table"}
    if msg["log_leadership_term_id"] < leader_term or state == CLOSED:
        return {"action": "answer"}
    return {"action": "table"}


def canvass_vector(name, leader_term, state, leadership_term_id, log_leadership_term_id):
    msg = {"leadership_term_id": leadership_term_id, "log_leadership_term_id": log_leadership_term_id}
    inputs = {"leader_term": leader_term, "state": state, "message": msg}
    return arithmetic(name, "canvass-position", inputs, canvass_position_action(leader_term, state, msg))


def answered_term(terms, reported_term):
    """spec/cluster.md, new leadership term's log leadership term id: the highest term in the
    leader's record at or below the term the reader reported, −1 when the record holds none, so
    that the term a leader answers for is one its own record holds (ADR 0096). `terms` is
    [term, base, end]."""
    held = [t[0] for t in terms if t[0] <= reported_term]
    return {"log_leadership_term_id": max(held) if held else NULL}


def next_fields(terms, reported_term, leader_term, leader_base):
    """spec/cluster.md, new leadership term's next fields: for a reported term that is the leader's
    own, −1 for all three; else the lowest term in the leader's record above the reported one, the
    leader's own when none lies between, with its base and its end position, the base of the term
    after it or −1 for an open term; a departure from Aeron's lookup of the term one above, whose
    record has no gaps. `terms` is [term, base, end], the leader's own term among them."""
    if reported_term == leader_term:
        return {"next_leadership_term_id": NULL, "next_term_base_log_position": NULL, "next_log_position": NULL}
    above = [t for t in terms if t[0] > reported_term]
    entry = min(above, key=lambda t: t[0]) if above else [leader_term, leader_base, NULL]
    return {"next_leadership_term_id": entry[0], "next_term_base_log_position": entry[1],
            "next_log_position": entry[2]}


def nlt(log_leadership_term_id, next_leadership_term_id, next_term_base_log_position, next_log_position,
        leadership_term_id, term_base_log_position, log_position):
    return {"log_leadership_term_id": log_leadership_term_id, "next_leadership_term_id": next_leadership_term_id,
            "next_term_base_log_position": next_term_base_log_position, "next_log_position": next_log_position,
            "leadership_term_id": leadership_term_id, "term_base_log_position": term_base_log_position,
            "log_position": log_position}


def replay_end(terms, position):
    """spec/cluster.md, "Messages outside an election": the leader's replay for a position begins in
    the recording of the term that begins at or below the position and ends above it (an open
    term, end −1, ends above every position), and a replication's replays end at the first term
    base log position above the position; −1 when the position lies in the leader's open term,
    a catch-up that follows the live recording. `terms` is the leader's log as [term, base, end]."""
    holder = next((t for t in terms if t[1] <= position and (t[2] == NULL or position < t[2])), None)
    if holder is None:
        return {"term": NULL, "end_position": NULL}
    above = [t[1] for t in terms if t[1] > position]
    return {"term": holder[0], "end_position": min(above) if above else NULL}


LEADER_TERMS = [[2, 0, 200], [3, 200, 500], [4, 500, 1000], [5, 1000, NULL]]


def replication_record(term_replicated, term_base, stop_position, leader_term, leader_base):
    """spec/cluster.md, "follower log replication": the terms a follower records, each with its base,
    when its replication reaches the stop position: the term replicated, and the leader's term at
    the leader's base after it when the stop is the leader's term base log position."""
    recorded = [[term_replicated, term_base]]
    if stop_position == leader_base:
        recorded.append([leader_term, leader_base])
    return {"recorded": recorded, "log_leadership_term_id": max(t for t, _ in recorded)}


def entry_position(position, length, mtu):
    """spec/cluster.md, "The log": the position after an entry of `length` bytes that begins at
    `position`, fragments of mtu − 32 and a last of the remainder, each frame aligned to 32."""
    payload = mtu - 32
    fragments = max(1, -(-length // payload))
    after = position
    for i in range(fragments):
        part = payload if i < fragments - 1 else length - payload * (fragments - 1)
        after += (32 + part + 31) // 32 * 32
    return {"fragments": fragments, "position_after": after}


def arithmetic(name, rule, inputs, fields):
    return {"kind": "arithmetic", "name": name, "rule": rule, "inputs": inputs, "fields": fields}


def positions(*items):
    """Each item: member, position, age in ns, and the term the report was for (3 unless given)."""
    return [{"member": i[0], "position": i[1], "age_ns": i[2], "term": i[3] if len(i) > 3 else 3} for i in items]


def commit_vector(name, items, leader_position, leader_term=3, term_base=0):
    inputs = {"positions": positions(*items), "leader_position": leader_position, "leader_term": leader_term,
              "term_base": term_base}
    return arithmetic(name, "commit-position", inputs,
                      commit_position(positions(*items), leader_position, leader_term, term_base))


VECTORS += [arithmetic(f"quorum-of-{n}", "quorum", {"members": n}, {"quorum": quorum(n)}) for n in range(1, 9)]
VECTORS += [
    commit_vector("commit-position-all-level", [(0, 98304, 0), (1, 98304, 1000), (2, 98304, 1000)], 98304),
    commit_vector("commit-position-slowest-left-out", [(0, 98304, 0), (1, 98304, 1000), (2, 65536, 1000)], 98304),
    commit_vector("commit-position-follower-ahead-of-leader", [(0, 65536, 0), (1, 98304, 1000), (2, 98304, 1000)], 65536),
    commit_vector("commit-position-inactive-member",
                  [(0, 98304, 0), (1, 98304, LEADER_HEARTBEAT_TIMEOUT_NS), (2, 65536, 1000)], 98304),
    commit_vector("commit-position-no-quorum-active",
                  [(0, 98304, 0), (1, 98304, 20_000_000_000), (2, 98304, 20_000_000_000)], 98304),
    commit_vector("commit-position-five-members",
                  [(0, 500, 0), (1, 400, 1), (2, 300, 1), (3, 200, 1), (4, 100, 1)], 500),
    commit_vector("commit-position-below-base-not-counted",
                  [(0, 1100, 0), (1, 1000, 1000), (2, 960, 1000)], 1100, leader_term=3, term_base=1000),
    commit_vector("commit-position-older-term-not-counted",
                  [(0, 1100, 0), (1, 1100, 1000, 1), (2, 1000, 1000, 1)], 1100, leader_term=3, term_base=1000),
    commit_vector("commit-position-at-base-counts",
                  [(0, 1100, 0), (1, 1000, 1000), (2, 960, 1000, 1)], 1100, leader_term=3, term_base=1000),
    arithmetic("compare-log-higher-term", "compare-log",
               {"term": 3, "position": 0, "other_term": 2, "other_position": 98304}, {"at_least": True}),
    arithmetic("compare-log-lower-term", "compare-log",
               {"term": 2, "position": 98304, "other_term": 3, "other_position": 0}, {"at_least": False}),
    arithmetic("compare-log-same-term-ahead", "compare-log",
               {"term": 3, "position": 98304, "other_term": 3, "other_position": 65536}, {"at_least": True}),
    arithmetic("compare-log-same-term-behind", "compare-log",
               {"term": 3, "position": 65536, "other_term": 3, "other_position": 98304}, {"at_least": False}),
    arithmetic("compare-log-equal", "compare-log",
               {"term": 3, "position": 65536, "other_term": 3, "other_position": 65536}, {"at_least": True}),
    arithmetic("compare-log-joined-term-no-entries", "compare-log",
               {"term": 1, "position": 1100, "other_term": 2, "other_position": 1000}, {"at_least": False}),
    arithmetic("compare-log-joined-term-outranks-longer-log", "compare-log",
               {"term": 2, "position": 1000, "other_term": 1, "other_position": 1100}, {"at_least": True}),
    arithmetic("compare-log-empty", "compare-log",
               {"term": NULL, "position": 0, "other_term": 0, "other_position": 0}, {"at_least": False}),
    arithmetic("ballot-unanimous", "ballot",
               {"members": 3, "candidate_term": 3, "votes": [{"member": 0, "vote": True, "term": 3}, {"member": 1, "vote": True, "term": 3},
                                        {"member": 2, "vote": True, "term": 3}], "heard": [0, 1, 2], "timeout_passed": False},
               {"leader": True, "unanimous": True}),
    arithmetic("ballot-quorum-before-timeout", "ballot",
               {"members": 3, "candidate_term": 3, "votes": [{"member": 0, "vote": True, "term": 3}, {"member": 1, "vote": True, "term": 3}],
                "heard": [0, 1, 2], "timeout_passed": False}, {"leader": False, "unanimous": False}),
    arithmetic("ballot-quorum-at-timeout", "ballot",
               {"members": 3, "candidate_term": 3, "votes": [{"member": 0, "vote": True, "term": 3}, {"member": 1, "vote": True, "term": 3}],
                "heard": [0, 1, 2], "timeout_passed": True}, {"leader": True, "unanimous": False}),
    arithmetic("ballot-one-against", "ballot",
               {"members": 3, "candidate_term": 3, "votes": [{"member": 0, "vote": True, "term": 3}, {"member": 1, "vote": True, "term": 3},
                                        {"member": 2, "vote": False, "term": 3}], "heard": [0, 1, 2], "timeout_passed": True},
               {"leader": False, "unanimous": False}),
    arithmetic("ballot-short-of-quorum", "ballot",
               {"members": 5, "candidate_term": 3, "votes": [{"member": 0, "vote": True, "term": 3}, {"member": 1, "vote": True, "term": 3}],
                "heard": [0, 1], "timeout_passed": True}, {"leader": False, "unanimous": False}),
    arithmetic("ballot-votes-for-an-older-term-not-leader", "ballot",
               {"members": 5, "candidate_term": 5, "votes": OLDER_TERM_VOTES, "heard": [0, 1, 2, 4], "timeout_passed": True},
               ballot(5, 5, OLDER_TERM_VOTES, [0, 1, 2, 4], True)),
    arithmetic("ballot-single-member", "ballot",
               {"members": 1, "candidate_term": 3, "votes": [{"member": 0, "vote": True, "term": 3}], "heard": [0], "timeout_passed": False},
               {"leader": True, "unanimous": True}),
    # The members heard (ADR 0077): a member that has said nothing since the candidate's election
    # began, a leader that was killed, is not waited for; a member heard is.
    arithmetic("ballot-quorum-of-the-members-heard-before-timeout", "ballot",
               {"members": 3, "candidate_term": 3, "votes": [{"member": 0, "vote": True, "term": 3}, {"member": 1, "vote": True, "term": 3}],
                "heard": [0, 1], "timeout_passed": False}, {"leader": True, "unanimous": True}),
    arithmetic("ballot-five-with-two-silent-before-timeout", "ballot",
               {"members": 5, "candidate_term": 3, "votes": [{"member": 0, "vote": True, "term": 3}, {"member": 1, "vote": True, "term": 3},
                                        {"member": 3, "vote": True, "term": 3}], "heard": [0, 1, 3], "timeout_passed": False},
               {"leader": True, "unanimous": True}),
    arithmetic("ballot-the-members-heard-short-of-a-quorum", "ballot",
               {"members": 5, "candidate_term": 3, "votes": [{"member": 0, "vote": True, "term": 3}, {"member": 1, "vote": True, "term": 3}],
                "heard": [0, 1], "timeout_passed": False}, {"leader": False, "unanimous": False}),
    arithmetic("ballot-a-member-heard-against-before-timeout", "ballot",
               {"members": 3, "candidate_term": 3, "votes": [{"member": 0, "vote": True, "term": 3}, {"member": 1, "vote": True, "term": 3},
                                        {"member": 2, "vote": False, "term": 3}], "heard": [0, 1, 2], "timeout_passed": False},
               {"leader": False, "unanimous": False}),
    arithmetic("ballot-a-member-heard-voted-in-an-older-term", "ballot",
               {"members": 3, "candidate_term": 4, "votes": [{"member": 0, "vote": True, "term": 4}, {"member": 1, "vote": True, "term": 4},
                                        {"member": 2, "vote": True, "term": 3}], "heard": [0, 1, 2], "timeout_passed": False},
               {"leader": False, "unanimous": False}),
    arithmetic("candidate-unanimous", "candidate",
               {"members": 3, "reports": [[3, 98304], [3, 98304], [2, 65536]], "own": [3, 98304],
                "deadline_passed": False}, {"nominate": True, "unanimous": True}),
    arithmetic("candidate-one-silent-before-deadline", "candidate",
               {"members": 3, "reports": [[3, 98304], [3, 98304], None], "own": [3, 98304],
                "deadline_passed": False}, {"nominate": False, "unanimous": False}),
    arithmetic("candidate-one-silent-at-deadline", "candidate",
               {"members": 3, "reports": [[3, 98304], [3, 98304], None], "own": [3, 98304],
                "deadline_passed": True}, {"nominate": True, "unanimous": False}),
    arithmetic("candidate-one-ahead-reported", "candidate",
               {"members": 3, "reports": [[3, 98304], [3, 131072], [3, 65536]], "own": [3, 98304],
                "deadline_passed": False}, {"nominate": False, "unanimous": False}),
    arithmetic("candidate-one-ahead-at-deadline", "candidate",
               {"members": 3, "reports": [[3, 98304], [3, 131072], [3, 65536]], "own": [3, 98304],
                "deadline_passed": True}, {"nominate": True, "unanimous": False}),
    arithmetic("candidate-ahead-by-term", "candidate",
               {"members": 3, "reports": [[3, 0], [2, 131072], [2, 131072]], "own": [3, 0],
                "deadline_passed": False}, {"nominate": True, "unanimous": True}),
    arithmetic("candidate-voter-joined-later-term", "candidate",
               {"members": 3, "reports": [[1, 1100], [2, 1000], None], "own": [1, 1100],
                "deadline_passed": True}, {"nominate": False, "unanimous": False}),
    arithmetic("candidate-short-of-quorum", "candidate",
               {"members": 5, "reports": [[3, 98304], [4, 0], None, None, None], "own": [3, 98304],
                "deadline_passed": True}, {"nominate": False, "unanimous": False}),
    arithmetic("replication-stop-below-next-base", "replication-stop",
               {"append_position": 960, "next_term_base_log_position": 1000, "next_log_position": NULL},
               replication_stop(960, 1000, NULL)),
    arithmetic("replication-stop-at-next-base-with-end", "replication-stop",
               {"append_position": 1000, "next_term_base_log_position": 1000, "next_log_position": 1100},
               replication_stop(1000, 1000, 1100)),
    arithmetic("replication-stop-at-next-base-no-end", "replication-stop",
               {"append_position": 1000, "next_term_base_log_position": 1000, "next_log_position": NULL},
               replication_stop(1000, 1000, NULL)),
    arithmetic("replication-stop-nothing-known", "replication-stop",
               {"append_position": 960, "next_term_base_log_position": NULL, "next_log_position": NULL},
               replication_stop(960, NULL, NULL)),
    arithmetic("new-leadership-term-returning-leader-truncates", "new-leadership-term",
               {"log_leadership_term_id": 4, "append_position": 1100, "candidate_term": 5,
                "in_canvass": True, "own_base_above": NULL, "message": nlt(4, 5, 1000, NULL, 5, 1000, 5000)},
               new_leadership_term_action(4, 1100, nlt(4, 5, 1000, NULL, 5, 1000, 5000), 5, True)),
    arithmetic("new-leadership-term-behind-replicates", "new-leadership-term",
               {"log_leadership_term_id": 4, "append_position": 960, "candidate_term": 5,
                "in_canvass": True, "own_base_above": NULL, "message": nlt(4, 5, 1000, NULL, 5, 1000, 5000)},
               new_leadership_term_action(4, 960, nlt(4, 5, 1000, NULL, 5, 1000, 5000), 5, True)),
    arithmetic("new-leadership-term-two-terms-behind", "new-leadership-term",
               {"log_leadership_term_id": 2, "append_position": 200, "candidate_term": 5,
                "in_canvass": True, "own_base_above": NULL, "message": nlt(2, 3, 200, 500, 5, 1000, 5000)},
               new_leadership_term_action(2, 200, nlt(2, 3, 200, 500, 5, 1000, 5000), 5, True)),
    arithmetic("new-leadership-term-own-term-replays", "new-leadership-term",
               {"log_leadership_term_id": 5, "append_position": 3000, "candidate_term": 5,
                "in_canvass": True, "own_base_above": NULL, "message": nlt(5, NULL, NULL, NULL, 5, 1000, 5000)},
               new_leadership_term_action(5, 3000, nlt(5, NULL, NULL, NULL, 5, 1000, 5000), 5, True)),
    arithmetic("new-leadership-term-other-log-term-canvasses", "new-leadership-term",
               {"log_leadership_term_id": 3, "append_position": 900, "candidate_term": 5,
                "in_canvass": True, "own_base_above": NULL, "message": nlt(4, 5, 1000, NULL, 5, 1000, 5000)},
               new_leadership_term_action(3, 900, nlt(4, 5, 1000, NULL, 5, 1000, 5000), 5, True)),
    arithmetic("new-leadership-term-no-next-term-fails", "new-leadership-term",
               {"log_leadership_term_id": 4, "append_position": 960, "candidate_term": 5,
                "in_canvass": True, "own_base_above": NULL, "message": nlt(4, NULL, NULL, NULL, 5, 1000, 5000)},
               new_leadership_term_action(4, 960, nlt(4, NULL, NULL, NULL, 5, 1000, 5000), 5, True)),
    arithmetic("new-leadership-term-at-leaders-base-records", "new-leadership-term",
               {"log_leadership_term_id": 1, "append_position": 1100, "candidate_term": 2,
                "in_canvass": True, "own_base_above": NULL, "message": nlt(1, 2, 1100, NULL, 2, 1100, 1100)},
               new_leadership_term_action(1, 1100, nlt(1, 2, 1100, NULL, 2, 1100, 1100), 2, True)),
    arithmetic("new-leadership-term-stale-tail-truncates", "new-leadership-term",
               {"log_leadership_term_id": 2, "append_position": 300, "candidate_term": 5,
                "in_canvass": True, "own_base_above": NULL, "message": nlt(2, 4, 200, 1000, 5, 1000, 1000)},
               new_leadership_term_action(2, 300, nlt(2, 4, 200, 1000, 5, 1000, 1000), 5, True)),
    arithmetic("new-leadership-term-at-next-base-no-end-waits", "new-leadership-term",
               {"log_leadership_term_id": 3, "append_position": 500, "candidate_term": 5,
                "in_canvass": True, "own_base_above": NULL, "message": nlt(3, 4, 500, NULL, 5, 1000, 5000)},
               new_leadership_term_action(3, 500, nlt(3, 4, 500, NULL, 5, 1000, 5000), 5, True)),
    arithmetic("new-leadership-term-below-candidate-term-ignored", "new-leadership-term",
               {"log_leadership_term_id": 2, "append_position": 100, "candidate_term": 4,
                "in_canvass": True, "own_base_above": NULL, "message": nlt(2, 3, 200, NULL, 3, 200, 200)},
               new_leadership_term_action(2, 100, nlt(2, 3, 200, NULL, 3, 200, 200), 4, True)),
    arithmetic("new-leadership-term-above-candidate-term-in-canvass-replays", "new-leadership-term",
               {"log_leadership_term_id": 5, "append_position": 3000, "candidate_term": 2,
                "in_canvass": True, "own_base_above": NULL, "message": nlt(5, NULL, NULL, NULL, 5, 1000, 5000)},
               new_leadership_term_action(5, 3000, nlt(5, NULL, NULL, NULL, 5, 1000, 5000), 2, True)),
    arithmetic("new-leadership-term-other-term-in-ballot-ignored", "new-leadership-term",
               {"log_leadership_term_id": 4, "append_position": 960, "candidate_term": 4,
                "in_canvass": False, "own_base_above": NULL, "message": nlt(4, 5, 1000, NULL, 5, 1000, 5000)},
               new_leadership_term_action(4, 960, nlt(4, 5, 1000, NULL, 5, 1000, 5000), 4, False)),
    arithmetic("new-leadership-term-term-the-leader-lacks-is-dropped", "new-leadership-term",
               {"log_leadership_term_id": 3, "append_position": 500, "candidate_term": 5,
                "in_canvass": True, "own_base_above": 500, "message": nlt(2, 5, 1000, NULL, 5, 1000, 5000)},
               new_leadership_term_action(3, 500, nlt(2, 5, 1000, NULL, 5, 1000, 5000), 5, True, 500)),
    arithmetic("new-leadership-term-term-the-leader-lacks-with-a-tail-truncates", "new-leadership-term",
               {"log_leadership_term_id": 3, "append_position": 700, "candidate_term": 5,
                "in_canvass": True, "own_base_above": 500, "message": nlt(2, 5, 1000, NULL, 5, 1000, 5000)},
               new_leadership_term_action(3, 700, nlt(2, 5, 1000, NULL, 5, 1000, 5000), 5, True, 500)),
    arithmetic("new-leadership-term-term-the-leader-lacks-in-ballot-is-dropped", "new-leadership-term",
               {"log_leadership_term_id": 3, "append_position": 500, "candidate_term": 5,
                "in_canvass": False, "own_base_above": 500, "message": nlt(2, 5, 1000, NULL, 5, 1000, 5000)},
               new_leadership_term_action(3, 500, nlt(2, 5, 1000, NULL, 5, 1000, 5000), 5, False, 500)),
    arithmetic("new-leadership-term-answer-to-an-older-report-canvasses", "new-leadership-term",
               {"log_leadership_term_id": 4, "append_position": 900, "candidate_term": 5,
                "in_canvass": True, "own_base_above": 200, "message": nlt(2, 3, 200, 500, 5, 1000, 5000)},
               new_leadership_term_action(4, 900, nlt(2, 3, 200, 500, 5, 1000, 5000), 5, True, 200)),
    arithmetic("new-leadership-term-no-term-of-the-leaders-drops-every-term", "new-leadership-term",
               {"log_leadership_term_id": 1, "append_position": 0, "candidate_term": 2,
                "in_canvass": True, "own_base_above": 0, "message": nlt(NULL, 2, 0, NULL, 2, 0, 0)},
               new_leadership_term_action(1, 0, nlt(NULL, 2, 0, NULL, 2, 0, 0), 2, True, 0)),
    vote_vector("request-vote-at-candidate-term-against", 3, [2, 200], 1, (3, 2, 200)),
    vote_vector("request-vote-below-candidate-term-against", 4, [2, 200], 1, (3, 2, 300)),
    vote_vector("request-vote-higher-term-log-behind-against-takes-term", 2, [2, 200], 1, (3, 2, 100)),
    vote_vector("request-vote-higher-term-log-equal-for", 2, [2, 200], 1, (3, 2, 200)),
    vote_vector("request-vote-higher-term-log-term-ahead-for", 2, [2, 500], 1, (3, 3, 100)),
    vote_vector("request-vote-second-request-for-a-voted-term-against", 3, [2, 200], 4, (3, 2, 300)),
    vote_vector("request-vote-in-follower-ballot-higher-term-for", 3, [2, 100], 4, (4, 2, 100)),
    vote_vector("request-vote-outside-the-voting-states-nothing", 3, [2, 200], 10, (4, 2, 200)),
    vote_vector("request-vote-in-init-nothing", 3, [2, 200], 0, (4, 2, 200)),
    vote_vector("request-vote-in-leader-ready-log-behind-against-takes-term", 3, [3, 200], 8, (4, 3, 100)),
    vote_vector("request-vote-in-leader-ready-log-at-least-nothing", 3, [3, 200], 8, (4, 3, 200)),
    vote_vector("request-vote-closed-nothing", 3, [3, 200], CLOSED, (4, 3, 200)),
    canvass_vector("canvass-position-stale-candidate-term-holds-election", 3, CLOSED, 4, 2),
    canvass_vector("canvass-position-higher-term-in-leader-ready-fails-election", 3, 8, 4, 2),
    canvass_vector("canvass-position-higher-log-term-in-leader-log-replication-fails-election", 3, 5, 4, 4),
    canvass_vector("canvass-position-higher-term-in-leader-replay-table", 3, 6, 4, 2),
    canvass_vector("canvass-position-log-term-behind-answered", 3, CLOSED, 3, 2),
    canvass_vector("canvass-position-log-term-behind-during-election-answered", 3, 8, 2, 2),
    canvass_vector("canvass-position-own-term-closed-answered", 3, CLOSED, 3, 3),
    canvass_vector("canvass-position-own-term-during-election-table", 3, 8, 3, 3),
    arithmetic("answered-term-the-reported-term", "answered-term",
               {"terms": LEADER_TERMS, "reported_term": 4}, answered_term(LEADER_TERMS, 4)),
    arithmetic("answered-term-the-term-below-one-the-record-lacks", "answered-term",
               {"terms": [[2, 0, 200], [4, 200, 1000], [5, 1000, NULL]], "reported_term": 3},
               answered_term([[2, 0, 200], [4, 200, 1000], [5, 1000, NULL]], 3)),
    arithmetic("answered-term-none-at-or-below", "answered-term",
               {"terms": [[2, 0, 200], [4, 200, 1000], [5, 1000, NULL]], "reported_term": 1},
               answered_term([[2, 0, 200], [4, 200, 1000], [5, 1000, NULL]], 1)),
    arithmetic("answered-term-no-term-reported", "answered-term",
               {"terms": LEADER_TERMS, "reported_term": NULL}, answered_term(LEADER_TERMS, NULL)),
    arithmetic("answered-term-the-leaders-own", "answered-term",
               {"terms": LEADER_TERMS, "reported_term": 5}, answered_term(LEADER_TERMS, 5)),
    arithmetic("next-fields-term-behind", "next-fields",
               {"terms": LEADER_TERMS, "reported_term": 2, "leader_term": 5, "leader_base": 1000},
               next_fields(LEADER_TERMS, 2, 5, 1000)),
    arithmetic("next-fields-term-before-leaders", "next-fields",
               {"terms": LEADER_TERMS, "reported_term": 4, "leader_term": 5, "leader_base": 1000},
               next_fields(LEADER_TERMS, 4, 5, 1000)),
    arithmetic("next-fields-leaders-own-term", "next-fields",
               {"terms": LEADER_TERMS, "reported_term": 5, "leader_term": 5, "leader_base": 1000},
               next_fields(LEADER_TERMS, 5, 5, 1000)),
    arithmetic("next-fields-term-the-leader-lacks", "next-fields",
               {"terms": [[2, 0, 200], [4, 200, 1000], [5, 1000, NULL]], "reported_term": 3, "leader_term": 5,
                "leader_base": 1000},
               next_fields([[2, 0, 200], [4, 200, 1000], [5, 1000, NULL]], 3, 5, 1000)),
    arithmetic("next-fields-gap-after-reported-term", "next-fields",
               {"terms": [[2, 0, 200], [4, 200, 1000], [5, 1000, NULL]], "reported_term": 2, "leader_term": 5,
                "leader_base": 1000},
               next_fields([[2, 0, 200], [4, 200, 1000], [5, 1000, NULL]], 2, 5, 1000)),
    arithmetic("replay-end-mid-term", "replay-end", {"terms": LEADER_TERMS, "position": 960},
               replay_end(LEADER_TERMS, 960)),
    arithmetic("replay-end-at-term-boundary", "replay-end", {"terms": LEADER_TERMS, "position": 200},
               replay_end(LEADER_TERMS, 200)),
    arithmetic("replay-end-at-leaders-base", "replay-end", {"terms": LEADER_TERMS, "position": 1000},
               replay_end(LEADER_TERMS, 1000)),
    arithmetic("replay-end-in-leaders-term", "replay-end", {"terms": LEADER_TERMS, "position": 3000},
               replay_end(LEADER_TERMS, 3000)),
    arithmetic("replay-end-at-log-start", "replay-end", {"terms": LEADER_TERMS, "position": 0},
               replay_end(LEADER_TERMS, 0)),
    arithmetic("replication-record-stop-at-leaders-base", "replication-record",
               {"term_replicated": 4, "term_base": 500, "stop_position": 1000, "leader_term": 5, "leader_base": 1000},
               replication_record(4, 500, 1000, 5, 1000)),
    arithmetic("replication-record-stop-below-base", "replication-record",
               {"term_replicated": 3, "term_base": 200, "stop_position": 500, "leader_term": 5, "leader_base": 1000},
               replication_record(3, 200, 500, 5, 1000)),
    arithmetic("entry-position-one-frame", "entry-position",
               {"position": 65536, "length": 56, "mtu": 1408}, entry_position(65536, 56, 1408)),
    arithmetic("entry-position-aligned", "entry-position",
               {"position": 65536, "length": 32, "mtu": 1408}, entry_position(65536, 32, 1408)),
    arithmetic("entry-position-full-frame", "entry-position",
               {"position": 0, "length": 1376, "mtu": 1408}, entry_position(0, 1376, 1408)),
    arithmetic("entry-position-two-fragments", "entry-position",
               {"position": 0, "length": 1377, "mtu": 1408}, entry_position(0, 1377, 1408)),
    arithmetic("entry-position-many-fragments", "entry-position",
               {"position": 98304, "length": 100000, "mtu": 1408}, entry_position(98304, 100000, 1408)),
    arithmetic("appointed-leader-lowest-of-three", "appointed-leader", {"members": [0, 1, 2]},
               appointed_leader([0, 1, 2])),
    arithmetic("appointed-leader-string-order-does-not-decide", "appointed-leader", {"members": [7, 3, 5]},
               appointed_leader([7, 3, 5])),
    arithmetic("appointed-leader-of-one", "appointed-leader", {"members": [4]}, appointed_leader([4])),
    arithmetic("appointed-leader-of-eight", "appointed-leader", {"members": [10, 11, 12, 13, 14, 15, 16, 9]},
               appointed_leader([10, 11, 12, 13, 14, 15, 16, 9])),
    arithmetic("appointed-canvass-every-member-level", "appointed-canvass",
               {"members": 3, "reports": [[NULL, 0], [NULL, 0], [NULL, 0]], "own": [NULL, 0],
                "deadline_passed": False}, appointed_canvass(3, [[NULL, 0], [NULL, 0], [NULL, 0]], [NULL, 0], False)),
    arithmetic("appointed-canvass-one-silent-before-the-deadline", "appointed-canvass",
               {"members": 3, "reports": [[NULL, 0], [NULL, 0], None], "own": [NULL, 0],
                "deadline_passed": False}, appointed_canvass(3, [[NULL, 0], [NULL, 0], None], [NULL, 0], False)),
    arithmetic("appointed-canvass-one-silent-at-the-deadline", "appointed-canvass",
               {"members": 3, "reports": [[NULL, 0], [NULL, 0], None], "own": [NULL, 0],
                "deadline_passed": True}, appointed_canvass(3, [[NULL, 0], [NULL, 0], None], [NULL, 0], True)),
    arithmetic("appointed-canvass-alone-at-the-deadline", "appointed-canvass",
               {"members": 3, "reports": [[NULL, 0], None, None], "own": [NULL, 0],
                "deadline_passed": True}, appointed_canvass(3, [[NULL, 0], None, None], [NULL, 0], True)),
    # The appointed leader started again on an empty log directory, among members that hold a
    # term it no longer has: it never leads, at once or at any deadline.
    arithmetic("appointed-canvass-empty-log-among-members-with-a-term", "appointed-canvass",
               {"members": 3, "reports": [[NULL, 0], [0, 4096], [0, 4096]], "own": [NULL, 0],
                "deadline_passed": True}, appointed_canvass(3, [[NULL, 0], [0, 4096], [0, 4096]], [NULL, 0], True)),
    arithmetic("appointed-canvass-empty-log-one-ahead-one-silent", "appointed-canvass",
               {"members": 3, "reports": [[NULL, 0], [0, 4096], None], "own": [NULL, 0],
                "deadline_passed": True}, appointed_canvass(3, [[NULL, 0], [0, 4096], None], [NULL, 0], True)),
    # Where `candidate` would lead with a quorum and leave the member ahead behind, the
    # appointed leader does not: no truncation of this version brings that member back.
    arithmetic("appointed-canvass-quorum-level-one-ahead", "appointed-canvass",
               {"members": 3, "reports": [[NULL, 0], [NULL, 0], [0, 4096]], "own": [NULL, 0],
                "deadline_passed": True}, appointed_canvass(3, [[NULL, 0], [NULL, 0], [0, 4096]], [NULL, 0], True)),
    arithmetic("appointed-canvass-ahead-by-a-term-of-no-entries", "appointed-canvass",
               {"members": 3, "reports": [[NULL, 0], [NULL, 0], [0, 0]], "own": [NULL, 0],
                "deadline_passed": True}, appointed_canvass(3, [[NULL, 0], [NULL, 0], [0, 0]], [NULL, 0], True)),
    arithmetic("appointed-canvass-ahead-by-position", "appointed-canvass",
               {"members": 3, "reports": [[0, 4096], [0, 4096], [0, 8192]], "own": [0, 4096],
                "deadline_passed": True}, appointed_canvass(3, [[0, 4096], [0, 4096], [0, 8192]], [0, 4096], True)),
    arithmetic("appointed-canvass-of-one", "appointed-canvass",
               {"members": 1, "reports": [[NULL, 0]], "own": [NULL, 0],
                "deadline_passed": False}, appointed_canvass(1, [[NULL, 0]], [NULL, 0], False)),
    arithmetic("appointed-canvass-five-four-level-at-the-deadline", "appointed-canvass",
               {"members": 5, "reports": [[NULL, 0], [NULL, 0], [NULL, 0], [NULL, 0], None], "own": [NULL, 0],
                "deadline_passed": True}, appointed_canvass(5, [[NULL, 0], [NULL, 0], [NULL, 0], [NULL, 0], None], [NULL, 0], True)),
    arithmetic("leader-ready-every-member", "leader-ready",
               {"members": 3, "heard": 3, "counted": 3, "ahead": 0,
                "deadline_passed": False}, leader_ready(3, 3, 3, 0, False)),
    # One member heard has not joined: the third of three that all canvassed, or the second,
    # which canvassed and is slow, while a third that was silent through the canvass reported
    # after it and has joined. The leader waits for the member it heard either way.
    arithmetic("leader-ready-one-heard-not-yet-joined-waits", "leader-ready",
               {"members": 3, "heard": 3, "counted": 2, "ahead": 0,
                "deadline_passed": False}, leader_ready(3, 3, 2, 0, False)),
    arithmetic("leader-ready-one-heard-not-joined-at-the-deadline", "leader-ready",
               {"members": 3, "heard": 3, "counted": 2, "ahead": 0,
                "deadline_passed": True}, leader_ready(3, 3, 2, 0, True)),
    arithmetic("leader-ready-every-member-heard-of-a-quorum", "leader-ready",
               {"members": 3, "heard": 2, "counted": 2, "ahead": 0,
                "deadline_passed": False}, leader_ready(3, 2, 2, 0, False)),
    arithmetic("leader-ready-alone-at-the-deadline-waits", "leader-ready",
               {"members": 3, "heard": 2, "counted": 1, "ahead": 0,
                "deadline_passed": True}, leader_ready(3, 2, 1, 0, True)),
    arithmetic("leader-ready-of-one", "leader-ready",
               {"members": 1, "heard": 1, "counted": 1, "ahead": 0,
                "deadline_passed": False}, leader_ready(1, 1, 1, 0, False)),
    arithmetic("leader-ready-five-four-heard-three-joined-waits", "leader-ready",
               {"members": 5, "heard": 4, "counted": 3, "ahead": 0,
                "deadline_passed": False}, leader_ready(5, 4, 3, 0, False)),
    arithmetic("leader-ready-five-four-heard-three-joined-at-the-deadline", "leader-ready",
               {"members": 5, "heard": 4, "counted": 3, "ahead": 0,
                "deadline_passed": True}, leader_ready(5, 4, 3, 0, True)),
    arithmetic("leader-ready-five-short-of-a-quorum-at-the-deadline", "leader-ready",
               {"members": 5, "heard": 4, "counted": 2, "ahead": 0,
                "deadline_passed": True}, leader_ready(5, 4, 2, 0, True)),
    arithmetic("leader-ready-a-member-ahead-never-closes", "leader-ready",
               {"members": 3, "heard": 2, "counted": 2, "ahead": 1,
                "deadline_passed": True}, leader_ready(3, 2, 2, 1, True)),
]

# ---- the file of a member's log's recordings ("The log's recordings") -----------------

LOG_RECORDINGS_MAGIC = b"CVLYLREC"
MAX_LOG_RECORDINGS = 1025


def log_recordings_bytes(member, live, live_start, entries, version=0,
                         magic=LOG_RECORDINGS_MAGIC, count=None):
    """The file's bytes: the header, then each entry as (recording id, start, stop)."""
    count = len(entries) if count is None else count
    out = magic + struct.pack("<iiqqi4x", version, member, live, live_start, count)
    for recording, start, stop in entries:
        out += struct.pack("<qqq", recording, start, stop)
    return out


def read_log_recordings(data, member):
    """The reference reader: the checks of "What a reader rejects", in their order."""
    if len(data) < 40:
        raise ValueError("recordings-too-short")
    if data[:8] != LOG_RECORDINGS_MAGIC:
        raise ValueError("recordings-magic")
    version, found, live, live_start, count = struct.unpack_from("<iiqqi", data, 8)
    if version != 0:
        raise ValueError("recordings-version")
    if found < 0 or found != member:
        raise ValueError("recordings-member")
    if count < 0 or count > MAX_LOG_RECORDINGS or len(data) != 40 + count * 24:
        raise ValueError("recordings-length")
    if live < 0 or live_start < 0 or live_start % 32:
        raise ValueError("recordings-live")
    entries = []
    before = 0
    for i in range(count):
        recording, start, stop = struct.unpack_from("<qqq", data, 40 + i * 24)
        if recording < 0 or start < before or start % 32 or stop % 32 or stop <= start:
            raise ValueError("recordings-entry")
        before = start
        entries.append({"recording_id": recording, "start_position": start,
                        "stop_position": stop})
    return {"member_id": found, "live_recording_id": live,
            "live_start_position": live_start, "entries": entries}


def read_from(fields, position):
    """The recording a position is read from: the first entry that holds it, else the
    live recording."""
    for entry in fields["entries"]:
        if entry["start_position"] <= position < entry["stop_position"]:
            return entry["recording_id"]
    return fields["live_recording_id"]


def log_recordings(name, data, member, positions=()):
    vector = {"kind": "log recordings", "name": name, "member": member, "bytes": data.hex()}
    try:
        fields = read_log_recordings(data, member)
        vector["fields"] = fields
        vector["reads"] = [{"position": p, "recording_id": read_from(fields, p)}
                           for p in positions]
    except ValueError as error:
        vector["error"] = str(error)
    return vector


VECTORS += [
    # A member on its first recording: nothing below it.
    log_recordings("log-recordings-none-below", log_recordings_bytes(1, 0, 0, []), 1,
                   [0, 96]),
    # A member that left the log at 1056 and joined it again: the recording it left,
    # then the live one.
    log_recordings("log-recordings-joined-again",
                   log_recordings_bytes(1, 3, 1056, [(0, 0, 1056)]), 1,
                   [0, 1024, 1056, 4096]),
    # A follower in a catch-up: a recording it stopped, its recording of the replay,
    # synced to 288, and the live recording, which begins inside the replay's at 192.
    # The positions the two share are read from the earlier one.
    log_recordings("log-recordings-catch-up-overlaps-the-live-recording",
                   log_recordings_bytes(2, 5, 192, [(2, 0, 96), (4, 96, 288)]), 2,
                   [0, 64, 96, 192, 256, 288, 320]),
    # A recording that lies inside the one before it: every position it holds is read
    # from the earlier one.
    log_recordings("log-recordings-one-inside-another",
                   log_recordings_bytes(0, 3, 320, [(1, 0, 320), (2, 96, 192)]), 0,
                   [0, 96, 160, 192, 320]),
    # Two entries that begin at one position: the first is read to its stop, then the
    # second.
    log_recordings("log-recordings-two-begin-together",
                   log_recordings_bytes(0, 6, 256, [(4, 0, 96), (5, 0, 256)]), 0,
                   [0, 64, 96, 224, 256]),
    # A table that forgot its oldest recordings: a position below the first entry is
    # the live recording's, whose replay the driver refuses.
    log_recordings("log-recordings-forgotten-below",
                   log_recordings_bytes(0, 9, 8192, [(7, 4096, 8192)]), 0,
                   [0, 4064, 4096, 8160, 8192]),
    # The rejections, in the order the checks run.
    log_recordings("log-recordings-too-short", log_recordings_bytes(1, 0, 0, [])[:39], 1),
    log_recordings("log-recordings-empty", b"", 1),
    log_recordings("log-recordings-magic",
                   log_recordings_bytes(1, 0, 0, [], magic=b"CVLYTERM"), 1),
    log_recordings("log-recordings-magic-before-version",
                   log_recordings_bytes(1, 0, 0, [], magic=b"\0" * 8, version=1), 1),
    log_recordings("log-recordings-version", log_recordings_bytes(1, 0, 0, [], version=1), 1),
    log_recordings("log-recordings-version-before-member",
                   log_recordings_bytes(2, 0, 0, [], version=-1), 1),
    log_recordings("log-recordings-member-of-another-name",
                   log_recordings_bytes(2, 0, 0, []), 1),
    log_recordings("log-recordings-member-below-zero", log_recordings_bytes(-1, 0, 0, []), -1),
    log_recordings("log-recordings-member-before-length",
                   log_recordings_bytes(2, 0, 0, [], count=7), 1),
    log_recordings("log-recordings-count-below-zero",
                   log_recordings_bytes(1, 0, 0, [], count=-1), 1),
    log_recordings("log-recordings-count-above-the-most",
                   log_recordings_bytes(1, 0, 0, [], count=1026), 1),
    log_recordings("log-recordings-fewer-entries-than-counted",
                   log_recordings_bytes(1, 3, 96, [(0, 0, 96)], count=2), 1),
    log_recordings("log-recordings-bytes-after-the-last-entry",
                   log_recordings_bytes(1, 3, 96, [(0, 0, 96)]) + b"\0", 1),
    log_recordings("log-recordings-length-before-live",
                   log_recordings_bytes(1, -1, 0, [], count=1), 1),
    log_recordings("log-recordings-live-id-below-zero", log_recordings_bytes(1, -1, 0, []), 1),
    log_recordings("log-recordings-live-start-below-zero",
                   log_recordings_bytes(1, 0, -32, []), 1),
    log_recordings("log-recordings-live-start-not-a-multiple-of-32",
                   log_recordings_bytes(1, 0, 48, []), 1),
    log_recordings("log-recordings-live-before-entry",
                   log_recordings_bytes(1, 0, 48, [(0, 0, 0)]), 1),
    log_recordings("log-recordings-entry-id-below-zero",
                   log_recordings_bytes(1, 3, 96, [(-1, 0, 96)]), 1),
    log_recordings("log-recordings-entry-start-below-zero",
                   log_recordings_bytes(1, 3, 96, [(0, -32, 96)]), 1),
    log_recordings("log-recordings-entry-start-not-a-multiple-of-32",
                   log_recordings_bytes(1, 3, 96, [(0, 16, 96)]), 1),
    log_recordings("log-recordings-entry-start-below-the-one-before",
                   log_recordings_bytes(1, 3, 192, [(0, 96, 192), (1, 64, 192)]), 1),
    log_recordings("log-recordings-entry-stop-not-a-multiple-of-32",
                   log_recordings_bytes(1, 3, 96, [(0, 0, 80)]), 1),
    log_recordings("log-recordings-entry-stop-at-its-start",
                   log_recordings_bytes(1, 3, 96, [(0, 64, 64)]), 1),
    log_recordings("log-recordings-entry-stop-below-its-start",
                   log_recordings_bytes(1, 3, 96, [(0, 64, 32)]), 1),
]

assert ballot(3, 3, [{"member": 0, "vote": True, "term": 3}, {"member": 1, "vote": True, "term": 3}], [0, 1, 2], True) == {"leader": True, "unanimous": False}
assert ballot(3, 3, [{"member": 0, "vote": True, "term": 3}, {"member": 1, "vote": True, "term": 3}], [0, 1], False) == {"leader": True, "unanimous": True}
assert ballot(3, 4, [{"member": 0, "vote": True, "term": 3}, {"member": 1, "vote": True, "term": 3}], [0, 1, 2], True) == {"leader": False, "unanimous": False}
assert at_least(3, 0, 2, 98304) and not at_least(2, 98304, 3, 0)
assert appointed_leader([7, 3, 5]) == {"leader": 3}
assert leader_ready(3, 3, 2, 0, False) == {"closes": False}
assert leader_ready(3, 3, 2, 0, True) == {"closes": True}
assert leader_ready(3, 2, 2, 1, True) == {"closes": False}
assert appointed_canvass(3, [(NULL, 0), (NULL, 0), (0, 4096)], (NULL, 0), True) == {"leads": False, "unanimous": False, "ahead": 1}
assert candidate(3, [(NULL, 0), (NULL, 0), (0, 4096)], (NULL, 0), True)["nominate"]
for v in VECTORS:
    if v["kind"] == "message" and "error" not in v:
        t = next(i for i, t in TEMPLATES.items() if t[0] == v["template"])
        for field, value in v["fields"].items():
            if field in MEMBER_IDS or field in POSITIONS:
                assert value >= (NULL if (t, field) in ALLOW_NULL else 0), (v["name"], field)
            if field in TERMS:
                assert value >= NULL, (v["name"], field)
            if (t, field) in ENUMS:
                assert value in ENUMS[(t, field)], (v["name"], field)
        assert t in STREAMS[v["stream"]], v["name"]


def main():
    out = Path(__file__).with_suffix(".json")
    names = [v["name"] for v in VECTORS]
    assert len(names) == len(set(names)), "vector names must be unique"
    doc = {
        "spec": "spec/cluster.md",
        "version": VERSION,
        "generated_by": "spec/vectors/cluster.py; do not edit by hand",
        "vectors": VECTORS,
    }
    out.write_text(json.dumps(doc, indent=2) + "\n")
    print(f"{out}: {len(VECTORS)} vectors")


if __name__ == "__main__":
    main()
