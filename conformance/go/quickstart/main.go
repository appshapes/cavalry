// The Go client's conformance runner (clients/README.md; ADR 0046, card 207): the
// ten-minute path's shape through clients/go, as conformance/python/quickstart.py runs it
// through clients/python. It checks that a package of another ABI version is refused by
// the check NewClient makes first, CheckABIVersion, which names both numbers. Against a
// driver at CAVALRY_DIR it checks the ABI version both ways, records stream 1001 on
// cavalry:ipc, publishes 1,000 messages of 64 bytes by claim and commit, each carrying its
// index and a pattern, and reads them back through the fragment handler once and in order,
// with the header's type, flags, version, frame length, term offset and session id checked
// against spec/frame.md; waits for the recording to sync to the stream's end and stops it;
// checks where a handler's panic goes, in both orders of an error and a string, that a
// header kept past its call reads as invalid and a committed claim's slice is nil, and the
// refusals of spec/c-abi.md, "Calls during a claim or a poll", a close from inside the
// handler included, with each per-message refusal's error in its handle's Err and a refused
// add's in its *Error, on stream 1003 (edges); checks that a reserved value beyond 2^53 reaches
// the handler exactly on stream 1005 (reservedValues); checks that a closed subscription's
// slot in the table is freed (stateFreed); checks that eight goroutines may share the
// Client, on streams 1101 to 1108 (sharedClient); does the same 1,000 over cavalry:udp on the
// loopback; and replays the recording onto stream 2001, each frame at the position the
// recording holds it at, with the end reported. Every wait is bounded at 10 s, and a wait
// that runs out says how far it got. It exits 1 with the function, the code and
// cavalry_errmsg when a call failed, or with the code's name when a result code did.
//
// Then the report, printed and never gated, from two rounds that check nothing else. The
// latency round (latency): 100,000 messages over cavalry:ipc on stream 1004, one at a time
// on this goroutine, each carrying the publisher's monotonic clock from before its commit,
// which the handler subtracts from the clock it reads first; the one-way p50, p99, p99.99
// and max, nearest rank, so ten samples lie above the p99.99. The allocation round
// (allocations): 10,000 messages on stream 1002 whose handler only counts and whose
// publisher copies one constant, with runtime.ReadMemStats read before and after the round
// with nothing between, which counts every allocation Go's heap made: the allocations and
// bytes per message on this side of the boundary, the client's code and the runtime's
// together. Go's standard library only (ADR 0003).
package main

import (
	"encoding/binary"
	"errors"
	"fmt"
	"math"
	"net"
	"os"
	"runtime"
	"slices"
	"strings"
	"sync"
	"time"

	cavalry "github.com/appshapes/cavalry/clients/go"
)

const (
	messages           = 1000
	latencyMessages    = 100000
	allocationMessages = 10000
	bodyLength         = 64
	stream             = 1001
	replayStream       = 2001
	wait               = 10 * time.Second
)

var codeNames = map[int64]string{
	cavalry.PublicationNotConnected: "NOT_CONNECTED", cavalry.PublicationBackPressured: "BACK_PRESSURED",
	cavalry.PublicationAdminAction: "ADMIN_ACTION", cavalry.PublicationClosed: "CLOSED",
	cavalry.PublicationMaxPositionExceeded: "MAX_POSITION_EXCEEDED", cavalry.PublicationError: "ERROR",
}

func fail(format string, args ...any) {
	fmt.Fprintf(os.Stderr, "conformance/go: "+format+"\n", args...)
	os.Exit(1)
}

func say(format string, args ...any) {
	fmt.Printf("conformance/go: "+format+"\n", args...)
}

func codeName(code int64) string {
	if name, ok := codeNames[code]; ok {
		return name
	}
	return fmt.Sprint(code)
}

func deadline() time.Time {
	return time.Now().Add(wait)
}

func failed(err error) {
	var abi *cavalry.Error
	if errors.As(err, &abi) {
		fail("%s failed with error %d: %s", abi.Fn, abi.Code, abi.Message)
	}
	fail("%v", err)
}

func must[T any](value T, err error) T {
	if err != nil {
		failed(err)
	}
	return value
}

func mustOK(err error) {
	if err != nil {
		failed(err)
	}
}

// receiver is what a subscription has received: the next index expected, and the
// frames' term ids and offsets kept from the handler for the positions.
type receiver struct {
	name         string
	subscription *cavalry.Subscription
	next         int
	sessionID    int32
	haveSession  bool
	termIDs      []int32
	termOffsets  []int32
	positions    []int64
	constants    *cavalry.Image
}

func (r *receiver) onFragment(body []byte, header *cavalry.Header) {
	if r.next >= messages {
		fail("%s: a fragment after all %d messages", r.name, messages)
	}
	index := int(binary.LittleEndian.Uint64(body))
	if index != r.next {
		fail("%s: expected message %d, got %d", r.name, r.next, index)
	}
	if len(body) != bodyLength {
		fail("%s: message %d has %d bytes, not %d", r.name, index, len(body), bodyLength)
	}
	for b := 8; b < bodyLength; b++ {
		if body[b] != byte(index+b) {
			fail("%s: message %d byte %d is %d, not %d", r.name, index, b, body[b], byte(index+b))
		}
	}
	frame, ok := header.Values()
	if !ok {
		fail("%s: cavalry_header_values refused message %d's header inside the handler", r.name, index)
	}
	// The header against spec/frame.md: a data frame, unfragmented, version 0, the body
	// plus the 32-byte header, aligned to 32 in its term.
	if frame.Type != cavalry.FrameTypeData || frame.Flags != cavalry.FlagsUnfragmented || frame.Version != 0 ||
		frame.FrameLength != 32+bodyLength || frame.TermOffset%32 != 0 {
		fail("%s: message %d's header reads type %d, flags %d, version %d, frame length %d, term offset %d",
			r.name, index, frame.Type, frame.Flags, frame.Version, frame.FrameLength, frame.TermOffset)
	}
	if !r.haveSession {
		r.sessionID = frame.SessionID
		r.haveSession = true
	} else if frame.SessionID != r.sessionID {
		fail("%s: message %d from session %d, the stream's is %d", r.name, index, frame.SessionID, r.sessionID)
	}
	r.termIDs = append(r.termIDs, frame.TermID)
	r.termOffsets = append(r.termOffsets, frame.TermOffset)
	r.next++
}

