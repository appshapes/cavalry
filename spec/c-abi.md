# C ABI, version 2

The C ABI is how a program in any language but Rust reaches the client API: the
`extern "C"` functions, the opaque handles, the structs a caller owns, the codes, the
strings, and the rules for lifetimes and threads that `cavalry-ffi` exposes and
`crates/cavalry-ffi/include/cavalry.h` declares. The header is this page's artefact,
generated from the crate and committed; this page is the record, and
`spec/vectors/c-abi.json` holds the two together. The ABI is the client API as it
stands (`spec/client-protocol.md`, `spec/log-buffer.md`), every public item of
`cavalry-client` in a C form or named under "Out of this version", and nothing the
Rust API lacks: one Rust core serves every language, and a client in another language
is a thin wrapper over this header. It is on the data plane: a claim, a commit, an
offer and a poll cross it per message. In Aeron, this is the C client, `aeronc.h`,
which re-implements the Java one; here the names and the codes are Aeron's where the
concept matches, and the differences are on this page (ADR 0051; `docs/glossary.md`).
The ABI is the one surface frozen between releases: a change to anything on this page
is a new version, a new ADR and new vectors, before any code.

## Handles

Nine opaque types, each a pointer the library gives and takes back and the caller
never dereferences:

| Type | What it is | Made by | Freed by |
| --- | --- | --- | --- |
| `cavalry_context_t` | the directory a client will attach to | `cavalry_context_init` | `cavalry_context_close` |
| `cavalry_t` | a client: the attachment to one driver and its client conductor thread | `cavalry_init` | `cavalry_close` |
| `cavalry_publication_t` | a publication, concurrent or exclusive; `is_exclusive` in its constants says which | `cavalry_add_publication`, `cavalry_add_exclusive_publication` | `cavalry_publication_close` |
| `cavalry_subscription_t` | a subscription | `cavalry_add_subscription` | `cavalry_subscription_close` |
| `cavalry_image_t` | one of a subscription's images, part of the subscription | `cavalry_subscription_image_at_index` | its subscription |
| `cavalry_recording_t` | a recording's handle; the recording itself is stopped by `cavalry_stop_recording` | `cavalry_start_recording` | `cavalry_recording_close` |
| `cavalry_replay_t` | a replay's handle; the replay itself is stopped by `cavalry_stop_replay` | `cavalry_start_replay` | `cavalry_replay_close` |
| `cavalry_committed_log_t` | a service's read of the committed log of the cluster member on the client's driver: a replay of the member's recording of the log and the subscription it is played into (`spec/cluster.md`, "The service") | `cavalry_committed_log` | `cavalry_committed_log_close` |
| `cavalry_header_t` | the frame header a poll hands the fragment handler, valid for the call | `cavalry_subscription_poll`, `cavalry_committed_log_poll` | the call's return |

One publication type serves both kinds of publication, as the Rust API's one
`Publication` does; there is no `cavalry_exclusive_publication_*` family.

## Structs

The structs the caller owns. Every struct has C's natural layout, with no packing.

| Struct | Fields, in order | Filled by |
| --- | --- | --- |
| `cavalry_buffer_claim_t` | `uint8_t *data`, `size_t length`, `int64_t position_after`, `cavalry_publication_t *publication` | `cavalry_publication_try_claim` |
| `cavalry_header_values_frame_t` | `int32_t frame_length`, `int8_t version`, `uint8_t flags`, `uint16_t type`, `int32_t term_offset`, `int32_t session_id`, `int32_t stream_id`, `int32_t term_id`, `int64_t reserved_value` | as the `frame` of the next |
| `cavalry_header_values_t` | `cavalry_header_values_frame_t frame` | `cavalry_header_values` |
| `cavalry_fault_t` | `int32_t code`, `int32_t session_id`, `int64_t position` | `cavalry_subscription_fault`, `cavalry_committed_log_fault` |
| `cavalry_publication_constants_t` | `const char *channel`, `int64_t registration_id`, `int32_t session_id`, `int32_t stream_id`, `size_t max_payload_length`, `size_t max_message_length`, `bool is_exclusive` | `cavalry_publication_constants` |
| `cavalry_subscription_constants_t` | `const char *channel`, `int64_t registration_id`, `int32_t stream_id` | `cavalry_subscription_constants` |
| `cavalry_image_constants_t` | `const char *source_identity`, `int64_t correlation_id`, `size_t position_bits_to_shift`, `int32_t session_id`, `int32_t initial_term_id` | `cavalry_image_constants` |
| `cavalry_recording_constants_t` | `const char *channel`, `int64_t recording_id`, `int32_t session_id`, `int32_t stream_id` | `cavalry_recording_constants` |
| `cavalry_replay_constants_t` | `const char *channel`, `int64_t replay_session_id`, `int64_t recording_id`, `int64_t start_position`, `int32_t session_id`, `int32_t stream_id` | `cavalry_replay_constants` |
| `cavalry_committed_log_constants_t` | `int64_t recording_id`, `int32_t member_id` | `cavalry_committed_log_constants` |

- **The buffer claim.** `data` is the frame's body in the term, `length` the length
  claimed, `position_after` the position after the frame, which `try_claim` also
  returns, and `publication` the handle the claim belongs to. The claim itself lives
  inside the publication handle, one outstanding claim per publication, which is the
  Rust borrow rule made a convention; the struct names that claim, and a commit or an
  abort checks that it does ("Which claim a struct names"). A caller keeps one struct
  per publication, and a claim allocates nothing.
