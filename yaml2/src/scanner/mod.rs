//! The scanner (lexer): turns source text into a flat token stream.
//!
//! This layer is consumed by the parser (Plan 4); until then its public(crate)
//! surface is exercised only by tests, so dead-code is allowed module-wide.
#![allow(dead_code, unused_imports)]

mod reader;
mod token;

use crate::error::{Error, ErrorKind, Position, Result, Span};
use crate::options::Limits;

pub(crate) use reader::Reader;
pub(crate) use token::{Token, TokenKind};

/// Tokenizes `input` fully into a vector of tokens.
pub(crate) fn tokenize(input: &str, limits: Limits) -> Result<Vec<Token>> {
    let mut scanner = Scanner::new(input, limits);
    let mut tokens = Vec::new();
    while let Some(token) = scanner.next_token()? {
        tokens.push(token);
    }
    Ok(tokens)
}

/// A streaming scanner over the input.
pub(crate) struct Scanner<'a> {
    reader: Reader<'a>,
    limits: Limits,
    started: bool,
    finished: bool,
}

impl<'a> Scanner<'a> {
    pub(crate) fn new(input: &'a str, limits: Limits) -> Self {
        Self {
            reader: Reader::new(input),
            limits,
            started: false,
            finished: false,
        }
    }

    /// Returns the next token, or `None` once the stream has ended.
    pub(crate) fn next_token(&mut self) -> Result<Option<Token>> {
        if !self.started {
            self.started = true;
            if self.reader.input_len() > self.limits.max_input_bytes {
                let pos = Position::new(0, 1, 1);
                return Err(Error::new(
                    ErrorKind::LimitExceeded,
                    "input exceeds the maximum allowed size",
                )
                .with_span(Span::new(pos, pos)));
            }
            let pos = self.reader.position();
            return Ok(Some(Token::new(TokenKind::StreamStart, Span::new(pos, pos))));
        }

        if self.finished {
            return Ok(None);
        }

        self.skip_to_next_token();
        let start = self.reader.position();
        match self.reader.peek() {
            None => {
                self.finished = true;
                Ok(Some(Token::new(TokenKind::StreamEnd, Span::new(start, start))))
            }
            Some(c) => self.scan_content(c, start).map(Some),
        }
    }

    /// Skips insignificant whitespace, line breaks, and comments. In the flow
    /// subset, line breaks between tokens are not significant.
    fn skip_to_next_token(&mut self) {
        loop {
            match self.reader.peek() {
                Some(' ') | Some('\t') | Some('\n') | Some('\r') => {
                    self.reader.advance();
                }
                Some('#') => {
                    while let Some(c) = self.reader.peek() {
                        if c == '\n' || c == '\r' {
                            break;
                        }
                        self.reader.advance();
                    }
                }
                _ => break,
            }
        }
    }

    /// Scans a content token starting at character `c`. Arms for the various
    /// token kinds are added in later tasks; until then anything is an error.
    fn scan_content(&mut self, c: char, start: Position) -> Result<Token> {
        Err(Error::new(ErrorKind::Scan, format!("unexpected character {c:?}"))
            .with_span(Span::new(start, self.reader.position())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::Limits;

    fn kinds(input: &str) -> Vec<TokenKind> {
        tokenize(input, Limits::default())
            .unwrap()
            .into_iter()
            .map(|t| t.kind)
            .collect()
    }

    #[test]
    fn empty_input_is_stream_start_then_end() {
        assert_eq!(kinds(""), vec![TokenKind::StreamStart, TokenKind::StreamEnd]);
    }

    #[test]
    fn whitespace_and_comments_are_skipped() {
        assert_eq!(
            kinds("   # a comment\n\n  # another\n"),
            vec![TokenKind::StreamStart, TokenKind::StreamEnd]
        );
    }

    #[test]
    fn input_over_size_limit_errors() {
        let limits = Limits { max_input_bytes: 4, ..Limits::default() };
        let err = tokenize("abcdef", limits).unwrap_err();
        assert_eq!(err.kind(), crate::error::ErrorKind::LimitExceeded);
    }

    #[test]
    fn stream_start_is_zero_width_at_origin() {
        let toks = tokenize("", Limits::default()).unwrap();
        assert_eq!(toks[0].span.start, crate::error::Position::new(0, 1, 1));
        assert_eq!(toks[0].span.end, crate::error::Position::new(0, 1, 1));
    }
}
