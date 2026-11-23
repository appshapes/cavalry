"""The .NET writer of clients/generate.py: `write_dotnet(header)` returns the files of
`clients/dotnet`, every one generated from the committed header (clients/README.md; ADR
0046, card 209), except `example/Program.cs` and `example/Example.csproj`, the README's
program, which are written by hand.

The library reaches the shared library through P/Invoke with the source-generated
marshalling (`LibraryImport`, .NET 7 and later; the library takes .NET 10, the current
long-term release), so it has no dependency and no native shim: `dotnet build` and
`dotnet run` are the whole toolchain. `Abi.cs` is the binding: the header's defines as
constants, a `StructLayout(LayoutKind.Sequential)` struct per struct, blittable so that
`ref` passes it pinned and copies nothing, a `LibraryImport` per function with its
parameters' C types mapped to blittable .NET types (a handle `nint`, a string `string`
marshalled as UTF-8, a caller-owned struct `ref`, an out handle `out nint`, the fragment
handler an unmanaged function pointer), a static constructor that resolves the library's
path and reads `cavalry_abi_version` before any other function, refusing a library of
another version, and the helpers the client's own code shares, `Check`, `LastError`,
`Errcode`, `Errmsg`, `String` and `FindLibrary`. The other files are the client's own
code: `CavalryException`, `HandlerException`, `FragmentHandler`, `Client`, `Publication`,
`Subscription`, `CommittedLog`, `ClusterSession`, `ClusterReplyValues`, `ClusterService`,
`ClusterEntryValues`, `Header`, `Image`, `Recording` and `Replay`, one type per file, plus
`Cavalry.csproj`. The fragment handler is reached through one `UnmanagedCallersOnly`
method, the trampoline, which finds the subscription by the `GCHandle` the subscription
hands the ABI as its `clientd`; a committed log, a cluster session and a cluster service
each have theirs in the same way, and a cluster service's passes the ABI the action its
handler returned (ADR 0124). Python 3.9 standard library only.
"""

from __future__ import annotations

import re

TARGET_FRAMEWORK = "net10.0"
LIBRARY = "cavalry_ffi"
CS_SCALAR = {"int": "int", "int8_t": "sbyte", "uint8_t": "byte", "uint16_t": "ushort", "int32_t": "int",
             "int64_t": "long", "size_t": "nuint", "bool": "bool", "void": "void"}
C_LIMITS = {"INT64_MAX": ("long", "long.MaxValue"), "INT64_MIN": ("long", "long.MinValue"),
            "INT32_MAX": ("int", "int.MaxValue"), "INT32_MIN": ("int", "int.MinValue"),
            "UINT8_MAX": ("int", "255"), "UINT16_MAX": ("int", "65535")}
CALLBACK = "delegate* unmanaged[Cdecl]<void*, byte*, nuint, void*, void>"
# The per-message methods of the generated C#, which clients/lint.py reads.
PER_MESSAGE_DOTNET = ("Publication.TryClaim", "Publication.SetReservedValue", "Publication.Commit", "Publication.Abort",
                      "Publication.Offer", "Publication.Claim", "Subscription.Poll", "Subscription.OnFragment",
                      "Subscription.Deliver", "CommittedLog.Poll", "CommittedLog.OnEntry", "CommittedLog.Deliver",
                      # A cluster session and a cluster service (ADR 0124).
                      "ClusterSession.Offer", "ClusterSession.Poll", "ClusterSession.OnReply", "ClusterSession.Deliver",
                      "ClusterService.Poll", "ClusterService.Reply", "ClusterService.OnClusterEntry",
                      "ClusterService.Deliver", "ClusterReplyValues.LeadershipTermId", "ClusterReplyValues.Timestamp",
                      "ClusterEntryValues.Position", "ClusterEntryValues.LeadershipTermId",
                      "ClusterEntryValues.Timestamp", "ClusterEntryValues.SessionId", "ClusterEntryValues.Type",
                      "ClusterEntryValues.LeaderMemberId", "ClusterEntryValues.TimedOut",
                      "Header.FrameLength", "Header.Version", "Header.Flags", "Header.Type",
                      "Header.TermOffset", "Header.SessionId", "Header.StreamId", "Header.TermId", "Header.ReservedValue")


def cs_name(define: str) -> str:
    """`CAVALRY_ERR_POLLING` as `ERR_POLLING`: the ABI's name without the prefix."""
    return define[len("CAVALRY_"):] if define.startswith("CAVALRY_") else define


def cs_constant(value: str) -> tuple[str, str]:
    """(type, literal) for a define's value."""
    if value in C_LIMITS:
        return C_LIMITS[value]
    if value.startswith('"'):
        return "string", value
    if re.fullmatch(r"-?\d+", value) or re.fullmatch(r"0x[0-9a-fA-F]+", value):
        return "int", value
    if re.fullmatch(r"\(?-?\d+\)?", value):
        return "int", value.strip("()")
    raise SystemExit(f"generate_dotnet: cannot read the define's value {value!r}")


def cs_type(ctype: str, header, where: str) -> str:
    """The .NET type for a C type of the header, at a function parameter (`param`), a
    function's return (`return`), a struct field (`field`) or the callback's parameter
    (`callback`): blittable throughout, so that the generated marshalling copies nothing."""
    c = " ".join(ctype.split())
    const = c.startswith("const ")
    base = c[6:] if const else c
    structs = [s.name for s in header.structs]
    if base == "char *":
        return "string" if where == "param" else "nint"
    if base == "uint8_t *":
        return "byte*"
    if base == "void *":
        return "void*" if where == "callback" else "nint"
    if base.endswith(" **"):
        inner = base[:-3]
        if inner in header.opaque and where == "param":
            return "out nint"
        # Bytes the library holds and the caller reads, as a restored state is.
        if inner == "uint8_t" and where == "param":
            return "out byte*"
        raise SystemExit(f"generate_dotnet: no .NET type for {c!r}")
    if base.endswith(" *"):
        inner = base[:-2]
        if inner in header.opaque:
            return "void*" if where == "callback" else "nint"
        if inner in structs and where == "param":
            return f"ref {inner}"
        if inner in CS_SCALAR and where == "param":
            return f"out {CS_SCALAR[inner]}"
        raise SystemExit(f"generate_dotnet: no .NET type for {c!r} at a {where}")
    if base in [cb.name for cb in header.callbacks]:
        return CALLBACK
    if base in structs:
        return base
    if base == "bool":
        return "byte" if where == "field" else "bool"
    if base in CS_SCALAR:
        return CS_SCALAR[base]
    raise SystemExit(f"generate_dotnet: no .NET type for {c!r}")


def cs_param_name(name: str) -> str:
    """A C parameter's name as a C# one: `out` is a keyword, and `string` too."""
    return {"out": "result", "string": "text", "params": "parameters"}.get(name, name)


def structs(header) -> str:
    lines = []
    for s in header.structs:
        lines.append("    /// <summary>`" + s.name + "` of spec/c-abi.md, \"Structs\": the caller's copy the ABI fills, laid out"
                     " as C lays it out.</summary>")
        lines.append("    [StructLayout(LayoutKind.Sequential)]")
        lines.append(f"    public struct {s.name}")
        lines.append("    {")
        for f in s.fields:
            lines.append(f"        public {cs_type(f.type, header, 'field')} {f.name};")
        lines.append("    }")
        lines.append("")
    return "\n".join(lines).rstrip("\n")


def imports(header) -> str:
    """One `LibraryImport` per function, `cavalry_abi_version` included; a `bool` is
    marshalled as one byte, as C's is, and a `string` parameter as UTF-8."""
    out = []
    for fn in header.functions:
        ret = cs_type(fn.ret, header, "return")
        params = []
        for p in fn.params:
            t = cs_type(p.type, header, "param")
            if t == "bool":
                t = "[MarshalAs(UnmanagedType.U1)] bool"
            params.append(f"{t} {cs_param_name(p.name)}")
        has_string = any(cs_type(p.type, header, "param") == "string" for p in fn.params)
        attr = f'[LibraryImport(Library{", StringMarshalling = StringMarshalling.Utf8" if has_string else ""})]'
        out.append(f"    {attr}")
        if ret == "bool":
            out.append("    [return: MarshalAs(UnmanagedType.U1)]")
        out.append(f"    internal static partial {ret} {fn.name}({', '.join(params)});")
        out.append("")
    return "\n".join(out).rstrip("\n")


def write_dotnet(header) -> dict[str, str]:
    defines = [(n, v) for n, v in header.defines if n != "CAVALRY_H"]
    names = [fn.name for fn in header.functions]
    missing = [n for n in ("CAVALRY_ABI_VERSION", "CAVALRY_ERR_ABI_VERSION") if n not in dict(defines)]
    missing += [n for n in ("cavalry_abi_version",) if n not in names]
    if missing:
        raise SystemExit(f"generate_dotnet: {', '.join(missing)} is not in the header, and the binding's static constructor"
                         f" checks the library's ABI version with it (spec/c-abi.md, \"Versions and what a reader rejects\")")
    [cb] = header.callbacks
    callback_params = ", ".join(cs_type(p.type, header, "callback") for p in cb.params)
    if f"delegate* unmanaged[Cdecl]<{callback_params}, void>" != CALLBACK or cb.ret != "void":
        raise SystemExit(f"generate_dotnet: the fragment handler's type changed to ({callback_params}) -> {cb.ret}; teach"
                         f" CALLBACK and the trampoline in clients/generate_dotnet.py the new shape")
    constants = "\n".join(f"    public const {cs_constant(v)[0]} {cs_name(n)} = {cs_constant(v)[1]}; // {n}"
                          for n, v in defines)
    abi = (ABI.replace("@CONSTANTS@", constants).replace("@STRUCTS@", structs(header)).replace("@IMPORTS@", imports(header))
           .replace("@LIBRARY@", LIBRARY))
    return {
        "Cavalry.csproj": CSPROJ.replace("@TFM@", TARGET_FRAMEWORK),
        "Abi.cs": abi,
        "CavalryException.cs": CAVALRY_EXCEPTION,
        "HandlerException.cs": HANDLER_EXCEPTION,
        "FragmentHandler.cs": FRAGMENT_HANDLER,
        "Client.cs": CLIENT,
        "Publication.cs": PUBLICATION,
        "Subscription.cs": SUBSCRIPTION,
        "CommittedLog.cs": COMMITTED_LOG,
        "ClusterSession.cs": CLUSTER_SESSION,
        "ClusterReplyValues.cs": CLUSTER_REPLY_VALUES,
        "ClusterService.cs": CLUSTER_SERVICE,
        "ClusterEntryValues.cs": CLUSTER_ENTRY_VALUES,
        "Header.cs": HEADER,
        "Image.cs": IMAGE,
        "Recording.cs": RECORDING,
        "Replay.cs": REPLAY,
    }


HEADING = """// Generated by clients/generate.py dotnet from crates/cavalry-ffi/include/cavalry.h; do not
// edit by hand, `just lint` regenerates and compares (clients/README.md; clients/dotnet/README.md).
"""

