// The JVM client's conformance runner (clients/README.md; ADR 0046, card 208): the
// ten-minute path's shape through clients/jvm, as conformance/python/quickstart.py runs it
// through clients/python. It checks that a package of another ABI version is refused by the
// check Abi makes when it loads, checkAbiVersion, which names both numbers. Against a driver
// at CAVALRY_DIR it checks the ABI version both ways, records stream 1001 on cavalry:ipc,
// publishes 1,000 messages of 64 bytes by claim and commit, each carrying its index and a
// pattern, and reads them back through the fragment handler once and in order, with the
// header's type, flags, version, frame length, term offset and session id checked against
// spec/frame.md; waits for the recording to sync to the stream's end and stops it; checks
// where a handler's throwable goes, in both orders of an Error and an exception, that a
// header kept past its call throws and a committed claim's segment is null, and the
// refusals of spec/c-abi.md, "Calls during a claim or a poll", a close from inside the
// handler included, that a commit, an abort and a reserved value after a close with a
// claim outstanding answer NO_CLAIM, and that a poll, a claim and a commit after the
// closes are answered by the ABI, on stream 1003 (edges); checks that a reserved value beyond 2^53 reaches
// the handler exactly on stream 1005 (reservedValues); adds publications from four threads
// through one Client at once (sharedClient); does the same 1,000 over cavalry:udp on the
// loopback; and replays the recording onto stream 2001, each frame at the position the
// recording holds it at, with the end reported. Every wait is bounded at 10 s, and a wait
// that runs out says how far it got. It exits 1 with the function, the code and
// cavalry_errmsg when a call failed, or with the code's name when a result code did.
//
// Then the report, printed and never gated, from two rounds that check nothing else. The
// latency round (latency): 100,000 messages over cavalry:ipc on stream 1004, one at a time
// on this thread, each carrying the publisher's System.nanoTime() from before its commit,
// which the handler subtracts from the clock it reads first; the one-way p50, p99, p99.99
// and max, nearest rank, so ten samples lie above the p99.99. The allocation round
// (allocations): 10,000 messages on stream 1002 whose handler only counts and whose
// publisher copies one constant, with the thread's allocated bytes read from
// com.sun.management.ThreadMXBean before and after the round with nothing between, which
// counts every object the thread allocated, freed or not, and the collections that ran.
// Java's standard library only (ADR 0003).

import com.appshapes.cavalry.Abi;
import com.appshapes.cavalry.CavalryException;
import com.appshapes.cavalry.Client;
import com.appshapes.cavalry.FragmentHandler;
import com.appshapes.cavalry.HandlerException;
import com.appshapes.cavalry.Header;
import com.appshapes.cavalry.Image;
import com.appshapes.cavalry.Publication;
import com.appshapes.cavalry.Recording;
import com.appshapes.cavalry.Replay;
import com.appshapes.cavalry.Subscription;

import java.lang.foreign.Arena;
import java.lang.foreign.MemorySegment;
import java.lang.management.GarbageCollectorMXBean;
import java.lang.management.ManagementFactory;
import java.net.DatagramSocket;
import java.net.InetAddress;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.Locale;

import static java.lang.foreign.ValueLayout.JAVA_LONG_UNALIGNED;

public final class Quickstart {
    static final int MESSAGES = 1000;
    static final int LATENCY_MESSAGES = 100_000;
    static final int ALLOCATION_MESSAGES = 10_000;
    static final int BODY = 64;
    static final int STREAM = 1001;
    static final int REPLAY_STREAM = 2001;
    static final long WAIT_NANOS = 10_000_000_000L;

    static void fail(String what) {
        System.err.println("conformance/jvm: " + what);
        System.exit(1);
    }

    static void say(String what) {
        System.out.println("conformance/jvm: " + what);
    }

    static String codeName(long code) {
        if (code == Abi.PUBLICATION_NOT_CONNECTED) return "NOT_CONNECTED";
        if (code == Abi.PUBLICATION_BACK_PRESSURED) return "BACK_PRESSURED";
        if (code == Abi.PUBLICATION_ADMIN_ACTION) return "ADMIN_ACTION";
        if (code == Abi.PUBLICATION_CLOSED) return "CLOSED";
        if (code == Abi.PUBLICATION_MAX_POSITION_EXCEEDED) return "MAX_POSITION_EXCEEDED";
        if (code == Abi.PUBLICATION_ERROR) return "ERROR";
        return Long.toString(code);
    }

    static long deadline() {
        return System.nanoTime() + WAIT_NANOS;
    }

    static boolean passed(long until) {
        return System.nanoTime() - until > 0;
    }

    /** What a subscription has received: the next index expected, and the frames' term ids
     * and offsets kept from the handler for the positions. */
    static final class Receiver {
        final String name;
        final Subscription subscription;
        int next;
        int sessionId;
        boolean haveSession;
        final int[] termIds = new int[MESSAGES];
        final int[] termOffsets = new int[MESSAGES];
        final long[] positions = new long[MESSAGES];
        Image constants;
        final FragmentHandler onFragment = this::onFragment;

