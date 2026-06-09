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

/// Serializes a single value to a YAML document string using default options.
pub fn to_string(value: &Value) -> Result<String> {
    to_string_with(value, &EmitOptions::default())
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
