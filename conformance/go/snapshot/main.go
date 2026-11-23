// The Go client's snapshot round (spec/c-abi.md, version 6, "A service's snapshot and its
// start"; ADR 0146, ADR 0147, ADR 0149): a service beside each of three cluster members that
// conformance/go/run.sh started with `cavalry run` takes a snapshot of its program's state
// that its member lists, and a service that starts again starts from it.
//
//	snapshot <cluster directory> <the cavalry binary>
//
// The members' drivers are <cluster directory>/member-0, -1 and -2. The round runs after the
// cluster round, on the log that round left. It is the Python runner's,
// conformance/python/snapshot.py, through this client (conformance/README.md, "The snapshot
// round"):
//
//  1. Each service asks where it starts, before its first poll: Restore answers
//     ClusterRestoredFromStart and no state, and a second is refused with
//     ErrClusterRestoreTooLate.
//  2. Each service reads an entry of the log. No snapshot is due, and Snapshot is refused
//     with ErrClusterNoSnapshotDue.
//  3. `cavalry snapshot` is run beside each member: beside the leader it exits 0 and prints
//     the snapshot's log position, and beside a follower it exits 1 and names the leader.
//  4. Each service's poll ends at the leader's request, handed over as ClusterEntrySnapshot,
//     and SnapshotDue is the position the command printed; the three services hold one
//     state there. From inside the handler at the request, SnapshotDue is false with the
//     thread's error at ErrPolling, and Snapshot and Restore are refused with it.
//  5. Each service gives its state: Snapshot is true. None is due after, and a second is
//     refused. The three services are then closed: one service beside a member takes
//     snapshots.
//  6. Beside each member a new service starts from the snapshot: Restore answers
//     ClusterRestoredState and the state as it was given, which only a member that lists the
//     snapshot names, and its position is the snapshot's.
//  7. `cavalry snapshot` is run beside the leader again. Each new service's first entry is
//     the request, at the first snapshot's position and no entry below it; its poll ends
//     there, and it gives its state. A start beside each member is then given that state.
//  8. A service that never asks where it starts reads the log from position 0. It meets the
//     first request with the state the first services gave, and the second with the state
//     the restarted ones hold. Its Snapshot is false both times.
//
// The program's state is how many entries it has read and a hash of each entry's type,
// position, term, time and payload, in order, written out 77 times, 2,472 bytes, so that a
// snapshot holds it in three messages: the same beside every member. Every wait is bounded
// at 60 s, and a snapshot that the driver gave up at a step is given again. It exits 1 with
// what it found when a check fails. Go's standard library only (ADR 0003).
package main

import (
	"bytes"
	"crypto/sha256"
	"encoding/binary"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	cavalry "github.com/appshapes/cavalry/clients/go"
)

const (
	members = 3
	wait    = 60 * time.Second
	idle    = 200 * time.Microsecond // between duty cycles: the members run in processes of their own
	copies  = 77                     // of the hash in a state: 2,472 bytes, three messages of a snapshot
	length  = 8 + sha256.Size*copies
)

func fail(format string, args ...any) {
	fmt.Fprintf(os.Stderr, "conformance/go: snapshot round: "+format+"\n", args...)
	os.Exit(1)
}

func mustOK(err error) {
	if err != nil {
		fail("%v", err)
	}
}

// codeOf is the ABI's code of err, 0 for nil and for an error that is not the ABI's.
func codeOf(err error) int32 {
	var abi *cavalry.Error
	if errors.As(err, &abi) {
		return abi.Code
	}
	return 0
}

// program is a service beside one member and its program: the state is how many entries it
// has read and a hash over them, which every member's service computes alike.
type program struct {
	member  int
	client  *cavalry.Client
	service *cavalry.ClusterService
	count   int64
	digest  [sha256.Size]byte
	entries []taken // every entry taken since this service began
	// What the three calls answered from inside the handler at the request: whether a
	// snapshot was due, the thread's error after that read, and the two calls' codes.
	inside      bool
	insideDue   bool
	insideCodes [3]int32
}

// taken is the type and the position of an entry a service was handed.
type taken struct {
	kind     int32
	position int64
}

// spent is a closed service, whose Position sets the thread's error to ErrNull.
var spent *cavalry.ClusterService

func start(cluster string, member int) *program {
	client, err := cavalry.NewClient(filepath.Join(cluster, fmt.Sprintf("member-%d", member)))
	mustOK(err)
	service, err := client.ClusterService()
	mustOK(err)
	return &program{member: member, client: client, service: service}
}

// state is the program's state as bytes: the count, eight bytes, and the hash 77 times.
func (p *program) state() []byte {
	out := make([]byte, 8, length)
	binary.LittleEndian.PutUint64(out, uint64(p.count))
	for i := 0; i < copies; i++ {
		out = append(out, p.digest[:]...)
	}
	return out
}