        Receiver(String name, Subscription subscription) {
            this.name = name;
            this.subscription = subscription;
        }

        void onFragment(MemorySegment body, Header header) {
            if (next >= MESSAGES) fail(name + ": a fragment after all " + MESSAGES + " messages");
            int index = (int) body.get(JAVA_LONG_UNALIGNED, 0);
            if (index != next) fail(name + ": expected message " + next + ", got " + index);
            if (body.byteSize() != BODY) fail(name + ": message " + index + " has " + body.byteSize() + " bytes, not " + BODY);
            for (int b = 8; b < BODY; b++) {
                int got = body.get(java.lang.foreign.ValueLayout.JAVA_BYTE, b) & 0xff;
                if (got != ((index + b) & 0xff)) {
                    fail(name + ": message " + index + " byte " + b + " is " + got + ", not " + ((index + b) & 0xff));
                }
            }
            // The header against spec/frame.md: a data frame, unfragmented, version 0, the body
            // plus the 32-byte header, aligned to 32 in its term.
            int type = header.type(), flags = header.flags(), version = header.version();
            int frameLength = header.frameLength(), termOffset = header.termOffset();
            if (type != Abi.FRAME_TYPE_DATA || flags != Abi.FLAGS_UNFRAGMENTED || version != 0 || frameLength != 32 + BODY
                || termOffset % 32 != 0) {
                fail(name + ": message " + index + "'s header reads type " + type + ", flags " + flags + ", version "
                    + version + ", frame length " + frameLength + ", term offset " + termOffset);
            }
            if (!haveSession) {
                sessionId = header.sessionId();
                haveSession = true;
            } else if (header.sessionId() != sessionId) {
                fail(name + ": message " + index + " from session " + header.sessionId() + ", the stream's is " + sessionId);
            }
            termIds[next] = header.termId();
            termOffsets[next] = termOffset;
            next++;
        }

        /** The constants of the image the frames came from, once a poll has read one, for the
         * positions (spec/c-abi.md, "A frame's position"); the string is copied now. */
        void takeConstants() {
            if (constants != null || !haveSession) return;
            for (Image image : subscription.images()) {
                if (image.sessionId() == sessionId) {
                    constants = image;
                    return;
                }
            }
        }

        void computePositions() {
            if (constants == null) fail(name + ": no image carried session " + sessionId + " after " + next + " messages");
            for (int i = 0; i < next; i++) {
                int termCount = termIds[i] - constants.initialTermId(); // 32-bit two's complement
                positions[i] = ((long) termCount << constants.positionBitsToShift()) + termOffsets[i];
            }
        }

        void expectNoMore() {
            int count = subscription.poll(onFragment, 16);
            if (count != 0) fail(name + ": " + count + " more fragment(s) after all " + MESSAGES + " messages");
        }
    }

    /** A claim refused with the position, below zero: fail at once on a code that does not
     * pass, ERROR with the ABI's error, CLOSED or MAX_POSITION_EXCEEDED, or once the round's
     * 10 s have passed, saying how far the round got; else return, to claim again. */
    static void refused(String name, long position, int sent, long until, int total) {
        if (position == Abi.PUBLICATION_ERROR) {
            fail(name + ": cavalry_publication_try_claim failed with error " + Abi.errcode() + ": " + Abi.errmsg());
        }
        if (position == Abi.PUBLICATION_CLOSED || position == Abi.PUBLICATION_MAX_POSITION_EXCEEDED) {
            fail(name + ": tryClaim returned " + codeName(position) + " (" + position + ") after " + sent + " of " + total
                + " messages");
        }
        if (passed(until)) {
            fail(name + ": published " + sent + " of " + total + " in 10 s; the last claim returned " + codeName(position)
                + " (" + position + ")");
        }
    }

    /** One poll of up to 16 fragments: the count, or a failure with the subscription's fault or
     * the handler's throwable, saying how far the round got. */
    static int pollOrFail(String name, Subscription subscription, FragmentHandler handler, int received, int total) {
        int count;
        try {
            count = subscription.poll(handler, 16);
        } catch (HandlerException e) {
            fail(name + ": the fragment handler threw after " + received + " of " + total + " messages: " + e);
            return -1;
        }
        if (count < 0) {
            Subscription.Fault fault = subscription.fault();
            fail(name + ": poll stopped on fault " + fault.code() + " of session " + fault.sessionId() + " at position "
                + fault.position() + " after " + received + " of " + total + " messages; " + Abi.errmsg());
        }
        return count;
    }

    static void writeMessage(MemorySegment claim, int i) {
        claim.set(JAVA_LONG_UNALIGNED, 0, i);
        for (int b = 8; b < BODY; b++) {
            claim.set(java.lang.foreign.ValueLayout.JAVA_BYTE, b, (byte) (i + b));
        }
    }

