// The .NET client's committed log round (spec/c-abi.md, version 2; ADR 0070):
// Client.CommittedLog and CommittedLog beside a cluster member that
// conformance/dotnet/run.sh started with `cavalry run`, one of three on the loopback, as
// conformance/python/committed_log.py runs it through clients/python.
//
//     CommittedLog members
//     CommittedLog <the member's driver directory> <the member's id>
//
// `members` prints a members string for three members with five free loopback UDP ports
// each, for run.sh to start the cluster with. The round: a cluster started with
// `cavalry run` holds one entry, the first of its leader's term, 56 bytes in a frame of 88
// at position 0, so the log's next position is 96. It reads that entry through the
// handler, with its position, its header and the handle's reads; makes each call the ABI
// refuses from inside the log's own handler; checks where a handler's exception goes and
// that its entry is not delivered again; opens the log again at 96, where nothing is
// committed yet; closes twice; and closes a log after its client. Every wait is bounded at
// 30 s. It exits 1 with what it found when a check fails. The .NET base library only
// (ADR 0003).

using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Net;
using System.Net.Sockets;
using System.Threading;
using Cavalry;

public static class CommittedLogRound
{
    const int WAIT_SECONDS = 30;
    const int ENTRY_FRAME = 88; // the term's first entry: 32 bytes of header and 56 of message
    const long NEXT = 96; // the frame, rounded up to a multiple of 32

    static void Fail(string what)
    {
        Console.Error.WriteLine("conformance/dotnet: committed log: " + what);
        Environment.Exit(1);
    }

    static void Say(string what)
    {
        Console.WriteLine("conformance/dotnet: committed log: " + what);
    }

    static long Deadline()
    {
        return Stopwatch.GetTimestamp() + Stopwatch.Frequency * WAIT_SECONDS;
    }

    /// <summary>A members string for three members, each with five free loopback UDP ports,
    /// the fifteen sockets held until every port is chosen so that none repeats.</summary>
    static string Members()
    {
        var sockets = new List<Socket>();
        var ports = new List<int>();
        for (int i = 0; i < 15; i++)
        {
            var socket = new Socket(AddressFamily.InterNetwork, SocketType.Dgram, ProtocolType.Udp);
            socket.Bind(new IPEndPoint(IPAddress.Loopback, 0));
            sockets.Add(socket);
            ports.Add(((IPEndPoint) socket.LocalEndPoint!).Port);
        }
        foreach (Socket socket in sockets)
        {
            socket.Dispose();
        }
        var members = new List<string>();
        for (int member = 0; member < 3; member++)
        {
            var endpoints = new List<string>();
            for (int i = 0; i < 5; i++)
            {
                endpoints.Add("127.0.0.1:" + ports[member * 5 + i]);
            }
            members.Add(member + "," + string.Join(",", endpoints));
        }
        return string.Join("|", members);
    }

    /// <summary>The member's committed log at the position, asked for until the member has one.</summary>
    static CommittedLog OpenLog(Client client, long position)
    {
        long until = Deadline();
        while (true)
        {
            CommittedLog? log = client.CommittedLog(position);
            if (log != null)
            {
                return log;
            }
            if (Stopwatch.GetTimestamp() > until)
            {
                Fail($"the driver at {client.Dir} hosted no member with a log within {WAIT_SECONDS} s");
            }
            Thread.Sleep(50);
        }
    }

    public static int Main(string[] args)
    {
        if (args.Length == 1 && args[0] == "members")
        {
            Console.WriteLine(Members());
            return 0;
        }
        if (args.Length != 2)
        {
            Fail("usage: CommittedLog members | CommittedLog <the member's driver directory> <the member's id>");
        }
        try
        {
            Round(args[0], int.Parse(args[1]));
        }
        catch (CavalryException error)
        {
            Fail($"{error.Function} failed with error {error.Code}: {error.AbiMessage}");
        }
        catch (HandlerException error)
        {
            Fail($"the handler threw: {error.InnerException}");
        }
        return 0;
    }

