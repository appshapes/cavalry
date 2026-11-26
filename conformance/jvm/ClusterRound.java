// The JVM client's cluster round (spec/c-abi.md, versions 4, 5 and 7; ADR 0122, ADR 0123,
// ADR 0124, ADR 0157): Client.clusterService and ClusterService beside each of three cluster members
// that conformance/jvm/run.sh started with `cavalry run`, and Client.connectCluster and
// ClusterSession on the runner's first driver, the one at CAVALRY_DIR, as
// conformance/python/cluster.py runs it through clients/python.
//
//     ClusterRound <cluster directory> <members string> <response channel>
//
// The members' drivers are <cluster directory>/member-0, -1 and -2. The round is the C
// runner's, conformance/c/cluster.c, through the client, and then the client's own edges.
//
// The C runner's checks: a reply from outside the handler is refused; each service reads an
// entry before the session connects, since a service answers no entry that its member held
// when the service first opened the member's log; one message is handed to all three
// services as one entry, with one position, term, time and session; one service answers it,
// the one beside the leader, and the two others are answered CLUSTER_REPLY_NOT_ANSWERING;
// the session reads one reply, that service's, with the entry's term and the cluster's
// time; each service holds one session, and its position is past the entry.
//
// The client's own edges: what offer refuses, and that it takes a message over a byte
// array; the calls the ABI refuses from inside a service's handler, on the service and on
// the client its poll holds, the client's own close among them, which must leave the
// client open, and from inside the session's, each made after the thread's
// error was set to another code, so that a refusal that answers what an open handle answers
// is told from an answer; the one object of values a handler is handed, and the body of no
// bytes of an entry that is no session's message; a service's handler that throws after it
// replied, and one that returns what is no action, whose entry is aborted, handed over
// again by the next poll and answered once; a reply too long for a frame and a reply to a
// session that is not open; a handler that returns ACTION_BREAK with two entries waiting; a
// session's handler that throws, whose reply is not handed over again; the program's
// timers, scheduleTimer and cancelTimer from inside the handler, as the C runner's round
// has one: a timer handed to every service once, as an entry of its own type with the
// timer's id, at one position and no earlier than the deadline its second schedule set, a
// second after its entry, where its first was an hour away, and not again within a timer
// refresh interval and more; a timer due at once and cancelled in the same handling handed
// to none; the least and the greatest long as ids, handed back as they are; both calls
// refused from outside the handler; the session's close
// as an entry every service is handed; a session and a service closed twice, and a service
// closed after its client.
//
// Then a report, printed and never gated: how many polls the three services made, how many
// took over a millisecond, and the longest, which is how long a poll held its thread
// (clients/jvm/README.md, "The call").
//
// Every wait is bounded at 60 s, the time a cluster may take to commit on a busy host. It
// exits 1 with what it found when a check fails. The JDK alone (ADR 0003).

import com.appshapes.cavalry.Abi;
import com.appshapes.cavalry.CavalryException;
import com.appshapes.cavalry.Client;
import com.appshapes.cavalry.ClusterEntryValues;
import com.appshapes.cavalry.ClusterReplyValues;
import com.appshapes.cavalry.ClusterService;
import com.appshapes.cavalry.ClusterSession;
import com.appshapes.cavalry.HandlerException;

import java.lang.foreign.MemorySegment;
import java.lang.foreign.ValueLayout;
import java.nio.charset.StandardCharsets;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.HashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.function.BooleanSupplier;
import java.util.function.Predicate;
import java.util.function.Supplier;

public final class ClusterRound {
    static final int MEMBERS = 3;
    static final long WAIT_NANOS = 60_000_000_000L;
    static final long IDLE_NANOS = 200_000; // between duty cycles: the members run in processes of their own
    static final long SETTLE_NANOS = 300_000_000; // the time in which a second reply, or a second entry, would come
    static final String TIMER_MESSAGE = "timer"; // the message at which each service's program schedules its timers
    static final long TIMER_ID = 77; // the timer that is handed over
    static final long CANCELLED_ID = 78; // the timer that is scheduled and cancelled in the same handling
    // How long after the message's entry the timer is due, in the cluster's milliseconds. A
    // second: the event of a timer that is due at once is stamped tens of milliseconds
    // after its message, the time an entry takes to be committed and read by a service on a
    // laptop, and more on a busy host, so the event of a timer that was wrongly due at once
    // is stamped that late, and a margin of that size would let it pass. Before ADR 0168 a
    // service's first timer waited for its publication to its member too, 38 to 68 ms in
    // the five clients' rounds as they were run on a laptop. The round prints what it was
    // in its own run.
    static final long TIMER_AFTER_MS = 1000;
    static final long HOUR_MS = 3_600_000; // the deadline its first schedule gives it, which the second replaces
    static final long REFRESH_NANOS = 1_200_000_000L; // a timer refresh interval of spec/cluster.md, "Values", 1 s, and more

    private ClusterRound() {
    }

    static void fail(String what) {
        System.err.println("conformance/jvm: cluster round: " + what);
        System.exit(1);
    }

