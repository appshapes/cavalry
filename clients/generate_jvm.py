"""The JVM writer of clients/generate.py: `write_jvm(header)` returns the files of
`clients/jvm`, every one generated from the committed header (clients/README.md; ADR 0046,
card 208), except `example/Example.java`, the README's program, which is written by hand.

The library reaches the shared library through Java's foreign function and memory API
(`java.lang.foreign`, final since Java 22; the library takes Java 25, the long-term
release that holds it), so it has no dependency, no JNI and no build tool: `javac` and
`java` are the whole toolchain. `Abi.java` is the binding: the header's defines as
constants, a `StructLayout` per struct with the padding C's alignment gives it and a
constant per field offset, a downcall `MethodHandle` per function with its
`FunctionDescriptor` from the parameters' C types, bound in a static initializer that
first reads `cavalry_abi_version` and refuses a library of another version, and the
helpers the client's own code shares, `check`, `lastError`, `failed`, `string` and
`findLibrary`. The other files are the client's own code: `CavalryException`,
`HandlerException`, `FragmentHandler`, `Client`, `Publication`, `Subscription`, `Header`,
`Image`, `Recording` and `Replay`, one public class per file as Java asks. The fragment
handler is reached through an upcall stub each subscription makes once from a method
handle bound to itself, so the ABI's `clientd` goes unused. Python 3.9 standard library
only.
"""

from __future__ import annotations

import re

from generate_node import SCALARS, layout

PACKAGE = "com.appshapes.cavalry"
JAVA_RELEASE = 25
JAVA_LAYOUT = {"int": "JAVA_INT", "int8_t": "JAVA_BYTE", "uint8_t": "JAVA_BYTE", "uint16_t": "JAVA_SHORT",
               "int32_t": "JAVA_INT", "int64_t": "JAVA_LONG", "size_t": "JAVA_LONG", "bool": "JAVA_BOOLEAN"}
JAVA_TYPE = {"int": "int", "int8_t": "byte", "uint8_t": "byte", "uint16_t": "short", "int32_t": "int",
             "int64_t": "long", "size_t": "long", "bool": "boolean", "void": "void"}
C_LIMITS = {"INT64_MAX": ("long", "Long.MAX_VALUE"), "INT64_MIN": ("long", "Long.MIN_VALUE"),
            "INT32_MAX": ("int", "Integer.MAX_VALUE"), "INT32_MIN": ("int", "Integer.MIN_VALUE"),
            "UINT8_MAX": ("int", "255"), "UINT16_MAX": ("int", "65535")}
# The functions bound with Linker.Option.critical, which lets a heap segment, a byte[]'s,
# cross the boundary while the call pins it: the ABI copies the bytes and returns at once,
# calls nothing back and blocks on nothing.
CRITICAL = ("cavalry_publication_offer",)
# The per-message methods of the generated Java, which clients/lint.py reads.
PER_MESSAGE_JVM = ("Publication.tryClaim", "Publication.setReservedValue", "Publication.commit", "Publication.abort",
                   "Publication.offer", "Subscription.poll", "Subscription.onFragment", "CommittedLog.poll",
                   "CommittedLog.onEntry", "Header.frameLength",
                   "Header.version", "Header.flags", "Header.type", "Header.termOffset", "Header.sessionId",
                   "Header.streamId", "Header.termId", "Header.reservedValue")


def java_name(define: str) -> str:
    """`CAVALRY_ERR_POLLING` as `ERR_POLLING`: the ABI's name without the prefix."""
    return define[len("CAVALRY_"):] if define.startswith("CAVALRY_") else define


def java_constant(value: str) -> tuple[str, str]:
    """(type, literal) for a define's value."""
    if value in C_LIMITS:
        return C_LIMITS[value]
    if value.startswith('"'):
        return "String", value
    if re.fullmatch(r"-?\d+", value) or re.fullmatch(r"0x[0-9a-fA-F]+", value):
        return "int", value
    if re.fullmatch(r"\(?-?\d+\)?", value):
        return "int", value.strip("()")
    raise SystemExit(f"generate_jvm: cannot read the define's value {value!r}")


def value_layout(ctype: str, header) -> str:
    t = " ".join(ctype.split())
    if t.endswith("*") or t in [cb.name for cb in header.callbacks]:
        return "ADDRESS"
    if t in JAVA_LAYOUT:
        return JAVA_LAYOUT[t]
    raise SystemExit(f"generate_jvm: no layout for {t!r}")


def java_type(ctype: str, header) -> str:
    t = " ".join(ctype.split())
    if t.endswith("*") or t in [cb.name for cb in header.callbacks]:
        return "MemorySegment"
    if t in JAVA_TYPE:
        return JAVA_TYPE[t]
    raise SystemExit(f"generate_jvm: no Java type for {t!r}")


def struct_layouts(header) -> tuple[str, str]:
    """The `StructLayout` constants, nested structs first, with the padding C's alignment
    gives each, and a `long` constant per field offset."""
    lay = layout(header)
    done: list[str] = []
    lines = []
    offsets = []

    def one(name):
        if name in done:
            return
        struct = next(s for s in header.structs if s.name == name)
        parts = []
        cur = 0
        for f in struct.fields:
            field = lay[name]["fields"][f.name]
            if field["kind"] == "pointer":
                part, size = "ADDRESS", 8
            elif isinstance(field["kind"], tuple):
                inner = field["kind"][1]
                one(inner)
                part, size = inner, lay[inner]["size"]
            else:
                part, size = JAVA_LAYOUT[field["kind"]], SCALARS[field["kind"]][0]
            if field["offset"] > cur:
                parts.append(f"MemoryLayout.paddingLayout({field['offset'] - cur})")
            parts.append(f'{part}.withName("{f.name}")')
            cur = field["offset"] + size
            offsets.append(f"    static final long {name}_{f.name} = {field['offset']};")
        if lay[name]["size"] > cur:
            parts.append(f"MemoryLayout.paddingLayout({lay[name]['size'] - cur})")
        lines.append(f"    static final StructLayout {name} = MemoryLayout.structLayout(\n        "
                     + ",\n        ".join(parts) + ");")
        done.append(name)

    for s in header.structs:
        one(s.name)
    return "\n".join(lines), "\n".join(offsets)


def handles(header) -> tuple[str, str]:
    """The `static final MethodHandle` fields and their bindings, `cavalry_abi_version` first,
    bound before the version check, and every other function after it."""
    fields = []
    bindings = []
    for fn in header.functions:
        fields.append(f"    static final MethodHandle {fn.name};")
        ret = " ".join(fn.ret.split())
        args = ", ".join(value_layout(p.type, header) for p in fn.params)
        if ret == "void":
            descriptor = f"FunctionDescriptor.ofVoid({args})"
        else:
            descriptor = f"FunctionDescriptor.of({value_layout(ret, header)}{', ' + args if args else ''})"
        options = ", Linker.Option.critical(true)" if fn.name in CRITICAL else ""
        bindings.append(f'        {fn.name} = LINKER.downcallHandle(LOOKUP.findOrThrow("{fn.name}"), {descriptor}{options});')
    version = next(b for b in bindings if "cavalry_abi_version" in b)
    others = "\n".join(b for b in bindings if b is not version)
    return "\n".join(fields), version + "\n@VERSION_CHECK@\n" + others


def write_jvm(header) -> dict[str, str]:
    defines = [(n, v) for n, v in header.defines if n != "CAVALRY_H"]
    if "CAVALRY_ABI_VERSION" not in dict(defines):
        raise SystemExit("generate_jvm: the header has no CAVALRY_ABI_VERSION, which the version check reads")
    constants = "\n".join(f"    public static final {java_constant(v)[0]} {java_name(n)} = {java_constant(v)[1]}; // {n}"
                          for n, v in defines)
    layouts, offsets = struct_layouts(header)
    fields, bindings = handles(header)
    bindings = bindings.replace("@VERSION_CHECK@", VERSION_CHECK)
    abi = (ABI.replace("@CONSTANTS@", constants).replace("@LAYOUTS@", layouts).replace("@OFFSETS@", offsets)
           .replace("@HANDLE_FIELDS@", fields).replace("@BINDINGS@", bindings).replace("@RELEASE@", str(JAVA_RELEASE)))
    base = f"src/{PACKAGE.replace('.', '/')}/"
    return {
        base + "Abi.java": abi,
        base + "CavalryException.java": CAVALRY_EXCEPTION,
        base + "HandlerException.java": HANDLER_EXCEPTION,
        base + "FragmentHandler.java": FRAGMENT_HANDLER,
        base + "EntryHandler.java": ENTRY_HANDLER,
        base + "Client.java": CLIENT,
        base + "Publication.java": PUBLICATION,
        base + "Subscription.java": SUBSCRIPTION,
        base + "CommittedLog.java": COMMITTED_LOG,
        base + "Header.java": HEADER,
        base + "Image.java": IMAGE,
        base + "Recording.java": RECORDING,
        base + "Replay.java": REPLAY,
    }


