// The .NET client's closed round (spec/c-abi.md, "cluster service"; ADR 0155): a
// ClusterService whose driver stops. A poll with a client its driver closed is one answer,
// -1 with ERR_CLOSED as the thread's error and as GetFault()'s code, at the first poll that
// fails and at every poll after it, with the client's reason CLOSED_DRIVER_CLOSED; and the
// service's close is then no error. The driver hosts no member, so the service holds no log
// and its poll has no command to send: the case in which a poll of a closed client took
// nothing and returned 0 for as long as it was called. conformance/dotnet/run.sh runs it
// last, against the driver its Quickstart ran on.
//
//     Closed <the driver's process id>
//
// The client attaches to the driver at CAVALRY_DIR, and the program stops that driver
// itself, with SIGTERM to the process id it was given, sent by the system's /bin/kill,
// since .NET's own Process.Kill sends a signal the driver cannot answer. A failed check prints
// what failed and exits 1; the wait is bounded at 60 s. The .NET base library only (ADR
// 0003).

using System;
using System.Diagnostics;
using System.Text;
using System.Threading;
using Cavalry;

static class ClosedRound
{
    const int WAIT_SECONDS = 60;
    const int POLLS_AFTER = 3;
    static int handed;

    static void Fail(string what)
    {
        Console.Error.WriteLine($"conformance/dotnet: closed round: {what}");
        Environment.Exit(1);
    }

    static int OnEntry(ReadOnlySpan<byte> body, ClusterEntryValues entry, ClusterService service)
    {
        handed++;
        return Abi.ACTION_CONTINUE;
    }

    /// <summary>What the poll that just returned -1 stopped on: ERR_CLOSED as the thread's
    /// error, read before any other call, and as the fault's code.</summary>
    static void Closed(string what, int errcode, ClusterService service)
    {
        int code = service.GetFault().Code;
        if (code != Abi.ERR_CLOSED || errcode != Abi.ERR_CLOSED)
        {
            Fail($"{what} stopped on code {code}, with the thread's error {errcode}; expected ERR_CLOSED, "
                + $"{Abi.ERR_CLOSED}, as both");
        }
    }

    static int Main(string[] args)
    {
        if (args.Length != 1 || !int.TryParse(args[0], out int pid) || pid <= 1)
        {
            Fail("usage: Closed <the driver's process id>");
            return 1;
        }
        try
        {
            Round(pid);
        }
        catch (CavalryException error)
        {
            Fail($"{error.Function} failed with error {error.Code}: {error.AbiMessage}");
        }
        return 0;
    }

    static void Round(int pid)
    {
        Client client = new();
        ClusterService service = client.ClusterService();

        // 1. Beside a driver that runs and hosts no member, a poll takes nothing.
        int taken = service.Poll(OnEntry, 10);
        if (taken != 0 || client.IsClosed())
        {
            Fail($"a poll beside a driver with no member returned {taken}, and the client reads closed {client.Closed()}; "
                + "expected 0 beside a driver that runs");
        }

        // 2. The driver stops. Until the client has seen it, a poll takes nothing; the
        // first poll that fails is the close, and nothing else.
        using (Process? kill = Process.Start(new ProcessStartInfo("/bin/kill") { ArgumentList = { "-TERM", pid.ToString() } }))
        {
            if (kill == null || !kill.WaitForExit(10_000) || kill.ExitCode != 0)
            {
                Fail($"the driver, process {pid}, could not be asked to stop: `/bin/kill -TERM {pid}` did not exit 0");
            }
        }
        long until = Stopwatch.GetTimestamp() + Stopwatch.Frequency * WAIT_SECONDS;
        int polls = 0;
        int errcode;
        while (true)
        {
            taken = service.Poll(OnEntry, 10);
            errcode = Abi.Errcode();
            polls++;
            if (taken == -1)
            {
                break;
            }
            if (taken != 0)
            {
                Fail($"a poll beside a driver with no member returned {taken}; expected 0, or -1 after the stop");
            }
            if (Stopwatch.GetTimestamp() > until)
            {
                Fail($"no poll failed within {WAIT_SECONDS} s of the driver's stop; the client reads closed {client.Closed()}");
            }
            Thread.Sleep(1);
        }
        Closed("the first poll that failed after the driver's stop", errcode, service);
        if (!client.IsClosed() || client.Closed() != Abi.CLOSED_DRIVER_CLOSED)
        {
            Fail($"the poll said closed, and the client reads closed {client.Closed()}; expected CLOSED_DRIVER_CLOSED, "
                + $"{Abi.CLOSED_DRIVER_CLOSED}");
        }

        // 3. Every poll after it is the same answer: a loop that looks at what its poll
        // returned ends. The thread's error is set to another code before each, by a reply
        // from outside the handler: a success clears nothing, so without this the code read
        // after the poll could be the first failed poll's.
        for (int after = 1; after <= POLLS_AFTER; after++)
        {
            if (service.Reply(1, Encoding.Latin1.GetBytes("x")) != -1 || Abi.Errcode() != Abi.ERR_CLUSTER_NO_ENTRY)
            {
                Fail($"a reply from outside the handler was not refused with ERR_CLUSTER_NO_ENTRY: error {Abi.Errcode()}");
            }
            taken = service.Poll(OnEntry, 10);
            errcode = Abi.Errcode();
            if (taken != -1)
            {
                Fail($"poll {after} after the first that failed returned {taken}; expected -1");
            }
            Closed($"poll {after} after the first that failed", errcode, service);
        }
        if (handed != 0)
        {
            Fail($"the handler was called {handed} times beside a driver with no member");
        }

        // 4. The service closes with no error, and its client after it: a driver that is
        // gone cannot be told.
        service.Close();
        client.Close();
        Console.WriteLine("conformance/dotnet: closed round: a service beside a driver with no member took nothing; after "
            + $"the driver's stop, poll {polls} and the {POLLS_AFTER} after it returned -1 with ERR_CLOSED, the client's "
            + "reason is CLOSED_DRIVER_CLOSED, and the service closed with no error");
        Console.WriteLine("conformance/dotnet: closed round ok");
    }
}
