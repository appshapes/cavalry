// The .NET client's cluster round (spec/c-abi.md, versions 4, 5 and 7; ADR 0122, ADR 0123,
// ADR 0124, ADR 0157): Client.ClusterService and ClusterService beside each of three cluster members
// that conformance/dotnet/run.sh started with `cavalry run`, and Client.ConnectCluster and
// ClusterSession on the runner's first driver, the one at CAVALRY_DIR, as
// conformance/python/cluster.py runs it through clients/python.
//
//     Cluster <cluster directory> <members string> <response channel>
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
// The client's own edges: what Offer refuses; the calls the ABI refuses from inside a
// service's handler, on the service and on the client its poll holds, the client's own
// close among them, which must leave the client open, and from inside the
// session's, each made after the thread's error was set to another code, so that a refusal
// that answers what an open handle answers is told from an answer; the one object of values
// a handler is handed, and the empty body of an entry that is no session's message; a
// service's handler that throws after it replied, and one that returns what is no action,
// whose entry is aborted, handed over again by the next poll and answered once; a reply too
// long for a frame and a reply to a session that is not open; a handler that returns
// ACTION_BREAK with two entries waiting; a session's handler that throws, whose reply is
// not handed over again; the program's timers, ScheduleTimer and CancelTimer from inside the
// handler, as the C runner's round has one: a timer handed to every service once, as an
// entry of its own type with the timer's id, at one position and no earlier than the
// deadline its second schedule set, a second after its entry, where its first was an hour
// away, and not again within a timer refresh interval and more; a timer due at once and
// cancelled in the same handling handed to none; the least and the greatest long as ids,
// handed back as they are; both calls refused from outside the handler; the session's
// close as an entry every service is handed; a
// session and a service closed twice, and a service closed after its client.
//
// Then a report, printed and never gated: how many polls the three services made, how many
// took over a millisecond, and the longest, which is how long a poll held its thread
// (clients/dotnet/README.md, "The call").
//
// Every wait is bounded at 60 s, the time a cluster may take to commit on a busy host. It
// exits 1 with what it found when a check fails. The .NET base library only (ADR 0003).

using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Text;
using System.Threading;
using Cavalry;

public static class ClusterRound
{
    const int MEMBERS = 3;
    const int WAIT_SECONDS = 60;
    // Between duty cycles, since the members run in processes of their own: the shortest
    // sleep Thread.Sleep makes, which counts in milliseconds and yields alone for less.
    const int IDLE_MILLISECONDS = 1;
    const int SETTLE_MILLISECONDS = 300; // the time in which a second reply, or a second entry, would come
    const string TIMER_MESSAGE = "timer"; // the message at which each service's program schedules its timers
    const long TIMER_ID = 77; // the timer that is handed over
    const long CANCELLED_ID = 78; // the timer that is scheduled and cancelled in the same handling
    // How long after the message's entry the timer is due, in the cluster's milliseconds. A
    // second: a service tells its member of a timer only once the poll that took the entry
    // has sent the six commands of its first timer, some 25 ms on a laptop and more on a
    // busy host, so the event of a timer that was wrongly due at once is stamped tens of
    // milliseconds after the message, 38 to 68 ms in the five clients' rounds as they were
    // run on a laptop, this one's the latest, and a margin of that size would let it pass.
    // The round prints what it was in its own run.
    const long TIMER_AFTER_MS = 1000;
    const long HOUR_MS = 3_600_000; // the deadline its first schedule gives it, which the second replaces
    const int REFRESH_MILLISECONDS = 1200; // a timer refresh interval of spec/cluster.md, "Values", 1 s, and more

    static void Fail(string what)
    {
        Console.Error.WriteLine("conformance/dotnet: cluster round: " + what);
        Environment.Exit(1);
    }

    static void Say(string what)
    {
        Console.WriteLine("conformance/dotnet: cluster round: " + what);
    }

    static long Deadline()
    {
        return Stopwatch.GetTimestamp() + Stopwatch.Frequency * WAIT_SECONDS;
    }

    static byte[] Bytes(string text)
    {
        return Encoding.Latin1.GetBytes(text);
    }

    static string Show<T>(IEnumerable<T>? items)
    {
        return items == null ? "nothing" : "[" + string.Join(", ", items) + "]";
    }

    /// <summary>One committed entry, as its handler read it.</summary>
    record struct Entry(int Type, long Position, long Term, long Timestamp, long Session, int Leader, bool TimedOut)
    {
        public static Entry Of(ClusterEntryValues entry)
        {
            return new Entry(entry.Type, entry.Position, entry.LeadershipTermId, entry.Timestamp, entry.SessionId,
                entry.LeaderMemberId, entry.TimedOut);
        }
    }

    /// <summary>A timer that was handed over: where, its id and the event's time.</summary>
    record struct Timer(long Position, long Id, long Timestamp);

    /// <summary>What one call answered, with the thread's error after it: a call that throws
    /// answers -1 with the exception's code.</summary>
    record struct Refusal(string Call, long Answer, int Code);

    record struct Answer(string Payload, long Term, long Timestamp);