    static void say(String what) {
        System.out.println("conformance/jvm: cluster round: " + what);
    }

    /** A message as a heap segment over its bytes, which offer and reply take as they are. */
    static MemorySegment bytes(String text) {
        return MemorySegment.ofArray(text.getBytes(StandardCharsets.ISO_8859_1));
    }

    static String text(MemorySegment body) {
        return new String(body.toArray(ValueLayout.JAVA_BYTE), StandardCharsets.ISO_8859_1);
    }

    /** One committed entry, as its handler read it. */
    record Entry(int type, long position, long term, long timestamp, long session, int leader, boolean timedOut) {
        static Entry of(ClusterEntryValues entry) {
            return new Entry(entry.type(), entry.position(), entry.leadershipTermId(), entry.timestamp(),
                entry.sessionId(), entry.leaderMemberId(), entry.timedOut());
        }
    }

    /** A timer that was handed over: where, its id and the event's time. */
    record Timer(long position, long id, long timestamp) {
    }

    /** What one call answered, with the thread's error after it: a call that throws answers
     * -1 with the exception's code. */
    record Refusal(String call, long answer, int code) {
    }

    record Reply(String payload, long term, long timestamp) {
    }

    /** A service that was closed before the round began: its position() hands the ABI a null
     * handle, which answers -1 with ERR_NULL. The package's bindings of the ABI are not
     * public, so this is the round's call that sets the thread's error to a code no refusal
     * gives. A success clears nothing (spec/c-abi.md, "Error codes"), so a refusal that
     * answers what an open handle answers is told from an answer by the code alone, which
     * must have changed from this one to ERR_POLLING. */
    static ClusterService spent;

    static void setAnotherError() {
        if (spent.position() != -1 || Abi.errcode() != Abi.ERR_NULL) {
            fail("a closed service's position() did not answer -1 with ERR_NULL: error " + Abi.errcode());
        }
    }

    /** What a call that answers a number answered, with the thread's error after it. */
    static Refusal asked(String call, Supplier<Number> made) {
        setAnotherError();
        return new Refusal(call, made.get().longValue(), Abi.errcode());
    }

    /** What a call that throws for a refusal did: 0 with the thread's error when it
     * returned, and -1 with the exception's code when it threw. */
    static Refusal thrown(String call, Runnable made) {
        setAnotherError();
        try {
            made.run();
            return new Refusal(call, 0, Abi.errcode());
        } catch (CavalryException refused) {
            return new Refusal(call, -1, refused.code());
        }
    }

    /** A service beside one member: what its handler was handed and what its calls
     * answered. The handler answers a session's message with its member's id, a colon and
     * the message, so that the reply a client reads says whose service sent it. */
    static final class Served {
        final int member;
        final Client client;
        final ClusterService service;
        final List<Entry> entries = new ArrayList<>(); // every entry taken
        final Map<String, List<Entry>> messages = new HashMap<>(); // a payload -> the entries it was taken as
        final Map<String, List<Long>> handed = new HashMap<>(); // a payload -> where it was handed over, taken or not
        final Map<String, List<Integer>> codes = new HashMap<>(); // a payload -> what each reply to it answered
        List<Refusal> refusals; // what the calls from inside the handler answered
        String throwOn; // a payload whose first handling replies and then throws
        String noActionOn; // a payload whose first handling returns what is no action
        String edgesOn; // a payload before whose reply a reply too long and one to no session are made
        List<Integer> edges;
        final Set<String> breakOn = new HashSet<>(); // payloads whose entries are taken with ACTION_BREAK
        final List<Timer> timers = new ArrayList<>(); // every timer handed over
        long timerAt; // when the last was handed over, by System.nanoTime
        List<Integer> timerCodes = List.of(); // what the schedules and the cancel at TIMER_MESSAGE answered
        ClusterEntryValues values; // the object of values the handler was handed, the same one for every entry
        int polls; // the report: every poll, those over a millisecond, and the longest
        int over;
        long longest;
        String longestOf = "no poll";

        Served(String cluster, int member) {
            this.member = member;
            this.client = new Client(Path.of(cluster, "member-" + member).toString());
            this.service = client.clusterService();
        }

        List<Entry> taken(String payload) {
            return messages.getOrDefault(payload, List.of());
        }

