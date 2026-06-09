//! Plain-scalar resolution: raw text -> typed `ValueData`, per schema.

use crate::meta::ScalarStyle;
use crate::options::Schema;
use crate::value::ValueData;

/// Resolves a scalar's raw text into typed data according to `style` and `schema`.
///
/// Non-plain scalars (quoted, literal, folded) are always strings.
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
    // Core 1.2 int grammar is `[-+]? [0-9]+`, so a leading `+` is valid here.
    // (The JSON schema's stricter no-`+` rule is handled in resolve_json.)
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
    // Core 1.2 floats permit a leading `+` and a leading-dot form like `.5`;
    // `f64::parse` is the final arbiter of structural validity.
    let allowed = |b: u8| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-');
    if !raw.bytes().all(allowed) {
        return None;
    }
    raw.parse::<f64>().ok()
}

fn resolve_json(raw: &str) -> ValueData {
    match raw {
        "null" => return ValueData::Null,
        "true" => return ValueData::Bool(true),
        "false" => return ValueData::Bool(false),
        _ => {}
    }
    match classify_json_number(raw) {
        JsonNumber::Int => match raw.parse::<i64>() {
            Ok(i) => ValueData::Int(i),
            Err(_) => ValueData::String(raw.to_string()),
        },
        JsonNumber::Float => match raw.parse::<f64>() {
            Ok(f) => ValueData::Float(f),
            Err(_) => ValueData::String(raw.to_string()),
        },
        JsonNumber::No => ValueData::String(raw.to_string()),
    }
}

enum JsonNumber {
    Int,
    Float,
    No,
}

/// Classifies `raw` against the JSON number grammar:
/// `-? (0 | [1-9][0-9]*) (. [0-9]+)? ([eE] [+-]? [0-9]+)?`
fn classify_json_number(raw: &str) -> JsonNumber {
    let bytes = raw.as_bytes();
    let mut i = 0;

    if i < bytes.len() && bytes[i] == b'-' {
        i += 1;
    }

    // Integer part.
    let int_start = i;
    if i < bytes.len() && bytes[i] == b'0' {
        i += 1;
    } else {
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
    }
    if i == int_start {
        return JsonNumber::No; // no integer digits
    }

    let mut is_float = false;

    // Fraction.
    if i < bytes.len() && bytes[i] == b'.' {
        is_float = true;
        i += 1;
        let frac_start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == frac_start {
            return JsonNumber::No; // '.' with no digits
        }
    }

    // Exponent.
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        is_float = true;
        i += 1;
        if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
            i += 1;
        }
        let exp_start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == exp_start {
            return JsonNumber::No; // exponent with no digits
        }
    }

    if i != bytes.len() {
        return JsonNumber::No; // trailing junk
    }
    if is_float {
        JsonNumber::Float
    } else {
        JsonNumber::Int
    }
}

// YAML 1.1 scalar resolution. Covers the user-facing differences from Core 1.2:
// the "Norway problem" booleans (yes/no/on/off/y/n) and leading-zero octal.
// Deferred to a later hardening pass: binary (0b...), sexagesimal (1:2:3),
// underscore digit separators, and `.inf`/`.nan` float specials (currently
// resolved as strings under the 1.1 schema).
fn resolve_yaml11(raw: &str) -> ValueData {
    match raw {
        "" | "~" | "null" | "Null" | "NULL" => return ValueData::Null,
        _ => {}
    }
    if let Some(b) = yaml11_bool(raw) {
        return ValueData::Bool(b);
    }
    // Leading-zero octal: 0[0-7]+ (but plain "0" is decimal zero).
    if let Some(rest) = raw.strip_prefix('0') {
        if !rest.is_empty() && rest.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
            if let Ok(i) = i64::from_str_radix(rest, 8) {
                return ValueData::Int(i);
            }
        }
    }
    // Decimal int: [-+]?[0-9]+
    {
        let body = raw.strip_prefix(['+', '-']).unwrap_or(raw);
        if !body.is_empty() && body.bytes().all(|b| b.is_ascii_digit()) {
            return match raw.parse::<i64>() {
                Ok(i) => ValueData::Int(i),
                Err(_) => ValueData::String(raw.to_string()),
            };
        }
    }
    if let Some(f) = parse_core_float(raw) {
        return ValueData::Float(f);
    }
    ValueData::String(raw.to_string())
}

