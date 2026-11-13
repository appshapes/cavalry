// The .NET client's committed log round (spec/c-abi.md, version 2; ADR 0070):
// Client.CommittedLog and CommittedLog beside a cluster member that
// conformance/dotnet/run.sh started with `cavalry run`, one of three on the loopback, as
// conformance/python/committed_log.py runs it through clients/python.
//
//     CommittedLog <the member's driver directory> <the member's id>
//
// A cluster started with `cavalry run` holds one entry, the first of its leader's term, 56
// bytes in a frame of 88 at position 0, so the log's next position is 96. The round opens
// twenty logs and closes each with no poll between, which leaves its client open (ADR
// 0078); reads that entry through the handler, with its position, its header and the
// handle's reads; makes each of the nine calls the ABI refuses from inside the log's own
// handler, each after it has set the thread's error to another code, so that a refusal
// that answers what a live log answers is told from an answer; makes a log's own image
// fault, by one byte of the frame its replay wrote, and reads the fault from the poll that
// meets it and from Faulted() for as long as the log is held (ADR 0080); checks where a
// handler's exception goes and that its entry is not delivered again; opens the log again
// at 96, where nothing is committed yet,
// beside a log at 0 that it reads the entry through meanwhile, so that the log at 96 has
// had the time a replay's image takes to join; closes twice; and closes a log after its
// client. Every wait is bounded at 30 s and sleeps a millisecond after a poll that
// delivered nothing. It exits 1 with what it found when a check fails. The .NET base
// library only (ADR 0003).

using System;
using System.Buffers.Binary;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Threading;
using Cavalry;