    static void publishAndReceive(Publication publication, Receiver r) {
        long until = deadline();
        for (int i = 0; i < MESSAGES; ) {
            // The image joins on a poll; take its constants once a frame from it is read.
            pollOrFail(r.name, r.subscription, r.onFragment, r.next, MESSAGES);
            r.takeConstants();
            long position = publication.tryClaim(BODY);
            if (position < 0) {
                refused(r.name, position, i, until, MESSAGES);
                continue;
            }
            writeMessage(publication.claim(), i);
            if (publication.commit() != position) fail(r.name + ": commit did not return the claim's position " + position);
            i++;
        }
        until = deadline();
        while (r.next < MESSAGES) {
            pollOrFail(r.name, r.subscription, r.onFragment, r.next, MESSAGES);
            r.takeConstants();
            if (passed(until)) fail(r.name + ": received " + r.next + " of " + MESSAGES + " in 10 s");
        }
        r.expectNoMore();
        r.computePositions();
    }

    /** Offers the message, retrying a refusal that passes, NOT_CONNECTED, BACK_PRESSURED or
     * ADMIN_ACTION, for up to 10 s; the position. */
    static long offerOrFail(String name, Publication publication, MemorySegment message, int sent) {
        long until = deadline();
        while (true) {
            long position = publication.offer(message);
            if (position >= 0) return position;
            if (position == Abi.PUBLICATION_CLOSED || position == Abi.PUBLICATION_MAX_POSITION_EXCEEDED
                || position == Abi.PUBLICATION_ERROR || passed(until)) {
                fail(name + ": offer returned " + codeName(position) + " (" + position + ") after " + sent + " messages");
            }
        }
    }

    static MemorySegment message(int i) {
        byte[] m = new byte[BODY];
        MemorySegment segment = MemorySegment.ofArray(m);
        segment.set(JAVA_LONG_UNALIGNED, 0, i);
        return segment;
    }

    static int indexOf(MemorySegment body) {
        return (int) body.get(JAVA_LONG_UNALIGNED, 0);
    }

    static long claimOrFail(String name, Publication publication, int sent) {
        long until = deadline();
        while (true) {
            long position = publication.tryClaim(BODY);
            if (position >= 0) return position;
            refused(name, position, sent, until, sent + 1);
        }
    }