HEADING = """// Generated by clients/generate.py jvm from crates/cavalry-ffi/include/cavalry.h; do not
// edit by hand, `just lint` regenerates and compares (clients/README.md; clients/jvm/README.md).
package com.appshapes.cavalry;
"""

VERSION_CHECK = """        int found;
        try {
            found = (int) cavalry_abi_version.invokeExact();
        } catch (Throwable t) {
            throw new CavalryException("cavalry_abi_version", 0, "the call failed before any other was bound: " + t);
        }
        if (found != ABI_VERSION) {
            throw new CavalryException("cavalry_abi_version", ERR_ABI_VERSION, "the library at " + LIBRARY
                + " is ABI version " + found + " and this package was generated from a header of ABI version "
                + ABI_VERSION + "; build the library from the same checkout (cargo build --release -p cavalry-ffi"
                + " --lib) or regenerate the package from the library's header with python3 clients/generate.py jvm;"
                + " see docs/reference/c-abi.md#versions-and-what-a-reader-rejects");
        }"""

ABI = HEADING + r'''
import java.lang.foreign.Arena;
import java.lang.foreign.FunctionDescriptor;
import java.lang.foreign.Linker;
import java.lang.foreign.MemoryLayout;
import java.lang.foreign.MemorySegment;
import java.lang.foreign.StructLayout;
import java.lang.foreign.SymbolLookup;
import java.lang.invoke.MethodHandle;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;

import static java.lang.foreign.ValueLayout.ADDRESS;
import static java.lang.foreign.ValueLayout.JAVA_BOOLEAN;
import static java.lang.foreign.ValueLayout.JAVA_BYTE;
import static java.lang.foreign.ValueLayout.JAVA_INT;
import static java.lang.foreign.ValueLayout.JAVA_LONG;
import static java.lang.foreign.ValueLayout.JAVA_SHORT;

/**
 * The binding over the C ABI of spec/c-abi.md (clients/jvm/README.md): the header's
 * constants under the ABI's names without their prefix, a layout per struct, a downcall
 * handle per function, and the helpers the client's own code shares. Loading this class
 * finds the shared library ({@link #findLibrary}), reads {@code cavalry_abi_version}
 * before it binds any other function, and refuses a library of another ABI version with a
 * {@link CavalryException} that names both numbers. Java @RELEASE@. The restricted methods of the
 * API, which {@code --enable-native-access} admits at run time, are the binding's whole point,
 * so their compile-time warning is suppressed here and in the two classes that reinterpret a
 * segment or make an upcall stub.
 */
@SuppressWarnings("restricted")
public final class Abi {
    private Abi() {
    }

@CONSTANTS@

@LAYOUTS@

@OFFSETS@

    static final Linker LINKER = Linker.nativeLinker();
    static final Arena ARENA = Arena.global();
    static final Path LIBRARY = findLibrary();
    static final SymbolLookup LOOKUP = SymbolLookup.libraryLookup(LIBRARY, ARENA);

@HANDLE_FIELDS@

    static {
@BINDINGS@
    }

    /** The shared library: {@code CAVALRY_FFI_LIBRARY} when set, else
     * {@code target/release/libcavalry_ffi} of the checkout this package sits in, which
     * {@code cargo build --release -p cavalry-ffi --lib} writes, found by walking up from
     * the directory these classes were loaded from. */
    public static Path findLibrary() {
        String set = System.getenv("CAVALRY_FFI_LIBRARY");
        if (set != null && !set.isEmpty()) {
            return Paths.get(set);
        }
        String suffix = System.getProperty("os.name", "").toLowerCase().contains("mac") ? ".dylib" : ".so";
        Path here;
        try {
            here = Paths.get(Abi.class.getProtectionDomain().getCodeSource().getLocation().toURI()).toAbsolutePath();
        } catch (Exception e) {
            here = Paths.get("").toAbsolutePath();
        }
        for (int up = 0; up < 8 && here != null; up++) {
            Path candidate = here.resolve("target").resolve("release").resolve("libcavalry_ffi" + suffix);
            if (Files.exists(candidate)) {
                return candidate;
            }
            here = here.getParent();
        }
        throw new CavalryException("findLibrary", 0, "libcavalry_ffi" + suffix + " was not found: build it with"
            + " `cargo build --release -p cavalry-ffi --lib` in the checkout, or set CAVALRY_FFI_LIBRARY to its path;"
            + " see clients/jvm/README.md");
    }

    /** The ABI's error code of this thread's last failed call ({@code cavalry_errcode}): what a
     * result code below zero, such as {@link #PUBLICATION_ERROR}, or a poll's -1 stands for. */
    public static int errcode() {
        try {
            return (int) cavalry_errcode.invokeExact();
        } catch (Throwable t) {
            throw failed("cavalry_errcode", t);
        }
    }

    /** The message of this thread's last failed call ({@code cavalry_errmsg}), or "". */
    public static String errmsg() {
        try {
            String message = string((MemorySegment) cavalry_errmsg.invokeExact());
            return message == null ? "" : message;
        } catch (Throwable t) {
            throw failed("cavalry_errmsg", t);
        }
    }

    /** The ABI version of the library this package loaded ({@code cavalry_abi_version}). */
    public static int libraryAbiVersion() {
        try {
            return (int) cavalry_abi_version.invokeExact();
        } catch (Throwable t) {
            throw failed("cavalry_abi_version", t);
        }
    }

    /** The library's own version, as {@code cavalry_version_full} gives it. */
    public static String version() {
        try {
            return string((MemorySegment) cavalry_version_full.invokeExact());
        } catch (Throwable t) {
            throw failed("cavalry_version_full", t);
        }
    }

    /** The check this class makes when it loads, with {@link #ABI_VERSION}: the library must be of
     * the ABI version the package was generated from, else a {@link CavalryException} with
     * {@link #ERR_ABI_VERSION} that names both numbers. */
    public static void checkAbiVersion(int expected) {
        int found = libraryAbiVersion();
        if (found != expected) {
            throw new CavalryException("cavalry_abi_version", ERR_ABI_VERSION, "the library at " + LIBRARY
                + " is ABI version " + found + " and this package was generated from a header of ABI version "
                + expected + "; build the library from the same checkout (cargo build --release -p cavalry-ffi"
                + " --lib) or regenerate the package from the library's header with python3 clients/generate.py jvm;"
                + " see docs/reference/c-abi.md#versions-and-what-a-reader-rejects");
        }
    }

    /** The exception for a call that failed: the ABI's code and message. */
    static CavalryException lastError(String fn) {
        return new CavalryException(fn, errcode(), errmsg());
    }

    /** Throws {@link #lastError} when a call's result is not 0. */
    static void check(String fn, int result) {
        if (result != 0) {
            throw lastError(fn);
        }
    }

    /** What a {@code catch (Throwable)} around {@code invokeExact} rethrows: the exception as it
     * is when it is this package's own, else a {@link CavalryException} naming the call, which
     * a handle that was bound cannot raise at run time. */
    static RuntimeException failed(String fn, Throwable t) {
        if (t instanceof RuntimeException e) {
            return e;
        }
        if (t instanceof Error e) {
            throw e;
        }
        return new CavalryException(fn, 0, "the downcall failed: " + t);
    }

    /** The C string at a pointer the ABI returned, copied, or null for NULL. */
    static String string(MemorySegment pointer) {
        if (pointer == null || pointer.address() == 0) {
            return null;
        }
        return pointer.reinterpret(Integer.MAX_VALUE).getString(0);
    }
}
'''

CAVALRY_EXCEPTION = HEADING + r'''
/**
 * A call that is not a message failed: {@link #function()} is the ABI function, and
 * {@link #code()} and {@link #abiMessage()} are the ABI's ({@code cavalry_errcode},
 * {@code cavalry_errmsg}), the message naming what to do next and its docs anchor.
 */
public final class CavalryException extends RuntimeException {
    private static final long serialVersionUID = 1L;
    private final String function;
    private final int code;
    private final String abiMessage;

    public CavalryException(String function, int code, String abiMessage) {
        super(function + ": " + abiMessage + " (error " + code + ")");
        this.function = function;
        this.code = code;
        this.abiMessage = abiMessage;
    }

    public String function() {
        return function;
    }

    public int code() {
        return code;
    }

    public String abiMessage() {
        return abiMessage;
    }
}
'''