    /// <summary>A service that was closed before the round began: its Position() hands the
    /// ABI a null handle, which answers -1 with ERR_NULL. The library's bindings of the ABI
    /// are not public, so this is the round's call that sets the thread's error to a code no
    /// refusal gives. A success clears nothing (spec/c-abi.md, "Error codes"), so a refusal
    /// that answers what an open handle answers is told from an answer by the code alone,
    /// which must have changed from this one to ERR_POLLING.</summary>
    static ClusterService? spent;

    static void SetAnotherError()
    {
        if (spent!.Position() != -1 || Abi.Errcode() != Abi.ERR_NULL)
        {
            Fail($"a closed service's Position() did not answer -1 with ERR_NULL: error {Abi.Errcode()}");
        }
    }

    /// <summary>What a call that answers a number answered, with the thread's error after
    /// it.</summary>
    static Refusal Asked(string call, Func<long> made)
    {
        SetAnotherError();
        return new Refusal(call, made(), Abi.Errcode());
    }

    /// <summary>What a call that throws for a refusal did: 0 with the thread's error when it
    /// returned, and -1 with the exception's code when it threw.</summary>
    static Refusal Thrown(string call, Action made)
    {
        SetAnotherError();
        try
        {
            made();
            return new Refusal(call, 0, Abi.Errcode());
        }
        catch (CavalryException refused)
        {
            return new Refusal(call, -1, refused.Code);
        }
    }

    /// <summary>A service beside one member: what its handler was handed and what its calls
    /// answered. The handler answers a session's message with its member's id, a colon and
    /// the message, so that the reply a client reads says whose service sent it.</summary>
    sealed class Served
    {
        public readonly int Member;
        public readonly Client Client;
        public readonly ClusterService Service;
        public readonly List<Entry> Entries = new(); // every entry taken
        public readonly Dictionary<string, List<Entry>> Messages = new(); // a payload -> the entries it was taken as
        public readonly Dictionary<string, List<long>> Handed = new(); // a payload -> where it was handed over, taken or not
        public readonly Dictionary<string, List<int>> Codes = new(); // a payload -> what each reply to it answered
        public List<Refusal>? Refusals; // what the calls from inside the handler answered
        public string? ThrowOn; // a payload whose first handling replies and then throws
        public string? NoActionOn; // a payload whose first handling returns what is no action
        public string? EdgesOn; // a payload before whose reply a reply too long and one to no session are made
        public List<int>? Edges;
        public readonly HashSet<string> BreakOn = new(); // payloads whose entries are taken with ACTION_BREAK
        public readonly List<Timer> Timers = new(); // every timer handed over
        public long TimerAt; // when the last was handed over, by Stopwatch.GetTimestamp
        public List<int> TimerCodes = new(); // what the schedules and the cancel at TIMER_MESSAGE answered
        public ClusterEntryValues? Values; // the object of values the handler was handed, the same one for every entry
        public int Polls; // the report: every poll, those over a millisecond, and the longest
        public int Over;
        public long Longest;
        public string LongestOf = "no poll";

        public Served(string cluster, int member)
        {
            Member = member;
            Client = new Client(Path.Combine(cluster, "member-" + member));
            Service = Client.ClusterService();
        }

        public List<Entry> Taken(string payload)
        {
            return Messages.TryGetValue(payload, out List<Entry>? taken) ? taken : new List<Entry>();
        }

        static List<T> At<T>(Dictionary<string, List<T>> map, string payload)
        {
            if (!map.TryGetValue(payload, out List<T>? list))
            {
                list = new List<T>();
                map[payload] = list;
            }
            return list;
        }

