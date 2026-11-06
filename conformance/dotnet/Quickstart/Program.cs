// The .NET client's conformance runner (clients/README.md; ADR 0046, card 209): the
// ten-minute path's shape through clients/dotnet, as conformance/python/quickstart.py runs
// it through clients/python. It checks that a package of another ABI version is refused by
// the check Abi makes when it loads, CheckAbiVersion, which names both numbers. Against a
// driver at CAVALRY_DIR it checks the ABI version both ways, records stream 1001 on
// cavalry:ipc, publishes 1,000 messages of 64 bytes by claim and commit, each carrying its
// index and a pattern, and reads them back through the fragment handler once and in order,
// with the header's type, flags, version, frame length, term offset and session id checked
// against spec/frame.md; waits for the recording to sync to the stream's end and stops it;
// checks where a handler's exception goes, in both orders of two exception types, that a
// header kept past its call throws and a committed claim's span is empty, and the refusals
// of spec/c-abi.md, "Calls during a claim or a poll", a close from inside the handler
// included, and that a poll, a claim and a commit after the closes are answered by the ABI,
// on stream 1003 (Edges); checks that a reserved value beyond 2^53 reaches the handler
// exactly on stream 1005 (ReservedValues); adds publications from four threads through one
// Client at once (SharedClient); does the same 1,000 over cavalry:udp on the loopback; and
// replays the recording onto stream 2001, each frame at the position the recording holds it
// at, with the end reported. Every wait is bounded at 10 s, and a wait that runs out says
// how far it got. It exits 1 with the function, the code and cavalry_errmsg when a call
// failed, or with the code's name when a result code did.
//
// Then the report, printed and never gated, from two rounds that check nothing else. The
// latency round (Latency): 100,000 messages over cavalry:ipc on stream 1004, one at a time
// on this thread, each carrying the publisher's Stopwatch timestamp from before its commit,
// which the handler subtracts from the clock it reads first; the one-way p50, p99, p99.99
// and max, nearest rank, so ten samples lie above the p99.99. The allocation round
// (Allocations): 10,000 messages on stream 1002 whose handler only counts and whose
// publisher copies one constant, with the thread's allocated bytes read from
// GC.GetAllocatedBytesForCurrentThread before and after the round with nothing between,
// which counts every object the thread allocated, freed or not, and the collections that
// ran. .NET's standard library only (ADR 0003).

using System;
using System.Buffers.Binary;
using System.Collections.Generic;
using System.Diagnostics;
using System.Globalization;
using System.Net;
using System.Net.Sockets;
using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;
using System.Threading;
using Cavalry;

public static class Quickstart
{
    const int MESSAGES = 1000;
    const int LATENCY_MESSAGES = 100_000;
    const int ALLOCATION_MESSAGES = 10_000;
    const int BODY = 64;
    const int STREAM = 1001;
    const int REPLAY_STREAM = 2001;
    const long WAIT_TICKS_PER_SECOND_FACTOR = 10;

    static void Fail(string what)
    {
        Console.Error.WriteLine("conformance/dotnet: " + what);
        Environment.Exit(1);
    }

    static void Say(string what)
    {
        Console.WriteLine("conformance/dotnet: " + what);
    }

    static string CodeName(long code)
    {
        if (code == Abi.PUBLICATION_NOT_CONNECTED) return "NOT_CONNECTED";
        if (code == Abi.PUBLICATION_BACK_PRESSURED) return "BACK_PRESSURED";
        if (code == Abi.PUBLICATION_ADMIN_ACTION) return "ADMIN_ACTION";
        if (code == Abi.PUBLICATION_CLOSED) return "CLOSED";
        if (code == Abi.PUBLICATION_MAX_POSITION_EXCEEDED) return "MAX_POSITION_EXCEEDED";
        if (code == Abi.PUBLICATION_ERROR) return "ERROR";
        return code.ToString(CultureInfo.InvariantCulture);
    }

    static long Deadline()
    {
        return Stopwatch.GetTimestamp() + Stopwatch.Frequency * WAIT_TICKS_PER_SECOND_FACTOR;
    }

    static bool Passed(long until)
    {
        return Stopwatch.GetTimestamp() - until > 0;
    }

    /// <summary>What a subscription has received: the next index expected, and the frames'
    /// term ids and offsets kept from the handler for the positions.</summary>
    sealed class Receiver
    {
        public readonly string Name;
        public readonly Subscription Subscription;
        public int Next;
        public int SessionId;
        public bool HaveSession;
        public readonly int[] TermIds = new int[MESSAGES];
        public readonly int[] TermOffsets = new int[MESSAGES];
        public readonly long[] Positions = new long[MESSAGES];
        public Image? Constants;
        public readonly FragmentHandler OnFragment;

        public Receiver(string name, Subscription subscription)
        {
            Name = name;
            Subscription = subscription;
            OnFragment = Receive;
        }

