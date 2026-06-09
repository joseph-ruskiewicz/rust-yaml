//! Plain-scalar resolution: raw text -> typed `ValueData`, per schema.

use crate::meta::ScalarStyle;
use crate::options::Schema;
use crate::value::ValueData;

/// Resolves a scalar's raw text into typed data according to `style` and `schema`.
///
/// Non-plain scalars (quoted, literal, folded) are always strings.
// wired into Value::from_scalar in Task 12
#[allow(dead_code)]
pub(crate) fn resolve(raw: &str, style: ScalarStyle, schema: Schema) -> ValueData {
    if style != ScalarStyle::Plain {
        return ValueData::String(raw.to_string());
    }
    match schema {
        Schema::Failsafe => ValueData::String(raw.to_string()),
        Schema::Core1_2 => resolve_core(raw),
        Schema::Json1_2 => resolve_json(raw),
        Schema::Yaml1_1 => resolve_yaml11(raw),
    }
}

fn resolve_core(raw: &str) -> ValueData {
    match raw {
        "" | "~" | "null" | "Null" | "NULL" => return ValueData::Null,
        "true" | "True" | "TRUE" => return ValueData::Bool(true),
        "false" | "False" | "FALSE" => return ValueData::Bool(false),
        ".inf" | ".Inf" | ".INF" | "+.inf" | "+.Inf" | "+.INF" => {
            return ValueData::Float(f64::INFINITY)
        }
        "-.inf" | "-.Inf" | "-.INF" => return ValueData::Float(f64::NEG_INFINITY),
        ".nan" | ".NaN" | ".NAN" => return ValueData::Float(f64::NAN),
        _ => {}
    }
    if is_core_int_shape(raw) {
        return match parse_core_int(raw) {
            Some(i) => ValueData::Int(i),
            None => ValueData::String(raw.to_string()),
        };
    }
    if let Some(f) = parse_core_float(raw) {
        return ValueData::Float(f);
    }
    ValueData::String(raw.to_string())
}

/// True if `raw` matches the Core int grammar: 0x..., 0o..., or [-+]?[0-9]+.
fn is_core_int_shape(raw: &str) -> bool {
    if let Some(hex) = raw.strip_prefix("0x") {
        return !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit());
    }
    if let Some(oct) = raw.strip_prefix("0o") {
        return !oct.is_empty() && oct.bytes().all(|b| (b'0'..=b'7').contains(&b));
    }
    let body = raw.strip_prefix(['+', '-']).unwrap_or(raw);
    !body.is_empty() && body.bytes().all(|b| b.is_ascii_digit())
}

fn parse_core_int(raw: &str) -> Option<i64> {
    if let Some(hex) = raw.strip_prefix("0x") {
        return i64::from_str_radix(hex, 16).ok();
    }
    if let Some(oct) = raw.strip_prefix("0o") {
        return i64::from_str_radix(oct, 8).ok();
    }
    raw.parse::<i64>().ok()
}

/// Parses a Core-1.2 float. Requires a '.' or exponent to distinguish from ints,
/// and only the float character set (rejecting Rust-accepted forms like "inf").
fn parse_core_float(raw: &str) -> Option<f64> {
    let has_dot_or_exp = raw.contains('.') || raw.contains('e') || raw.contains('E');
    if !has_dot_or_exp {
        return None;
    }
    let allowed =
        |b: u8| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-');
    if !raw.bytes().all(allowed) {
        return None;
    }
    raw.parse::<f64>().ok()
}

fn resolve_json(raw: &str) -> ValueData {
    // Implemented in Task 11.
    ValueData::String(raw.to_string())
}

fn resolve_yaml11(raw: &str) -> ValueData {
    // Implemented in Task 10.
    ValueData::String(raw.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn core(raw: &str) -> ValueData {
        resolve(raw, ScalarStyle::Plain, Schema::Core1_2)
    }

    #[test]
    fn core_null_bool() {
        assert!(matches!(core("null"), ValueData::Null));
        assert!(matches!(core("~"), ValueData::Null));
        assert!(matches!(core(""), ValueData::Null));
        assert!(matches!(core("true"), ValueData::Bool(true)));
        assert!(matches!(core("False"), ValueData::Bool(false)));
    }

    #[test]
    fn core_integers() {
        assert!(matches!(core("42"), ValueData::Int(42)));
        assert!(matches!(core("-7"), ValueData::Int(-7)));
        assert!(matches!(core("0x1F"), ValueData::Int(31)));
        assert!(matches!(core("0o17"), ValueData::Int(15)));
    }

    #[test]
    fn core_leading_zero_is_decimal_not_octal() {
        assert!(matches!(core("0777"), ValueData::Int(777)));
    }

    #[test]
    fn core_floats() {
        assert!(matches!(core("1.5"), ValueData::Float(f) if f == 1.5));
        assert!(matches!(core(".5"), ValueData::Float(f) if f == 0.5));
        assert!(matches!(core("1e3"), ValueData::Float(f) if f == 1000.0));
        assert!(matches!(core(".inf"), ValueData::Float(f) if f == f64::INFINITY));
        assert!(matches!(core(".nan"), ValueData::Float(f) if f.is_nan()));
    }

    #[test]
    fn core_norway_problem_absent() {
        assert!(matches!(core("NO"), ValueData::String(s) if s == "NO"));
        assert!(matches!(core("yes"), ValueData::String(s) if s == "yes"));
        assert!(matches!(core("on"), ValueData::String(s) if s == "on"));
    }

    #[test]
    fn core_plain_words_are_strings() {
        assert!(matches!(core("hello"), ValueData::String(s) if s == "hello"));
        assert!(matches!(core("1.2.3"), ValueData::String(s) if s == "1.2.3"));
    }

    #[test]
    fn non_plain_styles_are_always_strings() {
        assert!(matches!(
            resolve("true", ScalarStyle::SingleQuoted, Schema::Core1_2),
            ValueData::String(s) if s == "true"
        ));
        assert!(matches!(
            resolve("42", ScalarStyle::DoubleQuoted, Schema::Core1_2),
            ValueData::String(s) if s == "42"
        ));
    }

    #[test]
    fn integer_overflow_falls_back_to_string() {
        assert!(matches!(
            core("99999999999999999999999"),
            ValueData::String(_)
        ));
    }
}