        public int OnEntry(ReadOnlySpan<byte> body, ClusterEntryValues entry, ClusterService handedService)
        {
            if (!ReferenceEquals(handedService, Service))
            {
                Fail("the handler was handed another service than the one that was polled");
            }
            Values ??= entry;
            if (!ReferenceEquals(entry, Values))
            {
                Fail("the handler was handed another object of values than the one the service keeps");
            }
            Entry read = Entry.Of(entry);
            if (entry.Type != Abi.CLUSTER_ENTRY_SESSION_MESSAGE || entry.SessionId < 0)
            {
                if (body.Length != 0)
                {
                    Fail($"an entry that is no session's message has a payload of {body.Length} bytes: {read}");
                }
                if (entry.Type == Abi.CLUSTER_ENTRY_TIMER)
                {
                    Timers.Add(new Timer(entry.Position, entry.CorrelationId, entry.Timestamp));
                    TimerAt = Stopwatch.GetTimestamp();
                }
                else if (entry.CorrelationId != 0)
                {
                    Fail($"an entry that is no timer has the correlation id {entry.CorrelationId}: {read}");
                }
                Entries.Add(read);
                return Abi.ACTION_CONTINUE;
            }
            if (entry.CorrelationId != 0)
            {
                Fail($"a session's message has the correlation id {entry.CorrelationId}: {read}");
            }
            string payload = Encoding.Latin1.GetString(body);
            List<long> positions = At(Handed, payload);
            positions.Add(entry.Position);
            bool first = positions.Count == 1;
            if (payload == NoActionOn && first)
            {
                return 3; // Aeron's COMMIT, which Cavalry has no action for
            }
            ClusterService service = Service;
            if (payload == EdgesOn && Edges == null)
            {
                int tooLong = service.Reply(entry.SessionId, new byte[1345]);
                Edges = new List<int> { tooLong, Abi.Errcode(), service.Reply(entry.SessionId + 1000, Bytes("x")) };
            }
            // Reply first, then change the state: an entry that is aborted is handed over again.
            int code = service.Reply(entry.SessionId, Bytes(Member + ":" + payload));
            if (code < 0)
            {
                Fail($"a reply from inside the handler answered {code} with error {Abi.Errcode()}");
            }
            if (code == Abi.CLUSTER_REPLY_RETRY)
            {
                positions.RemoveAt(positions.Count - 1);
                return Abi.ACTION_ABORT;
            }
            if (payload == ThrowOn && first)
            {
                At(Codes, payload).Add(code);
                throw new InvalidOperationException("the handler's own");
            }
            if (Refusals == null)
            {
                Client client = Client;
                Refusals = new List<Refusal>
                {
                    Asked("Poll", () => service.Poll(OnEntry, 1)),
                    Asked("Position", () => service.Position()),
                    Asked("Answers", () => service.Answers() ? 1 : 0),
                    Asked("Sessions", () => service.Sessions()),
                    Thrown("GetFault", () => service.GetFault()),
                    Thrown("Close", () => service.Close()),
                    Asked("Client.IsClosed", () => client.IsClosed() ? 1 : 0),
                    Asked("Client.Closed", () => client.Closed()),
                    Thrown("Client.ClusterService", () => client.ClusterService()),
                    Thrown("Client.AddSubscription", () => client.AddSubscription("cavalry:ipc", 1229)),
                    // The client's own close, last: the ABI refuses it and frees nothing
                    // (spec/c-abi.md, "Calls during a claim or a poll"), so the client must
                    // stay open and still reach the ABI after it: IsClosed is refused again,
                    // false with ERR_POLLING. Were the handle forgotten, IsClosed would answer
                    // true with no call, the thread's error would stay the other code, and the
                    // poll after this one would hand the ABI a null client.
                    Thrown("Client.Close", () => client.Close()),
                    Asked("Client.IsClosed after its Close", () => client.IsClosed() ? 1 : 0),
                };
            }
            if (payload == TIMER_MESSAGE)
            {
                // The program's state changes at the entry it takes. One timer, scheduled
                // for an hour later and then again for a second later: were the second
                // schedule not to set the one timer's deadline, the round's wait would end
                // before the timer came. A second timer, due at once, cancelled again: were
                // the cancel to miss, it would be handed over. And the least and the
                // greatest long as ids, due at once, which each service must be handed back
                // as they are. Every member's service makes the same calls at the same
                // entry.
                TimerCodes = new List<int>
                {
                    service.ScheduleTimer(TIMER_ID, entry.Timestamp + HOUR_MS),
                    service.ScheduleTimer(TIMER_ID, entry.Timestamp + TIMER_AFTER_MS),
                    service.ScheduleTimer(CANCELLED_ID, entry.Timestamp),
                    service.CancelTimer(CANCELLED_ID),
                    service.ScheduleTimer(long.MinValue, entry.Timestamp),
                    service.ScheduleTimer(long.MaxValue, entry.Timestamp),
                };
            }
            At(Codes, payload).Add(code);
            At(Messages, payload).Add(read);
            Entries.Add(read);
            return BreakOn.Contains(payload) ? Abi.ACTION_BREAK : Abi.ACTION_CONTINUE;
        }

        /// <summary>One poll, timed: the count of entries taken. A handler's failure is the
        /// caller's to catch.</summary>
        public int Poll()
        {
            int before = Entries.Count;
            long started = Stopwatch.GetTimestamp();
            int count;
            try
            {
                count = Service.Poll(OnEntry, 16);
            }
            finally
            {
                long took = (long) Stopwatch.GetElapsedTime(started).TotalMicroseconds;
                Polls++;
                if (took > 1000)
                {
                    Over++;
                }
                if (took > Longest)
                {
                    Longest = took;
                    LongestOf = $"a poll of the service beside member {Member} that began after {before} entries and took "
                        + (Entries.Count - before);
                }
            }
            if (count < 0)
            {
                Fail($"the poll of the service beside member {Member} stopped on fault {Service.GetFault()}");
            }
            return count;
        }
    }

    /// <summary>What the session's handler was handed.</summary>
    sealed class Answered
    {
        public readonly ClusterSession Session;
        public readonly List<Answer> Replies = new();
        public List<Refusal>? Refusals;
        public string? ThrowOn; // a reply on which the handler throws
        public ClusterReplyValues? Values; // the object of values the handler was handed, the same one for every reply

        public Answered(ClusterSession session)
        {
            Session = session;
        }

