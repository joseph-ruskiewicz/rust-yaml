# yaml2 Scanner (Flow + Scalars) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the `yaml2` scanner foundation — a character reader with accurate source positions, the token type set, and tokenization of flow-style collections, document markers, all flow-context scalar styles (plain, single-quoted, double-quoted), anchors, aliases, and tags — with input-size limit enforcement.

**Architecture:** This is Plan 2 of ~8 for the `yaml2` crate (see `docs/superpowers/specs/2026-06-08-yaml2-parser-design.md`). It is the first half of the scanner layer in the pipeline `bytes → scanner (tokens) → parser (events) → composer (tree)`. This plan deliberately covers the **flow-first subset**: everything except indentation-driven block structure. Block sequences/mappings, block scalars (`|`, `>`), and multi-line plain scalars are **Plan 3**. The event parser is **Plan 4**. The scanner here is `pub(crate)` — tokens are an internal layer; the public API is events (Plan 4).

**Tech Stack:** Rust 2021, building on the completed foundation (`Position`, `Span`, `Error`/`ErrorKind`/`Result` in `error.rs`; `ScalarStyle` in `meta.rs`; `Limits` in `options.rs`). No new dependencies. No `unsafe`.

---

## Context for the engineer

A **scanner** (lexer) turns raw text into a flat stream of **tokens**. You do NOT need to understand YAML's grammar — that's the parser's job (a later plan). You only need to recognize lexical pieces and emit a token for each, every token carrying a source `Span`.

This plan handles **flow style** — YAML's JSON-like syntax — plus scalars and stream/document structure. Concretely, after this plan the scanner can tokenize input like:

```yaml
{ name: "Ada", scores: [90, 88, *prev], tag: !lang ~ }
```

into the sequence: `StreamStart`, `FlowMappingStart`, `Scalar("name", Plain)`, `Value`, `Scalar("Ada", DoubleQuoted)`, `FlowEntry`, `Scalar("scores", Plain)`, `Value`, `FlowSequenceStart`, `Scalar("90", Plain)`, `FlowEntry`, `Scalar("88", Plain)`, `FlowEntry`, `Alias("prev")`, `FlowSequenceEnd`, `FlowEntry`, `Scalar("tag", Plain)`, `Value`, `Tag("!lang")`, `Scalar("~", Plain)`, `FlowMappingEnd`, `StreamEnd`.

**Out of scope for this plan (Plan 3+):** block sequences (`- item`), block mappings via indentation, block scalars (`|`/`>`), multi-line plain scalars, `%YAML`/`%TAG` directives, verbatim `!<...>` tags, alias/depth limit enforcement (those are parser/composer concerns). Note `ScalarStyle::Literal` and `ScalarStyle::Folded` exist in the foundation but are produced only in Plan 3.

### Position conventions (from the foundation)
`Position { offset (0-based bytes), line (1-based), column (1-based chars) }`. `Span { start, end }` is half-open. A zero-width token (like `StreamStart`) uses `Span::new(pos, pos)`.

---

## File structure

A new `scanner/` module directory (a cohesive subsystem with three focused files):

| File | Responsibility |
|------|----------------|
| `yaml2/src/scanner/mod.rs` | Module root: `#![allow(dead_code)]` (consumed by the parser in Plan 4), the `Scanner` state machine, `tokenize()` helper, re-exports of `Token`/`TokenKind`/`Reader` within the crate |
| `yaml2/src/scanner/reader.rs` | `Reader` — character cursor with peek/advance and line/column/offset tracking |
| `yaml2/src/scanner/token.rs` | `Token`, `TokenKind` |
| `yaml2/src/lib.rs` | add `mod scanner;` (no public re-export) |

Why `#![allow(dead_code)]` on the scanner module: this entire layer is complete and tested here but not *consumed* by other crate code until the parser lands in Plan 4. A single module-level allow (with an explanatory comment) is cleaner than scattering per-item attributes across ~20 methods, and it is removed when Plan 4 wires the parser to the scanner.

---

## Task 1: Scanner module scaffold + Reader basics

**Files:**
- Create: `yaml2/src/scanner/mod.rs`
- Create: `yaml2/src/scanner/reader.rs`
- Modify: `yaml2/src/lib.rs`

