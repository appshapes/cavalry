"""The Node writer of clients/generate.py: `write_node(header)` returns the files of
`clients/node`, every one generated from the committed header (clients/README.md; ADR 0046,
card 206).

The package reaches the ABI through a native addon in C over N-API, `src/addon.c`, built
by `node-gyp` against the header and linked with `cavalry-ffi`'s static library, so the
package has no runtime dependency; the alternatives, a foreign-function package such as
`koffi` or `ffi-napi` (a third-party native module, with more objects built per call-back
than N-API's own call) and `napi-rs` over the Rust crates (which goes round the header;
ADR 0046, Alternatives), lost, as clients/node/README.md records. The addon has one
function per ABI function, marshalled by the parameter's C type: an opaque handle is an
N-API external, a caller-owned struct is the memory of an `ArrayBuffer` the client keeps,
refused when it is smaller than the struct or misaligned for it, the bytes of an offer are a
`Buffer`'s memory, strings are copied once, and a handle out parameter comes back as the
external. Nine functions are the addon's own, not the header's: `registerSubscription`
(the per-subscription state the trampoline needs, the JS `Header` object and the one
`cavalry_header_values_t` it fills), `poll` (the one `cavalry_subscription_poll` call with
the trampoline and the handler), `registerCommittedLog` and `committedLogPoll` (the same
state and the same trampoline for a committed log, whose handler is handed the entry's
position as a third argument, and the one `cavalry_committed_log_poll` call; ADR 0070),
`headerField` (a field of the current frame's header, read
through `cavalry_header_values` once per fragment on the first read), `view` (a `Buffer`
over the slice a claim reserved), `detach` (a view released), `stringAt` (the C string a
constants struct's pointer field names) and `buffersHeld` (the Buffers Node has not freed
yet). `index.js` is the client's own code and `index.d.ts` its types; `binding.gyp` and
`package.json` build it. Python 3.9 standard library only.
"""

from __future__ import annotations

import json

# C layout on the two 64-bit targets CI runs: size and alignment per scalar type.
SCALARS = {
    "int": (4, 4), "int8_t": (1, 1), "uint8_t": (1, 1), "uint16_t": (2, 2), "int32_t": (4, 4),
    "int64_t": (8, 8), "size_t": (8, 8), "bool": (1, 1),
}
NAPI_READ = {  # how index.js reads a scalar field from a DataView
    "int": "getInt32", "int8_t": "getInt8", "uint8_t": "getUint8", "uint16_t": "getUint16",
    "int32_t": "getInt32", "int64_t": "getBigInt64", "size_t": "getBigUint64", "bool": "getUint8",
}
C_LIMITS = {"INT64_MAX": "9223372036854775807n", "INT64_MIN": "-9223372036854775808n", "INT32_MAX": "2147483647",
            "INT32_MIN": "-2147483648", "UINT8_MAX": "255", "UINT16_MAX": "65535"}
HEADER_FIELDS = ["frame_length", "version", "flags", "type", "term_offset", "session_id", "stream_id", "term_id",
                 "reserved_value"]


def camel(name: str) -> str:
    head, *rest = name.split("_")
    return head + "".join(part.capitalize() for part in rest)


