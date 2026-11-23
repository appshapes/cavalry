// The JVM client's snapshot round (spec/c-abi.md, version 6, "A service's snapshot and its
// start"; ADR 0146, ADR 0147, ADR 0149): a service beside each of three cluster members that
// conformance/jvm/run.sh started with `cavalry run` takes a snapshot of its program's state
// that its member lists, and a service that starts again starts from it.
//
//     SnapshotRound <cluster directory> <the cavalry binary>
//
// The members' drivers are <cluster directory>/member-0, -1 and -2. The round runs after the
// cluster round, on the log that round left. It is the Python runner's,
// conformance/python/snapshot.py, through this client (conformance/README.md, "The snapshot
// round"):
//
// 1. Each service asks where it starts, before its first poll: restore answers
//    CLUSTER_RESTORED_FROM_START and no state, and a second is refused with
//    ERR_CLUSTER_RESTORE_TOO_LATE.
// 2. Each service reads an entry of the log. No snapshot is due, and snapshot is refused
//    with ERR_CLUSTER_NO_SNAPSHOT_DUE.
// 3. `cavalry snapshot` is run beside each member: beside the leader it exits 0 and prints
//    the snapshot's log position, and beside a follower it exits 1 and names the leader.
// 4. Each service's poll ends at the leader's request, handed over as
//    CLUSTER_ENTRY_SNAPSHOT, and snapshotDue is the position the command printed; the three
//    services hold one state there. From inside the handler at the request, snapshotDue is
//    empty with the thread's error at ERR_POLLING, and snapshot and restore are refused with
//    it.
// 5. Each service gives its state: snapshot is true. None is due after, and a second is
//    refused. The three services are then closed: one service beside a member takes
//    snapshots.
// 6. Beside each member a new service starts from the snapshot: restore answers
//    CLUSTER_RESTORED_STATE and the state as it was given, which only a member that lists
//    the snapshot names, and its position is the snapshot's.
// 7. `cavalry snapshot` is run beside the leader again. Each new service's first entry is
//    the request, at the first snapshot's position and no entry below it; its poll ends
//    there, and it gives its state. A start beside each member is then given that state.
// 8. A service that never asks where it starts reads the log from position 0. It meets the
//    first request with the state the first services gave, and the second with the state
//    the restarted ones hold. Its snapshot is false both times.
//
// The program's state is how many entries it has read and a hash of each entry's type,
// position, term, time and payload, in order, written out 77 times, 2,472 bytes, so that a
// snapshot holds it in three messages: the same beside every member. Every wait is bounded
// at 60 s, and a snapshot that the driver gave up at a step is given again. It exits 1 with
// what it found when a check fails. The JDK alone (ADR 0003).

import com.appshapes.cavalry.Abi;
import com.appshapes.cavalry.CavalryException;
import com.appshapes.cavalry.Client;
import com.appshapes.cavalry.ClusterEntryValues;
import com.appshapes.cavalry.ClusterService;

import java.io.IOException;
import java.lang.foreign.MemorySegment;
import java.lang.foreign.ValueLayout;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.charset.StandardCharsets;
import java.nio.file.Path;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.concurrent.TimeUnit;
import java.util.function.BooleanSupplier;
import java.util.function.Supplier;

public final class SnapshotRound {
    static final int MEMBERS = 3;
    static final long WAIT_NANOS = 60_000_000_000L;
    static final long IDLE_NANOS = 200_000L;        // between duty cycles
    static final int COPIES = 77;                   // of the hash in a state: three messages
    static final int STATE_LENGTH = 8 + 32 * COPIES; // 2,472 bytes

    /** A closed service, whose position sets the thread's error to ERR_NULL. */
    static ClusterService spent;

    private SnapshotRound() {
    }

    static void fail(String what) {
        System.err.println("conformance/jvm: snapshot round: " + what);
        System.exit(1);
    }

    /** What a call returned, or the code of the CavalryException it threw. */
    static String codeOf(Supplier<Object> call) {
        try {
            return "returned " + call.get();
        } catch (CavalryException thrown) {
            return "threw " + thrown.code();
        }
    }