// takeConstants keeps the constants of the image the frames came from, once a poll has
// read one, for the positions (spec/c-abi.md, "A frame's position").
func (r *receiver) takeConstants() {
	if r.constants != nil || !r.haveSession {
		return
	}
	for _, image := range must(r.subscription.Images()) {
		if image.SessionID == r.sessionID {
			r.constants = image
			return
		}
	}
}

func (r *receiver) computePositions() {
	if r.constants == nil {
		fail("%s: no image carried session %d after %d messages", r.name, r.sessionID, r.next)
	}
	c := r.constants
	for i := range r.termIDs {
		termCount := int64(r.termIDs[i] - c.InitialTermID) // 32-bit two's complement
		r.positions = append(r.positions, termCount<<c.PositionBitsToShift+int64(r.termOffsets[i]))
	}
}

func (r *receiver) expectNoMore() {
	count := must(r.subscription.Poll(r.onFragment, 16))
	if count != 0 {
		fail("%s: %d more fragment(s) after all %d messages", r.name, count, messages)
	}
}

// codeOf is the ABI's code of err, an *Error, or 0.
func codeOf(err error) int32 {
	var abi *cavalry.Error
	if errors.As(err, &abi) {
		return abi.Code
	}
	return 0
}

// refused handles a claim refused with position, below zero: fail at once on a code that
// does not pass, ERROR with the publication's Err, CLOSED or MAX_POSITION_EXCEEDED, or once
// the round's 10 s have passed, saying how far the round got; else return, to claim again.
func refused(name string, publication *cavalry.Publication, position int64, sent int, until time.Time, total int) {
	if position == cavalry.PublicationError {
		fail("%s: TryClaim returned ERROR: %v", name, publication.Err())
	}
	if position == cavalry.PublicationClosed || position == cavalry.PublicationMaxPositionExceeded {
		fail("%s: TryClaim returned %s (%d) after %d of %d messages", name, codeName(position), position, sent, total)
	}
	if time.Now().After(until) {
		fail("%s: published %d of %d in %v; the last claim returned %s (%d)", name, sent, total, wait, codeName(position), position)
	}
}

// pollOrFail is one poll of up to 16 fragments: the count, or a failure with the
// subscription's fault or the handler's panic, saying how far the round got.
func pollOrFail(name string, subscription *cavalry.Subscription, handler cavalry.FragmentHandler, received int, total int) int {
	count, err := subscription.Poll(handler, 16)
	if err != nil {
		fail("%s: the fragment handler panicked after %d of %d messages: %v", name, received, total, err)
	}
	if count < 0 {
		code, sessionID, position := must3(subscription.Fault())
		fail("%s: poll stopped on fault %d of session %d at position %d after %d of %d messages: %v",
			name, code, sessionID, position, received, total, subscription.Err())
	}
	return count
}

func must3(code int32, sessionID int32, position int64, err error) (int32, int32, int64) {
	if err != nil {
		failed(err)
	}
	return code, sessionID, position
}

func writeMessage(claim []byte, i int) {
	binary.LittleEndian.PutUint64(claim, uint64(i))
	for b := 8; b < bodyLength; b++ {
		claim[b] = byte(i + b)
	}
}

func publishAndReceive(publication *cavalry.Publication, r *receiver) {
	until := deadline()
	for i := 0; i < messages; {
		// The image joins on a poll; take its constants once a frame from it is read.
		pollOrFail(r.name, r.subscription, r.onFragment, r.next, messages)
		r.takeConstants()
		position := publication.TryClaim(bodyLength)
		if position < 0 {
			refused(r.name, publication, position, i, until, messages)
			continue
		}
		writeMessage(publication.Claim, i)
		if publication.Commit() != position {
			fail("%s: commit did not return the claim's position %d", r.name, position)
		}
		i++
	}
	until = deadline()
	for r.next < messages {
		pollOrFail(r.name, r.subscription, r.onFragment, r.next, messages)
		r.takeConstants()
		if time.Now().After(until) {
			fail("%s: received %d of %d in %v", r.name, r.next, messages, wait)
		}
	}
	r.expectNoMore()
	r.computePositions()
}

// offerOrFail offers message, retrying a refusal that passes, NOT_CONNECTED,
// BACK_PRESSURED or ADMIN_ACTION, for up to 10 s; the position.
func offerOrFail(name string, publication *cavalry.Publication, message []byte, sent int) int64 {
	until := deadline()
	for {
		position := publication.Offer(message)
		if position >= 0 {
			return position
		}
		if position == cavalry.PublicationClosed || position == cavalry.PublicationMaxPositionExceeded ||
			position == cavalry.PublicationError || time.Now().After(until) {
			fail("%s: Offer returned %s (%d) after %d messages", name, codeName(position), position, sent)
		}
	}
}

