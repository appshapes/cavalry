// The .NET client's snapshot round (spec/c-abi.md, version 6, "A service's snapshot and its
// start"; ADR 0146, ADR 0147, ADR 0149): a service beside each of three cluster members that
// conformance/dotnet/run.sh started with `cavalry run` takes a snapshot of its program's
// state that its member lists, and a service that starts again starts from it.
//
//     Snapshot <cluster directory> <the cavalry binary>
//
// The members' drivers are <cluster directory>/member-0, -1 and -2. The round runs after the
// cluster round, on the log that round left. It is the Python runner's,
// conformance/python/snapshot.py, through this client (conformance/README.md, "The snapshot
// round"):
//
// 1. Each service asks where it starts, before its first poll: Restore answers
//    CLUSTER_RESTORED_FROM_START and no state, and a second is refused with
//    ERR_CLUSTER_RESTORE_TOO_LATE.
// 2. Each service reads an entry of the log. No snapshot is due, and Snapshot is refused
//    with ERR_CLUSTER_NO_SNAPSHOT_DUE.
// 3. `cavalry snapshot` is run beside each member: beside the leader it exits 0 and prints
//    the snapshot's log position, and beside a follower it exits 1 and names the leader.
// 4. Each service's poll ends at the leader's request, handed over as
//    CLUSTER_ENTRY_SNAPSHOT, and SnapshotDue is the position the command printed; the three
//    services hold one state there. From inside the handler at the request, SnapshotDue is
//    null with the thread's error at ERR_POLLING, and Snapshot and Restore are refused with
//    it.
// 5. Each service gives its state: Snapshot is true. None is due after, and a second is
//    refused. The three services are then closed: one service beside a member takes
//    snapshots.
// 6. Beside each member a new service starts from the snapshot: Restore answers
//    CLUSTER_RESTORED_STATE and the state as it was given, which only a member that lists
//    the snapshot names, and its position is the snapshot's.
// 7. `cavalry snapshot` is run beside the leader again. Each new service's first entry is
//    the request, at the first snapshot's position and no entry below it; its poll ends
//    there, and it gives its state. A start beside each member is then given that state.
// 8. A service that never asks where it starts reads the log from position 0. It meets the
//    first request with the state the first services gave, and the second with the state
//    the restarted ones hold. Its Snapshot is false both times.
//
// The program's state is how many entries it has read and a hash of each entry's type,
// position, term, time and payload, in order, written out 77 times, 2,472 bytes, so that a
// snapshot holds it in three messages: the same beside every member. Every wait is bounded
// at 60 s, and a snapshot that the driver gave up at a step is given again. It exits 1 with
// what it found when a check fails. .NET's base class library only (ADR 0003).

using System;
using System.Buffers.Binary;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Security.Cryptography;
using System.Threading;
using Cavalry;

namespace SnapshotRound;

public static class Program
{
    const int Members = 3;
    static readonly TimeSpan Wait = TimeSpan.FromSeconds(60);
    static readonly TimeSpan Idle = TimeSpan.FromMilliseconds(0.2); // between duty cycles
    const int Copies = 77; // of the hash in a state: 2,472 bytes, three messages of a snapshot
    const int StateLength = 8 + 32 * Copies;

    /// <summary>A closed service, whose Position sets the thread's error to ERR_NULL.</summary>
    static ClusterService? spent;

    static void Fail(string what)
    {
        Console.Error.WriteLine("conformance/dotnet: snapshot round: " + what);
        Environment.Exit(1);
    }

    /// <summary>What <paramref name="call"/> returned, or the code of the
    /// <see cref="CavalryException"/> it threw.</summary>
    static string CodeOf(Func<object?> call)
    {
        try
        {
            return "returned " + (call() ?? "null");
        }
        catch (CavalryException thrown)
        {
            return "threw " + thrown.Code;
        }
    }

    /// <summary>A service beside one member and its program: the state is how many entries
    /// it has read and a hash over them, which every member's service computes alike.</summary>
    sealed class Service
    {
        public readonly int Member;
        public readonly Client Client;
        public readonly ClusterService Handle;
        public long Count;
        public byte[] Digest = new byte[32];
        public readonly List<(int Type, long Position)> Entries = new(); // every entry taken since it began
        public string? Inside; // what the three calls answered from inside the handler