CSPROJ = """<!-- Generated by clients/generate.py dotnet; do not edit by hand, `just lint` regenerates and
     compares (clients/README.md; clients/dotnet/README.md). The library over the C ABI: no
     package reference, no native shim; `AllowUnsafeBlocks` for the pointers the ABI hands
     over, and every warning an error. -->
<Project Sdk="Microsoft.NET.Sdk">

  <PropertyGroup>
    <TargetFramework>@TFM@</TargetFramework>
    <RootNamespace>Cavalry</RootNamespace>
    <AssemblyName>Cavalry</AssemblyName>
    <Nullable>enable</Nullable>
    <ImplicitUsings>disable</ImplicitUsings>
    <AllowUnsafeBlocks>true</AllowUnsafeBlocks>
    <TreatWarningsAsErrors>true</TreatWarningsAsErrors>
    <GenerateDocumentationFile>false</GenerateDocumentationFile>
    <SatelliteResourceLanguages>en</SatelliteResourceLanguages>
    <IsPackable>false</IsPackable>
  </PropertyGroup>

  <ItemGroup>
    <Compile Remove="example/**" />
  </ItemGroup>

</Project>
"""

ABI = HEADING + r'''
using System;
using System.IO;
using System.Reflection;
using System.Runtime.InteropServices;

namespace Cavalry;

/// <summary>
/// The binding over the C ABI of spec/c-abi.md (clients/dotnet/README.md): the header's
/// constants under the ABI's names without their prefix, a struct per struct, a
/// <c>LibraryImport</c> per function, and the helpers the client's own code shares. The
/// static constructor resolves the shared library (<see cref="FindLibrary"/>) and reads
/// <c>cavalry_abi_version</c> before any other function, refusing a library of another ABI
/// version with a <see cref="CavalryException"/> that names both numbers; the runtime
/// reports a static constructor's exception as a <c>TypeInitializationException</c> with it
/// as the inner exception.
/// </summary>
public static unsafe partial class Abi
{
    internal const string Library = "@LIBRARY@";

@CONSTANTS@

@STRUCTS@

@IMPORTS@

    /// <summary>The path the resolver loaded.</summary>
    public static readonly string LibraryPath;

    static Abi()
    {
        LibraryPath = FindLibrary();
        NativeLibrary.SetDllImportResolver(typeof(Abi).Assembly, (name, assembly, path) =>
            name == Library ? NativeLibrary.Load(LibraryPath) : IntPtr.Zero);
        CheckAbiVersion(ABI_VERSION);
    }

    /// <summary>The shared library: <c>CAVALRY_FFI_LIBRARY</c> when set, else
    /// <c>target/release/libcavalry_ffi</c> of the checkout this package sits in, which
    /// <c>cargo build --release -p cavalry-ffi --lib</c> writes, found by walking up from the
    /// directory this assembly was loaded from.</summary>
    public static string FindLibrary()
    {
        string? set = Environment.GetEnvironmentVariable("CAVALRY_FFI_LIBRARY");
        if (!string.IsNullOrEmpty(set))
        {
            return set;
        }
        string suffix = OperatingSystem.IsMacOS() ? ".dylib" : ".so";
        string location = typeof(Abi).Assembly.Location;
        string? here = string.IsNullOrEmpty(location) ? Directory.GetCurrentDirectory() : Path.GetDirectoryName(location);
        for (int up = 0; up < 8 && here != null; up++)
        {
            string candidate = Path.Combine(here, "target", "release", "libcavalry_ffi" + suffix);
            if (File.Exists(candidate))
            {
                return candidate;
            }
            here = Path.GetDirectoryName(here);
        }
        throw new CavalryException("FindLibrary", 0, "libcavalry_ffi" + suffix + " was not found: build it with"
            + " `cargo build --release -p cavalry-ffi --lib` in the checkout, or set CAVALRY_FFI_LIBRARY to its path;"
            + " see clients/dotnet/README.md");
    }

    /// <summary>The ABI's error code of this thread's last failed call (<c>cavalry_errcode</c>):
    /// what a result code below zero, such as <see cref="PUBLICATION_ERROR"/>, or a poll's -1
    /// stands for.</summary>
    public static int Errcode()
    {
        return cavalry_errcode();
    }

    /// <summary>The message of this thread's last failed call (<c>cavalry_errmsg</c>), or "".</summary>
    public static string Errmsg()
    {
        return String(cavalry_errmsg()) ?? "";
    }

    /// <summary>The ABI version of the library this package loaded (<c>cavalry_abi_version</c>).</summary>
    public static int LibraryAbiVersion()
    {
        return cavalry_abi_version();
    }

    /// <summary>The library's own version, as <c>cavalry_version_full</c> gives it.</summary>
    public static string Version()
    {
        return String(cavalry_version_full()) ?? "";
    }

    /// <summary>The check the static constructor makes, with <see cref="ABI_VERSION"/>: the
    /// library must be of the ABI version the package was generated from, else a
    /// <see cref="CavalryException"/> with <see cref="ERR_ABI_VERSION"/> that names both
    /// numbers.</summary>
    public static void CheckAbiVersion(int expected)
    {
        int found = cavalry_abi_version();
        if (found != expected)
        {
            throw new CavalryException("cavalry_abi_version", ERR_ABI_VERSION, "the library at " + LibraryPath
                + " is ABI version " + found + " and this package was generated from a header of ABI version "
                + expected + "; build the library from the same checkout (cargo build --release -p cavalry-ffi"
                + " --lib) or regenerate the package from the library's header with python3 clients/generate.py dotnet;"
                + " see docs/reference/c-abi.md#versions-and-what-a-reader-rejects");
        }
    }

    /// <summary>The exception for a call that failed: the ABI's code and message.</summary>
    internal static CavalryException LastError(string fn)
    {
        return new CavalryException(fn, Errcode(), Errmsg());
    }

    /// <summary>Throws <see cref="LastError"/> when a call's result is not 0.</summary>
    internal static void Check(string fn, int result)
    {
        if (result != 0)
        {
            throw LastError(fn);
        }
    }

    /// <summary>The C string at a pointer the ABI returned, copied, or null for NULL.</summary>
    internal static string? String(nint pointer)
    {
        return pointer == 0 ? null : Marshal.PtrToStringUTF8(pointer);
    }
}
'''

CAVALRY_EXCEPTION = HEADING + r'''
using System;

namespace Cavalry;

/// <summary>
/// A call that is not a message failed: <see cref="Function"/> is the ABI function, and
/// <see cref="Code"/> and <see cref="AbiMessage"/> are the ABI's (<c>cavalry_errcode</c>,
/// <c>cavalry_errmsg</c>), the message naming what to do next and its docs anchor.
/// </summary>
public sealed class CavalryException : Exception
{
    public string Function { get; }
    public int Code { get; }
    public string AbiMessage { get; }

    public CavalryException(string function, int code, string abiMessage)
        : base(function + ": " + abiMessage + " (error " + code + ")")
    {
        Function = function;
        Code = code;
        AbiMessage = abiMessage;
    }
}
'''

HANDLER_EXCEPTION = HEADING + r'''
using System;

namespace Cavalry;

/// <summary>
/// The handler threw during a poll, a subscription's fragment handler or, when
/// <see cref="CommittedLog"/> is true, a committed log's entry handler, or, when
/// <see cref="Cluster"/> is "session", a cluster session's reply handler:
/// <see cref="Fragments"/> is the count the poll returned once the ABI was done, of
/// fragments, of entries or of replies, or -1 when the poll also stopped on a fault, which the
/// <c>GetFault()</c> of the object that was polled reads until its next poll;
/// <see cref="Raised"/> how many handler calls threw, and
/// <see cref="Exception.InnerException"/> the first exception, kept as thrown.
/// A cluster service's entry handler, <see cref="Cluster"/> "service", is the one whose
/// failure stops its poll. The entry whose handler threw, or returned what is no action, was
/// aborted: it is not taken, the poll ended at it, and the next poll hands it over again.
/// <see cref="Fragments"/> is then the count of entries the poll took before that entry,
/// <see cref="Raised"/> is 1, and <see cref="Exception.InnerException"/> is what the handler
/// threw, or a <see cref="CavalryException"/> with <see cref="Abi.ERR_CLUSTER_ACTION"/> and
/// the ABI's message for the value the handler returned.
/// </summary>
public sealed class HandlerException : Exception
{
    public int Fragments { get; }
    public int Raised { get; }
    public bool CommittedLog { get; }

    /// <summary>"session" when the poll was a cluster session's, "service" when it was a
    /// cluster service's, whose entry was aborted and comes again, and null for a
    /// subscription's and a committed log's.</summary>
    public string? Cluster { get; }

    public HandlerException(int fragments, int raised, Exception cause, bool committedLog = false, string? cluster = null)
        : base(Describe(fragments, raised, cause, committedLog, cluster), cause)
    {
        Fragments = fragments;
        Raised = raised;
        CommittedLog = committedLog;
        Cluster = cluster;
    }

    /// <summary>The message: what the handler threw on, the fragments of a subscription's
    /// poll, the entries of a committed log's or the replies of a cluster session's, and
    /// what to do. The fault of a committed
    /// log's own image is reported by one poll (clients/dotnet/README.md, "The committed
    /// log"), so its message names the log's own <c>GetFault()</c>. A cluster service's
    /// says that its entry was aborted and comes again (clients/dotnet/README.md, "The
    /// cluster service").</summary>
    private static string Describe(int fragments, int raised, Exception cause, bool committedLog, string? cluster)
    {
        if (cluster == "service")
        {
            string took = fragments < 0
                ? "in a poll that also stopped on a fault, which this ClusterService's GetFault() reads until its next poll"
                : "after the " + fragments + " " + (fragments == 1 ? "entry" : "entries") + " this poll took";
            return "the entry handler threw, or returned what is no action, " + took + ". Its entry was aborted: it is"
                + " not taken, the poll ended at it, and the next poll hands it over again. Fix the handler: what it"
                + " did, " + cause.GetType().Name + ", is this exception's InnerException; every poll hands the entry"
                + " over, and throws this, until the handler takes it. See clients/dotnet/README.md#the-exception";
        }
        bool session = cluster == "session";
        string one = session ? "reply" : committedLog ? "entry" : "fragment";
        string many = session ? "replies" : committedLog ? "entries" : "fragments";
        string delivered;
        if (fragments < 0)
        {
            delivered = session
                ? "the replies this poll handed over before it stopped on a fault, which this ClusterSession's GetFault()"
                    + " reads until its next poll"
                : committedLog
                ? "the entries this poll delivered before it stopped on a fault, which this CommittedLog's GetFault() reads"
                    + " until its next poll, and its Faulted() from then on when the fault is the log's own image's"
                : "the fragments this poll delivered before it stopped on a fault, which Subscription.GetFault() reads";
        }
        else if (session)
        {
            delivered = "the " + fragments + " " + (fragments == 1 ? one : many) + " this poll handed over";
        }
        else if (committedLog)
        {
            delivered = "the " + fragments + " " + (fragments == 1 ? one : many) + " this poll delivered";
        }
        else
        {
            delivered = "the " + fragments + " fragment(s) this poll delivered";
        }
        return "the " + one + " handler threw on " + raised + " of " + delivered
            + "; every " + one + " reached the handler once, in order. Fix the handler: its first exception, " + cause.GetType().Name
            + ", is this exception's InnerException; polling again does not deliver these " + many + " again."
            + " See clients/dotnet/README.md#the-exception";
    }
}
'''