        public void OnReply(ReadOnlySpan<byte> body, ClusterReplyValues reply)
        {
            Values ??= reply;
            if (!ReferenceEquals(reply, Values))
            {
                Fail("the handler was handed another object of values than the one the session keeps");
            }
            string payload = Encoding.Latin1.GetString(body);
            Replies.Add(new Answer(payload, reply.LeadershipTermId, reply.Timestamp));
            if (Refusals == null)
            {
                ClusterSession session = Session;
                Refusals = new List<Refusal>
                {
                    Asked("Offer", () => session.Offer(Bytes("x"))),
                    Asked("Poll", () => session.Poll(OnReply, 1)),
                    Asked("LeadershipTermId", () => session.LeadershipTermId()),
                    Asked("LeaderMemberId", () => session.LeaderMemberId()),
                    Asked("StaleReplies", () => session.StaleReplies()),
                    Asked("Closed", () => session.Closed() == null ? 0 : 1),
                    Thrown("GetFault", () => session.GetFault()),
                    Thrown("Close", () => session.Close()),
                };
            }
            if (ThrowOn != null && payload.EndsWith(ThrowOn, StringComparison.Ordinal))
            {
                throw new InvalidOperationException("the reply handler's own");
            }
        }

        /// <summary>The replies to the message, whichever member's service sent them.</summary>
        public List<Answer> To(string message)
        {
            return Replies.Where(reply => reply.Payload.Substring(2) == message).ToList();
        }
    }

    /// <summary>One duty cycle: every service's poll but <paramref name="skip"/>'s, the
    /// session's when there is one, and a short sleep.</summary>
    static void Cycle(List<Served> served, Answered? answered, Served? skip)
    {
        foreach (Served s in served)
        {
            if (!ReferenceEquals(s, skip))
            {
                s.Poll();
            }
        }
        if (answered != null && answered.Session.Poll(answered.OnReply, 16) < 0)
        {
            Fail($"the session's poll stopped on fault {answered.Session.GetFault()}");
        }
        Thread.Sleep(IDLE_MILLISECONDS);
    }

    static void Await(string what, Func<bool> done, List<Served> served, Answered? answered, Served? skip)
    {
        long until = Deadline();
        while (!done())
        {
            if (Stopwatch.GetTimestamp() > until)
            {
                Fail($"{what}: not within {WAIT_SECONDS} s; the services took {Show(served.Select(s => s.Entries.Count))} "
                    + $"entries and the session read {(answered == null ? "no" : answered.Replies.Count.ToString())} replies");
            }
            Cycle(served, answered, skip);
        }
    }

    static void Settle(List<Served> served, Answered? answered, Served? skip)
    {
        long until = Stopwatch.GetTimestamp() + Stopwatch.Frequency * SETTLE_MILLISECONDS / 1000;
        while (Stopwatch.GetTimestamp() < until)
        {
            Cycle(served, answered, skip);
        }
    }

    static void Offer(ClusterSession session, string message, List<Served> served, Answered answered)
    {
        long until = Deadline();
        while (true)
        {
            long offered = session.Offer(Bytes(message));
            if (offered >= 0)
            {
                return;
            }
            if (offered == Abi.PUBLICATION_ERROR)
            {
                Fail($"the offer of \"{message}\" failed with error {Abi.Errcode()}");
            }
            if (offered == Abi.PUBLICATION_CLOSED)
            {
                Fail($"the session closed before \"{message}\": {session.Closed()}");
            }
            if (Stopwatch.GetTimestamp() > until)
            {
                Fail($"the session's publication did not take \"{message}\" within {WAIT_SECONDS} s: {offered}");
            }
            Cycle(served, answered, null);
        }
    }

    /// <summary>The HandlerException of the leader's service's next poll that throws one, with
    /// the other services and the session polled meanwhile.</summary>
    static HandlerException HandlerExceptionOf(Served leader, List<Served> served, Answered answered, string what)
    {
        long until = Deadline();
        while (true)
        {
            try
            {
                leader.Poll();
            }
            catch (HandlerException thrown)
            {
                return thrown;
            }
            if (Stopwatch.GetTimestamp() > until)
            {
                Fail($"{what}: no poll of the leader's service threw HandlerException within {WAIT_SECONDS} s");
            }
            Cycle(served, answered, leader);
        }
    }

