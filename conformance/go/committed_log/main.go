// The Go client's committed log round (spec/c-abi.md, version 2; ADR 0070):
// Client.CommittedLog and CommittedLog beside a cluster member that conformance/go/run.sh
// started with `cavalry run`, one of three on the loopback, as
// conformance/python/committed_log.py runs it through clients/python.
//
//	committed_log <the member's driver directory> <the member's id>
//
// A cluster started with `cavalry run` holds one entry, the first of its leader's term, 56
// bytes in a frame of 88 at position 0, so the log's next position is 96. The round reads
// that entry through the handler, with its position, its header and the handle's reads;
// makes each call the ABI refuses from inside the log's own handler; checks where a
// handler's panic goes and that its entry is not delivered again; opens the log again at
// 96, where nothing is committed yet; closes twice, with the log's slot in the table
// freed; and closes a log after its client. Every wait is bounded at 30 s. It exits 1 with
// what it found when a check fails. Go's standard library only (ADR 0003).
package main

import (
	"errors"
	"fmt"
	"os"
	"reflect"
	"strconv"
	"time"

	cavalry "github.com/appshapes/cavalry/clients/go"
)

const (
	wait       = 30 * time.Second
	entryFrame = 88 // the term's first entry: 32 bytes of header and 56 of message
	next       = 96 // the frame, rounded up to a multiple of 32
)

func fail(format string, args ...any) {
	fmt.Fprintf(os.Stderr, "conformance/go: committed log: "+format+"\n", args...)
	os.Exit(1)
}

func say(format string, args ...any) {
	fmt.Printf("conformance/go: committed log: "+format+"\n", args...)
}

func mustOK(err error) {
	if err != nil {
		fail("%v", err)
	}
}

func codeOf(err error) int32 {
	var abi *cavalry.Error
	if errors.As(err, &abi) {
		return abi.Code
	}
	return 0
}

// openLog is the member's committed log at position, asked for until the member has one.
func openLog(client *cavalry.Client, position int64) *cavalry.CommittedLog {
	until := time.Now().Add(wait)
	for {
		log, err := client.CommittedLog(position)
		mustOK(err)
		if log != nil {
			return log
		}
		if time.Now().After(until) {
			fail("the driver at %s hosted no member with a log within %v", client.Dir(), wait)
		}
		time.Sleep(50 * time.Millisecond)
	}
}

type entry struct {
	position    int64
	length      int
	frameLength int32
	flags       uint8
	frameType   uint16
}

type refusal struct {
	call   string
	answer int64
	code   int32
}