func message(i int) []byte {
	m := make([]byte, bodyLength)
	binary.LittleEndian.PutUint64(m, uint64(i))
	return m
}

func indexOf(body []byte) int {
	return int(binary.LittleEndian.Uint64(body))
}

func claimOrFail(name string, publication *cavalry.Publication, sent int) int64 {
	until := deadline()
	for {
		position := publication.TryClaim(bodyLength)
		if position >= 0 {
			return position
		}
		refused(name, publication, position, sent, until, sent+1)
	}
}

// edges checks the client's edges, on stream 1003. Where a handler's panic goes
// (clients/go/README.md, "The panic"): ten messages offered, then one poll of ten whose
// handler panics with an error on message 3. The ABI's poll consumes all ten, so every one
// must reach the handler, once and in order, and Poll must return a *HandlerError with
// Fragments 10, Raised 1 and the error as its Cause. Then a poll whose handler panics with
// a string on one message and an error on the next: Raised 2 and the string, the first, as
// the Cause. Then the lifetimes: a committed claim's slice is nil, and a header kept past
// its handler call reads as invalid. Then the refusals of spec/c-abi.md, "Calls during a
// claim or a poll", which leave the wrapper as they found it: a poll from inside a handler
// of its own subscription, a second claim with one outstanding, a close with a claim
// outstanding, and a close from inside the handler, which keeps the handle.
func edges(client *cavalry.Client) {
	name := "handler panics"
	subscription := must(client.AddSubscription("cavalry:ipc", stream+2))
	publication := must(client.AddPublication("cavalry:ipc", stream+2))
	var seen []int
	unexpected := func(body []byte, header *cavalry.Header) {
		fail("%s: a fragment the test did not expect, after %v", name, seen)
	}
	if publication.Err() != nil || subscription.Err() != nil {
		fail("%s: a new publication's Err is %v and a new subscription's %v; expected nil before any failure", name,
			publication.Err(), subscription.Err())
	}

	until := deadline()
	for {
		must(subscription.Poll(unexpected, 10)) // nothing is offered yet; the image joins on a poll
		position := publication.Offer(message(0))
		if position >= 0 {
			break
		}
		if position == cavalry.PublicationClosed || position == cavalry.PublicationMaxPositionExceeded ||
			position == cavalry.PublicationError || time.Now().After(until) {
			fail("%s: the first offer returned %s (%d)", name, codeName(position), position)
		}
	}
	for i := 1; i < 10; i++ {
		offerOrFail(name, publication, message(i), i)
	}

	panicsOn3 := func(body []byte, header *cavalry.Header) {
		index := indexOf(body)
		seen = append(seen, index)
		if index == 3 {
			panic(errors.New("message 3"))
		}
	}
	count, err := subscription.Poll(panicsOn3, 10)
	var handlerErr *cavalry.HandlerError
	if err == nil || !errors.As(err, &handlerErr) {
		fail("%s: a handler that panicked on message 3 did not make Poll return a *HandlerError (%v); it saw %v", name, err, seen)
	}
	cause, isError := handlerErr.Cause.(error)
	if !slices.Equal(seen, rangeOf(0, 10)) || count != 10 || handlerErr.Fragments != 10 || handlerErr.Raised != 1 ||
		!isError || cause.Error() != "message 3" || errors.Unwrap(handlerErr) != cause {
		fail("%s: the handler saw %v; Poll returned %d and %v with Fragments %d, Raised %d, Cause %v; expected messages "+
			"0 to 9 once and in order, 10, 1 and the error", name, seen, count, err, handlerErr.Fragments, handlerErr.Raised,
			handlerErr.Cause)
	}
	if !strings.Contains(err.Error(), "Fix the handler") || !strings.HasSuffix(err.Error(), "See clients/go/README.md#the-panic") {
		fail("%s: HandlerError's message does not say what to do and where it is explained: %v", name, err)
	}

	offerOrFail(name, publication, message(10), 10)
	offerOrFail(name, publication, message(11), 11)
	panicsTwice := func(body []byte, header *cavalry.Header) {
		index := indexOf(body)
		seen = append(seen, index)
		if index == 10 {
			panic("message 10")
		}
		panic(errors.New("message 11"))
	}
	count, err = subscription.Poll(panicsTwice, 10)
	handlerErr = nil
	if !errors.As(err, &handlerErr) || !slices.Equal(seen, rangeOf(0, 12)) || count != 2 || handlerErr.Raised != 2 ||
		handlerErr.Cause != "message 10" || handlerErr.Unwrap() != nil {
		fail("%s: Poll returned %d and %v, and the handler saw %v; expected *HandlerError with Fragments 2, Raised 2 and "+
			"the string, the first, as its Cause, and messages 0 to 11", name, count, err, seen)
	}
	if must(subscription.Poll(unexpected, 10)) != 0 {
		fail("%s: a fragment after all 12 messages", name)
	}
	say("%s: a handler that panicked on message 3 of a poll of 10 saw all 10 once, in order, and Poll returned a "+
		"*HandlerError (Fragments 10, Raised 1, the error as its Cause); a handler that panicked with a string and then an "+
		"error in one poll gave Raised 2 and the string as the Cause", name)

	// The lifetimes: the claim's slice is nil after its commit, and a header read after
	// its handler call is invalid. A slice kept past its call points into the log, which
	// Go cannot detach; the README says so.
	name = "lifetimes"
	position := claimOrFail(name, publication, 12)
	view := publication.Claim
	binary.LittleEndian.PutUint64(view, 12)
	if publication.Commit() != position || publication.Claim != nil {
		fail("%s: the commit did not return the claim's position %d and set Claim to nil", name, position)
	}
	var kept *cavalry.Header
	var first, again cavalry.Frame
	var firstOK, againOK bool
	keeps := func(body []byte, header *cavalry.Header) {
		kept = header
		first, firstOK = header.Values()
		again, againOK = header.Values()
	}
	until = deadline()
	for kept == nil {
		pollOrFail(name, subscription, keeps, 0, 1)
		if time.Now().After(until) {
			fail("%s: message 12 did not arrive in %v", name, wait)
		}
	}
	if !firstOK || !againOK || first != again {
		fail("%s: two reads of Values in one handler call gave %v (%v) and %v (%v); expected the one frame, twice", name,
			first, firstOK, again, againOK)
	}
	if frame, ok := kept.Values(); ok {
		fail("%s: a header read after its handler call gave %v", name, frame)
	}
	say("%s: a committed claim's slice is nil, a header read after its handler call is invalid, and Values gave the one "+
		"frame on each read inside the call", name)

	// Calls the ABI refuses during a poll or a claim leave the wrapper as they found it
	// (spec/c-abi.md, "Calls during a claim or a poll").
	name = "refusals"
	for i := 13; i < 16; i++ {
		offerOrFail(name, publication, message(i), i)
	}
	var outer []int
	// The refused poll's error twice: kept in the subscription by the shim, and read as
	// the thread's, which a handler may, since cgo holds it to the poll's thread.
	type nestedResult struct {
		count  int
		kept   int32
		thread int32
	}
	var nested []nestedResult
	nestedHandler := func(body []byte, header *cavalry.Header) {
		fail("%s: a poll from inside a handler of the same subscription delivered a fragment", name)
	}
	outerHandler := func(body []byte, header *cavalry.Header) {
		outer = append(outer, indexOf(body))
		count := must(subscription.Poll(nestedHandler, 10))
		nested = append(nested, nestedResult{count, codeOf(subscription.Err()), cavalry.Errcode()})
	}
	until = deadline()
	for len(outer) < 3 {
		pollOrFail(name, subscription, outerHandler, len(outer), 3)
		if time.Now().After(until) {
			fail("%s: received messages %v of 13 to 15 in %v", name, outer, wait)
		}
	}
	refusedPoll := nestedResult{-1, cavalry.ErrPolling, cavalry.ErrPolling}
	if !slices.Equal(outer, []int{13, 14, 15}) || !slices.Equal(nested, []nestedResult{refusedPoll, refusedPoll, refusedPoll}) {
		fail("%s: the outer handler saw %v and the polls from inside it returned %v (the count, Err's code, Errcode); "+
			"expected messages 13 to 15, and -1 with ErrPolling (%d) each time", name, outer, nested, cavalry.ErrPolling)
	}
	var abi *cavalry.Error
	if !errors.As(subscription.Err(), &abi) || abi.Fn != "cavalry_subscription_poll" || !strings.Contains(abi.Message, "(polling)") {
		fail("%s: after the refused polls the subscription's Err is %v; expected the refusal of cavalry_subscription_poll, "+
			"kept until the next failure", name, subscription.Err())
	}

	position = claimOrFail(name, publication, 16)
	firstClaim := publication.Claim
	binary.LittleEndian.PutUint64(firstClaim, 16)
	second := publication.TryClaim(bodyLength)
	secondErr := publication.Err()
	if second != cavalry.PublicationError || !errors.As(secondErr, &abi) || abi.Fn != "cavalry_publication_try_claim" ||
		abi.Code != cavalry.ErrClaimOutstanding || !strings.Contains(abi.Message, "(claim-outstanding)") ||
		len(publication.Claim) != len(firstClaim) || &publication.Claim[0] != &firstClaim[0] {
		fail("%s: a TryClaim with a claim outstanding returned %s with Err %v, and Claim is %v; expected ERROR with "+
			"cavalry_publication_try_claim's ErrClaimOutstanding and the outstanding claim's slice", name, codeName(second),
			secondErr, publication.Claim)
	}
	if publication.Commit() != position || publication.Claim != nil || publication.Err().Error() != secondErr.Error() {
		fail("%s: the outstanding claim's commit did not return its position %d, set Claim to nil and keep Err, %v", name,
			position, publication.Err())
	}
	// The struct's claim is committed: a second commit is the ABI's NO_CLAIM, in Err.
	if publication.Commit() != -1 || !errors.As(publication.Err(), &abi) || abi.Fn != "cavalry_buffer_claim_commit" ||
		abi.Code != cavalry.ErrNoClaim {
		fail("%s: a second commit of a committed claim left Err %v; expected cavalry_buffer_claim_commit's ErrNoClaim", name,
			publication.Err())
	}
	var received []int
	index := func(body []byte, header *cavalry.Header) { received = append(received, indexOf(body)) }
	until = deadline()
	for len(received) == 0 {
		pollOrFail(name, subscription, index, 16, 17)
		if time.Now().After(until) {
			fail("%s: message 16 did not arrive in %v", name, wait)
		}
	}
	if !slices.Equal(received, []int{16}) {
		fail("%s: received %v for the claim committed past a refused one; expected message 16", name, received)
	}

	claimOrFail(name, publication, 17)
	if err := publication.Close(); err != nil || publication.Claim != nil {
		fail("%s: the publication's close with a claim outstanding returned %v and left Claim %v", name, err, publication.Claim)
	}
	pollOrFail(name, subscription, unexpected, 17, 17) // the close aborted the claim: padding, never a fragment

	// A close from inside the handler: refused with ErrPolling, the handle kept, the
	// next message still delivered, and the close after the poll succeeds.
	publication = must(client.AddPublication("cavalry:ipc", stream+2))
	offerOrFail(name, publication, message(17), 17)
	var closeErr error
	closesInside := func(body []byte, header *cavalry.Header) {
		received = append(received, indexOf(body))
		closeErr = subscription.Close()
	}
	until = deadline()
	for len(received) < 2 {
		pollOrFail(name, subscription, closesInside, len(received), 2)
		if time.Now().After(until) {
			fail("%s: message 17 did not arrive in %v", name, wait)
		}
	}
	abi = nil
	if !errors.As(closeErr, &abi) || abi.Code != cavalry.ErrPolling || subscription.IsClosed() {
		fail("%s: a close from inside the handler returned %v and IsClosed is %v; expected *Error with ErrPolling and the "+
			"subscription open", name, closeErr, subscription.IsClosed())
	}
	offerOrFail(name, publication, message(18), 18)
	until = deadline()
	for len(received) < 3 {
		pollOrFail(name, subscription, index, len(received), 3)
		if time.Now().After(until) {
			fail("%s: message 18 did not arrive in %v after the refused close", name, wait)
		}
	}
	if !slices.Equal(received, []int{16, 17, 18}) {
		fail("%s: received %v; expected messages 16, 17 and 18, the last after the refused close", name, received)
	}
	registered := cavalry.Registered()
	if err := subscription.Close(); err != nil || !subscription.IsClosed() || cavalry.Registered() != registered-1 {
		fail("%s: the close after the poll returned %v, IsClosed is %v and the table holds %d, was %d", name, err,
			subscription.IsClosed(), cavalry.Registered(), registered)
	}
	mustOK(publication.Close())

	// A call that is not a message reads its error on the thread of the call that failed:
	// an add whose channel is not UTF-8, which the ABI refuses before the driver
	// (spec/c-abi.md, "Strings").
	abi = nil
	if _, err := client.AddPublication("cavalry:ipc\xff", stream+2); !errors.As(err, &abi) ||
		abi.Fn != "cavalry_add_publication" || abi.Code != cavalry.ErrNotUTF8 || abi.Message == "" {
		fail("%s: an add whose channel is not UTF-8 returned %v; expected *Error with cavalry_add_publication's "+
			"ErrNotUTF8 and its message", name, err)
	}
	say("%s: a poll from inside a handler of its own subscription returned -1 with ErrPolling, in the subscription's "+
		"Err and the handler's Errcode, and the outer handler saw messages 13 to 15; a TryClaim with a claim outstanding "+
		"returned ERROR with ErrClaimOutstanding in Err, Claim stayed the first slice and its commit set it to nil; a "+
		"second commit left ErrNoClaim in Err; a close with a claim outstanding set Claim to nil; a close from inside the "+
		"handler returned *Error with ErrPolling, kept the subscription open for message 18, and the close after the poll "+
		"freed its slot; an add whose channel is not UTF-8 returned *Error with ErrNotUTF8", name)
}