    /** A service beside one member and its program: the state is how many entries it has
     * read and a hash over them, which every member's service computes alike. */
    static final class Program {
        final int member;
        final Client client;
        final ClusterService service;
        long count;
        byte[] digest = new byte[32];
        final List<String> entries = new ArrayList<>(); // "type@position" of every entry taken
        String inside; // what the three calls answered from inside the handler

        Program(String cluster, int member) {
            this.member = member;
            this.client = new Client(Path.of(cluster, "member-" + member).toString());
            this.service = client.clusterService();
        }

        /** The program's state as bytes: the count, eight bytes, and the hash 77 times. */
        byte[] state() {
            ByteBuffer state = ByteBuffer.allocate(STATE_LENGTH).order(ByteOrder.LITTLE_ENDIAN).putLong(count);
            for (int copy = 0; copy < COPIES; copy++) {
                state.put(digest);
            }
            return state.array();
        }

        /** Starts the program from a state, as its snapshot held it. */
        void take(byte[] state) {
            if (state.length != STATE_LENGTH) {
                fail("a state of " + state.length + " bytes that is not one the program gave");
            }
            count = ByteBuffer.wrap(state).order(ByteOrder.LITTLE_ENDIAN).getLong(0);
            digest = Arrays.copyOfRange(state, 8, 40);
            if (!Arrays.equals(state(), state)) {
                fail("a state of " + state.length + " bytes that is not one the program gave");
            }
        }

        /** Gives the program's state to the service's snapshot: what it answered. A snapshot
         * that a step of the driver's gave up is still due, and is given again. */
        String give() throws InterruptedException {
            long deadline = System.nanoTime() + WAIT_NANOS;
            byte[] state = state();
            while (true) {
                String answered = codeOf(() -> service.snapshot(state));
                if (!answered.equals("threw " + Abi.ERR_CLUSTER_SNAPSHOT_FAILED) || System.nanoTime() > deadline) {
                    return answered;
                }
                Thread.sleep(10);
            }
        }

        int onEntry(MemorySegment body, ClusterEntryValues entry, ClusterService handed) {
            ByteBuffer values = ByteBuffer.allocate(28).order(ByteOrder.LITTLE_ENDIAN)
                .putInt(entry.type()).putLong(entry.position()).putLong(entry.leadershipTermId())
                .putLong(entry.timestamp());
            MessageDigest hash;
            try {
                hash = MessageDigest.getInstance("SHA-256");
            } catch (NoSuchAlgorithmException missing) {
                throw new IllegalStateException(missing);
            }
            hash.update(digest);
            hash.update(values.array());
            hash.update(body.toArray(ValueLayout.JAVA_BYTE));
            count += 1;
            digest = hash.digest();
            entries.add(entry.type() + "@" + entry.position());
            if (entry.type() == Abi.CLUSTER_ENTRY_SNAPSHOT && inside == null) {
                // The poll holds the service and its client. The read answers empty for a
                // refusal as for "none due", so the thread's error is set to another code
                // first, as the cluster round does, and must be ERR_POLLING after.
                if (spent.position() != -1 || Abi.errcode() != Abi.ERR_NULL) {
                    fail("a closed service's position() did not answer -1 with ERR_NULL: error " + Abi.errcode());
                }
                byte[] state = state();
                boolean due = handed.snapshotDue().isPresent();
                inside = due + ", " + Abi.errcode() + ", " + codeOf(() -> handed.snapshot(state)) + ", "
                    + codeOf(() -> handed.restore().restored());
            }
            return Abi.ACTION_CONTINUE;
        }

        int poll() {
            int taken = service.poll(this::onEntry, 16);
            if (taken < 0) {
                fail("the poll of the service beside member " + member + " stopped with error " + Abi.errcode());
            }
            return taken;
        }