        public Service(string cluster, int member)
        {
            Member = member;
            Client = new Client(Path.Combine(cluster, "member-" + member));
            Handle = Client.ClusterService();
        }

        /// <summary>The program's state as bytes: the count, eight bytes, and the hash 77
        /// times.</summary>
        public byte[] State()
        {
            byte[] state = new byte[StateLength];
            BinaryPrimitives.WriteInt64LittleEndian(state, Count);
            for (int copy = 0; copy < Copies; copy++)
            {
                Digest.CopyTo(state, 8 + 32 * copy);
            }
            return state;
        }

        /// <summary>Starts the program from a state, as its snapshot held it.</summary>
        public void Take(byte[] state)
        {
            if (state.Length != StateLength)
            {
                Fail("a state of " + state.Length + " bytes that is not one the program gave");
            }
            Count = BinaryPrimitives.ReadInt64LittleEndian(state);
            Digest = state.AsSpan(8, 32).ToArray();
            if (!State().SequenceEqual(state))
            {
                Fail("a state of " + state.Length + " bytes that is not one the program gave");
            }
        }

        /// <summary>Gives the program's state to the service's snapshot: what it answered. A
        /// snapshot that a step of the driver's gave up is still due, and is given
        /// again.</summary>
        public string Give()
        {
            Stopwatch waited = Stopwatch.StartNew();
            byte[] state = State();
            while (true)
            {
                string answered = CodeOf(() => Handle.Snapshot(state));
                if (answered != "threw " + Abi.ERR_CLUSTER_SNAPSHOT_FAILED || waited.Elapsed > Wait)
                {
                    return answered;
                }
                Thread.Sleep(10);
            }
        }

        int OnEntry(ReadOnlySpan<byte> body, ClusterEntryValues entry, ClusterService service)
        {
            byte[] hashed = new byte[32 + 28 + body.Length];
            Digest.CopyTo(hashed, 0);
            BinaryPrimitives.WriteInt32LittleEndian(hashed.AsSpan(32), entry.Type);
            BinaryPrimitives.WriteInt64LittleEndian(hashed.AsSpan(36), entry.Position);
            BinaryPrimitives.WriteInt64LittleEndian(hashed.AsSpan(44), entry.LeadershipTermId);
            BinaryPrimitives.WriteInt64LittleEndian(hashed.AsSpan(52), entry.Timestamp);
            body.CopyTo(hashed.AsSpan(60));
            Count += 1;
            Digest = SHA256.HashData(hashed);
            Entries.Add((entry.Type, entry.Position));
            if (entry.Type == Abi.CLUSTER_ENTRY_SNAPSHOT && Inside == null)
            {
                // The poll holds the service and its client. The read answers null for a
                // refusal as for "none due", so the thread's error is set to another code
                // first, as the cluster round does, and must be ERR_POLLING after.
                if (spent!.Position() != -1 || Abi.Errcode() != Abi.ERR_NULL)
                {
                    Fail("a closed service's Position() did not answer -1 with ERR_NULL: error " + Abi.Errcode());
                }
                byte[] state = State();
                long? due = service.SnapshotDue();
                Inside = (due?.ToString() ?? "null") + ", " + Abi.Errcode() + ", "
                    + CodeOf(() => service.Snapshot(state)) + ", "
                    + CodeOf(() => service.Restore().Restored);
            }
            return Abi.ACTION_CONTINUE;
        }

        public int Poll()
        {
            int count = Handle.Poll(OnEntry, 16);
            if (count < 0)
            {
                Fail("the poll of the service beside member " + Member + " stopped with error " + Abi.Errcode());
            }
            return count;
        }

        /// <summary>Asks where the service starts until its driver hosts a member with a
        /// log.</summary>
        public (int Restored, byte[]? State) Restore()
        {
            Stopwatch waited = Stopwatch.StartNew();
            while (true)
            {
                (int Restored, byte[]? State) found = Handle.Restore();
                if (found.Restored != Abi.CLUSTER_RESTORED_NO_MEMBER)
                {
                    return found;
                }
                if (waited.Elapsed > Wait)
                {
                    Fail("the driver of member " + Member + " hosts no member with a log after 60 s");
                }
                Thread.Sleep(1);
            }
        }

