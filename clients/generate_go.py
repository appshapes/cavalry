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
`Publication`, `Subscription`, `CommittedLog`, `Header`, `Image`, `Recording` and `Replay`;
`cavalry_test.go`, the package's tests, which need no driver: the subscription table at
its bound and the trampoline's refusal of a slot the table does not hold;
`trampoline.go`, the exported Go function the C trampoline calls per fragment (a file
with an `//export` may hold no C definition in its preamble, so it is a file of its own);
`shim.h` and `shim.c`, the C trampoline, the one `cavalry_subscription_poll` call, the one
`cavalry_committed_log_poll` call (ADR 0068) and the per-message calls that keep their error. Python 3.9 standard library only.
"""

from __future__ import annotations

import re

MODULE = "github.com/appshapes/cavalry/clients/go"
INITIALISMS = {"ABI": "ABI", "IO": "IO", "EOS": "EOS", "UTF8": "UTF8", "ID": "ID"}
C_LIMITS = {"INT64_MAX": "math.MaxInt64", "INT64_MIN": "math.MinInt64", "INT32_MAX": "math.MaxInt32",
            "INT32_MIN": "math.MinInt32", "UINT8_MAX": "math.MaxUint8", "UINT16_MAX": "math.MaxUint16"}
# The per-message functions of the generated Go, which clients/lint.py reads (clients/README.md).
PER_MESSAGE_GO = ("Publication.TryClaim", "Publication.SetReservedValue", "Publication.Commit", "Publication.Abort",
                  "Publication.Offer", "Subscription.Poll", "Subscription.deliver", "CommittedLog.Poll", "Header.Values",
                  "cavalryGoFragment")
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
 * subscription's table index as clientd, and each per-message call with the thread's
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

/* cavalry_publication_try_claim, with `error` written when it returns
 * CAVALRY_PUBLICATION_ERROR. */
int64_t cavalry_go_try_claim(cavalry_publication_t *publication, size_t length, cavalry_buffer_claim_t *claim,
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
int cavalry_go_poll(cavalry_subscription_t *subscription, uintptr_t index, size_t fragment_limit,
                    cavalry_go_error_t *error);

/* Polls the committed log `log` for up to `fragment_limit` entries, as cavalry_go_poll
 * polls a subscription: the same trampoline, with `index` the log's slot in the client's
 * table. The ABI's result: the count, or -1, with `error` written. */
int cavalry_go_committed_log_poll(cavalry_committed_log_t *log, uintptr_t index, size_t fragment_limit,
                                  cavalry_go_error_t *error);

#endif
'''

SHIM_C = r'''/* The C trampoline the ABI calls per fragment, the poll that installs it
 * (clients/go/README.md, "The call-back, by construction"), and the per-message calls
 * that copy the thread's error when they fail ("The thread's error"). Generated by
 * clients/generate.py go from crates/cavalry-ffi/include/cavalry.h; do not edit by hand,
 * `just lint` regenerates and compares, and clients/lint.py reads every function for C's
 * forms of a lock. */
#include "shim.h"

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

int64_t cavalry_go_try_claim(cavalry_publication_t *publication, size_t length, cavalry_buffer_claim_t *claim,
                             cavalry_go_error_t *error) {
  int64_t position = cavalry_publication_try_claim(publication, length, claim);
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

int cavalry_go_poll(cavalry_subscription_t *subscription, uintptr_t index, size_t fragment_limit,
                    cavalry_go_error_t *error) {
  int count = cavalry_subscription_poll(subscription, cavalry_go_trampoline, (void *) index, fragment_limit);
  if (count < 0) {
    cavalry_go_keep(error, "cavalry_subscription_poll");
  }
  return count;
}

int cavalry_go_committed_log_poll(cavalry_committed_log_t *log, uintptr_t index, size_t fragment_limit,
                                  cavalry_go_error_t *error) {
  int count = cavalry_committed_log_poll(log, cavalry_go_trampoline, (void *) index, fragment_limit);
  if (count < 0) {
    cavalry_go_keep(error, "cavalry_committed_log_poll");
  }
  return count;
}
'''

GO_TEST = r'''package cavalry

// The package's own tests, which conformance/go/run.sh runs with `go test ./...` beside
// `go vet`, with no driver: the subscription table at its bound (CLAUDE.md, invariant 3)
// and the trampoline's refusal of a slot the table does not hold. Generated by
// clients/generate.py go; do not edit by hand, `just lint` regenerates and compares.

import (
	"errors"
	"fmt"
	"strings"
	"testing"
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
'''

GO_TRAMPOLINE = r'''package cavalry

// The exported Go function the C trampoline of shim.c calls per fragment. It is in a
// file of its own because a file with an //export directive may hold only declarations
// in its preamble, and cavalry.go's preamble holds the build flags. Generated by
// clients/generate.py go; do not edit by hand, `just lint` regenerates and compares.

/*
#include <stddef.h>
#include <stdint.h>

#include "cavalry.h"
*/
import "C"

import "unsafe"

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
	if slot == 0 || slot > @MAX_SUBSCRIPTIONS@ {
		return
	}
	s := subscriptions[slot-1].Load()
	if s == nil {
		return
	}
	body := unsafe.Slice((*byte)(unsafe.Pointer(buffer)), int(length))
	s.header.ptr = header
	s.header.read = false
	s.deliver(body)
	s.header.ptr = nil
}
'''

GO_CLIENT = r'''// Package cavalry is the Go client over the C ABI of spec/c-abi.md (clients/go/README.md).
//
// Generated by clients/generate.py go; do not edit by hand, `just lint` regenerates and
// compares. A Client wraps a driver's directory; its Publication, Subscription, Recording,
// Replay and CommittedLog wrap the ABI's handles through cgo. Every message goes through one ABI call
// on the caller's goroutine: TryClaim, Offer, Commit, Abort, SetReservedValue, Poll and
// Values hold invariant 2's rules, no lock, no goroutine, no channel, no allocation, no
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

// HandlerError is the fragment handler panicked during a poll: Fragments is the count
// the poll returned once the ABI was done, Raised how many handler calls panicked, and
// Cause the first panic's value, kept as it was panicked.
type HandlerError struct {
	Fragments int
	Raised    int
	Cause     any
}

func (e *HandlerError) Error() string {
	delivered := fmt.Sprintf("the %d fragment(s) this poll delivered", e.Fragments)
	if e.Fragments < 0 {
		delivered = "the fragments this poll delivered before it stopped on a fault, which Subscription.Fault reads"
	}
	return fmt.Sprintf("the fragment handler panicked on %d of %s; every fragment reached the handler once, in "+
		"order. Fix the handler: its first panic's value, %v, is this error's Cause; polling again does not deliver "+
		"these fragments again. See clients/go/README.md#the-panic", e.Raised, delivered, e.Cause)
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

// The table the trampoline reads subscriptions from: C must not hold a Go pointer, so
// the ABI is given a subscription's 1-based index as the poll's clientd. The table is
// bounded (CLAUDE.md, invariant 3): a process holds at most maxSubscriptions open
// subscriptions through this package, and AddSubscription returns an *Error with
// ErrSubscriptionsFull above that. Slots are taken and freed under registryMu, on the
// control plane; the trampoline reads a slot with one atomic load.
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
		"this process holds %d open subscriptions and committed logs through the Go client, the table's bound; "+
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

// Registered is how many subscriptions the table holds: every open one, and none once
// each is closed. The runner reads it to check that a close frees the slot.
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
// Publication, Subscription, Recording and Replay is freed by its own Close and by
// nothing else, and a publication or a subscription answers as closed until then
// (spec/c-abi.md, "Lifetimes"). An unclosed Subscription keeps its slot in the table of
// @MAX_SUBSCRIPTIONS@, so a program that closes only its clients fills the table and
// AddSubscription returns ErrSubscriptionsFull. Idempotent; the handle is forgotten
// whatever the ABI returns.
func (c *Client) Close() error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.handle == nil {
		return nil
	}
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	err := check("cavalry_close", C.cavalry_close(c.handle))
	c.handle = nil
	return err
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
// unsafe.Slice and allocates nothing.
func (p *Publication) TryClaim(length int) int64 {
	position := int64(C.cavalry_go_try_claim(p.handle, C.size_t(length), &p.claim, &p.err))
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
func (s *Subscription) Poll(handler FragmentHandler, fragmentLimit int) (int, error) {
	if s.polling {
		return int(C.cavalry_go_poll(s.handle, C.uintptr_t(s.slot), C.size_t(fragmentLimit), &s.err)), nil
	}
	s.handler = handler
	s.polling = true
	s.raised = 0
	s.first = nil
	count := int(C.cavalry_go_poll(s.handle, C.uintptr_t(s.slot), C.size_t(fragmentLimit), &s.err))
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
// kept and every one counted, so the poll goes on to its next fragment. A committed log's
// entry goes to its entry handler with its position, which the ABI gives for the header
// its own poll handed over.
func (s *Subscription) deliver(body []byte) {
	defer func() {
		if r := recover(); r != nil {
			s.raised++
			if s.raised == 1 {
				s.first = r
			}
		}
	}()
	if s.entry != nil {
		s.entry(body, &s.header, int64(C.cavalry_committed_log_entry_position(s.header.ptr)))
		return
	}
	s.handler(body, &s.header)
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

// ImageAt is the image at index, with its constants copied now, or nil.
func (s *Subscription) ImageAt(index int) (*Image, error) {
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
// and returns the count, with a *HandlerError when a handler call panicked, once the ABI
// has returned; the entry was delivered and is not delivered again. Position is the
// position after the last entry delivered: where a service opens the log again. Err is
// the last poll that failed.
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
// nothing here), and a *HandlerError when a handler call panicked.
func (l *CommittedLog) Poll(handler EntryHandler, fragmentLimit int) (int, error) {
	s := l.state
	if s.polling {
		return int(C.cavalry_go_committed_log_poll(l.handle, C.uintptr_t(s.slot), C.size_t(fragmentLimit), &s.err)), nil
	}
	s.entry = handler
	s.polling = true
	s.raised = 0
	s.first = nil
	count := int(C.cavalry_go_committed_log_poll(l.handle, C.uintptr_t(s.slot), C.size_t(fragmentLimit), &s.err))
	s.polling = false
	s.entry = nil
	if s.raised == 0 {
		return count, nil
	}
	first := s.first
	s.first = nil
	return count, &HandlerError{Fragments: count, Raised: s.raised, Cause: first}
}

// Err is the failure of this log's last poll that returned -1, as Subscription.Err.
func (l *CommittedLog) Err() error { return keptError(&l.state.err) }

// Fault is what the last poll stopped on: the code (0 for none), the session and the
// position.
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

// IsEnded is whether the member closed or left its log: once a poll then returns 0,
// close this log and open another from Position.
func (l *CommittedLog) IsEnded() bool { return bool(C.cavalry_committed_log_is_ended(l.handle)) }

// FailedAt is the position after the last frame the driver's replay of the member's
// recording played before it failed, and true; or 0 and false for a log that is live or
// that ended because its member closed or left the recording.
func (l *CommittedLog) FailedAt() (int64, bool) {
	var position C.int64_t
	if !bool(C.cavalry_committed_log_failed_at(l.handle, &position)) {
		return 0, false
	}
	return int64(position), true
}

// Close stops the replay through the client and frees the handle and the log's slot in
// the table; idempotent. After the client's own Close it frees the handle alone, and the
// driver ends the replay by itself. From inside a handler of this log's own poll the ABI
// refuses the close with ErrPolling, and the log stays open.
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