HANDLER_EXCEPTION = HEADING + r'''
/**
 * The handler threw during a poll, a subscription's fragment handler or, when
 * {@link #committedLog()} is true, a committed log's entry handler: {@link #fragments()} is
 * the count the poll returned once the ABI was done, of fragments or of entries, or -1 when
 * the poll also stopped on a fault, which the {@code fault()} of the object that was polled
 * reads until its next poll; {@link #raised()} how many handler calls threw, and
 * {@link #getCause()} the first throwable, kept as thrown.
 */
public final class HandlerException extends RuntimeException {
    private static final long serialVersionUID = 1L;
    private final int fragments;
    private final int raised;
    private final boolean committedLog;

    public HandlerException(int fragments, int raised, Throwable cause) {
        this(fragments, raised, cause, false);
    }

    public HandlerException(int fragments, int raised, Throwable cause, boolean committedLog) {
        super(describe(fragments, raised, cause, committedLog), cause);
        this.fragments = fragments;
        this.raised = raised;
        this.committedLog = committedLog;
    }

    /** The message: what the handler threw on, the fragments of a subscription's poll or the
     * entries of a committed log's, and what to do. The fault of a committed log's own image
     * is reported by one poll (clients/jvm/README.md, "The committed log"), so its message
     * names the log's own {@code fault()}. */
    private static String describe(int fragments, int raised, Throwable cause, boolean committedLog) {
        String one = committedLog ? "entry" : "fragment";
        String many = committedLog ? "entries" : "fragments";
        String delivered;
        if (fragments < 0) {
            delivered = committedLog
                ? "the entries this poll delivered before it stopped on a fault, which this CommittedLog's fault() reads"
                    + " until its next poll and nothing reports after that, so read it and keep it before polling again"
                : "the fragments this poll delivered before it stopped on a fault, which Subscription.fault() reads";
        } else if (committedLog) {
            delivered = "the " + fragments + " " + (fragments == 1 ? one : many) + " this poll delivered";
        } else {
            delivered = "the " + fragments + " fragment(s) this poll delivered";
        }
        return "the " + one + " handler threw on " + raised + " of " + delivered
            + "; every " + one + " reached the handler once, in order. Fix the handler: its first throwable, " + cause
            + ", is this exception's cause; polling again does not deliver these " + many + " again."
            + " See clients/jvm/README.md#the-exception";
    }

    public int fragments() {
        return fragments;
    }

    public int raised() {
        return raised;
    }

    /** Whether the poll was a committed log's, whose {@code fault()} then reads its fault. */
    public boolean committedLog() {
        return committedLog;
    }
}
'''

FRAGMENT_HANDLER = HEADING + r'''
import java.lang.foreign.MemorySegment;

/**
 * Receives each fragment a poll delivers: {@code body} is a segment into the log, valid for
 * the call, and {@code header} the frame's header, valid for the call.
 */
@FunctionalInterface
public interface FragmentHandler {
    void onFragment(MemorySegment body, Header header);
}
'''

ENTRY_HANDLER = HEADING + r'''
import java.lang.foreign.MemorySegment;

/**
 * Receives each entry a committed log's poll delivers: {@code body} and {@code header} as a
 * fragment's, and {@code position} the entry's position in the log.
 */
@FunctionalInterface
public interface EntryHandler {
    void onEntry(MemorySegment body, Header header, long position);
}
'''

CLIENT = HEADING + r'''
import java.lang.foreign.Arena;
import java.lang.foreign.MemorySegment;

import static java.lang.foreign.ValueLayout.ADDRESS;

/**
 * A client of one driver: {@code cavalry_init} over a context with this package's ABI
 * version. Every method that reaches the client's handle is {@code synchronized}, so
 * threads may share a client: an add, a command or a close holds the lock for its one
 * ABI call, which waits on the driver for up to 10 s, and the lock is never on a message's
 * path (clients/jvm/README.md, "Threads"). {@link #close()} is idempotent, and the client
 * is an {@link AutoCloseable} for try-with-resources.
 */
public final class Client implements AutoCloseable {
    private MemorySegment handle;
    private final String dir;
    private final long clientId;

    /** The driver at {@code CAVALRY_DIR}, or at its default directory. */
    public Client() {
        this(null);
    }

    /** The driver at {@code dir}, or, for null, at {@code CAVALRY_DIR} or the default directory:
     * {@code cavalry_context_init_v} with this package's ABI version, then {@code cavalry_init}. */
    public Client(String dir) {
        Abi.checkAbiVersion(Abi.ABI_VERSION);
        MemorySegment client;
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment out = arena.allocate(ADDRESS);
            Abi.check("cavalry_context_init_v", (int) Abi.cavalry_context_init_v.invokeExact(out, Abi.ABI_VERSION));
            MemorySegment context = out.get(ADDRESS, 0);
            try {
                if (dir != null) {
                    Abi.check("cavalry_context_set_dir",
                        (int) Abi.cavalry_context_set_dir.invokeExact(context, arena.allocateFrom(dir)));
                }
                Abi.check("cavalry_init", (int) Abi.cavalry_init.invokeExact(out, context));
                client = out.get(ADDRESS, 0);
            } finally {
                int ignored = (int) Abi.cavalry_context_close.invokeExact(context);
            }
            this.handle = client;
            this.dir = Abi.string((MemorySegment) Abi.cavalry_dir.invokeExact(client));
            this.clientId = (long) Abi.cavalry_client_id.invokeExact(client);
        } catch (Throwable t) {
            throw Abi.failed("cavalry_init", t);
        }
    }

    /** The driver's directory. */
    public String dir() {
        return dir;
    }

    /** The id the driver gave this client. */
    public long clientId() {
        return clientId;
    }

    /** Whether the client is closed, by {@link #close()} or by the driver. */
    public synchronized boolean isClosed() {
        if (handle.address() == 0) {
            return true;
        }
        try {
            return (boolean) Abi.cavalry_is_closed.invokeExact(handle);
        } catch (Throwable t) {
            throw Abi.failed("cavalry_is_closed", t);
        }
    }

    /** 0 while open, else why the client closed, {@code Abi.CLOSED_*}. */
    public synchronized int closed() {
        if (handle.address() == 0) {
            return Abi.CLOSED_BY_USER;
        }
        try {
            return (int) Abi.cavalry_closed.invokeExact(handle);
        } catch (Throwable t) {
            throw Abi.failed("cavalry_closed", t);
        }
    }

    /** Closes the client and every handle it made. Idempotent; the handle is forgotten whatever
     * the ABI returns. */
    @Override
    public synchronized void close() {
        if (handle.address() == 0) {
            return;
        }
        MemorySegment closing = handle;
        handle = MemorySegment.NULL;
        try {
            Abi.check("cavalry_close", (int) Abi.cavalry_close.invokeExact(closing));
        } catch (Throwable t) {
            throw Abi.failed("cavalry_close", t);
        }
    }

    /** One add through the client's handle, under the lock of the public method that calls it. */
    private MemorySegment add(String fn, java.lang.invoke.MethodHandle function, String channel, int streamId) {
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment out = arena.allocate(ADDRESS);
            Abi.check(fn, (int) function.invokeExact(out, handle, arena.allocateFrom(channel), streamId));
            return out.get(ADDRESS, 0);
        } catch (Throwable t) {
            throw Abi.failed(fn, t);
        }
    }

    /** Adds a publication on the channel and stream, waiting on the driver for up to 10 s. */
    public synchronized Publication addPublication(String channel, int streamId) {
        return new Publication(add("cavalry_add_publication", Abi.cavalry_add_publication, channel, streamId));
    }

    /** Adds a publication of its own session on the channel and stream. */
    public synchronized Publication addExclusivePublication(String channel, int streamId) {
        return new Publication(add("cavalry_add_exclusive_publication", Abi.cavalry_add_exclusive_publication, channel,
            streamId));
    }

    /** Adds a subscription on the channel and stream. */
    public synchronized Subscription addSubscription(String channel, int streamId) {
        return new Subscription(add("cavalry_add_subscription", Abi.cavalry_add_subscription, channel, streamId));
    }

    /** Starts recording the live publication on the channel and stream. */
    public synchronized Recording startRecording(String channel, int streamId) {
        return new Recording(add("cavalry_start_recording", Abi.cavalry_start_recording, channel, streamId));
    }

    /** Adds the destination channel, {@code cavalry:udp?endpoint=<host>:<port>}, to the
     *  publication's control channel, answered by the driver (ADR 0053). */
    public synchronized void addDestination(Publication publication, String channel) {
        destination("cavalry_add_destination", Abi.cavalry_add_destination, publication, channel);
    }

    /** Removes the destination channel, as it was added; one not there is removed by nothing. */
    public synchronized void removeDestination(Publication publication, String channel) {
        destination("cavalry_remove_destination", Abi.cavalry_remove_destination, publication, channel);
    }

    private void destination(String fn, java.lang.invoke.MethodHandle function, Publication publication, String channel) {
        try (Arena arena = Arena.ofConfined()) {
            Abi.check(fn, (int) function.invokeExact(handle, publication.handle(), arena.allocateFrom(channel)));
        } catch (Throwable t) {
            throw Abi.failed(fn, t);
        }
    }

    /** Stops the recording. */
    public synchronized void stopRecording(long recordingId) {
        try {
            Abi.check("cavalry_stop_recording", (int) Abi.cavalry_stop_recording.invokeExact(handle, recordingId));
        } catch (Throwable t) {
            throw Abi.failed("cavalry_stop_recording", t);
        }
    }

    /** Truncates the recording to the position. */
    public synchronized void truncateRecording(long recordingId, long position) {
        try {
            Abi.check("cavalry_truncate_recording",
                (int) Abi.cavalry_truncate_recording.invokeExact(handle, recordingId, position));
        } catch (Throwable t) {
            throw Abi.failed("cavalry_truncate_recording", t);
        }
    }

    /** Purges the recording's segments before the new start position. */
    public synchronized void purgeSegments(long recordingId, long newStartPosition) {
        try {
            Abi.check("cavalry_purge_segments",
                (int) Abi.cavalry_purge_segments.invokeExact(handle, recordingId, newStartPosition));
        } catch (Throwable t) {
            throw Abi.failed("cavalry_purge_segments", t);
        }
    }

    /** Purges the recording. */
    public synchronized void purgeRecording(long recordingId) {
        try {
            Abi.check("cavalry_purge_recording", (int) Abi.cavalry_purge_recording.invokeExact(handle, recordingId));
        } catch (Throwable t) {
            throw Abi.failed("cavalry_purge_recording", t);
        }
    }

    /** Replays {@code length} bytes of the recording from {@code position} onto the replay stream
     * of the channel; {@link Abi#NULL_POSITION} and {@link Abi#NULL_LENGTH} replay all of it. */
    public synchronized Replay startReplay(long recordingId, long position, long length, int replayStreamId, String channel) {
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment out = arena.allocate(ADDRESS);
            Abi.check("cavalry_start_replay", (int) Abi.cavalry_start_replay.invokeExact(out, handle, recordingId,
                position, length, replayStreamId, arena.allocateFrom(channel)));
            return new Replay(out.get(ADDRESS, 0));
        } catch (Throwable t) {
            throw Abi.failed("cavalry_start_replay", t);
        }
    }

    /** Stops the replay session. */
    public synchronized void stopReplay(long replaySessionId) {
        try {
            Abi.check("cavalry_stop_replay", (int) Abi.cavalry_stop_replay.invokeExact(handle, replaySessionId));
        } catch (Throwable t) {
            throw Abi.failed("cavalry_stop_replay", t);
        }
    }

    /** Opens the committed log of the cluster member on this client's driver at
     * {@code position}, 0 or the {@link CommittedLog#position()} an earlier log reached. Null
     * while the driver hosts no member that has a recording of its log, so a service started
     * before its member asks again (spec/cluster.md, "The service"; spec/c-abi.md,
     * "Functions", committed log). */
    public synchronized CommittedLog committedLog(long position) {
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment out = arena.allocate(ADDRESS);
            Abi.check("cavalry_committed_log", (int) Abi.cavalry_committed_log.invokeExact(out, handle, position));
            MemorySegment log = out.get(ADDRESS, 0);
            return log.address() == 0 ? null : new CommittedLog(log, this);
        } catch (Throwable t) {
            throw Abi.failed("cavalry_committed_log", t);
        }
    }

    /** A committed log's close, under the client's lock: the ABI's result, with the thread's
     * error for the caller to read. After this client's own close the handle is null, and
     * the ABI frees the log and leaves the replay to the driver. */
    synchronized int closeCommittedLog(MemorySegment log) {
        try {
            return (int) Abi.cavalry_committed_log_close.invokeExact(log, handle);
        } catch (Throwable t) {
            throw Abi.failed("cavalry_committed_log_close", t);
        }
    }
}
'''

