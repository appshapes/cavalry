"""The Go writer of clients/generate.py: `write_go(header)` returns the files of
`clients/go`, every one generated from the committed header (clients/README.md; ADR 0046,
card 207), except `example/main.go`, the README's program, which is written by hand.

The module reaches the ABI through cgo, compiled against the header and linked with
`cavalry-ffi`'s static library, so it has no dependency beyond Go's standard library.
cgo's pointer rules decide what crosses the boundary: C must not hold a Go pointer, and
the ABI keeps the `clientd` of a poll for the poll's length, so the client hands the ABI
the subscription's index in a bounded table and the trampoline reads the subscription
back with one atomic load; a slice over the log is made with `unsafe.Slice` from the
address the ABI gives; the bytes of an offer are passed as the address of the slice's
first element, which cgo allows because they hold no Go pointer and the ABI copies them
before it returns. The ABI's error is the calling thread's and Go may run a goroutine's
next cgo call on another thread, so a call that is not a message holds the goroutine to
its thread with `runtime.LockOSThread` for the call and the read of its error, and a
per-message call goes through a function of `shim.c` that copies the error into the
handle in the same C call when it fails. The files: `go.mod`; `cavalry.go`, the client's
own code, with the header's defines as Go constants, `Error`, `HandlerError`, `Client`,
`Publication`, `Subscription`, `CommittedLog`, `ClusterSession`, `ClusterService`, `Header`,
`Image`, `Recording` and `Replay`;
`cavalry_test.go`, the package's tests, which need no driver: the subscription table at
its bound, the trampolines' refusal of a slot the table does not hold, and what a cluster
service's trampoline answers for its entry;
`trampoline.go`, the exported Go functions the C trampolines call, one per fragment, per
committed log's entry, per cluster session's reply and per cluster service's entry (a file
with an `//export` may hold no C definition in its preamble, so it is a file of its own);
`shim.h` and `shim.c`, the C trampolines, the one `cavalry_subscription_poll` call, the one
`cavalry_committed_log_poll` call (ADR 0070), the one `cavalry_cluster_session_poll` call
and the one `cavalry_cluster_service_poll` call, whose trampoline passes the ABI the action
the Go handler returned (ADR 0124), and the per-message calls that keep their error. Python 3.9 standard library only.
"""

from __future__ import annotations

import re

MODULE = "github.com/appshapes/cavalry/clients/go"
INITIALISMS = {"ABI": "ABI", "IO": "IO", "EOS": "EOS", "UTF8": "UTF8", "ID": "ID"}
C_LIMITS = {"INT64_MAX": "math.MaxInt64", "INT64_MIN": "math.MinInt64", "INT32_MAX": "math.MaxInt32",
            "INT32_MIN": "math.MinInt32", "UINT8_MAX": "math.MaxUint8", "UINT16_MAX": "math.MaxUint16"}
# The per-message functions of the generated Go, which clients/lint.py reads (clients/README.md).
PER_MESSAGE_GO = ("Publication.TryClaim", "Publication.SetReservedValue", "Publication.Commit", "Publication.Abort",
                  "Publication.Offer", "Subscription.Poll", "Subscription.deliver", "Subscription.deliverEntry",
                  "CommittedLog.Poll", "Header.Values", "cavalryGoFragment", "cavalryGoEntry",
                  # A cluster session and a cluster service (ADR 0124), and the schedule and
                  # the cancel of a service's timer (ADR 0157).
                  "ClusterSession.Offer", "ClusterSession.Poll", "ClusterService.Poll", "ClusterService.Reply",
                  "ClusterService.ScheduleTimer", "ClusterService.CancelTimer",
                  "Subscription.deliverReply", "Subscription.deliverClusterEntry", "cavalryGoReply",
                  "cavalryGoClusterEntry")
MAX_SUBSCRIPTIONS = 4096


def go_name(define: str) -> str:
    """`CAVALRY_PUBLICATION_BACK_PRESSURED` as `PublicationBackPressured`; an initialism
    stays upper-case (`ABIVersion`, `ErrNotUTF8`)."""
    parts = define.split("_")
    if parts[0] == "CAVALRY":
        parts = parts[1:]
    return "".join(INITIALISMS.get(p, p.capitalize()) for p in parts)


def go_value(value: str) -> str:
    if value in C_LIMITS:
        return C_LIMITS[value]
    if value.startswith('"'):
        return value
    if re.fullmatch(r"-?\d+", value) or re.fullmatch(r"0x[0-9a-fA-F]+", value):
        return value
    if re.fullmatch(r"\(?-?\d+\)?", value):
        return value.strip("()")
    raise SystemExit(f"generate_go: cannot read the define's value {value!r}")


def write_go(header) -> dict[str, str]:
    defines = [(n, v) for n, v in header.defines if n != "CAVALRY_H"]
    if "CAVALRY_ABI_VERSION" not in dict(defines):
        raise SystemExit("generate_go: the header has no CAVALRY_ABI_VERSION, which the version check reads")
    # Aligned as gofmt aligns a const block: the names to the widest, then `=`, then the
    # values to the widest, then the comment, so the committed file is gofmt's own.
    width = max(len(go_name(n)) for n, _ in defines)
    value_width = max(len(go_value(v)) for _, v in defines)
    lines = []
    for n, v in defines:
        lines.append(f"\t{go_name(n):<{width}} = {go_value(v):<{value_width}} // {n}")
    constants = "\n".join(lines)
    imports = ["fmt", "runtime", "sync", "sync/atomic", "unsafe"]
    if any(go_value(v).startswith("math.") for _, v in defines):
        imports.append("math")
    import_block = "\n".join(f'\t"{i}"' for i in sorted(imports))
    code = GO_CLIENT.replace("@CONSTANTS@", constants).replace("@IMPORTS@", import_block)
    code = code.replace("@MAX_SUBSCRIPTIONS@", str(MAX_SUBSCRIPTIONS))
    return {
        "go.mod": GO_MOD,
        "cavalry.go": code,
        "cavalry_test.go": GO_TEST,
        "trampoline.go": GO_TRAMPOLINE.replace("@MAX_SUBSCRIPTIONS@", str(MAX_SUBSCRIPTIONS)),
        "shim.h": SHIM_H,
        "shim.c": SHIM_C,
    }


GO_MOD = f"""module {MODULE}

go 1.24
"""

SHIM_H = r'''/* The C side of the Go client (clients/go/README.md): the one
 * cavalry_subscription_poll call, with the trampoline of shim.c as the handler and the
 * subscription's table index as clientd, the polls of a committed log, a cluster session
 * and a cluster service in the same way, and each per-message call with the thread's
 * error copied in the same C call when it fails. Generated by clients/generate.py go from
 * crates/cavalry-ffi/include/cavalry.h; do not edit by hand, `just lint` regenerates and
 * compares. */
#ifndef CAVALRY_GO_SHIM_H
#define CAVALRY_GO_SHIM_H

#include <stddef.h>
#include <stdint.h>

#include "cavalry.h"

/* A per-message call's failure, copied from the calling thread's error before the call
 * returns to Go, which may run the goroutine's next cgo call on another thread
 * (clients/go/README.md, "The thread's error"): the ABI function that failed,
 * cavalry_errcode's code and cavalry_errmsg's message, whole, since the ABI's buffer is
 * 512 bytes with its NUL too (spec/c-abi.md, "The message"). A code of 0 is no failure
 * yet. The handle keeps one, and the shim writes it only when the call failed. */
typedef struct cavalry_go_error_s {
  int32_t code;
  char function[48];
  char message[512];
} cavalry_go_error_t;

/* The code of a failure that is this client's own refusal, before the ABI was called, of a
 * count or a length below zero (ADR 0164). It is none of the ABI's: the ABI's codes are
 * above zero (spec/c-abi.md, "Error codes"). */
#define CAVALRY_GO_ERR_NEGATIVE (-1)

/* cavalry_publication_try_claim, with `error` written when it returns
 * CAVALRY_PUBLICATION_ERROR. */
int64_t cavalry_go_try_claim(cavalry_publication_t *publication, int64_t length, cavalry_buffer_claim_t *claim,
                             cavalry_go_error_t *error);

/* cavalry_buffer_claim_set_reserved_value, with `error` written when it returns -1. */
int cavalry_go_set_reserved_value(cavalry_buffer_claim_t *claim, int64_t value, cavalry_go_error_t *error);

/* cavalry_buffer_claim_commit, with `error` written when it returns -1. */
int64_t cavalry_go_commit(cavalry_buffer_claim_t *claim, cavalry_go_error_t *error);

/* cavalry_buffer_claim_abort, with `error` written when it returns -1. */
int64_t cavalry_go_abort(cavalry_buffer_claim_t *claim, cavalry_go_error_t *error);

/* cavalry_publication_offer, with `error` written when it returns
 * CAVALRY_PUBLICATION_ERROR. */
int64_t cavalry_go_offer(cavalry_publication_t *publication, const uint8_t *buffer, size_t length,
                         cavalry_go_error_t *error);

/* Polls `subscription` for up to `fragment_limit` fragments, calling the exported Go
 * function cavalryGoFragment for each with `index`, the subscription's 1-based index in
 * the client's table, as clientd. The ABI's result: the count, or -1, with `error`
 * written. */
int cavalry_go_poll(cavalry_subscription_t *subscription, uintptr_t index, int64_t fragment_limit,
                    cavalry_go_error_t *error);

/* Polls the committed log `log` for up to `fragment_limit` entries, as cavalry_go_poll
 * polls a subscription, with `index` the log's slot in the client's table. Its
 * trampoline is the log's own: it reads each entry's position in the log and calls the
 * exported Go function cavalryGoEntry with it. The ABI's result: the count, or -1, with
 * `error` written. */
int cavalry_go_committed_log_poll(cavalry_committed_log_t *log, uintptr_t index, int64_t fragment_limit,
                                  cavalry_go_error_t *error);

/* cavalry_cluster_session_offer, with `error` written when it returns
 * CAVALRY_PUBLICATION_ERROR. */
int64_t cavalry_go_cluster_session_offer(cavalry_cluster_session_t *session, const uint8_t *buffer, size_t length,
                                         cavalry_go_error_t *error);

/* Polls the cluster session `session` for up to `fragment_limit` fragments of its response
 * channel, as cavalry_go_poll polls a subscription, with `index` the session's slot in the
 * client's table. Its trampoline is the session's own: it reads each reply's values from
 * the header and calls the exported Go function cavalryGoReply with them. The ABI's
 * result: the count of replies handed over, or -1, with `error` written. */
int cavalry_go_cluster_session_poll(cavalry_cluster_session_t *session, uintptr_t index, int64_t fragment_limit,
                                    cavalry_go_error_t *error);

/* cavalry_cluster_service_reply, with `error` written when it returns -1, for a call from
 * outside the handler, or CAVALRY_CLUSTER_REPLY_TOO_LONG, which sets the thread's error
 * too. */
int cavalry_go_cluster_service_reply(cavalry_cluster_service_t *service, int64_t session_id, const uint8_t *buffer,
                                     size_t length, cavalry_go_error_t *error);

/* cavalry_cluster_service_schedule_timer and cavalry_cluster_service_cancel_timer, each
 * with `error` written when it returns -1, for a call from outside the handler. */
int cavalry_go_cluster_service_schedule_timer(cavalry_cluster_service_t *service, int64_t correlation_id,
                                              int64_t deadline, cavalry_go_error_t *error);
int cavalry_go_cluster_service_cancel_timer(cavalry_cluster_service_t *service, int64_t correlation_id,
                                            cavalry_go_error_t *error);

/* Polls the cluster service `service`, with `client`, for up to `fragment_limit` entries,
 * with `index` the service's slot in the client's table. Its trampoline is the service's
 * own: it reads each entry's values from the header, calls the exported Go function
 * cavalryGoClusterEntry with them, and passes the ABI the action that function returns,
 * unless it is CAVALRY_ACTION_CONTINUE, which the ABI does with no call. An action the ABI
 * refuses, and an entry whose values it does not give, abort the entry: the ABI's error is
 * copied into `error` in that call, and `*aborted` is 1 when the poll returns, else 0. The
 * ABI's result: the count of entries the handler took, or -1, with `error` written. */
int cavalry_go_cluster_service_poll(cavalry_cluster_service_t *service, cavalry_t *client, uintptr_t index,
                                    int64_t fragment_limit, int *aborted, cavalry_go_error_t *error);

#endif
'''

