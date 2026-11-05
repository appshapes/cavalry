//! Holds the committed header, `include/cavalry.h`, to `spec/vectors/c-abi.json`, the
//! page's vectors (`spec/c-abi.md`, "Versions and what a reader rejects"; ADR 0051):
//! every opaque type, every struct with its fields in order, the handler typedef, the
//! macro, every function with its return type and parameters, and every define with
//! its value, each way, and a header with one more of any kind fails. `just
//! conformance` runs this test with the other suites, and `just test` runs it too.

use std::collections::BTreeMap;

use serde_json::Value;

const VECTORS: &str = include_str!("../../../spec/vectors/c-abi.json");
const HEADER: &str = include_str!("../include/cavalry.h");

/// The header with its comments removed and its whitespace folded, one declaration
/// per `;`.
fn declarations(header: &str) -> Vec<String> {
    let mut text = String::new();
    let mut rest = header;
    while let Some(start) = rest.find("/*") {
        text.push_str(&rest[..start]);
        let end = rest[start..].find("*/").expect("a comment ends") + start + 2;
        rest = &rest[end..];
    }
    text.push_str(rest);
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        // The C++ guard's two lines are not declarations.
        if line.is_empty() || line == "extern \"C\" {" || line == "}" {
            continue;
        }
        out.push(line.to_string());
    }
    out
}

/// Folds whitespace so that `uint8_t *data` and `uint8_t * data` compare equal.
fn fold(text: &str) -> String {
    let mut out = String::new();
    let mut space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            space = true;
        } else {
            if space && !out.is_empty() && !matches!(ch, '*' | ',' | ')' | ';' | '(') {
                let last = out.chars().last().unwrap_or(' ');
                if !matches!(last, '*' | '(' | ',') || ch.is_alphanumeric() || ch == '_' {
                    out.push(' ');
                }
            }
            out.push(ch);
            space = false;
        }
    }
    out.replace("* ", "*").replace("( ", "(").replace(" )", ")")
}

/// Whether a `#define`'s text, after the keyword, is a function-like macro: a `(`
/// straight after the name, as C requires of one.
fn is_function_like(rest: &str) -> bool {
    let name_end = rest
        .find(|c: char| !c.is_alphanumeric() && c != '_')
        .unwrap_or(rest.len());
    rest[name_end..].starts_with('(')
}

/// The header's object-like defines: name to value.
fn defines(lines: &[String]) -> BTreeMap<String, String> {
    lines
        .iter()
        .filter_map(|line| line.strip_prefix("#define "))
        .filter(|rest| !is_function_like(rest))
        .filter_map(|rest| {
            let (name, value) = rest.split_once(' ')?;
            Some((name.to_string(), value.trim().to_string()))
        })
        .collect()
}

/// The header's function-like macros: name to the whole `#define` line.
fn macros(lines: &[String]) -> BTreeMap<String, String> {
    lines
        .iter()
        .filter(|line| {
            line.strip_prefix("#define ")
                .is_some_and(|rest| is_function_like(rest))
        })
        .map(|line| {
            let rest = &line["#define ".len()..];
            let name = &rest[..rest.find('(').expect("a function-like macro")];
            (name.to_string(), line.clone())
        })
        .collect()
}

/// The header's typedefs other than its structs: name to the folded statement. A
/// function pointer's name is the one inside `(*` and `)`; any other typedef's is its
/// last word.
fn typedefs(lines: &[String]) -> BTreeMap<String, String> {
    let joined = lines
        .iter()
        .filter(|l| !l.starts_with('#'))
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    let mut out = BTreeMap::new();
    for statement in joined.split(';') {
        let statement = statement.trim();
        if !statement.starts_with("typedef ") || statement.starts_with("typedef struct") {
            continue;
        }
        let name = match statement.find("(*") {
            Some(at) => {
                let rest = &statement[at + 2..];
                rest[..rest.find(')').expect("a function pointer's name closes")].trim()
            }
            None => statement
                .rsplit(|c: char| !c.is_alphanumeric() && c != '_')
                .find(|word| !word.is_empty())
                .expect("a typedef names a type"),
        };
        out.insert(name.to_string(), fold(&format!("{statement};")));
    }
    out
}