fn yaml11_bool(raw: &str) -> Option<bool> {
    match raw {
        "y" | "Y" | "yes" | "Yes" | "YES" | "true" | "True" | "TRUE" | "on" | "On" | "ON" => {
            Some(true)
        }
        "n" | "N" | "no" | "No" | "NO" | "false" | "False" | "FALSE" | "off" | "Off" | "OFF" => {
            Some(false)
        }
        _ => None,
    }
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

    #[test]
    fn core_plus_prefix_is_spec_compliant() {
        // YAML 1.2 Core int/float grammars both allow a leading `+`.
        assert!(matches!(core("+42"), ValueData::Int(42)));
        assert!(matches!(core("+0"), ValueData::Int(0)));
        assert!(matches!(core("+1.0"), ValueData::Float(f) if f == 1.0));
        assert!(matches!(core("+1e2"), ValueData::Float(f) if f == 100.0));
    }

    #[test]
    fn core_malformed_numbers_are_strings() {
        // Structurally invalid: f64::parse / int-shape reject these -> String.
        assert!(matches!(core("1+2"), ValueData::String(_)));
        assert!(matches!(core("+-1.0"), ValueData::String(_)));
        assert!(matches!(core("1e"), ValueData::String(_)));
        assert!(matches!(core("0x"), ValueData::String(_)));
        assert!(matches!(core("0o8"), ValueData::String(_)));
    }

    fn y11(raw: &str) -> ValueData {
        resolve(raw, ScalarStyle::Plain, Schema::Yaml1_1)
    }

    #[test]
    fn yaml11_norway_problem_present() {
        assert!(matches!(y11("NO"), ValueData::Bool(false)));
        assert!(matches!(y11("no"), ValueData::Bool(false)));
        assert!(matches!(y11("yes"), ValueData::Bool(true)));
        assert!(matches!(y11("on"), ValueData::Bool(true)));
        assert!(matches!(y11("off"), ValueData::Bool(false)));
        assert!(matches!(y11("y"), ValueData::Bool(true)));
        assert!(matches!(y11("N"), ValueData::Bool(false)));
    }

    #[test]
    fn yaml11_leading_zero_is_octal() {
        assert!(matches!(y11("0777"), ValueData::Int(511)));
        assert!(matches!(y11("0"), ValueData::Int(0)));
    }

    #[test]
    fn yaml11_decimal_and_string() {
        assert!(matches!(y11("42"), ValueData::Int(42)));
        assert!(matches!(y11("hello"), ValueData::String(s) if s == "hello"));
    }

    #[test]
    fn yaml11_null() {
        assert!(matches!(y11("~"), ValueData::Null));
        assert!(matches!(y11("null"), ValueData::Null));
        assert!(matches!(y11(""), ValueData::Null));
    }

    fn json(raw: &str) -> ValueData {
        resolve(raw, ScalarStyle::Plain, Schema::Json1_2)
    }

    #[test]
    fn json_strict_keywords() {
        assert!(matches!(json("null"), ValueData::Null));
        assert!(matches!(json("true"), ValueData::Bool(true)));
        assert!(matches!(json("false"), ValueData::Bool(false)));
        assert!(matches!(json("Null"), ValueData::String(s) if s == "Null"));
        assert!(matches!(json("True"), ValueData::String(s) if s == "True"));
    }

    #[test]
    fn json_numbers() {
        assert!(matches!(json("0"), ValueData::Int(0)));
        assert!(matches!(json("-12"), ValueData::Int(-12)));
        assert!(matches!(json("1.5"), ValueData::Float(f) if f == 1.5));
        assert!(matches!(json("1e3"), ValueData::Float(f) if f == 1000.0));
        assert!(matches!(json("01"), ValueData::String(s) if s == "01"));
        assert!(matches!(json("0x1F"), ValueData::String(s) if s == "0x1F"));
    }

    #[test]
    fn failsafe_everything_is_string() {
        assert!(matches!(
            resolve("true", ScalarStyle::Plain, Schema::Failsafe),
            ValueData::String(s) if s == "true"
        ));
        assert!(matches!(
            resolve("42", ScalarStyle::Plain, Schema::Failsafe),
            ValueData::String(s) if s == "42"
        ));
    }
}
