//! The scanner (lexer): turns source text into a flat token stream.
//!
//! This layer is consumed by the parser (Plan 4); until then its public(crate)
//! surface is exercised only by tests, so dead-code is allowed module-wide.
#![allow(dead_code)]

mod reader;
mod token;

use std::collections::VecDeque;

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
    /// Produced tokens not yet returned by `next_token`.
    tokens: VecDeque<Token>,
    /// Count of tokens already returned by `next_token` (for simple-key indexing).
    tokens_parsed: usize,
    started: bool,
    stream_end_produced: bool,
    flow_depth: usize,
    /// Current block indentation as a 0-based column; -1 == root.
    indent: i64,
    /// Stack of enclosing block indentations.
    indents: Vec<i64>,
    /// Whether a block simple key may begin at the current position.
    simple_key_allowed: bool,
    /// A buffered potential block mapping key, if any.
    simple_key: Option<SimpleKey>,
}

/// A buffered marker recording that a just-produced node might be a block
/// mapping key, to be confirmed when a `:` is found on the same line.
struct SimpleKey {
    /// Index of the key's first token in the overall token stream.
    token_number: usize,
    /// Source position where the key starts.
    mark: Position,
    /// Line the key began on (a key must be resolved on the same line).
    line: usize,
}

impl<'a> Scanner<'a> {
    pub(crate) fn new(input: &'a str, limits: Limits) -> Self {
        Self {
            reader: Reader::new(input),
            limits,
            tokens: VecDeque::new(),
            tokens_parsed: 0,
            started: false,
            stream_end_produced: false,
            flow_depth: 0,
            indent: -1,
            indents: Vec::new(),
            simple_key_allowed: true,
            simple_key: None,
        }
    }

    /// Returns the next token, or `None` once the stream has ended.
    pub(crate) fn next_token(&mut self) -> Result<Option<Token>> {
        loop {
            if let Some(token) = self.tokens.pop_front() {
                self.tokens_parsed += 1;
                return Ok(Some(token));
            }
            if self.stream_end_produced {
                return Ok(None);
            }
            self.fetch_more_tokens()?;
        }
    }

    /// Produces one or more tokens into the queue.
    fn fetch_more_tokens(&mut self) -> Result<()> {
        if !self.started {
            self.started = true;
            if self.reader.input_len() > self.limits.max_input_bytes {
                self.stream_end_produced = true;
                let pos = Position::new(0, 1, 1);
                return Err(Error::new(
                    ErrorKind::LimitExceeded,
                    "input exceeds the maximum allowed size",
                )
                .with_span(Span::new(pos, pos)));
            }
            let pos = self.reader.position();
            self.tokens
                .push_back(Token::new(TokenKind::StreamStart, Span::new(pos, pos)));
            return Ok(());
        }

        self.skip_to_next_token();

        // Block context: process indentation at the start of a line.
        if self.flow_depth == 0 {
            self.unroll_indent(Self::col0(self.reader.position()));
        }

        let start = self.reader.position();
        match self.reader.peek() {
            None => {
                if self.flow_depth == 0 {
                    self.unroll_indent(-1);
                }
                self.tokens
                    .push_back(Token::new(TokenKind::StreamEnd, Span::new(start, start)));
                self.stream_end_produced = true;
                Ok(())
            }
            Some(c) => {
                let token = self.scan_content(c, start)?;
                self.tokens.push_back(token);
                Ok(())
            }
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
            '.' if self.marker_ahead("...") => Ok(self.scan_marker(TokenKind::DocumentEnd, start)),
            '[' => {
                self.flow_depth += 1;
                Ok(self.single_char(TokenKind::FlowSequenceStart, start))
            }
            ']' => {
                self.flow_depth = self.flow_depth.saturating_sub(1);
                Ok(self.single_char(TokenKind::FlowSequenceEnd, start))
            }
            '{' => {
                self.flow_depth += 1;
                Ok(self.single_char(TokenKind::FlowMappingStart, start))
            }
            '}' => {
                self.flow_depth = self.flow_depth.saturating_sub(1);
                Ok(self.single_char(TokenKind::FlowMappingEnd, start))
            }
            ',' => Ok(self.single_char(TokenKind::FlowEntry, start)),
            ':' if self.flow_depth > 0 || self.indicator_terminator_next() => {
                Ok(self.single_char(TokenKind::Value, start))
            }
            '?' if self.indicator_terminator_next() => Ok(self.single_char(TokenKind::Key, start)),
            '\'' => self.scan_single_quoted(start),
            '"' => self.scan_double_quoted(start),
            '&' => self.scan_anchor_or_alias(start, true),
            '*' => self.scan_anchor_or_alias(start, false),
            '!' => Ok(self.scan_tag(start)),
            _ => self.scan_plain(start),
        }
    }