/// The header's functions: name to (return type, parameters as `type name`).
fn functions(lines: &[String]) -> BTreeMap<String, (String, Vec<String>)> {
    let joined = lines
        .iter()
        .filter(|l| !l.starts_with('#'))
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    let mut out = BTreeMap::new();
    for statement in joined.split(';') {
        let statement = statement.trim();
        let Some(open) = statement.find('(') else {
            continue;
        };
        if statement.contains("typedef") || statement.contains('{') {
            continue;
        }
        let head = fold(&statement[..open]);
        let Some(name_at) = head.rfind(|c: char| !c.is_alphanumeric() && c != '_') else {
            continue;
        };
        let name = head[name_at + 1..].to_string();
        if !name.starts_with("cavalry_") {
            continue;
        }
        let returns = fold(head[..=name_at].trim());
        let close = statement.rfind(')').expect("a function closes");
        let params: Vec<String> = statement[open + 1..close]
            .split(',')
            .map(|p| fold(p.trim()))
            .filter(|p| p != "void" && !p.is_empty())
            .collect();
        out.insert(name, (returns, params));
    }
    out
}

/// The header's structs: name to fields as `type name`, in order.
fn structs(lines: &[String]) -> BTreeMap<String, Vec<String>> {
    let joined = lines
        .iter()
        .filter(|l| !l.starts_with('#'))
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    let mut out = BTreeMap::new();
    let mut rest = joined.as_str();
    while let Some(start) = rest.find("typedef struct {") {
        let body_start = start + "typedef struct {".len();
        let body_end = rest[body_start..].find('}').expect("a struct closes") + body_start;
        let name_end = rest[body_end..].find(';').expect("a struct is named") + body_end;
        let name = rest[body_end + 1..name_end].trim().to_string();
        let fields: Vec<String> = rest[body_start..body_end]
            .split(';')
            .map(|f| fold(f.trim()))
            .filter(|f| !f.is_empty())
            .collect();
        out.insert(name, fields);
        rest = &rest[name_end + 1..];
    }
    out
}

fn opaque(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter_map(|line| line.strip_prefix("typedef struct "))
        .filter_map(|rest| {
            let (name, alias) = rest.trim_end_matches(';').split_once(' ')?;
            (name == alias).then(|| name.to_string())
        })
        .collect()
}

fn vector_field(field: &Value) -> String {
    let parts = field.as_array().expect("a field is [type, name]");
    let ty = parts[0].as_str().expect("a type");
    let name = parts[1].as_str().expect("a name");
    fold(&format!("{ty} {name}"))
}

fn define_value(value: &Value) -> String {
    match value {
        Value::String(s) => format!("\"{s}\""),
        Value::Number(n) if n.as_i64() == Some(i64::MAX) => "INT64_MAX".to_string(),
        other => other.to_string(),
    }
}

#[test]
fn the_header_declares_what_the_vectors_say_and_nothing_else() {
    check(HEADER);
}

/// The header with `extra` inserted before its functions, after its last typedef.
fn with_extra(extra: &str) -> String {
    let at = HEADER
        .find("#ifdef __cplusplus")
        .expect("the header opens its C++ guard");
    format!("{}{extra}\n\n{}", &HEADER[..at], &HEADER[at..])
}

#[test]
fn a_header_that_declares_more_than_the_vectors_is_rejected() {
    // Each kind the page names, one more than the vectors list.
    for (extra, says) in [
        (
            "typedef void (*cavalry_other_handler_t)(void *clientd);",
            "a typedef the vectors lack: cavalry_other_handler_t",
        ),
        (
            "typedef int32_t cavalry_alias_t;",
            "a typedef the vectors lack: cavalry_alias_t",
        ),
        (
            "#define cavalry_publication_init(publication) (publication)",
            "a macro the vectors lack: cavalry_publication_init",
        ),
        (
            "#define CAVALRY_EXTRA 1",
            "defines what the vectors lack: CAVALRY_EXTRA",
        ),
        (
            "typedef struct cavalry_extra_t cavalry_extra_t;",
            "the opaque types",
        ),
        (
            "typedef struct {\n  int32_t field;\n} cavalry_extra_values_t;",
            "a struct the vectors lack: cavalry_extra_values_t",
        ),
        (
            "int cavalry_extra(void);",
            "a function the vectors lack: cavalry_extra",
        ),
    ] {
        let header = with_extra(extra);
        let refused = std::panic::catch_unwind(|| check(&header))
            .expect_err(&format!("a header with `{extra}` passed"));
        let message = refused
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| refused.downcast_ref::<&str>().copied())
            .unwrap_or_default();
        assert!(
            message.contains(says),
            "a header with `{extra}` failed for another reason: {message}"
        );
    }
}

