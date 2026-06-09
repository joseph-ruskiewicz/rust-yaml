//! A modern, YAML 1.2-compliant parser and emitter for Rust.

mod error;
mod meta;
mod options;
mod scalar;
mod value;

pub use error::{Error, ErrorKind, Position, Result, Span};
pub use meta::{Comments, Meta, ScalarStyle};
pub use options::{EmitOptions, Limits, MergeKeys, ParseOptions, Schema};
pub use value::{Mapping, Value, ValueData};

#[cfg(test)]
mod smoke {
    #[test]
    fn workspace_builds() {
        assert_eq!(2 + 2, 4);
    }
}