    /// Scans a single-line plain scalar in flow context.
    fn scan_plain(&mut self, start: Position) -> Result<Token> {
        let mut value = String::new();
        // Position immediately after the last non-whitespace character consumed,
        // so the span does not include trailing whitespace that gets trimmed.
        let mut content_end = start;
        loop {
            match self.reader.peek() {
                None | Some('\n') | Some('\r') => break,
                Some(',') | Some('[') | Some(']') | Some('{') | Some('}') => break,
                Some(':') if self.indicator_terminator_next() => break,
                Some(' ') | Some('\t') => {
                    // A space before '#' ends the scalar (comment); otherwise the
                    // space may be internal — keep it and let trailing trim handle
                    // the end-of-scalar case.
                    if self.reader.peek_nth(1) == Some('#') {
                        break;
                    }
                    let c = self.reader.advance().unwrap();
                    value.push(c);
                }
                Some(c) => {
                    self.reader.advance();
                    value.push(c);
                    content_end = self.reader.position();
                }
            }
        }
        // `scan_plain` is only entered on a non-break character (break chars are
        // handled by earlier `scan_content` arms or the loop above), so at least
        // one character is always consumed.
        debug_assert!(!value.is_empty(), "scan_plain produced an empty scalar");
        // Internal whitespace is accumulated during the loop; only trailing
        // spaces/tabs are stripped here.
        let trimmed_len = value.trim_end_matches([' ', '\t']).len();
        value.truncate(trimmed_len);
        Ok(Token::new(
            TokenKind::Scalar {
                value,
                style: ScalarStyle::Plain,
            },
            Span::new(start, content_end),
        ))
    }

    /// Scans `&name` (anchor) or `*name` (alias). `is_anchor` selects which.
    fn scan_anchor_or_alias(&mut self, start: Position, is_anchor: bool) -> Result<Token> {
        self.reader.advance(); // '&' or '*'
        let name = self.take_name();
        if name.is_empty() {
            let what = if is_anchor { "anchor" } else { "alias" };
            return Err(Error::new(ErrorKind::Scan, format!("empty {what} name"))
                .with_span(Span::new(start, self.reader.position())));
        }
        let kind = if is_anchor {
            TokenKind::Anchor(name)
        } else {
            TokenKind::Alias(name)
        };
        Ok(Token::new(kind, Span::new(start, self.reader.position())))
    }

    /// Scans a tag token, keeping the raw text including the leading `!`.
    fn scan_tag(&mut self, start: Position) -> Token {
        self.reader.advance(); // leading '!'
        let mut text = String::from("!");
        // A second '!' is part of the handle (e.g. `!!str`).
        if self.reader.peek() == Some('!') {
            self.reader.advance();
            text.push('!');
        }
        text.push_str(&self.take_name());
        Token::new(
            TokenKind::Tag(text),
            Span::new(start, self.reader.position()),
        )
    }

    /// Consumes a run of name characters (non-whitespace, non-flow-indicator).
    fn take_name(&mut self) -> String {
        let mut name = String::new();
        while let Some(c) = self.reader.peek() {
            if c.is_whitespace() || matches!(c, ',' | '[' | ']' | '{' | '}') {
                break;
            }
            self.reader.advance();
            name.push(c);
        }
        name
    }