- **The header values.** `cavalry_header_values_frame_t` is the data frame header's
  nine fields in the order and the types of `spec/frame.md`, 32 bytes, and
  `cavalry_header_values_t` nests it as `frame`, so a program ported from Aeron reads
  `values.frame.session_id` unchanged. The outer struct has no `initial_term_id` and no
  `position_bits_to_shift`: the header the handler is given carries neither, and a
  ported line that reads them fails to compile rather than reading a wrong value. The
  image's constants carry the two instead ("A frame's position"). One field differs
  from Aeron's frame struct in type: `type` is `uint16_t`, as `spec/frame.md` types it,
  where Aeron's is `int16_t`; `version` is `int8_t` in both. Every frame type `spec/`
  gives is below 12, so `type` reads the same in either.
- **The fault.** The fault the subscription's last poll stopped on, or the committed
  log's. `code` is 120, 121
  or 122 ("Error codes"), the code `cavalry_errcode` gave after that poll, and 0 when
  the last poll returned a count. `session_id` is the image's for 120 and 121, else 0.
  `position` is the reader's position when the check failed for 120, else
  `CAVALRY_NULL_POSITION`. The check that failed is in the message. A poll that
  cannot answer leaves the fault as it was, and before any poll the fault is 0, 0 and
  `CAVALRY_NULL_POSITION`.
- **The constants.** Each is filled by its handle's `_constants` function from a copy
  the handle took when it was made, so the call cannot fail on the driver's state; the
  strings are valid for the handle's life.

## Functions

Each function returns 0 for success and −1 with the thread's error set, unless this
table gives another return type; a function that cannot answer returns the value
"When a function cannot answer" gives for its type. A function marked `const` on a
handle writes nothing through that pointer; it promises nothing about threads
("Threads").