public static class CommittedLogRound
{
    const int WAIT_SECONDS = 30;
    const int IDLE_MILLISECONDS = 1; // after a poll that delivered nothing: three drivers share the host
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
        if (args.Length != 3)
        {
            Fail("usage: CommittedLog <the member's driver directory> <the member's id> <its recording's id>");
        }
        try
        {
            Round(args[0], int.Parse(args[1]), long.Parse(args[2]));
        }
        catch (CavalryException error)
        {
            Fail($"{error.Function} failed with error {error.Code}: {error.AbiMessage}");
        }
        catch (HandlerException error)
        {
            Fail($"the handler threw: {error.InnerException}");
        }
        catch (IOException error)
        {
            // The round reads and writes one file itself, the replay's log buffer.
            Fail($"a file of the member's driver directory could not be read or written: {error.Message}");
        }
        return 0;
    }

    // The round beside the member in memberDir, whose log is one recording that begins
    // at 0: recordingId, which conformance/log-member.sh read from the member's file of
    // its log's recordings.
    static void Round(string memberDir, int memberId, long recordingId)
    {
        var client = new Client(memberDir);
        // A closed log, for the refusals below: its Position() hands the ABI a null handle,
        // which answers -1 with ERR_NULL. The package's bindings of the ABI are internal, so
        // the round cannot ask for the position of a null header: this is its call that sets
        // the thread's error to another code. It is closed with no poll between, and so are
        // nineteen more: a log's replay's image may not have joined when it closes, and the
        // client's conductor then reads of an image whose log is gone. The client stays open
        // (ADR 0078; before it, card 241, this closed the client as corrupt in 8 of 60
        // tries), which the rest of the round shows.
        CommittedLog spent = OpenLog(client, 0);
        spent.Close();
        for (int i = 0; i < 19; i++)
        {
            OpenLog(client, 0).Close();
        }
        if (client.IsClosed())
        {
            Fail($"a log opened and closed with no poll between closed its client: {client.Closed()}");
        }
        Say("twenty logs opened and closed with no poll between left the client open");
        CommittedLog log = OpenLog(client, 0);
        if (log.MemberId != memberId || log.RecordingId != recordingId)
        {
            Fail($"the log is member {log.MemberId}'s recording {log.RecordingId}, not member {memberId}'s recording {recordingId}");
        }
        if (log.Position() != 0 || log.CommitPosition() != 0)
        {
            Fail($"a log opened at 0 began at {log.Position()} with bound {log.CommitPosition()}");
        }

        // The entry, with what each call on the log answers from inside its own handler.
        var entries = new List<string>();
        var refusals = new List<string>();
        bool inside = false;
        // Sets the thread's error to one no refusal gives, ERR_NULL, through the closed log's
        // Position(). A success clears nothing (spec/c-abi.md, "Error codes"), and a refused
        // IsEnded(), FailedAt() or WithdrawnAt() answers what a live log answers, false or
        // null, so the code alone tells that refusal from an answer: it must have changed
        // from this one to ERR_POLLING. Without this the code would be ERR_POLLING already,
        // left by the call before.
        Action setAnotherError = () =>
        {
            if (spent.Position() != -1 || Abi.Errcode() != Abi.ERR_NULL)
            {
                Fail($"a closed log's Position() did not answer -1 with ERR_NULL: error {Abi.Errcode()}");
            }
        };
        EntryHandler onEntry = null!;
        onEntry = (body, header, position) =>
        {
            entries.Add($"{position} {body.Length} {header.FrameLength} {header.Flags} {header.Type}");
            if (inside)
            {
                return;
            }
            inside = true;
            setAnotherError();
            refusals.Add($"Poll {log.Poll(onEntry, 1)} {Abi.Errcode()}");
            setAnotherError();
            refusals.Add($"Position {log.Position()} {Abi.Errcode()}");
            setAnotherError();
            refusals.Add($"CommitPosition {log.CommitPosition()} {Abi.Errcode()}");
            setAnotherError();
            refusals.Add($"IsEnded {log.IsEnded()} {Abi.Errcode()}");
            setAnotherError();
            refusals.Add($"FailedAt {log.FailedAt()?.ToString() ?? "null"} {Abi.Errcode()}");
            setAnotherError();
            refusals.Add($"WithdrawnAt {log.WithdrawnAt()?.ToString() ?? "null"} {Abi.Errcode()}");
            setAnotherError();
            refusals.Add($"Faulted {log.Faulted()?.ToString() ?? "null"} {Abi.Errcode()}");
            setAnotherError();
            try
            {
                log.GetFault();
                refusals.Add($"GetFault 0 {Abi.Errcode()}");
            }
            catch (CavalryException error)
            {
                refusals.Add($"GetFault -1 {error.Code}");
            }
            setAnotherError();
            try
            {
                log.Close();
                refusals.Add($"Close 0 {Abi.Errcode()}");
            }
            catch (CavalryException error)
            {
                refusals.Add($"Close -1 {error.Code}");
            }
        };
        long until = Deadline();
        while (entries.Count == 0)
        {
            int count = log.Poll(onEntry, 16);
            if (count < 0)
            {
                Fail($"the term's first entry: the poll stopped on fault {log.GetFault()}");
            }
            if (Stopwatch.GetTimestamp() > until)
            {
                Fail($"the term's first entry: not within {WAIT_SECONDS} s; position {log.Position()}, commit position " +
                     $"{log.CommitPosition()}, ended {log.IsEnded()}");
            }
            if (count == 0)
            {
                Thread.Sleep(IDLE_MILLISECONDS);
            }
        }
        string expectedEntry = $"0 {ENTRY_FRAME - 32} {ENTRY_FRAME} {Abi.FLAGS_UNFRAGMENTED} {Abi.FRAME_TYPE_DATA}";
        if (entries.Count != 1 || entries[0] != expectedEntry)
        {
            Fail($"expected one entry \"{expectedEntry}\" (position, bytes, frame length, flags, type), got " +
                 string.Join("; ", entries) + ". One cause of more than one entry is a new term: a cluster that " +
                 "elected again between its formation and this read holds that term's first entry after this one; " +
                 "the members' output, which run.sh prints, says whether it did");
        }
        int polling = Abi.ERR_POLLING;
        string[] expected =
        {
            $"Poll -1 {polling}", $"Position -1 {polling}", $"CommitPosition -1 {polling}", $"IsEnded False {polling}",
            $"FailedAt null {polling}", $"WithdrawnAt null {polling}", $"Faulted null {polling}",
            $"GetFault -1 {polling}",
            $"Close -1 {polling}",
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
        if (log.IsEnded() || log.FailedAt() != null || log.WithdrawnAt() != null || log.Faulted() != null ||
            fault.Code != 0 || fault.SessionId != 0 || fault.Position != Abi.NULL_POSITION)
        {
            Fail($"a live log read ended {log.IsEnded()}, failed at {log.FailedAt()}, withdrawn at " +
                 $"{log.WithdrawnAt()}, faulted {log.Faulted()}, fault {fault}");
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
            $"{log.CommitPosition()}; nine calls from inside the handler refused with ERR_POLLING, each made " +
            "with the thread's error at ERR_NULL");
        log.Close();
        log.Close(); // idempotent

        // A log whose own image faults (ADR 0080). The log's replay plays into a log buffer of
        // its own, the one file that appears under the member's `publications/` when the log is
        // opened. Before any poll of the log, the round changes one byte of the entry the
        // replay wrote there, its frame's version, to one no reader accepts (spec/frame.md,
        // "What a reader rejects"). The poll that reads the frame returns -1 with the fault,
        // once; GetFault() answers it until the next poll; and Faulted() answers it from that
        // poll on, the same code, session and position, whatever is polled after.
        string publications = Path.Combine(memberDir, "publications");
        var before = new HashSet<string>(Directory.GetFiles(publications));
        CommittedLog damaged = OpenLog(client, 0);
        string[] replayed = Directory.GetFiles(publications).Where(file => !before.Contains(file)).ToArray();
        if (replayed.Length != 1)
        {
            Fail($"opening a log made {string.Join(", ", replayed)} under {publications}, not the replay's one log buffer");
        }
        if (damaged.Faulted() != null)
        {
            Fail($"a log that has not polled reads a fault: {damaged.Faulted()}");
        }
        // The file is read and written at a position, through its handle and with no buffer
        // of this program's own. A FileStream keeps 4096 bytes it has read, and a Seek inside
        // them does not read the file again: a first read that came before the replay wrote
        // the frame would be read back, zeros, until the deadline, with the frame long there.
        using (var replayLog = File.OpenHandle(replayed[0], FileMode.Open, FileAccess.ReadWrite, FileShare.ReadWrite))
        {
            var frameLength = new byte[4];
            until = Deadline();
            while (true)
            {
                if (RandomAccess.Read(replayLog, frameLength, 0) == 4
                    && BinaryPrimitives.ReadInt32LittleEndian(frameLength) >= 32)
                {
                    break;
                }
                if (Stopwatch.GetTimestamp() > until)
                {
                    Fail($"the replay wrote no frame at position 0 of {replayed[0]} within {WAIT_SECONDS} s");
                }
                Thread.Sleep(IDLE_MILLISECONDS);
            }
            RandomAccess.Write(replayLog, new byte[] { 9 }, 4); // the frame's version byte
        }
        var delivered = new List<long>();
        until = Deadline();
        while (true)
        {
            int polled = damaged.Poll((body, header, position) => delivered.Add(position), 16);
            if (polled < 0)
            {
                break;
            }
            if (polled > 0 || delivered.Count > 0)
            {
                Fail($"a frame that fails its check was delivered: {string.Join(", ", delivered)}");
            }
            if (Stopwatch.GetTimestamp() > until)
            {
                Fail($"no poll met the damaged frame within {WAIT_SECONDS} s");
            }
            Thread.Sleep(IDLE_MILLISECONDS);
        }
        Subscription.Fault met = damaged.GetFault();
        if (met.Code != Abi.ERR_FAULT_IMAGE || met.Position != 0)
        {
            Fail($"the poll that met the damaged frame stopped on fault {met}, not ERR_FAULT_IMAGE at position 0");
        }
        for (int polls = 0; polls < 20; polls++)
        {
            Subscription.Fault? kept = damaged.Faulted();
            if (kept != met)
            {
                Fail($"after {polls} more polls Faulted() is {kept?.ToString() ?? "null"}, not the fault the poll met, {met}");
            }
            if (damaged.Poll((body, header, position) => delivered.Add(position), 16) != 0 || damaged.GetFault().Code != 0)
            {
                Fail($"a poll after the fault did not return 0 and clear GetFault(): {damaged.GetFault()}");
            }
        }
        if (!damaged.IsEnded() || damaged.FailedAt() != null || damaged.WithdrawnAt() != null || delivered.Count > 0)
        {
            Fail($"the faulted log read ended {damaged.IsEnded()}, failed at {damaged.FailedAt()}, withdrawn at " +
                 $"{damaged.WithdrawnAt()}, and delivered {string.Join(", ", delivered)}");
        }
        damaged.Close();
        if (client.IsClosed())
        {
            Fail($"a log whose image faulted closed its client: {client.Closed()}");
        }
        Say($"a log whose image faulted at position 0 read fault {met} from the poll that met it and from " +
            "Faulted() after twenty more polls");

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
            if (thrown == null)
            {
                Thread.Sleep(IDLE_MILLISECONDS);
            }
        }
        if (seen.Count != 1 || seen[0] != 0 || thrown!.InnerException is not InvalidOperationException ||
            thrown.Raised != 1 || throwing.Position() != NEXT)
        {
            Fail($"a throwing handler saw {string.Join(",", seen)}, threw {thrown}, and left the log at {throwing.Position()}");
        }
        // The exception is the log's own: it counts entries and names no subscription.
        if (thrown!.Fragments != 1 || !thrown.CommittedLog ||
            !thrown.Message.Contains("the entry handler threw on 1 of the 1 entry") || thrown.Message.Contains("Subscription"))
        {
            Fail($"a committed log's HandlerException has Fragments {thrown.Fragments}, CommittedLog {thrown.CommittedLog} " +
                 $"and says: {thrown.Message}");
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
        // A replay's image joins a moment after the open, so polls made at once would pass
        // before the image is there, whatever the open did with the position. So a second
        // log is opened at 0 beside this one, on the same client, and both are polled until
        // that one has delivered the entry at 0 and this one has taken the member's commit
        // position as its bound, which a poll does once the log's image has joined
        // (`CommittedLog::poll`, crates/cavalry-client/src/committed_log.rs): by then this
        // log's replay, asked for first, has had the time the other's took to join and
        // deliver.
        CommittedLog rest = OpenLog(client, NEXT);
        if (rest.Position() != NEXT)
        {
            Fail($"a log opened at {NEXT} began at {rest.Position()}");
        }
        CommittedLog beside = OpenLog(client, 0);
        var atNext = new List<long>();
        var atStart = new List<long>();
        EntryHandler onNext = (body, header, position) => atNext.Add(position);
        EntryHandler onStart = (body, header, position) => atStart.Add(position);
        until = Deadline();
        while (atStart.Count == 0 || rest.CommitPosition() < NEXT)
        {
            int restCount = rest.Poll(onNext, 16);
            int besideCount = beside.Poll(onStart, 16);
            if (restCount < 0 || besideCount < 0)
            {
                Fail($"the polls of the logs at {NEXT} and at 0 returned {restCount} and {besideCount}: faults " +
                     $"{rest.GetFault()} and {beside.GetFault()}");
            }
            if (Stopwatch.GetTimestamp() > until)
            {
                Fail($"the log at 0 beside the log at {NEXT} delivered {string.Join(",", atStart)} and the log at {NEXT} " +
                     $"took the bound {rest.CommitPosition()}, not the entry at 0 and {NEXT}, within {WAIT_SECONDS} s");
            }
            if (restCount == 0 && besideCount == 0)
            {
                Thread.Sleep(IDLE_MILLISECONDS);
            }
        }
        for (int i = 0; i < 50; i++)
        {
            if (rest.Poll(onNext, 16) < 0)
            {
                Fail($"the poll of the log at {NEXT} stopped on fault {rest.GetFault()}");
            }
        }
        if (atStart[0] != 0 || atNext.Count != 0 || rest.Position() != NEXT)
        {
            Fail($"a log opened at {NEXT} delivered the entries at {string.Join(",", atNext)} and is at {rest.Position()}, " +
                 $"while a log opened at 0 beside it delivered the entries at {string.Join(",", atStart)}. An entry below " +
                 $"{NEXT} is one the open did not start past; one at {NEXT} or above is a new term's, of a cluster that " +
                 "elected again, which the members' output, printed by run.sh, shows");
        }
        beside.Close();
        // A log that outlives its client: its poll stops on the closed fault, and its close
        // frees it with no client to stop the replay through.
        client.Close();
        if (rest.Poll(onEntry, 16) != -1 || rest.GetFault().Code != Abi.ERR_FAULT_CLOSED)
        {
            Fail($"a poll after the client's close did not stop on the closed fault: {rest.GetFault()}");
        }
        rest.Close();
        Say($"a throwing handler's entry is not delivered again; nothing at position {NEXT} while a log at 0 beside " +
            "it read the entry at 0; a log closed after its client");
    }
}