    public static int Main(string[] args)
    {
        if (args.Length != 3)
        {
            Fail("usage: Cluster <cluster directory> <members string> <response channel>");
        }
        try
        {
            Round(args[0], args[1], args[2]);
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

    static void Round(string cluster, string members, string responseChannel)
    {
        // 1. A service beside each member, over a client on that member's driver.
        List<Served> served = new();
        for (int member = 0; member < MEMBERS; member++)
        {
            served.Add(new Served(cluster, member));
        }
        // The service the round sets the thread's error with, closed at once.
        spent = served[0].Client.ClusterService();
        spent.Close();
        // The handler's reply is for the handler: from here there is no entry.
        if (served[0].Service.Reply(1, Bytes("x")) != -1 || Abi.Errcode() != Abi.ERR_CLUSTER_NO_ENTRY)
        {
            Fail($"a reply from outside the handler was not refused with ERR_CLUSTER_NO_ENTRY: error {Abi.Errcode()}");
        }

        // 2. Each service opens its member's log and reads the term's first entry before the
        // session connects: a service answers no entry that its member held when the service
        // first opened the log (spec/cluster.md, "Replies").
        Await("each service reads an entry of its member's log", () => served.All(s => s.Entries.Count > 0), served, null,
            null);

        // 3. A session with the cluster, on the driver at CAVALRY_DIR. The connect returns once
        // the leader has opened the session.
        Client client = new();
        ClusterSession session = client.ConnectCluster(members, responseChannel);
        Answered answered = new(session);
        if (session.Closed() != null || session.SessionId < 0 || session.MaxMessageLength <= 0)
        {
            Fail($"a session just opened reads closed {session.Closed()}, id {session.SessionId} and a longest message of "
                + $"{session.MaxMessageLength} bytes");
        }
        // What Offer refuses: a message longer than one frame carries.
        if (session.Offer(new byte[session.MaxMessageLength + 1]) != Abi.PUBLICATION_ERROR || Abi.Errcode() != Abi.ERR_TOO_LONG)
        {
            Fail($"a message of {session.MaxMessageLength + 1} bytes was not refused with PUBLICATION_ERROR and "
                + $"ERR_TOO_LONG: error {Abi.Errcode()}");
        }

        // 4. One message, taken by every service and answered by one.
        Offer(session, "hello", served, answered);
        Await("the message is taken by three services and answered",
            () => answered.Replies.Count > 0 && served.All(s => s.Taken("hello").Count > 0), served, answered, null);
        Settle(served, answered, null);
        if (answered.Replies.Count != 1)
        {
            Fail($"the session read {answered.Replies.Count} replies to one message: {Show(answered.Replies)}");
        }

        // 5. The same entry beside every member, answered by the leader's service alone.
        Served? leader = null;
        Entry first = served[0].Taken("hello")[0];
        int polling = Abi.ERR_POLLING;
        foreach (Served s in served)
        {
            if (!s.Taken("hello").SequenceEqual(new[] { first }) || first.Session != session.SessionId)
            {
                Fail($"the service beside member {s.Member} took the message as {Show(s.Taken("hello"))}, and the service "
                    + $"beside member 0 as {first}, of session {session.SessionId}");
            }
            if (s.Service.Sessions() != 1)
            {
                Fail($"the service beside member {s.Member} holds {s.Service.Sessions()} sessions open, not one");
            }
            if (s.Service.Position() <= first.Position)
            {
                Fail($"the service beside member {s.Member} is at {s.Service.Position()}, not past the entry at {first.Position}");
            }
            if (s.Service.GetFault().Code != 0)
            {
                Fail($"the service beside member {s.Member} reads fault {s.Service.GetFault()} after polls that took entries");
            }
            Refusal[] expected =
            {
                new("Poll", -1, polling), new("Position", -1, polling), new("Answers", 0, polling),
                new("Sessions", -1, polling), new("GetFault", -1, polling), new("Close", -1, polling),
                new("Client.IsClosed", 0, polling), new("Client.Closed", -1, polling),
                new("Client.ClusterService", -1, polling), new("Client.AddSubscription", -1, polling),
                new("Client.Close", -1, polling), new("Client.IsClosed after its Close", 0, polling),
            };
            if (s.Refusals == null || !s.Refusals.SequenceEqual(expected))
            {
                Fail($"the calls from inside the handler of the service beside member {s.Member} answered "
                    + $"{Show(s.Refusals)}, not {Show(expected)}");
            }
            List<int> codes = s.Codes["hello"];
            if (codes.SequenceEqual(new[] { Abi.CLUSTER_REPLY_SENT }))
            {
                if (leader != null)
                {
                    Fail($"the services beside members {leader.Member} and {s.Member} both answered one entry");
                }
                if (!s.Service.Answers())
                {
                    Fail($"the service beside member {s.Member} answered and says it does not answer");
                }
                leader = s;
            }
            else if (!codes.SequenceEqual(new[] { Abi.CLUSTER_REPLY_NOT_ANSWERING }) || s.Service.Answers())
            {
                Fail($"the service beside member {s.Member}, a follower, answered {Show(codes)} and says it answers: "
                    + s.Service.Answers());
            }
        }
        if (leader == null)
        {
            Fail("no service answered the entry");
        }
        Served led = leader!;
        if (!answered.Replies.SequenceEqual(new[] { new Answer(led.Member + ":hello", first.Term, first.Timestamp) }))
        {
            Fail($"the reply {Show(answered.Replies)} is not the leader's service's, of member {led.Member}, with the "
                + $"entry's term {first.Term} and the cluster's time {first.Timestamp}");
        }
        Refusal[] refused =
        {
            new("Offer", Abi.PUBLICATION_ERROR, polling), new("Poll", -1, polling), new("LeadershipTermId", -1, polling),
            new("LeaderMemberId", -1, polling), new("StaleReplies", -1, polling), new("Closed", 0, polling),
            new("GetFault", -1, polling), new("Close", -1, polling),
        };
        if (answered.Refusals == null || !answered.Refusals.SequenceEqual(refused))
        {
            Fail($"the calls from inside the session's handler answered {Show(answered.Refusals)}, not {Show(refused)}");
        }
        if (session.LeaderMemberId() != led.Member || session.LeadershipTermId() != first.Term || session.Closed() != null
            || session.GetFault().Code != 0)
        {
            Fail($"the session sends to member {session.LeaderMemberId()} in term {session.LeadershipTermId()}, reads "
                + $"closed {session.Closed()} and fault {session.GetFault()}; the entry's leader is member {led.Member} "
                + $"and its term {first.Term}");
        }
        List<Entry> terms = led.Entries.Where(e => e.Type == Abi.CLUSTER_ENTRY_NEW_LEADERSHIP_TERM).ToList();
        List<Entry> opens = led.Entries.Where(e => e.Type == Abi.CLUSTER_ENTRY_SESSION_OPEN).ToList();
        if (terms.Count == 0 || terms[^1].Leader != led.Member || opens.Count != 1 || opens[0].Session != session.SessionId)
        {
            Fail($"the leader's service read the new leadership term events {Show(terms)} and the session open events "
                + $"{Show(opens)}; the leader is member {led.Member} and the session {session.SessionId}");
        }
        Say($"session {session.SessionId}'s message, the entry at position {first.Position} of term {first.Term}, taken by "
            + $"three services and answered once, by the service beside member {led.Member}; twelve calls from inside "
            + "a service's handler, the client's close among them, and eight from inside the session's refused with "
            + "ERR_POLLING");

        // 6. A handler that replies and then throws: its entry is aborted, so the poll ends at
        // it with the entry not taken, Poll throws, and the next poll hands the entry over
        // again; the reply the first handling sent is not sent again.
        led.ThrowOn = "throw";
        long before = led.Service.Position();
        Offer(session, "throw", served, answered);
        HandlerException thrown = HandlerExceptionOf(led, served, answered, "a handler that throws");
        if (thrown.Cluster != "service" || thrown.Raised != 1 || thrown.InnerException is not InvalidOperationException
            || thrown.Fragments < 0 || thrown.CommittedLog || !thrown.Message.Contains("Its entry was aborted"))
        {
            Fail($"a service's HandlerException has Cluster {thrown.Cluster}, Fragments {thrown.Fragments}, Raised "
                + $"{thrown.Raised} and InnerException {thrown.InnerException}, and says: {thrown.Message}");
        }
        long aborted = led.Handed["throw"][0];
        long at = led.Service.Position();
        if (at < before || at > aborted || led.Taken("throw").Count != 0)
        {
            Fail($"after its handler threw on the entry at {aborted} the service is at {at}, from {before}, and took "
                + Show(led.Taken("throw")));
        }
        Await("the aborted entry is handed over again and its one reply read",
            () => answered.To("throw").Count > 0 && served.All(s => s.Taken("throw").Count > 0), served, answered, null);
        Settle(served, answered, null);
        if (!led.Handed["throw"].SequenceEqual(new[] { aborted, aborted })
            || !led.Codes["throw"].SequenceEqual(new[] { Abi.CLUSTER_REPLY_SENT, Abi.CLUSTER_REPLY_SENT })
            || answered.To("throw").Count != 1)
        {
            Fail($"the entry whose handler threw was handed over at {Show(led.Handed["throw"])}, its replies answered "
                + $"{Show(led.Codes["throw"])}, and the session read {Show(answered.To("throw"))}");
        }

        // 7. A handler that returns what is no action, as a throw: aborted, thrown by Poll with
        // the ABI's own error, and handed over again. On the second handling, before the
        // reply: a reply too long for a frame, and a reply to a session that is not open.
        led.NoActionOn = "no action";
        led.EdgesOn = "no action";
        Offer(session, "no action", served, answered);
        thrown = HandlerExceptionOf(led, served, answered, "a handler that returns what is no action");
        if (thrown.Cluster != "service" || thrown.Raised != 1 || thrown.InnerException is not CavalryException cause
            || cause.Code != Abi.ERR_CLUSTER_ACTION || cause.Function != "cavalry_cluster_service_action"
            || !cause.AbiMessage.Contains("3 is no action"))
        {
            Fail($"a handler that returned 3 made Poll throw {thrown.Message} from {thrown.InnerException}");
        }
        Await("the entry with no action is handed over again and answered",
            () => answered.To("no action").Count > 0 && served.All(s => s.Taken("no action").Count > 0), served, answered,
            null);
        Settle(served, answered, null);
        List<long> handed = led.Handed["no action"];
        if (handed.Count != 2 || handed[0] != handed[1] || answered.To("no action").Count != 1)
        {
            Fail($"the entry whose handler returned no action was handed over at {Show(handed)} and the session read "
                + Show(answered.To("no action")));
        }
        int[] edges = { Abi.CLUSTER_REPLY_TOO_LONG, Abi.ERR_TOO_LONG, Abi.CLUSTER_REPLY_NO_SESSION };
        if (led.Edges == null || !led.Edges.SequenceEqual(edges)
            || !led.Codes["no action"].SequenceEqual(new[] { Abi.CLUSTER_REPLY_SENT }))
        {
            Fail($"a reply of 1,345 bytes, the thread's error after it, and a reply to session {session.SessionId + 1000} "
                + $"answered {Show(led.Edges)}, not {Show(edges)}, and the reply after them {Show(led.Codes["no action"])}");
        }

        // 8. ACTION_BREAK: with two entries waiting, a poll takes one and ends. The leader's
        // service is not polled until the two others have taken both entries and a moment
        // more, so that its member has committed both.
        led.BreakOn.Add("break 1");
        led.BreakOn.Add("break 2");
        Offer(session, "break 1", served, answered);
        Offer(session, "break 2", served, answered);
        Await("the followers' services take two more entries",
            () => served.All(s => ReferenceEquals(s, led) || s.Taken("break 2").Count > 0), served, answered, led);
        Settle(served, answered, led);
        List<int> counts = new();
        long until = Deadline();
        while (led.Taken("break 2").Count == 0)
        {
            if (Stopwatch.GetTimestamp() > until)
            {
                Fail($"the leader's service took {Show(counts)} entries in its polls and not both entries");
            }
            int count = led.Poll();
            if (count != 0)
            {
                counts.Add(count);
            }
            else
            {
                Cycle(served, answered, led);
            }
        }
        if (!counts.SequenceEqual(new[] { 1, 1 }))
        {
            Fail($"with ACTION_BREAK returned for each of two entries the leader's polls took {Show(counts)}, not one each");
        }
        Await("the two replies are read", () => answered.To("break 1").Count > 0 && answered.To("break 2").Count > 0, served,
            answered, null);
        List<Answer> last = answered.Replies.GetRange(answered.Replies.Count - 2, 2);
        if (last[0].Payload.Substring(2) != "break 1" || last[1].Payload.Substring(2) != "break 2")
        {
            Fail($"the replies to two messages came out of order: {Show(last)}");
        }

        // 9. The session's handler throws: the reply was handed over, Poll throws once the ABI
        // has returned, and the reply is not handed over again.
        answered.ThrowOn = "reply throws";
        Offer(session, "reply throws", served, answered);
        HandlerException? replyThrown = null;
        until = Deadline();
        while (replyThrown == null)
        {
            if (Stopwatch.GetTimestamp() > until)
            {
                Fail("the reply on which the session's handler throws was not read");
            }
            try
            {
                Cycle(served, answered, null);
            }
            catch (HandlerException error)
            {
                replyThrown = error;
            }
        }
        answered.ThrowOn = null;
        if (replyThrown.Cluster != "session" || replyThrown.Fragments != 1 || replyThrown.Raised != 1
            || replyThrown.InnerException is not InvalidOperationException
            || !replyThrown.Message.Contains("the reply handler threw on 1 of the 1 reply this poll handed over"))
        {
            Fail($"a session's HandlerException has Cluster {replyThrown.Cluster}, Fragments {replyThrown.Fragments}, "
                + $"Raised {replyThrown.Raised} and InnerException {replyThrown.InnerException}, and says: "
                + replyThrown.Message);
        }
        Settle(served, answered, null);
        if (answered.To("reply throws").Count != 1 || session.StaleReplies() != 0)
        {
            Fail($"the reply whose handler threw was read {answered.To("reply throws").Count} times, and the session "
                + $"passed over {session.StaleReplies()} stale replies");
        }
        Say("a handler that threw and one that returned no action had their entries aborted, handed over again and "
            + "answered once; a reply of 1,345 bytes and a reply to no session refused; ACTION_BREAK took one of two "
            + "entries; a reply whose handler threw is not handed over again");

        // 9b. The program's timers (spec/c-abi.md, version 7; spec/cluster.md, "The service",
        // "Timers"; ADR 0157). Each service's program schedules four timers at one message
        // and cancels one: the three are handed to every service once each, as entries of
        // their own type with the timers' ids, at one position each and none earlier than
        // its deadline, and the cancelled one to none. Step 10 holds the "once" for a timer
        // refresh interval and more. From outside the handler both calls are refused, the
        // thread's error having been set to another code first.
        foreach (Refusal refusal in new[]
        {
            Asked("ScheduleTimer", () => served[0].Service.ScheduleTimer(1, 1)),
            Asked("CancelTimer", () => served[0].Service.CancelTimer(1)),
        })
        {
            if (refusal.Answer != -1 || refusal.Code != Abi.ERR_CLUSTER_NO_ENTRY)
            {
                Fail($"{refusal.Call} from outside the handler answered {refusal.Answer} with error {refusal.Code}, not -1 "
                    + "with ERR_CLUSTER_NO_ENTRY");
            }
        }
        Offer(session, TIMER_MESSAGE, served, answered);
        long[] timerIds = { long.MinValue, TIMER_ID, long.MaxValue };
        Await("each service is handed its three timers", () => served.All(s => s.Timers.Count >= timerIds.Length), served,
            answered, null);
        Settle(served, answered, null);
        Entry scheduled = served[0].Taken(TIMER_MESSAGE)[0];
        int[] timerCodes =
        {
            Abi.CLUSTER_TIMER_SCHEDULED, Abi.CLUSTER_TIMER_SCHEDULED, Abi.CLUSTER_TIMER_SCHEDULED, 0,
            Abi.CLUSTER_TIMER_SCHEDULED, Abi.CLUSTER_TIMER_SCHEDULED,
        };
        foreach (Served s in served)
        {
            if (!s.TimerCodes.SequenceEqual(timerCodes))
            {
                Fail($"the service beside member {s.Member} was answered {Show(s.TimerCodes)} for its schedules and its "
                    + $"cancel; expected CLUSTER_TIMER_SCHEDULED, {Abi.CLUSTER_TIMER_SCHEDULED}, for each schedule and 0 for "
                    + "the cancel");
            }
            if (!s.Timers.Select(t => t.Id).Order().SequenceEqual(timerIds))
            {
                Fail($"the service beside member {s.Member} was handed the timers {Show(s.Timers)}; expected timer "
                    + $"{TIMER_ID}, the least long and the greatest once each, and timer {CANCELLED_ID}, which it cancelled, "
                    + "never");
            }
            if (!s.Timers.SequenceEqual(served[0].Timers))
            {
                Fail($"the service beside member {s.Member} was handed its timers as {Show(s.Timers)} and the service "
                    + $"beside member {served[0].Member} as {Show(served[0].Timers)}; expected one position and one time "
                    + "for each");
            }
            foreach (Timer t in s.Timers)
            {
                long after = t.Id == TIMER_ID ? TIMER_AFTER_MS : 0;
                if (t.Timestamp < scheduled.Timestamp + after || t.Position <= scheduled.Position)
                {
                    Fail($"the service beside member {s.Member} was handed timer {t.Id} at position {t.Position} and time "
                        + $"{t.Timestamp}; it was scheduled at position {scheduled.Position} and time "
                        + $"{scheduled.Timestamp} for {after} ms later");
                }
            }
        }
        // What a deadline that was ignored would have read: the time of the timers that
        // were due at once, which the member was told of with timer 77. The deadline check
        // above proves something for as long as this is below the deadline.
        Timer timer = served[0].Timers.First(t => t.Id == TIMER_ID);
        long atOnce = served[0].Timers.Where(t => t.Id != TIMER_ID).Max(t => t.Timestamp) - scheduled.Timestamp;
        string margin = atOnce < TIMER_AFTER_MS ? "" : ", so the deadline's check proved nothing in this run";
        Say($"timer {TIMER_ID}, scheduled by each service at a message for an hour later and then for {TIMER_AFTER_MS} ms "
            + $"later, handed to three services once, at position {timer.Position}, "
            + $"{timer.Timestamp - scheduled.Timestamp} ms after the message; the least and the greatest long as ids, due "
            + $"at once, handed to each once with those ids, {atOnce} ms after the message{margin}; timer {CANCELLED_ID}, "
            + "due at once and cancelled in the same handling, handed to none; both calls refused from outside the handler");

        // 10. The session before its client. Its close is an entry every service is handed,
        // after which the log holds no session open.
        session.Close();
        session.Close(); // idempotent
        if (session.Closed() != null)
        {
            Fail($"a session that Close() freed reads closed {session.Closed()}");
        }
        client.Close();
        Await("each service reads the session's close",
            () => served.All(s => s.Entries.Any(e => e.Type == Abi.CLUSTER_ENTRY_SESSION_CLOSE)), served, null, null);
        foreach (Served s in served)
        {
            List<Entry> closes = s.Entries.Where(e => e.Type == Abi.CLUSTER_ENTRY_SESSION_CLOSE).ToList();
            if (closes.Count != 1 || closes[0].Session != first.Session || closes[0].TimedOut || s.Service.Sessions() != 0)
            {
                Fail($"the service beside member {s.Member} read the session close events {Show(closes)} and holds "
                    + $"{s.Service.Sessions()} sessions open; the session {first.Session} was closed by its client");
            }
            if (!s.Entries.SequenceEqual(served[0].Entries))
            {
                Fail($"the services beside members 0 and {s.Member} took different entries: {Show(served[0].Entries)} and "
                    + Show(s.Entries));
            }
        }
        // The timers of step 9b, once each: a leader gives a timer that is still pending in
        // its service another event a timer refresh interval after the first, so the
        // services are polled until that long has passed since the last of them was handed
        // a timer.
        long refreshed = served.Max(s => s.TimerAt) + Stopwatch.Frequency * REFRESH_MILLISECONDS / 1000;
        while (Stopwatch.GetTimestamp() < refreshed)
        {
            Cycle(served, null, null);
        }
        foreach (Served s in served)
        {
            if (s.Timers.Count != 3)
            {
                Fail($"the service beside member {s.Member} was handed the timers {Show(s.Timers)} by "
                    + $"{REFRESH_MILLISECONDS} ms after the last; expected three, once each");
            }
        }

        // 11. Each service before its own client, twice; and the last after its client.
        foreach (Served s in served.Take(MEMBERS - 1))
        {
            s.Service.Close();
            s.Service.Close(); // idempotent
            s.Client.Close();
        }
        Served lastServed = served[MEMBERS - 1];
        lastServed.Client.Close();
        lastServed.Service.Close();
        Served longest = served.MaxBy(s => s.Longest)!;
        Say($"{served[0].Entries.Count} entries taken by each of three services; the session's close read by each; a "
            + "service closed after its client");
        Say($"report: {served.Sum(s => s.Polls)} polls of the three services, each holding its thread for its run: "
            + $"{served.Sum(s => s.Over)} took over 1 ms, and the longest {longest.Longest} us, {longest.LongestOf} (not gated)");
        Console.WriteLine("conformance/dotnet: cluster round ok");
    }
}