        void Receive(ReadOnlySpan<byte> body, Header header)
        {
            if (Next >= MESSAGES) Fail(Name + ": a fragment after all " + MESSAGES + " messages");
            int index = (int) BinaryPrimitives.ReadInt64LittleEndian(body);
            if (index != Next) Fail(Name + ": expected message " + Next + ", got " + index);
            if (body.Length != BODY) Fail(Name + ": message " + index + " has " + body.Length + " bytes, not " + BODY);
            for (int b = 8; b < BODY; b++)
            {
                int got = body[b];
                if (got != ((index + b) & 0xff))
                {
                    Fail(Name + ": message " + index + " byte " + b + " is " + got + ", not " + ((index + b) & 0xff));
                }
            }
            // The header against spec/frame.md: a data frame, unfragmented, version 0, the body
            // plus the 32-byte header, aligned to 32 in its term.
            int type = header.Type, flags = header.Flags, version = header.Version;
            int frameLength = header.FrameLength, termOffset = header.TermOffset;
            if (type != Abi.FRAME_TYPE_DATA || flags != Abi.FLAGS_UNFRAGMENTED || version != 0 || frameLength != 32 + BODY
                || termOffset % 32 != 0)
            {
                Fail(Name + ": message " + index + "'s header reads type " + type + ", flags " + flags + ", version "
                    + version + ", frame length " + frameLength + ", term offset " + termOffset);
            }
            if (!HaveSession)
            {
                SessionId = header.SessionId;
                HaveSession = true;
            }
            else if (header.SessionId != SessionId)
            {
                Fail(Name + ": message " + index + " from session " + header.SessionId + ", the stream's is " + SessionId);
            }
            TermIds[Next] = header.TermId;
            TermOffsets[Next] = termOffset;
            Next++;
        }

        /// <summary>The constants of the image the frames came from, once a poll has read one,
        /// for the positions (spec/c-abi.md, "A frame's position"); the string is copied now.</summary>
        public void TakeConstants()
        {
            if (Constants != null || !HaveSession) return;
            foreach (Image image in Subscription.Images())
            {
                if (image.SessionId == SessionId)
                {
                    Constants = image;
                    return;
                }
            }
        }

        public void ComputePositions()
        {
            if (Constants == null) Fail(Name + ": no image carried session " + SessionId + " after " + Next + " messages");
            for (int i = 0; i < Next; i++)
            {
                int termCount = TermIds[i] - Constants!.InitialTermId; // 32-bit two's complement
                Positions[i] = ((long) termCount << (int) Constants.PositionBitsToShift) + TermOffsets[i];
            }
        }

        public void ExpectNoMore()
        {
            int count = Subscription.Poll(OnFragment, 16);
            if (count != 0) Fail(Name + ": " + count + " more fragment(s) after all " + MESSAGES + " messages");
        }
    }

    /// <summary>A claim refused with the position, below zero: fail at once on a code that does
    /// not pass, ERROR with the ABI's error, CLOSED or MAX_POSITION_EXCEEDED, or once the
    /// round's 10 s have passed, saying how far the round got; else return, to claim again.</summary>
    static void Refused(string name, long position, int sent, long until, int total)
    {
        if (position == Abi.PUBLICATION_ERROR)
        {
            Fail(name + ": cavalry_publication_try_claim failed with error " + Abi.Errcode() + ": " + Abi.Errmsg());
        }
        if (position == Abi.PUBLICATION_CLOSED || position == Abi.PUBLICATION_MAX_POSITION_EXCEEDED)
        {
            Fail(name + ": TryClaim returned " + CodeName(position) + " (" + position + ") after " + sent + " of " + total
                + " messages");
        }
        if (Passed(until))
        {
            Fail(name + ": published " + sent + " of " + total + " in 10 s; the last claim returned " + CodeName(position)
                + " (" + position + ")");
        }
    }

    /// <summary>One poll of up to 16 fragments: the count, or a failure with the subscription's
    /// fault or the handler's exception, saying how far the round got.</summary>
    static int PollOrFail(string name, Subscription subscription, FragmentHandler handler, int received, int total)
    {
        int count;
        try
        {
            count = subscription.Poll(handler, 16);
        }
        catch (HandlerException e)
        {
            Fail(name + ": the fragment handler threw after " + received + " of " + total + " messages: " + e);
            return -1;
        }
        if (count < 0)
        {
            Subscription.Fault fault = subscription.GetFault();
            Fail(name + ": poll stopped on fault " + fault.Code + " of session " + fault.SessionId + " at position "
                + fault.Position + " after " + received + " of " + total + " messages; " + Abi.Errmsg());
        }
        return count;
    }

    static void WriteMessage(Span<byte> claim, int i)
    {
        BinaryPrimitives.WriteInt64LittleEndian(claim, i);
        for (int b = 8; b < BODY; b++)
        {
            claim[b] = (byte) (i + b);
        }
    }

