// The Go client's cluster round (spec/c-abi.md, versions 4 and 5; ADR 0122, ADR 0123, ADR
// 0124): Client.ClusterService and ClusterService beside each of three cluster members
// that conformance/go/run.sh started with `cavalry run`, and Client.ConnectCluster and
// ClusterSession on the runner's first driver, the one at CAVALRY_DIR, as
// conformance/python/cluster.py runs it through clients/python.
//
//	cluster <cluster directory> <members string> <response channel>
//
// The members' drivers are <cluster directory>/member-0, -1 and -2. The round is the C
// runner's, conformance/c/cluster.c, through the client, and then the client's own edges.
//
// The C runner's checks: a reply from outside the handler is refused; each service reads an
// entry before the session connects, since a service answers no entry that its member held
// when the service first opened the member's log; one message is handed to all three
// services as one entry, with one position, term, time and session; one service answers it,
// the one beside the leader, and the two others are answered ClusterReplyNotAnswering; the
// session reads one reply, that service's, with the entry's term and the cluster's time;
// each service holds one session, and its position is past the entry.
//
// The client's own edges: what Offer refuses; the calls the ABI refuses from inside a
// service's handler, on the service and on the client its poll holds, and from inside the
// session's, each made after the thread's error was set to another code, so that a refusal
// that answers what an open handle answers is told from an answer; the one struct of values
// a handler is handed, and the nil body of an entry that is no session's message; a
// service's handler that panics after it replied, and one that returns what is no action,
// whose entry is aborted, handed over again by the next poll and answered once; a reply too
// long for a frame and a reply to a session that is not open; a handler that returns
// ActionBreak with two entries waiting; a session's handler that panics, whose reply is not
// handed over again; the session's close as an entry every service is handed; a session
// and a service closed twice, each with its slot in the table freed, and a service closed
// after its client.
//
// Then a report, printed and never gated: how many polls the three services made, how many
// took over a millisecond, and the longest, which is how long a poll held its goroutine
// (clients/go/README.md, "The call").
//
// Every wait is bounded at 60 s, the time a cluster may take to commit on a busy host. It
// exits 1 with what it found when a check fails. Go's standard library only (ADR 0003).
package main

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"time"

	cavalry "github.com/appshapes/cavalry/clients/go"
)

const (
	members = 3
	wait    = 60 * time.Second
	idle    = 200 * time.Microsecond // between duty cycles: the members run in processes of their own
	settled = 300 * time.Millisecond // the time in which a second reply, or a second entry, would come
)

var errOwn = errors.New("the handler's own")

func fail(format string, args ...any) {
	fmt.Fprintf(os.Stderr, "conformance/go: cluster round: "+format+"\n", args...)
	os.Exit(1)
}