def layout(header) -> dict:
    """Each struct's size and its fields' offsets, in C's natural alignment, nested structs
    included, for the DataView reads of index.js and the ArrayBuffers it allocates."""
    done: dict = {}

    def one(name):
        if name in done:
            return done[name]
        struct = next(s for s in header.structs if s.name == name)
        offset, align, fields = 0, 1, {}
        for f in struct.fields:
            t = " ".join(f.type.split())
            if t.endswith("*"):
                size, a, kind = 8, 8, "pointer"
            elif t in SCALARS:
                (size, a), kind = SCALARS[t], t
            else:
                sub = one(t)
                size, a, kind = sub["size"], sub["align"], ("struct", t)
            offset = (offset + a - 1) // a * a
            fields[f.name] = {"offset": offset, "kind": kind, "type": t}
            offset += size
            align = max(align, a)
        done[name] = {"size": (offset + align - 1) // align * align, "align": align, "fields": fields}
        return done[name]

    for s in header.structs:
        one(s.name)
    return done


def js_value(value: str) -> str:
    if value in C_LIMITS:
        return C_LIMITS[value]
    return value


# ------------------------------------------------------------------ the addon

def c_arg(param, header, index: int, lines: list, struct_names: set, opaque: set) -> str:
    """The C statements that read JS argument `index` as `param`, appended to `lines`; returns
    the C expression to pass. Pairs are handled by the caller."""
    t = " ".join(param.type.split())
    const = t.startswith("const ")
    base = t[6:] if const else t
    name = param.name
    if base == "char *":
        lines.append(f"  char {name}_buf[4096]; const char *{name} = string_arg(env, argv[{index}], {name}_buf, sizeof {name}_buf, \"{name}\");")
        lines.append(f"  if (string_failed) return NULL;")
        return name
    if base.endswith(" **"):
        inner = base[:-3]
        lines.append(f"  {inner} *{name} = NULL;")
        return f"&{name}"
    if base.endswith(" *"):
        inner = base[:-2]
        if inner in opaque:
            lines.append(f"  {inner} *{name} = ({inner} *) external_arg(env, argv[{index}], \"{name}\");")
            lines.append(f"  if (arg_failed) return NULL;")
            return name
        if inner in struct_names:
            lines.append(f"  {inner} *{name} = ({inner} *) struct_arg(env, argv[{index}], sizeof({inner}), "
                         f"_Alignof({inner}), \"{name}\");")
            lines.append(f"  if (arg_failed) return NULL;")
            return name
        if inner in SCALARS:
            lines.append(f"  {inner} {name} = 0;")
            return f"&{name}"
        raise SystemExit(f"generate_node: no marshalling for {t!r}")
    if base in SCALARS:
        reader = {"int": "int32_arg", "int32_t": "int32_arg", "int64_t": "int64_arg", "size_t": "size_arg",
                  "bool": "bool_arg", "uint8_t": "int32_arg", "uint16_t": "int32_arg", "int8_t": "int32_arg"}[base]
        lines.append(f"  {base} {name} = ({base}) {reader}(env, argv[{index}], \"{name}\");")
        lines.append(f"  if (arg_failed) return NULL;")
        return name
    raise SystemExit(f"generate_node: no marshalling for {t!r}")


# The functions that take the C fragment handler: each has a poll of the addon's own,
# `poll` and `committedLogPoll`, and no generic binding.
HANDLER_FUNCTIONS = ("cavalry_subscription_poll", "cavalry_committed_log_poll")
# The addon's own functions, registered beside the header's.
OWN_FUNCTIONS = ("registerSubscription", "poll", "registerCommittedLog", "committedLogPoll", "headerField", "view",
                 "detach", "stringAt", "buffersHeld")


def c_return(fn, header, call: str, outs: list, lines: list, opaque: set) -> None:
    ret = " ".join(fn.ret.split())
    if ret == "int" or ret == "int32_t":
        make = "make_int32(env, (int32_t) result)"
    elif ret == "int64_t":
        make = "make_int64(env, result)"
    elif ret == "bool":
        make = "make_bool(env, result)"
    elif ret == "const char *":
        make = "make_string(env, result)"
    elif ret.endswith(" *") and ret[:-2] in opaque:
        make = "make_external(env, (void *) result)"
    else:
        raise SystemExit(f"generate_node: no return marshalling for {ret!r}")
    lines.append(f"  {ret} result = {call};")
    if not outs:
        lines.append(f"  return {make};")
        return
    # A function with out parameters returns an array: the result, then each out value;
    # a handle out parameter is the external, or null when the result is not 0.
    lines.append(f"  napi_value tuple; napi_create_array_with_length(env, {len(outs) + 1}, &tuple);")
    lines.append(f"  napi_set_element(env, tuple, 0, {make});")
    for i, (kind, name) in enumerate(outs, 1):
        if kind == "handle":
            lines.append(f"  napi_set_element(env, tuple, {i}, make_external(env, (void *) {name}));")
        elif kind == "int64_t":
            lines.append(f"  napi_set_element(env, tuple, {i}, make_int64(env, {name}));")
        else:
            lines.append(f"  napi_set_element(env, tuple, {i}, make_int32(env, (int32_t) {name}));")
    lines.append("  return tuple;")


def write_addon(header) -> str:
    opaque = set(header.opaque)
    struct_names = {s.name for s in header.structs}
    out = []
    out.append("/* The C ABI of spec/c-abi.md as an N-API addon: one function per ABI function, marshalled")
    out.append(" * by its parameters' C types, and nine of the addon's own for the subscription state, the")
    out.append(" * poll, a committed log's state and poll, the header's fields, the views, the constants'")
    out.append(" * strings and the Buffers Node still")
    out.append(" * holds. Generated by clients/generate.py node from")
    out.append(" * crates/cavalry-ffi/include/cavalry.h; do not edit by hand, `just lint` regenerates and")
    out.append(" * compares (clients/README.md; clients/node/README.md, \"The runtime's side\"). */")
    out.append("#define NAPI_VERSION 8")
    out.append("#include <node_api.h>")
    out.append("#include <stdbool.h>")
    out.append("#include <stdint.h>")
    out.append("#include <stdio.h>")
    out.append("#include <stdlib.h>")
    out.append("#include <string.h>")
    out.append('#include "cavalry.h"')
    out.append("")
    # The two polls take the C fragment handler, which no generic marshalling makes: the
    # subscription's is `poll` below and the committed log's `committedLogPoll`, each
    # written by hand (spec/c-abi.md, version 2; ADR 0070).
    functions = [fn for fn in header.functions if fn.name not in HANDLER_FUNCTIONS]
    wrappers = []
    for fn in functions:
        lines = [f"static napi_value fn_{fn.name}(napi_env env, napi_callback_info info) {{"]
        params = fn.params
        n = len(params)
        js_args = 0
        args = []
        outs = []
        i = 0
        body = []
        while i < n:
            p = params[i]
            t = " ".join(p.type.split())
            nxt = params[i + 1] if i + 1 < n else None
            if t in ("const uint8_t *", "uint8_t *") and nxt is not None and " ".join(nxt.type.split()) == "size_t":
                body.append(f"  size_t {nxt.name} = 0; const uint8_t *{p.name} = (const uint8_t *) bytes_arg(env, argv[{js_args}], &{nxt.name}, \"{p.name}\");")
                body.append("  if (arg_failed) return NULL;")
                args += [p.name, nxt.name]
                js_args += 1
                i += 2
                continue
            base = t[6:] if t.startswith("const ") else t
            if base.endswith(" **"):
                expr = c_arg(p, header, js_args, body, struct_names, opaque)
                outs.append(("handle", p.name))
                args.append(expr)
                i += 1
                continue
            if base.endswith(" *") and base[:-2] in SCALARS:
                expr = c_arg(p, header, js_args, body, struct_names, opaque)
                outs.append((base[:-2], p.name))
                args.append(expr)
                i += 1
                continue
            expr = c_arg(p, header, js_args, body, struct_names, opaque)
            args.append(expr)
            js_args += 1
            i += 1
        lines.append(f"  size_t argc = {js_args}; napi_value argv[{max(js_args, 1)}];")
        lines.append(f"  if (!read_args(env, info, {js_args}, &argc, argv, \"{fn.name}\")) return NULL;")
        lines += body
        c_return(fn, header, f"{fn.name}({', '.join(args)})", outs, lines, opaque)
        lines.append("}")
        lines.append("")
        wrappers += lines
    body = "\n".join(wrappers)
    helpers = ADDON_HELPERS
    for reader in OPTIONAL_READERS:
        if f"{reader}(" not in body:
            helpers = helpers.replace(OPTIONAL_READERS[reader], "")
    out.append(helpers)
    out += wrappers
    out.append("static napi_value init(napi_env env, napi_value exports) {")
    out.append("  napi_property_descriptor properties[] = {")
    for fn in functions:
        out.append(f'    {{ "{fn.name}", NULL, fn_{fn.name}, NULL, NULL, NULL, napi_default, NULL }},')
    for own in OWN_FUNCTIONS:
        out.append(f'    {{ "{own}", NULL, fn_{own}, NULL, NULL, NULL, napi_default, NULL }},')
    out.append("  };")
    out.append("  napi_define_properties(env, exports, sizeof properties / sizeof properties[0], properties);")
    out.append("  return exports;")
    out.append("}")
    out.append("")
    out.append("NAPI_MODULE(cavalry_addon, init)")
    return "\n".join(out) + "\n"


# Readers only a wrapper would use, left out of the addon when no function of the header
# takes the type, so that the addon compiles under -Wall -Wextra without a warning.
OPTIONAL_READERS = {
    "bool_arg": '''static bool bool_arg(napi_env env, napi_value v, const char *name) {
  bool b = false;
  if (napi_get_value_bool(env, v, &b) != napi_ok) { type_error(env, name, "a boolean"); return false; }
  return b;
}

''',
}

ADDON_HELPERS = r'''/* Argument readers. Each sets arg_failed and throws a TypeError naming the parameter when
 * the JS value is not what the ABI's parameter takes; a wrapper returns NULL after one, which
 * N-API reads as the pending exception. The flags are per thread: each worker thread of the
 * process calls the addon on its own thread (clients/node/README.md, "Threads"). */
static _Thread_local bool arg_failed;
static _Thread_local bool string_failed;

static bool read_args(napi_env env, napi_callback_info info, size_t expected, size_t *argc, napi_value *argv,
                      const char *function) {
  arg_failed = false; string_failed = false;
  napi_value this_arg;
  if (napi_get_cb_info(env, info, argc, argv, &this_arg, NULL) != napi_ok) return false;
  if (*argc < expected) {
    char message[160];
    snprintf(message, sizeof message, "%s takes %zu argument(s), %zu given", function, expected, *argc);
    napi_throw_type_error(env, NULL, message);
    return false;
  }
  return true;
}

static void type_error(napi_env env, const char *name, const char *wanted) {
  char message[160];
  snprintf(message, sizeof message, "%s: %s expected", name, wanted);
  napi_throw_type_error(env, NULL, message);
  arg_failed = true;
}

static bool is_nullish(napi_env env, napi_value v) {
  napi_valuetype t;
  return napi_typeof(env, v, &t) == napi_ok && (t == napi_null || t == napi_undefined);
}

/* An opaque handle: an external, or null for a NULL pointer, which the ABI refuses with
 * CAVALRY_ERR_NULL. */
static void *external_arg(napi_env env, napi_value v, const char *name) {
  if (is_nullish(env, v)) return NULL;
  void *p = NULL;
  if (napi_get_value_external(env, v, &p) != napi_ok) { type_error(env, name, "a handle or null"); return NULL; }
  return p;
}

/* Bytes the caller owns: an ArrayBuffer, a Buffer or any typed array, or a DataView, whose
 * memory the ABI reads or writes for the call; null is a NULL pointer. */
static void *bytes_arg(napi_env env, napi_value v, size_t *length, const char *name) {
  if (is_nullish(env, v)) { if (length) *length = 0; return NULL; }
  bool is;
  void *data = NULL; size_t n = 0;
  if (napi_is_arraybuffer(env, v, &is) == napi_ok && is) {
    if (napi_get_arraybuffer_info(env, v, &data, &n) != napi_ok) goto bad;
  } else if (napi_is_typedarray(env, v, &is) == napi_ok && is) {
    napi_typedarray_type t; napi_value ab; size_t offset; void *base;
    if (napi_get_typedarray_info(env, v, &t, &n, &base, &ab, &offset) != napi_ok) goto bad;
    size_t width = (t == napi_int8_array || t == napi_uint8_array || t == napi_uint8_clamped_array) ? 1 :
                   (t == napi_int16_array || t == napi_uint16_array) ? 2 :
                   (t == napi_int32_array || t == napi_uint32_array || t == napi_float32_array) ? 4 : 8;
    n *= width;
    data = base;
  } else if (napi_is_dataview(env, v, &is) == napi_ok && is) {
    napi_value ab; size_t offset;
    if (napi_get_dataview_info(env, v, &n, &data, &ab, &offset) != napi_ok) goto bad;
  } else {
    goto bad;
  }
  if (length) *length = n;
  return data;
bad:
  type_error(env, name, "an ArrayBuffer, a Buffer, a typed array, a DataView or null");
  return NULL;
}

/* A caller-owned struct: bytes_arg's memory, refused with a RangeError when it is smaller
 * than the struct or not aligned for it, since the ABI reads and writes the whole struct at
 * that address; null is a NULL pointer, which the ABI refuses with CAVALRY_ERR_NULL. */
static void *struct_arg(napi_env env, napi_value v, size_t size, size_t align, const char *name) {
  size_t n = 0;
  void *data = bytes_arg(env, v, &n, name);
  if (arg_failed || data == NULL) return data;
  if (n < size || (uintptr_t) data % align != 0) {
    char message[200];
    snprintf(message, sizeof message, "%s: %zu bytes aligned to %zu expected, the struct's size and alignment; %zu "
             "given at an address %s", name, size, align, n, (uintptr_t) data % align != 0 ? "not aligned" : "aligned");
    napi_throw_range_error(env, NULL, message);
    arg_failed = true;
    return NULL;
  }
  return data;
}

static const char *string_arg(napi_env env, napi_value v, char *buf, size_t cap, const char *name) {
  if (is_nullish(env, v)) return NULL;
  size_t n = 0;
  if (napi_get_value_string_utf8(env, v, buf, cap, &n) != napi_ok) {
    type_error(env, name, "a string or null"); string_failed = true; return NULL;
  }
  if (n + 1 >= cap) {
    char message[160];
    snprintf(message, sizeof message, "%s: a string under %zu bytes expected", name, cap);
    napi_throw_range_error(env, NULL, message); string_failed = true; return NULL;
  }
  return buf;
}

/* An int64: a number, or a BigInt, which carries every int64 exactly; a BigInt outside the
 * int64 range is refused with a RangeError, since N-API would wrap it to another value. */
static int64_t int64_arg(napi_env env, napi_value v, const char *name) {
  int64_t n = 0; bool lossless;
  napi_valuetype t;
  if (napi_typeof(env, v, &t) == napi_ok && t == napi_bigint) {
    if (napi_get_value_bigint_int64(env, v, &n, &lossless) == napi_ok) {
      if (lossless) return n;
      char message[160];
      snprintf(message, sizeof message, "%s: a BigInt from -(2n ** 63n) to 2n ** 63n - 1n expected, an int64", name);
      napi_throw_range_error(env, NULL, message);
      arg_failed = true;
      return 0;
    }
  } else if (napi_get_value_int64(env, v, &n) == napi_ok) {
    return n;
  }
  type_error(env, name, "a number or a BigInt"); return 0;
}

static int32_t int32_arg(napi_env env, napi_value v, const char *name) {
  int32_t n = 0;
  if (napi_get_value_int32(env, v, &n) != napi_ok) { type_error(env, name, "a number"); return 0; }
  return n;
}

static size_t size_arg(napi_env env, napi_value v, const char *name) {
  int64_t n = int64_arg(env, v, name);
  if (!arg_failed && n < 0) { type_error(env, name, "a non-negative size"); return 0; }
  return (size_t) n;
}

static bool bool_arg(napi_env env, napi_value v, const char *name) {
  bool b = false;
  if (napi_get_value_bool(env, v, &b) != napi_ok) { type_error(env, name, "a boolean"); return false; }
  return b;
}

static napi_value make_int32(napi_env env, int32_t n) { napi_value v; napi_create_int32(env, n, &v); return v; }
/* An int64 as a JS number: exact below 2^53 in magnitude, which every position, length,
 * count and id of the ABI is. The header's reserved value is none of these, so it goes
 * through make_bigint64. */
static napi_value make_int64(napi_env env, int64_t n) { napi_value v; napi_create_int64(env, n, &v); return v; }
/* An int64 as a BigInt, exact over the whole int64 range: the header's reserved value, any
 * int64 a publisher sets (spec/c-abi.md, cavalry_header_values_frame_t). */
static napi_value make_bigint64(napi_env env, int64_t n) { napi_value v; napi_create_bigint_int64(env, n, &v); return v; }
static napi_value make_bool(napi_env env, bool b) { napi_value v; napi_get_boolean(env, b, &v); return v; }
static napi_value make_null(napi_env env) { napi_value v; napi_get_null(env, &v); return v; }
static napi_value make_string(napi_env env, const char *s) {
  if (s == NULL) return make_null(env);
  napi_value v; napi_create_string_utf8(env, s, NAPI_AUTO_LENGTH, &v); return v;
}
static napi_value make_external(napi_env env, void *p) {
  if (p == NULL) return make_null(env);
  napi_value v; napi_create_external(env, p, NULL, NULL, &v); return v;
}

/* N-API's statuses by number, for the Error a poll reports when the runtime refused a step
 * of a fragment's call; by number, since the header of an older Node lacks the later names. */
static const char *const status_names[] = {
  "napi_ok", "napi_invalid_arg", "napi_object_expected", "napi_string_expected", "napi_name_expected",
  "napi_function_expected", "napi_number_expected", "napi_boolean_expected", "napi_array_expected",
  "napi_generic_failure", "napi_pending_exception", "napi_cancelled", "napi_escape_called_twice",
  "napi_handle_scope_mismatch", "napi_callback_scope_mismatch", "napi_queue_full", "napi_closing",
  "napi_bigint_expected", "napi_date_expected", "napi_arraybuffer_expected",
  "napi_detachable_arraybuffer_expected", "napi_would_deadlock", "napi_no_external_buffers_allowed",
  "napi_cannot_run_js",
};
#define NO_EXTERNAL_BUFFERS 22 /* napi_no_external_buffers_allowed: a runtime built with V8's sandbox */

/* External Buffers made on this thread, received bodies and claims' views, whose native
 * objects Node has not freed yet. Node frees them when its event loop turns, not at the
 * detach, and then calls buffer_freed, each Buffer's finalizer, which counts them back
 * (clients/node/README.md, "The runtime's queue"). Per thread, as each worker thread's
 * Buffers are freed on its own loop. */
static _Thread_local int64_t buffers_held;

static void buffer_freed(napi_env env, void *data, void *hint) {
  (void) env; (void) data; (void) hint;
  buffers_held--;
}

/* What a poll reports as the cause: the first fragment of the poll that did not reach the
 * handler cleanly, by the handler's exception or by an N-API call that failed. */
typedef enum { FIRST_NONE, FIRST_EXCEPTION, FIRST_FAILURE } first_t;

/* The per-subscription state the trampoline needs, made by registerSubscription and freed
 * with the external that holds it. */
typedef struct {
  napi_env env;                      /* valid during a poll */
  napi_ref header_ref;               /* the JS Header object the handler receives; weak, see registerSubscription */
  napi_value handler;                /* the handler of the poll in progress, else NULL; see fn_poll */
  napi_ref holder_ref;               /* an object whose element 0 holds the poll's first exception */
  uint32_t raised;                   /* handler calls that threw in this poll */
  uint32_t undelivered;              /* fragments of this poll the handler never saw */
  first_t first;                     /* what this poll reports as the cause */
  const char *failed;                /* FIRST_FAILURE: what the addon could not do, by which N-API call */
  int failed_status;                 /* FIRST_FAILURE: that call's napi_status */
  bool failed_frame;                 /* FIRST_FAILURE: whether the frame's two fields below were read */
  int32_t failed_session_id;
  int32_t failed_term_offset;
  int64_t failed_position;           /* FIRST_FAILURE of a committed log: the entry's position, as the ABI gives it */
  int64_t undelivered_at;            /* a committed log, while undelivered is above 0: the position of the poll's first
                                      * entry the handler never saw, where the service opens the log again */
  const cavalry_header_t *current;   /* the frame's header during a handler call */
  cavalry_header_values_t values;    /* read once per fragment, on the first field read */
  bool values_read;
  bool polling;
  bool positions;                    /* a committed log's state: the handler's third argument is the entry's position */
} subscription_state_t;

/* The first fragment of a poll that did not reach the handler cleanly for want of an N-API
 * call: what the addon could not do, the call's status and the frame, read through the ABI,
 * which needs no N-API, so that poll reports it once the ABI has returned; for a committed
 * log's entry, its position too, which the ABI gives without N-API as well. A later one is
 * counted by the caller and not kept. */
static void keep_failure(subscription_state_t *s, const cavalry_header_t *header, const char *what, napi_status status) {
  if (s->first != FIRST_NONE) return;
  s->first = FIRST_FAILURE;
  s->failed = what;
  s->failed_status = (int) status;
  cavalry_header_values_t values;
  s->failed_frame = cavalry_header_values(header, &values) == 0;
  if (s->failed_frame) {
    s->failed_session_id = values.frame.session_id;
    s->failed_term_offset = values.frame.term_offset;
  }
  s->failed_position = s->positions ? cavalry_committed_log_entry_position(header) : -1;
}

/* A fragment the handler never saw: the ABI has consumed it, so it is counted, and poll
 * reports it (invariant 1: a message is delivered once, or its absence is reported). For a
 * committed log the first such entry's position is kept too: the trampoline hands the handler
 * no later entry of the poll, and the service opens the log again there. */
static void undelivered(subscription_state_t *s, const cavalry_header_t *header, const char *what, napi_status status) {
  if (s->positions && s->undelivered == 0) s->undelivered_at = cavalry_committed_log_entry_position(header);
  s->undelivered++;
  keep_failure(s, header, what, status);
}

/* The handler threw on a fragment: counted, and the poll's first exception kept as thrown,
 * whatever its type, as element 0 of the subscription's holder object. A reference to the
 * value itself would refuse a string or a number at this N-API version, and a later Error
 * would then become the first. */
static void threw(subscription_state_t *s, const cavalry_header_t *header, napi_value exception) {
  s->raised++;
  if (s->first != FIRST_NONE) return;
  napi_value holder = NULL;
  napi_status status = napi_get_reference_value(s->env, s->holder_ref, &holder);
  if (status != napi_ok || holder == NULL) {
    keep_failure(s, header, "read the holder of the handler's exception (napi_get_reference_value)",
                 status == napi_ok ? napi_generic_failure : status);
  } else if ((status = napi_set_element(s->env, holder, 0, exception)) != napi_ok) {
    keep_failure(s, header, "keep the handler's exception (napi_set_element)", status);
  } else {
    s->first = FIRST_EXCEPTION;
  }
}

/* The ABI calls this for each fragment: the body as a Buffer over the log's bytes, no copy,
 * and the subscription's one Header object, whose fields read through headerField. The
 * handler's exception is N-API's pending exception when the call returns: the first is kept,
 * every one is counted, and the handler goes on being called for the poll's later fragments.
 * A fragment the addon cannot hand to the handler, because N-API refused a step of the call,
 * is counted too, and the first such failure kept, so that no fragment is consumed unseen
 * and unreported (clients/node/README.md, "The exception"). A committed log's entry after
 * such an entry is not handed to the handler either, and is counted the same way: a service
 * applies every entry in order, so it must never apply one after a gap, and it opens the log
 * again at the first entry it did not see (clients/node/README.md, "The committed log"). The
 * body is detached after the call, so a kept body reads as empty. */
static void trampoline(void *clientd, const uint8_t *buffer, size_t length, const cavalry_header_t *header) {
  subscription_state_t *s = clientd;
  if (s->positions && s->undelivered != 0) {
    s->undelivered++;
    return;
  }
  napi_env env = s->env;
  napi_handle_scope scope;
  napi_status status = napi_open_handle_scope(env, &scope);
  if (status != napi_ok) {
    undelivered(s, header, "open a handle scope (napi_open_handle_scope)", status);
    return;
  }
  napi_value body = NULL, hdr = NULL, undefined = NULL, result = NULL, position = NULL;
  status = napi_create_external_buffer(env, length, (void *) buffer, buffer_freed, NULL, &body);
  if (status == napi_ok) buffers_held++;
  if (status != napi_ok) {
    body = NULL;
    undelivered(s, header, "make the body's Buffer (napi_create_external_buffer)", status);
  } else if ((status = napi_get_reference_value(env, s->header_ref, &hdr)) != napi_ok || hdr == NULL) {
    undelivered(s, header, "read the Header the handler is handed (napi_get_reference_value)",
                status == napi_ok ? napi_generic_failure : status);
  } else if ((status = napi_get_undefined(env, &undefined)) != napi_ok) {
    undelivered(s, header, "read undefined for the handler's this (napi_get_undefined)", status);
  } else if (s->positions &&
             (status = napi_create_int64(env, cavalry_committed_log_entry_position(header), &position)) != napi_ok) {
    /* A committed log's entry: its position in the log, which the ABI gives for the header
     * its own poll handed over, as a number, as every position this client returns. */
    undelivered(s, header, "make the entry's position (napi_create_int64)", status);
  } else {
    s->current = header;
    s->values_read = false;
    napi_value args[3] = { body, hdr, position };
    status = napi_call_function(env, undefined, s->handler, s->positions ? 3 : 2, args, &result);
    s->current = NULL;
    if (status != napi_ok) {
      /* An exception pending means the handler ran and threw; none means N-API refused the
       * call before the handler ran. When N-API cannot say, the fragment counts as unseen. */
      bool pending = false;
      napi_value exception;
      napi_status asked = napi_is_exception_pending(env, &pending);
      if (asked != napi_ok) {
        undelivered(s, header, "learn whether the handler threw (napi_is_exception_pending)", asked);
      } else if (!pending) {
        undelivered(s, header, "call the handler (napi_call_function)", status);
      } else if ((status = napi_get_and_clear_last_exception(env, &exception)) != napi_ok) {
        s->raised++;
        keep_failure(s, header, "read the handler's exception (napi_get_and_clear_last_exception)", status);
      } else {
        threw(s, header, exception);
      }
    }
  }
  if (body != NULL) {
    napi_typedarray_type t; size_t n; void *data; napi_value ab; size_t offset;
    if (napi_get_typedarray_info(env, body, &t, &n, &data, &ab, &offset) == napi_ok) napi_detach_arraybuffer(env, ab);
  }
  napi_close_handle_scope(env, scope);
}

static void free_state(napi_env env, void *data, void *hint) {
  (void) hint;
  subscription_state_t *s = data;
  if (s->header_ref) napi_delete_reference(env, s->header_ref);
  if (s->holder_ref) napi_delete_reference(env, s->holder_ref);
  free(s);
}

/* registerSubscription(header): the subscription's state, holding a reference to its Header
 * and the holder object a poll keeps its first exception in, made here once per subscription
 * so that a throwing poll makes nothing new to keep it. The reference to the Header is weak,
 * a count of 0: the Header holds the external that holds the state, so a strong one would
 * root the Header, and with it the state, for the life of the process. The Subscription
 * holds its Header for every poll, so the weak reference reads it whenever a poll runs; once
 * the program drops the Subscription, a collection frees the Header and the external, whose
 * finalizer, free_state, frees the state. `function` is the addon function's own name, and
 * `owner` whose state it is, for the errors it throws. */
static napi_value register_state(napi_env env, napi_callback_info info, bool positions, const char *function,
                                 const char *owner) {
  size_t argc = 1; napi_value argv[1];
  char message[192];
  if (!read_args(env, info, 1, &argc, argv, function)) return NULL;
  subscription_state_t *s = calloc(1, sizeof *s);
  if (s == NULL) {
    snprintf(message, sizeof message, "%s: out of memory", function);
    napi_throw_error(env, NULL, message);
    return NULL;
  }
  s->positions = positions;
  if (napi_create_reference(env, argv[0], 0, &s->header_ref) != napi_ok) {
    free(s);
    snprintf(message, sizeof message, "%s Header object", owner);
    type_error(env, "header", message);
    return NULL;
  }
  napi_value holder, v;
  if (napi_create_object(env, &holder) != napi_ok || napi_create_reference(env, holder, 1, &s->holder_ref) != napi_ok ||
      napi_create_external(env, s, free_state, NULL, &v) != napi_ok) {
    free_state(env, s, NULL);
    snprintf(message, sizeof message, "%s: N-API could not make %s state; the runtime is out of memory or shutting "
             "down", function, owner);
    napi_throw_error(env, NULL, message);
    return NULL;
  }
  return v;
}

static napi_value fn_registerSubscription(napi_env env, napi_callback_info info) {
  return register_state(env, info, false, "registerSubscription", "the subscription's");
}

/* registerCommittedLog(header): a committed log's state, a subscription's state whose
 * trampoline hands the handler a third argument, the entry's position in the log
 * (spec/c-abi.md, "A frame's position"; ADR 0070). */
static napi_value fn_registerCommittedLog(napi_env env, napi_callback_info info) {
  return register_state(env, info, true, "registerCommittedLog", "the committed log's");
}

/* The Error a poll reports for its first fragment that N-API kept from the handler: what the
 * addon could not do, by which call, N-API's status by number and name, the frame's session
 * and term offset, and what to do; the three numbers are its properties too. A committed
 * log's names the entry by its position as well, its `position` property, and, when an entry
 * of the poll never reached the handler, says to open the log again at the first such entry,
 * since the trampoline handed the handler no later one. NULL when N-API cannot make it
 * either. */
static napi_value failure_error(napi_env env, const subscription_state_t *s) {
  char frame[160];
  char reopen[224];
  char message[1024];
  const char *one = s->positions ? "entry" : "fragment";
  if (s->positions && s->failed_frame) {
    snprintf(frame, sizeof frame, "the entry at position %lld, of session %d at term offset %d",
             (long long) s->failed_position, (int) s->failed_session_id, (int) s->failed_term_offset);
  } else if (s->positions) {
    snprintf(frame, sizeof frame, "the entry at position %lld, whose header cavalry_header_values could not read",
             (long long) s->failed_position);
  } else if (s->failed_frame) {
    snprintf(frame, sizeof frame, "the fragment of session %d at term offset %d", (int) s->failed_session_id,
             (int) s->failed_term_offset);
  } else {
    snprintf(frame, sizeof frame, "a fragment whose header cavalry_header_values could not read");
  }
  if (s->positions && s->undelivered != 0) {
    snprintf(reopen, sizeof reopen, "The ABI consumed the entry, and the handler was handed no entry of this poll "
             "from position %lld on: close the log and open a new one at position %lld", (long long) s->undelivered_at,
             (long long) s->undelivered_at);
  } else {
    snprintf(reopen, sizeof reopen, "The ABI consumed the %s, so polling again does not deliver it", one);
  }
  int known = (int) (sizeof status_names / sizeof status_names[0]);
  const char *name = s->failed_status >= 0 && s->failed_status < known ? status_names[s->failed_status] :
                     "a status this addon does not name";
  const char *next = s->failed_status == NO_EXTERNAL_BUFFERS ?
      "this runtime forbids external buffers, as a Node or an Electron built with V8's sandbox does, and the "
      "client needs one to hand the handler the log's bytes without a copy: run the client on a Node built "
      "without the sandbox" :
      "the runtime refused a call the addon makes for every fragment: call poll on the thread that made the "
      "client, on a Node that package.json's engines admits";
  snprintf(message, sizeof message, "the addon could not %s for %s: N-API status %d, %s; %s. %s. See "
           "clients/node/README.md#the-exception", s->failed, frame, s->failed_status, name, next, reopen);
  napi_value text, error;
  if (napi_create_string_utf8(env, message, NAPI_AUTO_LENGTH, &text) != napi_ok ||
      napi_create_error(env, NULL, text, &error) != napi_ok) {
    return NULL;
  }
  napi_set_named_property(env, error, "napiStatus", make_int32(env, s->failed_status));
  if (s->failed_frame) {
    napi_set_named_property(env, error, "sessionId", make_int32(env, s->failed_session_id));
    napi_set_named_property(env, error, "termOffset", make_int32(env, s->failed_term_offset));
  }
  if (s->positions) napi_set_named_property(env, error, "position", make_int64(env, s->failed_position));
  return error;
}

/* What poll returns when a fragment of the poll did not reach the handler cleanly:
 * [count, raised, first, undelivered, undeliveredAt], `first` the handler's first exception
 * or the Error failure_error makes, whichever came first, and `undeliveredAt` a committed
 * log's position of the poll's first entry the handler never saw, else null; the state is
 * cleared for the next poll. */
static napi_value report(napi_env env, subscription_state_t *s, int count) {
  napi_value tuple, first = NULL, holder = NULL;
  if (s->first == FIRST_EXCEPTION) {
    if (napi_get_reference_value(env, s->holder_ref, &holder) == napi_ok && holder != NULL) {
      if (napi_get_element(env, holder, 0, &first) != napi_ok) first = NULL;
      napi_value undefined;
      if (napi_get_undefined(env, &undefined) == napi_ok) napi_set_element(env, holder, 0, undefined); /* not kept past the poll */
    }
  } else if (s->first == FIRST_FAILURE) {
    first = failure_error(env, s);
  }
  if (first == NULL) first = make_null(env);
  napi_create_array_with_length(env, 5, &tuple);
  napi_set_element(env, tuple, 0, make_int32(env, count));
  napi_set_element(env, tuple, 1, make_int32(env, (int32_t) s->raised));
  napi_set_element(env, tuple, 2, first);
  napi_set_element(env, tuple, 3, make_int32(env, (int32_t) s->undelivered));
  napi_set_element(env, tuple, 4, s->positions && s->undelivered != 0 ? make_int64(env, s->undelivered_at) : make_null(env));
  s->first = FIRST_NONE; s->raised = 0; s->undelivered = 0;
  return tuple;
}

/* poll(state, subscription, handler, fragmentLimit): the one cavalry_subscription_poll call.
 * Returns the count the ABI returned, or, when a fragment did not reach the handler cleanly,
 * what report makes. From a handler of this subscription's own poll it passes the call to the
 * ABI, which refuses it with -1 and CAVALRY_ERR_POLLING, and touches nothing. The state holds
 * the handler's napi_value for the poll, no reference: the value is valid until this call
 * returns, and every trampoline call, a nested one included, runs inside it. */
static napi_value fn_poll(napi_env env, napi_callback_info info) {
  size_t argc = 4; napi_value argv[4];
  if (!read_args(env, info, 4, &argc, argv, "poll")) return NULL;
  subscription_state_t *s = external_arg(env, argv[0], "state");
  if (arg_failed || s == NULL) { if (!arg_failed) type_error(env, "state", "the subscription's state"); return NULL; }
  cavalry_subscription_t *subscription = external_arg(env, argv[1], "subscription");
  if (arg_failed) return NULL;
  size_t limit = size_arg(env, argv[3], "fragmentLimit");
  if (arg_failed) return NULL;
  if (s->polling) return make_int32(env, cavalry_subscription_poll(subscription, trampoline, s, limit));
  napi_valuetype t;
  if (napi_typeof(env, argv[2], &t) != napi_ok || t != napi_function) { type_error(env, "handler", "a function"); return NULL; }
  s->env = env; s->handler = argv[2]; s->polling = true;
  s->first = FIRST_NONE; s->raised = 0; s->undelivered = 0;
  int count = cavalry_subscription_poll(subscription, trampoline, s, limit);
  s->polling = false; s->handler = NULL;
  if (s->raised == 0 && s->undelivered == 0) return make_int32(env, count);
  return report(env, s, count);
}

/* committedLogPoll(state, log, handler, fragmentLimit): the one cavalry_committed_log_poll
 * call, as poll is the subscription's: the count the ABI returned, or what report makes when
 * an entry did not reach the handler cleanly; from a handler of this log's own poll the call
 * goes to the ABI, which refuses it with -1 and CAVALRY_ERR_POLLING, and touches nothing. */
static napi_value fn_committedLogPoll(napi_env env, napi_callback_info info) {
  size_t argc = 4; napi_value argv[4];
  if (!read_args(env, info, 4, &argc, argv, "committedLogPoll")) return NULL;
  subscription_state_t *s = external_arg(env, argv[0], "state");
  if (arg_failed || s == NULL) { if (!arg_failed) type_error(env, "state", "the committed log's state"); return NULL; }
  cavalry_committed_log_t *log = external_arg(env, argv[1], "log");
  if (arg_failed) return NULL;
  size_t limit = size_arg(env, argv[3], "fragmentLimit");
  if (arg_failed) return NULL;
  if (s->polling) return make_int32(env, cavalry_committed_log_poll(log, trampoline, s, limit));
  napi_valuetype t;
  if (napi_typeof(env, argv[2], &t) != napi_ok || t != napi_function) { type_error(env, "handler", "a function"); return NULL; }
  s->env = env; s->handler = argv[2]; s->polling = true;
  s->first = FIRST_NONE; s->raised = 0; s->undelivered = 0;
  int count = cavalry_committed_log_poll(log, trampoline, s, limit);
  s->polling = false; s->handler = NULL;
  if (s->raised == 0 && s->undelivered == 0) return make_int32(env, count);
  return report(env, s, count);
}

/* headerField(state, index): the current frame's header field, read through
 * cavalry_header_values once per fragment on the first read; undefined outside a handler
 * call, since the header is valid for the call alone (spec/c-abi.md, "Lifetimes"). */
static napi_value fn_headerField(napi_env env, napi_callback_info info) {
  size_t argc = 2; napi_value argv[2];
  if (!read_args(env, info, 2, &argc, argv, "headerField")) return NULL;
  subscription_state_t *s = external_arg(env, argv[0], "state");
  if (arg_failed || s == NULL) { if (!arg_failed) type_error(env, "state", "the subscription's state"); return NULL; }
  int32_t index = int32_arg(env, argv[1], "index");
  if (arg_failed) return NULL;
  if (s->current == NULL) { napi_value u; napi_get_undefined(env, &u); return u; }
  if (!s->values_read) {
    if (cavalry_header_values(s->current, &s->values) != 0) { napi_value u; napi_get_undefined(env, &u); return u; }
    s->values_read = true;
  }
  const cavalry_header_values_frame_t *f = &s->values.frame;
  switch (index) {
    case 0: return make_int32(env, f->frame_length);
    case 1: return make_int32(env, f->version);
    case 2: return make_int32(env, f->flags);
    case 3: return make_int32(env, f->type);
    case 4: return make_int32(env, f->term_offset);
    case 5: return make_int32(env, f->session_id);
    case 6: return make_int32(env, f->stream_id);
    case 7: return make_int32(env, f->term_id);
    case 8: return make_bigint64(env, f->reserved_value);
    default: { napi_value u; napi_get_undefined(env, &u); return u; }
  }
}

/* view(claim): a Buffer over the slice the claim struct names, no copy, or null when the
 * struct names none. */
static napi_value fn_view(napi_env env, napi_callback_info info) {
  size_t argc = 1; napi_value argv[1];
  if (!read_args(env, info, 1, &argc, argv, "view")) return NULL;
  size_t n = 0;
  cavalry_buffer_claim_t *claim = bytes_arg(env, argv[0], &n, "claim");
  if (arg_failed) return NULL;
  if (claim == NULL || n < sizeof *claim || claim->data == NULL) return make_null(env);
  napi_value body;
  if (napi_create_external_buffer(env, claim->length, claim->data, buffer_freed, NULL, &body) != napi_ok) return make_null(env);
  buffers_held++;
  return body;
}

/* buffersHeld(): buffers_held, the external Buffers of this thread Node has not freed yet. */
static napi_value fn_buffersHeld(napi_env env, napi_callback_info info) {
  (void) info;
  return make_int64(env, buffers_held);
}

/* detach(view): releases a view returned by view or handed to a handler; a kept view then
 * reads as empty. */
static napi_value fn_detach(napi_env env, napi_callback_info info) {
  size_t argc = 1; napi_value argv[1];
  if (!read_args(env, info, 1, &argc, argv, "detach")) return NULL;
  if (!is_nullish(env, argv[0])) {
    napi_typedarray_type t; size_t n; void *data; napi_value ab; size_t offset;
    if (napi_get_typedarray_info(env, argv[0], &t, &n, &data, &ab, &offset) == napi_ok) napi_detach_arraybuffer(env, ab);
  }
  napi_value u; napi_get_undefined(env, &u); return u;
}

/* stringAt(struct, offset): the C string a pointer field of a constants struct names, as a
 * JS string, or null. */
static napi_value fn_stringAt(napi_env env, napi_callback_info info) {
  size_t argc = 2; napi_value argv[2];
  if (!read_args(env, info, 2, &argc, argv, "stringAt")) return NULL;
  size_t n = 0;
  char *base = bytes_arg(env, argv[0], &n, "struct");
  if (arg_failed) return NULL;
  int64_t offset = int64_arg(env, argv[1], "offset");
  if (arg_failed) return NULL;
  if (base == NULL || offset < 0 || (size_t) offset + sizeof(char *) > n) return make_null(env);
  const char *s; memcpy(&s, base + offset, sizeof s);
  return make_string(env, s);
}
'''


# ------------------------------------------------------------------ the package

def write_node(header) -> dict[str, str]:
    lay = layout(header)
    defines = [(n, v) for n, v in header.defines if n != "CAVALRY_H"]
    version = dict(header.defines).get("CAVALRY_ABI_VERSION", "0")
    constants = "\n".join(f"  {n}: {js_value(v)}," for n, v in defines)
    exported = "\n".join(f"  {n}: {('bigint' if js_value(v).endswith('n') else 'string' if v.startswith(chr(34)) else 'number')};"
                         for n, v in defines)
    layout_js = json.dumps({name: {"size": l["size"], "fields": {f: {"offset": d["offset"], "kind": d["kind"] if isinstance(d["kind"], str) else d["kind"][1]} for f, d in l["fields"].items()}} for name, l in lay.items()}, indent=2)
    index = NODE_CLIENT.replace("@ABI_VERSION@", version).replace("@CONSTANTS@", constants).replace("@LAYOUT@", layout_js)
    types = NODE_TYPES.replace("@ABI_VERSION@", version).replace("@CONSTANTS@", exported)
    return {
        "src/addon.c": write_addon(header),
        "binding.gyp": BINDING_GYP,
        "package.json": PACKAGE_JSON,
        "index.js": index,
        "index.d.ts": types,
    }


BINDING_GYP = '''{
  "targets": [
    {
      "target_name": "cavalry_addon",
      "sources": ["src/addon.c"],
      "include_dirs": ["../../crates/cavalry-ffi/include"],
      "libraries": ["<(module_root_dir)/../../target/release/libcavalry_ffi.a"],
      "cflags": ["-std=c11", "-Wall", "-Wextra"],
      "conditions": [
        ["OS=='mac'", {
          "xcode_settings": {"OTHER_CFLAGS": ["-std=c11", "-Wall", "-Wextra"]},
          "link_settings": {"libraries": ["-framework CoreFoundation", "-framework Security", "-lresolv"]}
        }],
        ["OS=='linux'", {
          "link_settings": {"libraries": ["-lpthread", "-ldl", "-lm"]}
        }]
      ]
    }
  ]
}
'''

PACKAGE_JSON = '''{
  "name": "@cavalry/client",
  "version": "0.1.0",
  "description": "The Node client over Cavalry's C ABI: a native addon over N-API, generated from the committed header (clients/node/README.md).",
  "main": "index.js",
  "types": "index.d.ts",
  "gypfile": true,
  "private": true,
  "scripts": {
    "install": "node-gyp rebuild"
  },
  "engines": {
    "node": ">=20"
  }
}
'''

NODE_CLIENT = r'''"use strict";
/**
 * The Node client over the C ABI of spec/c-abi.md (clients/node/README.md).
 *
 * Generated by clients/generate.py node; do not edit by hand, `just lint` regenerates and
 * compares. A `Client` wraps a driver's directory; its `Publication`, `Subscription`,
 * `Recording` and `Replay` wrap the ABI's handles through the addon in build/Release. Every
 * message goes through one ABI call on the caller's thread: `tryClaim`, `offer`, `commit`,
 * `abort`, `setReservedValue` and `poll` hold invariant 2's rules, no lock, no `await`, no
 * promise, no throw (the handler's own exception is thrown once `poll` has returned), and
 * `clients/lint.py` reads them for JavaScript's forms of each. Anything that is not a message,
 * an add, a command, a close, throws `CavalryError` with the ABI's code and message.
 */

const path = require("path");
const addon = require(path.join(__dirname, "build", "Release", "cavalry_addon.node"));

const ABI_VERSION = @ABI_VERSION@;

/** The header's constants, as the ABI names them (spec/c-abi.md). */
const constants = {
@CONSTANTS@
};

/** The result codes a message path returns instead of throwing (spec/c-abi.md, "Result
 * codes"), under the names Aeron's C client uses without the prefix. */
const NOT_CONNECTED = constants.CAVALRY_PUBLICATION_NOT_CONNECTED;
const BACK_PRESSURED = constants.CAVALRY_PUBLICATION_BACK_PRESSURED;
const ADMIN_ACTION = constants.CAVALRY_PUBLICATION_ADMIN_ACTION;
const CLOSED = constants.CAVALRY_PUBLICATION_CLOSED;
const MAX_POSITION_EXCEEDED = constants.CAVALRY_PUBLICATION_MAX_POSITION_EXCEEDED;
const ERROR = constants.CAVALRY_PUBLICATION_ERROR;
const NULL_POSITION = constants.CAVALRY_NULL_POSITION;

/** The structs' sizes and field offsets, as C lays them out on the 64-bit targets. */
const LAYOUT = @LAYOUT@;

const HEADER_FIELDS = ["frameLength", "version", "flags", "type", "termOffset", "sessionId", "streamId", "termId", "reservedValue"];

class CavalryError extends Error {
  /** A call that is not a message failed: `code` and `message` are the ABI's (`cavalry_errcode`,
   * `cavalry_errmsg`), the message naming what to do next and its docs anchor; `fn` is the ABI
   * function that failed. */
  constructor(fn, code, message) {
    super(`${fn}: ${message} (error ${code})`);
    this.name = "CavalryError";
    this.fn = fn;
    this.code = code;
    this.abiMessage = message;
  }
}

class HandlerError extends Error {
  /** A fragment of a poll did not reach the handler cleanly, or an entry of a committed log's
   * poll when `committedLog` is true: `fragments` is the count the poll returned once the ABI
   * was done, of fragments or of entries, or -1 when the poll also stopped on a fault, which
   * the `fault()` of the object that was polled reads until its next poll; `raised` how many
   * handler calls threw, `undelivered` how many fragments the handler never saw because the
   * runtime refused a step of the call, and `cause` the first of these, the handler's
   * exception as thrown or an Error naming the N-API call that failed. The ABI consumed every
   * one of the poll's fragments. A committed log hands its handler no entry of a poll after
   * one the handler never saw, and counts those in `undelivered` too: `undeliveredAt` is the
   * position of the first, where the service opens the log again, or null when every entry
   * reached the handler. */
  constructor(fragments, raised, cause, undelivered = 0, committedLog = false, undeliveredAt = null) {
    const one = committedLog ? "entry" : "fragment";
    const many = committedLog ? "entries" : "fragments";
    // The fault of a committed log's own image is one poll's to return; the log keeps it
    // (clients/node/README.md, "The committed log").
    const count = committedLog ? `the ${fragments} ${fragments === 1 ? one : many} this poll delivered` :
      `the ${fragments} fragment(s) this poll delivered`;
    const fault = committedLog ?
      "the entries this poll delivered before it stopped on a fault, which this CommittedLog's fault() reads " +
      "until its next poll, and its faulted() from then on when the fault is the log's own image's" :
      "the fragments this poll delivered before it stopped on a fault, which subscription.fault() reads";
    const delivered = fragments >= 0 ? count : fault;
    const what = undelivered === 0 ?
      `the ${one} handler threw on ${raised} of ${delivered}; every ${one} reached the handler once, in ` +
      `order. Fix the handler: its first exception, ${describe(cause)}, is this error's cause; polling again does ` +
      `not deliver these ${many} again.` :
      committedLog ?
      `${undelivered} of ${delivered} never reached the entry handler, from the entry at position ` +
      `${undeliveredAt} on, and the handler threw on ${raised} before it. The log's position() is past them, so ` +
      `close this log and open a new one at position ${undeliveredAt}, or at the position of an earlier entry ` +
      `the handler could not apply. The first failure, ${describe(cause)}, is this error's cause and says why.` :
      `${undelivered} of ${delivered} never reached the ${one} handler, and the handler threw on ${raised}; the ` +
      `ABI consumed them, so polling again does not deliver them. The first failure, ${describe(cause)}, is this ` +
      `error's cause and says what to do.`;
    super(`${what} See clients/node/README.md#the-exception`, { cause });
    this.name = "HandlerError";
    this.fragments = fragments;
    this.raised = raised;
    this.undelivered = undelivered;
    this.committedLog = committedLog;
    this.undeliveredAt = undeliveredAt;
  }
}

/** A thrown value as text for a message: `String` of it, or its tag when it has no string
 * form, such as an object without a prototype. */
function describe(value) {
  try {
    return String(value);
  } catch (_) {
    return Object.prototype.toString.call(value);
  }
}

// The load-time check (spec/c-abi.md, "Versions and what a reader rejects"): the addon is
// linked with the library of the checkout it was built in, and a package generated from a
// header of another version is refused here, before any other function is called.
{
  const found = addon.cavalry_abi_version();
  if (found !== ABI_VERSION) {
    throw new CavalryError("cavalry_abi_version", constants.CAVALRY_ERR_ABI_VERSION,
      `the library the addon links is ABI version ${found} and this package was generated from a header of ABI ` +
      `version ${ABI_VERSION}; rebuild the addon against the library built from the same checkout (cargo build ` +
      `--release -p cavalry-ffi --lib, then npm install in clients/node) or regenerate the package from ` +
      `the library's header with python3 clients/generate.py node; see docs/reference/c-abi.md#versions-and-what-a-reader-rejects`);
  }
}

function error(fn) {
  return new CavalryError(fn, addon.cavalry_errcode(), addon.cavalry_errmsg() || "");
}

function check(fn, result) {
  if (result !== 0) throw error(fn);
}

/** Close `owner.handle` with the ABI's close `fn`, then forget the handle. Every close frees
 * its handle, whatever it returns (spec/c-abi.md, "Lifetimes"), except one the ABI refuses:
 * a subscription's close from inside a handler of its own poll is -1 with
 * CAVALRY_ERR_POLLING and leaves the subscription open ("Calls during a claim or a poll"),
 * so the wrapper keeps the handle, to poll it and close it later. */
function closeHandle(owner, fn) {
  const result = addon[fn](owner.handle);
  if (result !== 0 && addon.cavalry_errcode() === constants.CAVALRY_ERR_POLLING) throw error(fn);
  owner.handle = null;
  check(fn, result);
}

/** A caller-owned struct: its ArrayBuffer, a DataView over it, and typed reads by the layout. */
class Struct {
  constructor(name) {
    this.layout = LAYOUT[name];
    this.buffer = new ArrayBuffer(this.layout.size);
    this.view = new DataView(this.buffer);
  }
  int32(field) { return this.view.getInt32(this.layout.fields[field].offset, true); }
  int64(field) { return Number(this.view.getBigInt64(this.layout.fields[field].offset, true)); }
  size(field) { return Number(this.view.getBigUint64(this.layout.fields[field].offset, true)); }
  bool(field) { return this.view.getUint8(this.layout.fields[field].offset) !== 0; }
  string(field) { return addon.stringAt(this.buffer, this.layout.fields[field].offset); }
}

class Client {
  /** A client of the driver at `dir`, or at `CAVALRY_DIR`, or at the driver's default directory:
   * `cavalry_context_init_v` with this package's ABI version, then `cavalry_init`. `close` is
   * idempotent; `Symbol.dispose` lets `using` close it. */
  constructor(dir) {
    const [code, context] = addon.cavalry_context_init_v(ABI_VERSION);
    check("cavalry_context_init_v", code);
    try {
      if (dir !== undefined && dir !== null) check("cavalry_context_set_dir", addon.cavalry_context_set_dir(context, dir));
      const [initCode, client] = addon.cavalry_init(context);
      check("cavalry_init", initCode);
      this.handle = client;
    } finally {
      addon.cavalry_context_close(context);
    }
    this.dir = addon.cavalry_dir(this.handle);
    this.clientId = addon.cavalry_client_id(this.handle);
  }
  close() {
    if (this.handle !== null) closeHandle(this, "cavalry_close");
  }
  get isClosed() { return this.handle === null || addon.cavalry_is_closed(this.handle); }
  /** 0 while open, else why the client closed, `CAVALRY_CLOSED_*`. */
  closed() { return addon.cavalry_closed(this.handle); }
  _add(fn, channel, streamId) {
    const [code, handle] = addon[fn](this.handle, channel, streamId);
    check(fn, code);
    return handle;
  }
  addPublication(channel, streamId) { return new Publication(this._add("cavalry_add_publication", channel, streamId)); }
  addExclusivePublication(channel, streamId) { return new Publication(this._add("cavalry_add_exclusive_publication", channel, streamId)); }
  addSubscription(channel, streamId) { return new Subscription(this._add("cavalry_add_subscription", channel, streamId)); }
  startRecording(channel, streamId) { return new Recording(this._add("cavalry_start_recording", channel, streamId)); }
  stopRecording(recordingId) { check("cavalry_stop_recording", addon.cavalry_stop_recording(this.handle, recordingId)); }
  /** Adds the destination `channel` to `publication`'s control channel (ADR 0053). */
  addDestination(publication, channel) { check("cavalry_add_destination", addon.cavalry_add_destination(this.handle, publication.handle, channel)); }
  /** Removes the destination `channel`, as it was added. */
  removeDestination(publication, channel) { check("cavalry_remove_destination", addon.cavalry_remove_destination(this.handle, publication.handle, channel)); }
  truncateRecording(recordingId, position) { check("cavalry_truncate_recording", addon.cavalry_truncate_recording(this.handle, recordingId, position)); }
  purgeSegments(recordingId, newStartPosition) { check("cavalry_purge_segments", addon.cavalry_purge_segments(this.handle, recordingId, newStartPosition)); }
  purgeRecording(recordingId) { check("cavalry_purge_recording", addon.cavalry_purge_recording(this.handle, recordingId)); }
  startReplay(recordingId, position, length, replayStreamId, channel) {
    const [code, handle] = addon.cavalry_start_replay(this.handle, recordingId, position, length, replayStreamId, channel);
    check("cavalry_start_replay", code);
    return new Replay(handle);
  }
  stopReplay(replaySessionId) { check("cavalry_stop_replay", addon.cavalry_stop_replay(this.handle, replaySessionId)); }
  /** The committed log of the cluster member on this client's driver, opened at `position`, 0
   * or the `position()` an earlier `CommittedLog` reached: a `CommittedLog`, or null while the
   * driver hosts no member that has a recording of its log, so a service started before its
   * member asks again (spec/cluster.md, "The service"; spec/c-abi.md, "Functions"). */
  committedLog(position) {
    const [code, handle] = addon.cavalry_committed_log(this.handle, position);
    check("cavalry_committed_log", code);
    return handle === null ? null : new CommittedLog(handle, this);
  }
}
if (typeof Symbol.dispose === "symbol") Client.prototype[Symbol.dispose] = function dispose() { this.close(); };

class Publication {
  /** A publication: `tryClaim` and `offer` return the position after the frame or a code below
   * zero, never throwing; `claim` is the Buffer over the slice the last `tryClaim` reserved, from
   * the one `cavalry_buffer_claim_t` this object keeps, detached by `commit`, `abort` and `close`. */
  constructor(handle) {
    this.handle = handle;
    this._claim = new Struct("cavalry_buffer_claim_t");
    this.claim = null;
    const c = new Struct("cavalry_publication_constants_t");
    check("cavalry_publication_constants", addon.cavalry_publication_constants(handle, c.buffer));
    this.channel = c.string("channel");
    this.registrationId = c.int64("registration_id");
    this.sessionId = c.int32("session_id");
    this.streamId = c.int32("stream_id");
    this.maxPayloadLength = c.size("max_payload_length");
    this.maxMessageLength = c.size("max_message_length");
    this.isExclusive = c.bool("is_exclusive");
  }
  /** Claim `length` bytes: the position after the frame, with `claim` the writable Buffer over
   * them, or a result code below zero, which leaves `claim` as it was. One ABI call; the Buffer
   * is this client's one allocation per sent message (README, "The runtime's side"). */
  tryClaim(length) {
    const result = addon.cavalry_publication_try_claim(this.handle, length, this._claim.buffer);
    if (result >= 0) this.claim = addon.view(this._claim.buffer);
    return result;
  }
  _release() {
    if (this.claim !== null) { addon.detach(this.claim); this.claim = null; }
  }
  /** Commit the claim: the position after the frame, or -1. */
  commit() { this._release(); return addon.cavalry_buffer_claim_commit(this._claim.buffer); }
  /** Abort the claim, leaving padding: the position after the frame, or -1. */
  abort() { this._release(); return addon.cavalry_buffer_claim_abort(this._claim.buffer); }
  /** Set the reserved value the claim's frame header carries, any int64: a BigInt carries
   * every one exactly, a number those below 2^53 in magnitude, and a BigInt outside the int64
   * range is refused with RangeError. 0, or -1 with the ABI's error. */
  setReservedValue(value) { return addon.cavalry_buffer_claim_set_reserved_value(this._claim.buffer, value); }
  /** Offer `message`, a Buffer or any typed array: the position after it, or a code below zero. */
  offer(message) { return addon.cavalry_publication_offer(this.handle, message); }
  position() {
    const result = addon.cavalry_publication_position(this.handle);
    if (result < 0) throw error("cavalry_publication_position");
    return result;
  }
  positionLimit() { return addon.cavalry_publication_position_limit(this.handle); }
  get isConnected() { return addon.cavalry_publication_is_connected(this.handle); }
  get isClosed() { return addon.cavalry_publication_is_closed(this.handle); }
  /** Close the publication; the ABI's close aborts a claim still outstanding, and the claim's
   * Buffer is released first. The claim struct is zeroed before the ABI's close, so that
   * `commit`, `abort` and `setReservedValue` after it hand the ABI a struct that names no
   * publication, which it answers with -1 and CAVALRY_ERR_NO_CLAIM without reading the handle
   * the close freed (spec/c-abi.md, "Which claim a struct names"). */
  close() {
    if (this.handle !== null) {
      this._release();
      new Uint8Array(this._claim.buffer).fill(0);
      closeHandle(this, "cavalry_publication_close");
    }
  }
}

class Header {
  /** The frame header a poll hands the handler, valid for the call: each getter reads the field
   * through the addon, which reads `cavalry_header_values` once per fragment into the struct it
   * keeps for the subscription; outside the call every getter is undefined. Every field is a
   * number but `reservedValue`, a BigInt, since it is any int64 a publisher sets. */
  constructor(state) { this._state = state; }
  get frameLength() { return addon.headerField(this._state, 0); }
  get version() { return addon.headerField(this._state, 1); }
  get flags() { return addon.headerField(this._state, 2); }
  get type() { return addon.headerField(this._state, 3); }
  get termOffset() { return addon.headerField(this._state, 4); }
  get sessionId() { return addon.headerField(this._state, 5); }
  get streamId() { return addon.headerField(this._state, 6); }
  get termId() { return addon.headerField(this._state, 7); }
  get reservedValue() { return addon.headerField(this._state, 8); }
}

class Image {
  /** One of a subscription's images, valid until the subscription's next poll or close. */
  constructor(pointer) {
    this.pointer = pointer;
    const c = new Struct("cavalry_image_constants_t");
    check("cavalry_image_constants", addon.cavalry_image_constants(pointer, c.buffer));
    this.sourceIdentity = c.string("source_identity");
    this.correlationId = c.int64("correlation_id");
    this.positionBitsToShift = c.size("position_bits_to_shift");
    this.sessionId = c.int32("session_id");
    this.initialTermId = c.int32("initial_term_id");
  }
  position() { return addon.cavalry_image_position(this.pointer); }
  get isEndOfStream() { return addon.cavalry_image_is_end_of_stream(this.pointer); }
}

class Subscription {
  /** A subscription: `poll(handler, fragmentLimit)` calls `handler(body, header)` for each
   * fragment, `body` a Buffer into the log valid for the call and `header` the one `Header` this
   * object keeps, and returns the fragment count. A handler's exception is kept until the ABI has
   * returned, every later fragment still reaching the handler, then thrown as `HandlerError`, as
   * is a fragment the runtime kept from the handler, counted in its `undelivered`. */
  constructor(handle) {
    this.handle = handle;
    this._header = new Header(null);
    this._state = addon.registerSubscription(this._header);
    this._header._state = this._state;
    const c = new Struct("cavalry_subscription_constants_t");
    check("cavalry_subscription_constants", addon.cavalry_subscription_constants(handle, c.buffer));
    this.channel = c.string("channel");
    this.registrationId = c.int64("registration_id");
    this.streamId = c.int32("stream_id");
  }
  /** One ABI call; the handler's exception, if any, is thrown after it returns. */
  poll(handler, fragmentLimit) {
    const result = addon.poll(this._state, this.handle, handler, fragmentLimit);
    if (typeof result === "number") return result;
    throw new HandlerError(result[0], result[1], result[2], result[3]);
  }
  /** What the last poll stopped on: {code, sessionId, position}; code 0 for none. */
  fault() {
    const f = new Struct("cavalry_fault_t");
    check("cavalry_subscription_fault", addon.cavalry_subscription_fault(this.handle, f.buffer));
    return { code: f.int32("code"), sessionId: f.int32("session_id"), position: f.int64("position") };
  }
  imageCount() { return addon.cavalry_subscription_image_count(this.handle); }
  imageAtIndex(index) {
    const pointer = addon.cavalry_subscription_image_at_index(this.handle, index);
    return pointer === null ? null : new Image(pointer);
  }
  images() {
    const images = [];
    for (let i = 0; i < this.imageCount(); i++) images.push(this.imageAtIndex(i));
    return images;
  }
  get isConnected() { return addon.cavalry_subscription_is_connected(this.handle); }
  get isClosed() { return addon.cavalry_subscription_is_closed(this.handle); }
  /** Close the subscription. From inside a handler of its own poll the ABI refuses the close,
   * which throws CavalryError with CAVALRY_ERR_POLLING and leaves the subscription open. */
  close() {
    if (this.handle !== null) closeHandle(this, "cavalry_subscription_close");
  }
}

class CommittedLog {
  /** A service's read of a cluster's committed log, beside the member on the client's driver:
   * `poll(handler, fragmentLimit)` calls `handler(body, header, position)` for each entry a
   * quorum has committed, in order and none at or past the member's commit position, `body` a
   * Buffer into the log valid for the call, `header` the one `Header` this object keeps and
   * `position` the entry's position in the log, and returns the entry count, or the ABI's -1
   * when the poll stopped on a fault, which `fault()` reads until the next poll. A handler's
   * exception is kept until the ABI has returned and then thrown as `HandlerError`, as a
   * subscription's is; its entry was delivered and is not delivered again. `position()` is the
   * position after the last entry delivered: where a service opens the log again. `isEnded`
   * says when to stop reading and what tells a whole end from a short one. */
  constructor(handle, client) {
    this.handle = handle;
    this._client = client;
    this._header = new Header(null);
    this._state = addon.registerCommittedLog(this._header);
    this._header._state = this._state;
    const c = new Struct("cavalry_committed_log_constants_t");
    if (addon.cavalry_committed_log_constants(handle, c.buffer) !== 0) {
      // The log is open and no object will hold it: its error is read first, since the
      // close sets the thread's own, and then the log is closed, as the Go client closes
      // one it cannot return.
      const failed = error("cavalry_committed_log_constants");
      addon.cavalry_committed_log_close(handle, client.handle);
      this.handle = null;
      throw failed;
    }
    this.recordingId = c.int64("recording_id");
    this.memberId = c.int32("member_id");
  }
  /** One ABI call, and its result: the count of entries delivered, or -1 when the poll stopped
   * on a fault. `fault()` then reads the fault, until the next poll: a code of
   * CAVALRY_ERR_FAULT_IMAGE or CAVALRY_ERR_FAULT_IMAGES_FULL is the one report that the
   * replay's image failed a check or could not join, the log is ended short of its entries, and
   * the next poll returns 0 and clears the fault here; `faulted()` keeps it (`isEnded`). The
   * handler's exception, if any, is thrown after the ABI call returns, as `HandlerError`, whose
   * `fragments` is this result, -1 included. An entry whose handler call threw was delivered
   * and is not delivered again: `position()` is past it, and past every entry this poll
   * delivered after it. An entry the runtime kept from the handler, and every later entry of
   * the same poll, which the addon then hands the handler none of, were not delivered:
   * `HandlerError`'s `undeliveredAt` is the first one's position, where the service opens the
   * log again. From a handler of this log's own poll the ABI refuses the call: -1 with
   * CAVALRY_ERR_POLLING, and nothing is touched. */
  poll(handler, fragmentLimit) {
    const result = addon.committedLogPoll(this._state, this.handle, handler, fragmentLimit);
    if (typeof result === "number") return result;
    throw new HandlerError(result[0], result[1], result[2], result[3], true, result[4]);
  }
  /** What the last poll stopped on: {code, sessionId, position}; code 0 when the last poll
   * returned a count. A poll that returns a count clears it, so read it after a poll that
   * returned -1 and before the next. */
  fault() {
    const f = new Struct("cavalry_fault_t");
    check("cavalry_committed_log_fault", addon.cavalry_committed_log_fault(this.handle, f.buffer));
    return { code: f.int32("code"), sessionId: f.int32("session_id"), position: f.int64("position") };
  }
  /** The position after the last entry delivered, or the position the log was opened at. */
  position() { return addon.cavalry_committed_log_position(this.handle); }
  /** The bound of the last poll: the highest commit position taken from the member's counter. */
  commitPosition() { return addon.cavalry_committed_log_commit_position(this.handle); }
  /** Whether this log has come to its end: no poll will deliver another entry. Stop reading when
   * it is true, not when a poll returns 0. The log delivered every entry it was owed only when
   * its three reports are all null: `failedAt()`, `withdrawnAt()` and `faulted()`. In every
   * case what the log delivered ends at `position()`, where a service opens the log again. */
  get isEnded() { return addon.cavalry_committed_log_is_ended(this.handle); }
  /** The position after the last frame the driver's replay of the member's recording played
   * before it failed, or null for a log that is live or that ended because its member closed or
   * left the recording. No entry from that position on is delivered. The entries below it may
   * still be on their way when this first is a position, so read it once `isEnded` is true.
   * They have all been delivered by then unless the driver withdrew the replay first, which it
   * does 5 s after a replay ends, one that failed included: `withdrawnAt()` is then a position
   * below this one, and the entries from there on, those below this position among them, were
   * not delivered either. What the log delivered ends at `position()`. */
  failedAt() {
    const [failed, position] = addon.cavalry_committed_log_failed_at(this.handle);
    return failed ? position : null;
  }
  /** The position at which the entries this log was not given begin, when the driver withdrew
   * the replay before the polls had read it to its end, or null. The driver keeps a replay 5 s
   * after it ends and then withdraws it, read or not: a service that did not poll for that long
   * after its member closed or left the log, or whose handler took that long, ends so. The
   * entries from that position on were played and not delivered. A replay that failed is
   * withdrawn in the same way: `failedAt()` is then a position too, and this one is the lower.
   * Read it once `isEnded` is true. */
  withdrawnAt() {
    const [withdrawn, position] = addon.cavalry_committed_log_withdrawn_at(this.handle);
    return withdrawn ? position : null;
  }
  /** The fault of the log's own image, the replay's, as {code, sessionId, position}, or null
   * for a log whose image has not faulted. The code is CAVALRY_ERR_FAULT_IMAGE when a frame of
   * the image failed a check, with the reader's position, or when the image was gone before
   * this client read it, with NULL_POSITION; it is CAVALRY_ERR_FAULT_IMAGES_FULL when the
   * image could not join. The poll that meets the fault returns -1 once, `fault()` answers it
   * until the next poll, and this answers it from that poll on, for as long as the log is
   * held: a service that did not keep the poll's answer reads it here, once `isEnded` is
   * true, and tells this end from a whole one. No entry from the fault on was delivered.
   * `withdrawnAt()` is null for such a log, and `failedAt()` is a position only when the
   * driver's replay failed as well. From inside a handler of this log's own poll the
   * ABI refuses the call with CAVALRY_ERR_POLLING, and this is null, a refusal and not an
   * answer. */
  faulted() {
    const f = new Struct("cavalry_fault_t");
    if (!addon.cavalry_committed_log_faulted(this.handle, f.buffer)) return null;
    return { code: f.int32("code"), sessionId: f.int32("session_id"), position: f.int64("position") };
  }
  /** Stops the replay through the client and frees the handle; a second close does nothing.
   * After the client's own close it frees the handle alone, and the driver ends the replay by
   * itself. The handle is freed whatever the ABI answers. When the ABI could not stop the
   * replay, through a client that the driver or a timeout closed or within the response
   * timeout, this throws CavalryError after the handle is freed: the log is closed, a second
   * close does nothing, and the driver ends the replay by itself. The one exception is a close
   * from inside a handler of this log's own poll, which the ABI refuses: that throws
   * CavalryError with CAVALRY_ERR_POLLING too, and leaves the log open. */
  close() {
    if (this.handle === null) return;
    const result = addon.cavalry_committed_log_close(this.handle, this._client.handle);
    if (result !== 0 && addon.cavalry_errcode() === constants.CAVALRY_ERR_POLLING) throw error("cavalry_committed_log_close");
    this.handle = null;
    check("cavalry_committed_log_close", result);
  }
}

class Recording {
  /** A recording the driver makes; `close` frees the handle and stops nothing. */
  constructor(handle) {
    this.handle = handle;
    const c = new Struct("cavalry_recording_constants_t");
    check("cavalry_recording_constants", addon.cavalry_recording_constants(handle, c.buffer));
    this.channel = c.string("channel");
    this.recordingId = c.int64("recording_id");
    this.sessionId = c.int32("session_id");
    this.streamId = c.int32("stream_id");
  }
  position() { return addon.cavalry_recording_position(this.handle); }
  syncedPosition() { return addon.cavalry_recording_synced_position(this.handle); }
  close() {
    if (this.handle !== null) closeHandle(this, "cavalry_recording_close");
  }
}

class Replay {
  /** A replay the driver plays; `ended()` is {ended, position, reason}. */
  constructor(handle) {
    this.handle = handle;
    const c = new Struct("cavalry_replay_constants_t");
    check("cavalry_replay_constants", addon.cavalry_replay_constants(handle, c.buffer));
    this.channel = c.string("channel");
    this.replaySessionId = c.int64("replay_session_id");
    this.recordingId = c.int64("recording_id");
    this.startPosition = c.int64("start_position");
    this.sessionId = c.int32("session_id");
    this.streamId = c.int32("stream_id");
  }
  position() { return addon.cavalry_replay_position(this.handle); }
  ended() {
    const [ended, position, reason] = addon.cavalry_replay_ended(this.handle);
    return { ended, position, reason };
  }
  close() {
    if (this.handle !== null) closeHandle(this, "cavalry_replay_close");
  }
}

function abiVersion() { return addon.cavalry_abi_version(); }
/** The library's own version, as `cavalry_version_full` gives it. */
function version() { return addon.cavalry_version_full(); }
/** The Buffers this thread was handed, received bodies and claims' views, whose native
 * objects Node has not freed yet. Node frees them when its event loop turns, so a program
 * that never returns to the event loop holds every one (README, "The runtime's queue"). */
function buffersHeld() { return addon.buffersHeld(); }

module.exports = {
  ABI_VERSION, constants, LAYOUT, HEADER_FIELDS,
  NOT_CONNECTED, BACK_PRESSURED, ADMIN_ACTION, CLOSED, MAX_POSITION_EXCEEDED, ERROR, NULL_POSITION,
  CavalryError, HandlerError, Client, Publication, Subscription, CommittedLog, Header, Image, Recording, Replay,
  abiVersion, version, buffersHeld, addon,
};
'''

NODE_TYPES = '''/** The Node client over the C ABI of spec/c-abi.md (clients/node/README.md). Generated by
 * clients/generate.py node; do not edit by hand. */

/** The ABI version of the header this package was generated from. */
export const ABI_VERSION: number;

/** The header's constants, as the ABI names them. */
export const constants: {
@CONSTANTS@
};

/** The result codes a message path returns instead of throwing (spec/c-abi.md, "Result codes"). */
export const NOT_CONNECTED: number;
export const BACK_PRESSURED: number;
export const ADMIN_ACTION: number;
export const CLOSED: number;
export const MAX_POSITION_EXCEEDED: number;
export const ERROR: number;
export const NULL_POSITION: number;

/** A call that is not a message failed, with the ABI's code and message. */
export class CavalryError extends Error {
  fn: string;
  code: number;
  abiMessage: string;
}

/** A fragment of a poll, or an entry of a committed log's when `committedLog` is true, did not
 * reach the handler cleanly: the handler threw on `raised` of them, `undelivered` never reached
 * it, and `cause` is the first of these. `fragments` is the poll's result, -1 when the poll
 * also stopped on a fault, which the `fault()` of the object that was polled reads until its
 * next poll. `undeliveredAt` is a committed log's position of the poll's first entry that
 * never reached the handler, after which the handler was handed no entry of the poll, and
 * where the service opens the log again; null when every entry reached it, and for a
 * subscription. */
export class HandlerError extends Error {
  fragments: number;
  raised: number;
  undelivered: number;
  committedLog: boolean;
  undeliveredAt: number | null;
}

/** The body as a Buffer into the log, valid for the call, and the frame's header. */
export type FragmentHandler = (body: Buffer, header: Header) => void;

/** A committed log's entry: the body and the header as a fragment's, and the entry's position
 * in the log. */
export type EntryHandler = (body: Buffer, header: Header, position: number) => void;

export class Client {
  constructor(dir?: string | null);
  readonly dir: string;
  readonly clientId: number;
  readonly isClosed: boolean;
  close(): void;
  closed(): number;
  addPublication(channel: string, streamId: number): Publication;
  addExclusivePublication(channel: string, streamId: number): Publication;
  addSubscription(channel: string, streamId: number): Subscription;
  startRecording(channel: string, streamId: number): Recording;
  stopRecording(recordingId: number): void;
  addDestination(publication: Publication, channel: string): void;
  removeDestination(publication: Publication, channel: string): void;
  truncateRecording(recordingId: number, position: number): void;
  purgeSegments(recordingId: number, newStartPosition: number): void;
  purgeRecording(recordingId: number): void;
  startReplay(recordingId: number, position: number, length: number, replayStreamId: number, channel: string): Replay;
  stopReplay(replaySessionId: number): void;
  committedLog(position: number): CommittedLog | null;
  [Symbol.dispose](): void;
}

export class Publication {
  readonly channel: string;
  readonly registrationId: number;
  readonly sessionId: number;
  readonly streamId: number;
  readonly maxPayloadLength: number;
  readonly maxMessageLength: number;
  readonly isExclusive: boolean;
  /** The Buffer over the slice the last `tryClaim` reserved, or null. */
  claim: Buffer | null;
  tryClaim(length: number): number;
  commit(): number;
  abort(): number;
  /** Any int64: a BigInt exactly, a number below 2^53 in magnitude. */
  setReservedValue(value: number | bigint): number;
  offer(message: Buffer | Uint8Array): number;
  position(): number;
  positionLimit(): number;
  readonly isConnected: boolean;
  readonly isClosed: boolean;
  close(): void;
}

export class Header {
  readonly frameLength: number | undefined;
  readonly version: number | undefined;
  readonly flags: number | undefined;
  readonly type: number | undefined;
  readonly termOffset: number | undefined;
  readonly sessionId: number | undefined;
  readonly streamId: number | undefined;
  readonly termId: number | undefined;
  /** Any int64 the publisher set, 0 unless set, as a BigInt so that every one is exact. */
  readonly reservedValue: bigint | undefined;
}

export class Image {
  readonly sourceIdentity: string;
  readonly correlationId: number;
  readonly positionBitsToShift: number;
  readonly sessionId: number;
  readonly initialTermId: number;
  position(): number;
  readonly isEndOfStream: boolean;
}

export class Subscription {
  readonly channel: string;
  readonly registrationId: number;
  readonly streamId: number;
  poll(handler: FragmentHandler, fragmentLimit: number): number;
  fault(): { code: number; sessionId: number; position: number };
  imageCount(): number;
  imageAtIndex(index: number): Image | null;
  images(): Image[];
  readonly isConnected: boolean;
  readonly isClosed: boolean;
  close(): void;
}

export class CommittedLog {
  readonly recordingId: number;
  readonly memberId: number;
  poll(handler: EntryHandler, fragmentLimit: number): number;
  fault(): { code: number; sessionId: number; position: number };
  position(): number;
  commitPosition(): number;
  readonly isEnded: boolean;
  failedAt(): number | null;
  withdrawnAt(): number | null;
  faulted(): { code: number; sessionId: number; position: number } | null;
  close(): void;
}

export class Recording {
  readonly channel: string;
  readonly recordingId: number;
  readonly sessionId: number;
  readonly streamId: number;
  position(): number;
  syncedPosition(): number;
  close(): void;
}

export class Replay {
  readonly channel: string;
  readonly replaySessionId: number;
  readonly recordingId: number;
  readonly startPosition: number;
  readonly sessionId: number;
  readonly streamId: number;
  position(): number;
  ended(): { ended: boolean; position: number; reason: number };
  close(): void;
}

export function abiVersion(): number;
export function version(): string;
/** The Buffers this thread was handed, received bodies and claims' views, that Node frees
 * when its event loop next turns. */
export function buffersHeld(): number;
'''
