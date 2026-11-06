#!/usr/bin/env python3
"""Write spec/vectors/c-abi.json: the vectors for spec/c-abi.md, version 1.

This script is the page's reference, independent of the Rust code and of the committed
header: it lists, from the page's own tables, every opaque type, every struct with its
fields in order, the handler typedef, the macro, every function with its return type
and parameters, and every define with its value. A conforming header declares exactly
these, and `cavalry-ffi`'s conformance test holds `crates/cavalry-ffi/include/cavalry.h`
to them. Run it from the repository root after changing the page:

    python3 spec/vectors/c-abi.py

Unlike the other pages' vectors these are not bytes: an ABI is names, types and values,
and a reader checks a header against them (spec/README.md, "Conventions").
"""

import json
from pathlib import Path

ABI_VERSION = 1

OPAQUE_TYPES = [
    "cavalry_context_t", "cavalry_t", "cavalry_publication_t", "cavalry_subscription_t",
    "cavalry_image_t", "cavalry_recording_t", "cavalry_replay_t", "cavalry_header_t",
]

# Every struct the caller owns, its fields in order, C's natural layout, no packing.
STRUCTS = {
    "cavalry_buffer_claim_t": [
        ("uint8_t *", "data"), ("size_t", "length"), ("int64_t", "position_after"),
        ("cavalry_publication_t *", "publication"),
    ],
    "cavalry_header_values_frame_t": [  # spec/frame.md's fields, in order and type
        ("int32_t", "frame_length"), ("int8_t", "version"), ("uint8_t", "flags"),
        ("uint16_t", "type"), ("int32_t", "term_offset"), ("int32_t", "session_id"),
        ("int32_t", "stream_id"), ("int32_t", "term_id"), ("int64_t", "reserved_value"),
    ],
    "cavalry_header_values_t": [("cavalry_header_values_frame_t", "frame")],
    "cavalry_fault_t": [("int32_t", "code"), ("int32_t", "session_id"), ("int64_t", "position")],
    "cavalry_publication_constants_t": [
        ("const char *", "channel"), ("int64_t", "registration_id"), ("int32_t", "session_id"),
        ("int32_t", "stream_id"), ("size_t", "max_payload_length"),
        ("size_t", "max_message_length"), ("bool", "is_exclusive"),
    ],
    "cavalry_subscription_constants_t": [
        ("const char *", "channel"), ("int64_t", "registration_id"), ("int32_t", "stream_id"),
    ],
    "cavalry_image_constants_t": [
        ("const char *", "source_identity"), ("int64_t", "correlation_id"),
        ("size_t", "position_bits_to_shift"), ("int32_t", "session_id"),
        ("int32_t", "initial_term_id"),
    ],
    "cavalry_recording_constants_t": [
        ("const char *", "channel"), ("int64_t", "recording_id"), ("int32_t", "session_id"),
        ("int32_t", "stream_id"),
    ],
    "cavalry_replay_constants_t": [
        ("const char *", "channel"), ("int64_t", "replay_session_id"), ("int64_t", "recording_id"),
        ("int64_t", "start_position"), ("int32_t", "session_id"), ("int32_t", "stream_id"),
    ],
}

TYPEDEFS = {
    "cavalry_fragment_handler_t":
        "void (*)(void *clientd, const uint8_t *buffer, size_t length, const cavalry_header_t *header)",
}

# The one macro: the header's own version, passed by default (spec/c-abi.md, "Versions").
MACROS = {
    "cavalry_context_init": {
        "params": ["context"],
        "expands_to": "cavalry_context_init_v((context), CAVALRY_ABI_VERSION)",
    },
}


def fn(returns, name, *params):
    return {"name": name, "returns": returns, "params": [list(p) for p in params]}


CTX = ("const cavalry_context_t *", "context")
CLIENT = ("cavalry_t *", "client")
CCLIENT = ("const cavalry_t *", "client")
PUB = ("cavalry_publication_t *", "publication")
CPUB = ("const cavalry_publication_t *", "publication")
SUB = ("cavalry_subscription_t *", "subscription")
CSUB = ("const cavalry_subscription_t *", "subscription")
CLAIM = ("cavalry_buffer_claim_t *", "claim")
IMAGE = ("const cavalry_image_t *", "image")
REC = ("const cavalry_recording_t *", "recording")
REPLAY = ("const cavalry_replay_t *", "replay")

