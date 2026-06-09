//! A modern, YAML 1.2-compliant parser and emitter for Rust.

mod error;
mod meta;

pub use error::{Error, ErrorKind, Position, Result, Span};
pub use meta::{Comments, Meta, ScalarStyle};

#[cfg(test)]
mod smoke {
    #[test]
    fn workspace_builds() {
        assert_eq!(2 + 2, 4);
    }
}