FRAGMENT_HANDLER = HEADING + r'''
using System;

namespace Cavalry;

/// <summary>
/// Receives each fragment a poll delivers: <paramref name="body"/> is a span over the log's
/// bytes, valid for the call, and <paramref name="header"/> the frame's header, valid for the
/// call.
/// </summary>
public delegate void FragmentHandler(ReadOnlySpan<byte> body, Header header);

/// <summary>
/// Receives each entry a committed log's poll delivers: <paramref name="body"/> and
/// <paramref name="header"/> as a fragment's, and <paramref name="position"/> the entry's
/// position in the log.
/// </summary>
public delegate void EntryHandler(ReadOnlySpan<byte> body, Header header, long position);

/// <summary>
/// Receives each reply a cluster session's poll hands over: <paramref name="body"/> is the
/// reply's payload, a span over the log's bytes, valid for the call, and
/// <paramref name="reply"/> the reply's values.
/// </summary>
public delegate void ReplyHandler(ReadOnlySpan<byte> body, ClusterReplyValues reply);

/// <summary>
/// Receives each committed entry a cluster service's poll hands over:
/// <paramref name="body"/> is a session message's payload, a span over the log's bytes,
/// valid for the call, and empty for every other entry; <paramref name="entry"/> is the
/// entry's values; and <paramref name="service"/> is the service that was polled, for its
/// <see cref="ClusterService.Reply"/>. What it returns is the entry's action:
/// <see cref="Abi.ACTION_CONTINUE"/> takes the entry and the poll goes on,
/// <see cref="Abi.ACTION_BREAK"/> takes it and ends the poll, and
/// <see cref="Abi.ACTION_ABORT"/> leaves it, ends the poll, and has the next poll hand it
/// over again.
/// </summary>
public delegate int ServiceHandler(ReadOnlySpan<byte> body, ClusterEntryValues entry, ClusterService service);
'''

CLIENT = HEADING + r'''
using System;
using System.Threading;

namespace Cavalry;

/// <summary>
/// A client of one driver: <c>cavalry_init</c> over a context with this package's ABI
/// version. Every method that reaches the client's handle holds the client's lock, so
/// threads may share a client: an add, a command or a close holds it for its one ABI
/// call, which waits on the driver for up to 10 s, and the lock is never on a message's
/// path (clients/dotnet/README.md, "Threads"). <see cref="Dispose"/> and <see cref="Close"/>
/// are the same and idempotent.
/// </summary>
public sealed class Client : IDisposable
{
    private readonly Lock gate = new();
    private nint handle;
    private readonly string dir;
    private readonly long clientId;

    /// <summary>The driver at <c>CAVALRY_DIR</c>, or at its default directory.</summary>
    public Client() : this(null)
    {
    }

    /// <summary>The driver at <paramref name="dir"/>, or, for null, at <c>CAVALRY_DIR</c> or the
    /// default directory: <c>cavalry_context_init_v</c> with this package's ABI version, then
    /// <c>cavalry_init</c>.</summary>
    public Client(string? dir)
    {
        Abi.CheckAbiVersion(Abi.ABI_VERSION);
        Abi.Check("cavalry_context_init_v", Abi.cavalry_context_init_v(out nint context, Abi.ABI_VERSION));
        nint client;
        try
        {
            if (dir != null)
            {
                Abi.Check("cavalry_context_set_dir", Abi.cavalry_context_set_dir(context, dir));
            }
            Abi.Check("cavalry_init", Abi.cavalry_init(out client, context));
        }
        finally
        {
            Abi.cavalry_context_close(context);
        }
        handle = client;
        this.dir = Abi.String(Abi.cavalry_dir(client)) ?? "";
        clientId = Abi.cavalry_client_id(client);
    }

    /// <summary>The driver's directory.</summary>
    public string Dir => dir;

    /// <summary>The id the driver gave this client.</summary>
    public long ClientId => clientId;

    /// <summary>Whether the client is closed, by <see cref="Close"/> or by the driver.</summary>
    public bool IsClosed()
    {
        lock (gate)
        {
            return handle == 0 || Abi.cavalry_is_closed(handle);
        }
    }

    /// <summary>0 while open, else why the client closed, <c>Abi.CLOSED_*</c>.</summary>
    public int Closed()
    {
        lock (gate)
        {
            return handle == 0 ? Abi.CLOSED_BY_USER : Abi.cavalry_closed(handle);
        }
    }

    /// <summary>Closes the client and every handle it made. Idempotent. The handle is
    /// forgotten whatever the ABI returns, but for one close: a close from inside the handler
    /// of a cluster service whose poll holds this client, which the ABI refuses with
    /// <see cref="Abi.ERR_POLLING"/> and which frees nothing (spec/c-abi.md, "Calls during a
    /// claim or a poll"). It then throws the <see cref="CavalryException"/> and keeps the
    /// handle: the client stays open, and the service's next poll and a close after the poll
    /// has returned use it. A call after a close hands the ABI a null handle, which it
    /// refuses with <see cref="Abi.ERR_NULL"/>.</summary>
    public void Close()
    {
        lock (gate)
        {
            if (handle == 0)
            {
                return;
            }
            int result = Abi.cavalry_close(handle);
            if (result != 0)
            {
                CavalryException error = Abi.LastError("cavalry_close");
                if (error.Code == Abi.ERR_POLLING)
                {
                    throw error;
                }
                handle = 0;
                throw error;
            }
            handle = 0;
        }
    }

    public void Dispose()
    {
        Close();
    }

    /// <summary>Adds a publication on the channel and stream, waiting on the driver for up to 10 s.</summary>
    public Publication AddPublication(string channel, int streamId)
    {
        lock (gate)
        {
            Abi.Check("cavalry_add_publication", Abi.cavalry_add_publication(out nint publication, handle, channel, streamId));
            return new Publication(publication);
        }
    }

    /// <summary>Adds a publication of its own session on the channel and stream.</summary>
    public Publication AddExclusivePublication(string channel, int streamId)
    {
        lock (gate)
        {
            Abi.Check("cavalry_add_exclusive_publication",
                Abi.cavalry_add_exclusive_publication(out nint publication, handle, channel, streamId));
            return new Publication(publication);
        }
    }

    /// <summary>Adds a subscription on the channel and stream.</summary>
    public Subscription AddSubscription(string channel, int streamId)
    {
        lock (gate)
        {
            Abi.Check("cavalry_add_subscription", Abi.cavalry_add_subscription(out nint subscription, handle, channel, streamId));
            return new Subscription(subscription);
        }
    }

    /// <summary>Starts recording the live publication on the channel and stream.</summary>
    public Recording StartRecording(string channel, int streamId)
    {
        lock (gate)
        {
            Abi.Check("cavalry_start_recording", Abi.cavalry_start_recording(out nint recording, handle, channel, streamId));
            return new Recording(recording);
        }
    }

    /// <summary>Adds the destination channel, cavalry:udp?endpoint=&lt;host&gt;:&lt;port&gt;, to
    /// the publication's control channel, answered by the driver (ADR 0053).</summary>
    public void AddDestination(Publication publication, string channel)
    {
        lock (gate)
        {
            Abi.Check("cavalry_add_destination", Abi.cavalry_add_destination(handle, publication.Handle, channel));
        }
    }

    /// <summary>Removes the destination channel, as it was added; one not there is removed by nothing.</summary>
    public void RemoveDestination(Publication publication, string channel)
    {
        lock (gate)
        {
            Abi.Check("cavalry_remove_destination", Abi.cavalry_remove_destination(handle, publication.Handle, channel));
        }
    }

    /// <summary>Stops the recording.</summary>
    public void StopRecording(long recordingId)
    {
        lock (gate)
        {
            Abi.Check("cavalry_stop_recording", Abi.cavalry_stop_recording(handle, recordingId));
        }
    }

    /// <summary>Truncates the recording to the position.</summary>
    public void TruncateRecording(long recordingId, long position)
    {
        lock (gate)
        {
            Abi.Check("cavalry_truncate_recording", Abi.cavalry_truncate_recording(handle, recordingId, position));
        }
    }

    /// <summary>Purges the recording's segments before the new start position.</summary>
    public void PurgeSegments(long recordingId, long newStartPosition)
    {
        lock (gate)
        {
            Abi.Check("cavalry_purge_segments", Abi.cavalry_purge_segments(handle, recordingId, newStartPosition));
        }
    }

    /// <summary>Purges the recording.</summary>
    public void PurgeRecording(long recordingId)
    {
        lock (gate)
        {
            Abi.Check("cavalry_purge_recording", Abi.cavalry_purge_recording(handle, recordingId));
        }
    }

    /// <summary>Replays <paramref name="length"/> bytes of the recording from <paramref name="position"/>
    /// onto the replay stream of the channel; <see cref="Abi.NULL_POSITION"/> and
    /// <see cref="Abi.NULL_LENGTH"/> replay all of it.</summary>
    public Replay StartReplay(long recordingId, long position, long length, int replayStreamId, string channel)
    {
        lock (gate)
        {
            Abi.Check("cavalry_start_replay", Abi.cavalry_start_replay(out nint replay, handle, recordingId, position, length,
                replayStreamId, channel));
            return new Replay(replay);
        }
    }

    /// <summary>Stops the replay session.</summary>
    public void StopReplay(long replaySessionId)
    {
        lock (gate)
        {
            Abi.Check("cavalry_stop_replay", Abi.cavalry_stop_replay(handle, replaySessionId));
        }
    }

    /// <summary>Opens the committed log of the cluster member on this client's driver at
    /// <paramref name="position"/>, 0 or the <see cref="CommittedLog.Position"/> an earlier log
    /// reached. Null while the driver hosts no member that has a recording of its log, so a
    /// service started before its member asks again (spec/cluster.md, "The service";
    /// spec/c-abi.md, "Functions", committed log).</summary>
    public CommittedLog? CommittedLog(long position)
    {
        lock (gate)
        {
            Abi.Check("cavalry_committed_log", Abi.cavalry_committed_log(out nint log, handle, position));
            return log == 0 ? null : new CommittedLog(log, this);
        }
    }

    /// <summary>A committed log's close, under the client's lock: the ABI's result, with the
    /// thread's error for the caller to read. After this client's own close the handle is
    /// null, and the ABI frees the log and leaves the replay to the driver.</summary>
    internal int CloseCommittedLog(nint log)
    {
        lock (gate)
        {
            return Abi.cavalry_committed_log_close(log, handle);
        }
    }

    /// <summary>Opens a session with a cluster: <paramref name="members"/> is the members
    /// string the cluster's members were started with, and
    /// <paramref name="responseChannel"/> a channel of this host for the cluster's answers,
    /// <c>cavalry:udp?endpoint=&lt;host&gt;:&lt;port&gt;</c>. The call returns once the leader
    /// has opened the session, and waits for that for 20 s at most beyond its adds, holding
    /// the client's lock as an add does. Its <see cref="CavalryException"/> has
    /// <see cref="Abi.ERR_CLUSTER_MEMBERS"/>, <see cref="Abi.ERR_CLUSTER_REFUSED"/>,
    /// <see cref="Abi.ERR_CLUSTER_VERSION"/> or <see cref="Abi.ERR_CLUSTER_CONNECT_TIMEOUT"/>,
    /// a fault's code, or the client's or the driver's (spec/cluster.md, "Sessions";
    /// spec/c-abi.md, "Functions", cluster session; ADR 0122).</summary>
    public ClusterSession ConnectCluster(string members, string responseChannel)
    {
        lock (gate)
        {
            Abi.Check("cavalry_connect_cluster", Abi.cavalry_connect_cluster(out nint session, handle, members,
                responseChannel));
            return new ClusterSession(session);
        }
    }

    /// <summary>Makes a cluster's service beside the member on this client's driver. It
    /// sends nothing: the service's first poll opens the member's log. The service's poll
    /// and its close are calls on this client. The poll is on every entry's path and takes
    /// no lock, so from here until the service's close this client is used by the thread
    /// that polls the service and by no other: give the program's other threads a
    /// <c>Client</c> of their own (spec/cluster.md, "The service"; spec/c-abi.md,
    /// "Functions", cluster service, and "Threads"; ADR 0123; ADR 0124).</summary>
    public ClusterService ClusterService()
    {
        lock (gate)
        {
            Abi.Check("cavalry_cluster_service", Abi.cavalry_cluster_service(out nint service, handle));
            return new ClusterService(service, this);
        }
    }

    /// <summary>The client's handle, for a cluster service's poll, which is a call on the
    /// client as well as on the service and takes no lock: null after this client's own
    /// close, which the ABI's poll then refuses.</summary>
    internal nint Handle => handle;

    /// <summary>A cluster service's close, under the client's lock, as a committed log's:
    /// the ABI's result, with the thread's error for the caller to read. After this
    /// client's own close the handle is null, and the ABI frees the service and leaves
    /// what it held to the driver.</summary>
    internal int CloseClusterService(nint service)
    {
        lock (gate)
        {
            return Abi.cavalry_cluster_service_close(service, handle);
        }
    }

    /// <summary>A service's snapshot: a call on this client, which the service's snapshot
    /// writes its recording with, so under this client's lock; the ABI's result, with the
    /// thread's error for the caller to read.</summary>
    internal unsafe int SnapshotClusterService(nint service, byte* state, nuint length)
    {
        lock (gate)
        {
            return Abi.cavalry_cluster_service_snapshot(service, handle, state, length);
        }
    }

    /// <summary>A service's start from its member's snapshot: a call on this client, under
    /// its lock; the ABI's restored code, with the thread's error for the caller to read,
    /// and for <see cref="Abi.CLUSTER_RESTORED_STATE"/> the state, copied here: the bytes
    /// are the handle's until the service's next poll or its close (spec/c-abi.md,
    /// "Lifetimes").</summary>
    internal unsafe int RestoreClusterService(nint service, out byte[]? state)
    {
        lock (gate)
        {
            int restored = Abi.cavalry_cluster_service_restore(service, handle, out byte* bytes, out nuint length);
            state = restored == Abi.CLUSTER_RESTORED_STATE
                ? new ReadOnlySpan<byte>(bytes, checked((int) length)).ToArray()
                : null;
            return restored;
        }
    }
}
'''

