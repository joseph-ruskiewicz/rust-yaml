//! A modern, YAML 1.2-compliant parser and emitter for Rust.

mod api;
mod composer;
mod emitter;
mod error;
mod event;
mod meta;
mod options;
mod parser;
mod scalar;
mod scanner;
mod value;

pub use api::{
    parse, parse_documents, parse_documents_with, parse_with, to_string, to_string_documents,
    to_string_documents_with, to_string_with,
};
pub use error::{Error, ErrorKind, Position, Result, Span};
pub use event::{Event, EventKind};
pub use meta::{Comments, Meta, ScalarStyle};
pub use options::{EmitOptions, Limits, MergeKeys, ParseOptions, Schema};
pub use parser::{parse_events, Parser};
pub use value::{Mapping, Value, ValueData};

#[cfg(test)]
mod smoke {
    #[test]
    fn workspace_builds() {
        assert_eq!(2 + 2, 4);
    }
}