PUBLICATION = HEADING + r'''
import java.lang.foreign.Arena;
import java.lang.foreign.MemorySegment;

import static java.lang.foreign.ValueLayout.ADDRESS;
import static java.lang.foreign.ValueLayout.JAVA_BOOLEAN;
import static java.lang.foreign.ValueLayout.JAVA_INT;
import static java.lang.foreign.ValueLayout.JAVA_LONG;

/**
 * A publication: {@link #tryClaim} and {@link #offer} return the position after the frame or
 * a code below zero, never throwing; {@link #claim()} is the segment over the bytes the last
 * {@code tryClaim} reserved, from the one {@code cavalry_buffer_claim_t} this publication
 * keeps, set to null by {@link #commit}, {@link #abort} and {@link #close}.
 */
@SuppressWarnings("restricted")
public final class Publication implements AutoCloseable {
    /** The address space as one segment, made once and never handed out: a claim's segment is
     * the slice of it at the address the ABI filled in, one {@code asSlice} and so one object
     * per claim, and the caller sees the claimed bytes alone (clients/jvm/README.md, "The view
     * over a claimed slice"). */
    private static final MemorySegment EVERYTHING = MemorySegment.NULL.reinterpret(Long.MAX_VALUE);

    private MemorySegment handle;

    /** The handle, for the client's own commands that name this publication. */
    MemorySegment handle() {
        return handle;
    }
    private final MemorySegment claimStruct = Arena.ofAuto().allocate(Abi.cavalry_buffer_claim_t);
    private MemorySegment claim;
    private final String channel;
    private final long registrationId;
    private final int sessionId;
    private final int streamId;
    private final long maxPayloadLength;
    private final long maxMessageLength;
    private final boolean exclusive;

    Publication(MemorySegment handle) {
        this.handle = handle;
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment k = arena.allocate(Abi.cavalry_publication_constants_t);
            Abi.check("cavalry_publication_constants", (int) Abi.cavalry_publication_constants.invokeExact(handle, k));
            channel = Abi.string(k.get(ADDRESS, Abi.cavalry_publication_constants_t_channel));
            registrationId = k.get(JAVA_LONG, Abi.cavalry_publication_constants_t_registration_id);
            sessionId = k.get(JAVA_INT, Abi.cavalry_publication_constants_t_session_id);
            streamId = k.get(JAVA_INT, Abi.cavalry_publication_constants_t_stream_id);
            maxPayloadLength = k.get(JAVA_LONG, Abi.cavalry_publication_constants_t_max_payload_length);
            maxMessageLength = k.get(JAVA_LONG, Abi.cavalry_publication_constants_t_max_message_length);
            exclusive = k.get(JAVA_BOOLEAN, Abi.cavalry_publication_constants_t_is_exclusive);
        } catch (Throwable t) {
            throw Abi.failed("cavalry_publication_constants", t);
        }
    }

    public String channel() {
        return channel;
    }

    public long registrationId() {
        return registrationId;
    }

    public int sessionId() {
        return sessionId;
    }

    public int streamId() {
        return streamId;
    }

    public long maxPayloadLength() {
        return maxPayloadLength;
    }

    public long maxMessageLength() {
        return maxMessageLength;
    }

    public boolean isExclusive() {
        return exclusive;
    }

    /** Claims {@code length} bytes: the position after the frame, with {@link #claim()} the
     * writable segment over them, or a result code below zero, which leaves the claim as it
     * was. One ABI call; the segment is the client's one object per sent message, a slice of
     * {@code EVERYTHING} at the claim's address read as a {@code long}, since reading it as
     * an {@code ADDRESS} would make a segment of length zero first (a pointer field is 8
     * bytes in every layout of this package). */
    public long tryClaim(int length) {
        try {
            long position = (long) Abi.cavalry_publication_try_claim.invokeExact(handle, (long) length, claimStruct);
            if (position >= 0) {
                claim = EVERYTHING.asSlice(claimStruct.get(JAVA_LONG, Abi.cavalry_buffer_claim_t_data),
                    claimStruct.get(JAVA_LONG, Abi.cavalry_buffer_claim_t_length));
            }
            return position;
        } catch (Throwable t) {
            throw Abi.failed("cavalry_publication_try_claim", t);
        }
    }

    /** The writable segment over the claimed bytes, valid until the commit or abort, or null. */
    public MemorySegment claim() {
        return claim;
    }

    /** Commits the claim: the position after the frame, or -1. */
    public long commit() {
        claim = null;
        try {
            return (long) Abi.cavalry_buffer_claim_commit.invokeExact(claimStruct);
        } catch (Throwable t) {
            throw Abi.failed("cavalry_buffer_claim_commit", t);
        }
    }

    /** Aborts the claim, leaving padding: the position after the frame, or -1. */
    public long abort() {
        claim = null;
        try {
            return (long) Abi.cavalry_buffer_claim_abort.invokeExact(claimStruct);
        } catch (Throwable t) {
            throw Abi.failed("cavalry_buffer_claim_abort", t);
        }
    }

    /** Sets the claimed frame's reserved value, any long: 0, or -1. */
    public int setReservedValue(long value) {
        try {
            return (int) Abi.cavalry_buffer_claim_set_reserved_value.invokeExact(claimStruct, value);
        } catch (Throwable t) {
            throw Abi.failed("cavalry_buffer_claim_set_reserved_value", t);
        }
    }

    /** Publishes the message, a native segment or a heap one over an array: the position after
     * it, or a code below zero. The ABI copies the bytes before it returns. */
    public long offer(MemorySegment message) {
        try {
            return (long) Abi.cavalry_publication_offer.invokeExact(handle, message, message.byteSize());
        } catch (Throwable t) {
            throw Abi.failed("cavalry_publication_offer", t);
        }
    }

    /** The publication's position, or {@link Abi#NULL_POSITION}. */
    public long position() {
        try {
            return (long) Abi.cavalry_publication_position.invokeExact(handle);
        } catch (Throwable t) {
            throw Abi.failed("cavalry_publication_position", t);
        }
    }

    /** The position the publication may publish up to. */
    public long positionLimit() {
        try {
            return (long) Abi.cavalry_publication_position_limit.invokeExact(handle);
        } catch (Throwable t) {
            throw Abi.failed("cavalry_publication_position_limit", t);
        }
    }

    /** Whether a subscriber is connected. */
    public boolean isConnected() {
        try {
            return (boolean) Abi.cavalry_publication_is_connected.invokeExact(handle);
        } catch (Throwable t) {
            throw Abi.failed("cavalry_publication_is_connected", t);
        }
    }

    /** Whether the publication is closed. */
    public boolean isClosed() {
        if (handle.address() == 0) {
            return true;
        }
        try {
            return (boolean) Abi.cavalry_publication_is_closed.invokeExact(handle);
        } catch (Throwable t) {
            throw Abi.failed("cavalry_publication_is_closed", t);
        }
    }

    /** Closes the publication, aborting an outstanding claim. Idempotent; the handle is
     * forgotten whatever the ABI returns, and a call after it hands the ABI a null handle,
     * which it refuses with {@link Abi#ERR_NULL} ({@link Abi#PUBLICATION_ERROR} from
     * {@link #tryClaim} and {@link #offer}; the claim's calls answer -1 with
     * {@link Abi#ERR_NO_CLAIM}, since the close left no claim). The claim struct is zeroed
     * first: its {@code publication} field names this handle, which the close frees, and
     * the ABI reads that field on a commit, an abort or a reserved value, so a struct left
     * as the last claim filled it would hand the ABI freed memory (spec/c-abi.md, "Which
     * claim a struct names"). A zeroed struct names no publication, and the ABI answers
     * it without reading a handle. */
    @Override
    public void close() {
        if (handle.address() == 0) {
            return;
        }
        claim = null;
        claimStruct.fill((byte) 0);
        MemorySegment closing = handle;
        handle = MemorySegment.NULL;
        try {
            Abi.check("cavalry_publication_close", (int) Abi.cavalry_publication_close.invokeExact(closing));
        } catch (Throwable t) {
            throw Abi.failed("cavalry_publication_close", t);
        }
    }
}
'''

