//! The scanner (lexer): turns source text into a flat token stream.
//!
//! This layer is consumed by the parser (Plan 5); until then its public(crate)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Chomping {
    Clip,
    Strip,
    Keep,
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
            if self.token_ready() {
                let token = self.tokens.pop_front().expect("token_ready guarantees one");
                self.tokens_parsed += 1;
                return Ok(Some(token));
            }
            if self.stream_end_produced && self.tokens.is_empty() {
                return Ok(None);
            }
            self.fetch_more_tokens()?;
        }
    }

    /// A front token may be returned only when no buffered simple key could
    /// still need to insert a `Key`/`BlockMappingStart` ahead of it.
    fn token_ready(&self) -> bool {
        if self.tokens.is_empty() {
            return false;
        }
        // Hold the front token while a simple key is still buffered: the upcoming
        // ':' may need to insert Key/BlockMappingStart ahead of it.
        self.simple_key.is_none()
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
        self.stale_simple_key();

        // Block context: process indentation at the start of a line.
        if self.flow_depth == 0 {
            self.unroll_indent(Self::col0(self.reader.position()));
        }

        let start = self.reader.position();
        match self.reader.peek() {
            None => {
                self.simple_key = None;
                if self.flow_depth == 0 {
                    self.unroll_indent(-1);
                }
                self.tokens
                    .push_back(Token::new(TokenKind::StreamEnd, Span::new(start, start)));
                self.stream_end_produced = true;
                Ok(())
            }
            Some(c) => {
                if self.flow_depth == 0 && Self::can_start_simple_key(c) {
                    self.save_simple_key(start);
                }
                let token = self.scan_content(c, start)?;
                if self.flow_depth == 0 && Self::is_node_token(&token.kind) {
                    // Block scalars end at a dedent boundary (already at the
                    // start of a new line). Keep simple_key_allowed true so that
                    // the first token on the next line can still be a mapping key.
                    self.simple_key_allowed = Self::is_block_scalar(&token.kind);
                }
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
                Some(' ') | Some('\t') => {
                    self.reader.advance();
                }
                Some('\n') | Some('\r') => {
                    self.reader.advance();
                    if self.flow_depth == 0 {
                        self.simple_key_allowed = true;
                    }
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
                self.unroll_indent(-1);
                self.remove_simple_key();
                self.simple_key_allowed = true;
                Ok(self.scan_marker(TokenKind::DocumentStart, start))
            }
            '.' if self.marker_ahead("...") => {
                self.unroll_indent(-1);
                self.remove_simple_key();
                self.simple_key_allowed = true;
                Ok(self.scan_marker(TokenKind::DocumentEnd, start))
            }
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
                if self.flow_depth == 0 {
                    self.fetch_block_value(start)
                } else {
                    Ok(self.single_char(TokenKind::Value, start))
                }
            }
            '?' if self.indicator_terminator_next() => Ok(self.single_char(TokenKind::Key, start)),
            '\'' => self.scan_single_quoted(start),
            '"' => self.scan_double_quoted(start),
            '&' => self.scan_anchor_or_alias(start, true),
            '*' => self.scan_anchor_or_alias(start, false),
            '!' => Ok(self.scan_tag(start)),
            '-' if self.flow_depth == 0 && self.block_entry_next() => self.fetch_block_entry(start),
            '|' if self.flow_depth == 0 => self.scan_block_scalar(true, start),
            '>' if self.flow_depth == 0 => self.scan_block_scalar(false, start),
            _ => self.scan_plain(start),
        }
    }

    /// Records that a block-level node beginning at `mark` could be a mapping
    /// key, if a simple key is currently allowed.
    fn save_simple_key(&mut self, mark: Position) {
        if self.flow_depth == 0 && self.simple_key_allowed {
            self.simple_key = Some(SimpleKey {
                token_number: self.tokens_parsed + self.tokens.len(),
                mark,
                line: mark.line,
            });
        }
    }

    /// Drops any buffered simple key.
    fn remove_simple_key(&mut self) {
        self.simple_key = None;
    }

    /// Releases a buffered simple key once scanning has moved past its line.
    /// A block simple key must be followed by `:` on its own line; otherwise the
    /// buffered node was a plain value, not a key.
    fn stale_simple_key(&mut self) {
        if let Some(key) = &self.simple_key {
            if key.line != self.reader.position().line {
                self.simple_key = None;
            }
        }
    }

    /// Whether a token represents a content node (scalar/anchor/alias/tag).
    /// After such a token at block level, a new simple key may not begin until
    /// a structural event (newline, block entry) re-enables it.
    fn is_node_token(kind: &TokenKind) -> bool {
        matches!(
            kind,
            TokenKind::Scalar { .. }
                | TokenKind::Anchor(_)
                | TokenKind::Alias(_)
                | TokenKind::Tag(_)
        )
    }

    /// Whether a token is a block scalar (literal or folded). Block scalars end
    /// at a dedent boundary, so `simple_key_allowed` must remain true afterward.
    fn is_block_scalar(kind: &TokenKind) -> bool {
        matches!(
            kind,
            TokenKind::Scalar { style: ScalarStyle::Literal, .. }
                | TokenKind::Scalar { style: ScalarStyle::Folded, .. }
        )
    }

    /// Whether `c` begins a node that could serve as a block mapping key
    /// (scalar, anchor, alias, or tag — i.e. not a structural indicator).
    fn can_start_simple_key(c: char) -> bool {
        !matches!(
            c,
            '-' | '?' | ':' | ',' | '[' | ']' | '{' | '}' | '#' | '|' | '>'
        )
    }

    /// Handles `:` in block context: converts a buffered simple key into a
    /// `Key` token (opening a `BlockMappingStart` if needed) and emits `Value`.
    fn fetch_block_value(&mut self, start: Position) -> Result<Token> {
        if let Some(key) = self.simple_key.take() {
            let index = key.token_number - self.tokens_parsed;
            let mut at = index;
            if self.roll_indent(
                Self::col0(key.mark),
                TokenKind::BlockMappingStart,
                key.mark,
                Some(at),
            ) {
                at += 1;
            }
            self.tokens.insert(
                at,
                Token::new(TokenKind::Key, Span::new(key.mark, key.mark)),
            );
            self.simple_key_allowed = false;
        } else {
            self.roll_indent(Self::col0(start), TokenKind::BlockMappingStart, start, None);
            self.simple_key_allowed = true;
        }
        self.reader.advance(); // consume ':'
        Ok(Token::new(
            TokenKind::Value,
            Span::new(start, self.reader.position()),
        ))
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
            // A ':' that is a value indicator (followed by space/flow/EOF) ends
            // the name, so an alias/anchor/tag can serve as a mapping key
            // (`*x: v`). A ':' followed by other content stays in the name.
            if c == ':' && self.indicator_terminator_next() {
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
            if let Some(i) = at {
                debug_assert!(
                    i <= self.tokens.len(),
                    "roll_indent insert index out of range"
                );
            }
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

    /// True if the char after `-` makes it a block entry indicator.
    fn block_entry_next(&self) -> bool {
        matches!(
            self.reader.peek_nth(1),
            None | Some(' ') | Some('\t') | Some('\n') | Some('\r')
        )
    }

    /// Handles a `-` block sequence entry: opens a sequence if needed, emits
    /// `BlockEntry`, and consumes the dash.
    fn fetch_block_entry(&mut self, start: Position) -> Result<Token> {
        let col = Self::col0(start);
        self.roll_indent(col, TokenKind::BlockSequenceStart, start, None);
        self.reader.advance(); // consume '-'
        self.simple_key_allowed = true;
        Ok(Token::new(
            TokenKind::BlockEntry,
            Span::new(start, self.reader.position()),
        ))
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

    /// Consumes a YAML line break (`\n`, `\r\n`, or lone `\r`).
    fn consume_line_break(&mut self) {
        match self.reader.peek() {
            Some('\r') => {
                self.reader.advance();
                if self.reader.peek() == Some('\n') {
                    self.reader.advance();
                }
            }
            Some('\n') => {
                self.reader.advance();
            }
            _ => {}
        }
    }

    /// Parses the block-scalar header after `|`/`>`: chomping (`-`/`+`) and an
    /// indentation indicator (1-9) in any order, then skips trailing spaces, an
    /// optional comment, and the header line break.
    fn scan_block_scalar_header(&mut self) -> (Chomping, Option<usize>) {
        let mut chomp = Chomping::Clip;
        let mut indent: Option<usize> = None;
        loop {
            match self.reader.peek() {
                Some('-') if chomp == Chomping::Clip => {
                    self.reader.advance();
                    chomp = Chomping::Strip;
                }
                Some('+') if chomp == Chomping::Clip => {
                    self.reader.advance();
                    chomp = Chomping::Keep;
                }
                Some(c @ '1'..='9') if indent.is_none() => {
                    self.reader.advance();
                    indent = Some((c as u8 - b'0') as usize);
                }
                _ => break,
            }
        }
        while matches!(self.reader.peek(), Some(' ') | Some('\t')) {
            self.reader.advance();
        }
        if self.reader.peek() == Some('#') {
            while let Some(c) = self.reader.peek() {
                if c == '\n' || c == '\r' {
                    break;
                }
                self.reader.advance();
            }
        }
        self.consume_line_break();
        (chomp, indent)
    }

    /// Scans a block scalar (`literal` = `|`, else `>`).
    fn scan_block_scalar(&mut self, literal: bool, start: Position) -> Result<Token> {
        self.reader.advance(); // '|' or '>'
        let (chomp, explicit_indent) = self.scan_block_scalar_header();
        let parent = self.indent; // 0-based; -1 at root

        let mut content_indent: Option<usize> = explicit_indent.map(|n| {
            // `content_indent` is the number of leading spaces to strip. The
            // indentation indicator `n` (1-9) is relative to the parent block.
            let base = if parent < 0 { 0 } else { (parent + 1) as usize };
            base + n
        });

        let mut lines: Vec<(String, bool)> = Vec::new();
        loop {
            let mut sp = 0usize;
            while self.reader.peek_nth(sp) == Some(' ') {
                sp += 1;
            }
            let after = self.reader.peek_nth(sp);
            if after.is_none() {
                break;
            }
            let blank = matches!(after, Some('\n') | Some('\r'));
            if blank {
                for _ in 0..sp {
                    self.reader.advance();
                }
                self.consume_line_break();
                lines.push((String::new(), false));
                continue;
            }
            let ci = match content_indent {
                Some(ci) => ci,
                None => {
                    if (sp as i64) <= parent {
                        break;
                    }
                    content_indent = Some(sp);
                    sp
                }
            };
            if sp < ci {
                break;
            }
            for _ in 0..ci {
                self.reader.advance();
            }
            let more_indented = sp > ci;
            let mut text = String::new();
            while let Some(c) = self.reader.peek() {
                if c == '\n' || c == '\r' {
                    break;
                }
                text.push(c);
                self.reader.advance();
            }
            lines.push((text, more_indented));
            if self.reader.peek().is_none() {
                break;
            }
            self.consume_line_break();
        }

        let value = if literal {
            Self::assemble_literal(&lines, chomp)
        } else {
            Self::assemble_folded(&lines, chomp)
        };
        let style = if literal {
            ScalarStyle::Literal
        } else {
            ScalarStyle::Folded
        };
        Ok(Token::new(
            TokenKind::Scalar { value, style },
            Span::new(start, self.reader.position()),
        ))
    }

    /// Joins literal block-scalar lines (every line keeps its break), then chomps.
    fn assemble_literal(lines: &[(String, bool)], chomp: Chomping) -> String {
        let mut value = String::new();
        for (text, _) in lines {
            value.push_str(text);
            value.push('\n');
        }
        Self::apply_chomping(value, chomp)
    }

    /// Applies the chomping indicator to a value's trailing line breaks.
    fn apply_chomping(value: String, chomp: Chomping) -> String {
        match chomp {
            Chomping::Strip => value.trim_end_matches('\n').to_string(),
            Chomping::Clip => {
                let trimmed = value.trim_end_matches('\n');
                if trimmed.is_empty() {
                    String::new()
                } else {
                    format!("{trimmed}\n")
                }
            }
            Chomping::Keep => value,
        }
    }

    /// Folded assembly — implemented in Task P4.4 (temporary literal behavior).
    fn assemble_folded(lines: &[(String, bool)], chomp: Chomping) -> String {
        Self::assemble_literal(lines, chomp)
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
        // In flow context `:` and `?` are indicators; here we test them there.
        assert_eq!(
            kinds("{, : ?}"),
            vec![
                TokenKind::StreamStart,
                TokenKind::FlowMappingStart,
                TokenKind::FlowEntry,
                TokenKind::Value,
                TokenKind::Key,
                TokenKind::FlowMappingEnd,
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

    fn one_scalar(input: &str) -> (String, ScalarStyle) {
        let mut v: Vec<(String, ScalarStyle)> = tokenize(input, Limits::default())
            .unwrap()
            .into_iter()
            .filter_map(|t| match t.kind {
                TokenKind::Scalar { value, style } => Some((value, style)),
                _ => None,
            })
            .collect();
        assert_eq!(v.len(), 1, "expected exactly one scalar");
        v.pop().unwrap()
    }

    #[test]
    fn literal_block_scalar_basic() {
        assert_eq!(
            one_scalar("|\n  line one\n  line two\n"),
            ("line one\nline two\n".to_string(), ScalarStyle::Literal)
        );
    }

    #[test]
    fn literal_block_scalar_strip() {
        assert_eq!(
            one_scalar("|-\n  a\n  b\n"),
            ("a\nb".to_string(), ScalarStyle::Literal)
        );
    }

    #[test]
    fn literal_block_scalar_keep() {
        assert_eq!(
            one_scalar("|+\n  a\n\n"),
            ("a\n\n".to_string(), ScalarStyle::Literal)
        );
    }

    #[test]
    fn literal_block_scalar_blank_line_inside() {
        assert_eq!(
            one_scalar("|\n  a\n\n  b\n"),
            ("a\n\nb\n".to_string(), ScalarStyle::Literal)
        );
    }

    #[test]
    fn block_scalar_explicit_indent() {
        // `|2` at root: content indent is exactly 2 spaces; extra spaces on the
        // second line are content.
        assert_eq!(
            one_scalar("|2\n  a\n    b\n"),
            ("a\n  b\n".to_string(), ScalarStyle::Literal)
        );
    }

    #[test]
    fn block_scalar_indent_and_chomp_combined() {
        assert_eq!(
            one_scalar("|2-\n  a\n  b\n"),
            ("a\nb".to_string(), ScalarStyle::Literal)
        );
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
        // In block context `key: value` is a single-pair block mapping.
        assert_eq!(
            kinds("key: value"),
            vec![
                TokenKind::StreamStart,
                TokenKind::BlockMappingStart,
                TokenKind::Key,
                TokenKind::Scalar {
                    value: "key".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::Value,
                TokenKind::Scalar {
                    value: "value".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::BlockEnd,
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
        // 'abc:' -> block mapping: Key, scalar "abc", Value (no value scalar).
        assert_eq!(
            kinds("abc:"),
            vec![
                TokenKind::StreamStart,
                TokenKind::BlockMappingStart,
                TokenKind::Key,
                TokenKind::Scalar {
                    value: "abc".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::Value,
                TokenKind::BlockEnd,
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
                TokenKind::Scalar {
                    value: "hello".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn block_indent_helpers_unroll_to_root_at_eof() {
        let toks = tokenize("hello\n", Limits::default()).unwrap();
        assert!(!toks.iter().any(|t| t.kind == TokenKind::BlockEnd));
    }

    #[test]
    fn simple_block_sequence() {
        assert_eq!(
            kinds("- a\n- b\n"),
            vec![
                TokenKind::StreamStart,
                TokenKind::BlockSequenceStart,
                TokenKind::BlockEntry,
                TokenKind::Scalar {
                    value: "a".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::BlockEntry,
                TokenKind::Scalar {
                    value: "b".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::BlockEnd,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn nested_block_sequence() {
        assert_eq!(
            kinds("- - a\n"),
            vec![
                TokenKind::StreamStart,
                TokenKind::BlockSequenceStart,
                TokenKind::BlockEntry,
                TokenKind::BlockSequenceStart,
                TokenKind::BlockEntry,
                TokenKind::Scalar {
                    value: "a".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::BlockEnd,
                TokenKind::BlockEnd,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn simple_block_mapping() {
        assert_eq!(
            kinds("key: value\n"),
            vec![
                TokenKind::StreamStart,
                TokenKind::BlockMappingStart,
                TokenKind::Key,
                TokenKind::Scalar {
                    value: "key".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::Value,
                TokenKind::Scalar {
                    value: "value".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::BlockEnd,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn two_pair_block_mapping() {
        assert_eq!(
            kinds("a: 1\nb: 2\n"),
            vec![
                TokenKind::StreamStart,
                TokenKind::BlockMappingStart,
                TokenKind::Key,
                TokenKind::Scalar {
                    value: "a".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::Value,
                TokenKind::Scalar {
                    value: "1".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::Key,
                TokenKind::Scalar {
                    value: "b".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::Value,
                TokenKind::Scalar {
                    value: "2".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::BlockEnd,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn quoted_key_block_mapping() {
        assert_eq!(
            kinds("\"k\": v\n"),
            vec![
                TokenKind::StreamStart,
                TokenKind::BlockMappingStart,
                TokenKind::Key,
                TokenKind::Scalar {
                    value: "k".to_string(),
                    style: ScalarStyle::DoubleQuoted
                },
                TokenKind::Value,
                TokenKind::Scalar {
                    value: "v".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::BlockEnd,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn nested_mapping_value_on_following_lines() {
        assert_eq!(
            kinds("outer:\n  inner: v\n"),
            vec![
                TokenKind::StreamStart,
                TokenKind::BlockMappingStart,
                TokenKind::Key,
                TokenKind::Scalar {
                    value: "outer".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::Value,
                TokenKind::BlockMappingStart,
                TokenKind::Key,
                TokenKind::Scalar {
                    value: "inner".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::Value,
                TokenKind::Scalar {
                    value: "v".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::BlockEnd,
                TokenKind::BlockEnd,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn sequence_under_mapping_key() {
        // Indentless sequence: the `-` entries are at the same column as the
        // `items` key, so NO BlockSequenceStart envelope is emitted (the parser
        // treats bare BlockEntry tokens after Value as an indentless sequence).
        assert_eq!(
            kinds("items:\n- a\n- b\n"),
            vec![
                TokenKind::StreamStart,
                TokenKind::BlockMappingStart,
                TokenKind::Key,
                TokenKind::Scalar {
                    value: "items".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::Value,
                TokenKind::BlockEntry,
                TokenKind::Scalar {
                    value: "a".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::BlockEntry,
                TokenKind::Scalar {
                    value: "b".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::BlockEnd,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn indented_sequence_under_mapping_key_has_envelope() {
        // When the sequence is MORE indented than the key, it DOES get a
        // BlockSequenceStart/BlockEnd envelope.
        assert_eq!(
            kinds("items:\n  - a\n  - b\n"),
            vec![
                TokenKind::StreamStart,
                TokenKind::BlockMappingStart,
                TokenKind::Key,
                TokenKind::Scalar {
                    value: "items".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::Value,
                TokenKind::BlockSequenceStart,
                TokenKind::BlockEntry,
                TokenKind::Scalar {
                    value: "a".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::BlockEntry,
                TokenKind::Scalar {
                    value: "b".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::BlockEnd,
                TokenKind::BlockEnd,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn mapping_in_sequence_entry() {
        assert_eq!(
            kinds("- k: v\n"),
            vec![
                TokenKind::StreamStart,
                TokenKind::BlockSequenceStart,
                TokenKind::BlockEntry,
                TokenKind::BlockMappingStart,
                TokenKind::Key,
                TokenKind::Scalar {
                    value: "k".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::Value,
                TokenKind::Scalar {
                    value: "v".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::BlockEnd,
                TokenKind::BlockEnd,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn bare_scalar_is_not_a_key() {
        assert_eq!(
            kinds("hello\nworld\n"),
            vec![
                TokenKind::StreamStart,
                TokenKind::Scalar {
                    value: "hello".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::Scalar {
                    value: "world".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn sequence_item_scalar_is_not_held_as_key() {
        assert_eq!(
            kinds("- a\n- b\n"),
            vec![
                TokenKind::StreamStart,
                TokenKind::BlockSequenceStart,
                TokenKind::BlockEntry,
                TokenKind::Scalar {
                    value: "a".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::BlockEntry,
                TokenKind::Scalar {
                    value: "b".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::BlockEnd,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn anchored_key_is_framed_correctly() {
        // `&a foo: bar` — the anchor is part of the KEY node, so Key must come
        // BEFORE the anchor, not between the anchor and the scalar.
        assert_eq!(
            kinds("&a foo: bar\n"),
            vec![
                TokenKind::StreamStart,
                TokenKind::BlockMappingStart,
                TokenKind::Key,
                TokenKind::Anchor("a".to_string()),
                TokenKind::Scalar {
                    value: "foo".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::Value,
                TokenKind::Scalar {
                    value: "bar".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::BlockEnd,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn block_mapping_with_comments_and_blanks() {
        let input = "# header\na: 1\n\n# mid\nb: 2\n";
        assert_eq!(
            kinds(input),
            vec![
                TokenKind::StreamStart,
                TokenKind::BlockMappingStart,
                TokenKind::Key,
                TokenKind::Scalar {
                    value: "a".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::Value,
                TokenKind::Scalar {
                    value: "1".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::Key,
                TokenKind::Scalar {
                    value: "b".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::Value,
                TokenKind::Scalar {
                    value: "2".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::BlockEnd,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn mapping_value_is_flow_collection() {
        assert_eq!(
            kinds("nums: [1, 2]\n"),
            vec![
                TokenKind::StreamStart,
                TokenKind::BlockMappingStart,
                TokenKind::Key,
                TokenKind::Scalar {
                    value: "nums".to_string(),
                    style: ScalarStyle::Plain
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
                TokenKind::BlockEnd,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn document_marker_resets_block_indent() {
        assert_eq!(
            kinds("a: 1\n---\nb: 2\n"),
            vec![
                TokenKind::StreamStart,
                TokenKind::BlockMappingStart,
                TokenKind::Key,
                TokenKind::Scalar {
                    value: "a".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::Value,
                TokenKind::Scalar {
                    value: "1".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::BlockEnd,
                TokenKind::DocumentStart,
                TokenKind::BlockMappingStart,
                TokenKind::Key,
                TokenKind::Scalar {
                    value: "b".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::Value,
                TokenKind::Scalar {
                    value: "2".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::BlockEnd,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn realistic_block_document() {
        let input = "name: Ada\njobs:\n  - lang: rust\n    years: 3\n  - lang: yaml\n";
        assert_eq!(
            kinds(input),
            vec![
                TokenKind::StreamStart,
                TokenKind::BlockMappingStart,
                TokenKind::Key,
                TokenKind::Scalar {
                    value: "name".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::Value,
                TokenKind::Scalar {
                    value: "Ada".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::Key,
                TokenKind::Scalar {
                    value: "jobs".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::Value,
                TokenKind::BlockSequenceStart,
                TokenKind::BlockEntry,
                TokenKind::BlockMappingStart,
                TokenKind::Key,
                TokenKind::Scalar {
                    value: "lang".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::Value,
                TokenKind::Scalar {
                    value: "rust".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::Key,
                TokenKind::Scalar {
                    value: "years".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::Value,
                TokenKind::Scalar {
                    value: "3".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::BlockEnd,
                TokenKind::BlockEntry,
                TokenKind::BlockMappingStart,
                TokenKind::Key,
                TokenKind::Scalar {
                    value: "lang".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::Value,
                TokenKind::Scalar {
                    value: "yaml".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::BlockEnd,
                TokenKind::BlockEnd,
                TokenKind::BlockEnd,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn flow_still_works_after_block_changes() {
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
    fn alias_as_block_mapping_key() {
        // `*x: v` -> the alias is the key; the ':' is a value indicator.
        assert_eq!(
            kinds("*x: v\n"),
            vec![
                TokenKind::StreamStart,
                TokenKind::BlockMappingStart,
                TokenKind::Key,
                TokenKind::Alias("x".to_string()),
                TokenKind::Value,
                TokenKind::Scalar {
                    value: "v".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::BlockEnd,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn anchor_name_may_contain_non_indicator_colon() {
        // `&x:y` (no space after ':') -> ':' is NOT a value indicator, so it is
        // part of the anchor name.
        assert_eq!(
            kinds("&x:y 1\n"),
            vec![
                TokenKind::StreamStart,
                TokenKind::Anchor("x:y".to_string()),
                TokenKind::Scalar {
                    value: "1".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn block_scalar_as_mapping_value() {
        assert_eq!(
            kinds("key: |\n  a\n  b\n"),
            vec![
                TokenKind::StreamStart,
                TokenKind::BlockMappingStart,
                TokenKind::Key,
                TokenKind::Scalar { value: "key".to_string(), style: ScalarStyle::Plain },
                TokenKind::Value,
                TokenKind::Scalar { value: "a\nb\n".to_string(), style: ScalarStyle::Literal },
                TokenKind::BlockEnd,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn block_scalar_then_next_key() {
        assert_eq!(
            kinds("a: |\n  x\nb: 2\n"),
            vec![
                TokenKind::StreamStart,
                TokenKind::BlockMappingStart,
                TokenKind::Key,
                TokenKind::Scalar { value: "a".to_string(), style: ScalarStyle::Plain },
                TokenKind::Value,
                TokenKind::Scalar { value: "x\n".to_string(), style: ScalarStyle::Literal },
                TokenKind::Key,
                TokenKind::Scalar { value: "b".to_string(), style: ScalarStyle::Plain },
                TokenKind::Value,
                TokenKind::Scalar { value: "2".to_string(), style: ScalarStyle::Plain },
                TokenKind::BlockEnd,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn block_scalar_as_sequence_item() {
        assert_eq!(
            kinds("- |\n  a\n- b\n"),
            vec![
                TokenKind::StreamStart,
                TokenKind::BlockSequenceStart,
                TokenKind::BlockEntry,
                TokenKind::Scalar { value: "a\n".to_string(), style: ScalarStyle::Literal },
                TokenKind::BlockEntry,
                TokenKind::Scalar { value: "b".to_string(), style: ScalarStyle::Plain },
                TokenKind::BlockEnd,
                TokenKind::StreamEnd,
            ]
        );
    }
}
