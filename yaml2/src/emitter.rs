//! The emitter: owned `Value` tree -> YAML text.

use crate::meta::ScalarStyle;
use crate::options::{EmitOptions, Schema};
use crate::value::{Value, ValueData};

/// Renders a single value as a YAML document (always ends with a newline).
pub(crate) fn emit(value: &Value, options: &EmitOptions) -> String {
    let mut out = String::new();
    emit_block_value(value, 0, options, &mut out);
    out
}

/// Renders multiple values as a multi-document stream, separating documents
/// with a `---` line. An empty slice yields an empty string.
pub(crate) fn emit_documents(values: &[Value], options: &EmitOptions) -> String {
    let mut out = String::new();
    for (i, value) in values.iter().enumerate() {
        if i > 0 {
            out.push_str("---\n");
        }
        emit_block_value(value, 0, options, &mut out);
    }
    out
}

/// Indentation for `level` nesting levels, `options.indent` spaces each.
fn pad(level: usize, options: &EmitOptions) -> String {
    " ".repeat(level * options.indent)
}

/// Emits a value occupying its own line(s) starting at indentation `level`.
/// Non-empty collections render in block style; everything else is a single
/// flow/scalar line.
fn emit_block_value(value: &Value, level: usize, options: &EmitOptions, out: &mut String) {
    match value.data() {
        ValueData::Sequence(items) if !items.is_empty() => {
            emit_block_sequence(items, level, options, out)
        }
        ValueData::Mapping(map) if !map.is_empty() => emit_block_mapping(map, level, options, out),
        _ => {
            out.push_str(&pad(level, options));
            out.push_str(&scalar_or_flow(value));
            out.push('\n');
        }
    }
}

/// True for a sequence or mapping with at least one element — these render on
/// their own indented lines rather than inline after a `-` or `key:`.
fn is_block_collection(value: &Value) -> bool {
    match value.data() {
        ValueData::Sequence(items) => !items.is_empty(),
        ValueData::Mapping(map) => !map.is_empty(),
        _ => false,
    }
}

fn emit_block_sequence(items: &[Value], level: usize, options: &EmitOptions, out: &mut String) {
    for item in items {
        out.push_str(&pad(level, options));
        out.push('-');
        if is_block_collection(item) {
            out.push('\n');
            emit_block_value(item, level + 1, options, out);
        } else {
            out.push(' ');
            out.push_str(&scalar_or_flow(item));
            out.push('\n');
        }
    }
}

fn emit_block_mapping(
    map: &crate::value::Mapping,
    level: usize,
    options: &EmitOptions,
    out: &mut String,
) {
    for (k, v) in map.iter() {
        out.push_str(&pad(level, options));
        out.push_str(&scalar_or_flow(k));
        out.push(':');
        if is_block_collection(v) {
            out.push('\n');
            emit_block_value(v, level + 1, options, out);
        } else {
            out.push(' ');
            out.push_str(&scalar_or_flow(v));
            out.push('\n');
        }
    }
}

/// Renders a value to a single-line string: scalars via `emit_scalar`,
/// collections via `emit_flow`.
fn scalar_or_flow(value: &Value) -> String {
    match value.data() {
        ValueData::Sequence(_) | ValueData::Mapping(_) => emit_flow(value),
        _ => emit_scalar(value),
    }
}