        public void Close()
        {
            Handle.Close();
            Client.Close();
        }
    }

    /// <summary>Polls every service that has no snapshot to give until done.</summary>
    static void WaitFor(string what, Func<bool> done, IReadOnlyList<Service> services)
    {
        Stopwatch waited = Stopwatch.StartNew();
        while (!done())
        {
            if (waited.Elapsed > Wait)
            {
                Fail(what + ", after 60 s");
            }
            foreach (Service service in services)
            {
                if (service.Handle.SnapshotDue() == null)
                {
                    service.Poll();
                }
            }
            Thread.Sleep(Idle);
        }
    }

    /// <summary>`cavalry snapshot` beside a member: its exit code and what it printed.</summary>
    static (int Exit, string Out, string Error) Ask(string binary, string cluster, int member)
    {
        var start = new ProcessStartInfo(binary)
        {
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            UseShellExecute = false,
        };
        start.ArgumentList.Add("snapshot");
        start.ArgumentList.Add("--dir");
        start.ArgumentList.Add(Path.Combine(cluster, "member-" + member));
        using Process ran = Process.Start(start) ?? throw new InvalidOperationException("no process");
        // Each stream is read as it comes, so the wait below is the bound: the command
        // prints a line or two, far under a pipe's size.
        var output = ran.StandardOutput.ReadToEndAsync();
        var error = ran.StandardError.ReadToEndAsync();
        if (!ran.WaitForExit((int) Wait.TotalMilliseconds))
        {
            ran.Kill();
            Fail("`cavalry snapshot` beside member " + member + " did not end in 60 s");
        }
        return (ran.ExitCode, output.Result, error.Result);
    }

    /// <summary>The log position that `cavalry snapshot` printed beside a member.</summary>
    static long PositionOf((int Exit, string Out, string Error) ran, int member)
    {
        string prefix = "member " + member + " lists a snapshot of log position ";
        if (ran.Exit != 0 || !ran.Out.StartsWith(prefix, StringComparison.Ordinal))
        {
            Fail("`cavalry snapshot` beside member " + member + ", exit " + ran.Exit + ": " + (ran.Out + ran.Error).Trim());
        }
        return long.Parse(ran.Out.Substring(prefix.Length).Split(':', ',')[0]);
    }