/// Holds `header` to the vectors, each way; panics on the first difference.
fn check(header: &str) {
    let vectors: Value = serde_json::from_str(VECTORS).expect("the vectors parse");
    assert_eq!(vectors["spec"], "spec/c-abi.md");
    let lines = declarations(header);

    // Opaque types, each way.
    let want: Vec<String> = vectors["opaque_types"]
        .as_array()
        .expect("opaque types")
        .iter()
        .map(|v| v.as_str().expect("a name").to_string())
        .collect();
    let mut have = opaque(&lines);
    have.sort();
    let mut want_sorted = want.clone();
    want_sorted.sort();
    assert_eq!(have, want_sorted, "the opaque types");

    // Structs, with their fields in order.
    let have = structs(&lines);
    let want = vectors["structs"].as_object().expect("structs");
    for (name, fields) in want {
        let fields: Vec<String> = fields
            .as_array()
            .expect("fields")
            .iter()
            .map(vector_field)
            .collect();
        let found = have
            .get(name)
            .unwrap_or_else(|| panic!("the header lacks struct {name}"));
        assert_eq!(found, &fields, "the fields of {name}");
    }
    for name in have.keys() {
        assert!(
            want.contains_key(name),
            "the header declares a struct the vectors lack: {name}"
        );
    }

    // Functions, with their return types and parameters.
    let have = functions(&lines);
    let want = vectors["functions"].as_array().expect("functions");
    for function in want {
        let name = function["name"].as_str().expect("a name");
        let returns = fold(function["returns"].as_str().expect("a return type"));
        let params: Vec<String> = function["params"]
            .as_array()
            .expect("params")
            .iter()
            .map(vector_field)
            .collect();
        let (found_returns, found_params) = have
            .get(name)
            .unwrap_or_else(|| panic!("the header lacks {name}"));
        assert_eq!(found_returns, &returns, "the return type of {name}");
        assert_eq!(found_params, &params, "the parameters of {name}");
    }
    let want_names: Vec<&str> = want.iter().map(|f| f["name"].as_str().unwrap()).collect();
    for name in have.keys() {
        assert!(
            want_names.contains(&name.as_str()),
            "the header declares a function the vectors lack: {name}"
        );
    }

    // The handler typedef, each way.
    let have = typedefs(&lines);
    let want = vectors["typedefs"].as_object().expect("typedefs");
    for (name, signature) in want {
        let signature = signature.as_str().expect("a signature");
        let (returns, params) = signature.split_once(" (*)").expect("a function pointer");
        let expected = fold(&format!("typedef {returns} (*{name}){params};"));
        let found = have
            .get(name)
            .unwrap_or_else(|| panic!("the header lacks the typedef {expected}"));
        assert_eq!(found, &expected, "the typedef {name}");
    }
    for name in have.keys() {
        assert!(
            want.contains_key(name),
            "the header declares a typedef the vectors lack: {name}"
        );
    }

    // The macro, each way.
    let have = macros(&lines);
    let want = vectors["macros"].as_object().expect("macros");
    for (name, macro_) in want {
        let params = macro_["params"]
            .as_array()
            .expect("params")
            .iter()
            .map(|p| p.as_str().unwrap())
            .collect::<Vec<_>>()
            .join(", ");
        let expands = macro_["expands_to"].as_str().expect("an expansion");
        let line = format!("#define {name}({params}) {expands}");
        let found = have
            .get(name)
            .unwrap_or_else(|| panic!("the header lacks the macro {line}"));
        assert_eq!(found, &line, "the macro {name}");
    }
    for name in have.keys() {
        assert!(
            want.contains_key(name),
            "the header declares a macro the vectors lack: {name}"
        );
    }

    // Defines, each way, the include guard aside.
    let have = defines(&lines);
    let want = vectors["defines"].as_object().expect("defines");
    for (name, value) in want {
        let found = have
            .get(name)
            .unwrap_or_else(|| panic!("the header lacks #define {name}"));
        assert_eq!(found, &define_value(value), "the value of {name}");
    }
    for name in have.keys() {
        if name == "CAVALRY_H" {
            continue;
        }
        assert!(
            want.contains_key(name),
            "the header defines what the vectors lack: {name}"
        );
    }
    assert_eq!(
        vectors["version"].as_i64(),
        Some(i64::from(cavalry_ffi::CAVALRY_ABI_VERSION)),
        "the vectors' version is the library's"
    );
}