        /** Asks where the service starts until its driver hosts a member with a log. */
        ClusterService.Restored restore() throws InterruptedException {
            long deadline = System.nanoTime() + WAIT_NANOS;
            while (true) {
                ClusterService.Restored found = service.restore();
                if (found.restored() != Abi.CLUSTER_RESTORED_NO_MEMBER) {
                    return found;
                }
                if (System.nanoTime() > deadline) {
                    fail("the driver of member " + member + " hosts no member with a log after 60 s");
                }
                Thread.sleep(1);
            }
        }

        void close() {
            service.close();
            client.close();
        }
    }

    /** Polls every program's service that has no snapshot to give until done. */
    static void waitFor(String what, BooleanSupplier done, List<Program> programs) throws InterruptedException {
        long deadline = System.nanoTime() + WAIT_NANOS;
        while (!done.getAsBoolean()) {
            if (System.nanoTime() > deadline) {
                fail(what + ", after 60 s");
            }
            for (Program program : programs) {
                if (program.service.snapshotDue().isEmpty()) {
                    program.poll();
                }
            }
            Thread.sleep(0, (int) IDLE_NANOS);
        }
    }

    /** One run of `cavalry snapshot`: its exit code and what it printed. */
    record Ran(int exit, String out, String error) {
    }

    /** `cavalry snapshot` beside a member. The wait is the bound, and the streams are read
     * after it: the command prints a line or two, far under a pipe's size. */
    static Ran ask(String binary, String cluster, int member) throws IOException, InterruptedException {
        String dir = Path.of(cluster, "member-" + member).toString();
        Process process = new ProcessBuilder(binary, "snapshot", "--dir", dir).start();
        if (!process.waitFor(60, TimeUnit.SECONDS)) {
            process.destroyForcibly();
            fail("`cavalry snapshot` beside member " + member + " did not end in 60 s");
        }
        String out = new String(process.getInputStream().readAllBytes(), StandardCharsets.UTF_8);
        String error = new String(process.getErrorStream().readAllBytes(), StandardCharsets.UTF_8);
        return new Ran(process.exitValue(), out, error);
    }

    /** The log position that `cavalry snapshot` printed beside a member. */
    static long positionOf(Ran ran, int member) {
        String prefix = "member " + member + " lists a snapshot of log position ";
        if (ran.exit() != 0 || !ran.out().startsWith(prefix)) {
            fail("`cavalry snapshot` beside member " + member + ", exit " + ran.exit() + ": "
                + (ran.out() + ran.error()).strip());
        }
        return Long.parseLong(ran.out().substring(prefix.length()).split("[:,]")[0]);
    }

    static boolean allDue(List<Program> programs) {
        return programs.stream().allMatch(program -> program.service.snapshotDue().isPresent());
    }

