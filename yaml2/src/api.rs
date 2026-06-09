//! Public, ergonomic entry points: source text -> `Value`.

use crate::composer::compose;
use crate::emitter::{emit, emit_documents};
use crate::error::{Error, ErrorKind, Result};
use crate::options::{EmitOptions, ParseOptions};
use crate::parser::parse_events;
use crate::value::Value;

/// Parses a single-document YAML string into a `Value` using default options.
///
/// An empty stream yields `Value::null()`. More than one document is an error;
/// use [`parse_documents`] for multi-document streams.
pub fn parse(input: &str) -> Result<Value> {
    parse_with(input, &ParseOptions::default())
}

/// Like [`parse`], with explicit options.
pub fn parse_with(input: &str, options: &ParseOptions) -> Result<Value> {
    let mut documents = parse_documents_with(input, options)?;
    match documents.len() {
        0 => Ok(Value::null()),
        1 => Ok(documents.pop().expect("len checked == 1")),
        n => Err(Error::new(
            ErrorKind::Compose,
            format!("expected a single document, found {n}; use parse_documents"),
        )),
    }
}

/// Parses every document in a YAML stream using default options.
pub fn parse_documents(input: &str) -> Result<Vec<Value>> {
    parse_documents_with(input, &ParseOptions::default())
}

/// Like [`parse_documents`], with explicit options.
pub fn parse_documents_with(input: &str, options: &ParseOptions) -> Result<Vec<Value>> {
    let events = parse_events(input, options)?;
    compose(&events, options)
}

/// Serializes any serializable value to a YAML document string.
#[cfg(feature = "serde")]
pub fn to_string<T: ?Sized + serde::Serialize>(value: &T) -> Result<String> {
    let value = crate::serde_support::to_value(value)?;
    to_string_with(&value, &EmitOptions::default())
}

/// Serializes a `Value` to a YAML document string using default options.
#[cfg(not(feature = "serde"))]
pub fn to_string(value: &Value) -> Result<String> {
    to_string_with(value, &EmitOptions::default())
}

/// Parses a single-document YAML string into any deserializable type.
#[cfg(feature = "serde")]
pub fn from_str<T: serde::de::DeserializeOwned>(input: &str) -> Result<T> {
    let value = parse(input)?;
    crate::serde_support::from_value(value)
}

/// Like [`to_string`], with explicit options.
pub fn to_string_with(value: &Value, options: &EmitOptions) -> Result<String> {
    Ok(emit(value, options))
}

/// Serializes multiple values to a multi-document YAML stream using default options.
pub fn to_string_documents(values: &[Value]) -> Result<String> {
    to_string_documents_with(values, &EmitOptions::default())
}

/// Like [`to_string_documents`], with explicit options.
pub fn to_string_documents_with(values: &[Value], options: &EmitOptions) -> Result<String> {
    Ok(emit_documents(values, options))
}

#[cfg(all(test, feature = "serde"))]
mod serde_roundtrip_tests {
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct Server {
        host: String,
        port: u16,
        tags: Vec<String>,
        enabled: bool,
    }

    #[test]
    fn from_str_into_struct() {
        let yaml = "host: localhost\nport: 8080\ntags: [a, b]\nenabled: true\n";
        let s: Server = crate::api::from_str(yaml).unwrap();
        assert_eq!(
            s,
            Server {
                host: "localhost".to_string(),
                port: 8080,
                tags: vec!["a".to_string(), "b".to_string()],
                enabled: true,
            }
        );
    }

    #[test]
    fn to_string_from_struct_roundtrips() {
        let server = Server {
            host: "db".to_string(),
            port: 5432,
            tags: vec!["primary".to_string()],
            enabled: false,
        };
        let yaml = crate::api::to_string(&server).unwrap();
        let back: Server = crate::api::from_str(&yaml).unwrap();
        assert_eq!(server, back);
    }

    #[test]
    fn to_string_still_works_on_value() {
        // The generic `to_string` must keep accepting a `Value` (Value: Serialize).
        let v = crate::Value::int(42);
        assert_eq!(crate::api::to_string(&v).unwrap(), "42\n");
    }

    #[test]
    fn optional_and_default_fields() {
        #[derive(Serialize, Deserialize, Debug, PartialEq)]
        struct Config {
            name: String,
            note: Option<String>,
        }
        let parsed: Config = crate::api::from_str("name: x\n").unwrap();
        assert_eq!(
            parsed,
            Config {
                name: "x".to_string(),
                note: None
            }
        );
    }
}