- [ ] **Step 1: Declare the module in `yaml2/src/lib.rs`**

Add after the existing `mod scalar;` line (no public re-export):

```rust
mod scanner;
```

- [ ] **Step 2: Create `yaml2/src/scanner/mod.rs`**

```rust
//! The scanner (lexer): turns source text into a flat token stream.
//!
//! This layer is consumed by the parser (Plan 4); until then its public(crate)
//! surface is exercised only by tests, so dead-code is allowed module-wide.
#![allow(dead_code)]

mod reader;
mod token;

pub(crate) use reader::Reader;
pub(crate) use token::{Token, TokenKind};
```

- [ ] **Step 3: Write the failing test in `yaml2/src/scanner/reader.rs`**

```rust
//! A character cursor over the input with source-position tracking.

use crate::error::Position;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_reader_is_at_start() {
        let r = Reader::new("abc");
        assert_eq!(r.position(), Position::new(0, 1, 1));
        assert!(!r.is_eof());
    }

    #[test]
    fn peek_does_not_consume() {
        let r = Reader::new("abc");
        assert_eq!(r.peek(), Some('a'));
        assert_eq!(r.peek(), Some('a'));
    }

    #[test]
    fn empty_input_is_eof() {
        let r = Reader::new("");
        assert!(r.is_eof());
        assert_eq!(r.peek(), None);
    }
}
```

- [ ] **Step 4: Run the test to verify it fails**

Run: `cargo test -p yaml2 scanner::reader`
Expected: FAIL — `cannot find type Reader`.

- [ ] **Step 5: Implement the `Reader` basics** (add above the `tests` module in `reader.rs`)

```rust
pub(crate) struct Reader<'a> {
    input: &'a str,
    offset: usize,
    line: usize,
    column: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(input: &'a str) -> Self {
        Self { input, offset: 0, line: 1, column: 1 }
    }

    /// Current source position.
    pub(crate) fn position(&self) -> Position {
        Position::new(self.offset, self.line, self.column)
    }

    /// Total length of the input in bytes.
    pub(crate) fn input_len(&self) -> usize {
        self.input.len()
    }

    pub(crate) fn is_eof(&self) -> bool {
        self.offset >= self.input.len()
    }

    /// The next character without consuming it.
    pub(crate) fn peek(&self) -> Option<char> {
        self.input[self.offset..].chars().next()
    }
}
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p yaml2 scanner::reader`
Expected: 3 passed.

- [ ] **Step 7: Commit**

```bash
git add yaml2/src/scanner/mod.rs yaml2/src/scanner/reader.rs yaml2/src/lib.rs
git commit -m "feat(yaml2): scaffold scanner module and Reader cursor"
```

---

## Task 2: Reader advance, lookahead, and line-break tracking

**Files:**
- Modify: `yaml2/src/scanner/reader.rs`

- [ ] **Step 1: Write the failing tests** (add to the `tests` module in `reader.rs`)

```rust
    #[test]
    fn advance_returns_and_consumes_chars() {
        let mut r = Reader::new("ab");
        assert_eq!(r.advance(), Some('a'));
        assert_eq!(r.advance(), Some('b'));
        assert_eq!(r.advance(), None);
        assert!(r.is_eof());
    }

    #[test]
    fn advance_tracks_column_and_offset() {
        let mut r = Reader::new("ab");
        r.advance();
        assert_eq!(r.position(), Position::new(1, 1, 2));
    }

    #[test]
    fn newline_advances_line_and_resets_column() {
        let mut r = Reader::new("a\nb");
        r.advance(); // a
        r.advance(); // \n
        assert_eq!(r.position(), Position::new(2, 2, 1));
        assert_eq!(r.peek(), Some('b'));
    }

    #[test]
    fn crlf_counts_as_one_line_break() {
        let mut r = Reader::new("a\r\nb");
        r.advance(); // a
        r.advance(); // \r
        r.advance(); // \n
        assert_eq!(r.position(), Position::new(3, 2, 1));
        assert_eq!(r.peek(), Some('b'));
    }

    #[test]
    fn lone_cr_is_a_line_break() {
        let mut r = Reader::new("a\rb");
        r.advance(); // a
        r.advance(); // \r
        assert_eq!(r.position().line, 2);
        assert_eq!(r.position().column, 1);
    }

    #[test]
    fn multibyte_char_advances_offset_by_utf8_len() {
        let mut r = Reader::new("é!"); // 'é' is 2 bytes
        r.advance();
        assert_eq!(r.position(), Position::new(2, 1, 2));
        assert_eq!(r.peek(), Some('!'));
    }

    #[test]
    fn peek_nth_and_starts_with() {
        let r = Reader::new("abc");
        assert_eq!(r.peek_nth(0), Some('a'));
        assert_eq!(r.peek_nth(2), Some('c'));
        assert_eq!(r.peek_nth(3), None);
        assert!(r.starts_with("abc"));
        assert!(!r.starts_with("abd"));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p yaml2 scanner::reader`
