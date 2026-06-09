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

/// Indentation for `level` nesting levels, `options.indent` spaces each.
fn pad(level: usize, options: &EmitOptions) -> String {
    " ".repeat(level * options.indent)
}

/// Emits a value occupying its own line(s) starting at indentation `level`.
/// Task 1 renders every value as a single flow line; Task 2 adds block forms.
fn emit_block_value(value: &Value, level: usize, options: &EmitOptions, out: &mut String) {
    out.push_str(&pad(level, options));
    out.push_str(&scalar_or_flow(value));
    out.push('\n');
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
}