func rangeOf(from, to int) []int {
	out := make([]int, 0, to-from)
	for i := from; i < to; i++ {
		out = append(out, i)
	}
	return out
}

// reservedValues checks, on stream 1005, that a reserved value beyond 2^53 and the
// smallest int64 reach the handler exactly.
func reservedValues(client *cavalry.Client) {
	name := "reserved values"
	subscription := must(client.AddSubscription("cavalry:ipc", stream+4))
	publication := must(client.AddPublication("cavalry:ipc", stream+4))
	values := []int64{1<<62 + 1, math.MinInt64}
	var read []int64
	onFragment := func(body []byte, header *cavalry.Header) {
		frame, ok := header.Values()
		if !ok {
			fail("%s: the header was refused inside the handler", name)
		}
		read = append(read, frame.ReservedValue)
	}
	until := deadline()
	for i, value := range values {
		for {
			must(subscription.Poll(onFragment, 10))
			position := publication.TryClaim(bodyLength)
			if position >= 0 {
				break
			}
			refused(name, publication, position, i, until, len(values))
		}
		binary.LittleEndian.PutUint64(publication.Claim, uint64(i))
		if publication.SetReservedValue(value) != 0 {
			fail("%s: SetReservedValue(%d) failed: %v", name, value, publication.Err())
		}
		publication.Commit()
	}
	until = deadline()
	for len(read) < len(values) {
		pollOrFail(name, subscription, onFragment, len(read), len(values))
		if time.Now().After(until) {
			fail("%s: received %d of %d in %v", name, len(read), len(values), wait)
		}
	}
	if !slices.Equal(read, values) {
		fail("%s: the handler read %v; expected %v exactly", name, read, values)
	}
	mustOK(publication.Close())
	mustOK(subscription.Close())
	say("%s: 2^62+1 and the smallest int64 reached the handler exactly through Values", name)
}