    public static void main(String[] args) throws IOException, InterruptedException {
        if (args.length != 2) {
            fail("usage: SnapshotRound <cluster directory> <the cavalry binary>");
        }
        String cluster = args[0];
        String binary = args[1];
        String tooLate = "threw " + Abi.ERR_CLUSTER_RESTORE_TOO_LATE;
        String noneDue = "threw " + Abi.ERR_CLUSTER_NO_SNAPSHOT_DUE;
        String polling = "threw " + Abi.ERR_POLLING;
        Program closed = new Program(cluster, 0);
        spent = closed.service;
        closed.close();

        // 1. Where each service starts: no snapshot of a service is listed yet.
        List<Program> programs = new ArrayList<>();
        for (int member = 0; member < MEMBERS; member++) {
            programs.add(new Program(cluster, member));
        }
        for (Program program : programs) {
            if (program.service.snapshotDue().isPresent()) {
                fail("a snapshot is due beside member " + program.member + " before its service's first poll");
            }
            ClusterService.Restored found = program.restore();
            if (found.restored() != Abi.CLUSTER_RESTORED_FROM_START || found.state() != null) {
                fail("the service beside member " + program.member + " starts with " + found.restored()
                    + ", not from the log's start");
            }
            String again = codeOf(() -> program.service.restore().restored());
            if (!again.equals(tooLate)) {
                fail("a second restore beside member " + program.member + ": " + again);
            }
        }

        // 2. Every service reads an entry of the log the cluster round left; none is due.
        waitFor("every service read an entry", () -> programs.stream().allMatch(program -> program.count > 0), programs);
        for (Program program : programs) {
            String refused = codeOf(() -> program.service.snapshot(program.state()));
            if (!refused.equals(noneDue)) {
                fail("a snapshot where none is due beside member " + program.member + ": " + refused);
            }
        }

        // 3. `cavalry snapshot` beside each member: the leader's is taken, a follower's names
        // the leader.
        List<Ran> asked = new ArrayList<>();
        int found = -1;
        for (int member = 0; member < MEMBERS; member++) {
            Ran ran = ask(binary, cluster, member);
            asked.add(ran);
            if (ran.exit() == 0) {
                if (found >= 0) {
                    fail("`cavalry snapshot` exited 0 beside members " + found + " and " + member);
                }
                found = member;
            }
        }
        if (found < 0) {
            fail("`cavalry snapshot` exited 0 beside no member: " + asked);
        }
        int leader = found;
        long position = positionOf(asked.get(leader), leader);
        for (int member = 0; member < MEMBERS; member++) {
            Ran ran = asked.get(member);
            if (member != leader && (ran.exit() != 1 || !ran.error().contains("member " + leader + " leads"))) {
                fail("`cavalry snapshot` beside member " + member + ", a follower, exit " + ran.exit() + ": "
                    + ran.error().strip());
            }
        }

        // 4. Every service's poll ends at the request, and the snapshot is due at the
        // position the command printed, with one state beside the three members.
        waitFor("every service's poll ended at the leader's request", () -> allDue(programs), programs);
        for (Program program : programs) {
            long due = program.service.snapshotDue().orElse(-1);
            String last = program.entries.get(program.entries.size() - 1);
            if (due != position || program.service.position() != position
                || !last.startsWith(Abi.CLUSTER_ENTRY_SNAPSHOT + "@")) {
                fail("beside member " + program.member + " the snapshot is due at " + due + ", the service is at "
                    + program.service.position() + " and its last entry is " + last
                    + ": the command printed " + position);
            }
            if (!("false, " + Abi.ERR_POLLING + ", " + polling + ", " + polling).equals(program.inside)) {
                fail("from inside the handler at the request, beside member " + program.member + ": "
                    + program.inside);
            }
        }
        byte[] given = programs.get(0).state();
        if (given.length != STATE_LENGTH) {
            fail("a state of " + given.length + " bytes");
        }
        for (Program program : programs) {
            if (!Arrays.equals(program.state(), given)) {
                fail("the service beside member " + program.member + " holds another state at the snapshot's position");
            }
        }

        // 5. Each service gives its state, once, and is closed: the service that starts from
        // the snapshot is the one that takes the next.
        for (Program program : programs) {
            String written = program.give();
            if (!written.equals("returned true")) {
                fail("the service beside member " + program.member + " gave its state: " + written);
            }
            if (program.service.snapshotDue().isPresent()) {
                fail("a snapshot is still due beside member " + program.member + " after it was taken");
            }
            String again = codeOf(() -> program.service.snapshot(program.state()));
            if (!again.equals(noneDue)) {
                fail("a second snapshot beside member " + program.member + ": " + again);
            }
            program.close();
        }

        // 6. The member lists it: a new service beside each member starts from it. A member
        // lists the snapshot when it reads the service's word of it, a duty cycle or so
        // later, so a start that finds none is made again with a new service.
        List<Program> restarted = new ArrayList<>();
        for (int member = 0; member < MEMBERS; member++) {
            long deadline = System.nanoTime() + WAIT_NANOS;
            Program program;
            ClusterService.Restored start;
            while (true) {
                program = new Program(cluster, member);
                start = program.restore();
                if (start.restored() == Abi.CLUSTER_RESTORED_STATE) {
                    break;
                }
                program.close();
                if (System.nanoTime() > deadline) {
                    fail("member " + member + " lists no snapshot of its service after 60 s: a new service starts with "
                        + start.restored());
                }
                Thread.sleep(10);
            }
            if (!Arrays.equals(start.state(), given)) {
                fail("the service beside member " + member + " starts from " + start.state().length
                    + " bytes that are not the state given");
            }
            program.take(start.state());
            if (program.service.position() != position) {
                fail("the service beside member " + member + " starts at " + program.service.position() + ", not at "
                    + position);
            }
            Program started = program;
            String again = codeOf(() -> started.service.restore().restored());
            if (!again.equals(tooLate)) {
                fail("a second restore beside member " + member + ": " + again);
            }
            restarted.add(program);
        }

        // 7. A second request, beside the leader: each restarted service's first entry is the
        // request, at the first snapshot's position, and it gives its state.
        long second = positionOf(ask(binary, cluster, leader), leader);
        if (second <= position) {
            fail("a second snapshot at " + second + ", not past the first at " + position);
        }
        waitFor("every restarted service's poll ended at the second request", () -> allDue(restarted), restarted);
        for (Program program : restarted) {
            if (!program.entries.equals(List.of(Abi.CLUSTER_ENTRY_SNAPSHOT + "@" + position))
                || program.service.snapshotDue().orElse(-1) != second) {
                fail("the service beside member " + program.member + " that started from its snapshot at " + position
                    + " was handed " + program.entries + ", with a snapshot due at " + program.service.snapshotDue()
                    + ": the second request is the one entry at " + position + ", and asks for a snapshot at "
                    + second);
            }
        }
        byte[] after = restarted.get(0).state();
        if (Arrays.equals(after, given)) {
            fail("the restarted services hold the first state at the second snapshot's position");
        }
        for (Program program : restarted) {
            if (!Arrays.equals(program.state(), after)) {
                fail("the restarted services do not hold one state at the second snapshot's position");
            }
            String written = program.give();
            if (!written.equals("returned true")) {
                fail("the restarted service beside member " + program.member + " gave its state: " + written);
            }
        }
        // Each member lists the second snapshot too: a start beside it is given the new
        // state, once the member has read the service's word of it.
        for (int member = 0; member < MEMBERS; member++) {
            long deadline = System.nanoTime() + WAIT_NANOS;
            while (true) {
                Program probe = new Program(cluster, member);
                ClusterService.Restored start = probe.restore();
                long at = probe.service.position();
                probe.close();
                if (start.restored() == Abi.CLUSTER_RESTORED_STATE && Arrays.equals(start.state(), after)
                    && at == second) {
                    break;
                }
                if (System.nanoTime() > deadline) {
                    fail("member " + member + " does not list its service's second snapshot after 60 s: a new "
                        + "service starts with " + start.restored() + " at " + at);
                }
                Thread.sleep(10);
            }
        }

        // 8. A service that never asks reads from position 0 and meets both requests, each
        // with the state the others held there: a start from a snapshot and a read from the
        // start reach one state. Its member lists a snapshot at each, or past it.
        Program late = new Program(cluster, leader);
        long[] met = {position, second};
        byte[][] held = {given, after};
        for (int request = 0; request < met.length; request++) {
            waitFor("a service that reads from the start meets the request for " + met[request],
                () -> late.service.snapshotDue().isPresent(), List.of(late));
            if (late.service.snapshotDue().orElse(-1) != met[request] || !Arrays.equals(late.state(), held[request])) {
                fail("a service that read from the start is due at " + late.service.snapshotDue() + ", not at "
                    + met[request] + ", or holds another state than the services held there");
            }
            String listed = late.give();
            if (!listed.equals("returned false")) {
                fail("a snapshot of position " + met[request] + ", which the member already lists or is past: "
                    + listed);
            }
        }

        for (Program program : restarted) {
            program.close();
        }
        late.close();
        System.out.println("conformance/jvm: snapshot round: three services gave a state of " + given.length
            + " bytes at log position " + position + ", asked for by `cavalry snapshot` beside member " + leader
            + "; each member listed its service's snapshot, a new service beside each started from it, read the "
            + "next request at " + position + " and gave its state at " + second);
        System.out.println("conformance/jvm: snapshot round ok");
    }
}