    static void PublishAndReceive(Publication publication, Receiver r)
    {
        long until = Deadline();
        for (int i = 0; i < MESSAGES;)
        {
            // The image joins on a poll; take its constants once a frame from it is read.
            PollOrFail(r.Name, r.Subscription, r.OnFragment, r.Next, MESSAGES);
            r.TakeConstants();
            long position = publication.TryClaim(BODY);
            if (position < 0)
            {
                Refused(r.Name, position, i, until, MESSAGES);
                continue;
            }
            WriteMessage(publication.Claim, i);
            if (publication.Commit() != position) Fail(r.Name + ": commit did not return the claim's position " + position);
            i++;
        }
        until = Deadline();
        while (r.Next < MESSAGES)
        {
            PollOrFail(r.Name, r.Subscription, r.OnFragment, r.Next, MESSAGES);
            r.TakeConstants();
            if (Passed(until)) Fail(r.Name + ": received " + r.Next + " of " + MESSAGES + " in 10 s");
        }
        r.ExpectNoMore();
        r.ComputePositions();
    }

    /// <summary>Offers the message, retrying a refusal that passes, NOT_CONNECTED,
    /// BACK_PRESSURED or ADMIN_ACTION, for up to 10 s; the position.</summary>
    static long OfferOrFail(string name, Publication publication, byte[] message, int sent)
    {
        long until = Deadline();
        while (true)
        {
            long position = publication.Offer(message);
            if (position >= 0) return position;
            if (position == Abi.PUBLICATION_CLOSED || position == Abi.PUBLICATION_MAX_POSITION_EXCEEDED
                || position == Abi.PUBLICATION_ERROR || Passed(until))
            {
                Fail(name + ": Offer returned " + CodeName(position) + " (" + position + ") after " + sent + " messages");
            }
        }
    }

    static byte[] Message(int i)
    {
        byte[] m = new byte[BODY];
        BinaryPrimitives.WriteInt64LittleEndian(m, i);
        return m;
    }

    static int IndexOf(ReadOnlySpan<byte> body)
    {
        return (int) BinaryPrimitives.ReadInt64LittleEndian(body);
    }

    static long ClaimOrFail(string name, Publication publication, int sent)
    {
        long until = Deadline();
        while (true)
        {
            long position = publication.TryClaim(BODY);
            if (position >= 0) return position;
            Refused(name, position, sent, until, sent + 1);
        }
    }

    static bool SameSpan(Span<byte> a, Span<byte> b)
    {
        return a.Length == b.Length && Unsafe.AreSame(ref MemoryMarshal.GetReference(a), ref MemoryMarshal.GetReference(b));
    }

    static List<int> Range(int from, int to)
    {
        List<int> outList = new();
        for (int i = from; i < to; i++) outList.Add(i);
        return outList;
    }

    static bool SameList(List<int> a, List<int> b)
    {
        if (a.Count != b.Count) return false;
        for (int i = 0; i < a.Count; i++) if (a[i] != b[i]) return false;
        return true;
    }

    static string Show(List<int> list)
    {
        return "[" + string.Join(", ", list) + "]";
    }