SUBSCRIPTION = HEADING + r'''
import java.lang.foreign.Arena;
import java.lang.foreign.FunctionDescriptor;
import java.lang.foreign.MemorySegment;
import java.lang.invoke.MethodHandle;
import java.lang.invoke.MethodHandles;
import java.lang.invoke.MethodType;
import java.util.ArrayList;
import java.util.List;

import static java.lang.foreign.ValueLayout.ADDRESS;
import static java.lang.foreign.ValueLayout.JAVA_INT;
import static java.lang.foreign.ValueLayout.JAVA_LONG;

/**
 * A subscription: {@link #poll} calls the handler for each fragment and returns the count,
 * throwing a {@link HandlerException} once the ABI has returned when a handler call threw,
 * every later fragment still having reached the handler. The handler is reached through an
 * upcall stub this subscription makes once from a method handle bound to itself.
 */
@SuppressWarnings("restricted")
public final class Subscription implements AutoCloseable {
    /** What the last poll stopped on: the code (0 for none), the session and the position. */
    public record Fault(int code, int sessionId, long position) {
    }

    private MemorySegment handle;
    private final Arena arena = Arena.ofShared();
    private MemorySegment stub;
    private final Header header;
    private FragmentHandler handler;
    private boolean polling;
    private int raised;
    private Throwable first;
    private final String channel;
    private final long registrationId;
    private final int streamId;

    Subscription(MemorySegment handle) {
        this.handle = handle;
        this.header = new Header(arena);
        try {
            MethodHandle target = MethodHandles.lookup().findVirtual(Subscription.class, "onFragment",
                MethodType.methodType(void.class, MemorySegment.class, MemorySegment.class, long.class,
                    MemorySegment.class)).bindTo(this);
            stub = Abi.LINKER.upcallStub(target, FunctionDescriptor.ofVoid(ADDRESS, ADDRESS, JAVA_LONG, ADDRESS), arena);
        } catch (ReflectiveOperationException e) {
            arena.close();
            throw new CavalryException("cavalry_add_subscription", 0, "the upcall stub could not be made: " + e);
        }
        try (Arena scratch = Arena.ofConfined()) {
            MemorySegment k = scratch.allocate(Abi.cavalry_subscription_constants_t);
            Abi.check("cavalry_subscription_constants", (int) Abi.cavalry_subscription_constants.invokeExact(handle, k));
            channel = Abi.string(k.get(ADDRESS, Abi.cavalry_subscription_constants_t_channel));
            registrationId = k.get(JAVA_LONG, Abi.cavalry_subscription_constants_t_registration_id);
            streamId = k.get(JAVA_INT, Abi.cavalry_subscription_constants_t_stream_id);
        } catch (Throwable t) {
            arena.close();
            throw Abi.failed("cavalry_subscription_constants", t);
        }
    }

    public String channel() {
        return channel;
    }

    public long registrationId() {
        return registrationId;
    }

    public int streamId() {
        return streamId;
    }

    /** Delivers up to {@code fragmentLimit} fragments to the handler: one ABI call, the count it
     * returned (or -1, with {@link Abi#errcode()} saying why: {@link Abi#ERR_POLLING} for a poll
     * from inside a handler of this subscription, which the ABI refuses and which touches
     * nothing here), and a {@link HandlerException} when a handler call threw. */
    public int poll(FragmentHandler handler, int fragmentLimit) {
        if (polling) {
            try {
                return (int) Abi.cavalry_subscription_poll.invokeExact(handle, stub, MemorySegment.NULL, (long) fragmentLimit);
            } catch (Throwable t) {
                throw Abi.failed("cavalry_subscription_poll", t);
            }
        }
        this.handler = handler;
        polling = true;
        raised = 0;
        first = null;
        int count;
        try {
            count = (int) Abi.cavalry_subscription_poll.invokeExact(handle, stub, MemorySegment.NULL, (long) fragmentLimit);
        } catch (Throwable t) {
            polling = false;
            this.handler = null;
            throw Abi.failed("cavalry_subscription_poll", t);
        }
        polling = false;
        this.handler = null;
        if (raised == 0) {
            return count;
        }
        Throwable cause = first;
        first = null;
        throw new HandlerException(count, raised, cause);
    }

    /** The upcall the ABI reaches through the stub, once per fragment: the body as a segment
     * over the log's bytes, no copy, the subscription's one {@link Header} pointed at the
     * frame, and the handler called inside a catch that keeps its first throwable and counts
     * every one, so that no throwable unwinds through the ABI's frames and every fragment
     * of the poll reaches the handler (clients/jvm/README.md, "The exception"). */
    void onFragment(MemorySegment clientd, MemorySegment buffer, long length, MemorySegment frame) {
        MemorySegment body = buffer.reinterpret(length);
        header.set(frame);
        try {
            handler.onFragment(body, header);
        } catch (Throwable t) {
            raised++;
            if (first == null) {
                first = t;
            }
        } finally {
            header.clear();
        }
    }

    /** What the last poll stopped on. */
    public Fault fault() {
        try (Arena scratch = Arena.ofConfined()) {
            MemorySegment f = scratch.allocate(Abi.cavalry_fault_t);
            Abi.check("cavalry_subscription_fault", (int) Abi.cavalry_subscription_fault.invokeExact(handle, f));
            return new Fault(f.get(JAVA_INT, Abi.cavalry_fault_t_code), f.get(JAVA_INT, Abi.cavalry_fault_t_session_id),
                f.get(JAVA_LONG, Abi.cavalry_fault_t_position));
        } catch (Throwable t) {
            throw Abi.failed("cavalry_subscription_fault", t);
        }
    }

    /** How many images the subscription has. */
    public int imageCount() {
        try {
            return (int) Abi.cavalry_subscription_image_count.invokeExact(handle);
        } catch (Throwable t) {
            throw Abi.failed("cavalry_subscription_image_count", t);
        }
    }

    /** The image at the index, with its constants copied now, or null; valid until the next
     * poll or close. */
    public Image imageAt(int index) {
        MemorySegment pointer;
        try {
            pointer = (MemorySegment) Abi.cavalry_subscription_image_at_index.invokeExact(handle, (long) index);
        } catch (Throwable t) {
            throw Abi.failed("cavalry_subscription_image_at_index", t);
        }
        return pointer.address() == 0 ? null : new Image(pointer);
    }

    /** Every image, valid until the next poll or close. */
    public List<Image> images() {
        int count = imageCount();
        List<Image> images = new ArrayList<>(count);
        for (int i = 0; i < count; i++) {
            Image image = imageAt(i);
            if (image != null) {
                images.add(image);
            }
        }
        return images;
    }

    /** Whether a publisher is connected. */
    public boolean isConnected() {
        try {
            return (boolean) Abi.cavalry_subscription_is_connected.invokeExact(handle);
        } catch (Throwable t) {
            throw Abi.failed("cavalry_subscription_is_connected", t);
        }
    }

    /** Whether the subscription is closed. */
    public boolean isClosed() {
        if (handle.address() == 0) {
            return true;
        }
        try {
            return (boolean) Abi.cavalry_subscription_is_closed.invokeExact(handle);
        } catch (Throwable t) {
            throw Abi.failed("cavalry_subscription_is_closed", t);
        }
    }

    /** Closes the subscription and frees its upcall stub. Idempotent. A close from inside a
     * handler of this subscription's own poll is refused by the ABI with
     * {@link Abi#ERR_POLLING} and leaves the subscription open (spec/c-abi.md, "Calls during a
     * claim or a poll"): it then throws the {@link CavalryException} and keeps the handle and
     * the stub. After a close, {@link #poll} hands the ABI a null handle and a null stub, and
     * the ABI answers -1 with {@link Abi#ERR_NULL}. */
    @Override
    public void close() {
        if (handle.address() == 0) {
            return;
        }
        int result;
        try {
            result = (int) Abi.cavalry_subscription_close.invokeExact(handle);
        } catch (Throwable t) {
            throw Abi.failed("cavalry_subscription_close", t);
        }
        if (result != 0) {
            CavalryException error = Abi.lastError("cavalry_subscription_close");
            if (error.code() == Abi.ERR_POLLING) {
                throw error;
            }
            handle = MemorySegment.NULL;
            stub = MemorySegment.NULL;
            arena.close();
            throw error;
        }
        handle = MemorySegment.NULL;
        stub = MemorySegment.NULL;
        arena.close();
    }
}
'''