PUBLICATION = HEADING + r'''
using System;

namespace Cavalry;

/// <summary>
/// A publication: <see cref="TryClaim"/> and <see cref="Offer"/> return the position after the
/// frame or a code below zero, never throwing; <see cref="Claim"/> is the span over the bytes
/// the last <c>TryClaim</c> reserved, from the one <c>cavalry_buffer_claim_t</c> this
/// publication keeps, empty after <see cref="Commit"/>, <see cref="Abort"/> and
/// <see cref="Close"/>.
/// </summary>
public sealed unsafe class Publication : IDisposable
{
    private nint handle;
    /// <summary>The handle, for the client's own commands that name this publication.</summary>
    internal nint Handle => handle;
    private Abi.cavalry_buffer_claim_t claim;
    private byte* data;
    private int length;
    private readonly string channel;
    private readonly long registrationId;
    private readonly int sessionId;
    private readonly int streamId;
    private readonly long maxPayloadLength;
    private readonly long maxMessageLength;
    private readonly bool exclusive;

    internal Publication(nint handle)
    {
        this.handle = handle;
        Abi.cavalry_publication_constants_t k = default;
        Abi.Check("cavalry_publication_constants", Abi.cavalry_publication_constants(handle, ref k));
        channel = Abi.String(k.channel) ?? "";
        registrationId = k.registration_id;
        sessionId = k.session_id;
        streamId = k.stream_id;
        maxPayloadLength = (long) k.max_payload_length;
        maxMessageLength = (long) k.max_message_length;
        exclusive = k.is_exclusive != 0;
    }

    public string Channel => channel;
    public long RegistrationId => registrationId;
    public int SessionId => sessionId;
    public int StreamId => streamId;
    public long MaxPayloadLength => maxPayloadLength;
    public long MaxMessageLength => maxMessageLength;
    public bool IsExclusive => exclusive;

    /// <summary>Claims <paramref name="length"/> bytes: the position after the frame, with
    /// <see cref="Claim"/> the writable span over them, or a result code below zero, which
    /// leaves the claim as it was. One ABI call, no allocation: the struct is this
    /// publication's field, pinned for the call.</summary>
    public long TryClaim(int length)
    {
        long position = Abi.cavalry_publication_try_claim(handle, (nuint) length, ref claim);
        if (position >= 0)
        {
            data = claim.data;
            this.length = (int) claim.length;
        }
        return position;
    }

    /// <summary>The writable span over the claimed bytes, valid until the commit or abort, or
    /// empty: a span is made over the address and length the ABI filled in, which allocates
    /// nothing.</summary>
    public Span<byte> Claim => data == null ? default : new Span<byte>(data, length);

    /// <summary>Commits the claim: the position after the frame, or -1.</summary>
    public long Commit()
    {
        data = null;
        return Abi.cavalry_buffer_claim_commit(ref claim);
    }

    /// <summary>Aborts the claim, leaving padding: the position after the frame, or -1.</summary>
    public long Abort()
    {
        data = null;
        return Abi.cavalry_buffer_claim_abort(ref claim);
    }

    /// <summary>Sets the claimed frame's reserved value, any long: 0, or -1.</summary>
    public int SetReservedValue(long value)
    {
        return Abi.cavalry_buffer_claim_set_reserved_value(ref claim, value);
    }

    /// <summary>Publishes the message: the position after it, or a code below zero. The ABI
    /// copies the bytes before it returns; the span is pinned for the call and nothing is
    /// allocated.</summary>
    public long Offer(ReadOnlySpan<byte> message)
    {
        fixed (byte* bytes = message)
        {
            return Abi.cavalry_publication_offer(handle, bytes, (nuint) message.Length);
        }
    }

    /// <summary>The publication's position, or <see cref="Abi.NULL_POSITION"/>.</summary>
    public long Position()
    {
        return Abi.cavalry_publication_position(handle);
    }

    /// <summary>The position the publication may publish up to.</summary>
    public long PositionLimit()
    {
        return Abi.cavalry_publication_position_limit(handle);
    }

    /// <summary>Whether a subscriber is connected.</summary>
    public bool IsConnected()
    {
        return Abi.cavalry_publication_is_connected(handle);
    }

    /// <summary>Whether the publication is closed.</summary>
    public bool IsClosed()
    {
        return handle == 0 || Abi.cavalry_publication_is_closed(handle);
    }

    /// <summary>Closes the publication, aborting an outstanding claim. Idempotent; the handle
    /// is forgotten whatever the ABI returns, and a call after it hands the ABI a null handle,
    /// which it refuses with <see cref="Abi.ERR_NULL"/> (<see cref="Abi.PUBLICATION_ERROR"/>
    /// from <see cref="TryClaim"/> and <see cref="Offer"/>). The claim struct is cleared too,
    /// so that <see cref="Commit"/>, <see cref="Abort"/> and <see cref="SetReservedValue"/>
    /// after the close answer -1 with <see cref="Abi.ERR_NO_CLAIM"/> and the ABI reads no
    /// freed handle through the struct's <c>publication</c> (spec/c-abi.md, "Which claim a
    /// struct names").</summary>
    public void Close()
    {
        if (handle == 0)
        {
            return;
        }
        data = null;
        claim = default;
        nint closing = handle;
        handle = 0;
        Abi.Check("cavalry_publication_close", Abi.cavalry_publication_close(closing));
    }

    public void Dispose()
    {
        Close();
    }
}
'''

SUBSCRIPTION = HEADING + r'''
using System;
using System.Collections.Generic;
using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;

namespace Cavalry;

/// <summary>
/// A subscription: <see cref="Poll"/> calls the handler for each fragment and returns the
/// count, throwing a <see cref="HandlerException"/> once the ABI has returned when a handler
/// call threw, every later fragment still having reached the handler. The handler is reached
/// through the one <c>UnmanagedCallersOnly</c> trampoline, which finds the subscription by
/// the <c>GCHandle</c> it hands the ABI as its <c>clientd</c>.
/// </summary>
public sealed unsafe class Subscription : IDisposable
{
    /// <summary>What the last poll stopped on: the code (0 for none), the session and the position.</summary>
    public readonly record struct Fault(int Code, int SessionId, long Position);

    private nint handle;
    private GCHandle self;
    private readonly Header header = new();
    private FragmentHandler? handler;
    private bool polling;
    private int raised;
    private Exception? first;
    private readonly string channel;
    private readonly long registrationId;
    private readonly int streamId;

    internal Subscription(nint handle)
    {
        this.handle = handle;
        self = GCHandle.Alloc(this);
        Abi.cavalry_subscription_constants_t k = default;
        try
        {
            Abi.Check("cavalry_subscription_constants", Abi.cavalry_subscription_constants(handle, ref k));
        }
        catch
        {
            self.Free();
            throw;
        }
        channel = Abi.String(k.channel) ?? "";
        registrationId = k.registration_id;
        streamId = k.stream_id;
    }

    public string Channel => channel;
    public long RegistrationId => registrationId;
    public int StreamId => streamId;

    /// <summary>Delivers up to <paramref name="fragmentLimit"/> fragments to the handler: one ABI
    /// call, the count it returned (or -1, with <see cref="Abi.Errcode"/> saying why:
    /// <see cref="Abi.ERR_POLLING"/> for a poll from inside a handler of this subscription,
    /// which the ABI refuses and which touches nothing here), and a
    /// <see cref="HandlerException"/> when a handler call threw. After a close, the ABI is
    /// handed a null handle and answers -1 with <see cref="Abi.ERR_NULL"/>.</summary>
    public int Poll(FragmentHandler handler, int fragmentLimit)
    {
        if (polling)
        {
            return Abi.cavalry_subscription_poll(handle, &OnFragment, GCHandle.ToIntPtr(self), (nuint) fragmentLimit);
        }
        this.handler = handler;
        polling = true;
        raised = 0;
        first = null;
        int count = Abi.cavalry_subscription_poll(handle, &OnFragment, GCHandle.ToIntPtr(self), (nuint) fragmentLimit);
        polling = false;
        this.handler = null;
        if (raised == 0)
        {
            return count;
        }
        Exception cause = first!;
        first = null;
        throw new HandlerException(count, raised, cause);
    }

    /// <summary>The trampoline the ABI reaches, once per fragment: the subscription from the
    /// <c>GCHandle</c> in <paramref name="clientd"/>, then <see cref="Deliver"/>. An exception
    /// must not unwind through an <c>UnmanagedCallersOnly</c> frame, where the runtime would
    /// end the process, so every one is caught there (clients/dotnet/README.md, "The exception").</summary>
    [UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]
    private static void OnFragment(void* clientd, byte* buffer, nuint length, void* frame)
    {
        ((Subscription) GCHandle.FromIntPtr((nint) clientd).Target!).Deliver(buffer, length, frame);
    }

    /// <summary>The body as a span over the log's bytes, no copy, the subscription's one
    /// <see cref="Header"/> pointed at the frame, and the handler called inside a catch that
    /// keeps its first exception and counts every one, so that every fragment of the poll
    /// reaches the handler.</summary>
    private void Deliver(byte* buffer, nuint length, void* frame)
    {
        try
        {
            header.Set((nint) frame);
            handler!(new ReadOnlySpan<byte>(buffer, (int) length), header);
        }
        catch (Exception e)
        {
            raised++;
            first ??= e;
        }
        finally
        {
            header.Clear();
        }
    }

    /// <summary>What the last poll stopped on.</summary>
    public Fault GetFault()
    {
        Abi.cavalry_fault_t f = default;
        Abi.Check("cavalry_subscription_fault", Abi.cavalry_subscription_fault(handle, ref f));
        return new Fault(f.code, f.session_id, f.position);
    }

    /// <summary>How many images the subscription has.</summary>
    public int ImageCount()
    {
        return Abi.cavalry_subscription_image_count(handle);
    }

    /// <summary>The image at the index, with its constants copied now, or null; valid until the
    /// next poll or close.</summary>
    public Image? ImageAt(int index)
    {
        nint pointer = Abi.cavalry_subscription_image_at_index(handle, (nuint) index);
        return pointer == 0 ? null : new Image(pointer);
    }

    /// <summary>Every image, valid until the next poll or close.</summary>
    public List<Image> Images()
    {
        int count = ImageCount();
        List<Image> images = new(count);
        for (int i = 0; i < count; i++)
        {
            Image? image = ImageAt(i);
            if (image != null)
            {
                images.Add(image);
            }
        }
        return images;
    }

    /// <summary>Whether a publisher is connected.</summary>
    public bool IsConnected()
    {
        return Abi.cavalry_subscription_is_connected(handle);
    }

    /// <summary>Whether the subscription is closed.</summary>
    public bool IsClosed()
    {
        return handle == 0 || Abi.cavalry_subscription_is_closed(handle);
    }

    /// <summary>Closes the subscription and frees its <c>GCHandle</c>. Idempotent. A close from
    /// inside a handler of this subscription's own poll is refused by the ABI with
    /// <see cref="Abi.ERR_POLLING"/> and leaves the subscription open (spec/c-abi.md, "Calls
    /// during a claim or a poll"): it then throws the <see cref="CavalryException"/> and keeps
    /// the handle and the <c>GCHandle</c>. After a close, <see cref="Poll"/> hands the ABI a
    /// null handle, which it refuses with <see cref="Abi.ERR_NULL"/>.</summary>
    public void Close()
    {
        if (handle == 0)
        {
            return;
        }
        int result = Abi.cavalry_subscription_close(handle);
        if (result != 0)
        {
            CavalryException error = Abi.LastError("cavalry_subscription_close");
            if (error.Code == Abi.ERR_POLLING)
            {
                throw error;
            }
            handle = 0;
            self.Free();
            throw error;
        }
        handle = 0;
        self.Free();
    }

    public void Dispose()
    {
        Close();
    }
}
'''