func main() {
	if len(os.Args) != 3 {
		fail("usage: committed_log <the member's driver directory> <the member's id>")
	}
	memberDir := os.Args[1]
	memberID, err := strconv.Atoi(os.Args[2])
	mustOK(err)
	client, err := cavalry.NewClient(memberDir)
	mustOK(err)
	held := cavalry.Registered()
	log := openLog(client, 0)
	if log.MemberID != int32(memberID) || log.RecordingID != 0 {
		fail("the log is member %d's recording %d, not member %d's recording 0", log.MemberID, log.RecordingID, memberID)
	}
	if log.Position() != 0 || log.CommitPosition() != 0 {
		fail("a log opened at 0 began at %d with bound %d", log.Position(), log.CommitPosition())
	}
	if cavalry.Registered() != held+1 {
		fail("an open log holds %d slots of the table, not one", cavalry.Registered()-held)
	}

	// The entry, with what each call on the log answers from inside its own handler. The
	// handler runs on the poll's thread, so Errcode after each call is that call's.
	var entries []entry
	var refusals []refusal
	var onEntry cavalry.EntryHandler
	onEntry = func(body []byte, header *cavalry.Header, position int64) {
		frame, ok := header.Values()
		if !ok {
			fail("the entry's header did not read")
		}
		entries = append(entries, entry{position, len(body), frame.FrameLength, frame.Flags, frame.Type})
		if len(refusals) > 0 {
			return
		}
		count, _ := log.Poll(onEntry, 1)
		refusals = append(refusals, refusal{"Poll", int64(count), codeOf(log.Err())})
		refusals = append(refusals, refusal{"Position", log.Position(), cavalry.Errcode()})
		refusals = append(refusals, refusal{"CommitPosition", log.CommitPosition(), cavalry.Errcode()})
		ended := int64(0)
		if log.IsEnded() {
			ended = 1
		}
		refusals = append(refusals, refusal{"IsEnded", ended, cavalry.Errcode()})
		at, failed := log.FailedAt()
		if failed {
			at = 1
		}
		refusals = append(refusals, refusal{"FailedAt", at, cavalry.Errcode()})
		at, withdrawn := log.WithdrawnAt()
		if withdrawn {
			at = 1
		}
		refusals = append(refusals, refusal{"WithdrawnAt", at, cavalry.Errcode()})
		closed := int64(0)
		closeErr := log.Close()
		if closeErr != nil {
			closed = -1
		}
		refusals = append(refusals, refusal{"Close", closed, codeOf(closeErr)})
	}
	until := time.Now().Add(wait)
	for len(entries) == 0 {
		count, err := log.Poll(onEntry, 16)
		mustOK(err)
		if count < 0 {
			fail("the term's first entry: the poll stopped: %v", log.Err())
		}
		if time.Now().After(until) {
			fail("the term's first entry: not within %v; position %d, commit position %d, ended %v", wait, log.Position(),
				log.CommitPosition(), log.IsEnded())
		}
	}
	expectedEntry := []entry{{0, entryFrame - 32, entryFrame, cavalry.FlagsUnfragmented, cavalry.FrameTypeData}}
	if !reflect.DeepEqual(entries, expectedEntry) {
		fail("expected one entry of %d bytes at position 0 in an unfragmented data frame of %d, got %v", entryFrame-32,
			entryFrame, entries)
	}
	polling := int32(cavalry.ErrPolling)
	expected := []refusal{{"Poll", -1, polling}, {"Position", -1, polling}, {"CommitPosition", -1, polling},
		{"IsEnded", 0, polling}, {"FailedAt", 0, polling}, {"WithdrawnAt", 0, polling}, {"Close", -1, polling}}
	if !reflect.DeepEqual(refusals, expected) {
		fail("the calls from inside the log's own handler answered %v, not %v", refusals, expected)
	}
	if log.Position() != next || log.CommitPosition() != next {
		fail("after the entry the log is at %d with bound %d, not %d and %d", log.Position(), log.CommitPosition(), next, next)
	}
	code, sessionID, position, err := log.Fault()
	mustOK(err)
	_, failed := log.FailedAt()
	_, withdrawn := log.WithdrawnAt()
	if log.IsEnded() || failed || withdrawn || code != 0 || sessionID != 0 || position != cavalry.NullPosition {
		fail("a live log read ended %v, failed %v, withdrawn %v, fault %d %d %d", log.IsEnded(), failed, withdrawn, code,
			sessionID, position)
	}
	for i := 0; i < 50; i++ {
		count, err := log.Poll(onEntry, 16)
		mustOK(err)
		if count != 0 {
			fail("an entry arrived with nothing more committed")
		}
	}
	if len(entries) != 1 {
		fail("the refused poll, or a later one, delivered more: %v", entries)
	}
	say("member %d's entry at position 0 read once, next position %d, commit position %d; seven calls from inside the "+
		"handler refused with ErrPolling", log.MemberID, log.Position(), log.CommitPosition())
	mustOK(log.Close())
	mustOK(log.Close()) // idempotent
	if cavalry.Registered() != held {
		fail("the table holds %d slots after the log's close, %d before its open", cavalry.Registered(), held)
	}

	// A handler that panics: the entry was delivered, the panic is returned as a
	// *HandlerError once the ABI has returned, and the log's position is past the entry,
	// so it is not delivered again.
	panicking := openLog(client, 0)
	var seen []int64
	panics := func(body []byte, header *cavalry.Header, position int64) {
		seen = append(seen, position)
		panic("the handler's own")
	}
	var handlerError *cavalry.HandlerError
	until = time.Now().Add(wait)
	for handlerError == nil {
		count, err := panicking.Poll(panics, 16)
		if err != nil && !errors.As(err, &handlerError) {
			fail("the second log's poll returned %v", err)
		}
		if count < 0 {
			fail("the second log's poll stopped: %v", panicking.Err())
		}
		if handlerError == nil && time.Now().After(until) {
			fail("the second log read no entry")
		}
	}
	if !reflect.DeepEqual(seen, []int64{0}) || handlerError.Raised != 1 || handlerError.Cause != "the handler's own" ||
		panicking.Position() != next {
		fail("a panicking handler saw %v, returned %v, and left the log at %d", seen, handlerError, panicking.Position())
	}
	for i := 0; i < 50; i++ {
		count, err := panicking.Poll(panics, 16)
		mustOK(err)
		if count != 0 {
			fail("the entry whose handler panicked was delivered again")
		}
	}
	mustOK(panicking.Close())

	// From the next position nothing is committed yet: no entry, and no entry before it.
	rest := openLog(client, next)
	if rest.Position() != next {
		fail("a log opened at %d began at %d", next, rest.Position())
	}
	for i := 0; i < 100; i++ {
		count, err := rest.Poll(onEntry, 16)
		mustOK(err)
		if count != 0 {
			fail("a log opened at %d delivered an entry: %v", next, entries)
		}
	}
	// A log that outlives its client: its poll stops on the closed fault, and its close
	// frees it, and its slot, with no client to stop the replay through.
	mustOK(client.Close())
	count, err := rest.Poll(onEntry, 16)
	mustOK(err)
	code, _, _, err = rest.Fault()
	mustOK(err)
	if count != -1 || code != cavalry.ErrFaultClosed {
		fail("a poll after the client's close returned %d with fault %d, not -1 with the closed fault", count, code)
	}
	mustOK(rest.Close())
	if cavalry.Registered() != held {
		fail("the table holds %d slots after every log's close, %d before the first open", cavalry.Registered(), held)
	}
	say("a panicking handler's entry is not delivered again; nothing at position %d; a log closed after its client; "+
		"the table's slots freed", next)
}