        int onEntry(MemorySegment body, ClusterEntryValues entry, ClusterService handedService) {
            if (handedService != service) {
                fail("the handler was handed another service than the one that was polled");
            }
            if (values == null) {
                values = entry;
            }
            if (entry != values) {
                fail("the handler was handed another object of values than the one the service keeps");
            }
            Entry read = Entry.of(entry);
            if (entry.type() != Abi.CLUSTER_ENTRY_SESSION_MESSAGE || entry.sessionId() < 0) {
                if (body.byteSize() != 0) {
                    fail("an entry that is no session's message has a payload of " + body.byteSize() + " bytes: " + read);
                }
                if (entry.type() == Abi.CLUSTER_ENTRY_TIMER) {
                    timers.add(new Timer(entry.position(), entry.correlationId(), entry.timestamp()));
                    timerAt = System.nanoTime();
                } else if (entry.correlationId() != 0) {
                    fail("an entry that is no timer has the correlation id " + entry.correlationId() + ": " + read);
                }
                entries.add(read);
                return Abi.ACTION_CONTINUE;
            }
            if (entry.correlationId() != 0) {
                fail("a session's message has the correlation id " + entry.correlationId() + ": " + read);
            }
            String payload = text(body);
            List<Long> positions = handed.computeIfAbsent(payload, key -> new ArrayList<>());
            positions.add(entry.position());
            boolean first = positions.size() == 1;
            if (payload.equals(noActionOn) && first) {
                return 3; // Aeron's COMMIT, which Cavalry has no action for
            }
            if (payload.equals(edgesOn) && edges == null) {
                int tooLong = service.reply(entry.sessionId(), MemorySegment.ofArray(new byte[1345]));
                edges = List.of(tooLong, Abi.errcode(), service.reply(entry.sessionId() + 1000, bytes("x")));
            }
            // Reply first, then change the state: an entry that is aborted is handed over again.
            int code = service.reply(entry.sessionId(), bytes(member + ":" + payload));
            if (code < 0) {
                fail("a reply from inside the handler answered " + code + " with error " + Abi.errcode());
            }
            if (code == Abi.CLUSTER_REPLY_RETRY) {
                positions.remove(positions.size() - 1);
                return Abi.ACTION_ABORT;
            }
            if (payload.equals(throwOn) && first) {
                codes.computeIfAbsent(payload, key -> new ArrayList<>()).add(code);
                throw new IllegalStateException("the handler's own");
            }
            if (refusals == null) {
                refusals = new ArrayList<>();
                refusals.add(asked("poll", () -> service.poll(this::onEntry, 1)));
                refusals.add(asked("position", service::position));
                refusals.add(asked("answers", () -> service.answers() ? 1 : 0));
                refusals.add(asked("sessions", service::sessions));
                refusals.add(thrown("fault", service::fault));
                refusals.add(thrown("close", service::close));
                refusals.add(asked("Client.isClosed", () -> client.isClosed() ? 1 : 0));
                refusals.add(asked("Client.closed", client::closed));
                refusals.add(thrown("Client.clusterService", client::clusterService));
                refusals.add(thrown("Client.addSubscription", () -> client.addSubscription("cavalry:ipc", 1229)));
                // The client's own close, last: the ABI refuses it and frees nothing
                // (spec/c-abi.md, "Calls during a claim or a poll"), so the client must
                // stay open and still reach the ABI after it: isClosed is refused again,
                // false with ERR_POLLING. Were the handle forgotten, isClosed would answer
                // true with no call, the thread's error would stay the other code, and the
                // poll after this one would hand the ABI a null client.
                refusals.add(thrown("Client.close", client::close));
                refusals.add(asked("Client.isClosed after its close", () -> client.isClosed() ? 1 : 0));
            }
            if (payload.equals(TIMER_MESSAGE)) {
                // The program's state changes at the entry it takes. One timer, scheduled
                // for an hour later and then again for a second later: were the second
                // schedule not to set the one timer's deadline, the round's wait would end
                // before the timer came. A second timer, due at once, cancelled again: were
                // the cancel to miss, it would be handed over. And the least and the
                // greatest long as ids, due at once, which each service must be handed back
                // as they are. Every member's service makes the same calls at the same
                // entry.
                timerCodes = List.of(
                    service.scheduleTimer(TIMER_ID, entry.timestamp() + HOUR_MS),
                    service.scheduleTimer(TIMER_ID, entry.timestamp() + TIMER_AFTER_MS),
                    service.scheduleTimer(CANCELLED_ID, entry.timestamp()),
                    service.cancelTimer(CANCELLED_ID),
                    service.scheduleTimer(Long.MIN_VALUE, entry.timestamp()),
                    service.scheduleTimer(Long.MAX_VALUE, entry.timestamp()));
            }
            codes.computeIfAbsent(payload, key -> new ArrayList<>()).add(code);
            messages.computeIfAbsent(payload, key -> new ArrayList<>()).add(read);
            entries.add(read);
            return breakOn.contains(payload) ? Abi.ACTION_BREAK : Abi.ACTION_CONTINUE;
        }

        /** One poll, timed: the count of entries taken. A handler's failure is the caller's
         * to catch. */
        int poll() {
            int before = entries.size();
            long started = System.nanoTime();
            int count;
            try {
                count = service.poll(this::onEntry, 16);
            } finally {
                long took = System.nanoTime() - started;
                polls++;
                if (took > 1_000_000) {
                    over++;
                }
                if (took > longest) {
                    longest = took;
                    longestOf = "a poll of the service beside member " + member + " that began after " + before
                        + " entries and took " + (entries.size() - before);
                }
            }
            if (count < 0) {
                fail("the poll of the service beside member " + member + " stopped on fault " + service.fault());
            }
            return count;
        }
    }

    /** What the session's handler was handed. */
    static final class Answered {
        final ClusterSession session;
        final List<Reply> replies = new ArrayList<>();
        List<Refusal> refusals;
        String throwOn; // a reply on which the handler throws
        ClusterReplyValues values; // the object of values the handler was handed, the same one for every reply

