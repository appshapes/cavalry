//! Holds the committed header, `include/cavalry.h`, to `spec/vectors/c-abi.json`, the
//! page's vectors (`spec/c-abi.md`, "Versions and what a reader rejects"; ADR 0051):
//! every opaque type, every struct with its fields in order, the handler typedef, the
//! macro, every function with its return type and parameters, and every define with
//! its value, each way. `just conformance` runs this test with the other suites.

use std::collections::BTreeMap;

use serde_json::Value;

const VECTORS: &str = include_str!("../../../spec/vectors/c-abi.json");
const HEADER: &str = include_str!("../include/cavalry.h");

/// The header with its comments removed and its whitespace folded, one declaration
/// per `;`.
fn declarations() -> Vec<String> {
    let mut text = String::new();
    let mut rest = HEADER;
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

fn defines(lines: &[String]) -> BTreeMap<String, String> {
    lines
        .iter()
        .filter_map(|line| line.strip_prefix("#define "))
        .filter_map(|rest| {
            let (name, value) = rest.split_once(' ')?;
            Some((name.to_string(), value.trim().to_string()))
        })
        .collect()
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
    let vectors: Value = serde_json::from_str(VECTORS).expect("the vectors parse");
    assert_eq!(vectors["spec"], "spec/c-abi.md");
    let lines = declarations();

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

    // The handler typedef and the macro.
    let joined = lines.join(" ");
    for (name, signature) in vectors["typedefs"].as_object().expect("typedefs") {
        let signature = signature.as_str().expect("a signature");
        let (returns, params) = signature.split_once(" (*)").expect("a function pointer");
        let expected = fold(&format!("typedef {returns} (*{name}){params};"));
        assert!(
            fold(&joined).contains(&expected),
            "the header lacks the typedef {expected}"
        );
    }
    for (name, macro_) in vectors["macros"].as_object().expect("macros") {
        let params = macro_["params"]
            .as_array()
            .expect("params")
            .iter()
            .map(|p| p.as_str().unwrap())
            .collect::<Vec<_>>()
            .join(", ");
        let expands = macro_["expands_to"].as_str().expect("an expansion");
        let line = format!("#define {name}({params}) {expands}");
        assert!(lines.contains(&line), "the header lacks the macro {line}");
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
        if name == "CAVALRY_H" || name.starts_with("cavalry_") {
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