    /// <summary>The client's edges, on stream 1003. Where a handler's exception goes
    /// (clients/dotnet/README.md, "The exception"): ten messages offered, then one poll of ten
    /// whose handler throws on message 3. The ABI's poll consumes all ten, so every one must
    /// reach the handler, once and in order, and Poll must throw HandlerException with
    /// Fragments 10, Raised 1 and the exception as its InnerException. Then a poll whose
    /// handler throws an InvalidOperationException on one message and an ArgumentException on
    /// the next: Raised 2 and the first as the inner exception. Then the lifetimes: a
    /// committed claim's span is empty, and a header kept past its handler call throws
    /// InvalidOperationException. Then the refusals of spec/c-abi.md, "Calls during a claim or
    /// a poll", which leave the wrapper as they found it: a poll from inside a handler of its
    /// own subscription, a second claim with one outstanding, a close with a claim
    /// outstanding, and a close from inside the handler, which keeps the handle. Then a poll,
    /// a claim and a commit after the closes, answered by the ABI.</summary>
    static void Edges(Client client)
    {
        string name = "handler exceptions";
        Subscription subscription = client.AddSubscription("cavalry:ipc", STREAM + 2);
        Publication publication = client.AddPublication("cavalry:ipc", STREAM + 2);
        List<int> seen = new();
        FragmentHandler unexpected = (body, header) => Fail("a fragment the test did not expect, after " + Show(seen));

        long until = Deadline();
        while (true)
        {
            subscription.Poll(unexpected, 10); // nothing is offered yet; the image joins on a poll
            long position = publication.Offer(Message(0));
            if (position >= 0) break;
            if (position == Abi.PUBLICATION_CLOSED || position == Abi.PUBLICATION_MAX_POSITION_EXCEEDED
                || position == Abi.PUBLICATION_ERROR || Passed(until))
            {
                Fail(name + ": the first offer returned " + CodeName(position) + " (" + position + ")");
            }
        }
        for (int i = 1; i < 10; i++) OfferOrFail(name, publication, Message(i), i);

        FragmentHandler throwsOn3 = (body, header) =>
        {
            int index = IndexOf(body);
            seen.Add(index);
            if (index == 3) throw new ArgumentException("message 3");
        };
        HandlerException? error = null;
        try
        {
            subscription.Poll(throwsOn3, 10);
        }
        catch (HandlerException e)
        {
            error = e;
        }
        if (error == null) Fail(name + ": a handler that threw on message 3 did not make Poll throw HandlerException; it saw " + Show(seen));
        if (!SameList(seen, Range(0, 10)) || error!.Fragments != 10 || error.Raised != 1
            || error.InnerException is not ArgumentException || error.InnerException.Message != "message 3")
        {
            Fail(name + ": the handler saw " + Show(seen) + "; HandlerException has Fragments " + error!.Fragments + ", Raised "
                + error.Raised + ", inner " + error.InnerException + "; expected messages 0 to 9 once and in order, 10, 1 and the exception");
        }
        if (!error.Message.Contains("Fix the handler") || !error.Message.EndsWith("See clients/dotnet/README.md#the-exception", StringComparison.Ordinal))
        {
            Fail(name + ": HandlerException's message does not say what to do and where it is explained: " + error.Message);
        }

        OfferOrFail(name, publication, Message(10), 10);
        OfferOrFail(name, publication, Message(11), 11);
        FragmentHandler throwsTwice = (body, header) =>
        {
            int index = IndexOf(body);
            seen.Add(index);
            if (index == 10) throw new InvalidOperationException("message 10");
            throw new ArgumentException("message 11");
        };
        error = null;
        try
        {
            subscription.Poll(throwsTwice, 10);
        }
        catch (HandlerException e)
        {
            error = e;
        }
        if (error == null || !SameList(seen, Range(0, 12)) || error.Fragments != 2 || error.Raised != 2
            || error.InnerException is not InvalidOperationException || error.InnerException.Message != "message 10")
        {
            Fail(name + ": Poll threw " + error + " and the handler saw " + Show(seen) + "; expected HandlerException with Fragments 2,"
                + " Raised 2 and the InvalidOperationException, the first, as its inner exception, and messages 0 to 11");
        }
        if (subscription.Poll(unexpected, 10) != 0) Fail(name + ": a fragment after all 12 messages");
        Say(name + ": a handler that threw on message 3 of a poll of 10 saw all 10 once, in order, and Poll threw HandlerException"
            + " (Fragments 10, Raised 1, the exception as its inner exception); a handler that threw two exceptions in one poll"
            + " gave Raised 2 and the first as the inner exception");

        // The lifetimes: the claim's span is empty after its commit, and a header read after
        // its handler call throws. A span kept past its call points into the log; the README
        // says so.
        name = "lifetimes";
        long claimed = ClaimOrFail(name, publication, 12);
        Span<byte> view = publication.Claim;
        BinaryPrimitives.WriteInt64LittleEndian(view, 12);
        if (publication.Commit() != claimed || !publication.Claim.IsEmpty)
        {
            Fail(name + ": the commit did not return the claim's position " + claimed + " and leave the claim empty");
        }
        Header? kept = null;
        int firstRead = 0, secondRead = 0;
        FragmentHandler keeps = (body, header) =>
        {
            kept = header;
            firstRead = header.SessionId;
            secondRead = header.SessionId;
        };
        until = Deadline();
        while (kept == null)
        {
            PollOrFail(name, subscription, keeps, 0, 1);
            if (Passed(until)) Fail(name + ": message 12 did not arrive in 10 s");
        }
        if (firstRead != secondRead) Fail(name + ": two reads of SessionId in one handler call gave " + firstRead + " and " + secondRead);
        try
        {
            int late = kept!.SessionId;
            Fail(name + ": a header read after its handler call gave session " + late);
        }
        catch (InvalidOperationException expected)
        {
            if (!expected.Message.Contains("clients/dotnet/README.md")) Fail(name + ": the late read's message names no docs: " + expected.Message);
        }
        Say(name + ": a committed claim's span is empty, a header read after its handler call threw InvalidOperationException, and"
            + " SessionId gave the one number on each read inside the call");

        // Calls the ABI refuses during a poll or a claim leave the wrapper as they found it
        // (spec/c-abi.md, "Calls during a claim or a poll").
        name = "refusals";
        for (int i = 13; i < 16; i++) OfferOrFail(name, publication, Message(i), i);
        List<int> outer = new();
        List<int[]> nested = new();
        FragmentHandler nestedHandler = (body, header) => Fail("a poll from inside a handler of the same subscription delivered a fragment");
        FragmentHandler outerHandler = (body, header) =>
        {
            outer.Add(IndexOf(body));
            int count = subscription.Poll(nestedHandler, 10);
            nested.Add(new[] { count, Abi.Errcode() });
        };
        until = Deadline();
        while (outer.Count < 3)
        {
            PollOrFail(name, subscription, outerHandler, outer.Count, 3);
            if (Passed(until)) Fail(name + ": received messages " + Show(outer) + " of 13 to 15 in 10 s");
        }
        bool refusedEach = nested.Count == 3;
        foreach (int[] result in nested) refusedEach &= result[0] == -1 && result[1] == Abi.ERR_POLLING;
        if (!SameList(outer, Range(13, 16)) || !refusedEach)
        {
            Fail(name + ": the outer handler saw " + Show(outer) + " and the polls from inside it returned " + nested.Count
                + " results; expected messages 13 to 15, and -1 with ERR_POLLING (" + Abi.ERR_POLLING + ") each time");
        }

        claimed = ClaimOrFail(name, publication, 16);
        Span<byte> first = publication.Claim;
        BinaryPrimitives.WriteInt64LittleEndian(first, 16);
        long second = publication.TryClaim(BODY);
        int code = Abi.Errcode();
        if (second != Abi.PUBLICATION_ERROR || code != Abi.ERR_CLAIM_OUTSTANDING || !SameSpan(publication.Claim, first))
        {
            Fail(name + ": a TryClaim with a claim outstanding returned " + CodeName(second) + " with error " + code
                + ", and the claim is " + publication.Claim.Length + " bytes; expected ERROR with ERR_CLAIM_OUTSTANDING and the outstanding claim's span");
        }
        if (publication.Commit() != claimed || !publication.Claim.IsEmpty)
        {
            Fail(name + ": the outstanding claim's commit did not return its position " + claimed + " and leave the claim empty");
        }
        List<int> received = new();
        FragmentHandler index = (body, header) => received.Add(IndexOf(body));
        until = Deadline();
        while (received.Count == 0)
        {
            PollOrFail(name, subscription, index, 16, 17);
            if (Passed(until)) Fail(name + ": message 16 did not arrive in 10 s");
        }
        if (!SameList(received, new List<int> { 16 })) Fail(name + ": received " + Show(received) + " for the claim committed past a refused one; expected message 16");

        ClaimOrFail(name, publication, 17);
        publication.Close();
        if (!publication.Claim.IsEmpty) Fail(name + ": the publication's close left a claim of " + publication.Claim.Length + " bytes");
        PollOrFail(name, subscription, unexpected, 17, 17); // the close aborted the claim: padding, never a fragment

        // A close from inside the handler: refused with ERR_POLLING, the handle kept, the next
        // message still delivered, and the close after the poll succeeds.
        Publication again = client.AddPublication("cavalry:ipc", STREAM + 2);
        OfferOrFail(name, again, Message(17), 17);
        CavalryException? closeError = null;
        FragmentHandler closesInside = (body, header) =>
        {
            received.Add(IndexOf(body));
            try
            {
                subscription.Close();
            }
            catch (CavalryException e)
            {
                closeError = e;
            }
        };
        until = Deadline();
        while (received.Count < 2)
        {
            PollOrFail(name, subscription, closesInside, received.Count, 2);
            if (Passed(until)) Fail(name + ": message 17 did not arrive in 10 s");
        }
        if (closeError == null || closeError.Code != Abi.ERR_POLLING || subscription.IsClosed())
        {
            Fail(name + ": a close from inside the handler gave " + closeError + " and IsClosed is " + subscription.IsClosed()
                + "; expected CavalryException with ERR_POLLING and the subscription open");
        }
        OfferOrFail(name, again, Message(18), 18);
        until = Deadline();
        while (received.Count < 3)
        {
            PollOrFail(name, subscription, index, received.Count, 3);
            if (Passed(until)) Fail(name + ": message 18 did not arrive in 10 s after the refused close");
        }
        if (!SameList(received, Range(16, 19))) Fail(name + ": received " + Show(received) + "; expected messages 16, 17 and 18, the last after the refused close");
        subscription.Close();
        if (!subscription.IsClosed()) Fail(name + ": the close after the poll left the subscription open");
        subscription.Close(); // idempotent
        again.Close();
        // After a close the wrapper hands the ABI a null handle, which it refuses with
        // ERR_NULL (spec/c-abi.md, "When a function cannot answer"): a poll answers -1, a
        // claim PUBLICATION_ERROR, and the commit of a struct that names no claim -1 with
        // ERR_NO_CLAIM; never a delivery, never a frame.
        int afterClose = subscription.Poll(unexpected, 10);
        int afterCloseCode = Abi.Errcode();
        long claimAfterClose = again.TryClaim(BODY);
        int claimAfterCloseCode = Abi.Errcode();
        long commitAfterClose = again.Commit();
        int commitAfterCloseCode = Abi.Errcode();
        if (afterClose != -1 || afterCloseCode != Abi.ERR_NULL || claimAfterClose != Abi.PUBLICATION_ERROR
            || claimAfterCloseCode != Abi.ERR_NULL || commitAfterClose != -1 || commitAfterCloseCode != Abi.ERR_NO_CLAIM)
        {
            Fail(name + ": after the closes, Poll returned " + afterClose + " with error " + afterCloseCode + ", TryClaim "
                + CodeName(claimAfterClose) + " with error " + claimAfterCloseCode + ", Commit " + commitAfterClose
                + " with error " + commitAfterCloseCode + "; expected -1 and ERROR with ERR_NULL (" + Abi.ERR_NULL
                + "), and -1 with ERR_NO_CLAIM (" + Abi.ERR_NO_CLAIM + ")");
        }
        Say(name + ": a poll from inside a handler of its own subscription returned -1 with ERR_POLLING and the outer handler saw"
            + " messages 13 to 15; a TryClaim with a claim outstanding returned ERROR with ERR_CLAIM_OUTSTANDING, the claim stayed"
            + " the first span and its commit left it empty; a close with a claim outstanding left the claim empty; a close"
            + " from inside the handler threw CavalryException with ERR_POLLING, kept the subscription open for message 18, and"
            + " the close after the poll closed it; after the closes, Poll and TryClaim were refused by the ABI with ERR_NULL"
            + " and Commit with ERR_NO_CLAIM");
    }