    static void Round(string memberDir, int memberId)
    {
        var client = new Client(memberDir);
        CommittedLog log = OpenLog(client, 0);
        if (log.MemberId != memberId || log.RecordingId != 0)
        {
            Fail($"the log is member {log.MemberId}'s recording {log.RecordingId}, not member {memberId}'s recording 0");
        }
        if (log.Position() != 0 || log.CommitPosition() != 0)
        {
            Fail($"a log opened at 0 began at {log.Position()} with bound {log.CommitPosition()}");
        }

        // The entry, with what each call on the log answers from inside its own handler.
        var entries = new List<string>();
        var refusals = new List<string>();
        EntryHandler onEntry = null!;
        onEntry = (body, header, position) =>
        {
            entries.Add($"{position} {body.Length} {header.FrameLength} {header.Flags} {header.Type}");
            if (refusals.Count > 0)
            {
                return;
            }
            refusals.Add($"Poll {log.Poll(onEntry, 1)} {Abi.Errcode()}");
            refusals.Add($"Position {log.Position()} {Abi.Errcode()}");
            refusals.Add($"CommitPosition {log.CommitPosition()} {Abi.Errcode()}");
            refusals.Add($"IsEnded {log.IsEnded()} {Abi.Errcode()}");
            refusals.Add($"FailedAt {log.FailedAt()?.ToString() ?? "null"} {Abi.Errcode()}");
            refusals.Add($"WithdrawnAt {log.WithdrawnAt()?.ToString() ?? "null"} {Abi.Errcode()}");
            try
            {
                log.Close();
                refusals.Add("Close 0 0");
            }
            catch (CavalryException error)
            {
                refusals.Add($"Close -1 {error.Code}");
            }
        };
        long until = Deadline();
        while (entries.Count == 0)
        {
            if (log.Poll(onEntry, 16) < 0)
            {
                Fail($"the term's first entry: the poll stopped on fault {log.GetFault()}");
            }
            if (Stopwatch.GetTimestamp() > until)
            {
                Fail($"the term's first entry: not within {WAIT_SECONDS} s; position {log.Position()}, commit position " +
                     $"{log.CommitPosition()}, ended {log.IsEnded()}");
            }
        }
        string expectedEntry = $"0 {ENTRY_FRAME - 32} {ENTRY_FRAME} {Abi.FLAGS_UNFRAGMENTED} {Abi.FRAME_TYPE_DATA}";
        if (entries.Count != 1 || entries[0] != expectedEntry)
        {
            Fail($"expected one entry \"{expectedEntry}\" (position, bytes, frame length, flags, type), got " +
                 string.Join("; ", entries));
        }
        int polling = Abi.ERR_POLLING;
        string[] expected =
        {
            $"Poll -1 {polling}", $"Position -1 {polling}", $"CommitPosition -1 {polling}", $"IsEnded False {polling}",
            $"FailedAt null {polling}", $"WithdrawnAt null {polling}", $"Close -1 {polling}",
        };
        if (string.Join("; ", refusals) != string.Join("; ", expected))
        {
            Fail($"the calls from inside the log's own handler answered {string.Join("; ", refusals)}, not " +
                 string.Join("; ", expected));
        }
        if (log.Position() != NEXT || log.CommitPosition() != NEXT)
        {
            Fail($"after the entry the log is at {log.Position()} with bound {log.CommitPosition()}, not {NEXT} and {NEXT}");
        }
        Subscription.Fault fault = log.GetFault();
        if (log.IsEnded() || log.FailedAt() != null || log.WithdrawnAt() != null || fault.Code != 0 ||
            fault.SessionId != 0 || fault.Position != Abi.NULL_POSITION)
        {
            Fail($"a live log read ended {log.IsEnded()}, failed at {log.FailedAt()}, withdrawn at " +
                 $"{log.WithdrawnAt()}, fault {fault}");
        }
        for (int i = 0; i < 50; i++)
        {
            if (log.Poll(onEntry, 16) != 0)
            {
                Fail("an entry arrived with nothing more committed");
            }
        }
        if (entries.Count != 1)
        {
            Fail("the refused poll, or a later one, delivered more: " + string.Join("; ", entries));
        }
        Say($"member {log.MemberId}'s entry at position 0 read once, next position {log.Position()}, commit position " +
            $"{log.CommitPosition()}; seven calls from inside the handler refused with ERR_POLLING");
        log.Close();
        log.Close(); // idempotent

        // A handler that throws: the entry was delivered, the exception is thrown after the
        // ABI has returned, and the log's position is past the entry, so it is not delivered
        // again.
        CommittedLog throwing = OpenLog(client, 0);
        var seen = new List<long>();
        EntryHandler throws = (body, header, position) =>
        {
            seen.Add(position);
            throw new InvalidOperationException("the handler's own");
        };
        HandlerException? thrown = null;
        until = Deadline();
        while (thrown == null)
        {
            try
            {
                if (throwing.Poll(throws, 16) < 0)
                {
                    Fail($"the second log's poll stopped on fault {throwing.GetFault()}");
                }
            }
            catch (HandlerException error)
            {
                thrown = error;
            }
            if (thrown == null && Stopwatch.GetTimestamp() > until)
            {
                Fail("the second log read no entry");
            }
        }
        if (seen.Count != 1 || seen[0] != 0 || thrown!.InnerException is not InvalidOperationException ||
            thrown.Raised != 1 || throwing.Position() != NEXT)
        {
            Fail($"a throwing handler saw {string.Join(",", seen)}, threw {thrown}, and left the log at {throwing.Position()}");
        }
        for (int i = 0; i < 50; i++)
        {
            if (throwing.Poll(throws, 16) != 0)
            {
                Fail("the entry whose handler threw was delivered again");
            }
        }
        throwing.Close();

        // From the next position nothing is committed yet: no entry, and no entry before it.
        CommittedLog rest = OpenLog(client, NEXT);
        if (rest.Position() != NEXT)
        {
            Fail($"a log opened at {NEXT} began at {rest.Position()}");
        }
        for (int i = 0; i < 100; i++)
        {
            if (rest.Poll(onEntry, 16) != 0)
            {
                Fail($"a log opened at {NEXT} delivered an entry: " + string.Join("; ", entries));
            }
        }
        // A log that outlives its client: its poll stops on the closed fault, and its close
        // frees it with no client to stop the replay through.
        client.Close();
        if (rest.Poll(onEntry, 16) != -1 || rest.GetFault().Code != Abi.ERR_FAULT_CLOSED)
        {
            Fail($"a poll after the client's close did not stop on the closed fault: {rest.GetFault()}");
        }
        rest.Close();
        Say($"a throwing handler's entry is not delivered again; nothing at position {NEXT}; a log closed after its client");
    }
}