// latency is the latency round, for the report: 100,000 messages over cavalry:ipc on
// stream 1004, one at a time on this goroutine. The publisher writes the monotonic clock,
// time.Since a start, into the claim before its commit; the handler reads the clock first
// and keeps the difference. The latencies, in order.
func latency(client *cavalry.Client) []int64 {
	name := "the latency round"
	subscription := must(client.AddSubscription("cavalry:ipc", stream+3))
	publication := must(client.AddPublication("cavalry:ipc", stream+3))
	latencies := make([]int64, latencyMessages)
	received := 0
	start := time.Now()
	onFragment := func(body []byte, header *cavalry.Header) {
		now := int64(time.Since(start))
		latencies[received] = now - int64(binary.LittleEndian.Uint64(body))
		received++
	}
	until := deadline()
	for i := 0; i < latencyMessages; i++ {
		for {
			position := publication.TryClaim(bodyLength)
			if position >= 0 {
				break
			}
			refused(name, publication, position, i, until, latencyMessages)
			pollOrFail(name, subscription, onFragment, received, latencyMessages) // the image joins on a poll
		}
		binary.LittleEndian.PutUint64(publication.Claim, uint64(time.Since(start)))
		publication.Commit()
		for received <= i {
			pollOrFail(name, subscription, onFragment, received, latencyMessages)
			if time.Now().After(until) {
				fail("%s: received %d of %d; message %d did not arrive in %v", name, received, latencyMessages, i, wait)
			}
		}
		until = deadline() // 10 s for each message, outside the interval it measures
	}
	if must(subscription.Poll(onFragment, 16)) != 0 {
		fail("%s: a fragment after all %d messages", name, latencyMessages)
	}
	mustOK(publication.Close())
	mustOK(subscription.Close())
	return latencies
}