/// Renders any value to single-line flow form (recursive). Used for empty and
/// inline collections and for non-scalar mapping keys.
fn emit_flow(value: &Value) -> String {
    match value.data() {
        ValueData::Sequence(items) => {
            let inner: Vec<String> = items.iter().map(emit_flow).collect();
            format!("[{}]", inner.join(", "))
        }
        ValueData::Mapping(map) => {
            let inner: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", emit_flow(k), emit_flow(v)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
        _ => emit_scalar(value),
    }
}

/// Renders a scalar value: plain when it re-parses to itself, otherwise
/// double-quoted with escapes. Collection inputs are routed through `emit_flow`
/// by callers, but are handled here defensively too.
fn emit_scalar(value: &Value) -> String {
    match value.data() {
        ValueData::Null => "null".to_string(),
        ValueData::Bool(true) => "true".to_string(),
        ValueData::Bool(false) => "false".to_string(),
        ValueData::Int(i) => i.to_string(),
        ValueData::Float(f) => format_float(*f),
        ValueData::String(s) => {
            if needs_quoting(s) {
                double_quote(s)
            } else {
                s.clone()
            }
        }
        ValueData::Sequence(_) | ValueData::Mapping(_) => emit_flow(value),
    }
}

/// Formats a float so it always re-parses as a float (never an int).
fn format_float(f: f64) -> String {
    if f.is_nan() {
        return ".nan".to_string();
    }
    if f.is_infinite() {
        return if f < 0.0 { "-.inf" } else { ".inf" }.to_string();
    }
    let s = format!("{f}");
    if s.contains('.') || s.contains('e') || s.contains('E') {
        s
    } else {
        format!("{s}.0")
    }
}

/// True when a string cannot be safely emitted as a plain scalar — either it
/// would change type on reparse, or it contains a structural hazard.
fn needs_quoting(s: &str) -> bool {
    if s.is_empty() {
        return true; // empty plain scalar parses as null
    }
    // Would a plain emission re-resolve to a different type?
    match crate::scalar::resolve(s, ScalarStyle::Plain, Schema::Core1_2) {
        ValueData::String(ref t) if t == s => {}
        _ => return true,
    }
    let first = s.as_bytes()[0];
    let leading_indicator = matches!(
        first,
        b'!' | b'&'
            | b'*'
            | b'?'
            | b'-'
            | b':'
            | b','
            | b'['
            | b']'
            | b'{'
            | b'}'
            | b'#'
            | b'|'
            | b'>'
            | b'@'
            | b'`'
            | b'"'
            | b'\''
            | b'%'
    );
    if leading_indicator {
        return true;
    }
    if s.starts_with(' ') || s.ends_with(' ') {
        return true;
    }
    if s.ends_with(':') || s.contains(": ") || s.contains(" #") {
        return true;
    }
    // Flow-indicator characters are unsafe inside flow context.
    if s.contains([',', '[', ']', '{', '}']) {
        return true;
    }
    if s.bytes().any(|b| b.is_ascii_control()) {
        return true;
    }
    false
}

/// Double-quotes a string, escaping quotes, backslashes, and control chars.
fn double_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn to_string(value: &Value) -> String {
        crate::api::to_string(value).unwrap()
    }

    /// parse -> emit -> parse must preserve the value.
    fn roundtrip(input: &str) {
        let v = crate::api::parse(input).unwrap();
        let text = crate::api::to_string(&v).unwrap();
        let v2 = crate::api::parse(&text).unwrap();
        assert_eq!(v, v2, "roundtrip changed value; emitted:\n{text}");
    }

    #[test]
    fn emits_plain_string() {
        assert_eq!(to_string(&Value::string("hello")), "hello\n");
    }

    #[test]
    fn emits_int_and_null_and_bool() {
        assert_eq!(to_string(&Value::int(42)), "42\n");
        assert_eq!(to_string(&Value::null()), "null\n");
        assert_eq!(to_string(&Value::bool(true)), "true\n");
    }

    #[test]
    fn float_always_has_point() {
        assert_eq!(to_string(&Value::float(5.0)), "5.0\n");
        assert_eq!(to_string(&Value::float(1.5)), "1.5\n");
        assert_eq!(to_string(&Value::float(f64::INFINITY)), ".inf\n");
    }

    #[test]
    fn quotes_strings_that_would_change_type() {
        assert_eq!(to_string(&Value::string("42")), "\"42\"\n");
        assert_eq!(to_string(&Value::string("true")), "\"true\"\n");
        assert_eq!(to_string(&Value::string("null")), "\"null\"\n");
        assert_eq!(to_string(&Value::string("")), "\"\"\n");
    }

    #[test]
    fn quotes_structurally_hazardous_strings() {
        assert_eq!(to_string(&Value::string("a: b")), "\"a: b\"\n");
        assert_eq!(to_string(&Value::string("# c")), "\"# c\"\n");
        assert_eq!(to_string(&Value::string(" x")), "\" x\"\n");
    }

    #[test]
    fn flow_collections_roundtrip() {
        roundtrip("[1, 2, 3]\n");
        roundtrip("{a: 1, b: 2}\n");
        roundtrip("a: 1\nb: 2\n");
        roundtrip("- a\n- b\n");
    }

    #[test]
    fn scalar_strings_roundtrip() {
        roundtrip("plain text\n");
        roundtrip("\"quoted: value\"\n");
        roundtrip("\"line\\nbreak\"\n");
    }

    #[test]
    fn block_sequence_layout() {
        let v = crate::api::parse("- a\n- b\n").unwrap();
        assert_eq!(to_string(&v), "- a\n- b\n");
    }

    #[test]
    fn block_mapping_layout() {
        let v = crate::api::parse("a: 1\nb: 2\n").unwrap();
        assert_eq!(to_string(&v), "a: 1\nb: 2\n");
    }

    #[test]
    fn nested_mapping_layout() {
        let v = crate::api::parse("outer:\n  inner: 7\n").unwrap();
        assert_eq!(to_string(&v), "outer:\n  inner: 7\n");
    }

    #[test]
    fn mapping_in_sequence_layout() {
        let v = crate::api::parse("- a: 1\n  b: 2\n").unwrap();
        // Collection entries are placed on the following line, indented.
        assert_eq!(to_string(&v), "-\n  a: 1\n  b: 2\n");
    }

    #[test]
    fn empty_collections_stay_flow() {
        assert_eq!(to_string(&Value::sequence(vec![])), "[]\n");
        assert_eq!(
            to_string(&Value::mapping(crate::value::Mapping::new())),
            "{}\n"
        );
    }

    #[test]
    fn complex_key_is_flow_inline() {
        // A sequence used as a mapping key renders in flow form before the colon.
        let mut m = crate::value::Mapping::new();
        m.insert(
            Value::sequence(vec![Value::int(1), Value::int(2)]),
            Value::string("v"),
        );
        assert_eq!(to_string(&Value::mapping(m)), "[1, 2]: v\n");
    }

    #[test]
    fn deep_structure_roundtrips() {
        roundtrip("a:\n  - 1\n  - 2\nb:\n  c: d\n");
        roundtrip("- - 1\n  - 2\n- x\n");
    }

    #[test]
    fn custom_indent_width() {
        let v = crate::api::parse("outer:\n  inner: 7\n").unwrap();
        let opts = EmitOptions {
            indent: 4,
            ..Default::default()
        };
        let text = crate::api::to_string_with(&v, &opts).unwrap();
        assert_eq!(text, "outer:\n    inner: 7\n");
    }

    #[test]
    fn documents_are_separated() {
        let docs = vec![Value::int(1), Value::int(2)];
        let text = crate::api::to_string_documents(&docs).unwrap();
        assert_eq!(text, "1\n---\n2\n");
    }

    #[test]
    fn empty_document_list_is_empty_string() {
        assert_eq!(crate::api::to_string_documents(&[]).unwrap(), "");
    }

    #[test]
    fn documents_roundtrip() {
        let docs = crate::api::parse_documents("--- a\n--- b\n").unwrap();
        let text = crate::api::to_string_documents(&docs).unwrap();
        let docs2 = crate::api::parse_documents(&text).unwrap();
        assert_eq!(docs, docs2);
    }

    #[test]
    fn realistic_document_roundtrips() {
        let input = "\
name: Ada
active: true
scores:
  - 10
  - 20
address:
  city: Portland
  zip: \"97201\"
tags: [x, y, z]
";
        roundtrip(input);
    }

    #[test]
    fn mapping_with_empty_and_null_roundtrips() {
        roundtrip("a:\nb: []\nc: {}\n");
    }

    #[test]
    fn special_strings_roundtrip() {
        roundtrip("- \"\"\n- \"true\"\n- \"42\"\n- \"a: b\"\n- \" leading\"\n");
    }

    #[test]
    fn key_order_is_preserved() {
        let v = crate::api::parse("z: 1\na: 2\nm: 3\n").unwrap();
        let text = crate::api::to_string(&v).unwrap();
        assert_eq!(text, "z: 1\na: 2\nm: 3\n");
    }

    #[test]
    fn deeply_nested_roundtrips() {
        roundtrip("a:\n  b:\n    c:\n      - 1\n      - d: e\n");
    }

    #[test]
    fn multiline_plain_roundtrips() {
        // The folded value re-emits (as a single line) and re-parses to the same value.
        roundtrip("summary: line one\n  line two\n");
        roundtrip("text: para one\n\n  para two\n");
    }
}