        Answered(ClusterSession session) {
            this.session = session;
        }

        void onReply(MemorySegment body, ClusterReplyValues reply) {
            if (values == null) {
                values = reply;
            }
            if (reply != values) {
                fail("the handler was handed another object of values than the one the session keeps");
            }
            String payload = text(body);
            replies.add(new Reply(payload, reply.leadershipTermId(), reply.timestamp()));
            if (refusals == null) {
                refusals = new ArrayList<>();
                refusals.add(asked("offer", () -> session.offer(bytes("x"))));
                refusals.add(asked("poll", () -> session.poll(this::onReply, 1)));
                refusals.add(asked("leadershipTermId", session::leadershipTermId));
                refusals.add(asked("leaderMemberId", session::leaderMemberId));
                refusals.add(asked("staleReplies", session::staleReplies));
                refusals.add(asked("closed", () -> session.closed() == null ? 0 : 1));
                refusals.add(thrown("fault", session::fault));
                refusals.add(thrown("close", session::close));
            }
            if (throwOn != null && payload.endsWith(throwOn)) {
                throw new IllegalStateException("the reply handler's own");
            }
        }

        /** The replies to the message, whichever member's service sent them. */
        List<Reply> to(String message) {
            return replies.stream().filter(reply -> reply.payload().substring(2).equals(message)).toList();
        }
    }

    /** One duty cycle: every service's poll but {@code skip}'s, the session's when there is
     * one, and a short sleep. */
    static void cycle(List<Served> served, Answered answered, Served skip) throws InterruptedException {
        for (Served s : served) {
            if (s != skip) {
                s.poll();
            }
        }
        if (answered != null && answered.session.poll(answered::onReply, 16) < 0) {
            fail("the session's poll stopped on fault " + answered.session.fault());
        }
        Thread.sleep(0, (int) IDLE_NANOS);
    }

    static void await(String what, BooleanSupplier done, List<Served> served, Answered answered, Served skip)
        throws InterruptedException {
        long until = System.nanoTime() + WAIT_NANOS;
        while (!done.getAsBoolean()) {
            if (System.nanoTime() > until) {
                fail(what + ": not within 60 s; the services took " + served.stream().map(s -> s.entries.size()).toList()
                    + " entries and the session read " + (answered == null ? "no" : answered.replies.size()) + " replies");
            }
            cycle(served, answered, skip);
        }
    }

    static void settle(List<Served> served, Answered answered, Served skip) throws InterruptedException {
        long until = System.nanoTime() + SETTLE_NANOS;
        while (System.nanoTime() < until) {
            cycle(served, answered, skip);
        }
    }

    static void offer(ClusterSession session, String message, List<Served> served, Answered answered)
        throws InterruptedException {
        long until = System.nanoTime() + WAIT_NANOS;
        while (true) {
            long offered = session.offer(bytes(message));
            if (offered >= 0) {
                return;
            }
            if (offered == Abi.PUBLICATION_ERROR) {
                fail("the offer of \"" + message + "\" failed with error " + Abi.errcode());
            }
            if (offered == Abi.PUBLICATION_CLOSED) {
                fail("the session closed before \"" + message + "\": " + session.closed());
            }
            if (System.nanoTime() > until) {
                fail("the session's publication did not take \"" + message + "\" within 60 s: " + offered);
            }
            cycle(served, answered, null);
        }
    }

    /** The HandlerException of the leader's service's next poll that throws one, with the
     * other services and the session polled meanwhile. */
    static HandlerException handlerException(Served leader, List<Served> served, Answered answered, String what)
        throws InterruptedException {
        long until = System.nanoTime() + WAIT_NANOS;
        while (true) {
            try {
                leader.poll();
            } catch (HandlerException thrown) {
                return thrown;
            }
            if (System.nanoTime() > until) {
                fail(what + ": no poll of the leader's service threw HandlerException within 60 s");
            }
            cycle(served, answered, leader);
        }
    }

    static boolean every(List<Served> served, Predicate<Served> has) {
        return served.stream().allMatch(has);
    }

    public static void main(String[] args) throws Exception {
        if (args.length != 3) {
            fail("usage: ClusterRound <cluster directory> <members string> <response channel>");
        }
        try {
            round(args[0], args[1], args[2]);
        } catch (CavalryException error) {
            fail(error.function() + " failed with error " + error.code() + ": " + error.abiMessage());
        } catch (HandlerException error) {
            fail("the handler threw: " + error.getCause());
        }
    }