// allocation is the allocation round's result: the messages measured, and Go's heap
// allocations and bytes over them.
type allocation struct {
	measured int
	mallocs  uint64
	bytes    uint64
}

// allocations is the allocation round, for the report: 10,000 messages over cavalry:ipc
// on stream 1002 whose handler only counts and whose publisher copies one constant, so
// that the checks' objects are not in the count, with runtime.ReadMemStats read before and
// after, nothing between, after a collection.
func allocations(client *cavalry.Client) allocation {
	name := "the allocation round"
	subscription := must(client.AddSubscription("cavalry:ipc", stream+1))
	publication := must(client.AddPublication("cavalry:ipc", stream+1))
	received := 0
	count := func(body []byte, header *cavalry.Header) { received++ }
	constant := make([]byte, bodyLength)
	for b := range constant {
		constant[b] = byte(b)
	}
	until := deadline()
	for {
		pollOrFail(name, subscription, count, received, allocationMessages)
		position := publication.TryClaim(bodyLength)
		if position >= 0 {
			break
		}
		refused(name, publication, position, 0, until, allocationMessages)
	}
	copy(publication.Claim, constant)
	publication.Commit()
	sent := 1
	pollOrFail(name, subscription, count, received, allocationMessages)

	var before, after runtime.MemStats
	runtime.GC()
	receivedBefore := received
	runtime.ReadMemStats(&before)
	until = deadline()
	for sent < allocationMessages {
		position := publication.TryClaim(bodyLength)
		if position >= 0 {
			copy(publication.Claim, constant)
			publication.Commit()
			sent++
		} else {
			refused(name, publication, position, sent, until, allocationMessages)
		}
		pollOrFail(name, subscription, count, received, allocationMessages)
	}
	for received < allocationMessages {
		pollOrFail(name, subscription, count, received, allocationMessages)
		if time.Now().After(until) {
			fail("%s: received %d of %d in %v", name, received, allocationMessages, wait)
		}
	}
	runtime.ReadMemStats(&after)

	if must(subscription.Poll(count, 16)) != 0 {
		fail("%s: a fragment after all %d messages", name, allocationMessages)
	}
	mustOK(publication.Close())
	mustOK(subscription.Close())
	return allocation{allocationMessages - receivedBefore, after.Mallocs - before.Mallocs, after.TotalAlloc - before.TotalAlloc}
}

// percentile is the nearest-rank percentile parts/whole of sorted values: the smallest
// value with at least that share of the values at or below it, and how many lie above.
func percentile(sorted []int64, parts, whole int) (int64, int) {
	rank := (len(sorted)*parts + whole - 1) / whole
	if rank < 1 {
		rank = 1
	}
	return sorted[rank-1], len(sorted) - rank
}

func withCommas(n int64) string {
	s := fmt.Sprint(n)
	negative := strings.HasPrefix(s, "-")
	if negative {
		s = s[1:]
	}
	var out []byte
	for i, c := range []byte(s) {
		if i > 0 && (len(s)-i)%3 == 0 {
			out = append(out, ',')
		}
		out = append(out, c)
	}
	if negative {
		return "-" + string(out)
	}
	return string(out)
}

func freeUDPPort() int {
	conn, err := net.ListenUDP("udp4", &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1), Port: 0})
	if err != nil {
		fail("no free UDP port on the loopback: %v", err)
	}
	port := conn.LocalAddr().(*net.UDPAddr).Port
	conn.Close()
	return port
}

// stateFreed checks that a closed subscription's slot in the table is freed, so that a
// program that adds and closes subscriptions does not fill the table.
func stateFreed(client *cavalry.Client) {
	before := cavalry.Registered()
	subscription := must(client.AddSubscription("cavalry:ipc", stream+6))
	if cavalry.Registered() != before+1 {
		fail("the table holds %d subscriptions after an add, was %d", cavalry.Registered(), before)
	}
	mustOK(subscription.Close())
	if cavalry.Registered() != before {
		fail("the table holds %d subscriptions after a close, was %d before the add", cavalry.Registered(), before)
	}
	if err := subscription.Close(); err != nil {
		fail("a second close returned %v", err)
	}
	say("a closed subscription's slot in the table is freed: %d held before the add, %d after the close", before, cavalry.Registered())
}