COMMITTED_LOG = HEADING + r'''
using System;
using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;

namespace Cavalry;

/// <summary>
/// A service's read of a cluster's committed log, beside the member on the client's driver
/// (spec/cluster.md, "The service"): <see cref="Poll"/> calls the handler for each entry a
/// quorum has committed, in order and none at or past the member's commit position, with the
/// entry's position in the log, and returns the count, or the ABI's -1 when the poll stopped
/// on a fault, which <see cref="GetFault"/> reads until the next poll, throwing a
/// <see cref="HandlerException"/> once the ABI has returned when a handler call threw; the
/// entry was delivered and is not delivered again. <see cref="Position"/> is the position
/// after the last entry delivered: where a service opens the log again.
/// <see cref="IsEnded"/> says when to stop reading and what tells a whole end from a short
/// one. The handler is
/// reached through an <c>UnmanagedCallersOnly</c> trampoline of the log's own, which finds
/// the log by the <c>GCHandle</c> it hands the ABI as its <c>clientd</c>.
/// </summary>
public sealed unsafe class CommittedLog : IDisposable
{
    private nint handle;
    private GCHandle self;
    private readonly Client client;
    private readonly Header header = new();
    private EntryHandler? handler;
    private bool polling;
    private int raised;
    private Exception? first;
    private readonly int memberId;
    private readonly long recordingId;

    internal CommittedLog(nint handle, Client client)
    {
        this.handle = handle;
        this.client = client;
        self = GCHandle.Alloc(this);
        Abi.cavalry_committed_log_constants_t k = default;
        try
        {
            Abi.Check("cavalry_committed_log_constants", Abi.cavalry_committed_log_constants(handle, ref k));
        }
        catch
        {
            // The log is open and no object will hold it: it is closed here, as the Go
            // client closes one it cannot return. The exception already holds the
            // constants' error, read before this close set the thread's own, and it is
            // the one to report whatever the close does.
            try
            {
                client.CloseCommittedLog(handle);
            }
            catch
            {
            }
            this.handle = 0;
            self.Free();
            throw;
        }
        memberId = k.member_id;
        recordingId = k.recording_id;
    }

    /// <summary>The id of the member whose log this is.</summary>
    public int MemberId => memberId;

    /// <summary>The id of the member's recording of the log that this handle replays.</summary>
    public long RecordingId => recordingId;

    /// <summary>Delivers up to <paramref name="fragmentLimit"/> entries to the handler: one ABI
    /// call, the count it returned (or -1, with <see cref="Abi.Errcode"/> saying why: a
    /// fault, which <see cref="GetFault"/> reads until the next poll, or
    /// <see cref="Abi.ERR_POLLING"/> for a poll from inside a handler of this log, which the ABI
    /// refuses and which touches nothing here), and a <see cref="HandlerException"/> when a
    /// handler call threw, whose <c>Fragments</c> is that count, -1 included. A fault code of
    /// <see cref="Abi.ERR_FAULT_IMAGE"/> or <see cref="Abi.ERR_FAULT_IMAGES_FULL"/> is the one
    /// report that the replay's image failed a check or could not join: the log is ended
    /// short of its entries, and the next poll returns 0 and clears the fault here;
    /// <see cref="Faulted"/> keeps it. An entry whose handler call threw was delivered and
    /// is not delivered again: <see cref="Position"/> is past it, and past every entry this
    /// poll delivered after it. After a close, the ABI is handed a null handle and answers
    /// -1 with <see cref="Abi.ERR_NULL"/>.</summary>
    public int Poll(EntryHandler handler, int fragmentLimit)
    {
        if (polling)
        {
            return Abi.cavalry_committed_log_poll(handle, &OnEntry, GCHandle.ToIntPtr(self), (nuint) fragmentLimit);
        }
        this.handler = handler;
        polling = true;
        raised = 0;
        first = null;
        int count = Abi.cavalry_committed_log_poll(handle, &OnEntry, GCHandle.ToIntPtr(self), (nuint) fragmentLimit);
        polling = false;
        this.handler = null;
        if (raised == 0)
        {
            return count;
        }
        Exception cause = first!;
        first = null;
        throw new HandlerException(count, raised, cause, committedLog: true);
    }

    /// <summary>The trampoline the ABI reaches, once per entry: the log from the
    /// <c>GCHandle</c> in <paramref name="clientd"/>, then <see cref="Deliver"/>, which catches
    /// every exception, since none may unwind through an <c>UnmanagedCallersOnly</c> frame
    /// (clients/dotnet/README.md, "The exception").</summary>
    [UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]
    private static void OnEntry(void* clientd, byte* buffer, nuint length, void* frame)
    {
        ((CommittedLog) GCHandle.FromIntPtr((nint) clientd).Target!).Deliver(buffer, length, frame);
    }

    /// <summary>The body as a span over the log's bytes, no copy, the log's one
    /// <see cref="Header"/> pointed at the frame, the entry's position, which the ABI gives
    /// for the header its own poll handed over, and the handler called inside a catch that
    /// keeps its first exception and counts every one, so that every entry of the poll
    /// reaches the handler.</summary>
    private void Deliver(byte* buffer, nuint length, void* frame)
    {
        try
        {
            header.Set((nint) frame);
            handler!(new ReadOnlySpan<byte>(buffer, (int) length), header,
                Abi.cavalry_committed_log_entry_position((nint) frame));
        }
        catch (Exception e)
        {
            raised++;
            first ??= e;
        }
        finally
        {
            header.Clear();
        }
    }

    /// <summary>What the last poll stopped on; its code is 0 when the last poll returned a
    /// count. A poll that returns a count clears it, so read it after a poll that returned
    /// -1 and before the next.</summary>
    public Subscription.Fault GetFault()
    {
        Abi.cavalry_fault_t f = default;
        Abi.Check("cavalry_committed_log_fault", Abi.cavalry_committed_log_fault(handle, ref f));
        return new Subscription.Fault(f.code, f.session_id, f.position);
    }

    /// <summary>The position after the last entry delivered, or the position the log was
    /// opened at before the first; -1 from inside a handler of this log's own poll.</summary>
    public long Position()
    {
        return Abi.cavalry_committed_log_position(handle);
    }

    /// <summary>The bound of the last poll: the highest commit position taken from the
    /// member's counter, 0 before the first poll.</summary>
    public long CommitPosition()
    {
        return Abi.cavalry_committed_log_commit_position(handle);
    }

    /// <summary>Whether this log has come to its end: no poll will deliver another entry.
    /// Stop reading when it is true, not when a poll returns 0. The log delivered every entry
    /// it was owed only when its three reports are all null: <see cref="FailedAt"/>,
    /// <see cref="WithdrawnAt"/> and <see cref="Faulted"/>. In every case what the log
    /// delivered ends at <see cref="Position"/>, where a service opens the log again.</summary>
    public bool IsEnded()
    {
        return Abi.cavalry_committed_log_is_ended(handle);
    }

    /// <summary>The position after the last frame the driver's replay of the member's
    /// recording played before it failed, or null for a log that is live, that ended
    /// because its member closed or left the recording, or that ended whole at the stop of
    /// a recording below the member's live one. No entry from that position on is
    /// delivered. The entries below it may still be on their way when this first is a
    /// position, so read it once <see cref="IsEnded"/> is true. They have all been delivered
    /// by then unless the driver withdrew the replay first, which it does 5 s after a replay
    /// ends, one that failed included: <see cref="WithdrawnAt"/> is then a position below
    /// this one, and the entries from there on, those below this position among them, were
    /// not delivered either. What the log delivered ends at <see cref="Position"/>.</summary>
    public long? FailedAt()
    {
        return Abi.cavalry_committed_log_failed_at(handle, out long position) ? position : null;
    }

    /// <summary>The position at which the entries this log was not given begin, when the
    /// driver withdrew the replay before the polls had read it to its end, or null. The
    /// driver keeps a replay 5 s after it ends and then withdraws it, read or not: a service
    /// that did not poll for that long after its member closed or left the log, or whose
    /// handler took that long, ends so, and so does a log on a recording below the
    /// member's live one while the member's commit position stays below that recording's
    /// stop for that long. The entries from that position on were played and not
    /// delivered. A replay that failed is withdrawn in the same way: <see cref="FailedAt"/>
    /// is then a position too, and this one is the lower. Read it once
    /// <see cref="IsEnded"/> is true.</summary>
    public long? WithdrawnAt()
    {
        return Abi.cavalry_committed_log_withdrawn_at(handle, out long position) ? position : null;
    }

    /// <summary>The fault of the log's own image, the replay's, or null for a log whose image
    /// has not faulted. Its code is <see cref="Abi.ERR_FAULT_IMAGE"/> when a frame of the
    /// image failed a check, with the reader's position, or when the image was gone before
    /// this client read it, with <see cref="Abi.NULL_POSITION"/>; it is
    /// <see cref="Abi.ERR_FAULT_IMAGES_FULL"/> when the image could not join. The poll that
    /// meets the fault returns -1 once, <see cref="GetFault"/> answers it until the next
    /// poll, and this answers it from that poll on, for as long as the log is held: a
    /// service that did not keep the poll's answer reads it here, once
    /// <see cref="IsEnded"/> is true, and tells this end from a whole one. No entry from the
    /// fault on was delivered. <see cref="WithdrawnAt"/> is null for such a log, and
    /// <see cref="FailedAt"/> is a position only when the driver's replay failed as well.
    /// From inside a
    /// handler of this log's own poll the ABI refuses the call with
    /// <see cref="Abi.ERR_POLLING"/>, and this is null, a refusal and not an answer.</summary>
    public Subscription.Fault? Faulted()
    {
        Abi.cavalry_fault_t f = default;
        if (!Abi.cavalry_committed_log_faulted(handle, ref f))
        {
            return null;
        }
        return new Subscription.Fault(f.code, f.session_id, f.position);
    }

    /// <summary>Stops the replay through the client and frees the handle and its
    /// <c>GCHandle</c>. Idempotent. After the client's own close it frees the handle alone,
    /// and the driver ends the replay by itself. The handle is freed whatever the ABI
    /// answers. When the ABI could not stop the replay, through a client that the driver or a
    /// timeout closed or within the response timeout, this throws the
    /// <see cref="CavalryException"/> after the handle and the <c>GCHandle</c> are freed:
    /// the log is closed, a second close does nothing, and the driver ends the replay by
    /// itself. The one exception is a close from inside a handler of this log's
    /// own poll, which the ABI refuses with <see cref="Abi.ERR_POLLING"/> and which leaves
    /// the log open (spec/c-abi.md, "Calls during a claim or a poll"): it then throws the
    /// <see cref="CavalryException"/> and keeps the handle and the <c>GCHandle</c>.</summary>
    public void Close()
    {
        if (handle == 0)
        {
            return;
        }
        int result = client.CloseCommittedLog(handle);
        if (result != 0)
        {
            CavalryException error = Abi.LastError("cavalry_committed_log_close");
            if (error.Code == Abi.ERR_POLLING)
            {
                throw error;
            }
            handle = 0;
            self.Free();
            throw error;
        }
        handle = 0;
        self.Free();
    }

    public void Dispose()
    {
        Close();
    }
}
'''