    /** The client's edges, on stream 1003. Where a handler's throwable goes
     * (clients/jvm/README.md, "The exception"): ten messages offered, then one poll of ten
     * whose handler throws on message 3. The ABI's poll consumes all ten, so every one must
     * reach the handler, once and in order, and poll must throw HandlerException with
     * fragments 10, raised 1 and the exception as its cause. Then a poll whose handler throws
     * an Error on one message and an exception on the next: raised 2 and the Error, the
     * first, as the cause. Then the lifetimes: a committed claim's segment is null, and a
     * header kept past its handler call throws IllegalStateException. Then the refusals of
     * spec/c-abi.md, "Calls during a claim or a poll", which leave the wrapper as they found
     * it: a poll from inside a handler of its own subscription, a second claim with one
     * outstanding, a close with a claim outstanding, after which the claim's commit, abort
     * and reserved value answer NO_CLAIM, and a close from inside the handler, which keeps
     * the handle. */
    static void edges(Client client) {
        String name = "handler exceptions";
        Subscription subscription = client.addSubscription("cavalry:ipc", STREAM + 2);
        Publication publication = client.addPublication("cavalry:ipc", STREAM + 2);
        List<Integer> seen = new ArrayList<>();
        FragmentHandler unexpected = (body, header) -> fail("a fragment the test did not expect, after " + seen);

        // A subscription takes its image on a poll, once the client's conductor has handed it
        // over, which is after the driver made it: a publication can accept an offer before
        // the subscription holds the image, and a poll then delivers nothing (cards 223, 233
        // and 235). So wait for the image first. Nothing is offered yet, so these polls deliver
        // nothing, and the one poll of ten below reads every message.
        long until = deadline();
        while (subscription.imageCount() == 0) {
            subscription.poll(unexpected, 10);
            if (passed(until)) fail(name + ": the subscription held no image after 10 s");
        }
        for (int i = 0; i < 10; i++) offerOrFail(name, publication, message(i), i);

        FragmentHandler throwsOn3 = (body, header) -> {
            int index = indexOf(body);
            seen.add(index);
            if (index == 3) throw new IllegalArgumentException("message 3");
        };
        HandlerException error = null;
        try {
            subscription.poll(throwsOn3, 10);
        } catch (HandlerException e) {
            error = e;
        }
        if (error == null) fail(name + ": a handler that threw on message 3 did not make poll throw HandlerException; it saw " + seen);
        if (!seen.equals(range(0, 10)) || error.fragments() != 10 || error.raised() != 1
            || !(error.getCause() instanceof IllegalArgumentException) || !"message 3".equals(error.getCause().getMessage())) {
            fail(name + ": the handler saw " + seen + "; HandlerException has fragments " + error.fragments() + ", raised "
                + error.raised() + ", cause " + error.getCause() + "; expected messages 0 to 9 once and in order, 10, 1 and the exception");
        }
        if (!error.getMessage().contains("Fix the handler") || !error.getMessage().endsWith("See clients/jvm/README.md#the-exception")) {
            fail(name + ": HandlerException's message does not say what to do and where it is explained: " + error.getMessage());
        }

        offerOrFail(name, publication, message(10), 10);
        offerOrFail(name, publication, message(11), 11);
        FragmentHandler throwsTwice = (body, header) -> {
            int index = indexOf(body);
            seen.add(index);
            if (index == 10) throw new AssertionError("message 10");
            throw new IllegalStateException("message 11");
        };
        error = null;
        try {
            subscription.poll(throwsTwice, 10);
        } catch (HandlerException e) {
            error = e;
        }
        if (error == null || !seen.equals(range(0, 12)) || error.fragments() != 2 || error.raised() != 2
            || !(error.getCause() instanceof AssertionError) || !"message 10".equals(error.getCause().getMessage())) {
            fail(name + ": poll threw " + error + " and the handler saw " + seen + "; expected HandlerException with fragments 2,"
                + " raised 2 and the Error, the first, as its cause, and messages 0 to 11");
        }
        if (subscription.poll(unexpected, 10) != 0) fail(name + ": a fragment after all 12 messages");
        say(name + ": a handler that threw on message 3 of a poll of 10 saw all 10 once, in order, and poll threw HandlerException"
            + " (fragments 10, raised 1, the exception as its cause); a handler that threw an Error and then an exception in one"
            + " poll gave raised 2 and the Error as the cause");

        // The lifetimes: the claim's segment is null after its commit, and a header read after
        // its handler call throws. A segment kept past its call points into the log; the README
        // says so.
        name = "lifetimes";
        long position = claimOrFail(name, publication, 12);
        MemorySegment view = publication.claim();
        view.set(JAVA_LONG_UNALIGNED, 0, 12);
        if (publication.commit() != position || publication.claim() != null) {
            fail(name + ": the commit did not return the claim's position " + position + " and set the claim to null");
        }
        Header[] kept = {null};
        int[] firstRead = {0, 0};
        FragmentHandler keeps = (body, header) -> {
            kept[0] = header;
            firstRead[0] = header.sessionId();
            firstRead[1] = header.sessionId();
        };
        until = deadline();
        while (kept[0] == null) {
            pollOrFail(name, subscription, keeps, 0, 1);
            if (passed(until)) fail(name + ": message 12 did not arrive in 10 s");
        }
        if (firstRead[0] != firstRead[1]) fail(name + ": two reads of sessionId in one handler call gave " + firstRead[0] + " and " + firstRead[1]);
        try {
            int late = kept[0].sessionId();
            fail(name + ": a header read after its handler call gave session " + late);
        } catch (IllegalStateException expected) {
            if (!expected.getMessage().contains("clients/jvm/README.md")) fail(name + ": the late read's message names no docs: " + expected.getMessage());
        }
        say(name + ": a committed claim's segment is null, a header read after its handler call threw IllegalStateException, and"
            + " sessionId gave the one number on each read inside the call");

        // Calls the ABI refuses during a poll or a claim leave the wrapper as they found it
        // (spec/c-abi.md, "Calls during a claim or a poll").
        name = "refusals";
        for (int i = 13; i < 16; i++) offerOrFail(name, publication, message(i), i);
        List<Integer> outer = new ArrayList<>();
        List<int[]> nested = new ArrayList<>();
        FragmentHandler nestedHandler = (body, header) -> fail("a poll from inside a handler of the same subscription delivered a fragment");
        FragmentHandler outerHandler = (body, header) -> {
            outer.add(indexOf(body));
            int count = subscription.poll(nestedHandler, 10);
            nested.add(new int[] {count, Abi.errcode()});
        };
        until = deadline();
        while (outer.size() < 3) {
            pollOrFail(name, subscription, outerHandler, outer.size(), 3);
            if (passed(until)) fail(name + ": received messages " + outer + " of 13 to 15 in 10 s");
        }
        boolean refusedEach = nested.size() == 3;
        for (int[] result : nested) refusedEach &= result[0] == -1 && result[1] == Abi.ERR_POLLING;
        if (!outer.equals(List.of(13, 14, 15)) || !refusedEach) {
            fail(name + ": the outer handler saw " + outer + " and the polls from inside it returned " + nested.size()
                + " results; expected messages 13 to 15, and -1 with ERR_POLLING (" + Abi.ERR_POLLING + ") each time");
        }

        position = claimOrFail(name, publication, 16);
        MemorySegment first = publication.claim();
        first.set(JAVA_LONG_UNALIGNED, 0, 16);
        long second = publication.tryClaim(BODY);
        int code = Abi.errcode();
        if (second != Abi.PUBLICATION_ERROR || code != Abi.ERR_CLAIM_OUTSTANDING || publication.claim() != first) {
            fail(name + ": a tryClaim with a claim outstanding returned " + codeName(second) + " with error " + code
                + ", and the claim is " + publication.claim() + "; expected ERROR with ERR_CLAIM_OUTSTANDING and the outstanding claim's segment");
        }
        if (publication.commit() != position || publication.claim() != null) {
            fail(name + ": the outstanding claim's commit did not return its position " + position + " and set the claim to null");
        }
        List<Integer> received = new ArrayList<>();
        FragmentHandler index = (body, header) -> received.add(indexOf(body));
        until = deadline();
        while (received.isEmpty()) {
            pollOrFail(name, subscription, index, 16, 17);
            if (passed(until)) fail(name + ": message 16 did not arrive in 10 s");
        }
        if (!received.equals(List.of(16))) fail(name + ": received " + received + " for the claim committed past a refused one; expected message 16");

        claimOrFail(name, publication, 17);
        publication.close();
        if (publication.claim() != null) fail(name + ": the publication's close left the claim " + publication.claim());
        // The struct claim 17 filled names the handle the close freed; the close zeroed it
        // first, so the claim's calls after it hand the ABI a struct that names no
        // publication, which it answers -1 with ERR_NO_CLAIM without reading a handle
        // (spec/c-abi.md, "Which claim a struct names").
        long staleCommit = publication.commit();
        int staleCommitCode = Abi.errcode();
        long staleAbort = publication.abort();
        int staleAbortCode = Abi.errcode();
        int staleReserved = publication.setReservedValue(1);
        int staleReservedCode = Abi.errcode();
        if (staleCommit != -1 || staleCommitCode != Abi.ERR_NO_CLAIM || staleAbort != -1 || staleAbortCode != Abi.ERR_NO_CLAIM
            || staleReserved != -1 || staleReservedCode != Abi.ERR_NO_CLAIM) {
            fail(name + ": after a close with a claim outstanding, commit returned " + staleCommit + " with error " + staleCommitCode
                + ", abort " + staleAbort + " with error " + staleAbortCode + ", setReservedValue " + staleReserved + " with error "
                + staleReservedCode + "; expected -1 with ERR_NO_CLAIM (" + Abi.ERR_NO_CLAIM + ") from each");
        }
        pollOrFail(name, subscription, unexpected, 17, 17); // the close aborted the claim: padding, never a fragment

        // A close from inside the handler: refused with ERR_POLLING, the handle kept, the next
        // message still delivered, and the close after the poll succeeds.
        Publication again = client.addPublication("cavalry:ipc", STREAM + 2);
        offerOrFail(name, again, message(17), 17);
        CavalryException[] closeError = {null};
        FragmentHandler closesInside = (body, header) -> {
            received.add(indexOf(body));
            try {
                subscription.close();
            } catch (CavalryException e) {
                closeError[0] = e;
            }
        };
        until = deadline();
        while (received.size() < 2) {
            pollOrFail(name, subscription, closesInside, received.size(), 2);
            if (passed(until)) fail(name + ": message 17 did not arrive in 10 s");
        }
        if (closeError[0] == null || closeError[0].code() != Abi.ERR_POLLING || subscription.isClosed()) {
            fail(name + ": a close from inside the handler gave " + closeError[0] + " and isClosed is " + subscription.isClosed()
                + "; expected CavalryException with ERR_POLLING and the subscription open");
        }
        offerOrFail(name, again, message(18), 18);
        until = deadline();
        while (received.size() < 3) {
            pollOrFail(name, subscription, index, received.size(), 3);
            if (passed(until)) fail(name + ": message 18 did not arrive in 10 s after the refused close");
        }
        if (!received.equals(List.of(16, 17, 18))) fail(name + ": received " + received + "; expected messages 16, 17 and 18, the last after the refused close");
        subscription.close();
        if (!subscription.isClosed()) fail(name + ": the close after the poll left the subscription open");
        subscription.close(); // idempotent
        again.close();
        // After a close the wrapper hands the ABI a null handle, which it refuses with
        // ERR_NULL (spec/c-abi.md, "When a function cannot answer"): a poll answers -1, a
        // claim PUBLICATION_ERROR, and the commit of a struct that names no claim -1 with
        // ERR_NO_CLAIM; never a delivery, never a frame.
        int afterClose = subscription.poll(unexpected, 10);
        int afterCloseCode = Abi.errcode();
        long claimAfterClose = again.tryClaim(BODY);
        int claimAfterCloseCode = Abi.errcode();
        long commitAfterClose = again.commit();
        int commitAfterCloseCode = Abi.errcode();
        if (afterClose != -1 || afterCloseCode != Abi.ERR_NULL || claimAfterClose != Abi.PUBLICATION_ERROR
            || claimAfterCloseCode != Abi.ERR_NULL || commitAfterClose != -1 || commitAfterCloseCode != Abi.ERR_NO_CLAIM) {
            fail(name + ": after the closes, poll returned " + afterClose + " with error " + afterCloseCode + ", tryClaim "
                + codeName(claimAfterClose) + " with error " + claimAfterCloseCode + ", commit " + commitAfterClose
                + " with error " + commitAfterCloseCode + "; expected -1 and ERROR with ERR_NULL (" + Abi.ERR_NULL
                + "), and -1 with ERR_NO_CLAIM (" + Abi.ERR_NO_CLAIM + ")");
        }
        say(name + ": a poll from inside a handler of its own subscription returned -1 with ERR_POLLING and the outer handler saw"
            + " messages 13 to 15; a tryClaim with a claim outstanding returned ERROR with ERR_CLAIM_OUTSTANDING, the claim stayed"
            + " the first segment and its commit set it to null; a close with a claim outstanding set the claim to null, and the"
            + " claim's commit, abort and reserved value after it returned -1 with ERR_NO_CLAIM; a close"
            + " from inside the handler threw CavalryException with ERR_POLLING, kept the subscription open for message 18, and"
            + " the close after the poll closed it; after the closes, poll and tryClaim were refused by the ABI with ERR_NULL"
            + " and commit with ERR_NO_CLAIM");
    }

