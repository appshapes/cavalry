// The JVM client's committed log round (spec/c-abi.md, version 2; ADR 0068):
// Client.committedLog and CommittedLog beside a cluster member that conformance/jvm/run.sh
// started with `cavalry run`, one of three on the loopback, as
// conformance/python/committed_log.py runs it through clients/python.
//
//     CommittedLogRound members
//     CommittedLogRound <the member's driver directory> <the member's id>
//
// `members` prints a members string for three members with five free loopback UDP ports
// each, for run.sh to start the cluster with. The round: a cluster started with
// `cavalry run` holds one entry, the first of its leader's term, 56 bytes in a frame of 88
// at position 0, so the log's next position is 96. It reads that entry through the
// handler, with its position, its header and the handle's reads; makes each call the ABI
// refuses from inside the log's own handler; checks where a handler's exception goes and
// that its entry is not delivered again; opens the log again at 96, where nothing is
// committed yet; closes twice; and closes a log after its client. Every wait is bounded at
// 30 s. It exits 1 with what it found when a check fails. The JDK alone (ADR 0003).

import com.appshapes.cavalry.Abi;
import com.appshapes.cavalry.CavalryException;
import com.appshapes.cavalry.Client;
import com.appshapes.cavalry.CommittedLog;
import com.appshapes.cavalry.EntryHandler;
import com.appshapes.cavalry.HandlerException;
import com.appshapes.cavalry.Subscription;

import java.net.DatagramSocket;
import java.net.InetAddress;
import java.util.ArrayList;
import java.util.List;

public final class CommittedLogRound {
    static final long WAIT_NANOS = 30_000_000_000L;
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

    /** A members string for three members, each with five free loopback UDP ports, the
     * fifteen sockets held until every port is chosen so that none repeats. */
    static String members() throws Exception {
        List<DatagramSocket> sockets = new ArrayList<>();
        List<Integer> ports = new ArrayList<>();
        for (int i = 0; i < 15; i++) {
            DatagramSocket socket = new DatagramSocket(0, InetAddress.getLoopbackAddress());
            sockets.add(socket);
            ports.add(socket.getLocalPort());
        }
        for (DatagramSocket socket : sockets) {
            socket.close();
        }
        List<String> members = new ArrayList<>();
        for (int member = 0; member < 3; member++) {
            List<String> endpoints = new ArrayList<>();
            for (int i = 0; i < 5; i++) {
                endpoints.add("127.0.0.1:" + ports.get(member * 5 + i));
            }
            members.add(member + "," + String.join(",", endpoints));
        }
        return String.join("|", members);
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
        if (args.length == 1 && args[0].equals("members")) {
            System.out.println(members());
            return;
        }
        if (args.length != 2) {
            fail("usage: CommittedLogRound members | CommittedLogRound <the member's driver directory> <the member's id>");
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
        EntryHandler[] self = new EntryHandler[1];
        EntryHandler onEntry = (body, header, position) -> {
            entries.add(position + " " + body.byteSize() + " " + header.frameLength() + " " + header.flags() + " "
                + header.type());
            if (!refusals.isEmpty()) {
                return;
            }
            refusals.add("poll " + log.poll(self[0], 1) + " " + Abi.errcode());
            refusals.add("position " + log.position() + " " + Abi.errcode());
            refusals.add("commitPosition " + log.commitPosition() + " " + Abi.errcode());
            refusals.add("isEnded " + log.isEnded() + " " + Abi.errcode());
            refusals.add("failedAt " + log.failedAt().isPresent() + " " + Abi.errcode());
            try {
                log.close();
                refusals.add("close 0 0");
            } catch (CavalryException error) {
                refusals.add("close -1 " + error.code());
            }
        };
        self[0] = onEntry;
        long until = System.nanoTime() + WAIT_NANOS;
        while (entries.isEmpty()) {
            if (log.poll(onEntry, 16) < 0) {
                fail("the term's first entry: the poll stopped on fault " + log.fault());
            }
            if (System.nanoTime() > until) {
                fail("the term's first entry: not within 30 s; position " + log.position() + ", commit position "
                    + log.commitPosition() + ", ended " + log.isEnded());
            }
        }
        String expectedEntry = "0 " + (ENTRY_FRAME - 32) + " " + ENTRY_FRAME + " " + Abi.FLAGS_UNFRAGMENTED + " "
            + Abi.FRAME_TYPE_DATA;
        if (entries.size() != 1 || !entries.get(0).equals(expectedEntry)) {
            fail("expected one entry \"" + expectedEntry + "\" (position, bytes, frame length, flags, type), got " + entries);
        }
        int polling = Abi.ERR_POLLING;
        List<String> expected = List.of("poll -1 " + polling, "position -1 " + polling, "commitPosition -1 " + polling,
            "isEnded false " + polling, "failedAt false " + polling, "close -1 " + polling);
        if (!refusals.equals(expected)) {
            fail("the calls from inside the log's own handler answered " + refusals + ", not " + expected);
        }
        if (log.position() != NEXT || log.commitPosition() != NEXT) {
            fail("after the entry the log is at " + log.position() + " with bound " + log.commitPosition() + ", not " + NEXT
                + " and " + NEXT);
        }
        Subscription.Fault fault = log.fault();
        if (log.isEnded() || log.failedAt().isPresent() || fault.code() != 0 || fault.sessionId() != 0
            || fault.position() != Abi.NULL_POSITION) {
            fail("a live log read ended " + log.isEnded() + ", failed at " + log.failedAt() + ", fault " + fault);
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
            + ", commit position " + log.commitPosition() + "; six calls from inside the handler refused with ERR_POLLING");
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
        }
        if (!seen.equals(List.of(0L)) || !(thrown.getCause() instanceof IllegalStateException) || thrown.raised() != 1
            || throwing.position() != NEXT) {
            fail("a throwing handler saw " + seen + ", threw " + thrown + ", and left the log at " + throwing.position());
        }
        for (int i = 0; i < 50; i++) {
            if (throwing.poll(throwsAlways, 16) != 0) {
                fail("the entry whose handler threw was delivered again");
            }
        }
        throwing.close();

        // From the next position nothing is committed yet: no entry, and no entry before it.
        CommittedLog rest = openLog(client, NEXT);
        if (rest.position() != NEXT) {
            fail("a log opened at " + NEXT + " began at " + rest.position());
        }
        for (int i = 0; i < 100; i++) {
            if (rest.poll(onEntry, 16) != 0) {
                fail("a log opened at " + NEXT + " delivered an entry: " + entries);
            }
        }
        // A log that outlives its client: its poll stops on the closed fault, and its close
        // frees it with no client to stop the replay through.
        client.close();
        if (rest.poll(onEntry, 16) != -1 || rest.fault().code() != Abi.ERR_FAULT_CLOSED) {
            fail("a poll after the client's close did not stop on the closed fault: " + rest.fault());
        }
        rest.close();
        say("a throwing handler's entry is not delivered again; nothing at position " + NEXT
            + "; a log closed after its client");
    }
}