    /// <summary>Checks, on stream 1005, that a reserved value beyond 2^53 and the smallest long
    /// reach the handler exactly.</summary>
    static void ReservedValues(Client client)
    {
        string name = "reserved values";
        Subscription subscription = client.AddSubscription("cavalry:ipc", STREAM + 4);
        Publication publication = client.AddPublication("cavalry:ipc", STREAM + 4);
        long[] values = { (1L << 62) + 1, long.MinValue };
        List<long> read = new();
        FragmentHandler onFragment = (body, header) => read.Add(header.ReservedValue);
        long until = Deadline();
        for (int i = 0; i < values.Length; i++)
        {
            while (true)
            {
                subscription.Poll(onFragment, 10);
                long position = publication.TryClaim(BODY);
                if (position >= 0) break;
                Refused(name, position, i, until, values.Length);
            }
            BinaryPrimitives.WriteInt64LittleEndian(publication.Claim, i);
            if (publication.SetReservedValue(values[i]) != 0)
            {
                Fail(name + ": SetReservedValue(" + values[i] + ") failed with error " + Abi.Errcode() + ": " + Abi.Errmsg());
            }
            publication.Commit();
        }
        until = Deadline();
        while (read.Count < values.Length)
        {
            PollOrFail(name, subscription, onFragment, read.Count, values.Length);
            if (Passed(until)) Fail(name + ": received " + read.Count + " of " + values.Length + " in 10 s");
        }
        if (read.Count != 2 || read[0] != values[0] || read[1] != values[1]) Fail(name + ": the handler read [" + string.Join(", ", read) + "]; expected [" + string.Join(", ", values) + "] exactly");
        publication.Close();
        subscription.Close();
        Say(name + ": 2^62+1 and the smallest long reached the handler exactly through ReservedValue");
    }