    static List<Integer> range(int from, int to) {
        List<Integer> out = new ArrayList<>();
        for (int i = from; i < to; i++) out.add(i);
        return out;
    }

    /** Checks, on stream 1005, that a reserved value beyond 2^53 and the smallest long reach
     * the handler exactly. */
    static void reservedValues(Client client) {
        String name = "reserved values";
        Subscription subscription = client.addSubscription("cavalry:ipc", STREAM + 4);
        Publication publication = client.addPublication("cavalry:ipc", STREAM + 4);
        long[] values = {(1L << 62) + 1, Long.MIN_VALUE};
        List<Long> read = new ArrayList<>();
        FragmentHandler onFragment = (body, header) -> read.add(header.reservedValue());
        long until = deadline();
        for (int i = 0; i < values.length; i++) {
            while (true) {
                subscription.poll(onFragment, 10);
                long position = publication.tryClaim(BODY);
                if (position >= 0) break;
                refused(name, position, i, until, values.length);
            }
            publication.claim().set(JAVA_LONG_UNALIGNED, 0, i);
            if (publication.setReservedValue(values[i]) != 0) {
                fail(name + ": setReservedValue(" + values[i] + ") failed with error " + Abi.errcode() + ": " + Abi.errmsg());
            }
            publication.commit();
        }
        until = deadline();
        while (read.size() < values.length) {
            pollOrFail(name, subscription, onFragment, read.size(), values.length);
            if (passed(until)) fail(name + ": received " + read.size() + " of " + values.length + " in 10 s");
        }
        if (!read.equals(List.of(values[0], values[1]))) fail(name + ": the handler read " + read + "; expected " + Arrays.toString(values) + " exactly");
        publication.close();
        subscription.close();
        say(name + ": 2^62+1 and the smallest long reached the handler exactly through reservedValue");
    }