CLUSTER_SESSION = HEADING + r'''
using System;
using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;

namespace Cavalry;

/// <summary>
/// A client's session with a cluster, made by <see cref="Client.ConnectCluster"/>
/// (spec/cluster.md, "Sessions"): <see cref="Offer"/> sends the leader a message of one frame
/// and returns the position after it or a result code below zero, never throwing for a code;
/// <see cref="Poll"/> calls the handler for each reply that is not stale, with the reply's
/// values, and returns the count, throwing a <see cref="HandlerException"/> once the ABI has
/// returned when a handler call threw. The poll is the session's duty cycle too: it sends
/// the keepalive, follows a new leader, and closes the session when the cluster says so, so
/// call it at least every 500 ms. A message taken is on its way to the leader and not
/// committed, and a reply is no delivery: a program that needs an answer offers again when
/// none comes. The handler is reached through an <c>UnmanagedCallersOnly</c> trampoline of
/// the session's own, which finds the session by the <c>GCHandle</c> it hands the ABI as its
/// <c>clientd</c>.
/// </summary>
public sealed unsafe class ClusterSession : IDisposable
{
    private nint handle;
    private GCHandle self;
    private readonly ClusterReplyValues reply = new();
    private ReplyHandler? handler;
    private bool polling;
    private int raised;
    private Exception? first;
    private readonly long sessionId;
    private readonly long maxMessageLength;

    internal ClusterSession(nint handle)
    {
        this.handle = handle;
        self = GCHandle.Alloc(this);
        Abi.cavalry_cluster_session_constants_t k = default;
        try
        {
            Abi.Check("cavalry_cluster_session_constants", Abi.cavalry_cluster_session_constants(handle, ref k));
        }
        catch
        {
            // The session is open and no object will hold it: it is closed here, as a
            // committed log that cannot be returned is. The exception already holds the
            // constants' error, read before this close set the thread's own, and it is
            // the one to report whatever the close does.
            Abi.cavalry_cluster_session_close(handle);
            this.handle = 0;
            self.Free();
            throw;
        }
        sessionId = k.session_id;
        maxMessageLength = (long) k.max_message_length;
    }

    /// <summary>The id the leader gave the session.</summary>
    public long SessionId => sessionId;

    /// <summary>The longest message <see cref="Offer"/> takes: what one frame carries.</summary>
    public long MaxMessageLength => maxMessageLength;

    /// <summary>Offers the leader the message: the position after it on the session's
    /// publication to the leader, or a code below zero. <see cref="Abi.PUBLICATION_ERROR"/>
    /// with <see cref="Abi.ERR_TOO_LONG"/> as <see cref="Abi.Errcode"/> is a message longer
    /// than <see cref="MaxMessageLength"/>, and <see cref="Abi.PUBLICATION_CLOSED"/> a session
    /// that is closed, for the reason <see cref="Closed"/> gives, or whose client is. From
    /// inside this session's own handler the ABI refuses the call with
    /// <see cref="Abi.PUBLICATION_ERROR"/> and <see cref="Abi.ERR_POLLING"/>: keep what the
    /// answer needs and offer after the poll has returned. One ABI call, which copies the
    /// bytes before it returns; the span is pinned for the call and nothing is
    /// allocated.</summary>
    public long Offer(ReadOnlySpan<byte> message)
    {
        fixed (byte* bytes = message)
        {
            return Abi.cavalry_cluster_session_offer(handle, bytes, (nuint) message.Length);
        }
    }

    /// <summary>Reads up to <paramref name="fragmentLimit"/> fragments of the response channel
    /// and hands the handler each reply that is not stale: one ABI call, the count of replies
    /// it handed over (or -1, with <see cref="Abi.Errcode"/> saying why: a fault, which
    /// <see cref="GetFault"/> reads until the next poll, or <see cref="Abi.ERR_POLLING"/> for
    /// a poll from inside a handler of this session, which the ABI refuses and which touches
    /// nothing here), and a <see cref="HandlerException"/> when a handler call threw, whose
    /// <c>Cluster</c> is "session". A stale reply, one of a leadership term below that of a
    /// reply this session has handed over, is passed over and counted in
    /// <see cref="StaleReplies"/>, and is not in the count. Every reply the ABI hands over
    /// reaches the handler, whether or not an earlier call threw.</summary>
    public int Poll(ReplyHandler handler, int fragmentLimit)
    {
        if (polling)
        {
            return Abi.cavalry_cluster_session_poll(handle, &OnReply, GCHandle.ToIntPtr(self), (nuint) fragmentLimit);
        }
        this.handler = handler;
        polling = true;
        raised = 0;
        first = null;
        int count = Abi.cavalry_cluster_session_poll(handle, &OnReply, GCHandle.ToIntPtr(self), (nuint) fragmentLimit);
        polling = false;
        this.handler = null;
        if (raised == 0)
        {
            return count;
        }
        Exception cause = first!;
        first = null;
        throw new HandlerException(count, raised, cause, cluster: "session");
    }

    /// <summary>The trampoline the ABI reaches, once per reply: the session from the
    /// <c>GCHandle</c> in <paramref name="clientd"/>, then <see cref="Deliver"/>, which catches
    /// every exception, since none may unwind through an <c>UnmanagedCallersOnly</c> frame
    /// (clients/dotnet/README.md, "The exception").</summary>
    [UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]
    private static void OnReply(void* clientd, byte* buffer, nuint length, void* frame)
    {
        ((ClusterSession) GCHandle.FromIntPtr((nint) clientd).Target!).Deliver(buffer, length, frame);
    }

    /// <summary>The reply's values read into the one object this session keeps, the body as a
    /// span over the log's bytes, no copy, and the handler called inside a catch that keeps
    /// its first exception and counts every one, so that every reply of the poll reaches the
    /// handler. The values are zero if the ABI refuses the header, which on the header its
    /// own poll handed over it cannot.</summary>
    private void Deliver(byte* buffer, nuint length, void* frame)
    {
        try
        {
            if (Abi.cavalry_cluster_reply_values((nint) frame, ref reply.values) != 0)
            {
                reply.values = default;
            }
            handler!(new ReadOnlySpan<byte>(buffer, (int) length), reply);
        }
        catch (Exception e)
        {
            raised++;
            first ??= e;
        }
    }

    /// <summary>What the last poll stopped on; its code is 0 when the last poll returned a
    /// count.</summary>
    public Subscription.Fault GetFault()
    {
        Abi.cavalry_fault_t f = default;
        Abi.Check("cavalry_cluster_session_fault", Abi.cavalry_cluster_session_fault(handle, ref f));
        return new Subscription.Fault(f.code, f.session_id, f.position);
    }

    /// <summary>The leadership term the session sends with: the term of the leader that
    /// opened it, or of the last new leader event; -1 from inside this session's own
    /// handler, which has each reply's own term.</summary>
    public long LeadershipTermId()
    {
        return Abi.cavalry_cluster_session_leadership_term_id(handle);
    }

    /// <summary>The id of the member the session sends to: the leader, as the cluster last
    /// said; -1 from inside this session's own handler.</summary>
    public int LeaderMemberId()
    {
        return Abi.cavalry_cluster_session_leader_member_id(handle);
    }

    /// <summary>How many stale replies this session's polls have passed over: the value of
    /// its counter <c>cluster.replies_stale</c>; -1 from inside this session's own
    /// handler.</summary>
    public long StaleReplies()
    {
        return Abi.cavalry_cluster_session_stale_replies(handle);
    }

    /// <summary>Why the session is closed, or null while it is open: "TIMEOUT" when the
    /// leader closed it for hearing nothing from it for the session timeout, "NO LEADER"
    /// when its leader was lost and no member took its place, or the detail of the
    /// cluster's closed event. A poll is what closes a session, so the answer changes only
    /// across one. From inside this session's own handler the ABI refuses the call with
    /// <see cref="Abi.ERR_POLLING"/>, and this is null, a refusal and not an answer; it is
    /// null too once <see cref="Close"/> has freed the session.</summary>
    public string? Closed()
    {
        return Abi.String(Abi.cavalry_cluster_session_closed(handle));
    }

    /// <summary>Offers the leader a session close for at most 500 ms, unless the session is
    /// closed already, then closes the session's publications, its subscription and its
    /// counter and frees the handle. Idempotent. Close a session before its client. The
    /// handle is freed whatever the ABI answers, so when the ABI's close failed this throws
    /// the <see cref="CavalryException"/> after the handle is freed. The one exception is a
    /// close from inside this session's own handler, which the ABI refuses with
    /// <see cref="Abi.ERR_POLLING"/> (spec/c-abi.md, "Calls during a claim or a poll"): it
    /// then throws the <see cref="CavalryException"/>, sends nothing, and keeps the
    /// handle.</summary>
    public void Close()
    {
        if (handle == 0)
        {
            return;
        }
        int result = Abi.cavalry_cluster_session_close(handle);
        if (result != 0)
        {
            CavalryException error = Abi.LastError("cavalry_cluster_session_close");
            if (error.Code == Abi.ERR_POLLING)
            {
                throw error;
            }
            handle = 0;
            self.Free();
            throw error;
        }
        handle = 0;
        self.Free();
    }

    public void Dispose()
    {
        Close();
    }
}
'''

CLUSTER_REPLY_VALUES = HEADING + r'''
namespace Cavalry;

/// <summary>
/// What a reply carries beside its payload, as <c>cavalry_cluster_reply_values</c> reads it.
/// It is the one object a cluster session keeps, over the one struct the session's trampoline
/// fills for each reply before the handler runs, so copy a value out to keep it.
/// </summary>
public sealed class ClusterReplyValues
{
    internal Abi.cavalry_cluster_reply_values_t values;

    internal ClusterReplyValues()
    {
    }

    /// <summary>The leadership term of the entry the reply answers.</summary>
    public long LeadershipTermId => values.leadership_term_id;

    /// <summary>The cluster's time when its leader appended that entry, in milliseconds since
    /// 1970-01-01T00:00:00Z.</summary>
    public long Timestamp => values.timestamp;
}
'''