FUNCTIONS = [
    # context
    fn("int", "cavalry_context_init_v", ("cavalry_context_t **", "context"), ("int", "abi_version")),
    fn("int", "cavalry_context_set_dir", ("cavalry_context_t *", "context"), ("const char *", "dir")),
    fn("const char *", "cavalry_context_get_dir", CTX),
    fn("int", "cavalry_context_close", ("cavalry_context_t *", "context")),
    # client
    fn("int", "cavalry_init", ("cavalry_t **", "client"), ("cavalry_context_t *", "context")),
    fn("int", "cavalry_close", CLIENT),
    fn("bool", "cavalry_is_closed", CCLIENT),
    fn("int32_t", "cavalry_closed", CCLIENT),
    fn("int64_t", "cavalry_client_id", CCLIENT),
    fn("const char *", "cavalry_dir", CCLIENT),
    # adds and commands
    fn("int", "cavalry_add_publication", ("cavalry_publication_t **", "publication"), CLIENT,
       ("const char *", "channel"), ("int32_t", "stream_id")),
    fn("int", "cavalry_add_exclusive_publication", ("cavalry_publication_t **", "publication"),
       CLIENT, ("const char *", "channel"), ("int32_t", "stream_id")),
    fn("int", "cavalry_add_subscription", ("cavalry_subscription_t **", "subscription"), CLIENT,
       ("const char *", "channel"), ("int32_t", "stream_id")),
    fn("int", "cavalry_start_recording", ("cavalry_recording_t **", "recording"), CLIENT,
       ("const char *", "channel"), ("int32_t", "stream_id")),
    fn("int", "cavalry_stop_recording", CLIENT, ("int64_t", "recording_id")),
    fn("int", "cavalry_truncate_recording", CLIENT, ("int64_t", "recording_id"), ("int64_t", "position")),
    fn("int", "cavalry_purge_segments", CLIENT, ("int64_t", "recording_id"),
       ("int64_t", "new_start_position")),
    fn("int", "cavalry_purge_recording", CLIENT, ("int64_t", "recording_id")),
    fn("int", "cavalry_start_replay", ("cavalry_replay_t **", "replay"), CLIENT,
       ("int64_t", "recording_id"), ("int64_t", "position"), ("int64_t", "length"),
       ("int32_t", "replay_stream_id"), ("const char *", "channel")),
    fn("int", "cavalry_stop_replay", CLIENT, ("int64_t", "replay_session_id")),
    # Version 1: the destination commands (ADR 0057).
    fn("int", "cavalry_add_destination", CLIENT, CPUB, ("const char *", "channel")),
    fn("int", "cavalry_remove_destination", CLIENT, CPUB, ("const char *", "channel")),
    # publication
    fn("int64_t", "cavalry_publication_try_claim", PUB, ("size_t", "length"), CLAIM),
    fn("int", "cavalry_buffer_claim_set_reserved_value", CLAIM, ("int64_t", "value")),
    fn("int64_t", "cavalry_buffer_claim_commit", CLAIM),
    fn("int64_t", "cavalry_buffer_claim_abort", CLAIM),
    fn("int64_t", "cavalry_publication_offer", PUB, ("const uint8_t *", "buffer"), ("size_t", "length")),
    fn("int64_t", "cavalry_publication_position", CPUB),
    fn("int64_t", "cavalry_publication_position_limit", CPUB),
    fn("bool", "cavalry_publication_is_connected", CPUB),
    fn("bool", "cavalry_publication_is_closed", CPUB),
    fn("int", "cavalry_publication_constants", CPUB, ("cavalry_publication_constants_t *", "constants")),
    fn("int", "cavalry_publication_close", PUB),
    # subscription
    fn("int", "cavalry_subscription_poll", SUB, ("cavalry_fragment_handler_t", "handler"),
       ("void *", "clientd"), ("size_t", "fragment_limit")),
    fn("int", "cavalry_subscription_fault", CSUB, ("cavalry_fault_t *", "fault")),
    fn("int", "cavalry_subscription_image_count", CSUB),
    fn("cavalry_image_t *", "cavalry_subscription_image_at_index", SUB, ("size_t", "index")),
    fn("bool", "cavalry_subscription_is_connected", CSUB),
    fn("bool", "cavalry_subscription_is_closed", CSUB),
    fn("int", "cavalry_subscription_constants", CSUB, ("cavalry_subscription_constants_t *", "constants")),
    fn("int", "cavalry_subscription_close", SUB),
    # image and header
    fn("int", "cavalry_image_constants", IMAGE, ("cavalry_image_constants_t *", "constants")),
    fn("int64_t", "cavalry_image_position", IMAGE),
    fn("bool", "cavalry_image_is_end_of_stream", IMAGE),
    fn("int", "cavalry_header_values", ("const cavalry_header_t *", "header"),
       ("cavalry_header_values_t *", "values")),
    # recording and replay
    fn("int", "cavalry_recording_constants", REC, ("cavalry_recording_constants_t *", "constants")),
    fn("int64_t", "cavalry_recording_position", REC),
    fn("int64_t", "cavalry_recording_synced_position", REC),
    fn("int", "cavalry_recording_close", ("cavalry_recording_t *", "recording")),
    fn("int", "cavalry_replay_constants", REPLAY, ("cavalry_replay_constants_t *", "constants")),
    fn("int64_t", "cavalry_replay_position", REPLAY),
    fn("bool", "cavalry_replay_ended", REPLAY, ("int64_t *", "position"), ("int32_t *", "reason")),
    fn("int", "cavalry_replay_close", ("cavalry_replay_t *", "replay")),
    # errors and version
    fn("int", "cavalry_errcode"),
    fn("const char *", "cavalry_errmsg"),
    fn("int", "cavalry_version_major"),
    fn("int", "cavalry_version_minor"),
    fn("int", "cavalry_version_patch"),
    fn("const char *", "cavalry_version_full"),
    fn("int", "cavalry_abi_version"),
]