// exchange offers one message carrying value on publication and polls subscription until
// it arrives, for up to 10 s: "", or what went wrong.
func exchange(publication *cavalry.Publication, subscription *cavalry.Subscription, value uint64) string {
	message := make([]byte, bodyLength)
	binary.LittleEndian.PutUint64(message, value)
	var got []uint64
	onFragment := func(body []byte, header *cavalry.Header) { got = append(got, binary.LittleEndian.Uint64(body)) }
	sent := false
	until := deadline()
	for len(got) == 0 {
		count, err := subscription.Poll(onFragment, 10)
		if err != nil {
			return fmt.Sprintf("the handler panicked: %v", err)
		}
		if count < 0 {
			return fmt.Sprintf("the poll stopped on a fault: %v", subscription.Err())
		}
		if !sent {
			position := publication.Offer(message)
			if position >= 0 {
				sent = true
			} else if position == cavalry.PublicationClosed || position == cavalry.PublicationMaxPositionExceeded ||
				position == cavalry.PublicationError {
				return fmt.Sprintf("Offer returned %s (%d): %v", codeName(position), position, publication.Err())
			}
		}
		if time.Now().After(until) {
			return fmt.Sprintf("message %d did not arrive in %v; offered: %v", value, wait, sent)
		}
	}
	if len(got) != 1 || got[0] != value {
		return fmt.Sprintf("received %v; expected message %d once", got, value)
	}
	return ""
}

// sharedClient checks that goroutines may share a Client (clients/go/README.md,
// "Threads"): eight goroutines at once, each adding a publication and a subscription on a
// stream of its own through the one client, sending one message from the one to the
// other, reading IsClosed, making an add the ABI refuses and closing what it added, four
// times over. Every add succeeds, every message arrives once, every refusal is its own
// *Error with ErrNotUTF8, and the table holds what it held before. The message also has
// the subscription hold its image before its close: a subscription closed while the
// client's conductor has yet to read its `available image` can close the client as
// corrupt, a race of the Rust client's own, not of this package, which a goroutine alone
// meets (the pull request's record, card 207).
func sharedClient(client *cavalry.Client) {
	const goroutines, rounds = 8, 4
	name := "shared client"
	before := cavalry.Registered()
	failures := make([]string, goroutines)
	var wg sync.WaitGroup
	for g := 0; g < goroutines; g++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			streamID := int32(stream + 100 + g)
			for r := 0; r < rounds; r++ {
				publication, err := client.AddPublication("cavalry:ipc", streamID)
				if err != nil {
					failures[g] = fmt.Sprintf("goroutine %d, round %d: AddPublication returned %v", g, r, err)
					return
				}
				subscription, err := client.AddSubscription("cavalry:ipc", streamID)
				if err != nil {
					failures[g] = fmt.Sprintf("goroutine %d, round %d: AddSubscription returned %v", g, r, err)
					publication.Close()
					return
				}
				if problem := exchange(publication, subscription, uint64(g*rounds+r)); problem != "" {
					failures[g] = fmt.Sprintf("goroutine %d, round %d: %s", g, r, problem)
				}
				_, refusal := client.AddPublication("cavalry:ipc\xff", streamID)
				if (codeOf(refusal) != cavalry.ErrNotUTF8 || client.IsClosed()) && failures[g] == "" {
					failures[g] = fmt.Sprintf("goroutine %d, round %d: a refused add returned %v and IsClosed is %v; "+
						"expected *Error with ErrNotUTF8 and an open client", g, r, refusal, client.IsClosed())
				}
				if err := subscription.Close(); err != nil && failures[g] == "" {
					failures[g] = fmt.Sprintf("goroutine %d, round %d: the subscription's close returned %v", g, r, err)
				}
				if err := publication.Close(); err != nil && failures[g] == "" {
					failures[g] = fmt.Sprintf("goroutine %d, round %d: the publication's close returned %v", g, r, err)
				}
				if failures[g] != "" {
					return
				}
			}
		}()
	}
	wg.Wait()
	var failed []string
	for _, failure := range failures {
		if failure != "" {
			failed = append(failed, failure)
		}
	}
	if len(failed) > 0 {
		fail("%s: the client's Closed is %d (spec/c-abi.md, CAVALRY_CLOSED_*); %s", name, client.Closed(),
			strings.Join(failed, "; "))
	}
	if cavalry.Registered() != before {
		fail("%s: the table holds %d subscriptions after the round, %d before", name, cavalry.Registered(), before)
	}
	say("%s: %d goroutines at once added and closed %d publications and %d subscriptions through one Client, each "+
		"pair's message arrived once, and each of their %d refused adds returned its own *Error with ErrNotUTF8", name,
		goroutines, goroutines*rounds, goroutines*rounds, goroutines*rounds)
}