    fn scan_single_quoted(&mut self, start: Position) -> Result<Token> {
        self.reader.advance(); // opening quote
        let mut value = String::new();
        loop {
            match self.reader.peek() {
                None => {
                    return Err(
                        Error::new(ErrorKind::Scan, "unterminated single-quoted scalar")
                            .with_span(Span::new(start, self.reader.position())),
                    );
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
                            TokenKind::Scalar {
                                value,
                                style: ScalarStyle::SingleQuoted,
                            },
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
                    return Err(
                        Error::new(ErrorKind::Scan, "unterminated double-quoted scalar")
                            .with_span(Span::new(start, self.reader.position())),
                    );
                }
                Some('"') => {
                    self.reader.advance();
                    return Ok(Token::new(
                        TokenKind::Scalar {
                            value,
                            style: ScalarStyle::DoubleQuoted,
                        },
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
            let d = self
                .reader
                .advance()
                .and_then(|c| c.to_digit(16))
                .ok_or_else(|| {
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

    /// 0-based indentation column for a position.
    fn col0(pos: Position) -> i64 {
        pos.column as i64 - 1
    }

    /// Closes block levels deeper than `col`, emitting one `BlockEnd` each.
    /// Inert in flow context.
    fn unroll_indent(&mut self, col: i64) {
        if self.flow_depth > 0 {
            return;
        }
        while self.indent > col {
            let pos = self.reader.position();
            self.tokens
                .push_back(Token::new(TokenKind::BlockEnd, Span::new(pos, pos)));
            self.indent = self.indents.pop().unwrap_or(-1);
        }
    }

    /// Opens a new block level at `col`, inserting `start_kind` at queue index
    /// `at` (or appending if `None`). Returns true if a level was opened.
    /// Inert in flow context.
    fn roll_indent(
        &mut self,
        col: i64,
        start_kind: TokenKind,
        mark: Position,
        at: Option<usize>,
    ) -> bool {
        if self.flow_depth > 0 {
            return false;
        }
        if self.indent < col {
            self.indents.push(self.indent);
            self.indent = col;
            let token = Token::new(start_kind, Span::new(mark, mark));
            match at {
                Some(i) => self.tokens.insert(i, token),
                None => self.tokens.push_back(token),
            }
            true
        } else {
            false
        }
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
        assert_eq!(
            kinds(""),
            vec![TokenKind::StreamStart, TokenKind::StreamEnd]
        );
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
        let limits = Limits {
            max_input_bytes: 4,
            ..Limits::default()
        };
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

    #[test]
    fn plain_scalar_simple() {
        assert_eq!(
            scalars("hello"),
            vec![("hello".to_string(), ScalarStyle::Plain)]
        );
    }

    #[test]
    fn plain_scalar_keeps_internal_spaces_trims_trailing() {
        assert_eq!(
            scalars("the quick brown   "),
            vec![("the quick brown".to_string(), ScalarStyle::Plain)]
        );
    }

    #[test]
    fn plain_scalar_with_colon_in_content() {
        assert_eq!(
            scalars("http://x"),
            vec![("http://x".to_string(), ScalarStyle::Plain)]
        );
    }

    #[test]
    fn plain_scalar_terminated_by_value_indicator() {
        assert_eq!(
            kinds("key: value"),
            vec![
                TokenKind::StreamStart,
                TokenKind::Scalar {
                    value: "key".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::Value,
                TokenKind::Scalar {
                    value: "value".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn plain_scalar_in_flow_sequence() {
        assert_eq!(
            kinds("[a, b]"),
            vec![
                TokenKind::StreamStart,
                TokenKind::FlowSequenceStart,
                TokenKind::Scalar {
                    value: "a".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::FlowEntry,
                TokenKind::Scalar {
                    value: "b".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::FlowSequenceEnd,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn plain_scalar_terminated_by_comment() {
        assert_eq!(
            scalars("value # trailing comment"),
            vec![("value".to_string(), ScalarStyle::Plain)]
        );
    }

    #[test]
    fn plain_scalar_hash_without_preceding_space_is_content() {
        // '#' only starts a comment when preceded by whitespace.
        assert_eq!(
            scalars("a#b"),
            vec![("a#b".to_string(), ScalarStyle::Plain)]
        );
    }

    #[test]
    fn plain_scalar_trailing_colon_at_eof() {
        // 'abc:' -> scalar "abc" then a Value token (':' at EOF is an indicator).
        assert_eq!(
            kinds("abc:"),
            vec![
                TokenKind::StreamStart,
                TokenKind::Scalar {
                    value: "abc".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::Value,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn anchor_alias_tag() {
        assert_eq!(
            kinds("&a *b !c"),
            vec![
                TokenKind::StreamStart,
                TokenKind::Anchor("a".to_string()),
                TokenKind::Alias("b".to_string()),
                TokenKind::Tag("!c".to_string()),
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn anchored_value_in_flow() {
        assert_eq!(
            kinds("[&x 1, *x]"),
            vec![
                TokenKind::StreamStart,
                TokenKind::FlowSequenceStart,
                TokenKind::Anchor("x".to_string()),
                TokenKind::Scalar {
                    value: "1".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::FlowEntry,
                TokenKind::Alias("x".to_string()),
                TokenKind::FlowSequenceEnd,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn double_bang_tag() {
        assert_eq!(
            kinds("!!str"),
            vec![
                TokenKind::StreamStart,
                TokenKind::Tag("!!str".to_string()),
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn empty_anchor_name_errors() {
        let err = tokenize("& ", Limits::default()).unwrap_err();
        assert_eq!(err.kind(), crate::error::ErrorKind::Scan);
    }

    #[test]
    fn full_flow_document_tokenizes() {
        let input = r#"{ name: "Ada", scores: [90, 88, *prev], tag: !lang ~ }"#;
        assert_eq!(
            kinds(input),
            vec![
                TokenKind::StreamStart,
                TokenKind::FlowMappingStart,
                TokenKind::Scalar {
                    value: "name".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::Value,
                TokenKind::Scalar {
                    value: "Ada".to_string(),
                    style: ScalarStyle::DoubleQuoted
                },
                TokenKind::FlowEntry,
                TokenKind::Scalar {
                    value: "scores".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::Value,
                TokenKind::FlowSequenceStart,
                TokenKind::Scalar {
                    value: "90".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::FlowEntry,
                TokenKind::Scalar {
                    value: "88".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::FlowEntry,
                TokenKind::Alias("prev".to_string()),
                TokenKind::FlowSequenceEnd,
                TokenKind::FlowEntry,
                TokenKind::Scalar {
                    value: "tag".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::Value,
                TokenKind::Tag("!lang".to_string()),
                TokenKind::Scalar {
                    value: "~".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::FlowMappingEnd,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn multiple_documents() {
        assert_eq!(
            kinds("--- [1]\n--- [2]\n"),
            vec![
                TokenKind::StreamStart,
                TokenKind::DocumentStart,
                TokenKind::FlowSequenceStart,
                TokenKind::Scalar {
                    value: "1".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::FlowSequenceEnd,
                TokenKind::DocumentStart,
                TokenKind::FlowSequenceStart,
                TokenKind::Scalar {
                    value: "2".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::FlowSequenceEnd,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn spans_are_accurate_for_a_small_input() {
        let toks = tokenize("a, b", Limits::default()).unwrap();
        let scalar_a = &toks[1];
        assert!(matches!(&scalar_a.kind, TokenKind::Scalar { value, .. } if value == "a"));
        assert_eq!(scalar_a.span.start, crate::error::Position::new(0, 1, 1));
        assert_eq!(scalar_a.span.end, crate::error::Position::new(1, 1, 2));

        let comma = &toks[2];
        assert_eq!(comma.kind, TokenKind::FlowEntry);
        assert_eq!(comma.span.start, crate::error::Position::new(1, 1, 2));
        assert_eq!(comma.span.end, crate::error::Position::new(2, 1, 3));
    }

    #[test]
    fn plain_scalar_span_excludes_trailing_whitespace() {
        // "a , b": scalar "a" with a trailing space before the comma.
        let toks = tokenize("a , b", Limits::default()).unwrap();
        let scalar_a = &toks[1];
        assert!(matches!(&scalar_a.kind, TokenKind::Scalar { value, .. } if value == "a"));
        // Span must end right after 'a' (offset 1, col 2), NOT after the space.
        assert_eq!(scalar_a.span.start, crate::error::Position::new(0, 1, 1));
        assert_eq!(scalar_a.span.end, crate::error::Position::new(1, 1, 2));
    }

    #[test]
    fn compact_json_flow_mapping() {
        assert_eq!(
            kinds(r#"{"a":"b"}"#),
            vec![
                TokenKind::StreamStart,
                TokenKind::FlowMappingStart,
                TokenKind::Scalar {
                    value: "a".to_string(),
                    style: ScalarStyle::DoubleQuoted
                },
                TokenKind::Value,
                TokenKind::Scalar {
                    value: "b".to_string(),
                    style: ScalarStyle::DoubleQuoted
                },
                TokenKind::FlowMappingEnd,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn compact_json_nested() {
        assert_eq!(
            kinds(r#"{"k":[1,2]}"#),
            vec![
                TokenKind::StreamStart,
                TokenKind::FlowMappingStart,
                TokenKind::Scalar {
                    value: "k".to_string(),
                    style: ScalarStyle::DoubleQuoted
                },
                TokenKind::Value,
                TokenKind::FlowSequenceStart,
                TokenKind::Scalar {
                    value: "1".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::FlowEntry,
                TokenKind::Scalar {
                    value: "2".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::FlowSequenceEnd,
                TokenKind::FlowMappingEnd,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn plain_colon_in_flow_sequence_stays_plain() {
        // In flow, `a:b` (colon not followed by space/flow-char) is ONE plain scalar.
        assert_eq!(
            kinds("[a:b]"),
            vec![
                TokenKind::StreamStart,
                TokenKind::FlowSequenceStart,
                TokenKind::Scalar {
                    value: "a:b".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::FlowSequenceEnd,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn root_colon_still_requires_terminator() {
        // Outside flow, `:` only acts as a value indicator with a following
        // terminator. `:x` at root is a plain scalar starting with a colon.
        assert_eq!(scalars(":x"), vec![(":x".to_string(), ScalarStyle::Plain)]);
    }

    #[test]
    fn bare_scalar_document_still_works() {
        assert_eq!(
            kinds("hello"),
            vec![
                TokenKind::StreamStart,
                TokenKind::Scalar { value: "hello".to_string(), style: ScalarStyle::Plain },
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn block_indent_helpers_unroll_to_root_at_eof() {
        let toks = tokenize("hello\n", Limits::default()).unwrap();
        assert!(!toks.iter().any(|t| t.kind == TokenKind::BlockEnd));
    }
}