Expected: FAIL — `no method named advance`.

- [ ] **Step 3: Implement `advance`, `peek_nth`, and `starts_with`** (add to the `impl Reader` block in `reader.rs`)

```rust
    /// The character `n` positions ahead without consuming (0 == next).
    pub(crate) fn peek_nth(&self, n: usize) -> Option<char> {
        self.input[self.offset..].chars().nth(n)
    }

    /// Whether the remaining input begins with `prefix`.
    pub(crate) fn starts_with(&self, prefix: &str) -> bool {
        self.input[self.offset..].starts_with(prefix)
    }

    /// Consume and return the next character, updating offset/line/column.
    ///
    /// Recognizes YAML line breaks `\n`, `\r\n`, and lone `\r` for line
    /// counting; the offset always advances by the char's UTF-8 length.
    pub(crate) fn advance(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.offset += c.len_utf8();
        match c {
            '\n' => {
                self.line += 1;
                self.column = 1;
            }
            '\r' => {
                // CRLF: the following '\n' performs the line break.
                // A lone CR is itself a line break.
                if self.peek() == Some('\n') {
                    self.column += 1;
                } else {
                    self.line += 1;
                    self.column = 1;
                }
            }
            _ => {
                self.column += 1;
            }
        }
        Some(c)
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p yaml2 scanner::reader`
Expected: all reader tests pass.

- [ ] **Step 5: Commit**

```bash
git add yaml2/src/scanner/reader.rs
git commit -m "feat(yaml2): add Reader advance, lookahead, and line-break tracking"
```

---

## Task 3: Token and TokenKind types

**Files:**
- Create: `yaml2/src/scanner/token.rs`

- [ ] **Step 1: Write the failing test in `yaml2/src/scanner/token.rs`**

```rust
//! The token type produced by the scanner.

use crate::error::Span;
use crate::meta::ScalarStyle;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Position;

    #[test]
    fn token_holds_kind_and_span() {
        let span = Span::new(Position::new(0, 1, 1), Position::new(1, 1, 2));
        let t = Token::new(TokenKind::FlowEntry, span);
        assert_eq!(t.kind, TokenKind::FlowEntry);
        assert_eq!(t.span, span);
    }

    #[test]
    fn scalar_kind_carries_value_and_style() {
        let kind = TokenKind::Scalar { value: "hi".to_string(), style: ScalarStyle::Plain };
        assert_eq!(
            kind,
            TokenKind::Scalar { value: "hi".to_string(), style: ScalarStyle::Plain }
        );
    }
```

