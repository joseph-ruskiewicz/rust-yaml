//! The scanner (lexer): turns source text into a flat token stream.
//!
//! This layer is consumed by the parser (Plan 4); until then its public(crate)
//! surface is exercised only by tests, so dead-code is allowed module-wide.
#![allow(dead_code, unused_imports)]

mod reader;
mod token;

pub(crate) use reader::Reader;
pub(crate) use token::{Token, TokenKind};
