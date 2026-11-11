// The JVM client's committed log round (spec/c-abi.md, version 2; ADR 0070):
// Client.committedLog and CommittedLog beside a cluster member that conformance/jvm/run.sh
// started with `cavalry run`, one of three on the loopback, as
// conformance/python/committed_log.py runs it through clients/python.
//
//     CommittedLogRound <the member's driver directory> <the member's id>
//
// A cluster started with `cavalry run` holds one entry, the first of its leader's term, 56
// bytes in a frame of 88 at position 0, so the log's next position is 96. The round opens
// twenty logs and closes each with no poll between, which leaves its client open (ADR
// 0078); reads that entry through the handler, with its position, its header and the
// handle's reads; makes each of the eight calls the ABI refuses from inside the log's own
// handler, each after it has set the thread's error to another code, so that a refusal
// that answers what a live log answers is told from an answer; checks where a handler's
// exception goes and that its entry is not delivered again; opens the log again at 96,
// where nothing is committed yet,
// beside a log at 0 that it reads the entry through meanwhile, so that the log at 96 has
// had the time a replay's image takes to join; closes twice; and closes a log after its
// client. Every wait is bounded at 30 s and sleeps a millisecond after a poll that
// delivered nothing. It exits 1 with what it found when a check fails. The JDK alone
// (ADR 0003).

import com.appshapes.cavalry.Abi;
import com.appshapes.cavalry.CavalryException;
import com.appshapes.cavalry.Client;
import com.appshapes.cavalry.CommittedLog;
import com.appshapes.cavalry.EntryHandler;
import com.appshapes.cavalry.HandlerException;
import com.appshapes.cavalry.Subscription;

import java.util.ArrayList;
import java.util.List;

public final class CommittedLogRound {
    static final long WAIT_NANOS = 30_000_000_000L;
    static final long IDLE_MILLIS = 1; // after a poll that delivered nothing: three drivers share the host
    static final int ENTRY_FRAME = 88; // the term's first entry: 32 bytes of header and 56 of message
    static final long NEXT = 96; // the frame, rounded up to a multiple of 32

    private CommittedLogRound() {
    }

    static void fail(String what) {
        System.err.println("conformance/jvm: committed log: " + what);
        System.exit(1);
    }

    static void say(String what) {
        System.out.println("conformance/jvm: committed log: " + what);
    }

    /** The member's committed log at the position, asked for until the member has one. */
    static CommittedLog openLog(Client client, long position) throws InterruptedException {
        long until = System.nanoTime() + WAIT_NANOS;
        while (true) {
            CommittedLog log = client.committedLog(position);
            if (log != null) {
                return log;
            }
            if (System.nanoTime() > until) {
                fail("the driver at " + client.dir() + " hosted no member with a log within 30 s");
            }
            Thread.sleep(50);
        }
    }

    public static void main(String[] args) throws Exception {
        if (args.length != 2) {
            fail("usage: CommittedLogRound <the member's driver directory> <the member's id>");
        }
        try {
            round(args[0], Integer.parseInt(args[1]));
        } catch (CavalryException error) {
            fail(error.function() + " failed with error " + error.code() + ": " + error.abiMessage());
        } catch (HandlerException error) {
            fail("the handler threw: " + error.getCause());
        }
    }

