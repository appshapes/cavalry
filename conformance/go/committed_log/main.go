// The Go client's committed log round (spec/c-abi.md, version 2; ADR 0070):
// Client.CommittedLog and CommittedLog beside a cluster member that conformance/go/run.sh
// started with `cavalry run`, one of three on the loopback, as
// conformance/python/committed_log.py runs it through clients/python.
//
//	committed_log <the member's driver directory> <the member's id>
//
// A cluster started with `cavalry run` holds one entry, the first of its leader's term, 56
// bytes in a frame of 88 at position 0, so the log's next position is 96. The round opens
// twenty logs and closes each with no poll between, which leaves its client open (ADR
// 0078); reads that entry through the handler, with its position, its header and the
// handle's reads; makes each of the nine calls the ABI refuses from inside the log's own
// handler, each after it has set the thread's error to another code, so that a refusal
// that answers what a live log answers is told from an answer; makes a log's own image
// fault, by one byte of the frame its replay wrote, and reads the fault from the poll that
// meets it and from Faulted for as long as the log is held (ADR 0080); checks where a
// handler's panic goes and that its entry is not delivered again; opens the log again at
// 96, where nothing is committed yet,
// beside a log at 0 that it reads the entry through meanwhile, so that the log at 96 has
// had the time a replay's image takes to join; closes twice, with the log's slot in the
// table freed; and closes a log after its client. Every wait is bounded at 30 s and sleeps
// a millisecond after a poll that delivered nothing. It exits 1 with what it found when a
// check fails. Go's standard library only (ADR 0003).
package main

import (
	"encoding/binary"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"strconv"
	"strings"
	"time"

	cavalry "github.com/appshapes/cavalry/clients/go"
)

