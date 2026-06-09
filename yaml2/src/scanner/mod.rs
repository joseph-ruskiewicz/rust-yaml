//! The scanner (lexer): turns source text into a flat token stream.
//!
//! This layer is consumed by the parser (Plan 4); until then its public(crate)
//! surface is exercised only by tests, so dead-code is allowed module-wide.
#![allow(dead_code, unused_imports)]

mod reader;
mod token;

use crate::error::{Error, ErrorKind, Position, Result, Span};
use crate::meta::ScalarStyle;
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
                self.finished = true;
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

    /// Scans a content token starting at character `c`.
    fn scan_content(&mut self, c: char, start: Position) -> Result<Token> {
        match c {
            '-' if self.marker_ahead("---") => {
                Ok(self.scan_marker(TokenKind::DocumentStart, start))
            }
            '.' if self.marker_ahead("...") => {
                Ok(self.scan_marker(TokenKind::DocumentEnd, start))
            }
            '[' => Ok(self.single_char(TokenKind::FlowSequenceStart, start)),
            ']' => Ok(self.single_char(TokenKind::FlowSequenceEnd, start)),
            '{' => Ok(self.single_char(TokenKind::FlowMappingStart, start)),
            '}' => Ok(self.single_char(TokenKind::FlowMappingEnd, start)),
            ',' => Ok(self.single_char(TokenKind::FlowEntry, start)),
            ':' if self.indicator_terminator_next() => {
                Ok(self.single_char(TokenKind::Value, start))
            }
            '?' if self.indicator_terminator_next() => {
                Ok(self.single_char(TokenKind::Key, start))
            }
            '\'' => self.scan_single_quoted(start),
            '"' => self.scan_double_quoted(start),
            _ => Err(Error::new(ErrorKind::Scan, format!("unexpected character {c:?}"))
                .with_span(Span::new(start, self.reader.position()))),
        }
    }

    fn scan_single_quoted(&mut self, start: Position) -> Result<Token> {
        self.reader.advance(); // opening quote
        let mut value = String::new();
        loop {
            match self.reader.peek() {
                None => {
                    return Err(Error::new(
                        ErrorKind::Scan,
                        "unterminated single-quoted scalar",
                    )
                    .with_span(Span::new(start, self.reader.position())));
                }
                Some('\'') => {
                    self.reader.advance(); // consume the quote
                    if self.reader.peek() == Some('\'') {
                        // Doubled '' -> literal single quote.
                        self.reader.advance();
                        value.push('\'');
                    } else {
                        // Closing quote.
                        return Ok(Token::new(
                            TokenKind::Scalar { value, style: ScalarStyle::SingleQuoted },
                            Span::new(start, self.reader.position()),
                        ));
                    }
                }
                Some(c) => {
                    self.reader.advance();
                    value.push(c);
                }
            }
        }
    }

    fn scan_double_quoted(&mut self, start: Position) -> Result<Token> {
        self.reader.advance(); // opening quote
        let mut value = String::new();
        loop {
            match self.reader.peek() {
                None => {
                    return Err(Error::new(
                        ErrorKind::Scan,
                        "unterminated double-quoted scalar",
                    )
                    .with_span(Span::new(start, self.reader.position())));
                }
                Some('"') => {
                    self.reader.advance();
                    return Ok(Token::new(
                        TokenKind::Scalar { value, style: ScalarStyle::DoubleQuoted },
                        Span::new(start, self.reader.position()),
                    ));
                }
                Some('\\') => {
                    self.reader.advance(); // consume backslash
                    let ch = self.scan_escape(start)?;
                    value.push(ch);
                }
                Some(c) => {
                    self.reader.advance();
                    value.push(c);
                }
            }
        }
    }

    /// Resolves a double-quoted escape sequence (the backslash is already
    /// consumed) into a single character.
    fn scan_escape(&mut self, start: Position) -> Result<char> {
        let esc = self.reader.advance().ok_or_else(|| {
            Error::new(ErrorKind::Scan, "unterminated escape sequence")
                .with_span(Span::new(start, self.reader.position()))
        })?;
        let ch = match esc {
            '0' => '\u{0}',
            'a' => '\u{7}',
            'b' => '\u{8}',
            't' | '\t' => '\u{9}',
            'n' => '\u{a}',
            'v' => '\u{b}',
            'f' => '\u{c}',
            'r' => '\u{d}',
            'e' => '\u{1b}',
            ' ' => ' ',
            '"' => '"',
            '/' => '/',
            '\\' => '\\',
            'N' => '\u{85}',
            '_' => '\u{a0}',
            'L' => '\u{2028}',
            'P' => '\u{2029}',
            'x' => self.scan_hex_escape(2, start)?,
            'u' => self.scan_hex_escape(4, start)?,
            'U' => self.scan_hex_escape(8, start)?,
            other => {
                return Err(Error::new(
                    ErrorKind::Scan,
                    format!("invalid escape sequence '\\{other}'"),
                )
                .with_span(Span::new(start, self.reader.position())));
            }
        };
        Ok(ch)
    }

    /// Reads `n` hex digits and converts them to a Unicode scalar value.
    fn scan_hex_escape(&mut self, n: usize, start: Position) -> Result<char> {
        let mut code: u32 = 0;
        for _ in 0..n {
            let d = self.reader.advance().and_then(|c| c.to_digit(16)).ok_or_else(|| {
                Error::new(ErrorKind::Scan, "invalid hex escape")
                    .with_span(Span::new(start, self.reader.position()))
            })?;
            code = code * 16 + d;
        }
        char::from_u32(code).ok_or_else(|| {
            Error::new(
                ErrorKind::Scan,
                format!("escape resolves to invalid Unicode scalar U+{code:04X}"),
            )
            .with_span(Span::new(start, self.reader.position()))
        })
    }

    /// True if the input begins with `marker` followed by whitespace, a line
    /// break, or end-of-input.
    fn marker_ahead(&self, marker: &str) -> bool {
        if !self.reader.starts_with(marker) {
            return false;
        }
        matches!(
            self.reader.peek_nth(marker.len()),
            None | Some(' ') | Some('\t') | Some('\n') | Some('\r')
        )
    }

    /// Consumes a three-character document marker.
    fn scan_marker(&mut self, kind: TokenKind, start: Position) -> Token {
        for _ in 0..3 {
            self.reader.advance();
        }
        Token::new(kind, Span::new(start, self.reader.position()))
    }

    /// Consumes one character and produces a token spanning it.
    fn single_char(&mut self, kind: TokenKind, start: Position) -> Token {
        self.reader.advance();
        Token::new(kind, Span::new(start, self.reader.position()))
    }

    /// True if the character after the current one is whitespace, a line break,
    /// a flow indicator, or end-of-input — i.e. `:`/`?` act as indicators.
    fn indicator_terminator_next(&self) -> bool {
        matches!(
            self.reader.peek_nth(1),
            None | Some(' ')
                | Some('\t')
                | Some('\n')
                | Some('\r')
                | Some(',')
                | Some('[')
                | Some(']')
                | Some('{')
                | Some('}')
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::ScalarStyle;
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
    fn flow_collection_indicators() {
        assert_eq!(
            kinds("[]{}"),
            vec![
                TokenKind::StreamStart,
                TokenKind::FlowSequenceStart,
                TokenKind::FlowSequenceEnd,
                TokenKind::FlowMappingStart,
                TokenKind::FlowMappingEnd,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn flow_entry_and_value_and_key() {
        assert_eq!(
            kinds(", : ?"),
            vec![
                TokenKind::StreamStart,
                TokenKind::FlowEntry,
                TokenKind::Value,
                TokenKind::Key,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn value_indicator_before_flow_end() {
        assert_eq!(
            kinds("{:}"),
            vec![
                TokenKind::StreamStart,
                TokenKind::FlowMappingStart,
                TokenKind::Value,
                TokenKind::FlowMappingEnd,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn document_start_and_end_markers() {
        assert_eq!(
            kinds("---\n...\n"),
            vec![
                TokenKind::StreamStart,
                TokenKind::DocumentStart,
                TokenKind::DocumentEnd,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn document_start_at_eof() {
        assert_eq!(
            kinds("---"),
            vec![
                TokenKind::StreamStart,
                TokenKind::DocumentStart,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn triple_dash_followed_by_content_is_marker_then_token() {
        assert_eq!(
            kinds("--- ["),
            vec![
                TokenKind::StreamStart,
                TokenKind::DocumentStart,
                TokenKind::FlowSequenceStart,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn stream_start_is_zero_width_at_origin() {
        let toks = tokenize("", Limits::default()).unwrap();
        assert_eq!(toks[0].span.start, crate::error::Position::new(0, 1, 1));
        assert_eq!(toks[0].span.end, crate::error::Position::new(0, 1, 1));
    }

    fn scalars(input: &str) -> Vec<(String, ScalarStyle)> {
        tokenize(input, Limits::default())
            .unwrap()
            .into_iter()
            .filter_map(|t| match t.kind {
                TokenKind::Scalar { value, style } => Some((value, style)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn single_quoted_basic() {
        assert_eq!(
            scalars("'hello world'"),
            vec![("hello world".to_string(), ScalarStyle::SingleQuoted)]
        );
    }

    #[test]
    fn single_quoted_doubled_quote_is_literal_quote() {
        assert_eq!(
            scalars("'it''s'"),
            vec![("it's".to_string(), ScalarStyle::SingleQuoted)]
        );
    }

    #[test]
    fn unterminated_single_quote_errors() {
        let err = tokenize("'oops", Limits::default()).unwrap_err();
        assert_eq!(err.kind(), crate::error::ErrorKind::Scan);
    }

    #[test]
    fn double_quoted_basic() {
        assert_eq!(
            scalars("\"hello\""),
            vec![("hello".to_string(), ScalarStyle::DoubleQuoted)]
        );
    }

    #[test]
    fn double_quoted_simple_escapes() {
        assert_eq!(
            scalars("\"a\\tb\\nc\\\"d\\\\e\""),
            vec![("a\tb\nc\"d\\e".to_string(), ScalarStyle::DoubleQuoted)]
        );
    }

    #[test]
    fn double_quoted_unicode_escapes() {
        // \x41 == 'A', B == 'B', \U00000043 == 'C'
        assert_eq!(
            scalars("\"\\x41\\u0042\\U00000043\""),
            vec![("ABC".to_string(), ScalarStyle::DoubleQuoted)]
        );
    }

    #[test]
    fn double_quoted_null_and_special_named_escapes() {
        // \0 -> NUL, \N -> U+0085, \_ -> U+00A0
        let got = scalars("\"\\0\\N\\_\"");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1, ScalarStyle::DoubleQuoted);
        assert_eq!(got[0].0, "\u{0}\u{85}\u{a0}");
    }

    #[test]
    fn unterminated_double_quote_errors() {
        let err = tokenize("\"oops", Limits::default()).unwrap_err();
        assert_eq!(err.kind(), crate::error::ErrorKind::Scan);
    }

    #[test]
    fn invalid_escape_errors() {
        let err = tokenize("\"\\q\"", Limits::default()).unwrap_err();
        assert_eq!(err.kind(), crate::error::ErrorKind::Scan);
    }
}