COMMITTED_LOG = HEADING + r'''
import java.lang.foreign.Arena;
import java.lang.foreign.FunctionDescriptor;
import java.lang.foreign.MemorySegment;
import java.lang.invoke.MethodHandle;
import java.lang.invoke.MethodHandles;
import java.lang.invoke.MethodType;
import java.util.OptionalLong;

import static java.lang.foreign.ValueLayout.ADDRESS;
import static java.lang.foreign.ValueLayout.JAVA_INT;
import static java.lang.foreign.ValueLayout.JAVA_LONG;

/**
 * A service's read of a cluster's committed log, beside the member on the client's driver
 * (spec/cluster.md, "The service"): {@link #poll} calls the handler for each entry a quorum
 * has committed, in order and none at or past the member's commit position, with the entry's
 * position in the log, and returns the count, or the ABI's -1 when the poll stopped on a
 * fault, which {@link #fault()} reads until the next poll, throwing a
 * {@link HandlerException} once the ABI has returned when a handler call threw; the entry
 * was delivered and is not delivered again. {@link #position()} is the position after the
 * last entry delivered: where a service opens the log again. {@link #isEnded()} says when
 * to stop reading and what tells a whole end from a short one. The handler is reached
 * through an upcall stub this log makes once from a method handle bound to itself.
 */
@SuppressWarnings("restricted")
public final class CommittedLog implements AutoCloseable {
    private MemorySegment handle;
    private final Client client;
    private final Arena arena = Arena.ofShared();
    private MemorySegment stub;
    private final Header header;
    private EntryHandler handler;
    private boolean polling;
    private int raised;
    private Throwable first;
    private final int memberId;
    private final long recordingId;

    CommittedLog(MemorySegment handle, Client client) {
        this.handle = handle;
        this.client = client;
        this.header = new Header(arena);
        try {
            MethodHandle target = MethodHandles.lookup().findVirtual(CommittedLog.class, "onEntry",
                MethodType.methodType(void.class, MemorySegment.class, MemorySegment.class, long.class,
                    MemorySegment.class)).bindTo(this);
            stub = Abi.LINKER.upcallStub(target, FunctionDescriptor.ofVoid(ADDRESS, ADDRESS, JAVA_LONG, ADDRESS), arena);
        } catch (ReflectiveOperationException e) {
            arena.close();
            throw new CavalryException("cavalry_committed_log", 0, "the upcall stub could not be made: " + e);
        }
        try (Arena scratch = Arena.ofConfined()) {
            MemorySegment k = scratch.allocate(Abi.cavalry_committed_log_constants_t);
            Abi.check("cavalry_committed_log_constants", (int) Abi.cavalry_committed_log_constants.invokeExact(handle, k));
            memberId = k.get(JAVA_INT, Abi.cavalry_committed_log_constants_t_member_id);
            recordingId = k.get(JAVA_LONG, Abi.cavalry_committed_log_constants_t_recording_id);
        } catch (Throwable t) {
            arena.close();
            throw Abi.failed("cavalry_committed_log_constants", t);
        }
    }

    /** The id of the member whose log this is. */
    public int memberId() {
        return memberId;
    }

    /** The id of the member's recording of the log that this handle replays. */
    public long recordingId() {
        return recordingId;
    }

    /** Delivers up to {@code fragmentLimit} entries to the handler: one ABI call, the count it
     * returned (or -1, with {@link Abi#errcode()} saying why: a fault, which {@link #fault()}
     * reads until the next poll, or {@link Abi#ERR_POLLING} for a poll from inside a handler
     * of this log, which the ABI refuses and which touches nothing here), and a
     * {@link HandlerException} when a handler call threw, whose {@code fragments()} is that
     * count, -1 included. A fault code of {@link Abi#ERR_FAULT_IMAGE} or
     * {@link Abi#ERR_FAULT_IMAGES_FULL} is the one report that the replay's image failed a
     * check or could not join: the log is ended short of its entries, and the next poll
     * returns 0 and clears the fault, so keep this answer ({@link #isEnded()}). An entry
     * whose handler call threw was delivered and is not delivered again:
     * {@link #position()} is past it, and past every entry this poll delivered after it. */
    public int poll(EntryHandler handler, int fragmentLimit) {
        if (polling) {
            try {
                return (int) Abi.cavalry_committed_log_poll.invokeExact(handle, stub, MemorySegment.NULL, (long) fragmentLimit);
            } catch (Throwable t) {
                throw Abi.failed("cavalry_committed_log_poll", t);
            }
        }
        this.handler = handler;
        polling = true;
        raised = 0;
        first = null;
        int count;
        try {
            count = (int) Abi.cavalry_committed_log_poll.invokeExact(handle, stub, MemorySegment.NULL, (long) fragmentLimit);
        } catch (Throwable t) {
            polling = false;
            this.handler = null;
            throw Abi.failed("cavalry_committed_log_poll", t);
        }
        polling = false;
        this.handler = null;
        if (raised == 0) {
            return count;
        }
        Throwable cause = first;
        first = null;
        throw new HandlerException(count, raised, cause, true);
    }

    /** The upcall the ABI reaches through the stub, once per entry: the body as a segment over
     * the log's bytes, no copy, the log's one {@link Header} pointed at the frame, the entry's
     * position, which the ABI gives for the header its own poll handed over, and the handler
     * called inside a catch that keeps its first throwable and counts every one, so that no
     * throwable unwinds through the ABI's frames and every entry of the poll reaches the
     * handler (clients/jvm/README.md, "The exception"). */
    void onEntry(MemorySegment clientd, MemorySegment buffer, long length, MemorySegment frame) {
        MemorySegment body = buffer.reinterpret(length);
        header.set(frame);
        try {
            handler.onEntry(body, header, entryPosition(frame));
        } catch (Throwable t) {
            raised++;
            if (first == null) {
                first = t;
            }
        } finally {
            header.clear();
        }
    }

    /** The entry's position in the log, from the header the poll handed over. The downcall
     * has a try of its own, so that the catch around the handler's call in
     * {@link #onEntry} holds no {@code invokeExact} and stays the one that counts what the
     * handler threw. */
    private static long entryPosition(MemorySegment frame) {
        try {
            return (long) Abi.cavalry_committed_log_entry_position.invokeExact(frame);
        } catch (Throwable t) {
            throw Abi.failed("cavalry_committed_log_entry_position", t);
        }
    }

    /** What the last poll stopped on; its code is 0 when the last poll returned a count. A
     * poll that returns a count clears it, so read it after a poll that returned -1 and
     * before the next. */
    public Subscription.Fault fault() {
        try (Arena scratch = Arena.ofConfined()) {
            MemorySegment f = scratch.allocate(Abi.cavalry_fault_t);
            Abi.check("cavalry_committed_log_fault", (int) Abi.cavalry_committed_log_fault.invokeExact(handle, f));
            return new Subscription.Fault(f.get(JAVA_INT, Abi.cavalry_fault_t_code),
                f.get(JAVA_INT, Abi.cavalry_fault_t_session_id), f.get(JAVA_LONG, Abi.cavalry_fault_t_position));
        } catch (Throwable t) {
            throw Abi.failed("cavalry_committed_log_fault", t);
        }
    }

    /** The position after the last entry delivered, or the position the log was opened at
     * before the first; -1 from inside a handler of this log's own poll. */
    public long position() {
        try {
            return (long) Abi.cavalry_committed_log_position.invokeExact(handle);
        } catch (Throwable t) {
            throw Abi.failed("cavalry_committed_log_position", t);
        }
    }

    /** The bound of the last poll: the highest commit position taken from the member's
     * counter, 0 before the first poll. */
    public long commitPosition() {
        try {
            return (long) Abi.cavalry_committed_log_commit_position.invokeExact(handle);
        } catch (Throwable t) {
            throw Abi.failed("cavalry_committed_log_commit_position", t);
        }
    }

    /** Whether this log has come to its end: no poll will deliver another entry. Stop reading
     * when it is true, not when a poll returns 0. The log delivered every entry it was owed
     * only when three things hold: {@link #failedAt()} is empty, {@link #withdrawnAt()} is
     * empty, and no {@link #poll} of this log returned -1 with the fault of the log's own
     * image, {@link Abi#ERR_FAULT_IMAGE} when the image failed a check or
     * {@link Abi#ERR_FAULT_IMAGES_FULL} when it could not join. That fault is reported once,
     * by the poll that meets it: {@link #fault()} answers it only until the next poll, which
     * returns 0 and clears it, and both reports are empty for such a log. So a service keeps
     * that poll's answer, since nothing reports it later. In every case what the log
     * delivered ends at {@link #position()}, where a service opens the log again. */
    public boolean isEnded() {
        try {
            return (boolean) Abi.cavalry_committed_log_is_ended.invokeExact(handle);
        } catch (Throwable t) {
            throw Abi.failed("cavalry_committed_log_is_ended", t);
        }
    }

    /** The position after the last frame the driver's replay of the member's recording played
     * before it failed, or empty for a log that is live or that ended because its member
     * closed or left the recording. No entry from that position on is delivered. The entries
     * below it may still be on their way when this first is a position, so read it once
     * {@link #isEnded()} is true. They have all been delivered by then unless the driver
     * withdrew the replay first, which it does 5 s after a replay ends, one that failed
     * included: {@link #withdrawnAt()} is then a position below this one, and the entries
     * from there on, those below this position among them, were not delivered either. What
     * the log delivered ends at {@link #position()}. */
    public OptionalLong failedAt() {
        try (Arena scratch = Arena.ofConfined()) {
            MemorySegment position = scratch.allocate(JAVA_LONG);
            boolean failed = (boolean) Abi.cavalry_committed_log_failed_at.invokeExact(handle, position);
            return failed ? OptionalLong.of(position.get(JAVA_LONG, 0)) : OptionalLong.empty();
        } catch (Throwable t) {
            throw Abi.failed("cavalry_committed_log_failed_at", t);
        }
    }

    /** The position at which the entries this log was not given begin, when the driver
     * withdrew the replay before the polls had read it to its end, or empty. The driver keeps
     * a replay 5 s after it ends and then withdraws it, read or not: a service that did not
     * poll for that long after its member closed or left the log, or whose handler took that
     * long, ends so. The entries from that position on were played and not delivered. A
     * replay that failed is withdrawn in the same way: {@link #failedAt()} is then a position
     * too, and this one is the lower. Read it once {@link #isEnded()} is true. */
    public OptionalLong withdrawnAt() {
        try (Arena scratch = Arena.ofConfined()) {
            MemorySegment position = scratch.allocate(JAVA_LONG);
            boolean withdrawn = (boolean) Abi.cavalry_committed_log_withdrawn_at.invokeExact(handle, position);
            return withdrawn ? OptionalLong.of(position.get(JAVA_LONG, 0)) : OptionalLong.empty();
        } catch (Throwable t) {
            throw Abi.failed("cavalry_committed_log_withdrawn_at", t);
        }
    }

    /** Stops the replay through the client and frees the handle and its upcall stub.
     * Idempotent. After the client's own close it frees the handle alone, and the driver ends
     * the replay by itself. The handle is freed whatever the ABI answers. When the ABI could
     * not stop the replay, through a client that the driver or a timeout closed or within the
     * response timeout, this throws the {@link CavalryException} after the handle and the
     * stub are freed: the log is closed, a second close does nothing, and the driver ends the
     * replay by itself. The one exception is a close from inside a handler of this log's own
     * poll, which the ABI refuses with {@link Abi#ERR_POLLING} and which leaves the log open
     * (spec/c-abi.md, "Calls during a claim or a poll"): it then throws the
     * {@link CavalryException} and keeps the handle and the stub. */
    @Override
    public void close() {
        if (handle.address() == 0) {
            return;
        }
        int result = client.closeCommittedLog(handle);
        if (result != 0) {
            CavalryException error = Abi.lastError("cavalry_committed_log_close");
            if (error.code() == Abi.ERR_POLLING) {
                throw error;
            }
            handle = MemorySegment.NULL;
            stub = MemorySegment.NULL;
            arena.close();
            throw error;
        }
        handle = MemorySegment.NULL;
        stub = MemorySegment.NULL;
        arena.close();
    }
}
'''