    /// <summary>Adds publications from four threads through one Client at once: the client's
    /// methods hold its lock, so threads may share it (clients/dotnet/README.md, "Threads").</summary>
    static void SharedClient(Client client)
    {
        List<Thread> threads = new();
        List<Publication> made = new();
        Exception? failure = null;
        for (int t = 0; t < 4; t++)
        {
            int stream = STREAM + 10 + t;
            Thread thread = new(() =>
            {
                try
                {
                    Publication publication = client.AddPublication("cavalry:ipc", stream);
                    lock (made)
                    {
                        made.Add(publication);
                    }
                }
                catch (Exception e)
                {
                    failure = e;
                }
            });
            threads.Add(thread);
            thread.Start();
        }
        foreach (Thread thread in threads) thread.Join(10_000);
        if (failure != null || made.Count != 4) Fail("adds from four threads through one client: " + made.Count + " made, failure " + failure);
        foreach (Publication publication in made) publication.Close();
        Say("four threads added a publication each through one Client at once; the client's lock serialised them");
    }

    /// <summary>The latency round, for the report: 100,000 messages over cavalry:ipc on stream
    /// 1004, one at a time on this thread. The publisher writes the Stopwatch timestamp into
    /// the claim just before its commit; the handler reads the clock first and keeps the
    /// difference. The latencies in nanoseconds, in order.</summary>
    static long[] Latency(Client client)
    {
        string name = "the latency round";
        Subscription subscription = client.AddSubscription("cavalry:ipc", STREAM + 3);
        Publication publication = client.AddPublication("cavalry:ipc", STREAM + 3);
        long[] latencies = new long[LATENCY_MESSAGES];
        int received = 0;
        FragmentHandler onFragment = (body, header) =>
        {
            long now = Stopwatch.GetTimestamp();
            latencies[received] = now - BinaryPrimitives.ReadInt64LittleEndian(body);
            received++;
        };
        long until = Deadline();
        for (int i = 0; i < LATENCY_MESSAGES; i++)
        {
            while (true)
            {
                long position = publication.TryClaim(BODY);
                if (position >= 0) break;
                Refused(name, position, i, until, LATENCY_MESSAGES);
                PollOrFail(name, subscription, onFragment, received, LATENCY_MESSAGES); // the image joins on a poll
            }
            BinaryPrimitives.WriteInt64LittleEndian(publication.Claim, Stopwatch.GetTimestamp());
            publication.Commit();
            while (received <= i)
            {
                PollOrFail(name, subscription, onFragment, received, LATENCY_MESSAGES);
                if (Passed(until)) Fail(name + ": received " + received + " of " + LATENCY_MESSAGES + "; message " + i + " did not arrive in 10 s");
            }
            until = Deadline(); // 10 s for each message, outside the interval it measures
        }
        if (subscription.Poll(onFragment, 16) != 0) Fail(name + ": a fragment after all " + LATENCY_MESSAGES + " messages");
        publication.Close();
        subscription.Close();
        double nanosPerTick = 1_000_000_000.0 / Stopwatch.Frequency;
        for (int i = 0; i < latencies.Length; i++) latencies[i] = (long) (latencies[i] * nanosPerTick);
        return latencies;
    }

