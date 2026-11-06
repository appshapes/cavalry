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
`Subscription`, `Header`, `Image`, `Recording` and `Replay`, one type per file, plus
`Cavalry.csproj`. The fragment handler is reached through one `UnmanagedCallersOnly`
method, the trampoline, which finds the subscription by the `GCHandle` the subscription
hands the ABI as its `clientd`. Python 3.9 standard library only.
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
                      "Subscription.Deliver", "Header.FrameLength", "Header.Version", "Header.Flags", "Header.Type",
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
/// The fragment handler threw during a poll: <see cref="Fragments"/> is the count the poll
/// returned once the ABI was done, <see cref="Raised"/> how many handler calls threw, and
/// <see cref="Exception.InnerException"/> the first exception, kept as thrown.
/// </summary>
public sealed class HandlerException : Exception
{
    public int Fragments { get; }
    public int Raised { get; }

    public HandlerException(int fragments, int raised, Exception cause)
        : base("the fragment handler threw on " + raised + " of " + (fragments >= 0
            ? "the " + fragments + " fragment(s) this poll delivered"
            : "the fragments this poll delivered before it stopped on a fault, which Subscription.GetFault() reads")
            + "; every fragment reached the handler once, in order. Fix the handler: its first exception, " + cause.GetType().Name
            + ", is this exception's InnerException; polling again does not deliver these fragments again."
            + " See clients/dotnet/README.md#the-exception", cause)
    {
        Fragments = fragments;
        Raised = raised;
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

    /// <summary>Closes the client and every handle it made. Idempotent; the handle is forgotten
    /// whatever the ABI returns, and a call after it hands the ABI a null handle, which it
    /// refuses with <see cref="Abi.ERR_NULL"/>.</summary>
    public void Close()
    {
        lock (gate)
        {
            if (handle == 0)
            {
                return;
            }
            nint closing = handle;
            handle = 0;
            Abi.Check("cavalry_close", Abi.cavalry_close(closing));
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