CLUSTER_SERVICE = HEADING + r'''
using System;
using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;

namespace Cavalry;

/// <summary>
/// A cluster's service beside the member on the client's driver, made by
/// <see cref="Client.ClusterService"/> (spec/cluster.md, "The service" and "Replies"):
/// <see cref="Poll"/> calls the handler for each entry a quorum has committed, in order, with
/// the entry's values and this service, and what the handler returns is the entry's action.
/// A handler that throws, or returns what is no action, has its entry aborted, and
/// <c>Poll</c> throws a <see cref="HandlerException"/> once the ABI has returned: an entry
/// is taken only by a handler that finished with it. <see cref="Reply"/> answers a session
/// from inside the handler. The handler is reached through an <c>UnmanagedCallersOnly</c>
/// trampoline of the service's own, which finds the service by the <c>GCHandle</c> it hands
/// the ABI as its <c>clientd</c>.
/// </summary>
public sealed unsafe class ClusterService : IDisposable
{
    private nint handle;
    private GCHandle self;
    private readonly Client client;
    private readonly ClusterEntryValues entry = new();
    private ServiceHandler? handler;
    private bool polling;
    private int raised;
    private Exception? first;
    private bool refused;
    private int refusedCode;
    private string? refusedMessage;

    internal ClusterService(nint handle, Client client)
    {
        this.handle = handle;
        this.client = client;
        self = GCHandle.Alloc(this);
    }

    /// <summary>Sends the message, from inside the handler, to the session with
    /// <paramref name="sessionId"/>, with the leadership term id and the cluster's time of
    /// the entry being handled, and returns what became of the reply,
    /// <c>Abi.CLUSTER_REPLY_*</c>, never throwing for a code. <c>_SENT</c>: it is on its way,
    /// or an earlier handling of the same entry sent it. <c>_NOT_ANSWERING</c>: this service
    /// does not answer the entry; its member did not lead the entry's term, or the entry is
    /// history. <c>_RETRY</c>: the session's response channel did not take it now; return
    /// <see cref="Abi.ACTION_ABORT"/>, and the next poll hands the entry over again.
    /// <c>_DROPPED</c>: it is lost, and counted in <c>cluster.replies_dropped</c>.
    /// <c>_NO_SESSION</c>: no session with that id is open in the log at this entry.
    /// <c>_TOO_LONG</c>: it is longer than one frame carries, with
    /// <see cref="Abi.ERR_TOO_LONG"/> as <see cref="Abi.Errcode"/>. Reply first, then change
    /// the service's state, since an aborted entry is handed over again. From outside the
    /// handler it is -1, with <see cref="Abi.ERR_CLUSTER_NO_ENTRY"/>. One ABI call, which
    /// allocates nothing and waits for nothing; the span is pinned for the call.</summary>
    public int Reply(long sessionId, ReadOnlySpan<byte> message)
    {
        fixed (byte* bytes = message)
        {
            return Abi.cavalry_cluster_service_reply(handle, sessionId, bytes, (nuint) message.Length);
        }
    }

    /// <summary>Hands up to <paramref name="fragmentLimit"/> committed entries to the handler:
    /// one ABI call, the count of entries the handler took, 0 while the driver hosts no
    /// member that has a log yet (or -1, with <see cref="Abi.Errcode"/> saying why: a fault,
    /// which <see cref="GetFault"/> reads until the next poll, or
    /// <see cref="Abi.ERR_POLLING"/> for a poll from inside a handler of this service, which
    /// the ABI refuses and which touches nothing here). The poll is the service's duty
    /// cycle: it opens the member's log, follows the log's sessions, and holds a publication
    /// to each open session while the service answers. Those are commands sent with the
    /// service's client, at a session's opening, where a term's leader changes and at a
    /// recording's end, and the poll waits for the driver's answer to each; none is sent for
    /// a session message. The poll takes no lock, and it is a call on the client: no other
    /// thread uses the service's client (<see cref="Client.ClusterService"/>). When the
    /// handler threw, or returned what is no action, its entry was aborted and this throws a
    /// <see cref="HandlerException"/> whose <c>Cluster</c> is "service": its
    /// <c>Fragments</c> is the count of entries taken before that entry, and its
    /// <c>InnerException</c> what the handler threw, or a <see cref="CavalryException"/> with
    /// <see cref="Abi.ERR_CLUSTER_ACTION"/> and the ABI's message for the value the handler
    /// returned. The next poll hands the aborted entry over again.</summary>
    public int Poll(ServiceHandler handler, int fragmentLimit)
    {
        if (polling)
        {
            return Abi.cavalry_cluster_service_poll(handle, client.Handle, &OnClusterEntry, GCHandle.ToIntPtr(self),
                (nuint) fragmentLimit);
        }
        this.handler = handler;
        polling = true;
        raised = 0;
        first = null;
        refused = false;
        int count = Abi.cavalry_cluster_service_poll(handle, client.Handle, &OnClusterEntry, GCHandle.ToIntPtr(self),
            (nuint) fragmentLimit);
        polling = false;
        this.handler = null;
        if (raised == 0 && !refused)
        {
            return count;
        }
        // The ABI refused what the handler returned when no exception was kept: its error,
        // read in the trampoline when the call failed.
        Exception cause = first ?? new CavalryException("cavalry_cluster_service_action", refusedCode, refusedMessage ?? "");
        first = null;
        refused = false;
        refusedMessage = null;
        throw new HandlerException(count, 1, cause, cluster: "service");
    }

    /// <summary>The trampoline the ABI reaches, once per entry: the service from the
    /// <c>GCHandle</c> in <paramref name="clientd"/>, then <see cref="Deliver"/>, which catches
    /// every exception, since none may unwind through an <c>UnmanagedCallersOnly</c> frame
    /// (clients/dotnet/README.md, "The exception").</summary>
    [UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]
    private static void OnClusterEntry(void* clientd, byte* buffer, nuint length, void* frame)
    {
        ((ClusterService) GCHandle.FromIntPtr((nint) clientd).Target!).Deliver(buffer, length, frame);
    }

    /// <summary>The entry's values read into the one object this service keeps, the body as a
    /// span over the log's bytes, no copy, and empty for an entry that is no session's
    /// message, and the handler called inside a catch that keeps what it threw. What the
    /// handler returned is the entry's action, passed to the ABI unless it is
    /// <see cref="Abi.ACTION_CONTINUE"/>, which is what the ABI does with no call. A handler
    /// that threw has its entry aborted, and so has one whose action the ABI refused, whose
    /// error is kept here, in the thread that made the call, for <see cref="Poll"/> to
    /// throw: the ABI ends the poll at an abort with the entry not taken, so that a service
    /// never passes an entry its handler did not finish (clients/dotnet/README.md, "The
    /// cluster service"). The values are zero if the ABI refuses the header, which on the
    /// header its own poll handed over it cannot.</summary>
    private void Deliver(byte* buffer, nuint length, void* frame)
    {
        int action = Abi.ACTION_ABORT;
        try
        {
            if (Abi.cavalry_cluster_entry_values((nint) frame, ref entry.values) != 0)
            {
                entry.values = default;
            }
            action = handler!(new ReadOnlySpan<byte>(buffer, (int) length), entry, this);
        }
        catch (Exception e)
        {
            raised++;
            first ??= e;
        }
        if (action != Abi.ACTION_CONTINUE && Abi.cavalry_cluster_service_action(handle, action) != 0)
        {
            refused = true;
            refusedCode = Abi.Errcode();
            refusedMessage = Abi.Errmsg();
            Abi.cavalry_cluster_service_action(handle, Abi.ACTION_ABORT);
        }
    }

    /// <summary>What the last poll stopped on; its code is 0 when the last poll returned a
    /// count. The code is a fault's, <see cref="Abi.ERR_CLUSTER_ENTRY"/> with the position of
    /// a committed entry that is no entry of this version's cluster protocol, or the
    /// client's or the driver's.</summary>
    public Subscription.Fault GetFault()
    {
        Abi.cavalry_fault_t f = default;
        Abi.Check("cavalry_cluster_service_fault", Abi.cavalry_cluster_service_fault(handle, ref f));
        return new Subscription.Fault(f.code, f.session_id, f.position);
    }

    /// <summary>The position after the last entry the handler took, 0 before the first: how
    /// far the service has applied the log; -1 from inside this service's own handler, which
    /// has each entry's own position.</summary>
    public long Position()
    {
        return Abi.cavalry_cluster_service_position(handle);
    }

    /// <summary>Whether the service answered the last entry it read: its member led that
    /// entry's term and, as the entry was read, led it still, and the entry was not history.
    /// False from inside this service's own handler, a refusal and not an answer: there,
    /// what <see cref="Reply"/> returns says whether the service answers the entry.</summary>
    public bool Answers()
    {
        return Abi.cavalry_cluster_service_answers(handle);
    }

    /// <summary>How many sessions the log holds open, as far as the service has read: 10 at
    /// most; -1 from inside this service's own handler.</summary>
    public int Sessions()
    {
        return Abi.cavalry_cluster_service_sessions(handle);
    }

    /// <summary>The log position of the snapshot the leader asked for, when the last entry of
    /// the last poll was <see cref="Abi.CLUSTER_ENTRY_SNAPSHOT"/> and the handler took it:
    /// the position after that entry, which <see cref="Position"/> is then. Null otherwise,
    /// and from the next poll on. Null too from inside this service's own handler, where
    /// the ABI refuses the read with <see cref="Abi.ERR_POLLING"/>: the entry's type says
    /// that the poll ends after it (spec/c-abi.md, "A service's snapshot and its start";
    /// ADR 0149).</summary>
    public long? SnapshotDue()
    {
        return Abi.cavalry_cluster_service_snapshot_due(handle, out long position) ? position : null;
    }

    /// <summary>Takes a snapshot of the program's state: the program's own bytes, in which
    /// the service reads nothing, 1 GiB at the most. Call it after a poll that ended at the
    /// leader's request, while <see cref="SnapshotDue"/> gives a position, and before the
    /// next poll. The service writes a recording through its member's driver and tells the
    /// member, which lists the snapshot; the call waits for the driver, a second at the
    /// most for each step of its own and the response timeout, 10 s, for an add or a command that a driver does not answer,
    /// under the client's lock. True when the snapshot was written and
    /// the member was told; false when nothing was written because the member lists a
    /// snapshot of a service at or past this position already. Throws
    /// <see cref="CavalryException"/>: <see cref="Abi.ERR_CLUSTER_NO_SNAPSHOT_DUE"/> where
    /// none is due, <see cref="Abi.ERR_CLUSTER_SNAPSHOT_TOO_LONG"/> for a state over 1 GiB,
    /// <see cref="Abi.ERR_CLUSTER_SNAPSHOT_FAILED"/> when a step made no progress, with the
    /// step in the message, <see cref="Abi.ERR_POLLING"/> from inside a handler of this
    /// service or of one whose poll holds the client, or the client's or the driver's
    /// code. After an exception the service is as it was and the snapshot is still due:
    /// call again, or poll on.</summary>
    public bool Snapshot(ReadOnlySpan<byte> state)
    {
        int result;
        fixed (byte* bytes = state)
        {
            result = client.SnapshotClusterService(handle, bytes, (nuint) state.Length);
        }
        if (result < 0)
        {
            throw Abi.LastError("cavalry_cluster_service_snapshot");
        }
        return result == 1;
    }

    /// <summary>Starts the service from the newest snapshot of a service that its member
    /// lists, before the service's first poll. <c>Restored</c> is one of
    /// <c>Abi.CLUSTER_RESTORED_*</c>. With <see cref="Abi.CLUSTER_RESTORED_STATE"/>,
    /// <c>State</c> is the program's state as the program gave it to
    /// <see cref="Snapshot"/>, a copy the caller owns; the service holds the sessions that
    /// were open at the snapshot's position, <see cref="Position"/> is that position, and
    /// the next poll hands over the entry after it. With
    /// <see cref="Abi.CLUSTER_RESTORED_FROM_START"/>, <c>State</c> is null: the member lists
    /// no snapshot of a service, or the one it lists could not be taken, and the polls read
    /// the log from position 0. With <see cref="Abi.CLUSTER_RESTORED_NO_MEMBER"/>,
    /// <c>State</c> is null: the driver hosts no member that has a log yet, and the program
    /// calls again before its first poll. A program that never calls it reads the log from
    /// position 0 at every start. The call waits for the driver to play the snapshot's
    /// recording back, under the client's lock. Throws <see cref="CavalryException"/>:
    /// <see cref="Abi.ERR_CLUSTER_RESTORE_TOO_LATE"/> once a poll has opened the member's
    /// log or a call has answered a state or the log's start,
    /// <see cref="Abi.ERR_POLLING"/> from inside a handler, or the client's or the driver's
    /// code, after which the service has not started and the program calls again.</summary>
    public (int Restored, byte[]? State) Restore()
    {
        int restored = client.RestoreClusterService(handle, out byte[]? state);
        if (restored <= 0)
        {
            throw Abi.LastError("cavalry_cluster_service_restore");
        }
        return (restored, state);
    }

    /// <summary>Closes the service's publications and its counter, stops its read of the log
    /// and frees the handle. Idempotent. Close a service before its client; after the
    /// client's own close it frees the handle alone, and the driver ends what the service
    /// held by itself. The handle is freed whatever the ABI answers, so when the ABI's close
    /// failed this throws the <see cref="CavalryException"/> after the handle is freed. The
    /// one exception is a close from inside this service's own handler, or from inside the
    /// handler of another service whose poll holds the client, which the ABI refuses with
    /// <see cref="Abi.ERR_POLLING"/> (spec/c-abi.md, "Calls during a claim or a poll"): it
    /// then throws the <see cref="CavalryException"/> and keeps the handle.</summary>
    public void Close()
    {
        if (handle == 0)
        {
            return;
        }
        int result = client.CloseClusterService(handle);
        if (result != 0)
        {
            CavalryException error = Abi.LastError("cavalry_cluster_service_close");
            if (error.Code == Abi.ERR_POLLING)
            {
                throw error;
            }
            handle = 0;
            self.Free();
            throw error;
        }
        handle = 0;
        self.Free();
    }

    public void Dispose()
    {
        Close();
    }
}
'''