| Group | Functions |
| --- | --- |
| context | `int cavalry_context_init_v(cavalry_context_t **context, int abi_version)`, which the header's macro `cavalry_context_init(context)` calls with `CAVALRY_ABI_VERSION` ("Versions and what a reader rejects"); `int cavalry_context_set_dir(cavalry_context_t *context, const char *dir)`; `const char *cavalry_context_get_dir(const cavalry_context_t *context)`; `int cavalry_context_close(cavalry_context_t *context)`. A context resolves its directory when it is made, as the Rust client's default does: `CAVALRY_DIR` when it is set and not empty, else the driver's default directory (`spec/client-protocol.md`, "The driver directory"). `set_dir` replaces it with the caller's bytes and refuses an empty string ("Strings"); `get_dir` returns the directory `cavalry_init` will use, never null for a context, valid until the next `set_dir` or the context's close. |
| client | `int cavalry_init(cavalry_t **client, cavalry_context_t *context)`: attaches to the driver at the context's directory and starts the client conductor, in one call, and copies the directory, so the client keeps no pointer to its context; `int cavalry_close(cavalry_t *client)`; `bool cavalry_is_closed(const cavalry_t *client)`; `int32_t cavalry_closed(const cavalry_t *client)`: 0 while open, else why, `CAVALRY_CLOSED_*` ("Constants"); `int64_t cavalry_client_id(const cavalry_t *client)`; `const char *cavalry_dir(const cavalry_t *client)`. |
| adds and commands | `int cavalry_add_publication(cavalry_publication_t **publication, cavalry_t *client, const char *channel, int32_t stream_id)`; `int cavalry_add_exclusive_publication(cavalry_publication_t **publication, cavalry_t *client, const char *channel, int32_t stream_id)`; `int cavalry_add_subscription(cavalry_subscription_t **subscription, cavalry_t *client, const char *channel, int32_t stream_id)`; `int cavalry_start_recording(cavalry_recording_t **recording, cavalry_t *client, const char *channel, int32_t stream_id)`; `int cavalry_stop_recording(cavalry_t *client, int64_t recording_id)`; `int cavalry_truncate_recording(cavalry_t *client, int64_t recording_id, int64_t position)`; `int cavalry_purge_segments(cavalry_t *client, int64_t recording_id, int64_t new_start_position)`; `int cavalry_purge_recording(cavalry_t *client, int64_t recording_id)`; `int cavalry_start_replay(cavalry_replay_t **replay, cavalry_t *client, int64_t recording_id, int64_t position, int64_t length, int32_t replay_stream_id, const char *channel)`; `int cavalry_stop_replay(cavalry_t *client, int64_t replay_session_id)`; `int cavalry_add_destination(cavalry_t *client, const cavalry_publication_t *publication, const char *channel)` and `int cavalry_remove_destination(cavalry_t *client, const cavalry_publication_t *publication, const char *channel)`: the client protocol's `add destination` and `remove destination` for the publication, whose registration id the function reads from the handle's copied constants, so that the call is allowed during a claim; 0 on `operation success`, −1 with the driver's code on its refusal (`spec/client-protocol.md`, "Commands"; ADR 0057). Each is synchronous: it returns when the driver has answered or the response timeout, 10 s, has passed, with one operation in flight per client, as the client protocol has it. The driver answers once it has queued the change for its sender, which takes its requests in order, at most the sender requests per cycle at the start of each duty cycle and none while its replies wait for room, so the change takes effect at the start of the cycle that takes it, the next cycle only when nothing waits ahead of it, and until then everything the endpoint sends still reaches a removed destination and nothing reaches an added one (`spec/client-protocol.md`, "Commands"; `spec/udp.md`, "Values"). The driver's refusal is the error, with the driver's code and message. |
| publication | `int64_t cavalry_publication_try_claim(cavalry_publication_t *publication, size_t length, cavalry_buffer_claim_t *claim)`: the position after the frame, with the struct filled, or a result code; `int cavalry_buffer_claim_set_reserved_value(cavalry_buffer_claim_t *claim, int64_t value)`; `int64_t cavalry_buffer_claim_commit(cavalry_buffer_claim_t *claim)` and `int64_t cavalry_buffer_claim_abort(cavalry_buffer_claim_t *claim)`: the position after the frame, or −1; `int64_t cavalry_publication_offer(cavalry_publication_t *publication, const uint8_t *buffer, size_t length)`: the position after the message, or a result code; `int64_t cavalry_publication_position(const cavalry_publication_t *publication)`: the producer position, where the next frame goes, as the Rust `Publication::position` gives it (`spec/log-buffer.md`, "Tail counters"); in a concurrent publication it counts frames that other handles on the log have claimed and not yet committed; or −1 with the thread's error set, `CAVALRY_ERR_LOG` when the log's active tail is not its active term's, the one check of `spec/log-buffer.md` this read runs; `int64_t cavalry_publication_position_limit(const cavalry_publication_t *publication)`; `bool cavalry_publication_is_connected(const cavalry_publication_t *publication)`; `bool cavalry_publication_is_closed(const cavalry_publication_t *publication)`; `int cavalry_publication_constants(const cavalry_publication_t *publication, cavalry_publication_constants_t *constants)`; `int cavalry_publication_close(cavalry_publication_t *publication)`. |
| subscription | `int cavalry_subscription_poll(cavalry_subscription_t *subscription, cavalry_fragment_handler_t handler, void *clientd, size_t fragment_limit)`: the fragments delivered, or −1 with the fault as the error; a `fragment_limit` above `INT_MAX` is read as `INT_MAX`, so that the count fits the `int`; `int cavalry_subscription_fault(const cavalry_subscription_t *subscription, cavalry_fault_t *fault)`; `int cavalry_subscription_image_count(const cavalry_subscription_t *subscription)`; `cavalry_image_t *cavalry_subscription_image_at_index(cavalry_subscription_t *subscription, size_t index)`: null past the count, with no error set; `bool cavalry_subscription_is_connected(const cavalry_subscription_t *subscription)`; `bool cavalry_subscription_is_closed(const cavalry_subscription_t *subscription)`; `int cavalry_subscription_constants(const cavalry_subscription_t *subscription, cavalry_subscription_constants_t *constants)`; `int cavalry_subscription_close(cavalry_subscription_t *subscription)`. |
| image and header | `int cavalry_image_constants(const cavalry_image_t *image, cavalry_image_constants_t *constants)`; `int64_t cavalry_image_position(const cavalry_image_t *image)`; `bool cavalry_image_is_end_of_stream(const cavalry_image_t *image)`; `int cavalry_header_values(const cavalry_header_t *header, cavalry_header_values_t *values)`. This version has no `cavalry_header_position`: a frame's position is computed from its image's constants ("A frame's position"). |
| recording and replay | `int cavalry_recording_constants(const cavalry_recording_t *recording, cavalry_recording_constants_t *constants)`; `int64_t cavalry_recording_position(const cavalry_recording_t *recording)`; `int64_t cavalry_recording_synced_position(const cavalry_recording_t *recording)`; `int cavalry_recording_close(cavalry_recording_t *recording)`, which frees the handle and stops nothing; `int cavalry_replay_constants(const cavalry_replay_t *replay, cavalry_replay_constants_t *constants)`; `int64_t cavalry_replay_position(const cavalry_replay_t *replay)`; `bool cavalry_replay_ended(const cavalry_replay_t *replay, int64_t *position, int32_t *reason)`: `true` once the replay has ended, with the end position and the reason, `CAVALRY_REPLAY_STOP_*` or −1 for none, written through the pointers; `int cavalry_replay_close(cavalry_replay_t *replay)`, which frees the handle. |
| committed log | `int cavalry_committed_log(cavalry_committed_log_t **log, cavalry_t *client, int64_t position)`: opens the committed log of the cluster member on the client's driver at `position`, a position at which an entry of the log begins or the log's start, 0, as the Rust `Cavalry::committed_log` does (`spec/cluster.md`, "The service"): it adds the subscription on the service log stream and starts the replay of the member's recording of the log that follows it. 0 with `*log` the handle; 0 with `*log` null while the driver hosts no member that has a recording of its log, which is an answer and not a failure, so no error is set and the caller asks again later; −1 with the driver's refusal or the client's error as any add gives it, and `*log` null. `int cavalry_committed_log_poll(cavalry_committed_log_t *log, cavalry_fragment_handler_t handler, void *clientd, size_t fragment_limit)`: hands the handler up to `fragment_limit` entries that begin below the member's commit position, each as its bytes in `spec/cluster.md`'s encoding and its frame's header, and returns the entries delivered, or −1 with the fault as the error; a `fragment_limit` above `INT_MAX` is read as `INT_MAX`. No entry at or past the commit position is delivered, whatever the member's recording holds. `int64_t cavalry_committed_log_entry_position(const cavalry_header_t *header)`: the position in the log at which the entry begins whose header the handler of `cavalry_committed_log_poll` was handed, for a call from inside that handler; −1 with `CAVALRY_ERR_NO_POSITION` for a header that `cavalry_subscription_poll` handed its handler, which carries no position ("A frame's position"). `int cavalry_committed_log_fault(const cavalry_committed_log_t *log, cavalry_fault_t *fault)`: the fault its last poll stopped on, as `cavalry_subscription_fault`. `int64_t cavalry_committed_log_position(const cavalry_committed_log_t *log)`: the position after the last entry delivered, or the position the log was opened at before the first: where a service that has applied every entry it was given opens the log again. `int64_t cavalry_committed_log_commit_position(const cavalry_committed_log_t *log)`: the bound of the last poll, the highest commit position taken from the member's counter, 0 before the first poll; it never falls. `bool cavalry_committed_log_is_ended(const cavalry_committed_log_t *log)`: `true` once the handle has come to its end: the member no longer names the recording it replays, the member's counters are gone, the recording stopped and the replay ended with it, or the replay's image failed a check. An ended handle delivers what is left below its bound and then nothing: once a poll returns 0, the service closes the handle and opens the log again from its position. `bool cavalry_committed_log_failed_at(const cavalry_committed_log_t *log, int64_t *position)`: `true` when the driver's replay of the member's recording failed, with the position after the last frame it played written through the pointer; `false`, with nothing written, for a log that is live or that ended because its member closed or left the recording. A log opened at a position where no entry begins fails so at once, at that position, with nothing delivered, since the bytes there are no frame. `int cavalry_committed_log_constants(const cavalry_committed_log_t *log, cavalry_committed_log_constants_t *constants)`: the id of the member whose log this is and the id of the member's recording that the handle replays. `int cavalry_committed_log_close(cavalry_committed_log_t *log, cavalry_t *client)`: stops the replay if it has not ended, closes the subscription and frees the handle; a replay the driver no longer knows is 0, since it has ended. A null `client` is allowed and is the Rust handle's drop: the subscription is closed, the handle is freed and the function returns 0, with the replay left to the driver, which ends it once the replay connect timeout has passed with no subscriber; that is how a log is closed after its client's `cavalry_close`, which frees the client handle. The handle is freed whatever the function returns: −1 with the client's error when the stop was not answered, by a client the driver or a timeout closed or within the response timeout, leaves the replay to the driver in the same way. |
| errors and version | `int cavalry_errcode(void)` and `const char *cavalry_errmsg(void)`: the calling thread's last error ("Error codes", "The message"); `int cavalry_version_major(void)`, `int cavalry_version_minor(void)`, `int cavalry_version_patch(void)` and `const char *cavalry_version_full(void)`: the crate's version; `int cavalry_abi_version(void)`: this page's version, 2, beside the header's `CAVALRY_ABI_VERSION`. |