SHIM_C = r'''/* The C trampoline the ABI calls per fragment, the poll that installs it
 * (clients/go/README.md, "The call-back, by construction"), and the per-message calls
 * that copy the thread's error when they fail ("The thread's error"). Generated by
 * clients/generate.py go from crates/cavalry-ffi/include/cavalry.h; do not edit by hand,
 * `just lint` regenerates and compares, and clients/lint.py reads every function for C's
 * forms of a lock. */
#include "shim.h"

#include <stdio.h>

#include "_cgo_export.h"

/* Copies the NUL-terminated `text` into `out` of `size` bytes, cut at `size` - 1 bytes,
 * then the NUL. */
static void cavalry_go_copy(char *out, size_t size, const char *text) {
  size_t i = 0;
  for (; i + 1 < size && text[i] != '\0'; i++) {
    out[i] = text[i];
  }
  out[i] = '\0';
}

/* Keeps the calling thread's error in `error`, with the ABI function that failed: called
 * in the C call that failed, so the thread is that call's. */
static void cavalry_go_keep(cavalry_go_error_t *error, const char *function) {
  error->code = (int32_t) cavalry_errcode();
  cavalry_go_copy(error->function, sizeof error->function, function);
  cavalry_go_copy(error->message, sizeof error->message, cavalry_errmsg());
}

/* Keeps in `error` this client's own refusal of a count or a length below zero, with the
 * ABI function it would have called. The ABI takes a count as a size_t, and a negative
 * int converted for it is a count near 2 ** 64: a claim of that many bytes, or a poll with
 * no limit, for a caller that asked for neither. So the count crosses to this file as an
 * int64, which holds every Go int, and is refused here, before the ABI is called. The
 * code is CAVALRY_GO_ERR_NEGATIVE, this client's own and none of the ABI's, whose codes
 * are above zero (spec/c-abi.md, "Error codes"; ADR 0164). */
static void cavalry_go_refuse(cavalry_go_error_t *error, const char *function, const char *parameter, int64_t given) {
  error->code = CAVALRY_GO_ERR_NEGATIVE;
  cavalry_go_copy(error->function, sizeof error->function, function);
  snprintf(error->message, sizeof error->message, "%s: an int from 0 expected, a count or a length, and %lld was "
           "given; the ABI would be passed another count, near 2 ** 64, and was not called. Pass a count that is not "
           "below zero. See clients/go/README.md#what-it-must-never-do", parameter, (long long) given);
}

int64_t cavalry_go_try_claim(cavalry_publication_t *publication, int64_t length, cavalry_buffer_claim_t *claim,
                             cavalry_go_error_t *error) {
  if (length < 0) {
    cavalry_go_refuse(error, "cavalry_publication_try_claim", "length", length);
    return CAVALRY_PUBLICATION_ERROR;
  }
  int64_t position = cavalry_publication_try_claim(publication, (size_t) length, claim);
  if (position == CAVALRY_PUBLICATION_ERROR) {
    cavalry_go_keep(error, "cavalry_publication_try_claim");
  }
  return position;
}

int cavalry_go_set_reserved_value(cavalry_buffer_claim_t *claim, int64_t value, cavalry_go_error_t *error) {
  int result = cavalry_buffer_claim_set_reserved_value(claim, value);
  if (result != 0) {
    cavalry_go_keep(error, "cavalry_buffer_claim_set_reserved_value");
  }
  return result;
}

int64_t cavalry_go_commit(cavalry_buffer_claim_t *claim, cavalry_go_error_t *error) {
  int64_t position = cavalry_buffer_claim_commit(claim);
  if (position < 0) {
    cavalry_go_keep(error, "cavalry_buffer_claim_commit");
  }
  return position;
}

int64_t cavalry_go_abort(cavalry_buffer_claim_t *claim, cavalry_go_error_t *error) {
  int64_t position = cavalry_buffer_claim_abort(claim);
  if (position < 0) {
    cavalry_go_keep(error, "cavalry_buffer_claim_abort");
  }
  return position;
}

int64_t cavalry_go_offer(cavalry_publication_t *publication, const uint8_t *buffer, size_t length,
                         cavalry_go_error_t *error) {
  int64_t position = cavalry_publication_offer(publication, buffer, length);
  if (position == CAVALRY_PUBLICATION_ERROR) {
    cavalry_go_keep(error, "cavalry_publication_offer");
  }
  return position;
}

/* The ABI's fragment handler: one call into Go per fragment, with the body's address and
 * length and the frame's header as they are. The casts drop `const`, which cgo's C types
 * do not carry; the Go side reads and never writes through them. */
static void cavalry_go_trampoline(void *clientd, const uint8_t *buffer, size_t length,
                                  const cavalry_header_t *header) {
  cavalryGoFragment((uintptr_t) clientd, (uint8_t *) buffer, length, (cavalry_header_t *) header);
}

int cavalry_go_poll(cavalry_subscription_t *subscription, uintptr_t index, int64_t fragment_limit,
                    cavalry_go_error_t *error) {
  if (fragment_limit < 0) {
    cavalry_go_refuse(error, "cavalry_subscription_poll", "fragmentLimit", fragment_limit);
    return -1;
  }
  int count = cavalry_subscription_poll(subscription, cavalry_go_trampoline, (void *) index, (size_t) fragment_limit);
  if (count < 0) {
    cavalry_go_keep(error, "cavalry_subscription_poll");
  }
  return count;
}

/* The ABI's fragment handler for a committed log's poll: one call into Go per entry, as
 * cavalry_go_trampoline makes for a fragment, with the entry's position in the log, which
 * the ABI gives for the header its own poll handed over. The position is read here, a C
 * call inside the ABI's call-back, so that the Go side makes no cgo call of its own for
 * it. */
static void cavalry_go_entry_trampoline(void *clientd, const uint8_t *buffer, size_t length,
                                        const cavalry_header_t *header) {
  cavalryGoEntry((uintptr_t) clientd, (uint8_t *) buffer, length, (cavalry_header_t *) header,
                 cavalry_committed_log_entry_position(header));
}

int cavalry_go_committed_log_poll(cavalry_committed_log_t *log, uintptr_t index, int64_t fragment_limit,
                                  cavalry_go_error_t *error) {
  if (fragment_limit < 0) {
    cavalry_go_refuse(error, "cavalry_committed_log_poll", "fragmentLimit", fragment_limit);
    return -1;
  }
  int count = cavalry_committed_log_poll(log, cavalry_go_entry_trampoline, (void *) index, (size_t) fragment_limit);
  if (count < 0) {
    cavalry_go_keep(error, "cavalry_committed_log_poll");
  }
  return count;
}

int64_t cavalry_go_cluster_session_offer(cavalry_cluster_session_t *session, const uint8_t *buffer, size_t length,
                                         cavalry_go_error_t *error) {
  int64_t position = cavalry_cluster_session_offer(session, buffer, length);
  if (position == CAVALRY_PUBLICATION_ERROR) {
    cavalry_go_keep(error, "cavalry_cluster_session_offer");
  }
  return position;
}

/* The ABI's fragment handler for a cluster session's poll: one call into Go per reply, as
 * cavalry_go_trampoline makes for a fragment, with the reply's values in the header's
 * place. They are read here, a C call inside the ABI's call-back, so that the Go side
 * makes no cgo call of its own for them (ADR 0124). The ABI gives them for the header its
 * own poll handed over; were it to refuse, the reply is handed over with both at -1. */
static void cavalry_go_reply_trampoline(void *clientd, const uint8_t *buffer, size_t length,
                                        const cavalry_header_t *header) {
  cavalry_cluster_reply_values_t values = { -1, -1 };
  cavalry_cluster_reply_values(header, &values);
  cavalryGoReply((uintptr_t) clientd, (uint8_t *) buffer, length, values.leadership_term_id, values.timestamp);
}

int cavalry_go_cluster_session_poll(cavalry_cluster_session_t *session, uintptr_t index, int64_t fragment_limit,
                                    cavalry_go_error_t *error) {
  if (fragment_limit < 0) {
    cavalry_go_refuse(error, "cavalry_cluster_session_poll", "fragmentLimit", fragment_limit);
    return -1;
  }
  int count = cavalry_cluster_session_poll(session, cavalry_go_reply_trampoline, (void *) index, (size_t) fragment_limit);
  if (count < 0) {
    cavalry_go_keep(error, "cavalry_cluster_session_poll");
  }
  return count;
}

int cavalry_go_cluster_service_reply(cavalry_cluster_service_t *service, int64_t session_id, const uint8_t *buffer,
                                     size_t length, cavalry_go_error_t *error) {
  int code = cavalry_cluster_service_reply(service, session_id, buffer, length);
  if (code < 0 || code == CAVALRY_CLUSTER_REPLY_TOO_LONG) {
    cavalry_go_keep(error, "cavalry_cluster_service_reply");
  }
  return code;
}

int cavalry_go_cluster_service_schedule_timer(cavalry_cluster_service_t *service, int64_t correlation_id,
                                              int64_t deadline, cavalry_go_error_t *error) {
  int code = cavalry_cluster_service_schedule_timer(service, correlation_id, deadline);
  if (code < 0) {
    cavalry_go_keep(error, "cavalry_cluster_service_schedule_timer");
  }
  return code;
}

int cavalry_go_cluster_service_cancel_timer(cavalry_cluster_service_t *service, int64_t correlation_id,
                                            cavalry_go_error_t *error) {
  int code = cavalry_cluster_service_cancel_timer(service, correlation_id);
  if (code < 0) {
    cavalry_go_keep(error, "cavalry_cluster_service_cancel_timer");
  }
  return code;
}

/* What a cluster service's poll hands its trampoline as clientd: the service's slot in the
 * table, for the Go side, the service's handle, for the entry's action, and where an
 * aborted entry is told. It is C memory, on the stack of the poll that made it, valid for
 * that poll, and it holds no Go pointer. */
typedef struct {
  uintptr_t index;
  cavalry_cluster_service_t *service;
  cavalry_go_error_t *error;
  int aborted;
} cavalry_go_service_poll_t;

/* The ABI's fragment handler for a cluster service's poll: one call into Go per entry,
 * with the entry's values, read here as a reply's are, and the action that call returns
 * passed to the ABI, unless it is CAVALRY_ACTION_CONTINUE, which is what the ABI does with
 * no call. The Go side returns CAVALRY_ACTION_ABORT for a handler that panicked. An action
 * the ABI refuses, and an entry whose values it does not give, which on the header its own
 * poll handed over it cannot, abort the entry too: the ABI's error is kept, in this call,
 * for the poll to return, and the entry is not taken, so that a service never passes an
 * entry its handler did not finish (ADR 0124). */
static void cavalry_go_service_trampoline(void *clientd, const uint8_t *buffer, size_t length,
                                          const cavalry_header_t *header) {
  cavalry_go_service_poll_t *poll = clientd;
  cavalry_cluster_entry_values_t values;
  int32_t action;
  if (cavalry_cluster_entry_values(header, &values) != 0) {
    cavalry_go_keep(poll->error, "cavalry_cluster_entry_values");
    poll->aborted = 1;
    cavalry_cluster_service_action(poll->service, CAVALRY_ACTION_ABORT);
    return;
  }
  action = cavalryGoClusterEntry(poll->index, (uint8_t *) buffer, length, values.position, values.leadership_term_id,
                                 values.timestamp, values.session_id, values.type, values.leader_member_id,
                                 values.timed_out ? 1 : 0, values.correlation_id);
  if (action == CAVALRY_ACTION_CONTINUE) {
    return;
  }
  if (cavalry_cluster_service_action(poll->service, action) != 0) {
    cavalry_go_keep(poll->error, "cavalry_cluster_service_action");
    poll->aborted = 1;
    cavalry_cluster_service_action(poll->service, CAVALRY_ACTION_ABORT);
  }
}

int cavalry_go_cluster_service_poll(cavalry_cluster_service_t *service, cavalry_t *client, uintptr_t index,
                                    int64_t fragment_limit, int *aborted, cavalry_go_error_t *error) {
  if (fragment_limit < 0) {
    *aborted = 0;
    cavalry_go_refuse(error, "cavalry_cluster_service_poll", "fragmentLimit", fragment_limit);
    return -1;
  }
  cavalry_go_service_poll_t poll = { index, service, error, 0 };
  int count = cavalry_cluster_service_poll(service, client, cavalry_go_service_trampoline, &poll,
                                           (size_t) fragment_limit);
  *aborted = poll.aborted;
  if (count < 0) {
    cavalry_go_keep(error, "cavalry_cluster_service_poll");
  }
  return count;
}
'''