HEADER = HEADING + r'''
import java.lang.foreign.Arena;
import java.lang.foreign.MemorySegment;

import static java.lang.foreign.ValueLayout.JAVA_BYTE;
import static java.lang.foreign.ValueLayout.JAVA_INT;
import static java.lang.foreign.ValueLayout.JAVA_LONG;
import static java.lang.foreign.ValueLayout.JAVA_SHORT;

/**
 * The frame header a poll hands the handler, valid for the call: each getter reads the field
 * from the one {@code cavalry_header_values_t} the subscription keeps, which the first getter
 * of a fragment fills through {@code cavalry_header_values}; outside the handler call every
 * getter throws {@link IllegalStateException}.
 */
public final class Header {
    private final MemorySegment values;
    private MemorySegment pointer;
    private boolean read;

    Header(Arena arena) {
        values = arena.allocate(Abi.cavalry_header_values_t);
    }

    void set(MemorySegment frame) {
        pointer = frame;
        read = false;
    }

    void clear() {
        pointer = null;
    }

    /** The guard every getter runs: the header must be inside its handler call, and the
     * struct is read once per fragment. */
    private void ensure() {
        if (pointer == null) {
            throw new IllegalStateException("the header is valid inside the handler call alone; read its fields"
                + " there and keep the values; see clients/jvm/README.md#the-call-back-by-construction");
        }
        if (!read) {
            try {
                Abi.check("cavalry_header_values", (int) Abi.cavalry_header_values.invokeExact(pointer, values));
            } catch (Throwable t) {
                throw Abi.failed("cavalry_header_values", t);
            }
            read = true;
        }
    }

    public int frameLength() {
        ensure();
        return values.get(JAVA_INT, Abi.cavalry_header_values_t_frame + Abi.cavalry_header_values_frame_t_frame_length);
    }

    public int version() {
        ensure();
        return values.get(JAVA_BYTE, Abi.cavalry_header_values_t_frame + Abi.cavalry_header_values_frame_t_version);
    }

    public int flags() {
        ensure();
        return values.get(JAVA_BYTE, Abi.cavalry_header_values_t_frame + Abi.cavalry_header_values_frame_t_flags) & 0xff;
    }

    public int type() {
        ensure();
        return values.get(JAVA_SHORT, Abi.cavalry_header_values_t_frame + Abi.cavalry_header_values_frame_t_type) & 0xffff;
    }

    public int termOffset() {
        ensure();
        return values.get(JAVA_INT, Abi.cavalry_header_values_t_frame + Abi.cavalry_header_values_frame_t_term_offset);
    }

    public int sessionId() {
        ensure();
        return values.get(JAVA_INT, Abi.cavalry_header_values_t_frame + Abi.cavalry_header_values_frame_t_session_id);
    }

    public int streamId() {
        ensure();
        return values.get(JAVA_INT, Abi.cavalry_header_values_t_frame + Abi.cavalry_header_values_frame_t_stream_id);
    }

    public int termId() {
        ensure();
        return values.get(JAVA_INT, Abi.cavalry_header_values_t_frame + Abi.cavalry_header_values_frame_t_term_id);
    }

    public long reservedValue() {
        ensure();
        return values.get(JAVA_LONG, Abi.cavalry_header_values_t_frame + Abi.cavalry_header_values_frame_t_reserved_value);
    }
}
'''