func say(format string, args ...any) {
	fmt.Printf("conformance/go: cluster round: "+format+"\n", args...)
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

// refusal is what one call answered, with the thread's error after it: a call that returns
// an error answers -1 with the error's code.
type refusal struct {
	call   string
	answer int64
	code   int32
}

// spent is a service that was closed before the round began: its Position hands the ABI a
// null handle, which answers -1 with ErrNull. The package exports no function of the ABI
// that takes a null header, so this is the round's call that sets the thread's error to a
// code no refusal gives. A success clears nothing (spec/c-abi.md, "Error codes"), so a
// refusal that answers what an open handle answers is told from an answer by the code
// alone, which must have changed from this one to ErrPolling.
var spent *cavalry.ClusterService

func setAnotherError() {
	if spent.Position() != -1 || cavalry.Errcode() != cavalry.ErrNull {
		fail("a closed service's Position did not answer -1 with ErrNull: error %d", cavalry.Errcode())
	}
}

func flag(value bool) int64 {
	if value {
		return 1
	}
	return 0
}

func failed(err error) int64 {
	if err != nil {
		return -1
	}
	return 0
}

// served is a service beside one member: what its handler was handed and what its calls
// answered. The handler answers a session's message with its member's id, a colon and the
// message, so that the reply a client reads says whose service sent it.
type served struct {
	member     int
	client     *cavalry.Client
	service    *cavalry.ClusterService
	entries    []cavalry.ClusterEntryValues            // every entry taken
	messages   map[string][]cavalry.ClusterEntryValues // a message's payload -> the entries it was taken as
	handed     map[string][]int64                      // a message's payload -> the positions it was handed over at, taken or not
	codes      map[string][]int32                      // a message's payload -> what each reply to it answered
	refusals   []refusal                               // what the calls from inside the handler answered
	nested     error                                   // the service's Err after the poll from inside its handler
	panicOn    string                                  // a payload whose first handling replies and then panics
	noActionOn string                                  // a payload whose first handling returns what is no action
	edgesOn    string                                  // a payload before whose reply a reply too long and one to no session are made
	edges      []int32
	breakOn    map[string]bool             // payloads whose entries are taken with ActionBreak
	values     *cavalry.ClusterEntryValues // the struct of values the handler was handed, the same one for every entry
	polls      int                         // the report: every poll, those over a millisecond, and the longest
	over       int
	longest    time.Duration
	longestOf  string
}

func serve(cluster string, member int) *served {
	client, err := cavalry.NewClient(filepath.Join(cluster, fmt.Sprintf("member-%d", member)))
	mustOK(err)
	service, err := client.ClusterService()
	mustOK(err)
	return &served{member: member, client: client, service: service, messages: map[string][]cavalry.ClusterEntryValues{},
		handed: map[string][]int64{}, codes: map[string][]int32{}, breakOn: map[string]bool{}, longestOf: "no poll"}
}

func (s *served) onEntry(body []byte, entry *cavalry.ClusterEntryValues, service *cavalry.ClusterService) int32 {
	if service != s.service {
		fail("the handler was handed another service than the one that was polled")
	}
	if s.values == nil {
		s.values = entry
	}
	if entry != s.values {
		fail("the handler was handed another struct of values than the one the service keeps")
	}
	values := *entry
	if entry.Type != cavalry.ClusterEntrySessionMessage || entry.SessionID < 0 {
		// The ABI's buffer points at no byte for these, and the client makes no slice of it.
		if body != nil {
			fail("an entry that is no session's message has a body that is not nil, of %d bytes: %+v", len(body), values)
		}
		s.entries = append(s.entries, values)
		return cavalry.ActionContinue
	}
	payload := string(body)
	s.handed[payload] = append(s.handed[payload], entry.Position)
	first := len(s.handed[payload]) == 1
	if s.noActionOn == payload && first {
		return 3 // Aeron's COMMIT, which Cavalry has no action for
	}
	if s.edgesOn == payload && s.edges == nil {
		tooLong := service.Reply(entry.SessionID, make([]byte, 1345))
		s.edges = []int32{tooLong, cavalry.Errcode(), codeOf(service.Err()), service.Reply(entry.SessionID+1000, []byte("x"))}
	}
	// Reply first, then change the state: an entry that is aborted is handed over again.
	code := service.Reply(entry.SessionID, []byte(fmt.Sprintf("%d:%s", s.member, payload)))
	if code < 0 {
		fail("a reply from inside the handler answered %d: %v", code, service.Err())
	}
	if code == cavalry.ClusterReplyRetry {
		s.handed[payload] = s.handed[payload][:len(s.handed[payload])-1]
		return cavalry.ActionAbort
	}
	if s.panicOn == payload && first {
		s.codes[payload] = append(s.codes[payload], code)
		panic(errOwn)
	}
	if s.refusals == nil {
		setAnotherError()
		count, _ := service.Poll(s.onEntry, 1)
		s.refusals = append(s.refusals, refusal{"Poll", int64(count), cavalry.Errcode()})
		s.nested = service.Err()
		setAnotherError()
		s.refusals = append(s.refusals, refusal{"Position", service.Position(), cavalry.Errcode()})
		setAnotherError()
		s.refusals = append(s.refusals, refusal{"Answers", flag(service.Answers()), cavalry.Errcode()})
		setAnotherError()
		s.refusals = append(s.refusals, refusal{"Sessions", int64(service.Sessions()), cavalry.Errcode()})
		setAnotherError()
		_, _, _, err := service.Fault()
		s.refusals = append(s.refusals, refusal{"Fault", failed(err), codeOf(err)})
		setAnotherError()
		err = service.Close()
		s.refusals = append(s.refusals, refusal{"Close", failed(err), codeOf(err)})
		setAnotherError()
		s.refusals = append(s.refusals, refusal{"Client.IsClosed", flag(s.client.IsClosed()), cavalry.Errcode()})
		setAnotherError()
		_, err = s.client.ClusterService()
		s.refusals = append(s.refusals, refusal{"Client.ClusterService", failed(err), codeOf(err)})
		setAnotherError()
		_, err = s.client.AddSubscription("cavalry:ipc", 1229)
		s.refusals = append(s.refusals, refusal{"Client.AddSubscription", failed(err), codeOf(err)})
	}
	s.codes[payload] = append(s.codes[payload], code)
	s.messages[payload] = append(s.messages[payload], values)
	s.entries = append(s.entries, values)
	if s.breakOn[payload] {
		return cavalry.ActionBreak
	}
	return cavalry.ActionContinue
}

// poll is one poll, timed: the count of entries taken, and the *HandlerError of a handler
// that failed, which is the caller's to read.
func (s *served) poll() (int, error) {
	before := len(s.entries)
	started := time.Now()
	count, err := s.service.Poll(s.onEntry, 16)
	took := time.Since(started)
	s.polls++
	if took > time.Millisecond {
		s.over++
	}
	if took > s.longest {
		s.longest = took
		s.longestOf = fmt.Sprintf("a poll of the service beside member %d that began after %d entries and took %d",
			s.member, before, len(s.entries)-before)
	}
	if count < 0 {
		fail("the poll of the service beside member %d stopped: %v", s.member, s.service.Err())
	}
	return count, err
}

type reply struct {
	payload   string
	term      int64
	timestamp int64
}

// answered is what the session's handler was handed.
type answered struct {
	session  *cavalry.ClusterSession
	replies  []reply
	refusals []refusal
	nested   error
	panicOn  string                      // a reply on which the handler panics
	values   *cavalry.ClusterReplyValues // the struct of values the handler was handed, the same one for every reply
}

func (a *answered) onReply(body []byte, values *cavalry.ClusterReplyValues) {
	if a.values == nil {
		a.values = values
	}
	if values != a.values {
		fail("the handler was handed another struct of values than the one the session keeps")
	}
	payload := string(body)
	a.replies = append(a.replies, reply{payload, values.LeadershipTermID, values.Timestamp})
	if a.refusals == nil {
		session := a.session
		setAnotherError()
		a.refusals = append(a.refusals, refusal{"Offer", session.Offer([]byte("x")), cavalry.Errcode()})
		a.nested = session.Err()
		setAnotherError()
		count, _ := session.Poll(a.onReply, 1)
		a.refusals = append(a.refusals, refusal{"Poll", int64(count), cavalry.Errcode()})
		setAnotherError()
		a.refusals = append(a.refusals, refusal{"LeadershipTermID", session.LeadershipTermID(), cavalry.Errcode()})
		setAnotherError()
		a.refusals = append(a.refusals, refusal{"LeaderMemberID", int64(session.LeaderMemberID()), cavalry.Errcode()})
		setAnotherError()
		a.refusals = append(a.refusals, refusal{"StaleReplies", session.StaleReplies(), cavalry.Errcode()})
		setAnotherError()
		_, closed := session.Closed()
		a.refusals = append(a.refusals, refusal{"Closed", flag(closed), cavalry.Errcode()})
		setAnotherError()
		_, _, _, err := session.Fault()
		a.refusals = append(a.refusals, refusal{"Fault", failed(err), codeOf(err)})
		setAnotherError()
		err = session.Close()
		a.refusals = append(a.refusals, refusal{"Close", failed(err), codeOf(err)})
	}
	if a.panicOn != "" && strings.HasSuffix(payload, a.panicOn) {
		panic(errOwn)
	}
}

// to is the replies to message, whichever member's service sent them.
func (a *answered) to(message string) []reply {
	var out []reply
	for _, r := range a.replies {
		if len(r.payload) > 2 && r.payload[2:] == message {
			out = append(out, r)
		}
	}
	return out
}

// cycle is one duty cycle: every service's poll but skip's, the session's when there is
// one, and a short sleep. It returns the first *HandlerError a poll returned, the leader's
// service's or the session's, which the caller expects or fails on.
func cycle(all []*served, a *answered, skip *served) error {
	var first error
	for _, s := range all {
		if s == skip {
			continue
		}
		if _, err := s.poll(); err != nil && first == nil {
			first = err
		}
	}
	if a != nil {
		count, err := a.session.Poll(a.onReply, 16)
		if count < 0 {
			fail("the session's poll stopped: %v", a.session.Err())
		}
		if err != nil && first == nil {
			first = err
		}
	}
	time.Sleep(idle)
	return first
}

func taken(all []*served) []int {
	out := make([]int, len(all))
	for i, s := range all {
		out[i] = len(s.entries)
	}
	return out
}

func waitFor(what string, done func() bool, all []*served, a *answered, skip *served) {
	until := time.Now().Add(wait)
	for !done() {
		if time.Now().After(until) {
			read := "no"
			if a != nil {
				read = fmt.Sprint(len(a.replies))
			}
			fail("%s: not within %v; the services took %v entries and the session read %s replies", what, wait, taken(all), read)
		}
		mustOK(cycle(all, a, skip))
	}
}

func settle(all []*served, a *answered, skip *served) {
	for until := time.Now().Add(settled); time.Now().Before(until); {
		mustOK(cycle(all, a, skip))
	}
}

func offer(session *cavalry.ClusterSession, message string, all []*served, a *answered) {
	until := time.Now().Add(wait)
	for {
		offered := session.Offer([]byte(message))
		if offered >= 0 {
			return
		}
		if offered == cavalry.PublicationError {
			fail("the offer of %q failed: %v", message, session.Err())
		}
		if offered == cavalry.PublicationClosed {
			reason, _ := session.Closed()
			fail("the session closed before %q: %s", message, reason)
		}
		if time.Now().After(until) {
			fail("the session's publication did not take %q within %v: %d", message, wait, offered)
		}
		mustOK(cycle(all, a, nil))
	}
}

// handlerError is the *HandlerError of the leader's service's next poll that returns one,
// with the other services and the session polled meanwhile.
func handlerError(leader *served, all []*served, a *answered, what string) *cavalry.HandlerError {
	until := time.Now().Add(wait)
	for {
		if _, err := leader.poll(); err != nil {
			var handler *cavalry.HandlerError
			if !errors.As(err, &handler) {
				fail("%s: the leader's service's poll returned %v, not a *HandlerError", what, err)
			}
			return handler
		}
		if time.Now().After(until) {
			fail("%s: no poll of the leader's service returned a *HandlerError within %v", what, wait)
		}
		mustOK(cycle(all, a, leader))
	}
}

func all(services []*served, has func(*served) bool) bool {
	for _, s := range services {
		if !has(s) {
			return false
		}
	}
	return true
}

func main() {
	if len(os.Args) != 4 {
		fail("usage: cluster <cluster directory> <members string> <response channel>")
	}
	cluster, membersString, responseChannel := os.Args[1], os.Args[2], os.Args[3]
	held := cavalry.Registered()

	// 1. A service beside each member, over a client on that member's driver.
	var services []*served
	for member := 0; member < members; member++ {
		services = append(services, serve(cluster, member))
	}
	if cavalry.Registered() != held+members {
		fail("three open services hold %d slots of the table, not three", cavalry.Registered()-held)
	}
	// The service the round sets the thread's error with, closed at once: its slot is freed.
	var err error
	spent, err = services[0].client.ClusterService()
	mustOK(err)
	mustOK(spent.Close())
	if cavalry.Registered() != held+members {
		fail("a service's close left the table holding %d slots, not the three open services'", cavalry.Registered()-held)
	}
	// The handler's reply is for the handler: from here there is no entry.
	if answer := services[0].service.Reply(1, []byte("x")); answer != -1 || codeOf(services[0].service.Err()) != cavalry.ErrClusterNoEntry {
		fail("a reply from outside the handler answered %d with %v, not -1 with ErrClusterNoEntry", answer, services[0].service.Err())
	}

	// 2. Each service opens its member's log and reads the term's first entry before the
	// session connects: a service answers no entry that its member held when the service
	// first opened the log (spec/cluster.md, "Replies").
	waitFor("each service reads an entry of its member's log",
		func() bool { return all(services, func(s *served) bool { return len(s.entries) > 0 }) }, services, nil, nil)

	// 3. A session with the cluster, on the driver at CAVALRY_DIR. The connect returns once
	// the leader has opened the session.
	client, err := cavalry.NewClient("")
	mustOK(err)
	session, err := client.ConnectCluster(membersString, responseChannel)
	mustOK(err)
	a := &answered{session: session}
	if reason, closed := session.Closed(); closed || session.SessionID < 0 || session.MaxMessageLength <= 0 {
		fail("a session just opened reads closed %v (%q), id %d and a longest message of %d bytes", closed, reason,
			session.SessionID, session.MaxMessageLength)
	}
	if cavalry.Registered() != held+members+1 {
		fail("an open session holds %d slots of the table, not one", cavalry.Registered()-held-members)
	}
	// What Offer refuses: a message longer than one frame carries.
	if offered := session.Offer(make([]byte, session.MaxMessageLength+1)); offered != cavalry.PublicationError ||
		codeOf(session.Err()) != cavalry.ErrTooLong {
		fail("a message of %d bytes answered %d with %v, not PublicationError with ErrTooLong", session.MaxMessageLength+1,
			offered, session.Err())
	}

	// 4. One message, taken by every service and answered by one.
	offer(session, "hello", services, a)
	waitFor("the message is taken by three services and answered", func() bool {
		return len(a.replies) > 0 && all(services, func(s *served) bool { return len(s.messages["hello"]) > 0 })
	}, services, a, nil)
	settle(services, a, nil)
	if len(a.replies) != 1 {
		fail("the session read %d replies to one message: %v", len(a.replies), a.replies)
	}

	// 5. The same entry beside every member, answered by the leader's service alone.
	var leader *served
	first := services[0].messages["hello"][0]
	polling := int32(cavalry.ErrPolling)
	for _, s := range services {
		if !reflect.DeepEqual(s.messages["hello"], []cavalry.ClusterEntryValues{first}) || first.SessionID != session.SessionID {
			fail("the service beside member %d took the message as %+v, and the service beside member 0 as %+v, of session %d",
				s.member, s.messages["hello"], first, session.SessionID)
		}
		if s.service.Sessions() != 1 {
			fail("the service beside member %d holds %d sessions open, not one", s.member, s.service.Sessions())
		}
		if s.service.Position() <= first.Position {
			fail("the service beside member %d is at %d, not past the entry at %d", s.member, s.service.Position(), first.Position)
		}
		if code, _, _, err := s.service.Fault(); err != nil || code != 0 {
			fail("the service beside member %d reads fault %d and %v after polls that took entries", s.member, code, err)
		}
		expected := []refusal{{"Poll", -1, polling}, {"Position", -1, polling}, {"Answers", 0, polling},
			{"Sessions", -1, polling}, {"Fault", -1, polling}, {"Close", -1, polling}, {"Client.IsClosed", 0, polling},
			{"Client.ClusterService", -1, polling}, {"Client.AddSubscription", -1, polling}}
		if !reflect.DeepEqual(s.refusals, expected) || codeOf(s.nested) != polling {
			fail("the calls from inside the handler of the service beside member %d answered %v, not %v, and the refused "+
				"poll left Err %v", s.member, s.refusals, expected, s.nested)
		}
		switch {
		case reflect.DeepEqual(s.codes["hello"], []int32{cavalry.ClusterReplySent}):
			if leader != nil {
				fail("the services beside members %d and %d both answered one entry", leader.member, s.member)
			}
			if !s.service.Answers() {
				fail("the service beside member %d answered and says it does not answer", s.member)
			}
			leader = s
		case !reflect.DeepEqual(s.codes["hello"], []int32{cavalry.ClusterReplyNotAnswering}) || s.service.Answers():
			fail("the service beside member %d, a follower, answered %v and says it answers: %v", s.member,
				s.codes["hello"], s.service.Answers())
		}
	}
	if leader == nil {
		fail("no service answered the entry")
	}
	if want := []reply{{fmt.Sprintf("%d:hello", leader.member), first.LeadershipTermID, first.Timestamp}}; !reflect.DeepEqual(a.replies, want) {
		fail("the reply %v is not the leader's service's, of member %d, with the entry's term %d and the cluster's time %d",
			a.replies, leader.member, first.LeadershipTermID, first.Timestamp)
	}
	expected := []refusal{{"Offer", cavalry.PublicationError, polling}, {"Poll", -1, polling}, {"LeadershipTermID", -1, polling},
		{"LeaderMemberID", -1, polling}, {"StaleReplies", -1, polling}, {"Closed", 0, polling}, {"Fault", -1, polling},
		{"Close", -1, polling}}
	if !reflect.DeepEqual(a.refusals, expected) || codeOf(a.nested) != polling {
		fail("the calls from inside the session's handler answered %v, not %v, and the refused offer left Err %v",
			a.refusals, expected, a.nested)
	}
	code, _, _, err := session.Fault()
	mustOK(err)
	if _, closed := session.Closed(); int(session.LeaderMemberID()) != leader.member ||
		session.LeadershipTermID() != first.LeadershipTermID || closed || code != 0 {
		fail("the session sends to member %d in term %d, reads closed %v and fault %d; the entry's leader is member %d "+
			"and its term %d", session.LeaderMemberID(), session.LeadershipTermID(), closed, code, leader.member,
			first.LeadershipTermID)
	}
	var terms, opens []cavalry.ClusterEntryValues
	for _, e := range leader.entries {
		switch e.Type {
		case cavalry.ClusterEntryNewLeadershipTerm:
			terms = append(terms, e)
		case cavalry.ClusterEntrySessionOpen:
			opens = append(opens, e)
		}
	}
	if len(terms) == 0 || int(terms[len(terms)-1].LeaderMemberID) != leader.member || len(opens) != 1 ||
		opens[0].SessionID != session.SessionID {
		fail("the leader's service read the new leadership term events %+v and the session open events %+v; the leader "+
			"is member %d and the session %d", terms, opens, leader.member, session.SessionID)
	}
	say("session %d's message, the entry at position %d of term %d, taken by three services and answered once, by the "+
		"service beside member %d; nine calls from inside a service's handler and eight from inside the session's "+
		"refused with ErrPolling; an entry that is no session's message has a nil body", session.SessionID,
		first.Position, first.LeadershipTermID, leader.member)

	// 6. A handler that replies and then panics: its entry is aborted, so the poll ends at
	// it with the entry not taken, Poll returns a *HandlerError, and the next poll hands the
	// entry over again; the reply the first handling sent is not sent again.
	leader.panicOn = "panic"
	before := leader.service.Position()
	offer(session, "panic", services, a)
	handler := handlerError(leader, services, a, "a handler that panics")
	if handler.Cluster != "service" || handler.Raised != 1 || !errors.Is(handler, errOwn) || handler.Fragments < 0 ||
		handler.CommittedLog || !strings.Contains(handler.Error(), "Its entry was aborted") {
		fail("a service's HandlerError has Cluster %q, Fragments %d, Raised %d and Cause %v, and says: %v", handler.Cluster,
			handler.Fragments, handler.Raised, handler.Cause, handler)
	}
	aborted := leader.handed["panic"][0]
	if at := leader.service.Position(); at < before || at > aborted || len(leader.messages["panic"]) != 0 {
		fail("after its handler panicked on the entry at %d the service is at %d, from %d, and took %+v", aborted, at,
			before, leader.messages["panic"])
	}
	waitFor("the aborted entry is handed over again and its one reply read", func() bool {
		return len(a.to("panic")) > 0 && all(services, func(s *served) bool { return len(s.messages["panic"]) > 0 })
	}, services, a, nil)
	settle(services, a, nil)
	sent := []int32{cavalry.ClusterReplySent, cavalry.ClusterReplySent}
	if !reflect.DeepEqual(leader.handed["panic"], []int64{aborted, aborted}) || !reflect.DeepEqual(leader.codes["panic"], sent) ||
		len(a.to("panic")) != 1 {
		fail("the entry whose handler panicked was handed over at %v, its replies answered %v, and the session read %v",
			leader.handed["panic"], leader.codes["panic"], a.to("panic"))
	}

	// 7. A handler that returns what is no action, as a panic: aborted, returned by Poll
	// with the ABI's own error, and handed over again. On the second handling, before the
	// reply: a reply too long for a frame, and a reply to a session that is not open.
	leader.noActionOn, leader.edgesOn = "no action", "no action"
	offer(session, "no action", services, a)
	handler = handlerError(leader, services, a, "a handler that returns what is no action")
	var refused *cavalry.Error
	if cause, ok := handler.Cause.(error); !ok || !errors.As(cause, &refused) || handler.Cluster != "service" ||
		handler.Raised != 1 || refused.Code != cavalry.ErrClusterAction || refused.Fn != "cavalry_cluster_service_action" ||
		!strings.Contains(refused.Message, "3 is no action") {
		fail("a handler that returned 3 made Poll return %v, with Cluster %q, Raised %d and Cause %v", handler,
			handler.Cluster, handler.Raised, handler.Cause)
	}
	waitFor("the entry with no action is handed over again and answered", func() bool {
		return len(a.to("no action")) > 0 && all(services, func(s *served) bool { return len(s.messages["no action"]) > 0 })
	}, services, a, nil)
	settle(services, a, nil)
	if handed := leader.handed["no action"]; len(handed) != 2 || handed[0] != handed[1] || len(a.to("no action")) != 1 {
		fail("the entry whose handler returned no action was handed over at %v and the session read %v", handed,
			a.to("no action"))
	}
	edges := []int32{cavalry.ClusterReplyTooLong, cavalry.ErrTooLong, cavalry.ErrTooLong, cavalry.ClusterReplyNoSession}
	if !reflect.DeepEqual(leader.edges, edges) || !reflect.DeepEqual(leader.codes["no action"], []int32{cavalry.ClusterReplySent}) {
		fail("a reply of 1,345 bytes, the thread's error and the service's Err after it, and a reply to session %d "+
			"answered %v, not %v, and the reply after them %v", session.SessionID+1000, leader.edges, edges,
			leader.codes["no action"])
	}

	// 8. ActionBreak: with two entries waiting, a poll takes one and ends. The leader's
	// service is not polled until the two others have taken both entries and a moment more,
	// so that its member has committed both.
	leader.breakOn = map[string]bool{"break 1": true, "break 2": true}
	offer(session, "break 1", services, a)
	offer(session, "break 2", services, a)
	waitFor("the followers' services take two more entries", func() bool {
		return all(services, func(s *served) bool { return s == leader || len(s.messages["break 2"]) > 0 })
	}, services, a, leader)
	settle(services, a, leader)
	var counts []int
	for until := time.Now().Add(wait); len(leader.messages["break 2"]) == 0; {
		if time.Now().After(until) {
			fail("the leader's service took %v entries in its polls and not both entries", counts)
		}
		count, err := leader.poll()
		mustOK(err)
		if count != 0 {
			counts = append(counts, count)
		} else {
			mustOK(cycle(services, a, leader))
		}
	}
	if !reflect.DeepEqual(counts, []int{1, 1}) {
		fail("with ActionBreak returned for each of two entries the leader's polls took %v, not one each", counts)
	}
	waitFor("the two replies are read", func() bool { return len(a.to("break 1")) > 0 && len(a.to("break 2")) > 0 },
		services, a, nil)
	if last := a.replies[len(a.replies)-2:]; last[0].payload[2:] != "break 1" || last[1].payload[2:] != "break 2" {
		fail("the replies to two messages came out of order: %v", last)
	}

	// 9. The session's handler panics: the reply was handed over, Poll returns a
	// *HandlerError once the ABI has returned, and the reply is not handed over again.
	a.panicOn = "reply panics"
	offer(session, "reply panics", services, a)
	handler = nil
	for until := time.Now().Add(wait); handler == nil; {
		if time.Now().After(until) {
			fail("the reply on which the session's handler panics was not read")
		}
		if err := cycle(services, a, nil); err != nil && !errors.As(err, &handler) {
			fail("a poll returned %v, not a *HandlerError", err)
		}
	}
	a.panicOn = ""
	if handler.Cluster != "session" || handler.Fragments != 1 || handler.Raised != 1 || !errors.Is(handler, errOwn) ||
		!strings.Contains(handler.Error(), "the reply handler panicked on 1 of the 1 reply this poll handed over") {
		fail("a session's HandlerError has Cluster %q, Fragments %d, Raised %d and Cause %v, and says: %v", handler.Cluster,
			handler.Fragments, handler.Raised, handler.Cause, handler)
	}
	settle(services, a, nil)
	if len(a.to("reply panics")) != 1 || session.StaleReplies() != 0 {
		fail("the reply whose handler panicked was read %d times, and the session passed over %d stale replies",
			len(a.to("reply panics")), session.StaleReplies())
	}
	say("a handler that panicked and one that returned no action had their entries aborted, handed over again and " +
		"answered once; a reply of 1,345 bytes and a reply to no session refused; ActionBreak took one of two entries; " +
		"a reply whose handler panicked is not handed over again")

	// 10. The session before its client. Its close is an entry every service is handed,
	// after which the log holds no session open.
	mustOK(session.Close())
	mustOK(session.Close()) // idempotent
	if reason, closed := session.Closed(); closed {
		fail("a session that Close freed reads closed: %q", reason)
	}
	if cavalry.Registered() != held+members {
		fail("the session's close left the table holding %d slots, not the three open services'", cavalry.Registered()-held)
	}
	mustOK(client.Close())
	closes := func(s *served) []cavalry.ClusterEntryValues {
		var out []cavalry.ClusterEntryValues
		for _, e := range s.entries {
			if e.Type == cavalry.ClusterEntrySessionClose {
				out = append(out, e)
			}
		}
		return out
	}
	waitFor("each service reads the session's close",
		func() bool { return all(services, func(s *served) bool { return len(closes(s)) > 0 }) }, services, nil, nil)
	for _, s := range services {
		closed := closes(s)
		if len(closed) != 1 || closed[0].SessionID != first.SessionID || closed[0].TimedOut || s.service.Sessions() != 0 {
			fail("the service beside member %d read the session close events %+v and holds %d sessions open; the session "+
				"%d was closed by its client", s.member, closed, s.service.Sessions(), first.SessionID)
		}
		if !reflect.DeepEqual(s.entries, services[0].entries) {
			fail("the services beside members 0 and %d took different entries: %+v and %+v", s.member, services[0].entries,
				s.entries)
		}
	}

	// 11. Each service before its own client, twice; and the last after its client.
	for _, s := range services[:members-1] {
		mustOK(s.service.Close())
		mustOK(s.service.Close()) // idempotent
		mustOK(s.client.Close())
	}
	last := services[members-1]
	mustOK(last.client.Close())
	mustOK(last.service.Close())
	if cavalry.Registered() != held {
		fail("the table holds %d slots after every close, %d before the round", cavalry.Registered(), held)
	}
	polls, over, longest := 0, 0, services[0]
	for _, s := range services {
		polls += s.polls
		over += s.over
		if s.longest > longest.longest {
			longest = s
		}
	}
	say("%d entries taken by each of three services; the session's close read by each; a service closed after its "+
		"client; every slot of the table freed", len(services[0].entries))
	say("report: %d polls of the three services, each holding its goroutine for its run: %d took over 1 ms, and the "+
		"longest %d us, %s (not gated)", polls, over, longest.longest.Microseconds(), longest.longestOf)
	fmt.Println("conformance/go: cluster round ok")
}