(continued — close the `tests` module after the next step's note)

Add the closing brace for the `tests` module now:

```rust
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p yaml2 scanner::token`
Expected: FAIL — `cannot find type Token`.

- [ ] **Step 3: Implement the token types** (add above the `tests` module in `token.rs`)

```rust
/// A lexical token with its source span.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl Token {
    pub(crate) fn new(kind: TokenKind, span: Span) -> Self {
        Self { kind, span }
    }
}

/// The lexical categories the scanner emits.
///
/// Block-structure tokens (block mapping/sequence start/end, block entry) and
/// directives are added in Plan 3; this set covers the flow subset.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TokenKind {
    StreamStart,
    StreamEnd,
    /// `---`
    DocumentStart,
    /// `...`
    DocumentEnd,
    /// `[`
    FlowSequenceStart,
    /// `]`
    FlowSequenceEnd,
    /// `{`
    FlowMappingStart,
    /// `}`
    FlowMappingEnd,
    /// `,`
    FlowEntry,
    /// `?` (explicit key indicator)
    Key,
    /// `:` (value indicator)
    Value,
    /// `&name`
    Anchor(String),
    /// `*name`
    Alias(String),
    /// `!tag` (raw text including the leading `!`; full resolution is later)
    Tag(String),
    /// A scalar value with the style it was written in.
    Scalar { value: String, style: ScalarStyle },
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p yaml2 scanner::token`
Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add yaml2/src/scanner/token.rs
git commit -m "feat(yaml2): add scanner Token and TokenKind types"
```

---

## Task 4: Scanner skeleton — stream tokens, whitespace/comment skipping, limits

**Files:**
- Modify: `yaml2/src/scanner/mod.rs`

- [ ] **Step 1: Write the failing tests** (add to the bottom of `yaml2/src/scanner/mod.rs`, after the `pub(crate) use` lines)

```rust
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p yaml2 scanner::tests`
Expected: FAIL — `cannot find function tokenize`.

- [ ] **Step 3: Implement the scanner skeleton** (add to `yaml2/src/scanner/mod.rs`, between the `pub(crate) use` lines and the `tests` module; add the needed imports at the top of the file just under the `#![allow(dead_code)]`/module-doc and `mod` lines)

Add these imports near the top of `mod.rs` (after the `mod token;` line):

```rust
use crate::error::{Error, ErrorKind, Position, Result, Span};
use crate::options::Limits;
```

Then add the scanner:

```rust
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
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p yaml2 scanner::tests`
Expected: 4 passed.

- [ ] **Step 5: Verify clippy is clean**

Run: `cargo clippy -p yaml2 --all-targets -- -D warnings`
Expected: clean (the module-level `#![allow(dead_code)]` suppresses not-yet-consumed warnings).

- [ ] **Step 6: Commit**

```bash
git add yaml2/src/scanner/mod.rs
git commit -m "feat(yaml2): add scanner skeleton with stream tokens and limits"
```

---

## Task 5: Flow indicator tokens

**Files:**
- Modify: `yaml2/src/scanner/mod.rs`

- [ ] **Step 1: Write the failing tests** (add to the `tests` module in `mod.rs`)

```rust
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
        // ',' is FlowEntry; ':' is Value only when followed by a terminator;
        // '?' is Key only when followed by a terminator.
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p yaml2 scanner::tests::flow`
Expected: FAIL — currently `scan_content` errors on `[`.

- [ ] **Step 3: Replace `scan_content` and add helpers** (in `mod.rs`, replace the entire `scan_content` method body with a match, and add the helper methods below it)

```rust
    fn scan_content(&mut self, c: char, start: Position) -> Result<Token> {
        match c {
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
            _ => Err(Error::new(ErrorKind::Scan, format!("unexpected character {c:?}"))
                .with_span(Span::new(start, self.reader.position()))),
        }
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
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p yaml2 scanner::tests`
Expected: all current scanner tests pass.

- [ ] **Step 5: Commit**

```bash
git add yaml2/src/scanner/mod.rs
git commit -m "feat(yaml2): scan flow collection, entry, key, and value indicators"
```

---

## Task 6: Document markers `---` and `...`

A document marker is `---` or `...` appearing at the start of a line, followed by whitespace, a line break, or end-of-input. Since the flow subset skips line breaks in `skip_to_next_token`, we recognize the markers when they appear at the cursor; the "start of line / followed by blank" check uses lookahead.

**Files:**
- Modify: `yaml2/src/scanner/mod.rs`

- [ ] **Step 1: Write the failing tests** (add to the `tests` module in `mod.rs`)

```rust
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p yaml2 scanner::tests::document`
Expected: FAIL — `---` currently errors (it would try `-` as content).

- [ ] **Step 3: Add marker recognition** (in `mod.rs`, add two new arms to the `scan_content` match BEFORE the `_` catch-all arm, and add the `scan_marker` helper)

Add these arms (place them as the first two arms of the match, before `'['`):

```rust
            '-' if self.marker_ahead("---") => {
                Ok(self.scan_marker(TokenKind::DocumentStart, start))
            }
            '.' if self.marker_ahead("...") => {
                Ok(self.scan_marker(TokenKind::DocumentEnd, start))
            }
```

Add these helper methods (next to the other helpers):

```rust
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
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p yaml2 scanner::tests`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add yaml2/src/scanner/mod.rs
git commit -m "feat(yaml2): scan document start/end markers"
```

---

## Task 7: Single-quoted scalars

Single-quoted scalars run from `'` to the next `'`, with a doubled `''` representing a literal single quote. (Multi-line single-quoted folding is deferred to Plan 3; for this slice, an unterminated quote before EOF is an error.)

**Files:**
- Modify: `yaml2/src/scanner/mod.rs`

- [ ] **Step 1: Write the failing tests** (add to the `tests` module in `mod.rs`)

```rust
    use crate::meta::ScalarStyle;

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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p yaml2 scanner::tests::single_quoted`
Expected: FAIL.

- [ ] **Step 3: Add the single-quote arm and scanner** (add an arm to `scan_content` before the `_` catch-all: `'\'' => self.scan_single_quoted(start),` and add the method)

```rust
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
```

Note: add `use crate::meta::ScalarStyle;` to the imports at the top of `mod.rs` if not already present (Task 5 didn't need it; this task does).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p yaml2 scanner::tests`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add yaml2/src/scanner/mod.rs
git commit -m "feat(yaml2): scan single-quoted scalars"
```

---

## Task 8: Double-quoted scalars (with escapes)

Double-quoted scalars run from `"` to the next unescaped `"` and support YAML escape sequences. (Multi-line double-quoted folding is deferred to Plan 3; unterminated before EOF is an error.)

**Files:**
- Modify: `yaml2/src/scanner/mod.rs`

- [ ] **Step 1: Write the failing tests** (add to the `tests` module in `mod.rs`)

```rust
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p yaml2 scanner::tests::double_quoted`
Expected: FAIL.

- [ ] **Step 3: Add the double-quote arm and scanner** (add an arm to `scan_content` before the `_` catch-all: `'"' => self.scan_double_quoted(start),` and add the methods)

```rust
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
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p yaml2 scanner::tests`
Expected: all pass.

- [ ] **Step 5: Verify clippy is clean**

Run: `cargo clippy -p yaml2 --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add yaml2/src/scanner/mod.rs
git commit -m "feat(yaml2): scan double-quoted scalars with escape sequences"
```

---

## Task 9: Plain scalars (flow context, single-line)

A plain (unquoted) scalar runs until a terminator. In the flow subset the terminators are: a flow indicator (`,`, `[`, `]`, `{`, `}`), a `:` that is followed by whitespace/flow-indicator/EOF (a value indicator), a ` #` comment (whitespace then `#`), a line break, or end-of-input. Internal spaces are kept; trailing whitespace is trimmed. The scanner reaches plain-scalar scanning only when no other token matched (the `_` arm), so the first character is already known not to be an indicator that starts another token.

**Files:**
- Modify: `yaml2/src/scanner/mod.rs`

- [ ] **Step 1: Write the failing tests** (add to the `tests` module in `mod.rs`)

```rust
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
        // 'http://x' — the ':' is followed by '/', not a terminator.
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
                TokenKind::Scalar { value: "key".to_string(), style: ScalarStyle::Plain },
                TokenKind::Value,
                TokenKind::Scalar { value: "value".to_string(), style: ScalarStyle::Plain },
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
                TokenKind::Scalar { value: "a".to_string(), style: ScalarStyle::Plain },
                TokenKind::FlowEntry,
                TokenKind::Scalar { value: "b".to_string(), style: ScalarStyle::Plain },
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p yaml2 scanner::tests::plain`
Expected: FAIL — plain scalars currently hit the `_` error arm.

- [ ] **Step 3: Replace the `_` arm of `scan_content`** (change the catch-all `_ => Err(...)` arm to call the plain scanner) and add the method

Change the catch-all arm to:

```rust
            _ => self.scan_plain(start),
```

Add the method:

```rust
    /// Scans a single-line plain scalar in flow context.
    fn scan_plain(&mut self, start: Position) -> Result<Token> {
        let mut value = String::new();
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
                }
            }
        }
        let trimmed_len = value.trim_end_matches([' ', '\t']).len();
        value.truncate(trimmed_len);
        Ok(Token::new(
            TokenKind::Scalar { value, style: ScalarStyle::Plain },
            Span::new(start, self.reader.position()),
        ))
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p yaml2 scanner::tests`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add yaml2/src/scanner/mod.rs
git commit -m "feat(yaml2): scan single-line plain scalars in flow context"
```

---

## Task 10: Anchors, aliases, and tags

`&name` is an anchor, `*name` an alias, `!...` a tag. Anchor/alias names run over non-whitespace characters excluding the flow indicators `,[]{}`. A tag is the `!` plus the same run of name characters (kept raw, including the leading `!`).

**Files:**
- Modify: `yaml2/src/scanner/mod.rs`

- [ ] **Step 1: Write the failing tests** (add to the `tests` module in `mod.rs`)

```rust
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
                TokenKind::Scalar { value: "1".to_string(), style: ScalarStyle::Plain },
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p yaml2 scanner::tests::anchor`
Expected: FAIL (`&` currently routes to plain scalar and produces wrong tokens).

- [ ] **Step 3: Add the arms and helpers** (add three arms to `scan_content` before the `_` catch-all, and add the helper methods)

Add these arms (before `_ => self.scan_plain(start),`):

```rust
            '&' => self.scan_anchor_or_alias(start, true),
            '*' => self.scan_anchor_or_alias(start, false),
            '!' => Ok(self.scan_tag(start)),
```

Add the methods:

```rust
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
        Token::new(TokenKind::Tag(text), Span::new(start, self.reader.position()))
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
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p yaml2 scanner::tests`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add yaml2/src/scanner/mod.rs
git commit -m "feat(yaml2): scan anchors, aliases, and tags"
```

---

## Task 11: Integration, multi-document, and full verification

**Files:**
- Modify: `yaml2/src/scanner/mod.rs`

- [ ] **Step 1: Write the integration tests** (add to the `tests` module in `mod.rs`)

```rust
    #[test]
    fn full_flow_document_tokenizes() {
        let input = r#"{ name: "Ada", scores: [90, 88, *prev], tag: !lang ~ }"#;
        assert_eq!(
            kinds(input),
            vec![
                TokenKind::StreamStart,
                TokenKind::FlowMappingStart,
                TokenKind::Scalar { value: "name".to_string(), style: ScalarStyle::Plain },
                TokenKind::Value,
                TokenKind::Scalar { value: "Ada".to_string(), style: ScalarStyle::DoubleQuoted },
                TokenKind::FlowEntry,
                TokenKind::Scalar { value: "scores".to_string(), style: ScalarStyle::Plain },
                TokenKind::Value,
                TokenKind::FlowSequenceStart,
                TokenKind::Scalar { value: "90".to_string(), style: ScalarStyle::Plain },
                TokenKind::FlowEntry,
                TokenKind::Scalar { value: "88".to_string(), style: ScalarStyle::Plain },
                TokenKind::FlowEntry,
                TokenKind::Alias("prev".to_string()),
                TokenKind::FlowSequenceEnd,
                TokenKind::FlowEntry,
                TokenKind::Scalar { value: "tag".to_string(), style: ScalarStyle::Plain },
                TokenKind::Value,
                TokenKind::Tag("!lang".to_string()),
                TokenKind::Scalar { value: "~".to_string(), style: ScalarStyle::Plain },
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
                TokenKind::Scalar { value: "1".to_string(), style: ScalarStyle::Plain },
                TokenKind::FlowSequenceEnd,
                TokenKind::DocumentStart,
                TokenKind::FlowSequenceStart,
                TokenKind::Scalar { value: "2".to_string(), style: ScalarStyle::Plain },
                TokenKind::FlowSequenceEnd,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn spans_are_accurate_for_a_small_input() {
        // input: a, b   (indices: a@0, ,@1, space@2, b@3)
        let toks = tokenize("a, b", Limits::default()).unwrap();
        // toks[0] = StreamStart (zero-width @0)
        let scalar_a = &toks[1];
        assert!(matches!(&scalar_a.kind, TokenKind::Scalar { value, .. } if value == "a"));
        assert_eq!(scalar_a.span.start, crate::error::Position::new(0, 1, 1));
        assert_eq!(scalar_a.span.end, crate::error::Position::new(1, 1, 2));

        let comma = &toks[2];
        assert_eq!(comma.kind, TokenKind::FlowEntry);
        assert_eq!(comma.span.start, crate::error::Position::new(1, 1, 2));
        assert_eq!(comma.span.end, crate::error::Position::new(2, 1, 3));
    }
```

- [ ] **Step 2: Run the tests to verify they pass**

Run: `cargo test -p yaml2 scanner::tests`
Expected: all pass (no implementation changes needed — these exercise the assembled scanner).

If any fail, do NOT change the test expectations to match buggy output — investigate the scanner. Report a BLOCKED/DONE_WITH_CONCERNS status describing the discrepancy.

- [ ] **Step 3: Run the full crate suite**

Run: `cargo test -p yaml2`
Expected: all foundation + scanner tests pass.

- [ ] **Step 4: Verify clippy and formatting**

Run: `cargo clippy -p yaml2 --all-targets -- -D warnings`
Expected: clean.

Run: `cargo fmt` then `cargo fmt --check`
Expected: no diff.

- [ ] **Step 5: Commit**

```bash
git add yaml2/src/scanner/mod.rs
git commit -m "test(yaml2): integration tests for flow tokenization and spans"
```

---

## Self-Review

**Spec coverage (this plan's portion of the scanner layer):**
- Scanner as the first pipeline stage (`bytes → tokens`) — all tasks. ✔
- Source spans on every token (`Position`/`Span` from the foundation) — Tasks 1, 2 (reader positions), 3 (Token carries Span), 11 (span accuracy test). ✔
- Security: input-size limit enforced, returning `ErrorKind::LimitExceeded` — Task 4. ✔
- Flow collections, entry/key/value indicators — Tasks 5. ✔
- Document markers (multi-document streams) — Tasks 6, 11. ✔
- All flow-context scalar styles: single-quoted, double-quoted (with full escape set incl. `\x`/`\u`/`\U`), plain — Tasks 7, 8, 9. ✔
- Anchors, aliases, tags — Task 10. ✔
- `Error`/`ErrorKind::Scan` with spans for lexical errors — Tasks 7–10. ✔
- Pure Rust, no `unsafe` (foundation lint still in force). ✔

**Deferred to later plans (correctly out of scope here):** block sequences/mappings (indentation), block scalars (`|`/`>`), multi-line plain/quoted folding, `%YAML`/`%TAG` directives, verbatim `!<...>` tags and full tag resolution, alias/nesting-depth limit enforcement (Plan 4 parser/composer), the event parser and the public streaming API (Plan 4), and resolving scalars to typed values via `Value::from_scalar` (composer, Plan 5).

**Placeholder scan:** No "TBD/TODO". The `scan_content` body in Task 4 is an explicit, temporary catch-all error that is progressively replaced by real arms in Tasks 5–10 (each with tests); its final form (Task 9 onward) routes unknown starts to `scan_plain`. ✔

**Type consistency:** `Reader` methods (`new`, `position`, `input_len`, `is_eof`, `peek`, `peek_nth`, `starts_with`, `advance`) are defined in Tasks 1–2 and used consistently in Tasks 4–10. `Token::new(kind, span)` and `TokenKind` variants are defined in Task 3 and used identically thereafter. `Scanner::new(input, limits)`, `next_token() -> Result<Option<Token>>`, and `tokenize(input, limits) -> Result<Vec<Token>>` are defined in Task 4 and used unchanged in tests through Task 11. The `scan_content` match arms added across Tasks 5–10 share the helpers `single_char`, `indicator_terminator_next`, `marker_ahead`, `scan_marker`, `take_name`. `ScalarStyle` (Plain/SingleQuoted/DoubleQuoted) is the foundation enum; Literal/Folded are intentionally not produced here. ✔