    static long Collections()
    {
        long count = 0;
        for (int generation = 0; generation <= GC.MaxGeneration; generation++) count += GC.CollectionCount(generation);
        return count;
    }

    /// <summary>The allocation round, for the report: 10,000 messages over cavalry:ipc on
    /// stream 1002 whose handler only counts and whose publisher copies one constant, so that
    /// the checks' objects are not in the count, with the thread's allocated bytes read
    /// before and after, nothing between, after a collection. {measured, bytes, collections}.</summary>
    static long[] Allocations(Client client)
    {
        string name = "the allocation round";
        Subscription subscription = client.AddSubscription("cavalry:ipc", STREAM + 1);
        Publication publication = client.AddPublication("cavalry:ipc", STREAM + 1);
        int received = 0;
        FragmentHandler count = (body, header) => received++;
        byte[] constant = new byte[BODY];
        for (int b = 0; b < BODY; b++) constant[b] = (byte) b;
        long until = Deadline();
        while (true)
        {
            PollOrFail(name, subscription, count, received, ALLOCATION_MESSAGES);
            long position = publication.TryClaim(BODY);
            if (position >= 0) break;
            Refused(name, position, 0, until, ALLOCATION_MESSAGES);
        }
        constant.CopyTo(publication.Claim);
        publication.Commit();
        int sent = 1;
        PollOrFail(name, subscription, count, received, ALLOCATION_MESSAGES);

        GC.Collect();
        GC.WaitForPendingFinalizers();
        int receivedBefore = received;
        long collectionsBefore = Collections();
        long bytesBefore = GC.GetAllocatedBytesForCurrentThread();
        until = Deadline();
        while (sent < ALLOCATION_MESSAGES)
        {
            long position = publication.TryClaim(BODY);
            if (position >= 0)
            {
                constant.CopyTo(publication.Claim);
                publication.Commit();
                sent++;
            }
            else
            {
                Refused(name, position, sent, until, ALLOCATION_MESSAGES);
            }
            PollOrFail(name, subscription, count, received, ALLOCATION_MESSAGES);
        }
        while (received < ALLOCATION_MESSAGES)
        {
            PollOrFail(name, subscription, count, received, ALLOCATION_MESSAGES);
            if (Passed(until)) Fail(name + ": received " + received + " of " + ALLOCATION_MESSAGES + " in 10 s");
        }
        long bytesAfter = GC.GetAllocatedBytesForCurrentThread();
        long collectionsAfter = Collections();

        if (subscription.Poll(count, 16) != 0) Fail(name + ": a fragment after all " + ALLOCATION_MESSAGES + " messages");
        publication.Close();
        subscription.Close();
        return new[] { ALLOCATION_MESSAGES - receivedBefore, bytesAfter - bytesBefore, collectionsAfter - collectionsBefore };
    }

    /// <summary>The nearest-rank percentile parts/whole of sorted values: the smallest value
    /// with at least that share of the values at or below it, and how many lie above.</summary>
    static long[] Percentile(long[] sorted, int parts, int whole)
    {
        int rank = Math.Max(1, (int) Math.Ceiling((double) sorted.Length * parts / whole));
        return new[] { sorted[rank - 1], sorted.Length - rank };
    }

    static string Commas(long n)
    {
        return n.ToString("N0", CultureInfo.InvariantCulture);
    }

    static int FreeUdpPort()
    {
        using Socket socket = new(AddressFamily.InterNetwork, SocketType.Dgram, ProtocolType.Udp);
        socket.Bind(new IPEndPoint(IPAddress.Loopback, 0));
        return ((IPEndPoint) socket.LocalEndPoint!).Port;
    }