    /** Adds publications from four platform threads through one Client at once: the client's
     * methods are synchronised, so threads may share it (clients/jvm/README.md, "Threads"). */
    static void sharedClient(Client client) throws InterruptedException {
        List<Thread> threads = new ArrayList<>();
        List<Publication> made = new ArrayList<>();
        Throwable[] failure = {null};
        for (int t = 0; t < 4; t++) {
            final int stream = STREAM + 10 + t;
            Thread thread = Thread.ofPlatform().unstarted(() -> {
                try {
                    Publication publication = client.addPublication("cavalry:ipc", stream);
                    synchronized (made) {
                        made.add(publication);
                    }
                } catch (Throwable e) {
                    failure[0] = e;
                }
            });
            threads.add(thread);
            thread.start();
        }
        for (Thread thread : threads) thread.join(10_000);
        if (failure[0] != null || made.size() != 4) fail("adds from four threads through one client: " + made.size() + " made, failure " + failure[0]);
        for (Publication publication : made) publication.close();
        say("four threads added a publication each through one Client at once; the client's lock serialised them");
    }

    /** The latency round, for the report: 100,000 messages over cavalry:ipc on stream 1004,
     * one at a time on this thread. The publisher writes System.nanoTime() into the claim
     * just before its commit; the handler reads the clock first and keeps the difference.
     * The latencies, in order. */
    static long[] latency(Client client) {
        String name = "the latency round";
        Subscription subscription = client.addSubscription("cavalry:ipc", STREAM + 3);
        Publication publication = client.addPublication("cavalry:ipc", STREAM + 3);
        long[] latencies = new long[LATENCY_MESSAGES];
        int[] received = {0};
        FragmentHandler onFragment = (body, header) -> {
            long now = System.nanoTime();
            latencies[received[0]] = now - body.get(JAVA_LONG_UNALIGNED, 0);
            received[0]++;
        };
        long until = deadline();
        for (int i = 0; i < LATENCY_MESSAGES; i++) {
            while (true) {
                long position = publication.tryClaim(BODY);
                if (position >= 0) break;
                refused(name, position, i, until, LATENCY_MESSAGES);
                pollOrFail(name, subscription, onFragment, received[0], LATENCY_MESSAGES); // the image joins on a poll
            }
            publication.claim().set(JAVA_LONG_UNALIGNED, 0, System.nanoTime());
            publication.commit();
            while (received[0] <= i) {
                pollOrFail(name, subscription, onFragment, received[0], LATENCY_MESSAGES);
                if (passed(until)) fail(name + ": received " + received[0] + " of " + LATENCY_MESSAGES + "; message " + i + " did not arrive in 10 s");
            }
            until = deadline(); // 10 s for each message, outside the interval it measures
        }
        if (subscription.poll(onFragment, 16) != 0) fail(name + ": a fragment after all " + LATENCY_MESSAGES + " messages");
        publication.close();
        subscription.close();
        return latencies;
    }

