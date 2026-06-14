//! The scanner (lexer): turns source text into a flat token stream.
//!
//! This layer is consumed by the parser (Plan 5); until then its public(crate)
//! surface is exercised only by tests, so dead-code is allowed module-wide.
#![allow(dead_code)]

mod reader;
mod token;

use std::collections::{HashMap, VecDeque};

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
    /// Tag handle -> prefix map for the current document (`%TAG` directives).
    tag_handles: HashMap<String, String>,
    /// True when the next directive or document marker should reset the handle
    /// map (directives apply to one document only).
    next_doc_needs_reset: bool,
    /// True while the cursor is in the leading whitespace of a line (before any
    /// token on that line). Used to detect tab indentation.
    at_line_start: bool,
    /// True when a tab has appeared in the current line's leading whitespace.
    tab_in_indent: bool,
    /// True once the current document has begun (a `---` or first content token).
    /// Directives are only valid before a document, after a `...` footer.
    doc_open: bool,
    /// True when at least one directive has been collected for the upcoming
    /// document; it must be followed by an explicit `---` document start.
    pending_directives: bool,
    /// True when a `%YAML` directive has been seen in the current directive block
    /// (at most one is allowed per document).
    yaml_directive_seen: bool,
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
            tag_handles: Self::default_tag_handles(),
            next_doc_needs_reset: true,
            at_line_start: true,
            tab_in_indent: false,
            doc_open: false,
            pending_directives: false,
            yaml_directive_seen: false,
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

        self.skip_to_next_token()?;
        self.stale_simple_key();

        // A `%` at column 1 in block context begins a directive line. Directives
        // are only valid before a document; inside a body a `...` footer is
        // required first.
        if self.flow_depth == 0
            && self.reader.peek() == Some('%')
            && self.reader.position().column == 1
        {
            if self.doc_open {
                let pos = self.reader.position();
                return Err(Error::new(
                    ErrorKind::Scan,
                    "directives must be preceded by a '...' document end",
                )
                .with_span(Span::new(pos, pos)));
            }
            self.scan_directive()?;
            return Ok(());
        }

        // Tabs may not be used for indentation in block context.
        if self.flow_depth == 0 && self.tab_in_indent && self.reader.peek().is_some() {
            let pos = self.reader.position();
            return Err(
                Error::new(ErrorKind::Scan, "tabs cannot be used for indentation")
                    .with_span(Span::new(pos, pos)),
            );
        }

        // Block context: process indentation at the start of a line.
        if self.flow_depth == 0 {
            self.unroll_indent(Self::col0(self.reader.position()));
        }

        let start = self.reader.position();
        match self.reader.peek() {
            None => {
                if self.pending_directives {
                    return Err(Error::new(
                        ErrorKind::Scan,
                        "directives must be followed by a '---' document start",
                    )
                    .with_span(Span::new(start, start)));
                }
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
                // The first content token of a document opens it. Directives
                // require an explicit `---`, so bare content with pending
                // directives is invalid.
                if self.flow_depth == 0
                    && !self.doc_open
                    && !matches!(
                        token.kind,
                        TokenKind::DocumentStart | TokenKind::DocumentEnd
                    )
                {
                    if self.pending_directives {
                        return Err(Error::new(
                            ErrorKind::Scan,
                            "directives must be followed by a '---' document start",
                        )
                        .with_span(Span::new(start, start)));
                    }
                    self.doc_open = true;
                }
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
    fn skip_to_next_token(&mut self) -> Result<()> {
        // A comment is only valid at line start or after separating whitespace.
        // Column 1 means the cursor sits at the start of a line (robust even
        // after multi-line scanners that leave `at_line_start` stale).
        let mut separated = self.reader.position().column == 1;
        loop {
            match self.reader.peek() {
                Some(' ') => {
                    separated = true;
                    self.reader.advance();
                }
                Some('\t') => {
                    if self.flow_depth == 0 && self.at_line_start {
                        self.tab_in_indent = true;
                    }
                    separated = true;
                    self.reader.advance();
                }
                Some('\n') | Some('\r') => {
                    self.reader.advance();
                    if self.flow_depth == 0 {
                        self.simple_key_allowed = true;
                    }
                    self.at_line_start = true;
                    self.tab_in_indent = false;
                    separated = true;
                }
                Some('#') => {
                    if !separated {
                        let pos = self.reader.position();
                        return Err(Error::new(
                            ErrorKind::Scan,
                            "a comment must be preceded by whitespace",
                        )
                        .with_span(Span::new(pos, pos)));
                    }
                    while let Some(c) = self.reader.peek() {
                        if c == '\n' || c == '\r' {
                            break;
                        }
                        self.reader.advance();
                    }
                }
                _ => {
                    self.at_line_start = false;
                    break;
                }
            }
        }
        Ok(())
    }

    /// Scans a content token starting at character `c`.
    fn scan_content(&mut self, c: char, start: Position) -> Result<Token> {
        match c {
            '-' if self.marker_ahead("---") => {
                self.unroll_indent(-1);
                self.remove_simple_key();
                self.simple_key_allowed = true;
                if self.next_doc_needs_reset {
                    self.reset_tag_handles();
                }
                self.next_doc_needs_reset = true;
                // `---` consumes any pending directives and opens the document.
                self.pending_directives = false;
                self.yaml_directive_seen = false;
                self.doc_open = true;
                Ok(self.scan_marker(TokenKind::DocumentStart, start))
            }
            '.' if self.marker_ahead("...") => {
                if self.pending_directives {
                    return Err(Error::new(
                        ErrorKind::Scan,
                        "directives must be followed by a '---' document start",
                    )
                    .with_span(Span::new(start, start)));
                }
                self.unroll_indent(-1);
                self.remove_simple_key();
                self.simple_key_allowed = true;
                self.reset_tag_handles();
                self.next_doc_needs_reset = true;
                // `...` closes the document; the next directive block is fresh.
                self.doc_open = false;
                self.yaml_directive_seen = false;
                let token = self.scan_marker(TokenKind::DocumentEnd, start);
                // Only whitespace or a comment may follow `...` on its line.
                let trailing = self.reader.peek_nth(self.reader.count_leading_spaces());
                match trailing {
                    None | Some('\n') | Some('\r') | Some('#') => Ok(token),
                    Some(_) => Err(Error::new(
                        ErrorKind::Scan,
                        "content is not allowed after a '...' document end marker",
                    )
                    .with_span(Span::new(start, self.reader.position()))),
                }
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
            '?' if self.flow_depth == 0 && self.indicator_terminator_next() => {
                self.fetch_block_key(start)
            }
            '?' if self.indicator_terminator_next() => Ok(self.single_char(TokenKind::Key, start)),
            '\'' => self.scan_single_quoted(start),
            '"' => self.scan_double_quoted(start),
            '&' => self.scan_anchor_or_alias(start, true),
            '*' => self.scan_anchor_or_alias(start, false),
            '!' => self.scan_tag(start),
            '-' if self.flow_depth == 0 && self.block_entry_next() => self.fetch_block_entry(start),
            // A lone `-` before a flow indicator is not a valid plain scalar.
            '-' if self.flow_depth > 0
                && matches!(self.reader.peek_nth(1), Some(',') | Some(']') | Some('}')) =>
            {
                Err(Error::new(
                    ErrorKind::Scan,
                    "'-' is not a valid plain scalar in a flow collection",
                )
                .with_span(Span::new(start, self.reader.position())))
            }
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
            TokenKind::Scalar {
                style: ScalarStyle::Literal,
                ..
            } | TokenKind::Scalar {
                style: ScalarStyle::Folded,
                ..
            }
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

    /// Scans a plain scalar, folding multi-line continuations per YAML 1.2.
    /// A single line break folds to a space; blank lines fold to newlines; each
    /// line's leading/trailing whitespace is stripped. A continuation line must
    /// be more indented than the current block indent (block context) or simply
    /// present (flow context), and must not begin a document marker, comment, or
    /// a new construct.
    fn scan_plain(&mut self, start: Position) -> Result<Token> {
        let mut value = String::new();
        // Position just after the last committed content char (span end).
        let mut content_end = start;
        // Intra-line whitespace pending before the next content char.
        let mut whitespaces = String::new();
        // Pending fold for a line break between content runs.
        let mut leading_break = false;
        let mut trailing_breaks = 0usize;

        'scan: loop {
            // Consume content on the current line.
            loop {
                match self.reader.peek() {
                    None | Some('\n') | Some('\r') => break,
                    Some(',') | Some('[') | Some(']') | Some('{') | Some('}') => break 'scan,
                    Some(':') if self.indicator_terminator_next() => break 'scan,
                    Some(' ') | Some('\t') => {
                        // A space immediately before '#' starts a comment.
                        if self.reader.peek() == Some(' ') && self.reader.peek_nth(1) == Some('#') {
                            break 'scan;
                        }
                        whitespaces.push(self.reader.advance().unwrap());
                    }
                    Some(c) => {
                        if leading_break {
                            if trailing_breaks == 0 {
                                value.push(' ');
                            } else {
                                for _ in 0..trailing_breaks {
                                    value.push('\n');
                                }
                            }
                            leading_break = false;
                            trailing_breaks = 0;
                        } else {
                            value.push_str(&whitespaces);
                        }
                        whitespaces.clear();
                        self.reader.advance();
                        value.push(c);
                        content_end = self.reader.position();
                    }
                }
            }
            // Only a line break can start a continuation.
            if !matches!(self.reader.peek(), Some('\n') | Some('\r')) {
                break;
            }
            if !self.plain_continues(&mut leading_break, &mut trailing_breaks) {
                break;
            }
            // Trailing whitespace of the prior line is dropped on a fold.
            whitespaces.clear();
        }

        debug_assert!(!value.is_empty(), "scan_plain produced an empty scalar");
        Ok(Token::new(
            TokenKind::Scalar {
                value,
                style: ScalarStyle::Plain,
            },
            Span::new(start, content_end),
        ))
    }

    /// At a line break inside a plain scalar, decides whether the scalar
    /// continues. If so, consumes the break(s) and the continuation line's
    /// leading whitespace, sets the fold state, and returns true. Otherwise
    /// restores the reader to the break and returns false.
    fn plain_continues(&mut self, leading_break: &mut bool, trailing_breaks: &mut usize) -> bool {
        let mark = self.reader.mark();
        self.consume_line_break();
        let mut extra = 0usize;
        // Consume blank lines (whitespace-only), counting them as fold breaks.
        loop {
            while matches!(self.reader.peek(), Some(' ') | Some('\t')) {
                self.reader.advance();
            }
            match self.reader.peek() {
                Some('\n') | Some('\r') => {
                    self.consume_line_break();
                    extra += 1;
                }
                _ => break,
            }
        }
        let col = self.reader.position().column as i64 - 1;
        let continues = match self.reader.peek() {
            None => false,
            Some('#') => false,
            _ if self.marker_ahead("---") || self.marker_ahead("...") => false,
            Some(',') | Some('[') | Some(']') | Some('{') | Some('}') if self.flow_depth > 0 => {
                false
            }
            Some(':') if self.indicator_terminator_next() => false,
            Some('-') if self.flow_depth == 0 && self.block_entry_next() => false,
            Some('?') if self.indicator_terminator_next() => false,
            _ => self.flow_depth > 0 || col > self.indent,
        };
        if continues {
            *leading_break = true;
            *trailing_breaks = extra;
            true
        } else {
            self.reader.reset(mark);
            false
        }
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

    /// Resets the tag handle map to its document defaults.
    fn reset_tag_handles(&mut self) {
        self.tag_handles = Self::default_tag_handles();
    }

    /// Scans a directive line (`%YAML ...` or `%TAG ...`) at column 1, updating
    /// the handle map. Directives apply to the upcoming document only, so the
    /// first directive of a new document block resets the map. Produces no token.
    fn scan_directive(&mut self) -> Result<()> {
        let start = self.reader.position();
        if self.next_doc_needs_reset {
            self.reset_tag_handles();
            self.next_doc_needs_reset = false;
        }
        self.reader.advance(); // '%'
        let name = self.take_directive_word();
        if name == "TAG" {
            self.skip_inline_blanks();
            let handle = self.take_directive_word();
            self.skip_inline_blanks();
            let prefix = self.take_directive_word();
            if !handle.is_empty() && !prefix.is_empty() {
                self.tag_handles.insert(handle, prefix);
            }
        } else if name == "YAML" {
            if self.yaml_directive_seen {
                return Err(Error::new(
                    ErrorKind::Scan,
                    "a document may have at most one %YAML directive",
                )
                .with_span(Span::new(start, self.reader.position())));
            }
            self.skip_inline_blanks();
            let version = self.take_directive_word();
            if !Self::is_yaml_version(&version) {
                return Err(Error::new(
                    ErrorKind::Scan,
                    format!("invalid %YAML version {version:?}"),
                )
                .with_span(Span::new(start, self.reader.position())));
            }
            // Only a trailing comment may follow the version.
            self.skip_inline_blanks();
            match self.reader.peek() {
                None | Some('\n') | Some('\r') | Some('#') => {}
                Some(_) => {
                    return Err(Error::new(
                        ErrorKind::Scan,
                        "extra content after the %YAML directive version",
                    )
                    .with_span(Span::new(start, self.reader.position())))
                }
            }
            self.yaml_directive_seen = true;
        }
        // Any unknown directive is accepted and ignored.
        // Consume the rest of the line (the line break is left for the caller).
        while !matches!(self.reader.peek(), None | Some('\n') | Some('\r')) {
            self.reader.advance();
        }
        self.pending_directives = true;
        Ok(())
    }

    /// True if `v` is a `major.minor` YAML version (both parts ASCII digits).
    fn is_yaml_version(v: &str) -> bool {
        match v.split_once('.') {
            Some((major, minor)) => {
                !major.is_empty()
                    && !minor.is_empty()
                    && major.bytes().all(|b| b.is_ascii_digit())
                    && minor.bytes().all(|b| b.is_ascii_digit())
            }
            None => false,
        }
    }

    /// Consumes a run of non-whitespace characters (a directive token).
    fn take_directive_word(&mut self) -> String {
        let mut word = String::new();
        while let Some(c) = self.reader.peek() {
            if c.is_whitespace() {
                break;
            }
            self.reader.advance();
            word.push(c);
        }
        word
    }

    /// Consumes inline spaces and tabs (not line breaks).
    fn skip_inline_blanks(&mut self) {
        while matches!(self.reader.peek(), Some(' ') | Some('\t')) {
            self.reader.advance();
        }
    }

    /// The default tag handle map: primary `!` (local) and secondary `!!`
    /// (the YAML core tag namespace).
    fn default_tag_handles() -> HashMap<String, String> {
        let mut map = HashMap::new();
        map.insert("!".to_string(), "!".to_string());
        map.insert("!!".to_string(), "tag:yaml.org,2002:".to_string());
        map
    }

    /// Scans and resolves a tag against the current handle map. Produces the
    /// fully resolved tag: a verbatim URI, a core/custom URI from a handle, a
    /// local `!suffix`, or the non-specific `!`.
    fn scan_tag(&mut self, start: Position) -> Result<Token> {
        self.reader.advance(); // leading '!'

        // Verbatim tag: !<uri>
        if self.reader.peek() == Some('<') {
            self.reader.advance(); // '<'
            let mut uri = String::new();
            loop {
                match self.reader.peek() {
                    Some('>') => {
                        self.reader.advance();
                        break;
                    }
                    Some(c) if !c.is_whitespace() => {
                        self.reader.advance();
                        uri.push(c);
                    }
                    _ => {
                        return Err(Error::new(ErrorKind::Scan, "unterminated verbatim tag")
                            .with_span(Span::new(start, self.reader.position())))
                    }
                }
            }
            if uri.is_empty() {
                return Err(Error::new(ErrorKind::Scan, "empty verbatim tag")
                    .with_span(Span::new(start, self.reader.position())));
            }
            return Ok(Token::new(
                TokenKind::Tag(uri),
                Span::new(start, self.reader.position()),
            ));
        }

        // Shorthand. The first segment is empty for `!!` and for the bare `!`.
        let first = self.take_tag_word();
        let resolved = if self.reader.peek() == Some('!') {
            // Named or secondary handle: `!<first>!<suffix>`.
            self.reader.advance(); // second '!'
            let handle = format!("!{first}!");
            let suffix = self.take_tag_word();
            match self.tag_handles.get(&handle) {
                Some(prefix) => format!("{prefix}{suffix}"),
                None => {
                    return Err(Error::new(
                        ErrorKind::Scan,
                        format!("undefined tag handle '{handle}'"),
                    )
                    .with_span(Span::new(start, self.reader.position())))
                }
            }
        } else {
            // Primary handle `!` with suffix `first` (empty => non-specific `!`).
            let prefix = self
                .tag_handles
                .get("!")
                .cloned()
                .unwrap_or_else(|| "!".to_string());
            format!("{prefix}{first}")
        };

        Ok(Token::new(
            TokenKind::Tag(resolved),
            Span::new(start, self.reader.position()),
        ))
    }

    /// Consumes a run of tag-shorthand characters: non-whitespace, non-flow-
    /// indicator, stopping at `!` (a handle boundary) and at a `:` value
    /// indicator.
    fn take_tag_word(&mut self) -> String {
        let mut word = String::new();
        while let Some(c) = self.reader.peek() {
            if c.is_whitespace() || matches!(c, ',' | '[' | ']' | '{' | '}' | '!') {
                break;
            }
            if c == ':' && self.indicator_terminator_next() {
                break;
            }
            self.reader.advance();
            word.push(c);
        }
        word
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
                Some('\n') | Some('\r') => {
                    let trimmed = value.trim_end_matches([' ', '\t']).len();
                    value.truncate(trimmed);
                    let folded = self.scan_flow_folded_breaks();
                    if self.at_document_marker() {
                        return Err(Error::new(
                            ErrorKind::Scan,
                            "a document marker may not appear inside a quoted scalar",
                        )
                        .with_span(Span::new(start, self.reader.position())));
                    }
                    value.push_str(&folded);
                }
                Some(c) => {
                    self.reader.advance();
                    value.push(c);
                }
            }
        }
    }

    /// True when the cursor sits at the start of a line that begins a `---` or
    /// `...` document marker.
    fn at_document_marker(&self) -> bool {
        self.reader.position().column == 1 && (self.marker_ahead("---") || self.marker_ahead("..."))
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
                    // Escaped line break = line continuation (no inserted space).
                    if matches!(self.reader.peek_nth(1), Some('\n') | Some('\r')) {
                        self.reader.advance(); // backslash
                        self.consume_line_break();
                        while matches!(self.reader.peek(), Some(' ') | Some('\t')) {
                            self.reader.advance();
                        }
                    } else {
                        self.reader.advance(); // backslash
                        let ch = self.scan_escape(start)?;
                        value.push(ch);
                    }
                }
                Some('\n') | Some('\r') => {
                    let trimmed = value.trim_end_matches([' ', '\t']).len();
                    value.truncate(trimmed);
                    let folded = self.scan_flow_folded_breaks();
                    if self.at_document_marker() {
                        return Err(Error::new(
                            ErrorKind::Scan,
                            "a document marker may not appear inside a quoted scalar",
                        )
                        .with_span(Span::new(start, self.reader.position())));
                    }
                    value.push_str(&folded);
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

    /// At a line break inside a flow (quoted) scalar, consumes the break, any
    /// following blank lines, and the leading whitespace of the continuation.
    /// Returns the folded text: a single break → one space; N blank lines → N
    /// newlines. The caller must trim trailing whitespace before the break.
    fn scan_flow_folded_breaks(&mut self) -> String {
        self.consume_line_break();
        let mut breaks = 0usize;
        loop {
            while matches!(self.reader.peek(), Some(' ') | Some('\t')) {
                self.reader.advance();
            }
            match self.reader.peek() {
                Some('\n') | Some('\r') => {
                    self.consume_line_break();
                    breaks += 1;
                }
                _ => break,
            }
        }
        if breaks == 0 {
            " ".to_string()
        } else {
            "\n".repeat(breaks)
        }
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
        // A block sequence entry may only begin where a node could start a new
        // block (line start, or after another `-`). A `-` mid-line after a `:`
        // value or a node property (`key: - x`, `&a - x`) is invalid.
        if !self.simple_key_allowed {
            return Err(Error::new(
                ErrorKind::Scan,
                "block sequence entries are not allowed here",
            )
            .with_span(Span::new(start, start)));
        }
        let col = Self::col0(start);
        self.roll_indent(col, TokenKind::BlockSequenceStart, start, None);
        self.reader.advance(); // consume '-'
        self.simple_key_allowed = true;
        Ok(Token::new(
            TokenKind::BlockEntry,
            Span::new(start, self.reader.position()),
        ))
    }

    /// Handles a `?` explicit block mapping key: opens a mapping if needed and
    /// emits `Key`. The following key node is not an implicit simple key, so
    /// `simple_key_allowed` is cleared until the next line break.
    fn fetch_block_key(&mut self, start: Position) -> Result<Token> {
        let col = Self::col0(start);
        self.roll_indent(col, TokenKind::BlockMappingStart, start, None);
        self.remove_simple_key();
        self.reader.advance(); // consume '?'
        self.simple_key_allowed = false;
        Ok(Token::new(
            TokenKind::Key,
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
    fn scan_block_scalar_header(&mut self, start: Position) -> Result<(Chomping, Option<usize>)> {
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
        let mut had_space = false;
        while matches!(self.reader.peek(), Some(' ') | Some('\t')) {
            had_space = true;
            self.reader.advance();
        }
        // After the indicators, only a comment (whitespace-preceded) or the end
        // of the line may follow. A leftover digit (`|0`, `|10`) or `#` without a
        // preceding space (`>#`) is an invalid header.
        match self.reader.peek() {
            None | Some('\n') | Some('\r') => {}
            Some('#') if had_space => {
                while let Some(c) = self.reader.peek() {
                    if c == '\n' || c == '\r' {
                        break;
                    }
                    self.reader.advance();
                }
            }
            Some(_) => {
                return Err(Error::new(ErrorKind::Scan, "invalid block scalar header")
                    .with_span(Span::new(start, self.reader.position())))
            }
        }
        self.consume_line_break();
        Ok((chomp, indent))
    }

    /// Scans a block scalar (`literal` = `|`, else `>`).
    fn scan_block_scalar(&mut self, literal: bool, start: Position) -> Result<Token> {
        self.reader.advance(); // '|' or '>'
        let (chomp, explicit_indent) = self.scan_block_scalar_header(start)?;
        let parent = self.indent; // 0-based; -1 at root

        let mut content_indent: Option<usize> = explicit_indent.map(|n| {
            // `content_indent` is the number of leading spaces to strip. The
            // indentation indicator `n` (1-9) is relative to the parent block.
            let base = if parent < 0 { 0 } else { (parent + 1) as usize };
            base + n
        });

        let mut lines: Vec<(String, bool)> = Vec::new();
        loop {
            let sp = self.reader.count_leading_spaces();
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

    /// Joins folded block-scalar lines per YAML 1.2 §8.1.3. Between two
    /// consecutive non-empty lines separated by `k` blank lines, the separator
    /// is `\n` repeated `k+1` times if either line is more-indented; otherwise a
    /// single space when `k == 0`, or `\n` repeated `k` times when `k > 0`.
    /// Leading blank lines and the final break are emitted explicitly, then the
    /// chomping indicator is applied.
    fn assemble_folded(lines: &[(String, bool)], chomp: Chomping) -> String {
        let mut value = String::new();
        let mut pending_blanks = 0usize;
        let mut have_prev = false;
        let mut prev_more = false;
        for (text, more) in lines {
            if text.is_empty() {
                pending_blanks += 1;
                continue;
            }
            if !have_prev {
                // Leading blank lines become leading newlines.
                for _ in 0..pending_blanks {
                    value.push('\n');
                }
                value.push_str(text);
            } else if prev_more || *more {
                // A more-indented line on either side keeps every break literal.
                for _ in 0..pending_blanks + 1 {
                    value.push('\n');
                }
                value.push_str(text);
            } else if pending_blanks == 0 {
                // Single break between normal lines folds to a space.
                value.push(' ');
                value.push_str(text);
            } else {
                // Blank lines between normal lines: one break folds away.
                for _ in 0..pending_blanks {
                    value.push('\n');
                }
                value.push_str(text);
            }
            have_prev = true;
            prev_more = *more;
            pending_blanks = 0;
        }
        if have_prev {
            // Trailing break for the last content line, plus any trailing blanks.
            for _ in 0..pending_blanks + 1 {
                value.push('\n');
            }
        } else {
            // Only blank lines (or nothing).
            for _ in 0..pending_blanks {
                value.push('\n');
            }
        }
        Self::apply_chomping(value, chomp)
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
    fn content_after_document_end_marker_is_error() {
        assert_eq!(
            tokenize("---\nx\n... junk\n", Limits::default())
                .unwrap_err()
                .kind(),
            crate::error::ErrorKind::Scan
        );
        // A trailing comment after `...` is fine.
        assert!(tokenize("---\nx\n... # bye\n", Limits::default()).is_ok());
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
    fn invalid_block_scalar_headers_error() {
        for input in ["|0\n x\n", "|10\n x\n", ">#c\n  x\n"] {
            assert_eq!(
                tokenize(input, Limits::default()).unwrap_err().kind(),
                crate::error::ErrorKind::Scan,
                "expected scan error for {input:?}"
            );
        }
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
                TokenKind::Tag("tag:yaml.org,2002:str".to_string()),
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn verbatim_tag_uses_uri_as_is() {
        assert_eq!(
            kinds("!<tag:example.com,2000:x> v"),
            vec![
                TokenKind::StreamStart,
                TokenKind::Tag("tag:example.com,2000:x".to_string()),
                TokenKind::Scalar {
                    value: "v".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn non_specific_tag_resolves_to_bang() {
        assert_eq!(
            kinds("! v"),
            vec![
                TokenKind::StreamStart,
                TokenKind::Tag("!".to_string()),
                TokenKind::Scalar {
                    value: "v".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn undefined_named_handle_is_scan_error() {
        let err = tokenize("!e!foo v", Limits::default()).unwrap_err();
        assert_eq!(err.kind(), crate::error::ErrorKind::Scan);
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
    fn block_entry_mid_line_after_value_or_property_is_error() {
        // A `-` cannot start a block sequence after a `:` value or an anchor on
        // the same line; it must begin where a key could (line start / after `-`).
        for input in ["key: - a\n     - b\n", "&anchor - x\n"] {
            assert_eq!(
                tokenize(input, Limits::default()).unwrap_err().kind(),
                crate::error::ErrorKind::Scan,
                "expected scan error for {input:?}"
            );
        }
        // Compact nested sequences and indentless sequences stay valid.
        assert!(tokenize("- - a\n", Limits::default()).is_ok());
        assert!(tokenize("key:\n- a\n", Limits::default()).is_ok());
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
        // With multi-line plain folding, `hello\nworld` at root (indent -1) folds
        // into a single scalar. The key assertion still holds: no Key token is
        // produced — the two words are content of one scalar, not a key-value pair.
        assert_eq!(
            kinds("hello\nworld\n"),
            vec![
                TokenKind::StreamStart,
                TokenKind::Scalar {
                    value: "hello world".to_string(),
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
                TokenKind::Scalar {
                    value: "key".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::Value,
                TokenKind::Scalar {
                    value: "a\nb\n".to_string(),
                    style: ScalarStyle::Literal
                },
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
                TokenKind::Scalar {
                    value: "a".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::Value,
                TokenKind::Scalar {
                    value: "x\n".to_string(),
                    style: ScalarStyle::Literal
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
    fn block_scalar_as_sequence_item() {
        assert_eq!(
            kinds("- |\n  a\n- b\n"),
            vec![
                TokenKind::StreamStart,
                TokenKind::BlockSequenceStart,
                TokenKind::BlockEntry,
                TokenKind::Scalar {
                    value: "a\n".to_string(),
                    style: ScalarStyle::Literal
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
    fn folded_block_scalar_basic() {
        assert_eq!(
            one_scalar(">\n  a\n  b\n"),
            ("a b\n".to_string(), ScalarStyle::Folded)
        );
    }

    #[test]
    fn folded_block_scalar_blank_line_is_newline() {
        assert_eq!(
            one_scalar(">\n  a\n\n  b\n"),
            ("a\nb\n".to_string(), ScalarStyle::Folded)
        );
    }

    #[test]
    fn folded_block_scalar_more_indented_kept_literal() {
        assert_eq!(
            one_scalar(">\n  a\n   b\n  c\n"),
            ("a\n b\nc\n".to_string(), ScalarStyle::Folded)
        );
    }

    #[test]
    fn folded_block_scalar_strip() {
        assert_eq!(
            one_scalar(">-\n  a\n  b\n"),
            ("a b".to_string(), ScalarStyle::Folded)
        );
    }

    #[test]
    fn folded_blank_after_more_indented_kept() {
        // The more-indented line keeps a literal break AND the blank adds one.
        assert_eq!(
            one_scalar(">\n  a\n   b\n\n  c\n"),
            ("a\n b\n\nc\n".to_string(), ScalarStyle::Folded)
        );
    }

    #[test]
    fn folded_two_blank_lines() {
        assert_eq!(
            one_scalar(">\n  a\n\n\n  b\n"),
            ("a\n\nb\n".to_string(), ScalarStyle::Folded)
        );
    }

    #[test]
    fn folded_leading_blank_line() {
        assert_eq!(
            one_scalar(">\n\n  a\n"),
            ("\na\n".to_string(), ScalarStyle::Folded)
        );
    }

    #[test]
    fn folded_trailing_blanks_keep() {
        assert_eq!(
            one_scalar(">+\n  a\n\n\n"),
            ("a\n\n\n".to_string(), ScalarStyle::Folded)
        );
    }

    #[test]
    fn double_quoted_multiline_folds_to_space() {
        assert_eq!(
            one_scalar("\"a\nb\""),
            ("a b".to_string(), ScalarStyle::DoubleQuoted)
        );
    }

    #[test]
    fn document_marker_inside_quoted_scalar_is_error() {
        assert_eq!(
            tokenize("\"a\n---\nb\"", Limits::default())
                .unwrap_err()
                .kind(),
            crate::error::ErrorKind::Scan
        );
        assert_eq!(
            tokenize("'a\n...\nb'", Limits::default())
                .unwrap_err()
                .kind(),
            crate::error::ErrorKind::Scan
        );
    }

    #[test]
    fn indented_dots_inside_quoted_scalar_are_content() {
        // An indented `...` is folded content, not a document marker.
        assert_eq!(
            one_scalar("\"a\n  ... b\""),
            ("a ... b".to_string(), ScalarStyle::DoubleQuoted)
        );
    }

    #[test]
    fn double_quoted_multiline_trims_surrounding_whitespace() {
        assert_eq!(
            one_scalar("\"a   \n   b\""),
            ("a b".to_string(), ScalarStyle::DoubleQuoted)
        );
    }

    #[test]
    fn double_quoted_blank_line_becomes_newline() {
        assert_eq!(
            one_scalar("\"a\n\nb\""),
            ("a\nb".to_string(), ScalarStyle::DoubleQuoted)
        );
    }

    #[test]
    fn double_quoted_escaped_line_continuation() {
        assert_eq!(
            one_scalar("\"a\\\n   b\""),
            ("ab".to_string(), ScalarStyle::DoubleQuoted)
        );
    }

    #[test]
    fn single_quoted_multiline_folds_to_space() {
        assert_eq!(
            one_scalar("'a\nb'"),
            ("a b".to_string(), ScalarStyle::SingleQuoted)
        );
    }

    #[test]
    fn single_quoted_multiline_trims_whitespace() {
        assert_eq!(
            one_scalar("'a  \n  b'"),
            ("a b".to_string(), ScalarStyle::SingleQuoted)
        );
    }

    #[test]
    fn single_quoted_blank_line_becomes_newline() {
        assert_eq!(
            one_scalar("'a\n\nb'"),
            ("a\nb".to_string(), ScalarStyle::SingleQuoted)
        );
    }

    #[test]
    fn single_quoted_doubled_quote_still_works_multiline() {
        assert_eq!(
            one_scalar("'it''s\nok'"),
            ("it's ok".to_string(), ScalarStyle::SingleQuoted)
        );
    }

    #[test]
    fn block_scalars_in_a_mapping() {
        let input = "literal: |\n  a\n  b\nfolded: >\n  c\n  d\n";
        assert_eq!(
            kinds(input),
            vec![
                TokenKind::StreamStart,
                TokenKind::BlockMappingStart,
                TokenKind::Key,
                TokenKind::Scalar {
                    value: "literal".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::Value,
                TokenKind::Scalar {
                    value: "a\nb\n".to_string(),
                    style: ScalarStyle::Literal
                },
                TokenKind::Key,
                TokenKind::Scalar {
                    value: "folded".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::Value,
                TokenKind::Scalar {
                    value: "c d\n".to_string(),
                    style: ScalarStyle::Folded
                },
                TokenKind::BlockEnd,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn multiline_quoted_value_in_mapping() {
        assert_eq!(
            kinds("msg: \"hello\n  world\"\n"),
            vec![
                TokenKind::StreamStart,
                TokenKind::BlockMappingStart,
                TokenKind::Key,
                TokenKind::Scalar {
                    value: "msg".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::Value,
                TokenKind::Scalar {
                    value: "hello world".to_string(),
                    style: ScalarStyle::DoubleQuoted
                },
                TokenKind::BlockEnd,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn flow_and_block_scalars_unaffected() {
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
    fn literal_block_scalar_no_trailing_newline_at_eof() {
        // last line has no trailing newline; clip still yields one.
        assert_eq!(
            one_scalar("|\n  a"),
            ("a\n".to_string(), ScalarStyle::Literal)
        );
    }

    #[test]
    fn literal_block_scalar_crlf_lines() {
        // CRLF line endings must not leave stray '\r' in the value.
        assert_eq!(
            one_scalar("|\r\n  a\r\n  b\r\n"),
            ("a\nb\n".to_string(), ScalarStyle::Literal)
        );
    }

    #[test]
    fn block_scalar_huge_leading_space_run_is_linear() {
        // 100k leading spaces — must complete quickly (O(n), not O(n^2)).
        let spaces = " ".repeat(100_000);
        let input = format!("|\n{spaces}x\n");
        let (value, style) = one_scalar(&input);
        assert_eq!(style, ScalarStyle::Literal);
        // Auto-detected content indent is 100_000, so "x" is the content.
        assert_eq!(value, "x\n");
    }

    #[test]
    fn plain_folds_single_break_to_space() {
        assert_eq!(
            scalars("one\ntwo\n"),
            vec![("one two".to_string(), ScalarStyle::Plain)]
        );
    }

    #[test]
    fn plain_folds_blank_line_to_newline() {
        assert_eq!(
            scalars("a\n\nb\n"),
            vec![("a\nb".to_string(), ScalarStyle::Plain)]
        );
    }

    #[test]
    fn plain_continuation_in_mapping_value() {
        assert_eq!(
            kinds("key: one\n  two\n"),
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
                    value: "one two".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::BlockEnd,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn plain_continuation_stops_at_dedent() {
        // `next` at column 0 is not more indented than the mapping (indent 0),
        // so the value scalar ends and `next: x` is a sibling entry.
        assert_eq!(
            kinds("key: one\n  two\nnext: x\n"),
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
                    value: "one two".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::Key,
                TokenKind::Scalar {
                    value: "next".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::Value,
                TokenKind::Scalar {
                    value: "x".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::BlockEnd,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn plain_continuation_stops_at_document_marker() {
        assert_eq!(
            scalars("one\n...\n"),
            vec![("one".to_string(), ScalarStyle::Plain)]
        );
    }

    #[test]
    fn plain_continuation_stops_at_comment_line() {
        // The comment line ends the first scalar; `b` is a separate scalar.
        assert_eq!(
            scalars("a\n# c\nb\n"),
            vec![
                ("a".to_string(), ScalarStyle::Plain),
                ("b".to_string(), ScalarStyle::Plain),
            ]
        );
    }

    #[test]
    fn plain_folds_in_flow_sequence() {
        assert_eq!(
            kinds("[one\n two]"),
            vec![
                TokenKind::StreamStart,
                TokenKind::FlowSequenceStart,
                TokenKind::Scalar {
                    value: "one two".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::FlowSequenceEnd,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn plain_single_line_unchanged() {
        // Regression: single-line plain scalars keep their internal spaces.
        assert_eq!(
            scalars("a b c\n"),
            vec![("a b c".to_string(), ScalarStyle::Plain)]
        );
    }

    #[test]
    fn plain_root_sequence_after_scalar_is_not_folded() {
        // `- b` begins a block entry, so it does not continue the scalar `a`.
        assert_eq!(
            kinds("a\n- b\n"),
            vec![
                TokenKind::StreamStart,
                TokenKind::Scalar {
                    value: "a".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::BlockSequenceStart,
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
    fn tag_directive_defines_named_handle() {
        assert_eq!(
            kinds("%TAG !e! tag:example.com,2000:\n--- !e!foo v\n"),
            vec![
                TokenKind::StreamStart,
                TokenKind::DocumentStart,
                TokenKind::Tag("tag:example.com,2000:foo".to_string()),
                TokenKind::Scalar {
                    value: "v".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn yaml_directive_is_accepted_and_ignored() {
        assert_eq!(
            kinds("%YAML 1.2\n--- x\n"),
            vec![
                TokenKind::StreamStart,
                TokenKind::DocumentStart,
                TokenKind::Scalar {
                    value: "x".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn secondary_handle_can_be_overridden() {
        assert_eq!(
            kinds("%TAG !! tag:example.com,2000:\n--- !!foo v\n"),
            vec![
                TokenKind::StreamStart,
                TokenKind::DocumentStart,
                TokenKind::Tag("tag:example.com,2000:foo".to_string()),
                TokenKind::Scalar {
                    value: "v".to_string(),
                    style: ScalarStyle::Plain
                },
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn handles_reset_between_documents() {
        // `!e!` is defined for the first document only; the second document must
        // not see it, so its use is a scan error.
        let err = tokenize(
            "%TAG !e! tag:example.com,2000:\n--- !e!a\n--- !e!b\n",
            Limits::default(),
        )
        .unwrap_err();
        assert_eq!(err.kind(), crate::error::ErrorKind::Scan);
    }

    #[test]
    fn leading_tab_indentation_is_rejected() {
        let err = tokenize("\tkey: value", Limits::default()).unwrap_err();
        assert_eq!(err.kind(), crate::error::ErrorKind::Scan);
    }

    #[test]
    fn mixed_space_then_tab_indentation_is_rejected() {
        let err = tokenize("  \tkey: value", Limits::default()).unwrap_err();
        assert_eq!(err.kind(), crate::error::ErrorKind::Scan);
    }

    #[test]
    fn nested_tab_indentation_is_rejected() {
        let err = tokenize("key:\n\tnested: v\n", Limits::default()).unwrap_err();
        assert_eq!(err.kind(), crate::error::ErrorKind::Scan);
    }

    #[test]
    fn tab_as_separation_is_allowed() {
        // A tab after the `:` token is separation, not indentation.
        assert_eq!(
            kinds("key:\tvalue\n"),
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
    fn tab_in_flow_context_is_allowed() {
        assert!(tokenize("[\n\ta, b]", Limits::default()).is_ok());
    }

    #[test]
    fn tab_on_blank_line_is_allowed() {
        assert!(tokenize("a: 1\n\t\nb: 2\n", Limits::default()).is_ok());
    }

    #[test]
    fn tab_before_comment_is_allowed() {
        assert!(tokenize("a: 1\n\t# c\nb: 2\n", Limits::default()).is_ok());
    }

    #[test]
    fn trailing_tab_only_line_is_allowed() {
        // A tab with no following content (EOF) is not an indentation error.
        assert!(tokenize("a: 1\n\t", Limits::default()).is_ok());
    }
}