CLUSTER_ENTRY_VALUES = HEADING + r'''
namespace Cavalry;

/// <summary>
/// One committed entry as <c>cavalry_cluster_entry_values</c> reads it. It is the one object
/// a cluster service keeps, over the one struct the service's trampoline fills for each
/// entry before the handler runs, so copy a value out to keep it.
/// </summary>
public sealed class ClusterEntryValues
{
    internal Abi.cavalry_cluster_entry_values_t values;

    internal ClusterEntryValues()
    {
    }

    /// <summary>The position in the log at which the entry begins.</summary>
    public long Position => values.position;

    /// <summary>The leadership term the entry was appended in.</summary>
    public long LeadershipTermId => values.leadership_term_id;

    /// <summary>The cluster's time when its leader appended the entry, in milliseconds since
    /// 1970-01-01T00:00:00Z: the one clock a service reads, so that every member's service
    /// reaches the same state.</summary>
    public long Timestamp => values.timestamp;

    /// <summary>The session of a session's message, open or close, and -1 for every other
    /// entry.</summary>
    public long SessionId => values.session_id;

    /// <summary>What the entry is, <c>Abi.CLUSTER_ENTRY_*</c>.</summary>
    public int Type => values.type;

    /// <summary>The member that leads the term a new leadership term event begins, and -1
    /// for every other entry.</summary>
    public int LeaderMemberId => values.leader_member_id;

    /// <summary>Whether the leader closed the session of a session close event for the
    /// session timeout.</summary>
    public bool TimedOut => values.timed_out != 0;
}
'''

HEADER = HEADING + r'''
using System;

namespace Cavalry;

/// <summary>
/// The frame header a poll hands the handler, valid for the call: each getter reads the field
/// from the one <c>cavalry_header_values_t</c> the subscription keeps, which the first getter
/// of a fragment fills through <c>cavalry_header_values</c>; outside the handler call every
/// getter throws <see cref="InvalidOperationException"/>.
/// </summary>
public sealed class Header
{
    private Abi.cavalry_header_values_t values;
    private nint pointer;
    private bool read;

    internal Header()
    {
    }

    internal void Set(nint frame)
    {
        pointer = frame;
        read = false;
    }

    internal void Clear()
    {
        pointer = 0;
    }

    /// <summary>The guard every getter runs: the header must be inside its handler call, and
    /// the struct is read once per fragment.</summary>
    private void Ensure()
    {
        if (pointer == 0)
        {
            throw new InvalidOperationException("the header is valid inside the handler call alone; read its fields"
                + " there and keep the values; see clients/dotnet/README.md#the-call-back-by-construction");
        }
        if (!read)
        {
            Abi.Check("cavalry_header_values", Abi.cavalry_header_values(pointer, ref values));
            read = true;
        }
    }

    public int FrameLength
    {
        get
        {
            Ensure();
            return values.frame.frame_length;
        }
    }

    public int Version
    {
        get
        {
            Ensure();
            return values.frame.version;
        }
    }

    public int Flags
    {
        get
        {
            Ensure();
            return values.frame.flags;
        }
    }

    public int Type
    {
        get
        {
            Ensure();
            return values.frame.type;
        }
    }

    public int TermOffset
    {
        get
        {
            Ensure();
            return values.frame.term_offset;
        }
    }

    public int SessionId
    {
        get
        {
            Ensure();
            return values.frame.session_id;
        }
    }

    public int StreamId
    {
        get
        {
            Ensure();
            return values.frame.stream_id;
        }
    }

    public int TermId
    {
        get
        {
            Ensure();
            return values.frame.term_id;
        }
    }

    public long ReservedValue
    {
        get
        {
            Ensure();
            return values.frame.reserved_value;
        }
    }
}
'''

IMAGE = HEADING + r'''
using System;

namespace Cavalry;

/// <summary>One of a subscription's images, valid until the subscription's next poll or close.</summary>
public sealed class Image
{
    private readonly nint pointer;
    private readonly string sourceIdentity;
    private readonly long correlationId;
    private readonly long positionBitsToShift;
    private readonly int sessionId;
    private readonly int initialTermId;

    internal Image(nint pointer)
    {
        this.pointer = pointer;
        Abi.cavalry_image_constants_t k = default;
        Abi.Check("cavalry_image_constants", Abi.cavalry_image_constants(pointer, ref k));
        sourceIdentity = Abi.String(k.source_identity) ?? "";
        correlationId = k.correlation_id;
        positionBitsToShift = (long) k.position_bits_to_shift;
        sessionId = k.session_id;
        initialTermId = k.initial_term_id;
    }

    public string SourceIdentity => sourceIdentity;
    public long CorrelationId => correlationId;
    public long PositionBitsToShift => positionBitsToShift;
    public int SessionId => sessionId;
    public int InitialTermId => initialTermId;

    /// <summary>The image's position.</summary>
    public long Position()
    {
        return Abi.cavalry_image_position(pointer);
    }

    /// <summary>Whether the image's publisher has ended the stream.</summary>
    public bool IsEndOfStream()
    {
        return Abi.cavalry_image_is_end_of_stream(pointer);
    }
}
'''

RECORDING = HEADING + r'''
using System;

namespace Cavalry;

/// <summary>A recording the driver makes; <see cref="Close"/> frees the handle and stops nothing.</summary>
public sealed class Recording : IDisposable
{
    private nint handle;
    private readonly string channel;
    private readonly long recordingId;
    private readonly int sessionId;
    private readonly int streamId;

    internal Recording(nint handle)
    {
        this.handle = handle;
        Abi.cavalry_recording_constants_t k = default;
        Abi.Check("cavalry_recording_constants", Abi.cavalry_recording_constants(handle, ref k));
        channel = Abi.String(k.channel) ?? "";
        recordingId = k.recording_id;
        sessionId = k.session_id;
        streamId = k.stream_id;
    }

    public string Channel => channel;
    public long RecordingId => recordingId;
    public int SessionId => sessionId;
    public int StreamId => streamId;

    /// <summary>The recording's position.</summary>
    public long Position()
    {
        return Abi.cavalry_recording_position(handle);
    }

    /// <summary>The position the recording has synced to disk.</summary>
    public long SyncedPosition()
    {
        return Abi.cavalry_recording_synced_position(handle);
    }

    /// <summary>Frees the handle; the recording goes on until <see cref="Client.StopRecording"/>.</summary>
    public void Close()
    {
        if (handle == 0)
        {
            return;
        }
        nint closing = handle;
        handle = 0;
        Abi.Check("cavalry_recording_close", Abi.cavalry_recording_close(closing));
    }

    public void Dispose()
    {
        Close();
    }
}
'''

REPLAY = HEADING + r'''
using System;

namespace Cavalry;

/// <summary>A replay the driver plays.</summary>
public sealed class Replay : IDisposable
{
    /// <summary>Whether the replay has ended, at which position and why (<c>Abi.REPLAY_STOP_*</c>).</summary>
    public readonly record struct End(bool Ended, long Position, int Reason);

    private nint handle;
    private readonly string channel;
    private readonly long replaySessionId;
    private readonly long recordingId;
    private readonly long startPosition;
    private readonly int sessionId;
    private readonly int streamId;

    internal Replay(nint handle)
    {
        this.handle = handle;
        Abi.cavalry_replay_constants_t k = default;
        Abi.Check("cavalry_replay_constants", Abi.cavalry_replay_constants(handle, ref k));
        channel = Abi.String(k.channel) ?? "";
        replaySessionId = k.replay_session_id;
        recordingId = k.recording_id;
        startPosition = k.start_position;
        sessionId = k.session_id;
        streamId = k.stream_id;
    }

    public string Channel => channel;
    public long ReplaySessionId => replaySessionId;
    public long RecordingId => recordingId;
    public long StartPosition => startPosition;
    public int SessionId => sessionId;
    public int StreamId => streamId;

    /// <summary>The replay's position.</summary>
    public long Position()
    {
        return Abi.cavalry_replay_position(handle);
    }

    /// <summary>Whether the replay has ended, where and why.</summary>
    public End Ended()
    {
        bool ended = Abi.cavalry_replay_ended(handle, out long position, out int reason);
        return new End(ended, position, reason);
    }

    /// <summary>Frees the handle; the replay goes on until <see cref="Client.StopReplay"/> or its end.</summary>
    public void Close()
    {
        if (handle == 0)
        {
            return;
        }
        nint closing = handle;
        handle = 0;
        Abi.Check("cavalry_replay_close", Abi.cavalry_replay_close(closing));
    }

    public void Dispose()
    {
        Close();
    }
}
'''