    static long collections() {
        long count = 0;
        for (GarbageCollectorMXBean gc : ManagementFactory.getGarbageCollectorMXBeans()) {
            count += Math.max(0, gc.getCollectionCount());
        }
        return count;
    }

    /** The allocation round, for the report: 10,000 messages over cavalry:ipc on stream 1002
     * whose handler only counts and whose publisher copies one constant, so that the checks'
     * objects are not in the count, with the thread's allocated bytes read before and after,
     * nothing between, after a collection. {measured, bytes, collections}. */
    static long[] allocations(Client client) {
        String name = "the allocation round";
        Subscription subscription = client.addSubscription("cavalry:ipc", STREAM + 1);
        Publication publication = client.addPublication("cavalry:ipc", STREAM + 1);
        int[] received = {0};
        FragmentHandler count = (body, header) -> received[0]++;
        byte[] pattern = new byte[BODY];
        for (int b = 0; b < BODY; b++) pattern[b] = (byte) b;
        MemorySegment constant = Arena.global().allocate(BODY);
        MemorySegment.copy(MemorySegment.ofArray(pattern), 0, constant, 0, BODY);
        long until = deadline();
        while (true) {
            pollOrFail(name, subscription, count, received[0], ALLOCATION_MESSAGES);
            long position = publication.tryClaim(BODY);
            if (position >= 0) break;
            refused(name, position, 0, until, ALLOCATION_MESSAGES);
        }
        MemorySegment.copy(constant, 0, publication.claim(), 0, BODY);
        publication.commit();
        int sent = 1;
        pollOrFail(name, subscription, count, received[0], ALLOCATION_MESSAGES);

        com.sun.management.ThreadMXBean threads = (com.sun.management.ThreadMXBean) ManagementFactory.getThreadMXBean();
        System.gc();
        int receivedBefore = received[0];
        long collectionsBefore = collections();
        long bytesBefore = threads.getCurrentThreadAllocatedBytes();
        until = deadline();
        while (sent < ALLOCATION_MESSAGES) {
            long position = publication.tryClaim(BODY);
            if (position >= 0) {
                MemorySegment.copy(constant, 0, publication.claim(), 0, BODY);
                publication.commit();
                sent++;
            } else {
                refused(name, position, sent, until, ALLOCATION_MESSAGES);
            }
            pollOrFail(name, subscription, count, received[0], ALLOCATION_MESSAGES);
        }
        while (received[0] < ALLOCATION_MESSAGES) {
            pollOrFail(name, subscription, count, received[0], ALLOCATION_MESSAGES);
            if (passed(until)) fail(name + ": received " + received[0] + " of " + ALLOCATION_MESSAGES + " in 10 s");
        }
        long bytesAfter = threads.getCurrentThreadAllocatedBytes();
        long collectionsAfter = collections();

        if (subscription.poll(count, 16) != 0) fail(name + ": a fragment after all " + ALLOCATION_MESSAGES + " messages");
        publication.close();
        subscription.close();
        return new long[] {ALLOCATION_MESSAGES - receivedBefore, bytesAfter - bytesBefore, collectionsAfter - collectionsBefore};
    }

    /** The nearest-rank percentile parts/whole of sorted values: the smallest value with at
     * least that share of the values at or below it, and how many lie above. */
    static long[] percentile(long[] sorted, int parts, int whole) {
        int rank = Math.max(1, (int) Math.ceil((double) sorted.length * parts / whole));
        return new long[] {sorted[rank - 1], sorted.length - rank};
    }

    static String commas(long n) {
        return String.format(Locale.US, "%,d", n);
    }

    static int freeUdpPort() throws Exception {
        try (DatagramSocket socket = new DatagramSocket(0, InetAddress.getLoopbackAddress())) {
            return socket.getLocalPort();
        }
    }