// take starts the program from state, as its snapshot held it.
func (p *program) take(state []byte) {
	if len(state) != length || !bytes.Equal(state[8:], bytes.Repeat(state[8:8+sha256.Size], copies)) {
		fail("a state of %d bytes that is not one the program gave", len(state))
	}
	p.count = int64(binary.LittleEndian.Uint64(state))
	copy(p.digest[:], state[8:8+sha256.Size])
}

// give gives the program's state to the service's snapshot: what it answered. A snapshot
// that a step of the driver's gave up is still due, and is given again.
func (p *program) give() (bool, error) {
	deadline := time.Now().Add(wait)
	for {
		written, err := p.service.Snapshot(p.state())
		if codeOf(err) != cavalry.ErrClusterSnapshotFailed || time.Now().After(deadline) {
			return written, err
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func (p *program) onEntry(body []byte, entry *cavalry.ClusterEntryValues, service *cavalry.ClusterService) int32 {
	var values [28]byte
	binary.LittleEndian.PutUint32(values[0:], uint32(entry.Type))
	binary.LittleEndian.PutUint64(values[4:], uint64(entry.Position))
	binary.LittleEndian.PutUint64(values[12:], uint64(entry.LeadershipTermID))
	binary.LittleEndian.PutUint64(values[20:], uint64(entry.Timestamp))
	hash := sha256.New()
	hash.Write(p.digest[:])
	hash.Write(values[:])
	hash.Write(body)
	p.count++
	copy(p.digest[:], hash.Sum(nil))
	p.entries = append(p.entries, taken{entry.Type, entry.Position})
	if entry.Type == cavalry.ClusterEntrySnapshot && !p.inside {
		// The poll holds the service and its client. The read answers false for a refusal
		// as for "none due", so the thread's error is set to another code first, as the
		// cluster round does, and must be ErrPolling after: cgo holds a handler to the
		// poll's thread.
		p.inside = true
		if spent.Position() != -1 || cavalry.Errcode() != cavalry.ErrNull {
			fail("a closed service's Position did not answer -1 with ErrNull: error %d", cavalry.Errcode())
		}
		_, p.insideDue = service.SnapshotDue()
		p.insideCodes[0] = cavalry.Errcode()
		_, err := service.Snapshot(p.state())
		p.insideCodes[1] = codeOf(err)
		_, _, err = service.Restore()
		p.insideCodes[2] = codeOf(err)
	}
	return cavalry.ActionContinue
}

func (p *program) poll() int {
	count, err := p.service.Poll(p.onEntry, 16)
	mustOK(err)
	if count < 0 {
		fail("the poll of the service beside member %d stopped: %v", p.member, p.service.Err())
	}
	return count
}

// restore asks where the service starts until its driver hosts a member with a log.
func (p *program) restore() (int32, []byte) {
	deadline := time.Now().Add(wait)
	for {
		restored, state, err := p.service.Restore()
		mustOK(err)
		if restored != cavalry.ClusterRestoredNoMember {
			return restored, state
		}
		if time.Now().After(deadline) {
			fail("the driver of member %d hosts no member with a log after %v", p.member, wait)
		}
		time.Sleep(time.Millisecond)
	}
}

func (p *program) close() {
	mustOK(p.service.Close())
	mustOK(p.client.Close())
}

func due(p *program) bool {
	_, is := p.service.SnapshotDue()
	return is
}

// waitFor polls every program's service that has no snapshot to give until done.
func waitFor(what string, done func() bool, programs []*program) {
	deadline := time.Now().Add(wait)
	for !done() {
		if time.Now().After(deadline) {
			fail("%s, after %v", what, wait)
		}
		for _, p := range programs {
			if !due(p) {
				p.poll()
			}
		}
		time.Sleep(idle)
	}
}

func all(programs []*program, is func(*program) bool) bool {
	for _, p := range programs {
		if !is(p) {
			return false
		}
	}
	return true
}

// ran is what `cavalry snapshot` did beside one member.
type ran struct {
	exit           int
	stdout, stderr string
}

// ask runs `cavalry snapshot` beside member, bounded at the round's wait.
func ask(binary, cluster string, member int) ran {
	var stdout, stderr bytes.Buffer
	command := exec.Command(binary, "snapshot", "--dir", filepath.Join(cluster, fmt.Sprintf("member-%d", member)))
	command.Stdout, command.Stderr = &stdout, &stderr
	mustOK(command.Start())
	ended := make(chan error, 1)
	go func() { ended <- command.Wait() }()
	exit := 0
	select {
	case err := <-ended:
		if err != nil {
			var exited *exec.ExitError
			if !errors.As(err, &exited) {
				fail("`cavalry snapshot` beside member %d did not run: %v", member, err)
			}
			exit = exited.ExitCode()
		}
	case <-time.After(wait):
		_ = command.Process.Kill()
		fail("`cavalry snapshot` beside member %d did not end in %v", member, wait)
	}
	return ran{exit, stdout.String(), stderr.String()}
}

// positionOf is the log position that `cavalry snapshot` printed beside member.
func positionOf(r ran, member int) int64 {
	prefix := fmt.Sprintf("member %d lists a snapshot of log position ", member)
	if r.exit != 0 || !strings.HasPrefix(r.stdout, prefix) {
		fail("`cavalry snapshot` beside member %d, exit %d: %s", member, r.exit, strings.TrimSpace(r.stdout+r.stderr))
	}
	number := strings.FieldsFunc(r.stdout[len(prefix):], func(c rune) bool { return c == ':' || c == ',' })[0]
	position, err := strconv.ParseInt(number, 10, 64)
	mustOK(err)
	return position
}

func main() {
	if len(os.Args) != 3 {
		fail("usage: snapshot <cluster directory> <the cavalry binary>")
	}
	cluster, binary := os.Args[1], os.Args[2]
	closed := start(cluster, 0)
	spent = closed.service
	closed.close()

	// 1. Where each service starts: no snapshot of a service is listed yet.
	var programs []*program
	for member := 0; member < members; member++ {
		programs = append(programs, start(cluster, member))
	}
	for _, p := range programs {
		if due(p) {
			fail("a snapshot is due beside member %d before its service's first poll", p.member)
		}
		if restored, state := p.restore(); restored != cavalry.ClusterRestoredFromStart || state != nil {
			fail("the service beside member %d starts with %d and %d bytes, not from the log's start",
				p.member, restored, len(state))
		}
		if _, _, err := p.service.Restore(); codeOf(err) != cavalry.ErrClusterRestoreTooLate {
			fail("a second restore beside member %d: %v", p.member, err)
		}
	}

	// 2. Every service reads an entry of the log the cluster round left; none is due.
	waitFor("every service read an entry", func() bool {
		return all(programs, func(p *program) bool { return p.count > 0 })
	}, programs)
	for _, p := range programs {
		if _, err := p.service.Snapshot(p.state()); codeOf(err) != cavalry.ErrClusterNoSnapshotDue {
			fail("a snapshot where none is due beside member %d: %v", p.member, err)
		}
	}

	// 3. `cavalry snapshot` beside each member: the leader's is taken, a follower's names
	// the leader.
	var asked []ran
	leader := -1
	for member := 0; member < members; member++ {
		asked = append(asked, ask(binary, cluster, member))
		if asked[member].exit == 0 {
			if leader >= 0 {
				fail("`cavalry snapshot` exited 0 beside members %d and %d", leader, member)
			}
			leader = member
		}
	}
	if leader < 0 {
		fail("`cavalry snapshot` exited 0 beside no member: %+v", asked)
	}
	position := positionOf(asked[leader], leader)
	for member, r := range asked {
		if member != leader && (r.exit != 1 || !strings.Contains(r.stderr, fmt.Sprintf("member %d leads", leader))) {
			fail("`cavalry snapshot` beside member %d, a follower, exit %d: %s", member, r.exit, strings.TrimSpace(r.stderr))
		}
	}

	// 4. Every service's poll ends at the request, and the snapshot is due at the position
	// the command printed, with one state beside the three members.
	waitFor("every service's poll ended at the leader's request", func() bool { return all(programs, due) }, programs)
	refused := [3]int32{cavalry.ErrPolling, cavalry.ErrPolling, cavalry.ErrPolling}
	for _, p := range programs {
		at, _ := p.service.SnapshotDue()
		last := p.entries[len(p.entries)-1]
		if at != position || p.service.Position() != position || last.kind != cavalry.ClusterEntrySnapshot {
			fail("beside member %d the snapshot is due at %d, the service is at %d and its last entry is %+v: "+
				"the command printed %d", p.member, at, p.service.Position(), last, position)
		}
		if !p.inside || p.insideDue || p.insideCodes != refused {
			fail("from inside the handler at the request, beside member %d: due %v, codes %v",
				p.member, p.insideDue, p.insideCodes)
		}
	}
	given := programs[0].state()
	if len(given) != length {
		fail("a state of %d bytes", len(given))
	}
	for _, p := range programs {
		if !bytes.Equal(p.state(), given) {
			fail("the service beside member %d holds another state at the snapshot's position", p.member)
		}
	}

	// 5. Each service gives its state, once, and is closed: the service that starts from
	// the snapshot is the one that takes the next.
	for _, p := range programs {
		if written, err := p.give(); err != nil || !written {
			fail("the service beside member %d gave its state: written %v, %v", p.member, written, err)
		}
		if due(p) {
			fail("a snapshot is still due beside member %d after it was taken", p.member)
		}
		if _, err := p.service.Snapshot(p.state()); codeOf(err) != cavalry.ErrClusterNoSnapshotDue {
			fail("a second snapshot beside member %d: %v", p.member, err)
		}
		p.close()
	}

	// 6. The member lists it: a new service beside each member starts from it. A member
	// lists the snapshot when it reads the service's word of it, a duty cycle or so later,
	// so a start that finds none is made again with a new service.
	var restarted []*program
	for member := 0; member < members; member++ {
		deadline := time.Now().Add(wait)
		var p *program
		var state []byte
		for {
			p = start(cluster, member)
			var restored int32
			restored, state = p.restore()
			if restored == cavalry.ClusterRestoredState {
				break
			}
			p.close()
			if time.Now().After(deadline) {
				fail("member %d lists no snapshot of its service after %v: a new service starts with %d",
					member, wait, restored)
			}
			time.Sleep(10 * time.Millisecond)
		}
		if !bytes.Equal(state, given) {
			fail("the service beside member %d starts from %d bytes that are not the state given", member, len(state))
		}
		p.take(state)
		if p.service.Position() != position {
			fail("the service beside member %d starts at %d, not at %d", member, p.service.Position(), position)
		}
		if _, _, err := p.service.Restore(); codeOf(err) != cavalry.ErrClusterRestoreTooLate {
			fail("a second restore beside member %d: %v", member, err)
		}
		restarted = append(restarted, p)
	}

	// 7. A second request, beside the leader: each restarted service's first entry is the
	// request, at the first snapshot's position, and it gives its state.
	second := positionOf(ask(binary, cluster, leader), leader)
	if second <= position {
		fail("a second snapshot at %d, not past the first at %d", second, position)
	}
	waitFor("every restarted service's poll ended at the second request",
		func() bool { return all(restarted, due) }, restarted)
	for _, p := range restarted {
		at, _ := p.service.SnapshotDue()
		if len(p.entries) != 1 || p.entries[0] != (taken{cavalry.ClusterEntrySnapshot, position}) || at != second {
			fail("the service beside member %d that started from its snapshot at %d was handed %+v, with a snapshot "+
				"due at %d: the second request is the one entry at %d, and asks for a snapshot at %d",
				p.member, position, p.entries, at, position, second)
		}
	}
	after := restarted[0].state()
	if bytes.Equal(after, given) || !all(restarted, func(p *program) bool { return bytes.Equal(p.state(), after) }) {
		fail("the restarted services do not hold one new state at the second snapshot's position")
	}
	for _, p := range restarted {
		if written, err := p.give(); err != nil || !written {
			fail("the restarted service beside member %d gave its state: written %v, %v", p.member, written, err)
		}
	}
	// Each member lists the second snapshot too: a start beside it is given the new state,
	// once the member has read the service's word of it.
	for member := 0; member < members; member++ {
		deadline := time.Now().Add(wait)
		for {
			probe := start(cluster, member)
			restored, state := probe.restore()
			at := probe.service.Position()
			probe.close()
			if restored == cavalry.ClusterRestoredState && bytes.Equal(state, after) && at == second {
				break
			}
			if time.Now().After(deadline) {
				fail("member %d does not list its service's second snapshot after %v: a new service starts with %d at %d",
					member, wait, restored, at)
			}
			time.Sleep(10 * time.Millisecond)
		}
	}

	// 8. A service that never asks reads from position 0 and meets both requests, each with
	// the state the others held there: a start from a snapshot and a read from the start
	// reach one state. Its member lists a snapshot at each, or past it.
	late := start(cluster, leader)
	for _, met := range []struct {
		at   int64
		held []byte
	}{{position, given}, {second, after}} {
		waitFor(fmt.Sprintf("a service that reads from the start meets the request for %d", met.at),
			func() bool { return due(late) }, []*program{late})
		if at, _ := late.service.SnapshotDue(); at != met.at || !bytes.Equal(late.state(), met.held) {
			fail("a service that read from the start is due at %d, not at %d, or holds another state than the "+
				"services held there", at, met.at)
		}
		if written, err := late.give(); err != nil || written {
			fail("a snapshot of position %d, which the member already lists or is past: written %v, %v", met.at, written, err)
		}
	}

	for _, p := range append(restarted, late) {
		p.close()
	}
	fmt.Printf("conformance/go: snapshot round: three services gave a state of %d bytes at log position %d, asked "+
		"for by `cavalry snapshot` beside member %d; each member listed its service's snapshot, a new service "+
		"beside each started from it, read the next request at %d and gave its state at %d\n",
		len(given), position, leader, position, second)
	fmt.Println("conformance/go: snapshot round ok")
}