The fragment handler is `typedef void (*cavalry_fragment_handler_t)(void *clientd,
const uint8_t *buffer, size_t length, const cavalry_header_t *header)`: the body as a
view into the log and the frame's header, both valid for the call, as the Rust
`Subscription::poll` hands them. Nothing is copied, and the library makes nothing new
for the call. The handler runs on the polling thread, inside
`cavalry_subscription_poll`. `cavalry_committed_log_poll` takes the same handler, so
that a client in any language reads a committed log through the trampoline it reads a
subscription through; the Rust `CommittedLog::poll` hands its handler the entry's
position as a third argument, and here the handler asks for it with
`cavalry_committed_log_entry_position(header)`.

## Result codes

`cavalry_publication_try_claim` and `cavalry_publication_offer` return the position
after the frame or the message, or one of six negative values, Aeron's:

| Define | Value | When |
| --- | --- | --- |
| `CAVALRY_PUBLICATION_NOT_CONNECTED` | −1 | no subscriber has a position on the stream and no recording is live, so the message would have nobody to reach |
| `CAVALRY_PUBLICATION_BACK_PRESSURED` | −2 | the slowest subscriber is a publication window behind (`spec/log-buffer.md`, "The driver") |
| `CAVALRY_PUBLICATION_ADMIN_ACTION` | −3 | the term rotated under the claim: try again at once |
| `CAVALRY_PUBLICATION_CLOSED` | −4 | the client this publication belongs to is closed; `cavalry_closed` says why |
| `CAVALRY_PUBLICATION_MAX_POSITION_EXCEEDED` | −5 | the stream reached the largest position its term length allows |
| `CAVALRY_PUBLICATION_ERROR` | −6 | the thread's error is set: a message longer than the publication allows, `CAVALRY_ERR_TOO_LONG`, with the length asked for and the limit in the message, as Aeron's C client answers a length over its limit with `ERROR` and `EINVAL`; or a call that cannot answer |

The five are the Rust `Offer`'s five Aeron variants on Aeron's values, and `ERROR` is
the one place `Offer::TooLong` reaches a caller; there is no seventh code.

## Error codes

`cavalry_errcode` is the calling thread's last error, one of the values the header
defines as `CAVALRY_ERR_*`. A success clears nothing: the error is the last failure's
until the next. With no failure yet on the thread, the code is 0 and the message the
empty string.