    static void round(String memberDir, int memberId) throws Exception {
        Client client = new Client(memberDir);
        // A closed log, for the refusals below: its position() hands the ABI a null handle,
        // which answers -1 with ERR_NULL. The package's bindings of the ABI are not public,
        // so the round cannot ask for the position of a null header: this is its call that
        // sets the thread's error to another code. It is closed with no poll between, and
        // so are nineteen more: a log's replay's image may not have joined when it closes,
        // and the client's conductor then reads of an image whose log is gone. The client
        // stays open (ADR 0078; before it, card 241, this closed the client as corrupt in 8
        // of 60 tries), which the rest of the round shows.
        CommittedLog spent = openLog(client, 0);
        spent.close();
        for (int i = 0; i < 19; i++) {
            openLog(client, 0).close();
        }
        if (client.isClosed()) {
            fail("a log opened and closed with no poll between closed its client: " + client.closed());
        }
        say("twenty logs opened and closed with no poll between left the client open");
        CommittedLog log = openLog(client, 0);
        if (log.memberId() != memberId || log.recordingId() != 0) {
            fail("the log is member " + log.memberId() + "'s recording " + log.recordingId() + ", not member " + memberId
                + "'s recording 0");
        }
        if (log.position() != 0 || log.commitPosition() != 0) {
            fail("a log opened at 0 began at " + log.position() + " with bound " + log.commitPosition());
        }

        // The entry, with what each call on the log answers from inside its own handler.
        List<String> entries = new ArrayList<>();
        List<String> refusals = new ArrayList<>();
        boolean[] inside = new boolean[1];
        // Sets the thread's error to one no refusal gives, ERR_NULL, through the closed log's
        // position(). A success clears nothing (spec/c-abi.md, "Error codes"), and a refused
        // isEnded(), failedAt() or withdrawnAt() answers what a live log answers, false or
        // empty, so the code alone tells that refusal from an answer: it must have changed
        // from this one to ERR_POLLING. Without this the code would be ERR_POLLING already,
        // left by the call before.
        Runnable setAnotherError = () -> {
            if (spent.position() != -1 || Abi.errcode() != Abi.ERR_NULL) {
                fail("a closed log's position() did not answer -1 with ERR_NULL: error " + Abi.errcode());
            }
        };
        EntryHandler[] self = new EntryHandler[1];
        EntryHandler onEntry = (body, header, position) -> {
            entries.add(position + " " + body.byteSize() + " " + header.frameLength() + " " + header.flags() + " "
                + header.type());
            if (inside[0]) {
                return;
            }
            inside[0] = true;
            setAnotherError.run();
            refusals.add("poll " + log.poll(self[0], 1) + " " + Abi.errcode());
            setAnotherError.run();
            refusals.add("position " + log.position() + " " + Abi.errcode());
            setAnotherError.run();
            refusals.add("commitPosition " + log.commitPosition() + " " + Abi.errcode());
            setAnotherError.run();
            refusals.add("isEnded " + log.isEnded() + " " + Abi.errcode());
            setAnotherError.run();
            refusals.add("failedAt " + log.failedAt().isPresent() + " " + Abi.errcode());
            setAnotherError.run();
            refusals.add("withdrawnAt " + log.withdrawnAt().isPresent() + " " + Abi.errcode());
            setAnotherError.run();
            try {
                log.fault();
                refusals.add("fault 0 " + Abi.errcode());
            } catch (CavalryException error) {
                refusals.add("fault -1 " + error.code());
            }
            setAnotherError.run();
            try {
                log.close();
                refusals.add("close 0 " + Abi.errcode());
            } catch (CavalryException error) {
                refusals.add("close -1 " + error.code());
            }
        };
        self[0] = onEntry;
        long until = System.nanoTime() + WAIT_NANOS;
        while (entries.isEmpty()) {
            int count = log.poll(onEntry, 16);
            if (count < 0) {
                fail("the term's first entry: the poll stopped on fault " + log.fault());
            }
            if (System.nanoTime() > until) {
                fail("the term's first entry: not within 30 s; position " + log.position() + ", commit position "
                    + log.commitPosition() + ", ended " + log.isEnded());
            }
            if (count == 0) {
                Thread.sleep(IDLE_MILLIS);
            }
        }
        String expectedEntry = "0 " + (ENTRY_FRAME - 32) + " " + ENTRY_FRAME + " " + Abi.FLAGS_UNFRAGMENTED + " "
            + Abi.FRAME_TYPE_DATA;
        if (entries.size() != 1 || !entries.get(0).equals(expectedEntry)) {
            fail("expected one entry \"" + expectedEntry + "\" (position, bytes, frame length, flags, type), got " + entries
                + ". One cause of more than one entry is a new term: a cluster that elected again between its formation "
                + "and this read holds that term's first entry after this one; the members' output, which run.sh "
                + "prints, says whether it did");
        }
        int polling = Abi.ERR_POLLING;
        List<String> expected = List.of("poll -1 " + polling, "position -1 " + polling, "commitPosition -1 " + polling,
            "isEnded false " + polling, "failedAt false " + polling, "withdrawnAt false " + polling,
            "fault -1 " + polling, "close -1 " + polling);
        if (!refusals.equals(expected)) {
            fail("the calls from inside the log's own handler answered " + refusals + ", not " + expected);
        }
        if (log.position() != NEXT || log.commitPosition() != NEXT) {
            fail("after the entry the log is at " + log.position() + " with bound " + log.commitPosition() + ", not " + NEXT
                + " and " + NEXT);
        }
        Subscription.Fault fault = log.fault();
        if (log.isEnded() || log.failedAt().isPresent() || log.withdrawnAt().isPresent() || fault.code() != 0
            || fault.sessionId() != 0 || fault.position() != Abi.NULL_POSITION) {
            fail("a live log read ended " + log.isEnded() + ", failed at " + log.failedAt() + ", withdrawn at "
                + log.withdrawnAt() + ", fault " + fault);
        }
        for (int i = 0; i < 50; i++) {
            if (log.poll(onEntry, 16) != 0) {
                fail("an entry arrived with nothing more committed");
            }
        }
        if (entries.size() != 1) {
            fail("the refused poll, or a later one, delivered more: " + entries);
        }
        say("member " + log.memberId() + "'s entry at position 0 read once, next position " + log.position()
            + ", commit position " + log.commitPosition() + "; eight calls from inside the handler refused with "
            + "ERR_POLLING, each made with the thread's error at ERR_NULL");
        log.close();
        log.close(); // idempotent

        // A handler that throws: the entry was delivered, the exception is thrown after the
        // ABI has returned, and the log's position is past the entry, so it is not delivered
        // again.
        CommittedLog throwing = openLog(client, 0);
        List<Long> seen = new ArrayList<>();
        EntryHandler throwsAlways = (body, header, position) -> {
            seen.add(position);
            throw new IllegalStateException("the handler's own");
        };
        HandlerException thrown = null;
        until = System.nanoTime() + WAIT_NANOS;
        while (thrown == null) {
            try {
                if (throwing.poll(throwsAlways, 16) < 0) {
                    fail("the second log's poll stopped on fault " + throwing.fault());
                }
            } catch (HandlerException error) {
                thrown = error;
            }
            if (thrown == null && System.nanoTime() > until) {
                fail("the second log read no entry");
            }
            if (thrown == null) {
                Thread.sleep(IDLE_MILLIS);
            }
        }
        if (!seen.equals(List.of(0L)) || !(thrown.getCause() instanceof IllegalStateException) || thrown.raised() != 1
            || throwing.position() != NEXT) {
            fail("a throwing handler saw " + seen + ", threw " + thrown + ", and left the log at " + throwing.position());
        }
        // The exception is the log's own: it counts entries and names no subscription.
        if (thrown.fragments() != 1 || !thrown.committedLog()
            || !thrown.getMessage().contains("the entry handler threw on 1 of the 1 entry")
            || thrown.getMessage().contains("Subscription")) {
            fail("a committed log's HandlerException has fragments " + thrown.fragments() + ", committedLog "
                + thrown.committedLog() + " and says: " + thrown.getMessage());
        }
        for (int i = 0; i < 50; i++) {
            if (throwing.poll(throwsAlways, 16) != 0) {
                fail("the entry whose handler threw was delivered again");
            }
        }
        throwing.close();

        // From the next position nothing is committed yet: no entry, and no entry before it.
        // A replay's image joins a moment after the open, so polls made at once would pass
        // before the image is there, whatever the open did with the position. So a second
        // log is opened at 0 beside this one, on the same client, and both are polled until
        // that one has delivered the entry at 0 and this one has taken the member's commit
        // position as its bound, which a poll does once the log's image has joined
        // (`CommittedLog::poll`, crates/cavalry-client/src/committed_log.rs): by then this
        // log's replay, asked for first, has had the time the other's took to join and
        // deliver.
        CommittedLog rest = openLog(client, NEXT);
        if (rest.position() != NEXT) {
            fail("a log opened at " + NEXT + " began at " + rest.position());
        }
        CommittedLog beside = openLog(client, 0);
        List<Long> atNext = new ArrayList<>();
        List<Long> atStart = new ArrayList<>();
        EntryHandler onNext = (body, header, position) -> atNext.add(position);
        EntryHandler onStart = (body, header, position) -> atStart.add(position);
        until = System.nanoTime() + WAIT_NANOS;
        while (atStart.isEmpty() || rest.commitPosition() < NEXT) {
            int restCount = rest.poll(onNext, 16);
            int besideCount = beside.poll(onStart, 16);
            if (restCount < 0 || besideCount < 0) {
                fail("the polls of the logs at " + NEXT + " and at 0 returned " + restCount + " and " + besideCount
                    + ": faults " + rest.fault() + " and " + beside.fault());
            }
            if (System.nanoTime() > until) {
                fail("the log at 0 beside the log at " + NEXT + " delivered " + atStart + " and the log at " + NEXT
                    + " took the bound " + rest.commitPosition() + ", not the entry at 0 and " + NEXT + ", within 30 s");
            }
            if (restCount == 0 && besideCount == 0) {
                Thread.sleep(IDLE_MILLIS);
            }
        }
        for (int i = 0; i < 50; i++) {
            if (rest.poll(onNext, 16) < 0) {
                fail("the poll of the log at " + NEXT + " stopped on fault " + rest.fault());
            }
        }
        if (atStart.get(0) != 0 || !atNext.isEmpty() || rest.position() != NEXT) {
            fail("a log opened at " + NEXT + " delivered the entries at " + atNext + " and is at " + rest.position()
                + ", while a log opened at 0 beside it delivered the entries at " + atStart + ". An entry below " + NEXT
                + " is one the open did not start past; one at " + NEXT + " or above is a new term's, of a cluster that "
                + "elected again, which the members' output, printed by run.sh, shows");
        }
        beside.close();
        // A log that outlives its client: its poll stops on the closed fault, and its close
        // frees it with no client to stop the replay through.
        client.close();
        if (rest.poll(onEntry, 16) != -1 || rest.fault().code() != Abi.ERR_FAULT_CLOSED) {
            fail("a poll after the client's close did not stop on the closed fault: " + rest.fault());
        }
        rest.close();
        say("a throwing handler's entry is not delivered again; nothing at position " + NEXT
            + " while a log at 0 beside it read the entry at 0; a log closed after its client");
    }
}