IMAGE = HEADING + r'''
import java.lang.foreign.Arena;
import java.lang.foreign.MemorySegment;

import static java.lang.foreign.ValueLayout.ADDRESS;
import static java.lang.foreign.ValueLayout.JAVA_INT;
import static java.lang.foreign.ValueLayout.JAVA_LONG;

/** One of a subscription's images, valid until the subscription's next poll or close. */
public final class Image {
    private final MemorySegment pointer;
    private final String sourceIdentity;
    private final long correlationId;
    private final long positionBitsToShift;
    private final int sessionId;
    private final int initialTermId;

    Image(MemorySegment pointer) {
        this.pointer = pointer;
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment k = arena.allocate(Abi.cavalry_image_constants_t);
            Abi.check("cavalry_image_constants", (int) Abi.cavalry_image_constants.invokeExact(pointer, k));
            sourceIdentity = Abi.string(k.get(ADDRESS, Abi.cavalry_image_constants_t_source_identity));
            correlationId = k.get(JAVA_LONG, Abi.cavalry_image_constants_t_correlation_id);
            positionBitsToShift = k.get(JAVA_LONG, Abi.cavalry_image_constants_t_position_bits_to_shift);
            sessionId = k.get(JAVA_INT, Abi.cavalry_image_constants_t_session_id);
            initialTermId = k.get(JAVA_INT, Abi.cavalry_image_constants_t_initial_term_id);
        } catch (Throwable t) {
            throw Abi.failed("cavalry_image_constants", t);
        }
    }

    public String sourceIdentity() {
        return sourceIdentity;
    }

    public long correlationId() {
        return correlationId;
    }

    public long positionBitsToShift() {
        return positionBitsToShift;
    }

    public int sessionId() {
        return sessionId;
    }

    public int initialTermId() {
        return initialTermId;
    }

    /** The image's position. */
    public long position() {
        try {
            return (long) Abi.cavalry_image_position.invokeExact(pointer);
        } catch (Throwable t) {
            throw Abi.failed("cavalry_image_position", t);
        }
    }

    /** Whether the image's publisher has ended the stream. */
    public boolean isEndOfStream() {
        try {
            return (boolean) Abi.cavalry_image_is_end_of_stream.invokeExact(pointer);
        } catch (Throwable t) {
            throw Abi.failed("cavalry_image_is_end_of_stream", t);
        }
    }
}
'''

RECORDING = HEADING + r'''
import java.lang.foreign.Arena;
import java.lang.foreign.MemorySegment;

import static java.lang.foreign.ValueLayout.ADDRESS;
import static java.lang.foreign.ValueLayout.JAVA_INT;
import static java.lang.foreign.ValueLayout.JAVA_LONG;

/** A recording the driver makes; {@link #close()} frees the handle and stops nothing. */
public final class Recording implements AutoCloseable {
    private MemorySegment handle;
    private final String channel;
    private final long recordingId;
    private final int sessionId;
    private final int streamId;

    Recording(MemorySegment handle) {
        this.handle = handle;
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment k = arena.allocate(Abi.cavalry_recording_constants_t);
            Abi.check("cavalry_recording_constants", (int) Abi.cavalry_recording_constants.invokeExact(handle, k));
            channel = Abi.string(k.get(ADDRESS, Abi.cavalry_recording_constants_t_channel));
            recordingId = k.get(JAVA_LONG, Abi.cavalry_recording_constants_t_recording_id);
            sessionId = k.get(JAVA_INT, Abi.cavalry_recording_constants_t_session_id);
            streamId = k.get(JAVA_INT, Abi.cavalry_recording_constants_t_stream_id);
        } catch (Throwable t) {
            throw Abi.failed("cavalry_recording_constants", t);
        }
    }

    public String channel() {
        return channel;
    }

    public long recordingId() {
        return recordingId;
    }

    public int sessionId() {
        return sessionId;
    }

    public int streamId() {
        return streamId;
    }

    /** The recording's position. */
    public long position() {
        try {
            return (long) Abi.cavalry_recording_position.invokeExact(handle);
        } catch (Throwable t) {
            throw Abi.failed("cavalry_recording_position", t);
        }
    }

    /** The position the recording has synced to disk. */
    public long syncedPosition() {
        try {
            return (long) Abi.cavalry_recording_synced_position.invokeExact(handle);
        } catch (Throwable t) {
            throw Abi.failed("cavalry_recording_synced_position", t);
        }
    }

    /** Frees the handle; the recording goes on until {@link Client#stopRecording}. */
    @Override
    public void close() {
        if (handle.address() == 0) {
            return;
        }
        MemorySegment closing = handle;
        handle = MemorySegment.NULL;
        try {
            Abi.check("cavalry_recording_close", (int) Abi.cavalry_recording_close.invokeExact(closing));
        } catch (Throwable t) {
            throw Abi.failed("cavalry_recording_close", t);
        }
    }
}
'''

REPLAY = HEADING + r'''
import java.lang.foreign.Arena;
import java.lang.foreign.MemorySegment;

import static java.lang.foreign.ValueLayout.ADDRESS;
import static java.lang.foreign.ValueLayout.JAVA_INT;
import static java.lang.foreign.ValueLayout.JAVA_LONG;

/** A replay the driver plays. */
public final class Replay implements AutoCloseable {
    /** Whether the replay has ended, at which position and why ({@code Abi.REPLAY_STOP_*}). */
    public record End(boolean ended, long position, int reason) {
    }

    private MemorySegment handle;
    private final String channel;
    private final long replaySessionId;
    private final long recordingId;
    private final long startPosition;
    private final int sessionId;
    private final int streamId;

    Replay(MemorySegment handle) {
        this.handle = handle;
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment k = arena.allocate(Abi.cavalry_replay_constants_t);
            Abi.check("cavalry_replay_constants", (int) Abi.cavalry_replay_constants.invokeExact(handle, k));
            channel = Abi.string(k.get(ADDRESS, Abi.cavalry_replay_constants_t_channel));
            replaySessionId = k.get(JAVA_LONG, Abi.cavalry_replay_constants_t_replay_session_id);
            recordingId = k.get(JAVA_LONG, Abi.cavalry_replay_constants_t_recording_id);
            startPosition = k.get(JAVA_LONG, Abi.cavalry_replay_constants_t_start_position);
            sessionId = k.get(JAVA_INT, Abi.cavalry_replay_constants_t_session_id);
            streamId = k.get(JAVA_INT, Abi.cavalry_replay_constants_t_stream_id);
        } catch (Throwable t) {
            throw Abi.failed("cavalry_replay_constants", t);
        }
    }

    public String channel() {
        return channel;
    }

    public long replaySessionId() {
        return replaySessionId;
    }

    public long recordingId() {
        return recordingId;
    }

    public long startPosition() {
        return startPosition;
    }

    public int sessionId() {
        return sessionId;
    }

    public int streamId() {
        return streamId;
    }

    /** The replay's position. */
    public long position() {
        try {
            return (long) Abi.cavalry_replay_position.invokeExact(handle);
        } catch (Throwable t) {
            throw Abi.failed("cavalry_replay_position", t);
        }
    }

    /** Whether the replay has ended, where and why. */
    public End ended() {
        try (Arena arena = Arena.ofConfined()) {
            MemorySegment position = arena.allocate(JAVA_LONG);
            MemorySegment reason = arena.allocate(JAVA_INT);
            boolean ended = (boolean) Abi.cavalry_replay_ended.invokeExact(handle, position, reason);
            return new End(ended, position.get(JAVA_LONG, 0), reason.get(JAVA_INT, 0));
        } catch (Throwable t) {
            throw Abi.failed("cavalry_replay_ended", t);
        }
    }

    /** Frees the handle; the replay goes on until {@link Client#stopReplay} or its end. */
    @Override
    public void close() {
        if (handle.address() == 0) {
            return;
        }
        MemorySegment closing = handle;
        handle = MemorySegment.NULL;
        try {
            Abi.check("cavalry_replay_close", (int) Abi.cavalry_replay_close.invokeExact(closing));
        } catch (Throwable t) {
            throw Abi.failed("cavalry_replay_close", t);
        }
    }
}
'''