    public static void main(String[] args) throws Exception {
        // The load-time check (spec/c-abi.md, "Versions and what a reader rejects"): the check
        // Abi makes when it loads refuses a package whose ABI_VERSION is not the library's,
        // with a CavalryException naming both numbers.
        int version = Abi.ABI_VERSION;
        CavalryException refusal = null;
        try {
            Abi.checkAbiVersion(version + 1);
        } catch (CavalryException e) {
            refusal = e;
        }
        if (refusal == null) fail("a package of ABI version " + (version + 1) + " was not refused against a library of version " + version);
        if (refusal.code() != Abi.ERR_ABI_VERSION || !refusal.abiMessage().contains("is ABI version " + version + " and")
            || !refusal.abiMessage().contains("from a header of ABI version " + (version + 1) + ";")) {
            fail("a package of ABI version " + (version + 1) + " was refused with " + refusal + "; expected ERR_ABI_VERSION naming both versions");
        }
        say("ABI version: a package of version " + (version + 1) + " was refused by checkAbiVersion: " + refusal.getMessage());
        if (Abi.libraryAbiVersion() != version) fail("the library's ABI version is " + Abi.libraryAbiVersion() + ", the package's " + version);

        long[] latencies;
        long[] allocated;
        try (Client client = new Client()) {
            say("client " + client.clientId() + " on " + client.dir() + ", library " + Abi.version() + ", ABI version "
                + Abi.libraryAbiVersion() + ", Java " + Runtime.version());

            // 1. Record the stream, then publish 1,000 messages over cavalry:ipc and read them
            // back, once and in order.
            Subscription ipcSub = client.addSubscription("cavalry:ipc", STREAM);
            Publication ipcPub = client.addPublication("cavalry:ipc", STREAM);
            Recording recording = client.startRecording("cavalry:ipc", STREAM); // the publication first: it records a live one
            Receiver ipc = new Receiver("cavalry:ipc", ipcSub);
            publishAndReceive(ipcPub, ipc);
            long end = ipcPub.position();
            say(MESSAGES + " messages over cavalry:ipc by session " + ipc.sessionId + " received once, in order; the stream ends at " + end);
            long until = deadline();
            while (recording.syncedPosition() < end) {
                if (passed(until)) fail("the recording synced " + recording.syncedPosition() + " of " + end + " in 10 s");
            }
            client.stopRecording(recording.recordingId());
            long recordingId = recording.recordingId();
            recording.close();
            ipcPub.close();

            edges(client);
            reservedValues(client);
            sharedClient(client);

            // The report's two rounds, which check nothing else.
            latencies = latency(client);
            Arrays.sort(latencies);
            allocated = allocations(client);

            // 2. The same 1,000 over cavalry:udp on the loopback.
            String udpChannel = "cavalry:udp?endpoint=127.0.0.1:" + freeUdpPort();
            Subscription udpSub = client.addSubscription(udpChannel, STREAM);
            Publication udpPub = client.addPublication(udpChannel, STREAM);
            Receiver udp = new Receiver(udpChannel, udpSub);
            publishAndReceive(udpPub, udp);
            say(MESSAGES + " messages over " + udpChannel + " by session " + udp.sessionId + " received once, in order, from "
                + udp.constants.sourceIdentity());
            udpPub.close();
            udpSub.close();

            // 3. Replay the recording onto stream 2001: the same checks, each frame at the
            // position the recording holds it at (ADR 0030), and the end reported.
            Subscription replaySub = client.addSubscription("cavalry:ipc", REPLAY_STREAM);
            Replay replay = client.startReplay(recordingId, Abi.NULL_POSITION, Abi.NULL_LENGTH, REPLAY_STREAM, "cavalry:ipc");
            Receiver replayed = new Receiver("replay", replaySub);
            until = deadline();
            Replay.End state;
            while (true) {
                pollOrFail("replay", replaySub, replayed.onFragment, replayed.next, MESSAGES);
                replayed.takeConstants();
                state = replay.ended();
                if (replayed.next >= MESSAGES && state.ended()) break;
                if (passed(until)) {
                    if (state.ended()) fail("replayed " + replayed.next + " of " + MESSAGES + " in 10 s; the replay ended at " + state.position() + " with reason " + state.reason());
                    fail("replayed " + replayed.next + " of " + MESSAGES + " in 10 s; the replay has not ended, at position " + replay.position());
                }
            }
            replayed.expectNoMore();
            replayed.computePositions();
            if (state.position() != end || state.reason() != Abi.REPLAY_STOP_ENDED) {
                fail("the replay ended at " + state.position() + " with reason " + state.reason() + ", not at " + end + " with " + Abi.REPLAY_STOP_ENDED);
            }
            for (int i = 0; i < MESSAGES; i++) {
                if (replayed.positions[i] != ipc.positions[i]) fail("message " + i + " replayed at " + replayed.positions[i] + ", recorded at " + ipc.positions[i]);
            }
            say(MESSAGES + " messages replayed on stream " + REPLAY_STREAM + " at the positions the recording holds them, ended at " + state.position());
            replay.close();
            replaySub.close();
            ipcSub.close();
        }

        // The report, never gated: the laptop's or the runner's cost of this side of the
        // boundary (clients/jvm/README.md, "The runtime's side").
        long[] p50 = percentile(latencies, 50, 100);
        long[] p99 = percentile(latencies, 99, 100);
        long[] p9999 = percentile(latencies, 9999, 10000);
        say("report: one-way latency over " + commas(latencies.length) + " cavalry:ipc messages, one at a time on one thread, nearest"
            + " rank: p50 " + commas(p50[0]) + " ns, p99 " + commas(p99[0]) + " ns, p99.99 " + commas(p9999[0]) + " ns (" + p9999[1]
            + " samples above it), max " + commas(latencies[latencies.length - 1]) + " ns");
        say("report: allocations per message, this side of the boundary, over " + commas(allocated[0]) + " messages whose handler"
            + " counts and whose publisher copies a constant: " + String.format(Locale.US, "%.1f", (double) allocated[1] / allocated[0])
            + " bytes (com.sun.management.ThreadMXBean.getCurrentThreadAllocatedBytes, nothing read between), " + allocated[2]
            + " collection(s) during the round");
        say("ok");
    }
}