| Range | Values | What they are |
| --- | --- | --- |
| none | 0 `CAVALRY_ERR_NONE` | no failure yet |
| the driver's refusal | 1 to 99 | the client protocol's error code, on its value (`spec/client-protocol.md`, "Error codes"), with the driver's message. The header names the codes this version has: 1 `INVALID_CHANNEL`, 2 `UNKNOWN_SUBSCRIPTION`, 3 `UNKNOWN_PUBLICATION`, 4 `CHANNEL_ENDPOINT_ERROR`, 6 `UNKNOWN_COMMAND_TYPE`, 7 `MALFORMED_COMMAND`, 9 `UNKNOWN_HOST`, 10 `RESOURCE_TEMPORARILY_UNAVAILABLE`, 11 `GENERIC_ERROR`, 12 `STORAGE_SPACE`, 20 `NO_PUBLICATION_TO_RECORD`, 21 `UNKNOWN_RECORDING`, 22 `ALREADY_RECORDING`, 23 `POSITION_OUT_OF_RANGE`, 24 `RECORDING_BUSY`. A code from 1 to 99 passes through on its value whether this version names it or not, so that a newer driver's code reaches the caller during an upgrade. The client protocol keeps its codes below 100 for this |
| the client's own | 100 `NOT_FOUND` (no driver at the directory), 101 `DRIVER_GONE`, 102 `CLOSED`, 103 `SUBSCRIPTIONS_FULL`, 104 `REPLAYS_FULL`, 105 `REPLAY_QUEUE_FULL`, 106 `RESPONSE_TIMEOUT`, 107 `COMMAND_RING`, 108 `BROADCAST`, 109 `COUNTERS`, 110 `PROTOCOL`, 111 `LOG`, 112 `DRIVER`, 113 `IO`, 114 `REFUSED` | the Rust `ClientError`'s other kinds in order, with its message, then 114, a refusal whose code is outside 1 to 99, 0, negative or 100 and above, with the raw code in the message, so that it never reads as one of the codes below |
| the faults a poll stops on | 120 `FAULT_IMAGE`, 121 `FAULT_IMAGES_FULL`, 122 `FAULT_CLOSED` | the Rust `Fault`'s three; `cavalry_subscription_fault` gives their fields |
| the ABI's own | 130 `TOO_LONG`, 131 `NULL`, 132 `CLAIM_OUTSTANDING`, 133 `NO_CLAIM`, 134 `POLLING`, 135 `ABI_VERSION`, 136 `NOT_UTF8`, 137 `EMPTY_DIR`, 138 `NO_POSITION` | what the caller did, what to do next, and this page's anchor for the rule, in the message |

### The message

`cavalry_errmsg` is the error's message, UTF-8 and NUL-terminated, in a thread-local
buffer of 512 bytes: at most 511 bytes of message, then the NUL, always inside the
buffer; valid until the next failing call on the thread. A message of 511 bytes or
fewer is written whole. A longer one keeps its docs anchor and loses its middle: it is
written as its head, the three bytes `...`, and its tail, where the tail is the message
from its last `; see `, which every Rust message puts before its anchor, or, when that
part is longer than 128 bytes or the message has no `; see `, the message's last 128
bytes moved forward to the start of a UTF-8 sequence, and the head is as many of the
message's first bytes as fit before `...` and the tail, moved back to the start of a
UTF-8 sequence. The buffer is valid UTF-8 whatever the message held. The writer
allocates nothing.

## Constants

The header defines the Rust API's public constants and the values its enums carry
across the boundary:

| Define | Value | Meaning |
| --- | --- | --- |
| `CAVALRY_ABI_VERSION` | 2 | this page's version |
| `CAVALRY_DIR_ENV` | `"CAVALRY_DIR"` | the environment variable a context's default directory reads |
| `CAVALRY_MAX_IMAGES` | 256 | the images a subscription holds at most |
| `CAVALRY_SERVICE_LOG_STREAM_ID` | 104 | the `cavalry:ipc` stream a committed log's replay is played into (`spec/cluster.md`, "Members and streams"), which a program beside a service leaves to it |
| `CAVALRY_NULL_POSITION` | −1 | no position: a replay from the recording's start, a fault without one |
| `CAVALRY_NULL_LENGTH` | −1 | a replay to the recording's extent as it stands |
| `CAVALRY_FOLLOW_LENGTH` | `INT64_MAX` | a replay that follows a live recording |
| `CAVALRY_FLAGS_BEGIN`, `CAVALRY_FLAGS_END`, `CAVALRY_FLAGS_EOS`, `CAVALRY_FLAGS_UNFRAGMENTED` | `0x80`, `0x40`, `0x20`, `0xC0` | the header's flags (`spec/frame.md`) |
| `CAVALRY_FRAME_TYPE_PAD`, `CAVALRY_FRAME_TYPE_DATA` | 0, 1 | the header's types |
| `CAVALRY_CLOSED_BY_USER`, `_DRIVER_CLOSED`, `_DRIVER_TIMEOUT`, `_CLIENT_TIMEOUT`, `_LAPPED`, `_CORRUPT`, `_OVERRUN` | 1 to 7 | why a client closed, `cavalry_closed`'s answer, the Rust `Closed`'s reasons in order |
| `CAVALRY_REPLAY_STOP_ENDED`, `_STOPPED`, `_NOT_CONNECTED`, `_FAILED` | 0 to 3 | why a replay ended, `cavalry_replay_ended`'s reason, `ReplayStopReason` in order |

The Rust `BufferClaim::body` is the claim struct's `data` and `length`, its
`position_after` the struct's; `Recording::stop` and `Replay::stop` are
`cavalry_stop_recording` and `cavalry_stop_replay` with the handle's id.

## Strings

Every `const char *` the ABI takes is NUL-terminated and read during the call; the
library copies what it keeps. A `channel`, which every add, `cavalry_start_recording`
and `cavalry_start_replay` take, must be UTF-8, since it is a `&str` in the Rust API: a
channel that is not is −1 with `CAVALRY_ERR_NOT_UTF8`, nothing is sent to the driver,
and the message names the function and the parameter; the library does not replace
the bad bytes, because the driver would then be asked for a channel the caller did not
write. A directory is a path: on Unix `cavalry_context_set_dir` takes the bytes as they
are, and `cavalry_context_get_dir` and `cavalry_dir` return them unchanged; on a
platform whose paths are not bytes, a directory that is not UTF-8 is refused as a
channel is. A message shows a path with U+FFFD for each bad sequence, so it stays
UTF-8. An empty directory is refused: `cavalry_context_set_dir(context, "")` is −1
with `CAVALRY_ERR_EMPTY_DIR` and the context keeps the directory it had; a caller who
means the working directory passes `"."`, and one who wants the default does not call
`set_dir`.