const (
	wait       = 30 * time.Second
	idle       = time.Millisecond // after a poll that delivered nothing: three drivers share the host
	entryFrame = 88               // the term's first entry: 32 bytes of header and 56 of message
	next       = 96               // the frame, rounded up to a multiple of 32
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
	// A closed log, for the refusals below: its Position hands the ABI a null handle, which
	// answers -1 with ErrNull. The package exports no function of the ABI that takes a null
	// header, so this is the round's call that sets the thread's error to another code. It
	// is closed with no poll between, and so are nineteen more: a log's replay's image may
	// not have joined when it closes, and the client's conductor then reads of an image
	// whose log is gone. The client stays open (ADR 0078; before it, card 241, this closed
	// the client as corrupt in 8 of 60 tries), which the rest of the round shows.
	spent := openLog(client, 0)
	mustOK(spent.Close())
	for i := 0; i < 19; i++ {
		mustOK(openLog(client, 0).Close())
	}
	if client.IsClosed() {
		fail("a log opened and closed with no poll between closed its client: %d", client.Closed())
	}
	say("twenty logs opened and closed with no poll between left the client open")
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
	var nested error
	inside := false
	// setAnotherError sets the thread's error to one no refusal gives, ErrNull, through the
	// closed log's Position. A success clears nothing (spec/c-abi.md, "Error codes"), and a
	// refused IsEnded, FailedAt or WithdrawnAt answers what a live log answers, false, so
	// the code alone tells that refusal from an answer: it must have changed from this one
	// to ErrPolling. Without this the code would be ErrPolling already, left by the call
	// before.
	setAnotherError := func() {
		if spent.Position() != -1 || cavalry.Errcode() != cavalry.ErrNull {
			fail("a closed log's Position did not answer -1 with ErrNull: error %d", cavalry.Errcode())
		}
	}
	var onEntry cavalry.EntryHandler
	onEntry = func(body []byte, header *cavalry.Header, position int64) {
		frame, ok := header.Values()
		if !ok {
			fail("the entry's header did not read")
		}
		entries = append(entries, entry{position, len(body), frame.FrameLength, frame.Flags, frame.Type})
		if inside {
			return
		}
		inside = true
		setAnotherError()
		count, _ := log.Poll(onEntry, 1)
		refusals = append(refusals, refusal{"Poll", int64(count), cavalry.Errcode()})
		nested = log.Err()
		setAnotherError()
		refusals = append(refusals, refusal{"Position", log.Position(), cavalry.Errcode()})
		setAnotherError()
		refusals = append(refusals, refusal{"CommitPosition", log.CommitPosition(), cavalry.Errcode()})
		setAnotherError()
		ended := int64(0)
		if log.IsEnded() {
			ended = 1
		}
		refusals = append(refusals, refusal{"IsEnded", ended, cavalry.Errcode()})
		setAnotherError()
		at, failed := log.FailedAt()
		if failed {
			at = 1
		}
		refusals = append(refusals, refusal{"FailedAt", at, cavalry.Errcode()})
		setAnotherError()
		at, withdrawn := log.WithdrawnAt()
		if withdrawn {
			at = 1
		}
		refusals = append(refusals, refusal{"WithdrawnAt", at, cavalry.Errcode()})
		setAnotherError()
		kept := int64(0)
		if _, _, _, ok := log.Faulted(); ok {
			kept = 1
		}
		refusals = append(refusals, refusal{"Faulted", kept, cavalry.Errcode()})
		setAnotherError()
		faulted := int64(0)
		_, _, _, faultErr := log.Fault()
		if faultErr != nil {
			faulted = -1
		}
		refusals = append(refusals, refusal{"Fault", faulted, codeOf(faultErr)})
		setAnotherError()
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
		if count == 0 {
			time.Sleep(idle)
		}
	}
	expectedEntry := []entry{{0, entryFrame - 32, entryFrame, cavalry.FlagsUnfragmented, cavalry.FrameTypeData}}
	if !reflect.DeepEqual(entries, expectedEntry) {
		fail("expected one entry of %d bytes at position 0 in an unfragmented data frame of %d, got %v. One cause of "+
			"more than one entry is a new term: a cluster that elected again between its formation and this read holds "+
			"that term's first entry after this one; the members' output, which run.sh prints, says whether it did",
			entryFrame-32, entryFrame, entries)
	}
	polling := int32(cavalry.ErrPolling)
	if codeOf(nested) != polling {
		fail("the poll refused from inside the log's own handler left Err %v, not ErrPolling", nested)
	}
	expected := []refusal{{"Poll", -1, polling}, {"Position", -1, polling}, {"CommitPosition", -1, polling},
		{"IsEnded", 0, polling}, {"FailedAt", 0, polling}, {"WithdrawnAt", 0, polling}, {"Faulted", 0, polling},
		{"Fault", -1, polling}, {"Close", -1, polling}}
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
	_, _, _, kept := log.Faulted()
	if log.IsEnded() || failed || withdrawn || kept || code != 0 || sessionID != 0 || position != cavalry.NullPosition {
		fail("a live log read ended %v, failed %v, withdrawn %v, faulted %v, fault %d %d %d", log.IsEnded(), failed,
			withdrawn, kept, code, sessionID, position)
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
	say("member %d's entry at position 0 read once, next position %d, commit position %d; nine calls from inside the "+
		"handler refused with ErrPolling, each made with the thread's error at ErrNull", log.MemberID, log.Position(),
		log.CommitPosition())
	mustOK(log.Close())
	mustOK(log.Close()) // idempotent
	if cavalry.Registered() != held {
		fail("the table holds %d slots after the log's close, %d before its open", cavalry.Registered(), held)
	}

	// A log whose own image faults (ADR 0080). The log's replay plays into a log buffer of
	// its own, the one file that appears under the member's `publications/` when the log is
	// opened. Before any poll of the log, the round changes one byte of the entry the replay
	// wrote there, its frame's version, to one no reader accepts (spec/frame.md, "What a
	// reader rejects"). The poll that reads the frame returns -1 with the fault, once; Fault
	// answers it until the next poll; and Faulted answers it from that poll on, the same
	// code, session and position, whatever is polled after.
	publications := filepath.Join(memberDir, "publications")
	before := map[string]bool{}
	listed, err := os.ReadDir(publications)
	mustOK(err)
	for _, file := range listed {
		before[file.Name()] = true
	}
	damaged := openLog(client, 0)
	var replayed []string
	listed, err = os.ReadDir(publications)
	mustOK(err)
	for _, file := range listed {
		if !before[file.Name()] {
			replayed = append(replayed, file.Name())
		}
	}
	if len(replayed) != 1 {
		fail("opening a log made %v under %s, not the replay's one log buffer", replayed, publications)
	}
	if _, _, _, kept := damaged.Faulted(); kept {
		fail("a log that has not polled reads a fault")
	}
	replayLog, err := os.OpenFile(filepath.Join(publications, replayed[0]), os.O_RDWR, 0)
	mustOK(err)
	frameLength := make([]byte, 4)
	until = time.Now().Add(wait)
	for {
		_, err := replayLog.ReadAt(frameLength, 0)
		mustOK(err)
		if int32(binary.LittleEndian.Uint32(frameLength)) >= 32 {
			break
		}
		if time.Now().After(until) {
			fail("the replay wrote no frame at position 0 of %s within %v", replayed[0], wait)
		}
		time.Sleep(idle)
	}
	_, err = replayLog.WriteAt([]byte{9}, 4) // the frame's version byte
	mustOK(err)
	mustOK(replayLog.Close())
	var delivered []int64
	counted := func(body []byte, header *cavalry.Header, position int64) { delivered = append(delivered, position) }
	until = time.Now().Add(wait)
	for {
		polled, err := damaged.Poll(counted, 16)
		mustOK(err)
		if polled < 0 {
			break
		}
		if polled > 0 || len(delivered) > 0 {
			fail("a frame that fails its check was delivered: %v", delivered)
		}
		if time.Now().After(until) {
			fail("no poll met the damaged frame within %v", wait)
		}
		time.Sleep(idle)
	}
	metCode, metSession, metPosition, err := damaged.Fault()
	mustOK(err)
	if metCode != cavalry.ErrFaultImage || metPosition != 0 {
		fail("the poll that met the damaged frame stopped on fault %d %d %d, not ErrFaultImage at position 0", metCode,
			metSession, metPosition)
	}
	for polls := 0; polls < 20; polls++ {
		keptCode, keptSession, keptPosition, kept := damaged.Faulted()
		if !kept || keptCode != metCode || keptSession != metSession || keptPosition != metPosition {
			fail("after %d more polls Faulted is %d %d %d %v, not the fault the poll met, %d %d %d", polls, keptCode,
				keptSession, keptPosition, kept, metCode, metSession, metPosition)
		}
		polled, err := damaged.Poll(counted, 16)
		mustOK(err)
		cleared, _, _, err := damaged.Fault()
		mustOK(err)
		if polled != 0 || cleared != 0 {
			fail("a poll after the fault returned %d and left Fault at %d, not 0 and 0", polled, cleared)
		}
	}
	_, damagedFailed := damaged.FailedAt()
	_, damagedWithdrawn := damaged.WithdrawnAt()
	if !damaged.IsEnded() || damagedFailed || damagedWithdrawn || len(delivered) > 0 {
		fail("the faulted log read ended %v, failed %v, withdrawn %v, and delivered %v", damaged.IsEnded(), damagedFailed,
			damagedWithdrawn, delivered)
	}
	mustOK(damaged.Close())
	if client.IsClosed() {
		fail("a log whose image faulted closed its client: %d", client.Closed())
	}
	say("a log whose image faulted at position 0 read fault %d %d %d from the poll that met it and from Faulted after "+
		"twenty more polls", metCode, metSession, metPosition)

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
		if handlerError == nil {
			time.Sleep(idle)
		}
	}
	if !reflect.DeepEqual(seen, []int64{0}) || handlerError.Raised != 1 || handlerError.Cause != "the handler's own" ||
		panicking.Position() != next {
		fail("a panicking handler saw %v, returned %v, and left the log at %d", seen, handlerError, panicking.Position())
	}
	// The error is the log's own: it counts entries and names no subscription.
	if handlerError.Fragments != 1 || !handlerError.CommittedLog ||
		!strings.Contains(handlerError.Error(), "the entry handler panicked on 1 of the 1 entry") ||
		strings.Contains(handlerError.Error(), "Subscription") {
		fail("a committed log's HandlerError has Fragments %d, CommittedLog %v and says: %v", handlerError.Fragments,
			handlerError.CommittedLog, handlerError)
	}
	for i := 0; i < 50; i++ {
		count, err := panicking.Poll(panics, 16)
		mustOK(err)
		if count != 0 {
			fail("the entry whose handler panicked was delivered again")
		}
	}
	mustOK(panicking.Close())

	// From the next position nothing is committed yet: no entry, and no entry before it. A
	// replay's image joins a moment after the open, so polls made at once would pass before
	// the image is there, whatever the open did with the position. So a second log is
	// opened at 0 beside this one, on the same client, and both are polled until that one
	// has delivered the entry at 0 and this one has taken the member's commit position as
	// its bound, which a poll does once the log's image has joined (`CommittedLog::poll`,
	// crates/cavalry-client/src/committed_log.rs): by then this log's replay, asked for
	// first, has had the time the other's took to join and deliver.
	rest := openLog(client, next)
	if rest.Position() != next {
		fail("a log opened at %d began at %d", next, rest.Position())
	}
	beside := openLog(client, 0)
	var atNext, atStart []int64
	onNext := func(body []byte, header *cavalry.Header, position int64) { atNext = append(atNext, position) }
	onStart := func(body []byte, header *cavalry.Header, position int64) { atStart = append(atStart, position) }
	until = time.Now().Add(wait)
	for len(atStart) == 0 || rest.CommitPosition() < next {
		restCount, err := rest.Poll(onNext, 16)
		mustOK(err)
		besideCount, err := beside.Poll(onStart, 16)
		mustOK(err)
		if restCount < 0 || besideCount < 0 {
			fail("the polls of the logs at %d and at 0 returned %d and %d: %v and %v", next, restCount, besideCount,
				rest.Err(), beside.Err())
		}
		if time.Now().After(until) {
			fail("the log at 0 beside the log at %d delivered %v and the log at %d took the bound %d, not the entry at 0 "+
				"and %d, within %v", next, atStart, next, rest.CommitPosition(), next, wait)
		}
		if restCount == 0 && besideCount == 0 {
			time.Sleep(idle)
		}
	}
	for i := 0; i < 50; i++ {
		count, err := rest.Poll(onNext, 16)
		mustOK(err)
		if count < 0 {
			fail("the poll of the log at %d stopped: %v", next, rest.Err())
		}
	}
	if atStart[0] != 0 || len(atNext) != 0 || rest.Position() != next {
		fail("a log opened at %d delivered the entries at %v and is at %d, while a log opened at 0 beside it delivered "+
			"the entries at %v. An entry below %d is one the open did not start past; one at %d or above is a new "+
			"term's, of a cluster that elected again, which the members' output, printed by run.sh, shows", next, atNext,
			rest.Position(), atStart, next, next)
	}
	mustOK(beside.Close())
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
	say("a panicking handler's entry is not delivered again; nothing at position %d while a log at 0 beside it read "+
		"the entry at 0; a log closed after its client; the table's slots freed", next)
}