GO_TEST = r'''package cavalry

// The package's own tests, which conformance/go/run.sh runs with `go test ./...` beside
// `go vet`, with no driver: the subscription table at its bound (CLAUDE.md, invariant 3),
// the trampolines' refusal of a slot the table does not hold, and what a cluster
// service's trampoline answers for its entry (ADR 0124). One more needs a
// cluster member, a committed log's open with the table full: run.sh runs it again beside
// the member conformance/log-member.sh chooses, and it is skipped with none. Generated by
// clients/generate.py go; do not edit by hand, `just lint` regenerates and compares.

import (
	"errors"
	"fmt"
	"os"
	"strings"
	"testing"
	"time"
)

// TestTheTableAtItsBound fills the table through register, which AddSubscription calls
// once the ABI's add has returned. The register past the bound returns an *Error with
// ErrSubscriptionsFull whose message names the bound and the README's anchor, and leaves
// the subscription without a slot; a slot that unregister frees is the next register's.
func TestTheTableAtItsBound(t *testing.T) {
	held := make([]*Subscription, 0, maxSubscriptions)
	t.Cleanup(func() {
		for _, s := range held {
			unregister(s)
		}
	})
	for len(held) < maxSubscriptions {
		s := &Subscription{}
		if err := register(s); err != nil {
			t.Fatalf("register %d of %d returned %v", len(held)+1, maxSubscriptions, err)
		}
		held = append(held, s)
	}
	if Registered() != maxSubscriptions {
		t.Fatalf("the table holds %d subscriptions; its bound is %d", Registered(), maxSubscriptions)
	}

	extra := &Subscription{}
	err := register(extra)
	var abi *Error
	if !errors.As(err, &abi) || abi.Fn != "cavalry_add_subscription" || abi.Code != ErrSubscriptionsFull ||
		!strings.Contains(abi.Message, fmt.Sprintf("holds %d open subscriptions", maxSubscriptions)) ||
		!strings.Contains(abi.Message, "see clients/go/README.md#threads") || extra.slot != 0 {
		t.Fatalf("register past the bound returned %v and left slot %d; expected *Error with ErrSubscriptionsFull "+
			"naming the bound and clients/go/README.md#threads, and no slot", err, extra.slot)
	}

	freed := held[maxSubscriptions/2]
	slot := freed.slot
	unregister(freed)
	if freed.slot != 0 || subscriptions[slot-1].Load() != nil || Registered() != maxSubscriptions-1 {
		t.Fatalf("unregister of slot %d left the slot %d and the table holding %d", slot, freed.slot, Registered())
	}
	next := &Subscription{}
	if err := register(next); err != nil || next.slot != slot {
		t.Fatalf("register after a free returned %v at slot %d; expected the freed slot %d", err, next.slot, slot)
	}
	held[maxSubscriptions/2] = next
	if err := register(&Subscription{}); !errors.As(err, &abi) || abi.Code != ErrSubscriptionsFull {
		t.Fatalf("register past the bound again returned %v; expected ErrSubscriptionsFull", err)
	}
}

// TestTheTrampolineCallsNoHandlerForASlotTheTableDoesNotHold calls cavalryGoFragment as
// the C trampoline does: a slot of 0, one past the table and an empty slot call no
// handler, and the slot a subscription holds calls its handler, once per call.
func TestTheTrampolineCallsNoHandlerForASlotTheTableDoesNotHold(t *testing.T) {
	if Registered() != 0 {
		t.Fatalf("the table holds %d subscriptions before the test; expected none", Registered())
	}
	calls := 0
	s := &Subscription{handler: func(body []byte, header *Header) { calls++ }}
	cavalryGoFragment(0, nil, 0, nil)
	cavalryGoFragment(maxSubscriptions+1, nil, 0, nil)
	cavalryGoFragment(1, nil, 0, nil)
	if calls != 0 {
		t.Fatalf("a slot of 0, of %d and an empty slot called the handler %d time(s); expected none",
			maxSubscriptions+1, calls)
	}
	if err := register(s); err != nil || s.slot != 1 {
		t.Fatalf("register into the empty table returned %v at slot %d; expected slot 1", err, s.slot)
	}
	t.Cleanup(func() { unregister(s) })
	cavalryGoFragment(1, nil, 0, nil)
	cavalryGoFragment(2, nil, 0, nil)
	cavalryGoFragment(maxSubscriptions, nil, 0, nil)
	if calls != 1 || s.header.ptr != nil {
		t.Fatalf("the subscription's slot and two empty slots called the handler %d time(s); expected 1", calls)
	}
}

// TestTheEntryTrampolineHandsThePositionOnAndCallsNoHandlerForASlotTheTableDoesNotHold
// calls cavalryGoEntry as the C trampoline of a committed log's poll does, with the
// position that trampoline read: a slot of 0, one past the table and an empty slot call
// no handler; the slot a committed log holds calls its entry handler once per call, with
// the position it was given; and a slot of the other kind, a subscription's for an entry
// and a log's for a fragment, runs no handler and is counted as a panic, which the poll
// then reports, where a silent drop would lose the message.
func TestTheEntryTrampolineHandsThePositionOnAndCallsNoHandlerForASlotTheTableDoesNotHold(t *testing.T) {
	if Registered() != 0 {
		t.Fatalf("the table holds %d slots before the test; expected none", Registered())
	}
	var positions []int64
	log := &Subscription{entry: func(body []byte, header *Header, position int64) { positions = append(positions, position) }}
	cavalryGoEntry(0, nil, 0, nil, 96)
	cavalryGoEntry(maxSubscriptions+1, nil, 0, nil, 96)
	cavalryGoEntry(1, nil, 0, nil, 96)
	if len(positions) != 0 {
		t.Fatalf("a slot of 0, of %d and an empty slot called the entry handler with %v; expected no call",
			maxSubscriptions+1, positions)
	}
	if err := register(log); err != nil || log.slot != 1 {
		t.Fatalf("register into the empty table returned %v at slot %d; expected slot 1", err, log.slot)
	}
	t.Cleanup(func() { unregister(log) })
	fragments := 0
	subscription := &Subscription{handler: func(body []byte, header *Header) { fragments++ }}
	if err := register(subscription); err != nil || subscription.slot != 2 {
		t.Fatalf("register beside the log returned %v at slot %d; expected slot 2", err, subscription.slot)
	}
	t.Cleanup(func() { unregister(subscription) })
	cavalryGoEntry(1, nil, 0, nil, 96)
	cavalryGoEntry(1, nil, 0, nil, 192)
	cavalryGoEntry(2, nil, 0, nil, 288)
	cavalryGoFragment(1, nil, 0, nil)
	cavalryGoEntry(maxSubscriptions, nil, 0, nil, 384)
	if len(positions) != 2 || positions[0] != 96 || positions[1] != 192 || fragments != 0 || log.header.ptr != nil {
		t.Fatalf("the log's slot called its entry handler with %v and the subscription's handler ran %d time(s); "+
			"expected positions 96 and 192 and no fragment", positions, fragments)
	}
	if log.raised != 1 || log.first == nil || subscription.raised != 1 || subscription.first == nil {
		t.Fatalf("a fragment to the log's slot and an entry to the subscription's counted %d and %d panics; "+
			"expected one each, with its value kept", log.raised, subscription.raised)
	}
}

// TestTheClusterTrampolinesHandTheValuesOnAndAbortAnEntryNoHandlerTook calls
// cavalryGoReply and cavalryGoClusterEntry as the C trampolines of a cluster session's
// poll and of a cluster service's do, with the values those trampolines read. A slot of 0,
// one past the table and an empty slot call no handler, and an entry there is answered
// ActionAbort, since no handler took it. A session's slot hands its handler the reply's
// two values in the one struct the session keeps. A service's slot hands its handler the
// entry's eight values in the one struct the service keeps, and the service, and answers
// what the handler returned, whatever it is: the C trampoline passes it to the ABI, which
// refuses what is no action. A handler that panics is counted, with its value kept, and
// its entry is answered ActionAbort; and a slot of another kind, a session's for an entry
// or a service's for a reply, runs no handler and is counted as a panic.
func TestTheClusterTrampolinesHandTheValuesOnAndAbortAnEntryNoHandlerTook(t *testing.T) {
	if Registered() != 0 {
		t.Fatalf("the table holds %d slots before the test; expected none", Registered())
	}
	var replies []ClusterReplyValues
	var keptReply *ClusterReplyValues
	session := &Subscription{reply: func(body []byte, reply *ClusterReplyValues) {
		replies = append(replies, *reply)
		keptReply = reply
	}}
	cavalryGoReply(0, nil, 0, 5, 1700000000000)
	cavalryGoReply(maxSubscriptions+1, nil, 0, 5, 1700000000000)
	cavalryGoReply(1, nil, 0, 5, 1700000000000)
	unheld := []int32{
		int32(cavalryGoClusterEntry(0, nil, 0, 320, 5, 1700000000000, 7, ClusterEntrySessionMessage, -1, 0, 0)),
		int32(cavalryGoClusterEntry(maxSubscriptions+1, nil, 0, 320, 5, 1700000000000, 7, ClusterEntrySessionMessage, -1, 0, 0)),
		int32(cavalryGoClusterEntry(1, nil, 0, 320, 5, 1700000000000, 7, ClusterEntrySessionMessage, -1, 0, 0)),
	}
	if len(replies) != 0 || unheld[0] != ActionAbort || unheld[1] != ActionAbort || unheld[2] != ActionAbort {
		t.Fatalf("a slot of 0, of %d and an empty slot called the reply handler %d time(s) and answered %v for an entry; "+
			"expected no call and ActionAbort, %d, for each", maxSubscriptions+1, len(replies), unheld, ActionAbort)
	}
	if err := register(session); err != nil || session.slot != 1 {
		t.Fatalf("register into the empty table returned %v at slot %d; expected slot 1", err, session.slot)
	}
	t.Cleanup(func() { unregister(session) })
	cavalryGoReply(1, nil, 0, 5, 1700000000000)
	cavalryGoReply(1, nil, 0, 6, 1700000000001)
	if len(replies) != 2 || replies[0] != (ClusterReplyValues{5, 1700000000000}) ||
		replies[1] != (ClusterReplyValues{6, 1700000000001}) || keptReply != &session.replyValues {
		t.Fatalf("the session's slot handed its handler %v; expected the two replies' values, in the struct the "+
			"session keeps", replies)
	}

	service := &ClusterService{}
	state := &Subscription{owner: service}
	var entries []ClusterEntryValues
	answer := int32(ActionContinue)
	state.service = func(body []byte, entry *ClusterEntryValues, handed *ClusterService) int32 {
		if handed != service || entry != &state.entryValues || body != nil {
			t.Errorf("the handler was handed service %p, entry %p and a body of %d bytes; expected the service of the "+
				"slot, the struct it keeps and no body", handed, entry, len(body))
		}
		entries = append(entries, *entry)
		if answer < 0 {
			panic("the handler's own")
		}
		return answer
	}
	if err := register(state); err != nil || state.slot != 2 {
		t.Fatalf("register beside the session returned %v at slot %d; expected slot 2", err, state.slot)
	}
	t.Cleanup(func() { unregister(state) })
	want := ClusterEntryValues{Position: 320, LeadershipTermID: 5, Timestamp: 1700000000000, SessionID: 7,
		Type: ClusterEntrySessionMessage, LeaderMemberID: -1, TimedOut: true, CorrelationID: 77}
	for _, answer = range []int32{ActionContinue, ActionBreak, ActionAbort, 3} {
		got := int32(cavalryGoClusterEntry(2, nil, 0, 320, 5, 1700000000000, 7, ClusterEntrySessionMessage, -1, 1, 77))
		if got != answer || entries[len(entries)-1] != want {
			t.Fatalf("a handler that returned %d was answered %d for the entry %+v; expected its own answer and %+v",
				answer, got, entries[len(entries)-1], want)
		}
	}
	if state.raised != 0 || len(entries) != 4 {
		t.Fatalf("four handlers that returned were counted as %d panics over %d entries", state.raised, len(entries))
	}
	answer = -1
	if got := cavalryGoClusterEntry(2, nil, 0, 416, 5, 1700000000000, 7, ClusterEntrySessionMessage, -1, 0, 0); got != ActionAbort ||
		state.raised != 1 || state.first != "the handler's own" {
		t.Fatalf("a handler that panicked was answered %d and counted %d time(s) with %v kept; expected ActionAbort, "+
			"one panic and its value", got, state.raised, state.first)
	}
	if got := cavalryGoClusterEntry(1, nil, 0, 512, 5, 1700000000000, 7, ClusterEntrySessionMessage, -1, 0, 0); got != ActionAbort ||
		session.raised != 1 || session.first == nil {
		t.Fatalf("an entry to the session's slot was answered %d and counted %d panic(s); expected ActionAbort and "+
			"one, with its value kept", got, session.raised)
	}
	cavalryGoReply(2, nil, 0, 5, 1700000000000)
	if state.raised != 2 || len(replies) != 2 {
		t.Fatalf("a reply to the service's slot counted %d panics and the session's handler ran %d times; expected "+
			"two panics in all and two replies", state.raised, len(replies))
	}
}

// TestACommittedLogAtTheTablesBound opens a committed log while the table is full, beside
// a cluster member: conformance/go/run.sh runs it with CAVALRY_GO_MEMBER_DIR naming the
// driver directory of the member conformance/log-member.sh chooses, member 1 unless it
// joined its leader's log late, and it is skipped with no member. The open returns no log
// and an *Error with ErrSubscriptionsFull under the log's own function,
// cavalry_committed_log, and the table holds what it held. The ABI's log is closed and not
// lost: eighty opens are refused so, more than the 64 replays a client follows at once,
// each still for the table's reason, and the open after a slot is freed succeeds.
func TestACommittedLogAtTheTablesBound(t *testing.T) {
	dir := os.Getenv("CAVALRY_GO_MEMBER_DIR")
	if dir == "" {
		t.Skip("no cluster member: conformance/go/run.sh runs this beside the member conformance/log-member.sh chooses, with CAVALRY_GO_MEMBER_DIR set")
	}
	client, err := NewClient(dir)
	if err != nil {
		t.Fatalf("NewClient(%q) returned %v", dir, err)
	}
	t.Cleanup(func() { client.Close() })
	// The member has a log before the table is filled: one opened, asked for until the
	// member has one, and closed.
	var first *CommittedLog
	for until := time.Now().Add(30 * time.Second); first == nil; time.Sleep(50 * time.Millisecond) {
		if first, err = client.CommittedLog(0); err != nil {
			t.Fatalf("the first open returned %v", err)
		}
		if first == nil && time.Now().After(until) {
			t.Fatalf("the driver at %s hosted no member with a log within 30 s", dir)
		}
	}
	if err := first.Close(); err != nil {
		t.Fatalf("the first log's close returned %v", err)
	}
	if Registered() != 0 {
		t.Fatalf("the table holds %d slots before it is filled; expected none", Registered())
	}
	held := make([]*Subscription, 0, maxSubscriptions)
	t.Cleanup(func() {
		for _, s := range held {
			unregister(s)
		}
	})
	for len(held) < maxSubscriptions {
		s := &Subscription{}
		if err := register(s); err != nil {
			t.Fatalf("register %d of %d returned %v", len(held)+1, maxSubscriptions, err)
		}
		held = append(held, s)
	}

	const refused = 80
	for i := 1; i <= refused; i++ {
		log, err := client.CommittedLog(0)
		var abi *Error
		if log != nil || !errors.As(err, &abi) || abi.Fn != "cavalry_committed_log" || abi.Code != ErrSubscriptionsFull ||
			!strings.Contains(abi.Message, "see clients/go/README.md#threads") {
			t.Fatalf("open %d of %d with the table full returned a log %v and %v; expected no log and an *Error of "+
				"cavalry_committed_log with ErrSubscriptionsFull naming clients/go/README.md#threads", i, refused, log != nil, err)
		}
		if Registered() != maxSubscriptions {
			t.Fatalf("open %d of %d with the table full left the table holding %d slots, not %d", i, refused,
				Registered(), maxSubscriptions)
		}
	}

	last := held[len(held)-1]
	held = held[:len(held)-1]
	unregister(last)
	log, err := client.CommittedLog(0)
	if err != nil || log == nil {
		t.Fatalf("the open after a slot was freed returned a log %v and %v; expected a log: an open refused for the "+
			"table's reason closes the ABI's log, and %d of them left open would hold every replay the client follows",
			log != nil, err, refused)
	}
	if Registered() != maxSubscriptions {
		t.Fatalf("an open log holds %d slots with the rest; expected the table full at %d", Registered(), maxSubscriptions)
	}
	if err := log.Close(); err != nil {
		t.Fatalf("the log's close returned %v", err)
	}
	if client.IsClosed() {
		t.Fatalf("the client closed: %d", client.Closed())
	}
}
'''

GO_TRAMPOLINE = r'''package cavalry

// The exported Go functions the C trampolines of shim.c call: one per fragment of a
// subscription's poll, one per entry of a committed log's, one per reply of a cluster
// session's and one per entry of a cluster service's. They are in a file of their
// own because a file with an //export directive may hold only declarations in its
// preamble, and cavalry.go's preamble holds the build flags. Generated by
// clients/generate.py go; do not edit by hand, `just lint` regenerates and compares.

/*
#include <stddef.h>
#include <stdint.h>

#include "cavalry.h"
*/
import "C"

import "unsafe"

// held is what the table holds in slot, a poll's clientd: the state of a subscription, of
// a committed log, of a cluster session or of a cluster service, or nil for a slot of 0,
// one past the table or one the table does not hold. One atomic load.
func held(slot C.uintptr_t) *Subscription {
	if slot == 0 || slot > @MAX_SUBSCRIPTIONS@ {
		return nil
	}
	return subscriptions[slot-1].Load()
}

// cavalryGoFragment is the fragment handler the ABI reaches through shim.c: slot is the
// poll's clientd, the subscription's 1-based index in the table, which the subscription
// registered under a lock when it was added and which stays valid for the poll, since the
// ABI refuses a close from inside the poll's handler. The C trampoline passes it as a
// uintptr_t, never as a pointer: Go's stack copier ends the process on a value below
// 4,096 in a pointer's place. A slot of 0, one past the table or one the table does not
// hold calls no handler, which cavalry_test.go checks. The body is a slice over the log's
// bytes, made with unsafe.Slice and never copied; the header is valid for the call. The
// handler runs inside deliver, which recovers its panic, so that no panic unwinds through
// the ABI's frames and every fragment of the poll reaches the handler (README, "The
// panic").
//
//export cavalryGoFragment
func cavalryGoFragment(slot C.uintptr_t, buffer *C.uint8_t, length C.size_t, header *C.cavalry_header_t) {
	s := held(slot)
	if s == nil {
		return
	}
	body := unsafe.Slice((*byte)(unsafe.Pointer(buffer)), int(length))
	s.header.ptr = header
	s.header.read = false
	s.deliver(body)
	s.header.ptr = nil
}

// cavalryGoEntry is the fragment handler of a committed log's poll, as cavalryGoFragment
// is a subscription's: slot is the log's 1-based index in the table, and position is the
// entry's position in the log, which the C trampoline read from the header before it
// called here, so this function makes no cgo call. A slot of 0, one past the table or one
// the table does not hold calls no handler, which cavalry_test.go checks. The handler
// runs inside deliverEntry, which recovers its panic; a slot that holds a subscription
// has no entry handler, and that call is a panic deliverEntry counts.
//
//export cavalryGoEntry
func cavalryGoEntry(slot C.uintptr_t, buffer *C.uint8_t, length C.size_t, header *C.cavalry_header_t,
	position C.int64_t) {
	s := held(slot)
	if s == nil {
		return
	}
	body := unsafe.Slice((*byte)(unsafe.Pointer(buffer)), int(length))
	s.header.ptr = header
	s.header.read = false
	s.deliverEntry(body, int64(position))
	s.header.ptr = nil
}

// cavalryGoReply is the fragment handler of a cluster session's poll, as cavalryGoFragment
// is a subscription's: slot is the session's 1-based index in the table, and
// leadershipTermID and timestamp are the reply's values, which the C trampoline read from
// the header before it called here, so this function makes no cgo call. It writes them
// into the one ClusterReplyValues the session keeps. A slot of 0, one past the table or
// one the table does not hold calls no handler, which cavalry_test.go checks. The body is
// a slice over the log's bytes, never copied, and nil for a reply of no bytes. The handler
// runs inside deliverReply, which recovers its panic.
//
//export cavalryGoReply
func cavalryGoReply(slot C.uintptr_t, buffer *C.uint8_t, length C.size_t, leadershipTermID C.int64_t,
	timestamp C.int64_t) {
	s := held(slot)
	if s == nil {
		return
	}
	var body []byte
	if length != 0 {
		body = unsafe.Slice((*byte)(unsafe.Pointer(buffer)), int(length))
	}
	s.replyValues.LeadershipTermID = int64(leadershipTermID)
	s.replyValues.Timestamp = int64(timestamp)
	s.deliverReply(body)
}

// cavalryGoClusterEntry is the fragment handler of a cluster service's poll: slot is the
// service's 1-based index in the table, and the eight values are the entry's, which the C
// trampoline read from the header before it called here, so this function makes no cgo
// call. It writes them into the one ClusterEntryValues the service keeps, and returns the
// entry's action, which the C trampoline passes to the ABI: what the handler returned, or
// ActionAbort when the handler panicked, which deliverClusterEntry recovers. A slot of 0,
// one past the table or one the table does not hold calls no handler and answers
// ActionAbort, since no handler took the entry, which cavalry_test.go checks.
//
// The body is a slice over the log's bytes, never copied, for a session's message, and
// nil for every other entry. For those the ABI's buffer points at no byte: it is an
// address of 1, and a slice at an address below 4,096 would end the process when Go next
// copied the goroutine's stack, so no slice is made from it.
//
//export cavalryGoClusterEntry
func cavalryGoClusterEntry(slot C.uintptr_t, buffer *C.uint8_t, length C.size_t, position C.int64_t,
	leadershipTermID C.int64_t, timestamp C.int64_t, sessionID C.int64_t, entryType C.int32_t,
	leaderMemberID C.int32_t, timedOut C.int32_t, correlationID C.int64_t) C.int32_t {
	s := held(slot)
	if s == nil {
		return ActionAbort
	}
	var body []byte
	if length != 0 {
		body = unsafe.Slice((*byte)(unsafe.Pointer(buffer)), int(length))
	}
	e := &s.entryValues
	e.Position = int64(position)
	e.LeadershipTermID = int64(leadershipTermID)
	e.Timestamp = int64(timestamp)
	e.SessionID = int64(sessionID)
	e.Type = int32(entryType)
	e.LeaderMemberID = int32(leaderMemberID)
	e.TimedOut = timedOut != 0
	e.CorrelationID = int64(correlationID)
	return C.int32_t(s.deliverClusterEntry(body))
}
'''