func main() {
	// The load-time check (spec/c-abi.md, "Versions and what a reader rejects"): the check
	// NewClient makes first refuses a package whose ABIVersion is not the library's, with
	// an *Error naming both numbers.
	version := int32(cavalry.ABIVersion)
	refusal := cavalry.CheckABIVersion(version + 1)
	var abi *cavalry.Error
	if refusal == nil {
		fail("a package of ABI version %d was not refused against a library of version %d", version+1, version)
	}
	if !errors.As(refusal, &abi) || abi.Code != cavalry.ErrABIVersion ||
		!strings.Contains(abi.Message, fmt.Sprintf("is ABI version %d and", version)) ||
		!strings.Contains(abi.Message, fmt.Sprintf("from a header of ABI version %d;", version+1)) {
		fail("a package of ABI version %d was refused with %v; expected *Error with ErrABIVersion naming both versions", version+1, refusal)
	}
	say("ABI version: a package of version %d was refused by CheckABIVersion: %v", version+1, refusal)
	if cavalry.LibraryABIVersion() != version || cavalry.CheckABIVersion(version) != nil {
		fail("the library's ABI version is %d, the package's %d", cavalry.LibraryABIVersion(), version)
	}

	client := must(cavalry.NewClient(""))
	say("client %d on %s, library %s, ABI version %d", client.ClientID(), client.Dir(), cavalry.Version(), cavalry.LibraryABIVersion())

	// 1. Record the stream, then publish 1,000 messages over cavalry:ipc and read them
	// back, once and in order.
	ipcSub := must(client.AddSubscription("cavalry:ipc", stream))
	ipcPub := must(client.AddPublication("cavalry:ipc", stream))
	recording := must(client.StartRecording("cavalry:ipc", stream)) // the publication first: it records a live one
	ipc := &receiver{name: "cavalry:ipc", subscription: ipcSub}
	publishAndReceive(ipcPub, ipc)
	end := ipcPub.Position()
	say("%d messages over cavalry:ipc by session %d received once, in order; the stream ends at %d", messages, ipc.sessionID, end)
	until := deadline()
	for recording.SyncedPosition() < end {
		if time.Now().After(until) {
			fail("the recording synced %d of %d in %v", recording.SyncedPosition(), end, wait)
		}
	}
	mustOK(client.StopRecording(recording.RecordingID))
	recordingID := recording.RecordingID
	mustOK(recording.Close())
	mustOK(ipcPub.Close())

	edges(client)
	reservedValues(client)
	stateFreed(client)
	sharedClient(client)

	// The report's two rounds, which check nothing else.
	latencies := latency(client)
	slices.Sort(latencies)
	allocated := allocations(client)

	// 2. The same 1,000 over cavalry:udp on the loopback.
	udpChannel := fmt.Sprintf("cavalry:udp?endpoint=127.0.0.1:%d", freeUDPPort())
	udpSub := must(client.AddSubscription(udpChannel, stream))
	udpPub := must(client.AddPublication(udpChannel, stream))
	udp := &receiver{name: udpChannel, subscription: udpSub}
	publishAndReceive(udpPub, udp)
	say("%d messages over %s by session %d received once, in order, from %s", messages, udpChannel, udp.sessionID, udp.constants.SourceIdentity)
	mustOK(udpPub.Close())
	mustOK(udpSub.Close())

	// 3. Replay the recording onto stream 2001: the same checks, each frame at the
	// position the recording holds it at (ADR 0030), and the end reported.
	replaySub := must(client.AddSubscription("cavalry:ipc", replayStream))
	replay := must(client.StartReplay(recordingID, cavalry.NullPosition, cavalry.NullLength, replayStream, "cavalry:ipc"))
	replayed := &receiver{name: "replay", subscription: replaySub}
	until = deadline()
	var ended bool
	var endedAt int64
	var reason int32
	for {
		pollOrFail("replay", replaySub, replayed.onFragment, replayed.next, messages)
		replayed.takeConstants()
		ended, endedAt, reason = replay.Ended()
		if replayed.next >= messages && ended {
			break
		}
		if time.Now().After(until) {
			if ended {
				fail("replayed %d of %d in %v; the replay ended at %d with reason %d", replayed.next, messages, wait, endedAt, reason)
			}
			fail("replayed %d of %d in %v; the replay has not ended, at position %d", replayed.next, messages, wait, replay.Position())
		}
	}
	replayed.expectNoMore()
	replayed.computePositions()
	if endedAt != end || reason != cavalry.ReplayStopEnded {
		fail("the replay ended at %d with reason %d, not at %d with %d", endedAt, reason, end, cavalry.ReplayStopEnded)
	}
	for i := 0; i < messages; i++ {
		if replayed.positions[i] != ipc.positions[i] {
			fail("message %d replayed at %d, recorded at %d", i, replayed.positions[i], ipc.positions[i])
		}
	}
	say("%d messages replayed on stream %d at the positions the recording holds them, ended at %d", messages, replayStream, endedAt)
	mustOK(replay.Close())
	mustOK(replaySub.Close())
	mustOK(ipcSub.Close())
	mustOK(client.Close())

	// The report, never gated: the laptop's or the runner's cost of this side of the
	// boundary (clients/go/README.md, "The runtime's side").
	p50, _ := percentile(latencies, 50, 100)
	p99, _ := percentile(latencies, 99, 100)
	p9999, above := percentile(latencies, 9999, 10000)
	say("report: one-way latency over %s cavalry:ipc messages, one at a time on one goroutine, nearest rank: p50 %s ns, "+
		"p99 %s ns, p99.99 %s ns (%d samples above it), max %s ns", withCommas(int64(len(latencies))), withCommas(p50),
		withCommas(p99), withCommas(p9999), above, withCommas(latencies[len(latencies)-1]))
	say("report: allocations per message, this side of the boundary, over %s messages whose handler counts and whose "+
		"publisher copies a constant: %.4f allocations and %.1f bytes (runtime.ReadMemStats, Mallocs and TotalAlloc, "+
		"nothing read between)", withCommas(int64(allocated.measured)), float64(allocated.mallocs)/float64(allocated.measured),
		float64(allocated.bytes)/float64(allocated.measured))
	say("ok")
}
