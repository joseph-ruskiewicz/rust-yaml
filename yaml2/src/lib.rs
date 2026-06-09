//! A modern, YAML 1.2-compliant parser and emitter for Rust.

mod error;

pub use error::{Position, Span};

#[cfg(test)]
mod smoke {
    #[test]
    fn workspace_builds() {
        assert_eq!(2 + 2, 4);
    }
}