GO_CLIENT = r'''// Package cavalry is the Go client over the C ABI of spec/c-abi.md (clients/go/README.md).
//
// Generated by clients/generate.py go; do not edit by hand, `just lint` regenerates and
// compares. A Client wraps a driver's directory; its Publication, Subscription, Recording,
// Replay, CommittedLog, ClusterSession and ClusterService wrap the ABI's handles through
// cgo. Every message goes through one ABI call
// on the caller's goroutine: TryClaim, Offer, Commit, Abort, SetReservedValue, Poll,
// Values and a cluster service's Reply hold invariant 2's rules, no lock, no goroutine, no
// channel, no allocation, no
// panic (a handler's own panic is recovered and returned by Poll once the ABI has
// returned), and clients/lint.py reads them for Go's forms of each; a per-message call
// that fails leaves its error in the handle's Err, copied by shim.c in the call that
// failed. Anything that is not a message, an add, a command, a close, holds the
// goroutine's OS thread for the call and returns an *Error with the ABI's code and
// message, read on the thread of the call that failed.
package cavalry

/*
#cgo CFLAGS: -I${SRCDIR}/../../crates/cavalry-ffi/include
#cgo LDFLAGS: ${SRCDIR}/../../target/release/libcavalry_ffi.a
#cgo linux LDFLAGS: -lpthread -ldl -lm
#cgo darwin LDFLAGS: -framework CoreFoundation -framework Security

#include <stdlib.h>

#include "cavalry.h"
#include "shim.h"
*/
import "C"

import (
@IMPORTS@
)

// The header's constants, as the ABI names them (spec/c-abi.md), in Go's casing.
const (
@CONSTANTS@
)

// Error is a call that is not a message failed: Fn is the ABI function, and Code and
// Message are the ABI's (cavalry_errcode, cavalry_errmsg), the message naming what to
// do next and its docs anchor.
type Error struct {
	Fn      string
	Code    int32
	Message string
}

func (e *Error) Error() string {
	return fmt.Sprintf("%s: %s (error %d)", e.Fn, e.Message, e.Code)
}

// ErrNegative is the Code of an *Error for a count or a length below zero, which this
// client refused before the ABI was called: TryClaim's length, a poll's fragment limit,
// ImageAt's index. It is no code of the ABI's, whose codes are above zero
// (spec/c-abi.md, "Error codes"); Fn is the ABI function the call would have made. As a
// size_t, which is what the ABI takes, a negative int is a count near 2 ** 64: a claim of
// that many bytes, or a poll with no limit (README, "What it must never do"; ADR 0164).
const ErrNegative int32 = C.CAVALRY_GO_ERR_NEGATIVE

// HandlerError is the handler panicked during a poll, a subscription's fragment handler
// or, when CommittedLog is true, a committed log's entry handler, or, when Cluster is
// "session", a cluster session's reply handler: Fragments is the count
// the poll returned once the ABI was done, of fragments, of entries or of replies, or -1
// when the
// poll also stopped on a fault, which the Err and the Fault of what was polled read;
// Raised how many handler calls panicked, and Cause the first panic's value, kept as it
// was panicked.
//
// A cluster service's entry handler, Cluster "service", is the one whose failure stops
// its poll. The entry whose handler panicked, or returned what is no action, was aborted:
// it is not taken, the poll ended at it, and the next poll hands it over again. Fragments
// is then the count of entries the poll took before that entry, Raised is 1, and Cause is
// the panic's value, or the *Error with ErrClusterAction that the ABI gave for the value
// the handler returned.
type HandlerError struct {
	Fragments    int
	Raised       int
	Cause        any
	CommittedLog bool
	Cluster      string
}

func (e *HandlerError) Error() string {
	if e.Cluster == "service" {
		// The one poll a handler's failure stops (README, "The cluster service").
		took := fmt.Sprintf("after the %d entries this poll took", e.Fragments)
		if e.Fragments == 1 {
			took = "after the 1 entry this poll took"
		}
		if e.Fragments < 0 {
			took = "in a poll that also stopped on a fault, which this ClusterService's Fault reads until its next " +
				"poll and its Err until its next failure"
		}
		return fmt.Sprintf("the entry handler panicked, or returned what is no action, %s. Its entry was aborted: "+
			"it is not taken, the poll ended at it, and the next poll hands it over again. Fix the handler: what it "+
			"did, %v, is this error's Cause; every poll hands the entry over, and returns this error, until the "+
			"handler takes it. See clients/go/README.md#the-panic", took, e.Cause)
	}
	one, many := "fragment", "fragments"
	delivered := fmt.Sprintf("the %d fragment(s) this poll delivered", e.Fragments)
	fault := "the fragments this poll delivered before it stopped on a fault, which Subscription.Fault reads"
	if e.CommittedLog {
		// The fault of a committed log's own image is reported by one poll (README, "The
		// committed log").
		one, many = "entry", "entries"
		delivered = fmt.Sprintf("the %d entries this poll delivered", e.Fragments)
		if e.Fragments == 1 {
			delivered = "the 1 entry this poll delivered"
		}
		fault = "the entries this poll delivered before it stopped on a fault, which this CommittedLog's Fault " +
			"reads until its next poll and its Err until its next failure, and its Faulted from then on when " +
			"the fault is the log's own image's"
	}
	if e.Cluster == "session" {
		one, many = "reply", "replies"
		delivered = fmt.Sprintf("the %d replies this poll handed over", e.Fragments)
		if e.Fragments == 1 {
			delivered = "the 1 reply this poll handed over"
		}
		fault = "the replies this poll handed over before it stopped on a fault, which this ClusterSession's Fault " +
			"reads until its next poll and its Err until its next failure"
	}
	if e.Fragments < 0 {
		delivered = fault
	}
	return fmt.Sprintf("the %s handler panicked on %d of %s; every %s reached the handler once, in "+
		"order. Fix the handler: its first panic's value, %v, is this error's Cause; polling again does not deliver "+
		"these %s again. See clients/go/README.md#the-panic", one, e.Raised, delivered, one, e.Cause, many)
}

// Unwrap returns the Cause when it is an error, for errors.Is and errors.As.
func (e *HandlerError) Unwrap() error {
	if err, ok := e.Cause.(error); ok {
		return err
	}
	return nil
}

// lastError is the calling OS thread's error as an *Error. The ABI keeps an error per
// thread (spec/c-abi.md, "Error codes") and Go may run a goroutine's next cgo call on
// another thread, so every function that calls lastError or check holds the goroutine to
// its thread with runtime.LockOSThread from before its first ABI call until it returns,
// which clients/lint.py checks (README, "The thread's error").
func lastError(fn string) *Error {
	return &Error{Fn: fn, Code: int32(C.cavalry_errcode()), Message: C.GoString(C.cavalry_errmsg())}
}

// check is nil for a result of 0, else lastError, under lastError's rule.
func check(fn string, result C.int) error {
	if result != 0 {
		return lastError(fn)
	}
	return nil
}

// keptError is the failure a function of shim.c copied into kept in the C call that
// failed, as an *Error, or nil before the first failure.
func keptError(kept *C.cavalry_go_error_t) error {
	if kept.code == 0 {
		return nil
	}
	return &Error{Fn: C.GoString(&kept.function[0]), Code: int32(kept.code), Message: C.GoString(&kept.message[0])}
}

// Errcode is the ABI's error code of the calling OS thread's last failed call
// (cavalry_errcode). Go may run a goroutine's next cgo call on another thread, so it is
// the code of the goroutine's own failed call only where the failing call and this read
// run on one thread: inside a fragment handler, which cgo holds to the poll's thread, as
// for a Header.Values that returned false, or between runtime.LockOSThread and
// runtime.UnlockOSThread around both. A per-message call's failure is Publication.Err's
// and Subscription.Err's, and every other call returns its *Error (README, "The
// thread's error").
func Errcode() int32 { return int32(C.cavalry_errcode()) }

// Errmsg is the message of the calling OS thread's last failed call (cavalry_errmsg),
// under Errcode's rule.
func Errmsg() string { return C.GoString(C.cavalry_errmsg()) }

// LibraryABIVersion is the ABI version of the library this package links
// (cavalry_abi_version).
func LibraryABIVersion() int32 { return int32(C.cavalry_abi_version()) }

// Version is the library's own version, as cavalry_version_full gives it.
func Version() string { return C.GoString(C.cavalry_version_full()) }

// CheckABIVersion is the check NewClient makes first, with ABIVersion, before any other
// function of the ABI (spec/c-abi.md, "Versions and what a reader rejects"): the library
// this package links must be of the ABI version the package was generated from, and a
// mismatch is an *Error with ErrABIVersion that names both numbers.
func CheckABIVersion(expected int32) error {
	found := LibraryABIVersion()
	if found == expected {
		return nil
	}
	return &Error{Fn: "cavalry_abi_version", Code: ErrABIVersion, Message: fmt.Sprintf(
		"the library this package links is ABI version %d and this package was generated from a header of ABI "+
			"version %d; rebuild the library from the same checkout (cargo build --release -p cavalry-ffi --lib) or "+
			"regenerate the package from the library's header with python3 clients/generate.py go; see "+
			"docs/reference/c-abi.md#versions-and-what-a-reader-rejects", found, expected)}
}

// The table the trampolines read subscriptions, committed logs, cluster sessions and
// cluster services from: C must not hold a Go pointer, so the ABI is given a 1-based index
// into it as the poll's clientd, or, for a cluster service, a C struct that holds the
// index. An open subscription holds one slot, and so does an open committed log, an open
// cluster session and an open cluster service. The table is bounded (CLAUDE.md, invariant
// 3): a process holds at most maxSubscriptions of the four together through this package,
// and AddSubscription, CommittedLog, ConnectCluster and ClusterService return an *Error
// with ErrSubscriptionsFull above that. Slots are taken and freed under registryMu, on the
// control plane; a trampoline reads a slot with one atomic load.
const maxSubscriptions = @MAX_SUBSCRIPTIONS@

var subscriptions [maxSubscriptions]atomic.Pointer[Subscription]
var registryMu sync.Mutex

func register(s *Subscription) error {
	registryMu.Lock()
	defer registryMu.Unlock()
	for i := range subscriptions {
		if subscriptions[i].Load() == nil {
			subscriptions[i].Store(s)
			s.slot = uintptr(i + 1)
			return nil
		}
	}
	return &Error{Fn: "cavalry_add_subscription", Code: ErrSubscriptionsFull, Message: fmt.Sprintf(
		"this process holds %d open subscriptions, committed logs, cluster sessions and cluster services through "+
			"the Go client, the table's bound; "+
			"close one before adding another; see clients/go/README.md#threads", maxSubscriptions)}
}

func unregister(s *Subscription) {
	registryMu.Lock()
	defer registryMu.Unlock()
	if s.slot != 0 {
		subscriptions[s.slot-1].Store(nil)
		s.slot = 0
	}
}

// Registered is how many slots of the table are held: one for every open subscription,
// committed log, cluster session and cluster service, and none once each is closed. The
// runner reads
// it to check that a close frees the slot.
func Registered() int {
	count := 0
	for i := range subscriptions {
		if subscriptions[i].Load() != nil {
			count++
		}
	}
	return count
}

func cString(s string) *C.char { return C.CString(s) }

func freeCString(s *C.char) { C.free(unsafe.Pointer(s)) }

// Client is a client of one driver: NewClient's cavalry_init over a context with this
// package's ABI version. Goroutines may share it: every call on its handle holds mu, since
// the ABI runs one call on a handle at a time and a read of the client never runs beside
// an add (spec/c-abi.md, "Threads"), so a call waits for the one another goroutine is
// making on this client, an add or a command for up to 10 s. No message passes through
// a Client. Close is idempotent.
type Client struct {
	mu       sync.Mutex
	handle   *C.cavalry_t
	dir      string
	clientID int64
}

// NewClient connects to the driver at dir, or at CAVALRY_DIR, or at the driver's default
// directory when dir is "": CheckABIVersion first, then cavalry_context_init_v with this
// package's ABI version, then cavalry_init.
func NewClient(dir string) (*Client, error) {
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	if err := CheckABIVersion(ABIVersion); err != nil {
		return nil, err
	}
	var context *C.cavalry_context_t
	if err := check("cavalry_context_init_v", C.cavalry_context_init_v(&context, C.int(ABIVersion))); err != nil {
		return nil, err
	}
	defer C.cavalry_context_close(context)
	if dir != "" {
		cdir := cString(dir)
		defer freeCString(cdir)
		if err := check("cavalry_context_set_dir", C.cavalry_context_set_dir(context, cdir)); err != nil {
			return nil, err
		}
	}
	var handle *C.cavalry_t
	if err := check("cavalry_init", C.cavalry_init(&handle, context)); err != nil {
		return nil, err
	}
	return &Client{handle: handle, dir: C.GoString(C.cavalry_dir(handle)), clientID: int64(C.cavalry_client_id(handle))}, nil
}

// Dir is the driver's directory.
func (c *Client) Dir() string { return c.dir }

// ClientID is the id the driver gave this client.
func (c *Client) ClientID() int64 { return c.clientID }

// IsClosed is whether the client is closed, by Close or by the driver.
func (c *Client) IsClosed() bool {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.handle == nil || bool(C.cavalry_is_closed(c.handle))
}

// Closed is 0 while open, else why the client closed, Closed* (CAVALRY_CLOSED_*).
func (c *Client) Closed() int32 {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.handle == nil {
		return ClosedByUser
	}
	return int32(C.cavalry_closed(c.handle))
}

// Close closes the client, cavalry_close, and none of the handles it made: each
// Publication, Subscription, Recording, Replay, CommittedLog, ClusterSession and
// ClusterService is freed by its own Close and by nothing else, and a publication or a
// subscription answers as closed until then (spec/c-abi.md, "Lifetimes"). An unclosed
// Subscription, CommittedLog, ClusterSession or ClusterService keeps its slot in the table
// of @MAX_SUBSCRIPTIONS@, so a program that closes only its clients fills the table, and
// AddSubscription, CommittedLog, ConnectCluster and ClusterService return
// ErrSubscriptionsFull. Idempotent. The handle is forgotten whatever the ABI returns, but
// for one close: a close from inside the handler of a cluster service whose poll holds
// this client, which the ABI refuses (spec/c-abi.md, "Calls during a claim or a poll").
// Close then returns the *Error with ErrPolling and keeps the handle, the client stays
// open, and the service's next poll and a Close after the poll has returned use it.
func (c *Client) Close() error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.handle == nil {
		return nil
	}
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	if result := C.cavalry_close(c.handle); result != 0 {
		err := lastError("cavalry_close")
		if err.Code == ErrPolling {
			return err
		}
		c.handle = nil
		return err
	}
	c.handle = nil
	return nil
}

// AddPublication adds a publication on channel and streamID, waiting on the driver for
// up to 10 s.
func (c *Client) AddPublication(channel string, streamID int32) (*Publication, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	cchannel := cString(channel)
	defer freeCString(cchannel)
	var handle *C.cavalry_publication_t
	if err := check("cavalry_add_publication", C.cavalry_add_publication(&handle, c.handle, cchannel, C.int32_t(streamID))); err != nil {
		return nil, err
	}
	return newPublication(handle)
}

// AddExclusivePublication adds a publication of its own session on channel and streamID.
func (c *Client) AddExclusivePublication(channel string, streamID int32) (*Publication, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	cchannel := cString(channel)
	defer freeCString(cchannel)
	var handle *C.cavalry_publication_t
	if err := check("cavalry_add_exclusive_publication", C.cavalry_add_exclusive_publication(&handle, c.handle, cchannel, C.int32_t(streamID))); err != nil {
		return nil, err
	}
	return newPublication(handle)
}

// AddSubscription adds a subscription on channel and streamID and registers it in the
// table the trampoline reads.
func (c *Client) AddSubscription(channel string, streamID int32) (*Subscription, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	cchannel := cString(channel)
	defer freeCString(cchannel)
	var handle *C.cavalry_subscription_t
	if err := check("cavalry_add_subscription", C.cavalry_add_subscription(&handle, c.handle, cchannel, C.int32_t(streamID))); err != nil {
		return nil, err
	}
	return newSubscription(handle)
}

// StartRecording starts recording the live publication on channel and streamID.
func (c *Client) StartRecording(channel string, streamID int32) (*Recording, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	cchannel := cString(channel)
	defer freeCString(cchannel)
	var handle *C.cavalry_recording_t
	if err := check("cavalry_start_recording", C.cavalry_start_recording(&handle, c.handle, cchannel, C.int32_t(streamID))); err != nil {
		return nil, err
	}
	return newRecording(handle)
}

// AddDestination adds the destination channel, cavalry:udp?endpoint=<host>:<port>,
// to the publication's control channel, answered by the driver (ADR 0053).
func (c *Client) AddDestination(p *Publication, channel string) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	cchannel := cString(channel)
	defer freeCString(cchannel)
	return check("cavalry_add_destination", C.cavalry_add_destination(c.handle, p.handle, cchannel))
}

// RemoveDestination removes the destination channel, as it was added; one not there
// is removed by nothing.
func (c *Client) RemoveDestination(p *Publication, channel string) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	cchannel := cString(channel)
	defer freeCString(cchannel)
	return check("cavalry_remove_destination", C.cavalry_remove_destination(c.handle, p.handle, cchannel))
}

// StopRecording stops the recording.
func (c *Client) StopRecording(recordingID int64) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	return check("cavalry_stop_recording", C.cavalry_stop_recording(c.handle, C.int64_t(recordingID)))
}

// TruncateRecording truncates the recording to position.
func (c *Client) TruncateRecording(recordingID int64, position int64) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	return check("cavalry_truncate_recording", C.cavalry_truncate_recording(c.handle, C.int64_t(recordingID), C.int64_t(position)))
}

// PurgeSegments purges the recording's segments before newStartPosition.
func (c *Client) PurgeSegments(recordingID int64, newStartPosition int64) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	return check("cavalry_purge_segments", C.cavalry_purge_segments(c.handle, C.int64_t(recordingID), C.int64_t(newStartPosition)))
}

// PurgeRecording purges the recording.
func (c *Client) PurgeRecording(recordingID int64) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	return check("cavalry_purge_recording", C.cavalry_purge_recording(c.handle, C.int64_t(recordingID)))
}

// StartReplay replays length bytes of the recording from position onto replayStreamID
// of channel; NullPosition and NullLength replay all of it.
func (c *Client) StartReplay(recordingID int64, position int64, length int64, replayStreamID int32, channel string) (*Replay, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	cchannel := cString(channel)
	defer freeCString(cchannel)
	var handle *C.cavalry_replay_t
	if err := check("cavalry_start_replay", C.cavalry_start_replay(&handle, c.handle, C.int64_t(recordingID), C.int64_t(position),
		C.int64_t(length), C.int32_t(replayStreamID), cchannel)); err != nil {
		return nil, err
	}
	return newReplay(handle)
}

// StopReplay stops the replay session.
func (c *Client) StopReplay(replaySessionID int64) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	return check("cavalry_stop_replay", C.cavalry_stop_replay(c.handle, C.int64_t(replaySessionID)))
}

// CommittedLog opens the committed log of the cluster member on this client's driver at
// position, 0 or the Position an earlier CommittedLog reached. It returns nil and no
// error while the driver hosts no member that has a recording of its log, so a service
// started before its member asks again (spec/cluster.md, "The service"; spec/c-abi.md,
// "Functions", committed log). An open log holds one slot of the subscriptions' table.
func (c *Client) CommittedLog(position int64) (*CommittedLog, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	var handle *C.cavalry_committed_log_t
	if err := check("cavalry_committed_log", C.cavalry_committed_log(&handle, c.handle, C.int64_t(position))); err != nil {
		return nil, err
	}
	if handle == nil {
		return nil, nil
	}
	var k C.cavalry_committed_log_constants_t
	if err := check("cavalry_committed_log_constants", C.cavalry_committed_log_constants(handle, &k)); err != nil {
		C.cavalry_committed_log_close(handle, c.handle)
		return nil, err
	}
	state := &Subscription{}
	if err := register(state); err != nil {
		C.cavalry_committed_log_close(handle, c.handle)
		// The table's refusal names the add of a subscription; this one was a log's.
		if full, ok := err.(*Error); ok {
			full.Fn = "cavalry_committed_log"
		}
		return nil, err
	}
	return &CommittedLog{handle: handle, client: c, state: state, MemberID: int32(k.member_id),
		RecordingID: int64(k.recording_id)}, nil
}

// ConnectCluster opens a session with a cluster: members is the members string the
// cluster's members were started with, and responseChannel a channel of this host for the
// cluster's answers, cavalry:udp?endpoint=<host>:<port>. The call returns once the leader
// has opened the session, and waits for that for 20 s at most beyond its adds, holding the
// client's mutex as an add does. Its *Error has ErrClusterMembers, ErrClusterRefused,
// ErrClusterVersion or ErrClusterConnectTimeout, a fault's code, or the client's or the
// driver's (spec/cluster.md, "Sessions"; spec/c-abi.md, "Functions", cluster session; ADR
// 0122). An open session holds one slot of the subscriptions' table.
func (c *Client) ConnectCluster(members string, responseChannel string) (*ClusterSession, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	cmembers := cString(members)
	defer freeCString(cmembers)
	cchannel := cString(responseChannel)
	defer freeCString(cchannel)
	var handle *C.cavalry_cluster_session_t
	if err := check("cavalry_connect_cluster", C.cavalry_connect_cluster(&handle, c.handle, cmembers, cchannel)); err != nil {
		return nil, err
	}
	var k C.cavalry_cluster_session_constants_t
	if err := check("cavalry_cluster_session_constants", C.cavalry_cluster_session_constants(handle, &k)); err != nil {
		C.cavalry_cluster_session_close(handle)
		return nil, err
	}
	state := &Subscription{}
	if err := register(state); err != nil {
		C.cavalry_cluster_session_close(handle)
		// The table's refusal names the add of a subscription; this one was a session's.
		if full, ok := err.(*Error); ok {
			full.Fn = "cavalry_connect_cluster"
		}
		return nil, err
	}
	return &ClusterSession{handle: handle, state: state, SessionID: int64(k.session_id),
		MaxMessageLength: int(k.max_message_length)}, nil
}

// ClusterService makes a cluster's service beside the member on this client's driver. It
// sends nothing: the service's first Poll opens the member's log. The service's Poll and
// its Close are calls on this client. Poll is on every entry's path and takes no lock, so
// from here until the service's Close this client is used by the goroutine that polls the
// service and by no other: give the program's other goroutines a Client of their own
// (spec/cluster.md, "The service"; spec/c-abi.md, "Functions", cluster service, and
// "Threads"; ADR 0123; ADR 0124). An open service holds one slot of the subscriptions'
// table.
func (c *Client) ClusterService() (*ClusterService, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	var handle *C.cavalry_cluster_service_t
	if err := check("cavalry_cluster_service", C.cavalry_cluster_service(&handle, c.handle)); err != nil {
		return nil, err
	}
	state := &Subscription{}
	if err := register(state); err != nil {
		C.cavalry_cluster_service_close(handle, c.handle)
		// The table's refusal names the add of a subscription; this one was a service's.
		if full, ok := err.(*Error); ok {
			full.Fn = "cavalry_cluster_service"
		}
		return nil, err
	}
	service := &ClusterService{handle: handle, client: c, state: state}
	state.owner = service
	return service, nil
}

// Publication is a publication: TryClaim and Offer return the position after the frame
// or a code below zero, never an error; Claim is the slice over the bytes the last
// TryClaim reserved, from the one cavalry_buffer_claim_t this publication keeps, set to
// nil by Commit, Abort and Close. Err is the last per-message call that failed.
type Publication struct {
	handle *C.cavalry_publication_t
	claim  C.cavalry_buffer_claim_t
	err    C.cavalry_go_error_t

	// Claim is the writable slice over the claimed bytes, valid until the commit or abort.
	Claim []byte

	Channel          string
	RegistrationID   int64
	SessionID        int32
	StreamID         int32
	MaxPayloadLength int
	MaxMessageLength int
	IsExclusive      bool
}

func newPublication(handle *C.cavalry_publication_t) (*Publication, error) {
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	var k C.cavalry_publication_constants_t
	if err := check("cavalry_publication_constants", C.cavalry_publication_constants(handle, &k)); err != nil {
		C.cavalry_publication_close(handle)
		return nil, err
	}
	return &Publication{handle: handle, Channel: C.GoString(k.channel), RegistrationID: int64(k.registration_id),
		SessionID: int32(k.session_id), StreamID: int32(k.stream_id), MaxPayloadLength: int(k.max_payload_length),
		MaxMessageLength: int(k.max_message_length), IsExclusive: bool(k.is_exclusive)}, nil
}

// TryClaim claims length bytes: the position after the frame, with Claim the writable
// slice over them, or a result code below zero, which leaves Claim as it was; Err says
// why a PublicationError. One cgo call, to shim.c's one ABI call; the slice is made with
// unsafe.Slice and allocates nothing. A length below zero is refused before the ABI is
// called: the ABI's code for an error, with an Err whose Code is ErrNegative.
func (p *Publication) TryClaim(length int) int64 {
	position := int64(C.cavalry_go_try_claim(p.handle, C.int64_t(length), &p.claim, &p.err))
	if position >= 0 {
		p.Claim = unsafe.Slice((*byte)(unsafe.Pointer(p.claim.data)), int(p.claim.length))
	}
	return position
}

// Commit commits the claim: the position after the frame, or -1, with Err saying why.
func (p *Publication) Commit() int64 {
	p.Claim = nil
	return int64(C.cavalry_go_commit(&p.claim, &p.err))
}

// Abort aborts the claim, leaving padding: the position after the frame, or -1, with Err
// saying why.
func (p *Publication) Abort() int64 {
	p.Claim = nil
	return int64(C.cavalry_go_abort(&p.claim, &p.err))
}

// SetReservedValue sets the claimed frame's reserved value, any int64: 0, or -1, with
// Err saying why.
func (p *Publication) SetReservedValue(value int64) int32 {
	return int32(C.cavalry_go_set_reserved_value(&p.claim, C.int64_t(value), &p.err))
}

// Offer publishes message: the position after it, or a code below zero; Err says why a
// PublicationError. The ABI copies the bytes before it returns, so the slice is the
// caller's again at once.
func (p *Publication) Offer(message []byte) int64 {
	var buffer *C.uint8_t
	if len(message) > 0 {
		buffer = (*C.uint8_t)(unsafe.Pointer(&message[0]))
	}
	return int64(C.cavalry_go_offer(p.handle, buffer, C.size_t(len(message)), &p.err))
}

// Err is the failure of this publication's last per-message call that failed, a
// PublicationError of TryClaim or Offer, or a -1 of Commit, Abort or SetReservedValue:
// an *Error with the ABI function, code and message, which shim.c copied from the
// thread's error in the C call that failed, kept until the next failure; nil before the
// first (README, "The thread's error").
func (p *Publication) Err() error { return keptError(&p.err) }

// Position is the publication's position, or NullPosition.
func (p *Publication) Position() int64 { return int64(C.cavalry_publication_position(p.handle)) }

// PositionLimit is the position the publication may publish up to.
func (p *Publication) PositionLimit() int64 {
	return int64(C.cavalry_publication_position_limit(p.handle))
}

// IsConnected is whether a subscriber is connected.
func (p *Publication) IsConnected() bool { return bool(C.cavalry_publication_is_connected(p.handle)) }

// IsClosed is whether the publication is closed.
func (p *Publication) IsClosed() bool {
	return p.handle == nil || bool(C.cavalry_publication_is_closed(p.handle))
}

// Close closes the publication, aborting an outstanding claim. Idempotent; the handle is
// forgotten whatever the ABI returns. The claim struct is zeroed before the ABI's close,
// so that Commit, Abort and SetReservedValue after it hand the ABI a struct that names no
// publication, which it answers with -1 and ErrNoClaim in Err without reading the handle
// the close freed (spec/c-abi.md, "Which claim a struct names").
func (p *Publication) Close() error {
	if p.handle == nil {
		return nil
	}
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	p.Claim = nil
	p.claim = C.cavalry_buffer_claim_t{}
	err := check("cavalry_publication_close", C.cavalry_publication_close(p.handle))
	p.handle = nil
	return err
}

// FragmentHandler receives each fragment a poll delivers: body is a slice into the log,
// valid for the call, and header is the frame's header, valid for the call.
type FragmentHandler func(body []byte, header *Header)

// EntryHandler receives each entry a committed log's poll delivers: body and header as a
// fragment's, and position the entry's position in the log.
type EntryHandler func(body []byte, header *Header, position int64)

// ReplyHandler receives each reply a cluster session's poll hands over: body is the
// reply's payload, a slice into the log, valid for the call, and reply the reply's
// values.
type ReplyHandler func(body []byte, reply *ClusterReplyValues)

// ServiceHandler receives each committed entry a cluster service's poll hands over: body is a
// session message's payload, a slice into the log, valid for the call, and nil for every other
// entry; entry is the entry's values; and service is the service that was polled, for its
// Reply, ScheduleTimer and CancelTimer. What it returns is the entry's action: ActionContinue
// takes the entry and the poll goes on, ActionBreak takes it and ends the poll, and
// ActionAbort leaves it, ends the poll, and has the next poll hand it over again.
type ServiceHandler func(body []byte, entry *ClusterEntryValues, service *ClusterService) int32

// ClusterReplyValues is what a reply carries beside its payload, as
// cavalry_cluster_reply_values reads it. It is the one struct a session keeps, written
// for each reply before the handler runs, so copy a field out to keep it.
type ClusterReplyValues struct {
	// LeadershipTermID is the leadership term of the entry the reply answers.
	LeadershipTermID int64
	// Timestamp is the cluster's time when its leader appended that entry, in milliseconds
	// since 1970-01-01T00:00:00Z.
	Timestamp int64
}

// ClusterEntryValues is one committed entry as cavalry_cluster_entry_values reads it. It
// is the one struct a service keeps, written for each entry before the handler runs, so
// copy a field out to keep it.
type ClusterEntryValues struct {
	// Position is where the entry begins in the log.
	Position int64
	// LeadershipTermID is the leadership term the entry was appended in.
	LeadershipTermID int64
	// Timestamp is the cluster's time when its leader appended the entry, in milliseconds
	// since 1970-01-01T00:00:00Z: the one clock a service reads, so that every member's
	// service reaches the same state.
	Timestamp int64
	// SessionID is the session of a session's message, open or close, and -1 for every
	// other entry.
	SessionID int64
	// Type is what the entry is, ClusterEntry* (CAVALRY_CLUSTER_ENTRY_*).
	Type int32
	// LeaderMemberID is the member that leads the term a new leadership term event begins,
	// and -1 for every other entry.
	LeaderMemberID int32
	// TimedOut is whether the leader closed the session of a session close event for the
	// session timeout.
	TimedOut bool
	// CorrelationID is the id of the timer that is handed over, for an entry whose Type
	// is ClusterEntryTimer, and 0 for every other entry, where it says nothing.
	CorrelationID int64
}

// Frame is a frame's header as cavalry_header_values reads it (spec/frame.md).
type Frame struct {
	FrameLength   int32
	Version       int8
	Flags         uint8
	Type          uint16
	TermOffset    int32
	SessionID     int32
	StreamID      int32
	TermID        int32
	ReservedValue int64
}

// Header is the frame header a poll hands the handler: Values reads it through
// cavalry_header_values once per fragment, on the first call, into the one struct the
// subscription keeps, and returns the Frame by value; outside the handler call Values
// returns false.
type Header struct {
	ptr    *C.cavalry_header_t
	values C.cavalry_header_values_t
	read   bool
}

// Values is the frame's header, and whether the header is valid now: false outside the
// handler call, or when the ABI refused the read, which Errcode and Errmsg then describe,
// since the handler runs on the poll's thread.
func (h *Header) Values() (Frame, bool) {
	if h.ptr == nil {
		return Frame{}, false
	}
	if !h.read {
		if C.cavalry_header_values(h.ptr, &h.values) != 0 {
			return Frame{}, false
		}
		h.read = true
	}
	f := &h.values.frame
	return Frame{FrameLength: int32(f.frame_length), Version: int8(f.version), Flags: uint8(f.flags), Type: uint16(f._type),
		TermOffset: int32(f.term_offset), SessionID: int32(f.session_id), StreamID: int32(f.stream_id),
		TermID: int32(f.term_id), ReservedValue: int64(f.reserved_value)}, true
}

// Image is one of a subscription's images, valid until the subscription's next poll or
// close.
type Image struct {
	ptr *C.cavalry_image_t

	SourceIdentity      string
	CorrelationID       int64
	PositionBitsToShift uint
	SessionID           int32
	InitialTermID       int32
}

// Position is the image's position.
func (i *Image) Position() int64 { return int64(C.cavalry_image_position(i.ptr)) }

// IsEndOfStream is whether the image's publisher has ended the stream.
func (i *Image) IsEndOfStream() bool { return bool(C.cavalry_image_is_end_of_stream(i.ptr)) }

// Subscription is a subscription: Poll calls the handler for each fragment and returns
// the count, with a *HandlerError when a handler call panicked, once the ABI has
// returned, every later fragment still having reached the handler. Err is the last poll
// that failed.
type Subscription struct {
	handle  *C.cavalry_subscription_t
	slot    uintptr
	header  Header
	handler FragmentHandler
	// entry is the handler of a committed log's poll in progress: a CommittedLog keeps its
	// slot in the table and its poll's state in a Subscription with no handle of its own.
	entry   EntryHandler
	polling bool
	raised  int
	first   any
	err     C.cavalry_go_error_t

	// reply and service are the handlers of a cluster session's poll and of a cluster
	// service's in progress: each keeps its slot in the table and its poll's state in a
	// Subscription, as a committed log does. replyValues and entryValues are the one struct
	// each hands its handler, owner is the service a service's handler is handed, and
	// aborted is what shim.c writes when it aborted an entry whose action the ABI refused.
	reply       ReplyHandler
	service     ServiceHandler
	replyValues ClusterReplyValues
	entryValues ClusterEntryValues
	owner       *ClusterService
	aborted     C.int

	Channel        string
	RegistrationID int64
	StreamID       int32
}

func newSubscription(handle *C.cavalry_subscription_t) (*Subscription, error) {
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	var k C.cavalry_subscription_constants_t
	if err := check("cavalry_subscription_constants", C.cavalry_subscription_constants(handle, &k)); err != nil {
		C.cavalry_subscription_close(handle)
		return nil, err
	}
	s := &Subscription{handle: handle, Channel: C.GoString(k.channel), RegistrationID: int64(k.registration_id),
		StreamID: int32(k.stream_id)}
	if err := register(s); err != nil {
		C.cavalry_subscription_close(handle)
		return nil, err
	}
	return s, nil
}

// Poll delivers up to fragmentLimit fragments to handler: one ABI call, the count it
// returned (or -1, with Err saying why: a fault, which Fault reads, or ErrPolling for a
// poll from inside a handler of this subscription, which the ABI refuses and which
// touches nothing here), and a *HandlerError when a handler call panicked.
// A fragmentLimit below zero is refused before the ABI is called: -1, with an Err whose
// Code is ErrNegative.
func (s *Subscription) Poll(handler FragmentHandler, fragmentLimit int) (int, error) {
	if s.polling {
		return int(C.cavalry_go_poll(s.handle, C.uintptr_t(s.slot), C.int64_t(fragmentLimit), &s.err)), nil
	}
	s.handler = handler
	s.polling = true
	s.raised = 0
	s.first = nil
	count := int(C.cavalry_go_poll(s.handle, C.uintptr_t(s.slot), C.int64_t(fragmentLimit), &s.err))
	s.polling = false
	s.handler = nil
	if s.raised == 0 {
		return count, nil
	}
	first := s.first
	s.first = nil
	return count, &HandlerError{Fragments: count, Raised: s.raised, Cause: first}
}

// deliver calls the handler for one fragment and recovers its panic: the first value is
// kept and every one counted, so the poll goes on to its next fragment. A poll with no
// fragment handler, a nil one or a committed log's slot, panics on the call and is
// counted as any panic is: the poll returns a *HandlerError, and nothing is dropped in
// silence.
func (s *Subscription) deliver(body []byte) {
	defer s.recovered()
	s.handler(body, &s.header)
}

// deliverEntry calls a committed log's entry handler for one entry, with its position,
// and recovers its panic as deliver does. The position is the one the C trampoline read
// from the header (shim.c), so no call crosses to C here. A poll with no entry handler,
// a nil one or a subscription's slot, is a counted panic, as in deliver.
func (s *Subscription) deliverEntry(body []byte, position int64) {
	defer s.recovered()
	s.entry(body, &s.header, position)
}

// deliverReply calls a cluster session's reply handler for one reply, with the values the
// C trampoline read from the header (shim.c), and recovers its panic as deliver does: the
// first value is kept and every one counted, and the poll goes on to its next reply.
func (s *Subscription) deliverReply(body []byte) {
	defer s.recovered()
	s.reply(body, &s.replyValues)
}

// deliverClusterEntry calls a cluster service's entry handler for one entry and returns
// the action the handler returned. A handler that panics has its panic recovered as
// deliver's is, and the entry's action is then ActionAbort: the entry is not taken, the
// poll ends at it, and the next poll hands it over again (README, "The cluster service").
// A poll with no service handler, a nil one or another kind of slot, is a counted panic
// and an abort, as in deliver.
func (s *Subscription) deliverClusterEntry(body []byte) (action int32) {
	action = ActionAbort
	defer s.recovered()
	return s.service(body, &s.entryValues, s.owner)
}

// recovered is the deferred call of deliver, deliverEntry, deliverReply and
// deliverClusterEntry: it recovers a handler's panic, keeps the first value and counts
// every one.
func (s *Subscription) recovered() {
	if r := recover(); r != nil {
		s.raised++
		if s.raised == 1 {
			s.first = r
		}
	}
}

// Err is the failure of this subscription's last poll that returned -1: an *Error with
// the ABI function, code and message, which shim.c copied from the thread's error in the
// C call that failed, kept until the next failure; nil before the first (README, "The
// thread's error").
func (s *Subscription) Err() error { return keptError(&s.err) }

// Fault is what the last poll stopped on: the code (0 for none), the session and the
// position.
func (s *Subscription) Fault() (code int32, sessionID int32, position int64, err error) {
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	var f C.cavalry_fault_t
	if err := check("cavalry_subscription_fault", C.cavalry_subscription_fault(s.handle, &f)); err != nil {
		return 0, 0, 0, err
	}
	return int32(f.code), int32(f.session_id), int64(f.position), nil
}

// ImageCount is how many images the subscription has.
func (s *Subscription) ImageCount() int { return int(C.cavalry_subscription_image_count(s.handle)) }

// ImageAt is the image at index, with its constants copied now, or nil. An index below
// zero is an *Error whose Code is ErrNegative, and the ABI is not called.
func (s *Subscription) ImageAt(index int) (*Image, error) {
	if index < 0 {
		return nil, &Error{Fn: "cavalry_subscription_image_at_index", Code: ErrNegative, Message: fmt.Sprintf(
			"index: an int from 0 expected, and %d was given; the ABI would be passed another index, near 2 ** 64, "+
				"and was not called. Pass an index that is not below zero. See "+
				"clients/go/README.md#what-it-must-never-do", index)}
	}
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	ptr := C.cavalry_subscription_image_at_index(s.handle, C.size_t(index))
	if ptr == nil {
		return nil, nil
	}
	var k C.cavalry_image_constants_t
	if err := check("cavalry_image_constants", C.cavalry_image_constants(ptr, &k)); err != nil {
		return nil, err
	}
	return &Image{ptr: ptr, SourceIdentity: C.GoString(k.source_identity), CorrelationID: int64(k.correlation_id),
		PositionBitsToShift: uint(k.position_bits_to_shift), SessionID: int32(k.session_id),
		InitialTermID: int32(k.initial_term_id)}, nil
}

// Images is every image, valid until the next poll or close.
func (s *Subscription) Images() ([]*Image, error) {
	count := s.ImageCount()
	images := make([]*Image, 0, count)
	for i := 0; i < count; i++ {
		image, err := s.ImageAt(i)
		if err != nil {
			return nil, err
		}
		if image != nil {
			images = append(images, image)
		}
	}
	return images, nil
}

// IsConnected is whether a publisher is connected.
func (s *Subscription) IsConnected() bool { return bool(C.cavalry_subscription_is_connected(s.handle)) }

// IsClosed is whether the subscription is closed.
func (s *Subscription) IsClosed() bool {
	return s.handle == nil || bool(C.cavalry_subscription_is_closed(s.handle))
}

// Close closes the subscription and frees its slot in the table. Idempotent. A close
// from inside a handler of this subscription's own poll is refused by the ABI with
// ErrPolling and leaves the subscription open (spec/c-abi.md, "Calls during a claim or a
// poll"): Close then returns the *Error and keeps the handle and the slot.
func (s *Subscription) Close() error {
	if s.handle == nil {
		return nil
	}
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	if result := C.cavalry_subscription_close(s.handle); result != 0 {
		err := lastError("cavalry_subscription_close")
		if err.Code == ErrPolling {
			return err
		}
		s.handle = nil
		unregister(s)
		return err
	}
	s.handle = nil
	unregister(s)
	return nil
}

// CommittedLog is a service's read of a cluster's committed log, beside the member on the
// client's driver: Poll calls the handler for each entry a quorum has committed, in order
// and none at or past the member's commit position, with the entry's position in the log,
// and returns the count, or the ABI's -1 when the poll stopped on a fault, with a
// *HandlerError when a handler call panicked, once the ABI has returned; the entry was
// delivered and is not delivered again. Position is the position after the last entry
// delivered: where a service opens the log again. Err is the last poll that failed.
// IsEnded says when to stop reading and what tells a whole end from a short one.
type CommittedLog struct {
	handle *C.cavalry_committed_log_t
	client *Client
	// state is the log's slot in the subscriptions' table, which the trampoline reaches
	// it by, and its poll's state: the header, the handler and what it panicked with.
	state *Subscription

	MemberID    int32
	RecordingID int64
}

// Poll delivers up to fragmentLimit entries to handler: one ABI call, the count it
// returned (or -1, with Err saying why: a fault, which Fault reads, or ErrPolling for a
// poll from inside a handler of this log, which the ABI refuses and which touches
// nothing here), and a *HandlerError when a handler call panicked. The error it returns
// is a handler's panic alone: a poll that stopped on a fault returns -1 and a nil error,
// so check the count. An Err whose Code is ErrFaultImage or ErrFaultImagesFull is the one
// report that the replay's image failed a check or could not join: the log is ended
// short of its entries, the next poll returns 0 and clears Fault, and Err keeps the
// failure only until the log's next one; Faulted keeps it (IsEnded). An entry whose
// handler call panicked was delivered and is not delivered again: Position is past it,
// and past every entry this poll delivered after it.
// A fragmentLimit below zero is refused before the ABI is called: -1, with an Err whose
// Code is ErrNegative.
func (l *CommittedLog) Poll(handler EntryHandler, fragmentLimit int) (int, error) {
	s := l.state
	if s.polling {
		return int(C.cavalry_go_committed_log_poll(l.handle, C.uintptr_t(s.slot), C.int64_t(fragmentLimit), &s.err)), nil
	}
	s.entry = handler
	s.polling = true
	s.raised = 0
	s.first = nil
	count := int(C.cavalry_go_committed_log_poll(l.handle, C.uintptr_t(s.slot), C.int64_t(fragmentLimit), &s.err))
	s.polling = false
	s.entry = nil
	if s.raised == 0 {
		return count, nil
	}
	first := s.first
	s.first = nil
	return count, &HandlerError{Fragments: count, Raised: s.raised, Cause: first, CommittedLog: true}
}

// Err is the failure of this log's last poll that returned -1, as Subscription.Err: an
// *Error with the ABI function, code and message, kept until the log's next failure; nil
// before the first. Its Code is ErrFaultImage, ErrFaultImagesFull or ErrFaultClosed for a
// poll that stopped on a fault.
func (l *CommittedLog) Err() error { return keptError(&l.state.err) }

// Fault is what the last poll stopped on: the code, the session and the position; the
// code is 0 when the last poll returned a count. A poll that returns a count clears it,
// so read it after a poll that returned -1 and before the next.
func (l *CommittedLog) Fault() (code int32, sessionID int32, position int64, err error) {
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	var f C.cavalry_fault_t
	if err := check("cavalry_committed_log_fault", C.cavalry_committed_log_fault(l.handle, &f)); err != nil {
		return 0, 0, 0, err
	}
	return int32(f.code), int32(f.session_id), int64(f.position), nil
}

// Position is the position after the last entry delivered, or the position the log was
// opened at before the first; -1 from inside a handler of this log's own poll.
func (l *CommittedLog) Position() int64 { return int64(C.cavalry_committed_log_position(l.handle)) }

// CommitPosition is the bound of the last poll: the highest commit position taken from
// the member's counter, 0 before the first poll.
func (l *CommittedLog) CommitPosition() int64 {
	return int64(C.cavalry_committed_log_commit_position(l.handle))
}

// IsEnded is whether this log has come to its end: no poll will deliver another entry.
// Stop reading when it is true, not when a poll returns 0. The log delivered every entry
// it was owed only when its three reports are all false: FailedAt, WithdrawnAt and
// Faulted. In every case what the log delivered ends at Position, where a service opens
// the log again.
func (l *CommittedLog) IsEnded() bool { return bool(C.cavalry_committed_log_is_ended(l.handle)) }

// FailedAt is the position after the last frame the driver's replay of the member's
// recording played before it failed, and true; or 0 and false for a log that is live,
// that ended because its member closed or left the recording, or that ended whole at the
// stop of a recording below the member's live one. No entry from that position on is
// delivered. The entries below it may still be on their way when this first is true, so
// read it once IsEnded is true. They have all been delivered by then
// unless the driver withdrew the replay first, which it does 5 s after a replay ends, one
// that failed included: WithdrawnAt is then true with a position below this one, and the
// entries from there on, those below this position among them, were not delivered
// either. What the log delivered ends at Position.
func (l *CommittedLog) FailedAt() (int64, bool) {
	var position C.int64_t
	if !bool(C.cavalry_committed_log_failed_at(l.handle, &position)) {
		return 0, false
	}
	return int64(position), true
}

// WithdrawnAt is the position at which the entries this log was not given begin, and
// true, when the driver withdrew the replay before the polls had read it to its end; or
// 0 and false. The driver keeps a replay 5 s after it ends and then withdraws it, read
// or not: a service that did not poll for that long after its member closed or left the
// log, or whose handler took that long, ends so, and so does a log on a recording below
// the member's live one while the member's commit position stays below that recording's
// stop for that long. The entries from that position on were played and not delivered.
// A replay that failed is withdrawn in the same way: FailedAt is then true too, and this
// position is the lower. Read it once IsEnded is true.
func (l *CommittedLog) WithdrawnAt() (int64, bool) {
	var position C.int64_t
	if !bool(C.cavalry_committed_log_withdrawn_at(l.handle, &position)) {
		return 0, false
	}
	return int64(position), true
}

// Faulted is the fault of the log's own image, the replay's, and true: the code, the
// image's session and the position; or zeros and false for a log whose image has not
// faulted. The code is ErrFaultImage when a frame of the image failed a check, with the
// reader's position, or when the image was gone before this client read it, with
// NullPosition; it is ErrFaultImagesFull when the image could not join. The poll that
// meets the fault returns -1 once, Fault answers it until the next poll and Err until the
// log's next failure, and this answers it from that poll on, for as long as the log is
// held: a service that did not keep the poll's answer reads it here, once IsEnded is
// true, and tells this end from a whole one. No entry from the fault on was delivered.
// WithdrawnAt is false for such a log, and FailedAt is true only when the driver's
// replay failed as well. From inside a handler of this log's own
// poll the ABI refuses the call, and this is false, a refusal and not an answer.
func (l *CommittedLog) Faulted() (code int32, sessionID int32, position int64, faulted bool) {
	var f C.cavalry_fault_t
	if !bool(C.cavalry_committed_log_faulted(l.handle, &f)) {
		return 0, 0, 0, false
	}
	return int32(f.code), int32(f.session_id), int64(f.position), true
}

// Close stops the replay through the client and frees the handle and the log's slot in
// the table; idempotent. After the client's own Close it frees the handle alone, and the
// driver ends the replay by itself. The handle and the slot are freed whatever the ABI
// answers. When the ABI could not stop the replay, through a client that the driver or a
// timeout closed or within the response timeout, Close returns the *Error after it has
// freed them: the log is closed, a second Close returns nil, and the driver ends the
// replay by itself. The one exception is a close from inside a handler of this log's own
// poll, which the ABI refuses: Close returns the *Error with ErrPolling, and the log
// stays open.
func (l *CommittedLog) Close() error {
	if l.handle == nil {
		return nil
	}
	l.client.mu.Lock()
	defer l.client.mu.Unlock()
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	if result := C.cavalry_committed_log_close(l.handle, l.client.handle); result != 0 {
		err := lastError("cavalry_committed_log_close")
		if err.Code == ErrPolling {
			return err
		}
		l.handle = nil
		unregister(l.state)
		return err
	}
	l.handle = nil
	unregister(l.state)
	return nil
}

// ClusterSession is a client's session with a cluster, made by Client.ConnectCluster
// (spec/cluster.md, "Sessions"): Offer sends the leader a message of one frame and returns
// the position after it or a result code below zero, never an error; Poll calls the
// handler for each reply that is not stale, with the reply's values, and returns the
// count, with a *HandlerError when a handler call panicked, once the ABI has returned.
// Poll is the session's duty cycle too: it sends the keepalive, follows a new leader, and
// closes the session when the cluster says so, so call it at least every 500 ms. A message
// taken is on its way to the leader and not committed, and a reply is no delivery: a
// program that needs an answer offers again when none comes. Err is the last Offer or
// Poll that failed.
type ClusterSession struct {
	handle *C.cavalry_cluster_session_t
	// state is the session's slot in the subscriptions' table, which the trampoline reaches
	// it by, and its poll's state: the handler, the reply's values and what it panicked with.
	state *Subscription

	// SessionID is the id the leader gave the session.
	SessionID int64
	// MaxMessageLength is the longest message Offer takes: what one frame carries.
	MaxMessageLength int
}

// Offer offers the leader message: the position after it on the session's publication to
// the leader, or a code below zero. Err says why a PublicationError: ErrTooLong for a
// message longer than MaxMessageLength, or ErrPolling for an offer from inside this
// session's own handler, which the ABI refuses, so keep what the answer needs and offer
// after the poll has returned. PublicationClosed is a session that is closed, for the
// reason Closed gives, or whose client is. One cgo call, to shim.c's one ABI call.
func (n *ClusterSession) Offer(message []byte) int64 {
	var buffer *C.uint8_t
	if len(message) > 0 {
		buffer = (*C.uint8_t)(unsafe.Pointer(&message[0]))
	}
	return int64(C.cavalry_go_cluster_session_offer(n.handle, buffer, C.size_t(len(message)), &n.state.err))
}

// Poll reads up to fragmentLimit fragments of the response channel and hands handler each
// reply that is not stale: one ABI call, the count of replies it handed over (or -1, with
// Err saying why: a fault, which Fault reads, or ErrPolling for a poll from inside a
// handler of this session, which the ABI refuses and which touches nothing here), and a
// *HandlerError when a handler call panicked. A stale reply, one of a leadership term
// below that of a reply this session has handed over, is passed over and counted in
// StaleReplies, and is not in the count. Every reply the ABI hands over reaches the
// handler, whether or not an earlier call panicked.
// A fragmentLimit below zero is refused before the ABI is called: -1, with an Err whose
// Code is ErrNegative.
func (n *ClusterSession) Poll(handler ReplyHandler, fragmentLimit int) (int, error) {
	s := n.state
	if s.polling {
		return int(C.cavalry_go_cluster_session_poll(n.handle, C.uintptr_t(s.slot), C.int64_t(fragmentLimit), &s.err)), nil
	}
	s.reply = handler
	s.polling = true
	s.raised = 0
	s.first = nil
	count := int(C.cavalry_go_cluster_session_poll(n.handle, C.uintptr_t(s.slot), C.int64_t(fragmentLimit), &s.err))
	s.polling = false
	s.reply = nil
	if s.raised == 0 {
		return count, nil
	}
	first := s.first
	s.first = nil
	return count, &HandlerError{Fragments: count, Raised: s.raised, Cause: first, Cluster: "session"}
}

// Err is the failure of this session's last Offer or Poll that failed, as
// Subscription.Err: an *Error with the ABI function, code and message, kept until the
// session's next failure; nil before the first.
func (n *ClusterSession) Err() error { return keptError(&n.state.err) }

// Fault is what the last poll stopped on: the code, the session and the position; the
// code is 0 when the last poll returned a count.
func (n *ClusterSession) Fault() (code int32, sessionID int32, position int64, err error) {
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	var f C.cavalry_fault_t
	if err := check("cavalry_cluster_session_fault", C.cavalry_cluster_session_fault(n.handle, &f)); err != nil {
		return 0, 0, 0, err
	}
	return int32(f.code), int32(f.session_id), int64(f.position), nil
}

// LeadershipTermID is the leadership term the session sends with: the term of the leader
// that opened it, or of the last new leader event. It is -1 from inside this session's own
// handler, which has each reply's own term.
func (n *ClusterSession) LeadershipTermID() int64 {
	return int64(C.cavalry_cluster_session_leadership_term_id(n.handle))
}

// LeaderMemberID is the id of the member the session sends to: the leader, as the cluster
// last said. It is -1 from inside this session's own handler.
func (n *ClusterSession) LeaderMemberID() int32 {
	return int32(C.cavalry_cluster_session_leader_member_id(n.handle))
}

// StaleReplies is how many stale replies this session's polls have passed over: the value
// of its counter cluster.replies_stale. It is -1 from inside this session's own handler.
func (n *ClusterSession) StaleReplies() int64 {
	return int64(C.cavalry_cluster_session_stale_replies(n.handle))
}

// Closed is why the session is closed, and true: "TIMEOUT" when the leader closed it for
// hearing nothing from it for the session timeout, "NO LEADER" when its leader was lost
// and no member took its place, or the detail of the cluster's closed event. It is "" and
// false while the session is open. A poll is what closes a session, so the answer changes
// only across one. From inside this session's own handler the ABI refuses the call with
// ErrPolling, and this is "" and false, a refusal and not an answer; it is "" and false
// too once Close has freed the session.
func (n *ClusterSession) Closed() (reason string, closed bool) {
	text := C.cavalry_cluster_session_closed(n.handle)
	if text == nil {
		return "", false
	}
	return C.GoString(text), true
}

// Close offers the leader a session close for at most 500 ms, unless the session is closed
// already, then closes the session's publications, its subscription and its counter and
// frees the handle and the session's slot in the table; idempotent. Close a session before
// its client. The handle and the slot are freed whatever the ABI answers, so when the
// ABI's close failed Close returns the *Error after it has freed them. The one exception
// is a close from inside this session's own handler, which the ABI refuses: Close returns
// the *Error with ErrPolling, sends nothing, and the session stays open.
func (n *ClusterSession) Close() error {
	if n.handle == nil {
		return nil
	}
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	if result := C.cavalry_cluster_session_close(n.handle); result != 0 {
		err := lastError("cavalry_cluster_session_close")
		if err.Code == ErrPolling {
			return err
		}
		n.handle = nil
		unregister(n.state)
		return err
	}
	n.handle = nil
	unregister(n.state)
	return nil
}

// ClusterService is a cluster's service beside the member on the client's driver, made by
// Client.ClusterService (spec/cluster.md, "The service" and "Replies"): Poll calls the
// handler for each entry a quorum has committed, in order, with the entry's values and the
// service, and what the handler returns is the entry's action. A handler that panics, or
// returns what is no action, has its entry aborted, and Poll returns a *HandlerError once
// the ABI has returned: an entry is taken only by a handler that finished with it. Reply
// answers a session from inside the handler, and ScheduleTimer and CancelTimer are the
// program's timers there: a timer that is due is handed to the handler as an entry of the
// Type ClusterEntryTimer, once, at one position beside every member. Err is the last Poll,
// Reply, ScheduleTimer or CancelTimer that failed.
type ClusterService struct {
	handle *C.cavalry_cluster_service_t
	client *Client
	// state is the service's slot in the subscriptions' table, which the trampoline reaches
	// it by, and its poll's state: the handler, the entry's values and what it panicked with.
	state *Subscription
}

// Reply sends message, from inside the handler, to the session sessionID, with the
// leadership term id and the cluster's time of the entry being handled, and returns what
// became of the reply, ClusterReply* (CAVALRY_CLUSTER_REPLY_*), never an error.
// ClusterReplySent: it is on its way, or an earlier handling of the same entry sent it.
// ClusterReplyNotAnswering: this service does not answer the entry; its member did not
// lead the entry's term, or the entry is history. ClusterReplyRetry: the session's response
// channel did not take it now; return ActionAbort, and the next poll hands the entry over
// again. ClusterReplyDropped: it is lost, and counted in cluster.replies_dropped.
// ClusterReplyNoSession: no session with that id is open in the log at this entry.
// ClusterReplyTooLong: it is longer than one frame carries, and Err says ErrTooLong. Reply
// first, then change the service's state, since an aborted entry is handed over again.
// From outside the handler it is -1, and Err says ErrClusterNoEntry. One cgo call, to
// shim.c's one ABI call, which allocates nothing and waits for nothing.
func (v *ClusterService) Reply(sessionID int64, message []byte) int32 {
	var buffer *C.uint8_t
	if len(message) > 0 {
		buffer = (*C.uint8_t)(unsafe.Pointer(&message[0]))
	}
	return int32(C.cavalry_go_cluster_service_reply(v.handle, C.int64_t(sessionID), buffer, C.size_t(len(message)),
		&v.state.err))
}

// ScheduleTimer schedules, from inside the handler, the timer correlationID, any int64 the
// program chooses, for deadline in the cluster's time, the clock of an entry's Timestamp, in
// milliseconds, and returns what became of the schedule, never an error.
// ClusterTimerScheduled: the timer is pending with that deadline, a new one or one that was
// pending, whose deadline this sets. ClusterTimerRefused: the service holds its 1,024
// pending timers and the id is none of them, and nothing changed; a program with more
// deadlines than that keeps them in its own state and schedules one timer for the earliest.
// When the deadline has come the handler is handed an entry whose Type is ClusterEntryTimer
// and whose CorrelationID is this id: once, at one position beside every member, never
// before its deadline and never after its cancel. A deadline at or before the entry's time
// is due at once. A handler that returns ActionAbort, or panics, is handed its entry again
// with every timer as it was before that handling. A program schedules its first timer only
// once every member of its cluster, with the service beside it, runs version 2 of the
// cluster protocol, and from that entry on no member is stepped back: a service of version 1
// hands its program no timer, so the two would compute two states from one log. From
// outside the handler it is -1, and Err says ErrClusterNoEntry. One cgo call, to shim.c's
// one ABI call, which allocates nothing and waits for nothing: the service tells its member
// after the handler has taken the entry, in Poll.
func (v *ClusterService) ScheduleTimer(correlationID, deadline int64) int32 {
	return int32(C.cavalry_go_cluster_service_schedule_timer(v.handle, C.int64_t(correlationID), C.int64_t(deadline),
		&v.state.err))
}

// CancelTimer makes, from inside the handler, the timer correlationID pending no more: its
// event, where the log holds one, is handed to no program. A timer that is not pending is
// no error. It returns 0, and -1 from outside the handler, where Err says ErrClusterNoEntry.
// One cgo call, to shim.c's one ABI call, which allocates nothing and waits for nothing.
func (v *ClusterService) CancelTimer(correlationID int64) int32 {
	return int32(C.cavalry_go_cluster_service_cancel_timer(v.handle, C.int64_t(correlationID), &v.state.err))
}

// Poll hands up to fragmentLimit committed entries to handler: one ABI call, the count of
// entries the service took, 0 while the driver, one that runs, hosts no member that has a
// log yet (or -1, with Err saying why: a fault, which Fault reads; ErrClosed for a client
// that its driver closed, as when the member's driver has stopped, which Fault reads too
// and every poll after it answers again, so that a program's loop ends on it; or, with Fault
// left as it was, ErrNull for a poll after the program's own Client.Close, which freed the
// client, and ErrPolling for a poll from inside a handler of this service, which the ABI
// refuses and which touches nothing here). A timer event whose timer is not pending, or
// not yet due, is taken and handed to no handler, so the count may be more than the handler
// was called. The poll is the service's duty cycle: it opens the member's log, follows the
// log's sessions, and holds a publication to each open session while the service answers.
// Those are commands sent with the service's client, at a session's opening, where a term's
// leader changes and at a recording's end, and the poll waits for the driver's answer to
// each; none is sent for a session message. Six more are sent once in a service's life, at
// the most: its five counters and its publication to its member, in the poll in which the
// program first schedules a timer or the service first passes a timer event over, after
// that poll's entries, where a snapshot has not added them already; that poll returns -1
// with the driver's refusal of one of them, after the entries its handler took are taken.
// After that the poll tells the member what each taken entry changed, and every pending
// timer again each second, by offers that wait for nothing. The poll takes no lock, and it is a call on the client: no
// other goroutine uses the service's client (Client.ClusterService). When the handler
// panicked, or returned what is no action, its entry was aborted and Poll returns a
// *HandlerError: its Fragments is the count of entries taken before that entry, and its
// Cause the panic's value, or the *Error with ErrClusterAction that the ABI gave for the
// value the handler returned. The next poll hands the aborted entry over again.
// A fragmentLimit below zero is refused before the ABI is called: -1, with an Err whose
// Code is ErrNegative.
func (v *ClusterService) Poll(handler ServiceHandler, fragmentLimit int) (int, error) {
	s := v.state
	if s.polling {
		return int(C.cavalry_go_cluster_service_poll(v.handle, v.client.handle, C.uintptr_t(s.slot),
			C.int64_t(fragmentLimit), &s.aborted, &s.err)), nil
	}
	s.service = handler
	s.polling = true
	s.raised = 0
	s.first = nil
	count := int(C.cavalry_go_cluster_service_poll(v.handle, v.client.handle, C.uintptr_t(s.slot),
		C.int64_t(fragmentLimit), &s.aborted, &s.err))
	s.polling = false
	s.service = nil
	if s.raised == 0 && s.aborted == 0 {
		return count, nil
	}
	cause := s.first
	s.first = nil
	if s.raised == 0 {
		// The ABI refused what the handler returned: shim.c kept the ABI's error in the call
		// that failed, and aborted the entry.
		cause = keptError(&s.err)
	}
	return count, &HandlerError{Fragments: count, Raised: 1, Cause: cause, Cluster: "service"}
}

// Err is the failure of this service's last Poll, Reply, ScheduleTimer or CancelTimer that
// failed, as Subscription.Err: an *Error with the ABI function, code and message, kept
// until the service's next failure; nil before the first.
func (v *ClusterService) Err() error { return keptError(&v.state.err) }

// Fault is what the last poll stopped on: the code, the session and the position; the
// code is 0 when the last poll returned a count. The code is a fault's, ErrClusterEntry
// with the position of a committed entry that is no entry of this version's cluster
// protocol, or the client's or the driver's.
func (v *ClusterService) Fault() (code int32, sessionID int32, position int64, err error) {
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	var f C.cavalry_fault_t
	if err := check("cavalry_cluster_service_fault", C.cavalry_cluster_service_fault(v.handle, &f)); err != nil {
		return 0, 0, 0, err
	}
	return int32(f.code), int32(f.session_id), int64(f.position), nil
}

// Position is the position after the last entry the handler took, 0 before the first: how
// far the service has applied the log. It is -1 from inside this service's own handler,
// which has each entry's own position; the poll moves this one when it returns.
func (v *ClusterService) Position() int64 { return int64(C.cavalry_cluster_service_position(v.handle)) }

// Answers is whether the service answered the last entry it read: its member led that
// entry's term and, as the entry was read, led it still, and the entry was not history.
// It is false from inside this service's own handler, a refusal and not an answer: there,
// what Reply returns says whether the service answers the entry.
func (v *ClusterService) Answers() bool { return bool(C.cavalry_cluster_service_answers(v.handle)) }

// Sessions is how many sessions the log holds open, as far as the service has read: 10 at
// most. It is -1 from inside this service's own handler.
func (v *ClusterService) Sessions() int { return int(C.cavalry_cluster_service_sessions(v.handle)) }

// SnapshotDue is the log position of the snapshot the leader asked for, and true, when the
// last entry of the last poll was ClusterEntrySnapshot and the handler took it: the
// position after that entry, which Position is then. It is 0 and false otherwise, and from
// the next poll on. It is false too from inside this service's own handler, where the ABI
// refuses the read with ErrPolling: the entry's Type says that the poll ends after it
// (spec/c-abi.md, "A service's snapshot and its start"; ADR 0149).
func (v *ClusterService) SnapshotDue() (position int64, due bool) {
	var at C.int64_t
	if !bool(C.cavalry_cluster_service_snapshot_due(v.handle, &at)) {
		return 0, false
	}
	return int64(at), true
}

// Snapshot takes a snapshot of the program's state: the program's own bytes, in which the
// service reads nothing, 1 GiB at the most. Call it after a poll that ended at the
// leader's request, while SnapshotDue gives a position, and before the next poll. The
// service writes a recording through its member's driver and tells the member, which
// lists the snapshot; the call waits for the driver, a second at the most for each step
// of its own and the response timeout, 10 s, for an add or a command that a driver does not answer,
// under the client's lock. It is true when the snapshot was written and the member was
// told, and false when nothing was written because the member lists a snapshot of a
// service at or past this position already. The *Error has ErrClusterNoSnapshotDue where
// none is due, ErrClusterSnapshotTooLong for a state over 1 GiB, ErrClusterSnapshotFailed
// when a step made no progress, with the step in the message, ErrPolling from inside a
// handler of this service or of one whose poll holds the client, or the client's or the
// driver's code. After an error the service is as it was and the snapshot is still due:
// call again, or poll on.
func (v *ClusterService) Snapshot(state []byte) (bool, error) {
	v.client.mu.Lock()
	defer v.client.mu.Unlock()
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	var buffer *C.uint8_t
	if len(state) > 0 {
		buffer = (*C.uint8_t)(unsafe.Pointer(&state[0]))
	}
	result := C.cavalry_cluster_service_snapshot(v.handle, v.client.handle, buffer, C.size_t(len(state)))
	if result < 0 {
		return false, lastError("cavalry_cluster_service_snapshot")
	}
	return result == 1, nil
}

// Restore starts the service from the newest snapshot of a service that its member lists,
// before the service's first poll. restored is one of ClusterRestored*. With
// ClusterRestoredState, state is the program's state as the program gave it to Snapshot, a
// copy the caller owns, never nil; the service holds the sessions that were open at the
// snapshot's position, Position is that position, and the next poll hands over the entry
// after it. With ClusterRestoredFromStart, state is nil: the member lists no snapshot of a
// service, or the one it lists could not be taken, and the polls read the log from
// position 0. With ClusterRestoredNoMember, state is nil: the driver hosts no member that
// has a log yet, and the program calls again before its first poll. A program that never
// calls it reads the log from position 0 at every start. The call waits for the driver to
// play the snapshot's recording back, under the client's lock. The *Error has
// ErrClusterRestoreTooLate once a poll has opened the member's log or a call has answered
// a state or the log's start, ErrPolling from inside a handler, or the client's or the
// driver's code, after which the service has not started and the program calls again.
func (v *ClusterService) Restore() (restored int32, state []byte, err error) {
	v.client.mu.Lock()
	defer v.client.mu.Unlock()
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	var data *C.uint8_t
	var length C.size_t
	result := C.cavalry_cluster_service_restore(v.handle, v.client.handle, &data, &length)
	if result <= 0 {
		return 0, nil, lastError("cavalry_cluster_service_restore")
	}
	if result != ClusterRestoredState {
		return int32(result), nil, nil
	}
	// The bytes are the handle's until the service's next poll or its close
	// (spec/c-abi.md, "Lifetimes"): copied here, under the client's lock.
	state = make([]byte, int(length))
	if length > 0 {
		copy(state, unsafe.Slice((*byte)(unsafe.Pointer(data)), int(length)))
	}
	return int32(result), state, nil
}

// Close closes the service's publications and its counter, stops its read of the log and
// frees the handle and the service's slot in the table; idempotent. Close a service before
// its client; after the client's own Close it frees the handle alone, and the driver ends
// what the service held by itself. The handle and the slot are freed whatever the ABI
// answers, so when the ABI's close failed Close returns the *Error after it has freed
// them. The one exception is a close from inside this service's own handler, or from
// inside the handler of another service whose poll holds the client, which the ABI
// refuses: Close returns the *Error with ErrPolling, and the service stays open.
func (v *ClusterService) Close() error {
	if v.handle == nil {
		return nil
	}
	v.client.mu.Lock()
	defer v.client.mu.Unlock()
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	if result := C.cavalry_cluster_service_close(v.handle, v.client.handle); result != 0 {
		err := lastError("cavalry_cluster_service_close")
		if err.Code == ErrPolling {
			return err
		}
		v.handle = nil
		unregister(v.state)
		return err
	}
	v.handle = nil
	unregister(v.state)
	return nil
}

// Recording is a recording the driver makes; Close frees the handle and stops nothing.
type Recording struct {
	handle *C.cavalry_recording_t

	Channel     string
	RecordingID int64
	SessionID   int32
	StreamID    int32
}

func newRecording(handle *C.cavalry_recording_t) (*Recording, error) {
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	var k C.cavalry_recording_constants_t
	if err := check("cavalry_recording_constants", C.cavalry_recording_constants(handle, &k)); err != nil {
		C.cavalry_recording_close(handle)
		return nil, err
	}
	return &Recording{handle: handle, Channel: C.GoString(k.channel), RecordingID: int64(k.recording_id),
		SessionID: int32(k.session_id), StreamID: int32(k.stream_id)}, nil
}

// Position is the recording's position.
func (r *Recording) Position() int64 { return int64(C.cavalry_recording_position(r.handle)) }

// SyncedPosition is the position the recording has synced to disk.
func (r *Recording) SyncedPosition() int64 {
	return int64(C.cavalry_recording_synced_position(r.handle))
}

// Close frees the handle; the recording goes on until StopRecording.
func (r *Recording) Close() error {
	if r.handle == nil {
		return nil
	}
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	err := check("cavalry_recording_close", C.cavalry_recording_close(r.handle))
	r.handle = nil
	return err
}

// Replay is a replay the driver plays.
type Replay struct {
	handle *C.cavalry_replay_t

	Channel         string
	ReplaySessionID int64
	RecordingID     int64
	StartPosition   int64
	SessionID       int32
	StreamID        int32
}

func newReplay(handle *C.cavalry_replay_t) (*Replay, error) {
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	var k C.cavalry_replay_constants_t
	if err := check("cavalry_replay_constants", C.cavalry_replay_constants(handle, &k)); err != nil {
		C.cavalry_replay_close(handle)
		return nil, err
	}
	return &Replay{handle: handle, Channel: C.GoString(k.channel), ReplaySessionID: int64(k.replay_session_id),
		RecordingID: int64(k.recording_id), StartPosition: int64(k.start_position), SessionID: int32(k.session_id),
		StreamID: int32(k.stream_id)}, nil
}

// Position is the replay's position.
func (r *Replay) Position() int64 { return int64(C.cavalry_replay_position(r.handle)) }

// Ended is whether the replay has ended, at which position and why (ReplayStop*).
func (r *Replay) Ended() (ended bool, position int64, reason int32) {
	var cposition C.int64_t
	var creason C.int32_t
	ended = bool(C.cavalry_replay_ended(r.handle, &cposition, &creason))
	return ended, int64(cposition), int32(creason)
}

// Close frees the handle; the replay goes on until StopReplay or its end.
func (r *Replay) Close() error {
	if r.handle == nil {
		return nil
	}
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	err := check("cavalry_replay_close", C.cavalry_replay_close(r.handle))
	r.handle = nil
	return err
}
'''
