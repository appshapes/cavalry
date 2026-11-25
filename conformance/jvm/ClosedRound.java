// The JVM client's closed round (spec/c-abi.md, "cluster service"; ADR 0155): a
// ClusterService whose driver stops. A poll with a client its driver closed is one answer,
// -1 with ERR_CLOSED as the thread's error and as fault()'s code, at the first poll that
// fails and at every poll after it, with the client's reason CLOSED_DRIVER_CLOSED; and the
// service's close is then no error. The driver hosts no member, so the service holds no log
// and its poll has no command to send: the case in which a poll of a closed client took
// nothing and returned 0 for as long as it was called. conformance/jvm/run.sh runs it last,
// against the driver its Quickstart ran on.
//
//     ClosedRound <the driver's process id>
//
// The client attaches to the driver at CAVALRY_DIR, and the program stops that driver
// itself, with the signal ProcessHandle.destroy sends the process id it was given, SIGTERM.
// A failed check prints what failed and exits 1; the wait is bounded at 60 s. The JDK alone
// (ADR 0003).

import com.appshapes.cavalry.Abi;
import com.appshapes.cavalry.CavalryException;
import com.appshapes.cavalry.Client;
import com.appshapes.cavalry.ClusterEntryValues;
import com.appshapes.cavalry.ClusterService;

import java.lang.foreign.MemorySegment;

public final class ClosedRound {
    static final long WAIT_NANOS = 60_000_000_000L;
    static final int POLLS_AFTER = 3;
    static int handed;

    private ClosedRound() {
    }

    static void fail(String what) {
        System.err.println("conformance/jvm: closed round: " + what);
        System.exit(1);
    }

    static int onEntry(MemorySegment body, ClusterEntryValues entry, ClusterService service) {
        handed++;
        return Abi.ACTION_CONTINUE;
    }

    /** What the poll that just returned -1 stopped on: ERR_CLOSED as the thread's error,
     * read before any other call, and as the fault's code. */
    static void closed(String what, int errcode, ClusterService service) {
        int code = service.fault().code();
        if (code != Abi.ERR_CLOSED || errcode != Abi.ERR_CLOSED) {
            fail(what + " stopped on code " + code + ", with the thread's error " + errcode + "; expected ERR_CLOSED, "
                + Abi.ERR_CLOSED + ", as both");
        }
    }

    public static void main(String[] args) throws Exception {
        long pid = args.length == 1 && args[0].matches("[0-9]{1,18}") ? Long.parseLong(args[0]) : 0;
        if (pid <= 1) {
            fail("usage: ClosedRound <the driver's process id>");
        }
        try {
            round(pid);
        } catch (CavalryException error) {
            fail(error.function() + " failed with error " + error.code() + ": " + error.abiMessage());
        }
    }

    static void round(long pid) throws Exception {
        Client client = new Client();
        ClusterService service = client.clusterService();

        // 1. Beside a driver that runs and hosts no member, a poll takes nothing.
        int taken = service.poll(ClosedRound::onEntry, 10);
        if (taken != 0 || client.isClosed()) {
            fail("a poll beside a driver with no member returned " + taken + ", and the client reads closed "
                + client.closed() + "; expected 0 beside a driver that runs");
        }

        // 2. The driver stops. Until the client has seen it, a poll takes nothing; the
        // first poll that fails is the close, and nothing else.
        ProcessHandle driver = ProcessHandle.of(pid).orElse(null);
        if (driver == null || !driver.destroy()) {
            fail("the driver, process " + pid + ", could not be asked to stop");
        }
        long until = System.nanoTime() + WAIT_NANOS;
        int polls = 0;
        int errcode;
        while (true) {
            taken = service.poll(ClosedRound::onEntry, 10);
            errcode = Abi.errcode();
            polls++;
            if (taken == -1) {
                break;
            }
            if (taken != 0) {
                fail("a poll beside a driver with no member returned " + taken + "; expected 0, or -1 after the stop");
            }
            if (System.nanoTime() - until > 0) {
                fail("no poll failed within 60 s of the driver's stop; the client reads closed " + client.closed());
            }
            Thread.sleep(1);
        }
        closed("the first poll that failed after the driver's stop", errcode, service);
        if (!client.isClosed() || client.closed() != Abi.CLOSED_DRIVER_CLOSED) {
            fail("the poll said closed, and the client reads closed " + client.closed() + "; expected CLOSED_DRIVER_CLOSED, "
                + Abi.CLOSED_DRIVER_CLOSED);
        }

        // 3. Every poll after it is the same answer: a loop that looks at what its poll
        // returned ends. The thread's error is set to another code before each, by a reply
        // from outside the handler: a success clears nothing, so without this the code read
        // after the poll could be the first failed poll's.
        for (int after = 1; after <= POLLS_AFTER; after++) {
            if (service.reply(1, MemorySegment.ofArray(new byte[] {'x'})) != -1 || Abi.errcode() != Abi.ERR_CLUSTER_NO_ENTRY) {
                fail("a reply from outside the handler was not refused with ERR_CLUSTER_NO_ENTRY: error " + Abi.errcode());
            }
            taken = service.poll(ClosedRound::onEntry, 10);
            errcode = Abi.errcode();
            if (taken != -1) {
                fail("poll " + after + " after the first that failed returned " + taken + "; expected -1");
            }
            closed("poll " + after + " after the first that failed", errcode, service);
        }
        if (handed != 0) {
            fail("the handler was called " + handed + " times beside a driver with no member");
        }

        // 4. The service closes with no error, and its client after it: a driver that is
        // gone cannot be told.
        service.close();
        client.close();
        System.out.println("conformance/jvm: closed round: a service beside a driver with no member took nothing; after the"
            + " driver's stop, poll " + polls + " and the " + POLLS_AFTER + " after it returned -1 with ERR_CLOSED, the"
            + " client's reason is CLOSED_DRIVER_CLOSED, and the service closed with no error");
        System.out.println("conformance/jvm: closed round ok");
    }
}