DEFINES = {
    "CAVALRY_ABI_VERSION": ABI_VERSION,
    "CAVALRY_DIR_ENV": "CAVALRY_DIR",
    "CAVALRY_MAX_IMAGES": 256,
    "CAVALRY_NULL_POSITION": -1,
    "CAVALRY_NULL_LENGTH": -1,
    "CAVALRY_FOLLOW_LENGTH": 9223372036854775807,  # INT64_MAX
    # the six result codes of a claim or an offer, Aeron's values
    "CAVALRY_PUBLICATION_NOT_CONNECTED": -1,
    "CAVALRY_PUBLICATION_BACK_PRESSURED": -2,
    "CAVALRY_PUBLICATION_ADMIN_ACTION": -3,
    "CAVALRY_PUBLICATION_CLOSED": -4,
    "CAVALRY_PUBLICATION_MAX_POSITION_EXCEEDED": -5,
    "CAVALRY_PUBLICATION_ERROR": -6,
    # the error codes: 0, the driver's 1 to 99 (spec/client-protocol.md, "Error codes",
    # the ones this version names), the client's from 100, the faults from 120, the
    # ABI's own from 130
    "CAVALRY_ERR_NONE": 0,
    "CAVALRY_ERR_INVALID_CHANNEL": 1,
    "CAVALRY_ERR_UNKNOWN_SUBSCRIPTION": 2,
    "CAVALRY_ERR_UNKNOWN_PUBLICATION": 3,
    "CAVALRY_ERR_CHANNEL_ENDPOINT_ERROR": 4,
    "CAVALRY_ERR_UNKNOWN_COMMAND_TYPE": 6,
    "CAVALRY_ERR_MALFORMED_COMMAND": 7,
    "CAVALRY_ERR_UNKNOWN_HOST": 9,
    "CAVALRY_ERR_RESOURCE_TEMPORARILY_UNAVAILABLE": 10,
    "CAVALRY_ERR_GENERIC_ERROR": 11,
    "CAVALRY_ERR_STORAGE_SPACE": 12,
    "CAVALRY_ERR_NO_PUBLICATION_TO_RECORD": 20,
    "CAVALRY_ERR_UNKNOWN_RECORDING": 21,
    "CAVALRY_ERR_ALREADY_RECORDING": 22,
    "CAVALRY_ERR_POSITION_OUT_OF_RANGE": 23,
    "CAVALRY_ERR_RECORDING_BUSY": 24,
    "CAVALRY_ERR_NOT_FOUND": 100,
    "CAVALRY_ERR_DRIVER_GONE": 101,
    "CAVALRY_ERR_CLOSED": 102,
    "CAVALRY_ERR_SUBSCRIPTIONS_FULL": 103,
    "CAVALRY_ERR_REPLAYS_FULL": 104,
    "CAVALRY_ERR_REPLAY_QUEUE_FULL": 105,
    "CAVALRY_ERR_RESPONSE_TIMEOUT": 106,
    "CAVALRY_ERR_COMMAND_RING": 107,
    "CAVALRY_ERR_BROADCAST": 108,
    "CAVALRY_ERR_COUNTERS": 109,
    "CAVALRY_ERR_PROTOCOL": 110,
    "CAVALRY_ERR_LOG": 111,
    "CAVALRY_ERR_DRIVER": 112,
    "CAVALRY_ERR_IO": 113,
    "CAVALRY_ERR_REFUSED": 114,
    "CAVALRY_ERR_FAULT_IMAGE": 120,
    "CAVALRY_ERR_FAULT_IMAGES_FULL": 121,
    "CAVALRY_ERR_FAULT_CLOSED": 122,
    "CAVALRY_ERR_TOO_LONG": 130,
    "CAVALRY_ERR_NULL": 131,
    "CAVALRY_ERR_CLAIM_OUTSTANDING": 132,
    "CAVALRY_ERR_NO_CLAIM": 133,
    "CAVALRY_ERR_POLLING": 134,
    "CAVALRY_ERR_ABI_VERSION": 135,
    "CAVALRY_ERR_NOT_UTF8": 136,
    "CAVALRY_ERR_EMPTY_DIR": 137,
    # why a client closed, `Closed`'s reasons in order
    "CAVALRY_CLOSED_BY_USER": 1,
    "CAVALRY_CLOSED_DRIVER_CLOSED": 2,
    "CAVALRY_CLOSED_DRIVER_TIMEOUT": 3,
    "CAVALRY_CLOSED_CLIENT_TIMEOUT": 4,
    "CAVALRY_CLOSED_LAPPED": 5,
    "CAVALRY_CLOSED_CORRUPT": 6,
    "CAVALRY_CLOSED_OVERRUN": 7,
    # why a replay ended, `ReplayStopReason` in order
    "CAVALRY_REPLAY_STOP_ENDED": 0,
    "CAVALRY_REPLAY_STOP_STOPPED": 1,
    "CAVALRY_REPLAY_STOP_NOT_CONNECTED": 2,
    "CAVALRY_REPLAY_STOP_FAILED": 3,
    # the header's flags and types (spec/frame.md)
    "CAVALRY_FLAGS_BEGIN": 0x80,
    "CAVALRY_FLAGS_END": 0x40,
    "CAVALRY_FLAGS_EOS": 0x20,
    "CAVALRY_FLAGS_UNFRAGMENTED": 0xC0,
    "CAVALRY_FRAME_TYPE_PAD": 0,
    "CAVALRY_FRAME_TYPE_DATA": 1,
}


def main():
    out = {
        "spec": "spec/c-abi.md",
        "version": ABI_VERSION,
        "generated_by": "spec/vectors/c-abi.py; do not edit by hand",
        "opaque_types": OPAQUE_TYPES,
        "structs": {name: [list(f) for f in fields] for name, fields in STRUCTS.items()},
        "typedefs": TYPEDEFS,
        "macros": MACROS,
        "functions": FUNCTIONS,
        "defines": DEFINES,
    }
    names = [f["name"] for f in FUNCTIONS]
    assert len(names) == len(set(names)), "a function is listed twice"
    path = Path(__file__).with_suffix(".json")
    path.write_text(json.dumps(out, indent=2) + "\n", encoding="utf-8")
    print(f"wrote {path}: {len(OPAQUE_TYPES)} opaque types, {len(STRUCTS)} structs, "
          f"{len(FUNCTIONS)} functions, {len(DEFINES)} defines")


if __name__ == "__main__":
    main()