## When a function cannot answer

A function cannot answer when it is given a null handle or a null required pointer,
which is `CAVALRY_ERR_NULL`, except a claim struct's null `publication`, which is
`CAVALRY_ERR_NO_CLAIM` ("Which claim a struct names"), and the `client` of
`cavalry_committed_log_close`, which is not required ("Functions"); when "Strings" refuses a string
argument; or when "Calls during a claim or a poll" refuses the call. One read fails on
the log's state as well: `cavalry_publication_position` is −1 with `CAVALRY_ERR_LOG`
when the log's active tail is not its active term's ("Functions"). That is a concurrent
publication's read racing a rotation, which `spec/log-buffer.md` has a publisher retry,
so the caller may read again; an exclusive publication reads its own tail and does not
fail this way. A function that cannot answer sets the thread's error and returns the
value its return type gives:

| Return type | Functions | Value when the function cannot answer |
| --- | --- | --- |
| `int` status | every function the function table gives no other type | −1 |
| `int64_t` position | `cavalry_publication_try_claim`, `cavalry_publication_offer` | `CAVALRY_PUBLICATION_ERROR`, −6 |
| `int64_t` | every other: commit, abort, the positions, the limit, `cavalry_client_id`, the committed log's three | −1; every position, limit and id the ABI returns is 0 or more, so −1 is never an answer |
| `int32_t` or `int` count | `cavalry_closed`, `cavalry_subscription_poll`, `cavalry_subscription_image_count`, `cavalry_committed_log_poll` | −1 |
| pointer | `cavalry_context_get_dir`, `cavalry_dir`, `cavalry_subscription_image_at_index` | null |
| `bool` | the `_is_connected`, `_is_closed`, `_is_end_of_stream` and `_is_ended` functions, and `cavalry_replay_ended` and `cavalry_committed_log_failed_at`, which then write nothing through their pointers | `false` |

A `bool` function fails only on a misuse, a null pointer or a call the tables below
refuse, never on the driver's state, so a correct program reads every `false` as the
answer. `cavalry_subscription_image_at_index` past the count is null with no error set:
"no such image" is the answer, not a misuse. `cavalry_committed_log` on a driver that
hosts no member with a recording of its log is 0 with a null `*log` and no error set,
for the same reason: "no log to read yet" is the answer, and the caller asks again.
`cavalry_committed_log_entry_position` fails on one thing more than a null header: a
header that carries no position, which is `CAVALRY_ERR_NO_POSITION`. The functions that take no handle,
`cavalry_errcode`, `cavalry_errmsg`, the version functions and `cavalry_abi_version`,
cannot fail.

## Calls during a claim or a poll

A live claim holds its publication until its commit or abort, and a poll holds its
subscription for the whole run of the handler; in Rust the borrow checker refuses every
other call on either, and here the convention replaces it. A refusal leaves the claim or
the poll as it was.

| Call | While a claim is outstanding on the publication |
| --- | --- |
| `cavalry_buffer_claim_set_reserved_value`, `_commit`, `_abort` | the claim's own functions, as the function table and "Which claim a struct names" say |
| `cavalry_publication_constants` | 0, from the handle's copy |
| `cavalry_publication_try_claim`, `cavalry_publication_offer` | `CAVALRY_PUBLICATION_ERROR` with `CAVALRY_ERR_CLAIM_OUTSTANDING`; Aeron's C client lets a concurrent publication offer beside a claim, and this ABI keeps the Rust rule |
| `cavalry_publication_position`, `_position_limit` | −1 with `CAVALRY_ERR_CLAIM_OUTSTANDING` |
| `cavalry_publication_is_connected`, `_is_closed` | `false` with `CAVALRY_ERR_CLAIM_OUTSTANDING` |
| `cavalry_publication_close` | aborts the claim, then closes |
| `cavalry_add_destination`, `cavalry_remove_destination` with this publication | allowed: each reads the handle's copied registration id, which the claim does not borrow, and runs on the client (ADR 0057) |

| Call from inside the handler | While the subscription's poll runs |
| --- | --- |
| `cavalry_header_values` on the handler's `header` | the function's own answer |
| `cavalry_subscription_constants` | 0, from the handle's copy |
| `cavalry_subscription_poll`, `_fault`, `_image_count`, `_close` | −1 with `CAVALRY_ERR_POLLING`; a refused close leaves the subscription open |
| `cavalry_subscription_image_at_index` | null with `CAVALRY_ERR_POLLING` |
| `cavalry_subscription_is_connected`, `_is_closed` | `false` with `CAVALRY_ERR_POLLING` |
| `cavalry_image_*` on an image of this subscription | undefined: the image pointer was valid only until this poll began ("Lifetimes") |
| any call on another handle | allowed, by the thread rules: a handler may offer on a publication, or poll another subscription |