    public static void Main()
    {
        // The load-time check (spec/c-abi.md, "Versions and what a reader rejects"): the check
        // Abi makes when it loads refuses a package whose ABI_VERSION is not the library's,
        // with a CavalryException naming both numbers.
        int version = Abi.ABI_VERSION;
        CavalryException? refusal = null;
        try
        {
            Abi.CheckAbiVersion(version + 1);
        }
        catch (CavalryException e)
        {
            refusal = e;
        }
        if (refusal == null) Fail("a package of ABI version " + (version + 1) + " was not refused against a library of version " + version);
        if (refusal!.Code != Abi.ERR_ABI_VERSION || !refusal.AbiMessage.Contains("is ABI version " + version + " and")
            || !refusal.AbiMessage.Contains("from a header of ABI version " + (version + 1) + ";"))
        {
            Fail("a package of ABI version " + (version + 1) + " was refused with " + refusal + "; expected ERR_ABI_VERSION naming both versions");
        }
        Say("ABI version: a package of version " + (version + 1) + " was refused by CheckAbiVersion: " + refusal.Message);
        if (Abi.LibraryAbiVersion() != version) Fail("the library's ABI version is " + Abi.LibraryAbiVersion() + ", the package's " + version);

        long[] latencies;
        long[] allocated;
        using (Client client = new())
        {
            Say("client " + client.ClientId + " on " + client.Dir + ", library " + Abi.Version() + ", ABI version "
                + Abi.LibraryAbiVersion() + ", .NET " + Environment.Version);

            // 1. Record the stream, then publish 1,000 messages over cavalry:ipc and read them
            // back, once and in order.
            Subscription ipcSub = client.AddSubscription("cavalry:ipc", STREAM);
            Publication ipcPub = client.AddPublication("cavalry:ipc", STREAM);
            Recording recording = client.StartRecording("cavalry:ipc", STREAM); // the publication first: it records a live one
            Receiver ipc = new("cavalry:ipc", ipcSub);
            PublishAndReceive(ipcPub, ipc);
            long end = ipcPub.Position();
            Say(MESSAGES + " messages over cavalry:ipc by session " + ipc.SessionId + " received once, in order; the stream ends at " + end);
            long until = Deadline();
            while (recording.SyncedPosition() < end)
            {
                if (Passed(until)) Fail("the recording synced " + recording.SyncedPosition() + " of " + end + " in 10 s");
            }
            client.StopRecording(recording.RecordingId);
            long recordingId = recording.RecordingId;
            recording.Close();
            ipcPub.Close();

            Edges(client);
            ReservedValues(client);
            SharedClient(client);

            // The report's two rounds, which check nothing else.
            latencies = Latency(client);
            Array.Sort(latencies);
            allocated = Allocations(client);

            // 2. The same 1,000 over cavalry:udp on the loopback.
            string udpChannel = "cavalry:udp?endpoint=127.0.0.1:" + FreeUdpPort();
            Subscription udpSub = client.AddSubscription(udpChannel, STREAM);
            Publication udpPub = client.AddPublication(udpChannel, STREAM);
            Receiver udp = new(udpChannel, udpSub);
            PublishAndReceive(udpPub, udp);
            Say(MESSAGES + " messages over " + udpChannel + " by session " + udp.SessionId + " received once, in order, from "
                + udp.Constants!.SourceIdentity);
            udpPub.Close();
            udpSub.Close();

            // 3. Replay the recording onto stream 2001: the same checks, each frame at the
            // position the recording holds it at (ADR 0030), and the end reported.
            Subscription replaySub = client.AddSubscription("cavalry:ipc", REPLAY_STREAM);
            Replay replay = client.StartReplay(recordingId, Abi.NULL_POSITION, Abi.NULL_LENGTH, REPLAY_STREAM, "cavalry:ipc");
            Receiver replayed = new("replay", replaySub);
            until = Deadline();
            Replay.End state;
            while (true)
            {
                PollOrFail("replay", replaySub, replayed.OnFragment, replayed.Next, MESSAGES);
                replayed.TakeConstants();
                state = replay.Ended();
                if (replayed.Next >= MESSAGES && state.Ended) break;
                if (Passed(until))
                {
                    if (state.Ended) Fail("replayed " + replayed.Next + " of " + MESSAGES + " in 10 s; the replay ended at " + state.Position + " with reason " + state.Reason);
                    Fail("replayed " + replayed.Next + " of " + MESSAGES + " in 10 s; the replay has not ended, at position " + replay.Position());
                }
            }
            replayed.ExpectNoMore();
            replayed.ComputePositions();
            if (state.Position != end || state.Reason != Abi.REPLAY_STOP_ENDED)
            {
                Fail("the replay ended at " + state.Position + " with reason " + state.Reason + ", not at " + end + " with " + Abi.REPLAY_STOP_ENDED);
            }
            for (int i = 0; i < MESSAGES; i++)
            {
                if (replayed.Positions[i] != ipc.Positions[i]) Fail("message " + i + " replayed at " + replayed.Positions[i] + ", recorded at " + ipc.Positions[i]);
            }
            Say(MESSAGES + " messages replayed on stream " + REPLAY_STREAM + " at the positions the recording holds them, ended at " + state.Position);
            replay.Close();
            replaySub.Close();
            ipcSub.Close();
        }

        // The report, never gated: the laptop's or the runner's cost of this side of the
        // boundary (clients/dotnet/README.md, "The runtime's side").
        long[] p50 = Percentile(latencies, 50, 100);
        long[] p99 = Percentile(latencies, 99, 100);
        long[] p9999 = Percentile(latencies, 9999, 10000);
        Say("report: one-way latency over " + Commas(latencies.Length) + " cavalry:ipc messages, one at a time on one thread, nearest"
            + " rank: p50 " + Commas(p50[0]) + " ns, p99 " + Commas(p99[0]) + " ns, p99.99 " + Commas(p9999[0]) + " ns (" + p9999[1]
            + " samples above it), max " + Commas(latencies[latencies.Length - 1]) + " ns");
        Say("report: allocations per message, this side of the boundary, over " + Commas(allocated[0]) + " messages whose handler"
            + " counts and whose publisher copies a constant: " + ((double) allocated[1] / allocated[0]).ToString("F1", CultureInfo.InvariantCulture)
            + " bytes (GC.GetAllocatedBytesForCurrentThread, nothing read between), " + allocated[2]
            + " collection(s) during the round");
        Say("ok");
    }
}
