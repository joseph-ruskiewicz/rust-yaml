//! A modern, YAML 1.2-compliant parser and emitter for Rust.

mod error;
mod meta;
mod value;
mod options;
mod scalar;

pub use error::{Error, ErrorKind, Position, Result, Span};
pub use meta::{Comments, Meta, ScalarStyle};
pub use value::{Mapping, Value, ValueData};
pub use options::{EmitOptions, Limits, MergeKeys, ParseOptions, Schema};

#[cfg(test)]
mod smoke {
    #[test]
    fn workspace_builds() {
        assert_eq!(2 + 2, 4);
    }
}
