// The Go client's closed round (spec/c-abi.md, "cluster service"; ADR 0155): a
// ClusterService whose driver stops. A poll with a client its driver closed is one answer,
// -1 with ErrClosed as the service's Err and as Fault's code, at the first poll that fails
// and at every poll after it, with the client's reason ClosedDriverClosed; and the
// service's close is then no error. The driver hosts no member, so the service holds no log
// and its poll has no command to send: the case in which a poll of a closed client took
// nothing and returned 0 for as long as it was called. conformance/go/run.sh runs it last,
// against the driver its quickstart ran on.
//
//	closed <the driver's process id>
//
// The client attaches to the driver at CAVALRY_DIR, and the program stops that driver
// itself, with SIGTERM to the process id it was given. A failed check prints what failed and
// exits 1; the wait is bounded at 60 s. Go's standard library only (ADR 0003).
package main

import (
	"errors"
	"fmt"
	"os"
	"strconv"
	"syscall"
	"time"

	cavalry "github.com/appshapes/cavalry/clients/go"
	"github.com/appshapes/cavalry/conformance/go/linked"
)

const (
	wait       = 60 * time.Second
	pollsAfter = 3
)

func fail(format string, args ...any) {
	fmt.Fprintf(os.Stderr, "conformance/go: closed round: "+format+"\n", args...)
	os.Exit(1)
}

func mustOK(err error) {
	if err != nil {
		fail("%v", err)
	}
}

// codeOf is the ABI's code of an error the client returned, and 0 for none.
func codeOf(err error) int32 {
	var failed *cavalry.Error
	if errors.As(err, &failed) {
		return failed.Code
	}
	return 0
}

func main() {
	linked.Check() // linked with the library run.sh has just built, or it stops here
	if len(os.Args) != 2 {
		fail("usage: closed <the driver's process id>")
	}
	pid, err := strconv.Atoi(os.Args[1])
	if err != nil || pid <= 1 {
		fail("usage: closed <the driver's process id>")
	}
	handed := 0
	onEntry := func(body []byte, entry *cavalry.ClusterEntryValues, service *cavalry.ClusterService) int32 {
		handed++
		return cavalry.ActionContinue
	}

	client, err := cavalry.NewClient("")
	mustOK(err)
	service, err := client.ClusterService()
	mustOK(err)

	// 1. Beside a driver that runs and hosts no member, a poll takes nothing.
	taken, err := service.Poll(onEntry, 10)
	mustOK(err)
	if taken != 0 || client.IsClosed() {
		fail("a poll beside a driver with no member returned %d, and the client reads closed %d; expected 0 beside a "+
			"driver that runs", taken, client.Closed())
	}

	// 2. The driver stops. Until the client has seen it, a poll takes nothing; the first
	// poll that fails is the close, and nothing else.
	mustOK(syscall.Kill(pid, syscall.SIGTERM))
	polls := 0
	for until := time.Now().Add(wait); ; {
		taken, err = service.Poll(onEntry, 10)
		mustOK(err)
		polls++
		if taken == -1 {
			break
		}
		if taken != 0 {
			fail("a poll beside a driver with no member returned %d; expected 0, or -1 after the stop", taken)
		}
		if time.Now().After(until) {
			fail("no poll failed within %v of the driver's stop; the client reads closed %d", wait, client.Closed())
		}
		time.Sleep(time.Millisecond)
	}
	closed := func(what string) {
		code, _, _, err := service.Fault()
		mustOK(err)
		if code != cavalry.ErrClosed || codeOf(service.Err()) != cavalry.ErrClosed {
			fail("%s stopped on code %d, with the service's Err %v; expected ErrClosed, %d, as both", what, code,
				service.Err(), cavalry.ErrClosed)
		}
	}
	closed("the first poll that failed after the driver's stop")
	if !client.IsClosed() || client.Closed() != cavalry.ClosedDriverClosed {
		fail("the poll said closed, and the client reads closed %d; expected ClosedDriverClosed, %d", client.Closed(),
			cavalry.ClosedDriverClosed)
	}

	// 3. Every poll after it is the same answer: a loop that looks at what its poll returned
	// ends. The service's Err is made another failure's before each, a reply from outside the
	// handler, since it is kept until the next failure and would otherwise be the first
	// failed poll's.
	for after := 1; after <= pollsAfter; after++ {
		if answer := service.Reply(1, []byte("x")); answer != -1 || codeOf(service.Err()) != cavalry.ErrClusterNoEntry {
			fail("a reply from outside the handler answered %d with %v, not -1 with ErrClusterNoEntry", answer, service.Err())
		}
		taken, err = service.Poll(onEntry, 10)
		mustOK(err)
		if taken != -1 {
			fail("poll %d after the first that failed returned %d; expected -1", after, taken)
		}
		closed(fmt.Sprintf("poll %d after the first that failed", after))
	}
	if handed != 0 {
		fail("the handler was called %d times beside a driver with no member", handed)
	}

	// 4. The service closes with no error, and its client after it: a driver that is gone
	// cannot be told.
	mustOK(service.Close())
	mustOK(client.Close())
	fmt.Printf("conformance/go: closed round: a service beside a driver with no member took nothing; after the driver's "+
		"stop, poll %d and the %d after it returned -1 with ErrClosed, the client's reason is ClosedDriverClosed, and "+
		"the service closed with no error\n", polls, pollsAfter)
	fmt.Println("conformance/go: closed round ok")
}