| Call from inside the handler | While the committed log's poll runs |
| --- | --- |
| `cavalry_header_values` and `cavalry_committed_log_entry_position` on the handler's `header` | the function's own answer |
| `cavalry_committed_log_constants` | 0, from the handle's copy |
| `cavalry_committed_log_poll`, `_fault` | −1 with `CAVALRY_ERR_POLLING` |
| `cavalry_committed_log_position`, `_commit_position` | −1 with `CAVALRY_ERR_POLLING`: the poll moves both when it returns, and the handler has each entry's own position |
| `cavalry_committed_log_is_ended`, `_failed_at` | `false` with `CAVALRY_ERR_POLLING` |
| `cavalry_committed_log_close` | −1 with `CAVALRY_ERR_POLLING`; a refused close frees nothing and leaves the log open, the one case in which the handle outlives its close |
| any call on another handle | allowed, by the thread rules: a handler may offer on a publication, the service's answer for one |

## Which claim a struct names

`cavalry_buffer_claim_commit`, `_abort` and `_set_reserved_value` act on the claim
outstanding on `claim->publication` only when the struct names it: its `publication`
is not null, a claim is outstanding on that publication, and the struct's
`position_after` is that claim's. Any other struct is −1 with `CAVALRY_ERR_NO_CLAIM`,
and the outstanding claim, if there is one, is untouched. So a struct kept after its
commit cannot commit or abort the next claim on its publication: claim c1, commit c1,
claim c2, then commit c1's struct again answers `NO_CLAIM`, c2 stays outstanding until
its own struct commits it, and no subscriber reads a frame whose body the publisher
had not written. The check compares the position and not the body's address, since a
position names one place in the stream while the log reuses each of its three
partitions every three terms. A null `publication` answers `NO_CLAIM` too, not
`CAVALRY_ERR_NULL`, so that a released struct answers the same in a release build,
which leaves it as its commit found it, and in a debug build, which poisons it
("Lifetimes"). A refused `try_claim`, a result code of −1 to −6 with a struct that is
not null, writes a null `data`, a length of 0, a `position_after` of
`CAVALRY_NULL_POSITION` and a null `publication`, so that a caller who misses the code
holds no pointer into the log; the one exception is a struct that names the claim
outstanding on the publication, which the refusal with `CAVALRY_ERR_CLAIM_OUTSTANDING`
leaves as it was, so the caller can still commit or abort that claim.

## Lifetimes

By convention and not by the borrow checker:

- Every handle but an image and a frame header is freed by its own close and by
  nothing else; "Handles" says what frees those two. A committed log's close takes
  the client as well, to stop the replay, or a null client to leave the replay to the
  driver, and frees the handle whatever it returns unless the call was refused from
  inside the log's own poll. A handle closed twice, or used
  after its close, is undefined. `cavalry_close` before a publication's or a
  subscription's close is allowed: the handle answers `CLOSED` or the `closed` fault
  from then and still needs its own close, as the Rust handles outlive their client.
  A committed log is the same: its poll answers the `closed` fault, and it still
  needs its own close, with a null `client`, since `cavalry_close` freed the client
  handle. A program that closes its logs before its client stops each replay at
  once.
  `cavalry_context_close` before or after the `cavalry_close` of a client the context
  made is allowed too, since the client copied the directory; a program ported from
  Aeron, which closes the context last, is correct here as well. One context may make
  any number of clients, each `cavalry_init` reading the directory at its call.
- A claim's `data` is valid from `try_claim` until its commit or abort, and a claim is
  committed or aborted once; a claim neither committed nor aborted before the
  publication's close is aborted by the close. The image pointer
  `cavalry_subscription_image_at_index` returns is valid until the subscription's next
  poll or close, and so is the `source_identity` string its constants carry, since the
  image is part of its subscription; the constants' numeric fields are the caller's
  copy and may be kept across polls. The handler's `buffer` and `header` are valid for
  the call. Every other constants struct's strings are valid for the handle's life.
  `cavalry_errmsg`'s string
  is valid until the next failing call on the thread. `cavalry_context_get_dir`'s is
  valid until the next `set_dir` or the context's close.
- Debug builds poison what is released, so misuse is loud: a committed or aborted
  claim's struct is overwritten with a null `data`, a length of 0, a `position_after`
  of `CAVALRY_NULL_POSITION` and a null `publication`; a closed handle's memory is
  filled with `0xDD` before it is freed.

## Threads

Every handle is used from one thread at a time: a call on a handle returns before the
next call on the same handle begins, whichever thread makes it. The rule covers every
function of the handle, as the Rust borrow does, which lets no `&self` call run beside
a `&mut self` one:

- A client's reads, `cavalry_is_closed`, `cavalry_closed`, `cavalry_client_id` and
  `cavalry_dir`, never run beside an add or a command, which waits up to 10 s, nor
  beside `cavalry_close`; a program that watches a client from a second thread holds
  its own lock around every call on the client handle.
- `cavalry_init` reads the context, so it counts as a call on the context as well as on
  the new client.
- An image is part of its subscription, so a call on an image counts as a call on its
  subscription.
- `cavalry_add_destination` and `cavalry_remove_destination` count as calls on the
  client and not on the publication they name: each reads only the registration id
  the publication handle copied when it was made, which no function of the
  publication writes. So either may run in one thread beside any call on the
  publication in another, an offer or a claim among them, but not beside the
  publication's close, which frees the handle ("Lifetimes"; ADR 0057).
- `cavalry_committed_log`, and `cavalry_committed_log_close` with a client, count as
  calls on the client as well as on the log: each sends commands and waits for their
  answers.
  Every other function of a committed log is a call on the log alone, so a service
  polls its log in one thread while another thread uses the client.
- Two handles may be used from two threads at once, a client and its publications,
  subscriptions, recordings, replays and committed logs among them.