    static void round(String cluster, String members, String responseChannel) throws Exception {
        // 1. A service beside each member, over a client on that member's driver.
        List<Served> served = new ArrayList<>();
        for (int member = 0; member < MEMBERS; member++) {
            served.add(new Served(cluster, member));
        }
        // The service the round sets the thread's error with, closed at once.
        spent = served.get(0).client.clusterService();
        spent.close();
        // The handler's reply is for the handler: from here there is no entry.
        if (served.get(0).service.reply(1, bytes("x")) != -1 || Abi.errcode() != Abi.ERR_CLUSTER_NO_ENTRY) {
            fail("a reply from outside the handler was not refused with ERR_CLUSTER_NO_ENTRY: error " + Abi.errcode());
        }

        // 2. Each service opens its member's log and reads the term's first entry before the
        // session connects: a service answers no entry that its member held when the service
        // first opened the log (spec/cluster.md, "Replies").
        await("each service reads an entry of its member's log", () -> every(served, s -> !s.entries.isEmpty()), served,
            null, null);

        // 3. A session with the cluster, on the driver at CAVALRY_DIR. The connect returns once
        // the leader has opened the session.
        Client client = new Client();
        ClusterSession session = client.connectCluster(members, responseChannel);
        Answered answered = new Answered(session);
        if (session.closed() != null || session.sessionId() < 0 || session.maxMessageLength() <= 0) {
            fail("a session just opened reads closed " + session.closed() + ", id " + session.sessionId()
                + " and a longest message of " + session.maxMessageLength() + " bytes");
        }
        // What offer refuses: a message longer than one frame carries.
        if (session.offer(MemorySegment.ofArray(new byte[(int) session.maxMessageLength() + 1])) != Abi.PUBLICATION_ERROR
            || Abi.errcode() != Abi.ERR_TOO_LONG) {
            fail("a message of " + (session.maxMessageLength() + 1) + " bytes was not refused with PUBLICATION_ERROR and"
                + " ERR_TOO_LONG: error " + Abi.errcode());
        }

        // 4. One message, taken by every service and answered by one.
        offer(session, "hello", served, answered);
        await("the message is taken by three services and answered",
            () -> !answered.replies.isEmpty() && every(served, s -> !s.taken("hello").isEmpty()), served, answered, null);
        settle(served, answered, null);
        if (answered.replies.size() != 1) {
            fail("the session read " + answered.replies.size() + " replies to one message: " + answered.replies);
        }

        // 5. The same entry beside every member, answered by the leader's service alone.
        Served leader = null;
        Entry first = served.get(0).taken("hello").get(0);
        int polling = Abi.ERR_POLLING;
        for (Served s : served) {
            if (!s.taken("hello").equals(List.of(first)) || first.session() != session.sessionId()) {
                fail("the service beside member " + s.member + " took the message as " + s.taken("hello")
                    + ", and the service beside member 0 as " + first + ", of session " + session.sessionId());
            }
            if (s.service.sessions() != 1) {
                fail("the service beside member " + s.member + " holds " + s.service.sessions() + " sessions open, not one");
            }
            if (s.service.position() <= first.position()) {
                fail("the service beside member " + s.member + " is at " + s.service.position() + ", not past the entry at "
                    + first.position());
            }
            if (s.service.fault().code() != 0) {
                fail("the service beside member " + s.member + " reads fault " + s.service.fault()
                    + " after polls that took entries");
            }
            List<Refusal> expected = List.of(new Refusal("poll", -1, polling), new Refusal("position", -1, polling),
                new Refusal("answers", 0, polling), new Refusal("sessions", -1, polling), new Refusal("fault", -1, polling),
                new Refusal("close", -1, polling), new Refusal("Client.isClosed", 0, polling),
                new Refusal("Client.closed", -1, polling),
                new Refusal("Client.clusterService", -1, polling), new Refusal("Client.addSubscription", -1, polling),
                new Refusal("Client.close", -1, polling), new Refusal("Client.isClosed after its close", 0, polling));
            if (!expected.equals(s.refusals)) {
                fail("the calls from inside the handler of the service beside member " + s.member + " answered "
                    + s.refusals + ", not " + expected);
            }
            List<Integer> codes = s.codes.get("hello");
            if (List.of(Abi.CLUSTER_REPLY_SENT).equals(codes)) {
                if (leader != null) {
                    fail("the services beside members " + leader.member + " and " + s.member + " both answered one entry");
                }
                if (!s.service.answers()) {
                    fail("the service beside member " + s.member + " answered and says it does not answer");
                }
                leader = s;
            } else if (!List.of(Abi.CLUSTER_REPLY_NOT_ANSWERING).equals(codes) || s.service.answers()) {
                fail("the service beside member " + s.member + ", a follower, answered " + codes + " and says it answers: "
                    + s.service.answers());
            }
        }
        if (leader == null) {
            fail("no service answered the entry");
        }
        final Served led = leader;
        if (!answered.replies.equals(List.of(new Reply(led.member + ":hello", first.term(), first.timestamp())))) {
            fail("the reply " + answered.replies + " is not the leader's service's, of member " + led.member
                + ", with the entry's term " + first.term() + " and the cluster's time " + first.timestamp());
        }
        List<Refusal> refused = List.of(new Refusal("offer", Abi.PUBLICATION_ERROR, polling), new Refusal("poll", -1, polling),
            new Refusal("leadershipTermId", -1, polling), new Refusal("leaderMemberId", -1, polling),
            new Refusal("staleReplies", -1, polling), new Refusal("closed", 0, polling), new Refusal("fault", -1, polling),
            new Refusal("close", -1, polling));
        if (!refused.equals(answered.refusals)) {
            fail("the calls from inside the session's handler answered " + answered.refusals + ", not " + refused);
        }
        if (session.leaderMemberId() != led.member || session.leadershipTermId() != first.term()
            || session.closed() != null || session.fault().code() != 0) {
            fail("the session sends to member " + session.leaderMemberId() + " in term " + session.leadershipTermId()
                + ", reads closed " + session.closed() + " and fault " + session.fault() + "; the entry's leader is member "
                + led.member + " and its term " + first.term());
        }
        List<Entry> terms = led.entries.stream().filter(e -> e.type() == Abi.CLUSTER_ENTRY_NEW_LEADERSHIP_TERM).toList();
        List<Entry> opens = led.entries.stream().filter(e -> e.type() == Abi.CLUSTER_ENTRY_SESSION_OPEN).toList();
        if (terms.isEmpty() || terms.get(terms.size() - 1).leader() != led.member || opens.size() != 1
            || opens.get(0).session() != session.sessionId()) {
            fail("the leader's service read the new leadership term events " + terms + " and the session open events "
                + opens + "; the leader is member " + led.member + " and the session " + session.sessionId());
        }
        say("session " + session.sessionId() + "'s message, the entry at position " + first.position() + " of term "
            + first.term() + ", taken by three services and answered once, by the service beside member " + led.member
            + "; twelve calls from inside a service's handler, the client's close among them, and eight from inside the "
            + "session's refused with ERR_POLLING");

        // 6. A handler that replies and then throws: its entry is aborted, so the poll ends at
        // it with the entry not taken, poll throws, and the next poll hands the entry over
        // again; the reply the first handling sent is not sent again.
        led.throwOn = "throw";
        long before = led.service.position();
        offer(session, "throw", served, answered);
        HandlerException thrown = handlerException(led, served, answered, "a handler that throws");
        if (!"service".equals(thrown.cluster()) || thrown.raised() != 1 || !(thrown.getCause() instanceof IllegalStateException)
            || thrown.fragments() < 0 || thrown.committedLog() || !thrown.getMessage().contains("Its entry was aborted")) {
            fail("a service's HandlerException has cluster " + thrown.cluster() + ", fragments " + thrown.fragments()
                + ", raised " + thrown.raised() + " and cause " + thrown.getCause() + ", and says: " + thrown.getMessage());
        }
        long aborted = led.handed.get("throw").get(0);
        long at = led.service.position();
        if (at < before || at > aborted || !led.taken("throw").isEmpty()) {
            fail("after its handler threw on the entry at " + aborted + " the service is at " + at + ", from " + before
                + ", and took " + led.taken("throw"));
        }
        await("the aborted entry is handed over again and its one reply read",
            () -> !answered.to("throw").isEmpty() && every(served, s -> !s.taken("throw").isEmpty()), served, answered, null);
        settle(served, answered, null);
        if (!led.handed.get("throw").equals(List.of(aborted, aborted))
            || !led.codes.get("throw").equals(List.of(Abi.CLUSTER_REPLY_SENT, Abi.CLUSTER_REPLY_SENT))
            || answered.to("throw").size() != 1) {
            fail("the entry whose handler threw was handed over at " + led.handed.get("throw") + ", its replies answered "
                + led.codes.get("throw") + ", and the session read " + answered.to("throw"));
        }

        // 7. A handler that returns what is no action, as a throw: aborted, thrown by poll with
        // the ABI's own error, and handed over again. On the second handling, before the
        // reply: a reply too long for a frame, and a reply to a session that is not open.
        led.noActionOn = "no action";
        led.edgesOn = "no action";
        offer(session, "no action", served, answered);
        thrown = handlerException(led, served, answered, "a handler that returns what is no action");
        if (!"service".equals(thrown.cluster()) || thrown.raised() != 1
            || !(thrown.getCause() instanceof CavalryException cause) || cause.code() != Abi.ERR_CLUSTER_ACTION
            || !cause.function().equals("cavalry_cluster_service_action") || !cause.abiMessage().contains("3 is no action")) {
            fail("a handler that returned 3 made poll throw " + thrown + " from " + thrown.getCause());
        }
        await("the entry with no action is handed over again and answered",
            () -> !answered.to("no action").isEmpty() && every(served, s -> !s.taken("no action").isEmpty()), served,
            answered, null);
        settle(served, answered, null);
        List<Long> handed = led.handed.get("no action");
        if (handed.size() != 2 || !handed.get(0).equals(handed.get(1)) || answered.to("no action").size() != 1) {
            fail("the entry whose handler returned no action was handed over at " + handed + " and the session read "
                + answered.to("no action"));
        }
        List<Integer> edges = List.of(Abi.CLUSTER_REPLY_TOO_LONG, Abi.ERR_TOO_LONG, Abi.CLUSTER_REPLY_NO_SESSION);
        if (!edges.equals(led.edges) || !led.codes.get("no action").equals(List.of(Abi.CLUSTER_REPLY_SENT))) {
            fail("a reply of 1,345 bytes, the thread's error after it, and a reply to session "
                + (session.sessionId() + 1000) + " answered " + led.edges + ", not " + edges + ", and the reply after them "
                + led.codes.get("no action"));
        }

        // 8. ACTION_BREAK: with two entries waiting, a poll takes one and ends. The leader's
        // service is not polled until the two others have taken both entries and a moment
        // more, so that its member has committed both.
        led.breakOn.add("break 1");
        led.breakOn.add("break 2");
        offer(session, "break 1", served, answered);
        offer(session, "break 2", served, answered);
        await("the followers' services take two more entries",
            () -> every(served, s -> s == led || !s.taken("break 2").isEmpty()), served, answered, led);
        settle(served, answered, led);
        List<Integer> counts = new ArrayList<>();
        long until = System.nanoTime() + WAIT_NANOS;
        while (led.taken("break 2").isEmpty()) {
            if (System.nanoTime() > until) {
                fail("the leader's service took " + counts + " entries in its polls and not both entries");
            }
            int count = led.poll();
            if (count != 0) {
                counts.add(count);
            } else {
                cycle(served, answered, led);
            }
        }
        if (!counts.equals(List.of(1, 1))) {
            fail("with ACTION_BREAK returned for each of two entries the leader's polls took " + counts + ", not one each");
        }
        await("the two replies are read", () -> !answered.to("break 1").isEmpty() && !answered.to("break 2").isEmpty(),
            served, answered, null);
        List<Reply> last = answered.replies.subList(answered.replies.size() - 2, answered.replies.size());
        if (!last.get(0).payload().substring(2).equals("break 1") || !last.get(1).payload().substring(2).equals("break 2")) {
            fail("the replies to two messages came out of order: " + last);
        }

        // 9. The session's handler throws: the reply was handed over, poll throws once the ABI
        // has returned, and the reply is not handed over again.
        answered.throwOn = "reply throws";
        offer(session, "reply throws", served, answered);
        thrown = null;
        until = System.nanoTime() + WAIT_NANOS;
        while (thrown == null) {
            if (System.nanoTime() > until) {
                fail("the reply on which the session's handler throws was not read");
            }
            try {
                cycle(served, answered, null);
            } catch (HandlerException error) {
                thrown = error;
            }
        }
        answered.throwOn = null;
        if (!"session".equals(thrown.cluster()) || thrown.fragments() != 1 || thrown.raised() != 1
            || !(thrown.getCause() instanceof IllegalStateException)
            || !thrown.getMessage().contains("the reply handler threw on 1 of the 1 reply this poll handed over")) {
            fail("a session's HandlerException has cluster " + thrown.cluster() + ", fragments " + thrown.fragments()
                + ", raised " + thrown.raised() + " and cause " + thrown.getCause() + ", and says: " + thrown.getMessage());
        }
        settle(served, answered, null);
        if (answered.to("reply throws").size() != 1 || session.staleReplies() != 0) {
            fail("the reply whose handler threw was read " + answered.to("reply throws").size()
                + " times, and the session passed over " + session.staleReplies() + " stale replies");
        }
        say("a handler that threw and one that returned no action had their entries aborted, handed over again and"
            + " answered once; a reply of 1,345 bytes and a reply to no session refused; ACTION_BREAK took one of two"
            + " entries; a reply whose handler threw is not handed over again");

        // 9b. The program's timers (spec/c-abi.md, version 7; spec/cluster.md, "The service",
        // "Timers"; ADR 0157). Each service's program schedules four timers at one message
        // and cancels one: the three are handed to every service once each, as entries of
        // their own type with the timers' ids, at one position each and none earlier than
        // its deadline, and the cancelled one to none. Step 10 holds the "once" for a timer
        // refresh interval and more. From outside the handler both calls are refused, the
        // thread's error having been set to another code first.
        for (Refusal refusal : List.of(
                asked("scheduleTimer", () -> served.get(0).service.scheduleTimer(1, 1)),
                asked("cancelTimer", () -> served.get(0).service.cancelTimer(1)))) {
            if (refusal.answer() != -1 || refusal.code() != Abi.ERR_CLUSTER_NO_ENTRY) {
                fail(refusal.call() + " from outside the handler answered " + refusal.answer() + " with error "
                    + refusal.code() + ", not -1 with ERR_CLUSTER_NO_ENTRY");
            }
        }
        offer(session, TIMER_MESSAGE, served, answered);
        List<Long> timerIds = List.of(Long.MIN_VALUE, TIMER_ID, Long.MAX_VALUE);
        await("each service is handed its three timers", () -> every(served, s -> s.timers.size() >= timerIds.size()),
            served, answered, null);
        settle(served, answered, null);
        Entry scheduled = served.get(0).taken(TIMER_MESSAGE).get(0);
        int scheduledCode = Abi.CLUSTER_TIMER_SCHEDULED;
        for (Served s : served) {
            if (!s.timerCodes.equals(List.of(scheduledCode, scheduledCode, scheduledCode, 0, scheduledCode, scheduledCode))) {
                fail("the service beside member " + s.member + " was answered " + s.timerCodes + " for its schedules and"
                    + " its cancel; expected CLUSTER_TIMER_SCHEDULED, " + scheduledCode + ", for each schedule and 0 for"
                    + " the cancel");
            }
            if (!s.timers.stream().map(Timer::id).sorted().toList().equals(timerIds)) {
                fail("the service beside member " + s.member + " was handed the timers " + s.timers + "; expected timer "
                    + TIMER_ID + ", the least long and the greatest once each, and timer " + CANCELLED_ID
                    + ", which it cancelled, never");
            }
            if (!s.timers.equals(served.get(0).timers)) {
                fail("the service beside member " + s.member + " was handed its timers as " + s.timers
                    + " and the service beside member " + served.get(0).member + " as " + served.get(0).timers
                    + "; expected one position and one time for each");
            }
            for (Timer t : s.timers) {
                long after = t.id() == TIMER_ID ? TIMER_AFTER_MS : 0;
                if (t.timestamp() < scheduled.timestamp() + after || t.position() <= scheduled.position()) {
                    fail("the service beside member " + s.member + " was handed timer " + t.id() + " at position "
                        + t.position() + " and time " + t.timestamp() + "; it was scheduled at position "
                        + scheduled.position() + " and time " + scheduled.timestamp() + " for " + after + " ms later");
                }
            }
        }
        // What a deadline that was ignored would have read: the time of the timers that
        // were due at once, which the member was told of with timer 77. The deadline check
        // above proves something for as long as this is below the deadline.
        Timer timer = served.get(0).timers.stream().filter(t -> t.id() == TIMER_ID).findFirst().orElseThrow();
        long atOnce = served.get(0).timers.stream().filter(t -> t.id() != TIMER_ID).mapToLong(Timer::timestamp).max()
            .orElseThrow() - scheduled.timestamp();
        say("timer " + TIMER_ID + ", scheduled by each service at a message for an hour later and then for "
            + TIMER_AFTER_MS + " ms later, handed to three services once, at position " + timer.position() + ", "
            + (timer.timestamp() - scheduled.timestamp()) + " ms after the message; the least and the greatest long as"
            + " ids, due at once, handed to each once with those ids, " + atOnce + " ms after the message"
            + (atOnce < TIMER_AFTER_MS ? "" : ", so the deadline's check proved nothing in this run") + "; timer "
            + CANCELLED_ID + ", due at once and cancelled in the same handling, handed to none; both calls refused from"
            + " outside the handler");

        // 10. The session before its client. Its close is an entry every service is handed,
        // after which the log holds no session open.
        session.close();
        session.close(); // idempotent
        if (session.closed() != null) {
            fail("a session that close() freed reads closed " + session.closed());
        }
        client.close();
        await("each service reads the session's close",
            () -> every(served, s -> s.entries.stream().anyMatch(e -> e.type() == Abi.CLUSTER_ENTRY_SESSION_CLOSE)), served,
            null, null);
        for (Served s : served) {
            List<Entry> closes = s.entries.stream().filter(e -> e.type() == Abi.CLUSTER_ENTRY_SESSION_CLOSE).toList();
            if (closes.size() != 1 || closes.get(0).session() != first.session() || closes.get(0).timedOut()
                || s.service.sessions() != 0) {
                fail("the service beside member " + s.member + " read the session close events " + closes + " and holds "
                    + s.service.sessions() + " sessions open; the session " + first.session() + " was closed by its client");
            }
            if (!s.entries.equals(served.get(0).entries)) {
                fail("the services beside members 0 and " + s.member + " took different entries: " + served.get(0).entries
                    + " and " + s.entries);
            }
        }
        // The timers of step 9b, once each: a leader gives a timer that is still pending in
        // its service another event a timer refresh interval after the first, so the
        // services are polled until that long has passed since the last of them was handed
        // a timer.
        long refreshed = served.stream().mapToLong(s -> s.timerAt).max().orElseThrow() + REFRESH_NANOS;
        while (System.nanoTime() - refreshed < 0) {
            cycle(served, null, null);
        }
        for (Served s : served) {
            if (s.timers.size() != 3) {
                fail("the service beside member " + s.member + " was handed the timers " + s.timers + " by "
                    + REFRESH_NANOS / 1_000_000 + " ms after the last; expected three, once each");
            }
        }

        // 11. Each service before its own client, twice; and the last after its client.
        for (Served s : served.subList(0, MEMBERS - 1)) {
            s.service.close();
            s.service.close(); // idempotent
            s.client.close();
        }
        Served lastServed = served.get(MEMBERS - 1);
        lastServed.client.close();
        lastServed.service.close();
        int polls = 0;
        int over = 0;
        Served longest = served.get(0);
        for (Served s : served) {
            polls += s.polls;
            over += s.over;
            if (s.longest > longest.longest) {
                longest = s;
            }
        }
        say(served.get(0).entries.size() + " entries taken by each of three services; the session's close read by each;"
            + " a service closed after its client");
        say("report: " + polls + " polls of the three services, each holding its thread for its run: " + over
            + " took over 1 ms, and the longest " + longest.longest / 1000 + " us, " + longest.longestOf + " (not gated)");
        System.out.println("conformance/jvm: cluster round ok");
    }
}