    public static int Main(string[] args)
    {
        if (args.Length != 2)
        {
            Fail("usage: Snapshot <cluster directory> <the cavalry binary>");
        }
        string cluster = args[0];
        string binary = args[1];
        string tooLate = "threw " + Abi.ERR_CLUSTER_RESTORE_TOO_LATE;
        string noneDue = "threw " + Abi.ERR_CLUSTER_NO_SNAPSHOT_DUE;
        string polling = "threw " + Abi.ERR_POLLING;
        var closed = new Service(cluster, 0);
        spent = closed.Handle;
        closed.Close();

        // 1. Where each service starts: no snapshot of a service is listed yet.
        List<Service> services = Enumerable.Range(0, Members).Select(member => new Service(cluster, member)).ToList();
        foreach (Service service in services)
        {
            if (service.Handle.SnapshotDue() != null)
            {
                Fail("a snapshot is due beside member " + service.Member + " before its service's first poll");
            }
            (int Restored, byte[]? State) found = service.Restore();
            if (found.Restored != Abi.CLUSTER_RESTORED_FROM_START || found.State != null)
            {
                Fail("the service beside member " + service.Member + " starts with " + found.Restored
                    + ", not from the log's start");
            }
            string again = CodeOf(() => service.Handle.Restore().Restored);
            if (again != tooLate)
            {
                Fail("a second restore beside member " + service.Member + ": " + again);
            }
        }

        // 2. Every service reads an entry of the log the cluster round left; none is due.
        WaitFor("every service read an entry", () => services.All(service => service.Count > 0), services);
        foreach (Service service in services)
        {
            byte[] state = service.State();
            string refused = CodeOf(() => service.Handle.Snapshot(state));
            if (refused != noneDue)
            {
                Fail("a snapshot where none is due beside member " + service.Member + ": " + refused);
            }
        }

        // 3. `cavalry snapshot` beside each member: the leader's is taken, a follower's names
        // the leader.
        List<(int Exit, string Out, string Error)> asked =
            Enumerable.Range(0, Members).Select(member => Ask(binary, cluster, member)).ToList();
        List<int> leaders = Enumerable.Range(0, Members).Where(member => asked[member].Exit == 0).ToList();
        if (leaders.Count != 1)
        {
            Fail("`cavalry snapshot` did not exit 0 beside exactly one member: " + string.Join("; ",
                asked.Select((ran, member) => "member " + member + ": exit " + ran.Exit + ": " + (ran.Out + ran.Error).Trim())));
        }
        int leader = leaders[0];
        long position = PositionOf(asked[leader], leader);
        for (int member = 0; member < Members; member++)
        {
            if (member != leader && (asked[member].Exit != 1
                || !asked[member].Error.Contains("member " + leader + " leads", StringComparison.Ordinal)))
            {
                Fail("`cavalry snapshot` beside member " + member + ", a follower, exit " + asked[member].Exit + ": "
                    + asked[member].Error.Trim());
            }
        }

        // 4. Every service's poll ends at the request, and the snapshot is due at the
        // position the command printed, with one state beside the three members.
        WaitFor("every service's poll ended at the leader's request",
            () => services.All(service => service.Handle.SnapshotDue() != null), services);
        foreach (Service service in services)
        {
            long? due = service.Handle.SnapshotDue();
            (int Type, long Position) last = service.Entries[^1];
            if (due != position || service.Handle.Position() != position || last.Type != Abi.CLUSTER_ENTRY_SNAPSHOT)
            {
                Fail("beside member " + service.Member + " the snapshot is due at " + due + ", the service is at "
                    + service.Handle.Position() + " and its last entry is " + last
                    + ": the command printed " + position);
            }
            if (service.Inside != "null, " + Abi.ERR_POLLING + ", " + polling + ", " + polling)
            {
                Fail("from inside the handler at the request, beside member " + service.Member + ": " + service.Inside);
            }
        }
        byte[] given = services[0].State();
        if (given.Length != StateLength || services.Any(service => !service.State().SequenceEqual(given)))
        {
            Fail("the three services do not hold one state of 2,472 bytes at the snapshot's position");
        }

        // 5. Each service gives its state, once, and is closed: the service that starts from
        // the snapshot is the one that takes the next.
        foreach (Service service in services)
        {
            byte[] state = service.State();
            string written = service.Give();
            if (written != "returned True")
            {
                Fail("the service beside member " + service.Member + " gave its state: " + written);
            }
            if (service.Handle.SnapshotDue() != null)
            {
                Fail("a snapshot is still due beside member " + service.Member + " after it was taken");
            }
            string again = CodeOf(() => service.Handle.Snapshot(state));
            if (again != noneDue)
            {
                Fail("a second snapshot beside member " + service.Member + ": " + again);
            }
            service.Close();
        }

        // 6. The member lists it: a new service beside each member starts from it. A member
        // lists the snapshot when it reads the service's word of it, a duty cycle or so
        // later, so a start that finds none is made again with a new service.
        var restarted = new List<Service>();
        for (int member = 0; member < Members; member++)
        {
            Stopwatch waited = Stopwatch.StartNew();
            Service service;
            (int Restored, byte[]? State) found;
            while (true)
            {
                service = new Service(cluster, member);
                found = service.Restore();
                if (found.Restored == Abi.CLUSTER_RESTORED_STATE)
                {
                    break;
                }
                service.Close();
                if (waited.Elapsed > Wait)
                {
                    Fail("member " + member + " lists no snapshot of its service after 60 s: a new service starts with "
                        + found.Restored);
                }
                Thread.Sleep(10);
            }
            byte[] state = found.State ?? Array.Empty<byte>();
            if (!state.SequenceEqual(given))
            {
                Fail("the service beside member " + member + " starts from " + state.Length
                    + " bytes that are not the state given");
            }
            service.Take(state);
            if (service.Handle.Position() != position)
            {
                Fail("the service beside member " + member + " starts at " + service.Handle.Position() + ", not at "
                    + position);
            }
            string again = CodeOf(() => service.Handle.Restore().Restored);
            if (again != tooLate)
            {
                Fail("a second restore beside member " + member + ": " + again);
            }
            restarted.Add(service);
        }

        // 7. A second request, beside the leader: each restarted service's first entry is the
        // request, at the first snapshot's position, and it gives its state.
        long second = PositionOf(Ask(binary, cluster, leader), leader);
        if (second <= position)
        {
            Fail("a second snapshot at " + second + ", not past the first at " + position);
        }
        WaitFor("every restarted service's poll ended at the second request",
            () => restarted.All(service => service.Handle.SnapshotDue() != null), restarted);
        foreach (Service service in restarted)
        {
            if (service.Entries.Count != 1 || service.Entries[0] != (Abi.CLUSTER_ENTRY_SNAPSHOT, position)
                || service.Handle.SnapshotDue() != second)
            {
                Fail("the service beside member " + service.Member + " that started from its snapshot at " + position
                    + " was handed " + string.Join(", ", service.Entries) + ", with a snapshot due at "
                    + service.Handle.SnapshotDue() + ": the second request is the one entry at " + position
                    + ", and asks for a snapshot at " + second);
            }
        }
        byte[] after = restarted[0].State();
        if (after.SequenceEqual(given) || restarted.Any(service => !service.State().SequenceEqual(after)))
        {
            Fail("the restarted services do not hold one new state at the second snapshot's position");
        }
        foreach (Service service in restarted)
        {
            string written = service.Give();
            if (written != "returned True")
            {
                Fail("the restarted service beside member " + service.Member + " gave its state: " + written);
            }
        }
        // Each member lists the second snapshot too: a start beside it is given the new
        // state, once the member has read the service's word of it.
        for (int member = 0; member < Members; member++)
        {
            Stopwatch waited = Stopwatch.StartNew();
            while (true)
            {
                var probe = new Service(cluster, member);
                (int Restored, byte[]? State) found = probe.Restore();
                long at = probe.Handle.Position();
                probe.Close();
                if (found.Restored == Abi.CLUSTER_RESTORED_STATE && found.State!.SequenceEqual(after) && at == second)
                {
                    break;
                }
                if (waited.Elapsed > Wait)
                {
                    Fail("member " + member + " does not list its service's second snapshot after 60 s: a new service "
                        + "starts with " + found.Restored + " at " + at);
                }
                Thread.Sleep(10);
            }
        }

        // 8. A service that never asks reads from position 0 and meets both requests, each
        // with the state the others held there: a start from a snapshot and a read from the
        // start reach one state. Its member lists a snapshot at each, or past it.
        var late = new Service(cluster, leader);
        foreach ((long at, byte[] held) in new[] { (position, given), (second, after) })
        {
            WaitFor("a service that reads from the start meets the request for " + at,
                () => late.Handle.SnapshotDue() != null, new[] { late });
            if (late.Handle.SnapshotDue() != at || !late.State().SequenceEqual(held))
            {
                Fail("a service that read from the start is due at " + late.Handle.SnapshotDue() + ", not at " + at
                    + ", or holds another state than the services held there");
            }
            string listed = late.Give();
            if (listed != "returned False")
            {
                Fail("a snapshot of position " + at + ", which the member already lists or is past: " + listed);
            }
        }

        foreach (Service service in restarted.Append(late))
        {
            service.Close();
        }
        Console.WriteLine("conformance/dotnet: snapshot round: three services gave a state of " + given.Length
            + " bytes at log position " + position + ", asked for by `cavalry snapshot` beside member " + leader
            + "; each member listed its service's snapshot, a new service beside each started from it, read the next "
            + "request at " + position + " and gave its state at " + second);
        Console.WriteLine("conformance/dotnet: snapshot round ok");
        return 0;
    }
}