Any handle may move between threads between calls. The client conductor is the
library's own thread, started by `cavalry_init` and joined by `cavalry_close`. No
`extern "C"` function of the library unwinds, and a `longjmp` out of the handler is
undefined.

## A frame's position

The image's `initial_term_id` and `position_bits_to_shift`, the base-2 logarithm of
its term length, are the Rust `Image::positions()` in C, under the names Aeron's own
`aeron_image_constants_t` gives them. A frame's position is its term count shifted
left by `position_bits_to_shift`, plus its `term_offset`, with the term count computed
in 32-bit two's complement as `spec/log-buffer.md`, "Positions", says: in C,
`(int32_t)((uint32_t)term_id - (uint32_t)initial_term_id)`, so that the subtraction is
not a signed overflow. The handler finds a frame's image by the header's `session_id`.
A caller that needs positions takes each image's constants after a poll with a
fragment limit of 0, which joins the images the driver announced and reads no frame,
and keeps the struct's numeric fields, `session_id`, `initial_term_id` and
`position_bits_to_shift`, across the polls that follow; its `source_identity` string is
valid until the subscription's next poll or close ("Lifetimes"), so a caller that wants
it copies it then.
A session id it holds no constants for is an image the poll joined at its start, and
the caller keeps that frame's `term_id` and `term_offset` and takes the image's
constants once the poll returns.

A committed log has no image to ask: the handle keeps the replay's image to itself.
Its poll carries each entry's position with the header it hands the handler, and
`cavalry_committed_log_entry_position(header)` returns it: the position in the log at
which the entry begins, which is the position in the member's recording and the same
on every member (`spec/cluster.md`, "The log"). The position after the entry is that
plus the header's `frame_length` rounded up to a multiple of 32, and after the poll
`cavalry_committed_log_position` gives the position after the last entry delivered.
A header from `cavalry_subscription_poll` carries no position in this version, and
the function says so with `CAVALRY_ERR_NO_POSITION` instead of returning a number.

## Versions and what a reader rejects

A program built against a header whose `CAVALRY_ABI_VERSION` differs from the
library's is refused at its first call: the header's `cavalry_context_init(context)`
expands to `cavalry_context_init_v(context, CAVALRY_ABI_VERSION)`, as zlib's
`deflateInit` expands to `deflateInit_` with `ZLIB_VERSION`, and the library compares
the number with its own and, when they differ, returns −1 with
`CAVALRY_ERR_ABI_VERSION` and a message that names both numbers and says to rebuild
against the library's header. The check is the default, so a C program that does
nothing about versions is checked, at one compare per context. A generated client,
which loads the library without the header's macros, calls `cavalry_context_init_v`
with the version its bindings were generated from, and calls `cavalry_abi_version()`
when it loads the library, before any other function, so that a mismatch fails at load
time.

Version 1 adds `cavalry_add_destination` and `cavalry_remove_destination` to "adds and
commands" and moves `CAVALRY_ABI_VERSION` to 1 (ADR 0057); every declaration of
version 0 is unchanged. Nothing is released (ADR 0003), so no built program meets the
refusal below; a generated client's package of version 0 refuses a version 1 library
when it loads, as its message says, until it is regenerated.

Version 2 adds the committed log: the opaque type `cavalry_committed_log_t`, the
struct `cavalry_committed_log_constants_t`, the ten functions of the "committed log"
group, `CAVALRY_SERVICE_LOG_STREAM_ID`, error code 138, `CAVALRY_ERR_NO_POSITION`, and
`CAVALRY_ABI_VERSION` 2 (ADR 0068); every declaration of version 1 is unchanged.
Nothing is released, so again no built program meets the refusal, and a generated
client's package of version 1 refuses a version 2 library when it loads until it is
regenerated.

A reader of the header, `cavalry-ffi`'s conformance test, checks it against
`spec/vectors/c-abi.json`, written by `spec/vectors/c-abi.py` from this page's tables:
every opaque type, every struct with its fields in order, the handler typedef, the
macro, every function with its return type and parameters, and every define with its
value. It rejects a header that lacks one of them, or declares one the vectors lack,
by name, so that this page and the header cannot drift apart. A change to any of them
is a new version of this page, a new ADR and new vectors, before any code; a newer
library answers an older program's `cavalry_context_init_v` with
`CAVALRY_ERR_ABI_VERSION`, never with a different behaviour under the old names.

## Out of this version

Image handlers and the invoker mode; counters and `add counter`, the Rust
`Cavalry::add_counter` and its `Counter`, and the Rust `Cavalry::counters()`; a
subscription's bounded poll, `Subscription::bounded_poll`, which the committed log's
poll is built on; the armed recording, `Cavalry::arm_recording` and
`ArmedRecording`, and a recording's stop position, `Recording::stop_position`; the
sync of a recording on command, `Cavalry::sync_recording`, `Recording::sync` and
`Recording::request_sync`; the channel status as a function, which the counters file
holds; a vectored offer, the controlled and block polls and the fragment assemblers;
the destinations of a subscription, Aeron's multi-destination subscription; `revoke`
and `append_padding`; `local_sockaddrs`; `print_counters` and
the `cnc` family, which `cavalry doctor` serves; `cavalry_header_position` and the
position context in the header's values; the Rust `Flags::NONE` and `Flags::RESERVED`,
which a C caller writes as 0 and reads from the flags byte; and the Rust `reason()` names of `Offer`,
`Closed`, `Fault` and `ClientError`, which every message carries in its parentheses
and the codes stand for. Each arrives, if a client needs it, under a new version of
this page with its ADR (ADR 0051, "Out of this ADR").
