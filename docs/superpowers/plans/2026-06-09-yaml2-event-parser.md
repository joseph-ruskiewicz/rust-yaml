# yaml2 Event Parser Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the `yaml2` event parser — a recursive-descent state machine that consumes the scanner's token stream and produces a public `Event` stream (the streaming API), enforcing `max_depth` and rejecting malformed token streams.

**Architecture:** Plan 5 of 9 for `yaml2` (see `docs/superpowers/specs/2026-06-08-yaml2-parser-design.md`). The pipeline is `bytes → scanner (tokens) → parser (events) → composer (tree)`. This plan is the parser: it tokenizes via the completed scanner, then recursively descends over the token vector producing `Event`s into a buffer exposed through `Parser::next_event()`. Recursion is bounded by `max_depth` (no stack overflow; deep input → `LimitExceeded`). Malformed token sequences (e.g. a `Value` with no preceding `Key`) are rejected with `Parse` errors.

**Tech Stack:** Rust 2021. Builds on Plans 1–4 (foundation, scanner). The scanner's `tokenize`, `Token`, `TokenKind` are `pub(crate)` and consumed here. No new dependencies. No `unsafe`.

---

## Context for the engineer

The scanner (`crate::scanner`, private module, `pub(crate)` items) turns text into a `Vec<Token>` via `tokenize(input: &str, limits: Limits) -> Result<Vec<Token>>`. `Token { kind: TokenKind, span: Span }`. `TokenKind` variants: `StreamStart`, `StreamEnd`, `DocumentStart`, `DocumentEnd`, `BlockSequenceStart`, `BlockMappingStart`, `BlockEnd`, `BlockEntry`, `FlowSequenceStart`, `FlowSequenceEnd`, `FlowMappingStart`, `FlowMappingEnd`, `FlowEntry`, `Key`, `Value`, `Anchor(String)`, `Alias(String)`, `Tag(String)`, `Scalar { value: String, style: ScalarStyle }`.

The **parser** turns tokens into **events** — the public streaming API. Events are the standard YAML event model:

- `StreamStart`, `StreamEnd`
- `DocumentStart`, `DocumentEnd`
- `Scalar { value, style, anchor, tag }`
- `Alias(name)`
- `SequenceStart { anchor, tag }`, `SequenceEnd`
- `MappingStart { anchor, tag }`, `MappingEnd`

The composer (Plan 6) consumes this event stream to build the `Value` tree. The parser's job is purely structural: pair `Key`/`Value`, bracket collections with `*Start`/`*End`, attach anchors/tags to nodes, synthesize empty scalars for implicit nulls, and reject malformed sequences.

### Token → event mapping (the core)

| Token(s) | Event(s) |
|----------|----------|
| `StreamStart` | `StreamStart` |
| `StreamEnd` | `StreamEnd` |
| `DocumentStart` (`---`) | `DocumentStart` (also implicit at first content) |
| `Scalar{v,s}` | `Scalar{v,s,anchor,tag}` |
| `Alias(n)` | `Alias(n)` |
| `&a` / `!t` before a node | folded into that node's `anchor`/`tag` |
| `BlockSequenceStart … BlockEntry … BlockEnd` | `SequenceStart … <entries> … SequenceEnd` |
| `BlockMappingStart … Key <k> Value <v> … BlockEnd` | `MappingStart … <k><v> … MappingEnd` |
| bare `BlockEntry …` (indentless seq) | `SequenceStart … SequenceEnd` (no `BlockEnd` token) |
| `FlowSequenceStart … FlowEntry … FlowSequenceEnd` | `SequenceStart … SequenceEnd` |
| `FlowMappingStart … FlowEntry … FlowMappingEnd` | `MappingStart … MappingEnd` |
| `[a: b]` (flow seq single-pair) | `SequenceStart, MappingStart, a, b, MappingEnd, SequenceEnd` |

### Key design points

- **Recursion + `max_depth`:** `parse_node` increments a depth counter and errors with `LimitExceeded` if it exceeds `options.limits.max_depth`, then decrements on exit. This bounds recursion (default 128) — no stack overflow, and a tracked Plan-3 follow-up is satisfied here.
- **Implicit empty scalars:** YAML allows empty nodes (`a:` with no value → null value; `- ` with nothing → null entry; `{a, b}` → keys with null values). The parser synthesizes an empty `Scalar { value: "", style: Plain }`.
- **Flow has no `Key` tokens:** the scanner does not emit `Key` inside flow mappings; the parser pairs implicit keys by looking for the `Value` (`:`) token. Explicit `?` (a `Key` token) is also handled.
- **Single-pair flow-sequence mappings (`[a: b]`):** require inserting `MappingStart` *before* the already-emitted key event — done by recording the event-buffer index before parsing the key and `insert`ing at that index when a `Value` follows.
- **Malformed rejection:** the collection loops error on unexpected tokens (e.g. `MappingStart` immediately followed by `Value` with no `Key`, as `a: b: c` produces).

### Public API

`Event`/`EventKind` and `Parser` are **public** (this is the streaming API). Tokens stay internal. `Parser::new(input, &ParseOptions) -> Result<Self>` parses eagerly; `next_event() -> Option<Event>` drains. A free `parse_events(input, &ParseOptions) -> Result<Vec<Event>>` is the workhorse the composer (Plan 6) uses.

---

## File structure

| File | Responsibility |
|------|----------------|
| `yaml2/src/event.rs` | `Event`, `EventKind` (the public event model) |
| `yaml2/src/parser.rs` | `Parser`, `parse_events`, the recursive-descent state machine |
| `yaml2/src/lib.rs` | `mod event; mod parser;` + `pub use` of `Event`, `EventKind`, `Parser`, `parse_events` |

---

## Task 1: Event types

**Files:** create `yaml2/src/event.rs`; modify `yaml2/src/lib.rs`.

- [ ] **Step 1: Declare the module and exports in `yaml2/src/lib.rs`.** Add after `mod scanner;`:

```rust
mod event;
```

Add to the public re-exports (after the `pub use scalar`-less existing block — i.e. with the other `pub use` lines):

```rust
pub use event::{Event, EventKind};
```

- [ ] **Step 2: Write the failing test in `yaml2/src/event.rs`**

```rust
//! The public event model produced by the parser.

use crate::error::Span;
use crate::meta::ScalarStyle;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Position;

    #[test]
    fn event_holds_kind_and_span() {
        let span = Span::new(Position::new(0, 1, 1), Position::new(0, 1, 1));
        let e = Event::new(EventKind::StreamStart, span);
        assert_eq!(e.kind, EventKind::StreamStart);
        assert_eq!(e.span, span);
    }

    #[test]
    fn scalar_event_carries_value_style_anchor_tag() {
        let kind = EventKind::Scalar {
            value: "x".to_string(),
            style: ScalarStyle::Plain,
            anchor: Some("a".to_string()),
            tag: Some("!t".to_string()),
        };
        assert_eq!(
            kind,
            EventKind::Scalar {
                value: "x".to_string(),
                style: ScalarStyle::Plain,
                anchor: Some("a".to_string()),
                tag: Some("!t".to_string()),
            }
        );
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p yaml2 event::`
Expected: FAIL — `cannot find type Event`.

- [ ] **Step 4: Implement the event types** (add above the `tests` module in `event.rs`)

```rust
/// A parser event with its source span.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub kind: EventKind,
    pub span: Span,
}

impl Event {
    pub fn new(kind: EventKind, span: Span) -> Self {
        Self { kind, span }
    }
}

/// The kinds of events the parser emits (the YAML event model).
#[derive(Debug, Clone, PartialEq)]
pub enum EventKind {
    StreamStart,
    StreamEnd,
    DocumentStart,
    DocumentEnd,
    /// A scalar node. `anchor`/`tag` are the node's properties if present.
    Scalar {
        value: String,
        style: ScalarStyle,
        anchor: Option<String>,
        tag: Option<String>,
    },
    /// An alias reference (`*name`).
    Alias(String),
    SequenceStart {
        anchor: Option<String>,
        tag: Option<String>,
    },
    SequenceEnd,
    MappingStart {
        anchor: Option<String>,
        tag: Option<String>,
    },
    MappingEnd,
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p yaml2 event::`
Expected: `2 passed`.

- [ ] **Step 6: Commit**

```bash
git add yaml2/src/event.rs yaml2/src/lib.rs
git commit -m "feat(yaml2): add the public Event model"
```

---

## Task 2: Parser scaffold and stream framing

**Files:** create `yaml2/src/parser.rs`; modify `yaml2/src/lib.rs`.

- [ ] **Step 1: Declare the module and exports in `yaml2/src/lib.rs`.** Add after `mod event;`:

```rust
mod parser;
```

Add to the public re-exports:

```rust
pub use parser::{parse_events, Parser};
```

- [ ] **Step 2: Write the failing test in `yaml2/src/parser.rs`**

```rust
//! The event parser: tokens -> events (recursive descent, depth-bounded).

use crate::error::{Error, ErrorKind, Result, Span};
use crate::event::{Event, EventKind};
use crate::meta::ScalarStyle;
use crate::options::ParseOptions;
use crate::scanner::{tokenize, Token, TokenKind};

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(input: &str) -> Vec<EventKind> {
        parse_events(input, &ParseOptions::default())
            .unwrap()
            .into_iter()
            .map(|e| e.kind)
            .collect()
    }

    #[test]
    fn empty_input_is_stream_start_then_end() {
        assert_eq!(kinds(""), vec![EventKind::StreamStart, EventKind::StreamEnd]);
    }

    #[test]
    fn comment_only_is_stream_start_then_end() {
        assert_eq!(
            kinds("# just a comment\n"),
            vec![EventKind::StreamStart, EventKind::StreamEnd]
        );
    }

    #[test]
    fn parser_pull_api_drains_events() {
        let mut p = Parser::new("", &ParseOptions::default()).unwrap();
        assert_eq!(p.next_event().map(|e| e.kind), Some(EventKind::StreamStart));
        assert_eq!(p.next_event().map(|e| e.kind), Some(EventKind::StreamEnd));
        assert_eq!(p.next_event(), None);
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p yaml2 parser::`
Expected: FAIL — `cannot find function parse_events`.

- [ ] **Step 4: Implement the parser scaffold, helpers, and stream framing** (add above the `tests` module in `parser.rs`)

```rust
/// Parses `input` into a complete event vector.
pub fn parse_events(input: &str, options: &ParseOptions) -> Result<Vec<Event>> {
    let tokens = tokenize(input, options.limits)?;
    let mut parser = ParserState {
        tokens,
        pos: 0,
        events: Vec::new(),
        depth: 0,
        max_depth: options.limits.max_depth,
    };
    parser.parse_stream()?;
    Ok(parser.events)
}

/// A pull-based event parser over an input document.
pub struct Parser {
    events: std::vec::IntoIter<Event>,
}

impl Parser {
    /// Parses `input` eagerly; subsequent `next_event` calls drain the result.
    pub fn new(input: &str, options: &ParseOptions) -> Result<Self> {
        Ok(Self {
            events: parse_events(input, options)?.into_iter(),
        })
    }

    /// Returns the next event, or `None` once the stream is exhausted.
    pub fn next_event(&mut self) -> Option<Event> {
        self.events.next()
    }
}

/// Internal recursive-descent state.
struct ParserState {
    tokens: Vec<Token>,
    pos: usize,
    events: Vec<Event>,
    depth: usize,
    max_depth: usize,
}

impl ParserState {
    fn peek(&self) -> Option<&TokenKind> {
        self.tokens.get(self.pos).map(|t| &t.kind)
    }

    /// The span of the current token (or the last token's span at end of input).
    fn span(&self) -> Span {
        self.tokens
            .get(self.pos)
            .or_else(|| self.tokens.last())
            .map(|t| t.span)
            .unwrap_or_default()
    }

    /// Consumes and returns the current token. Callers must `peek` first.
    fn bump(&mut self) -> Token {
        let token = self.tokens[self.pos].clone();
        self.pos += 1;
        token
    }

    fn emit(&mut self, kind: EventKind, span: Span) {
        self.events.push(Event::new(kind, span));
    }

    fn emit_empty_scalar(&mut self) {
        let span = self.span();
        self.emit(
            EventKind::Scalar {
                value: String::new(),
                style: ScalarStyle::Plain,
                anchor: None,
                tag: None,
            },
            span,
        );
    }

    fn error(&self, message: &str) -> Error {
        Error::new(ErrorKind::Parse, message).with_span(self.span())
    }

    fn parse_stream(&mut self) -> Result<()> {
        match self.peek() {
            Some(TokenKind::StreamStart) => {
                let span = self.span();
                self.bump();
                self.emit(EventKind::StreamStart, span);
            }
            _ => return Err(self.error("expected stream start")),
        }

        loop {
            match self.peek() {
                None | Some(TokenKind::StreamEnd) => {
                    let span = self.span();
                    if self.peek().is_some() {
                        self.bump();
                    }
                    self.emit(EventKind::StreamEnd, span);
                    break;
                }
                Some(TokenKind::DocumentEnd) => {
                    // A bare `...` with no document; skip it.
                    self.bump();
                }
                Some(TokenKind::DocumentStart) => {
                    let span = self.span();
                    self.bump();
                    self.emit(EventKind::DocumentStart, span);
                    self.parse_document_content()?;
                    self.finish_document();
                }
                _ => {
                    let span = self.span();
                    self.emit(EventKind::DocumentStart, span);
                    self.parse_document_content()?;
                    self.finish_document();
                }
            }
        }
        Ok(())
    }

    /// Parses a document's root node, or an empty node for an empty document.
    fn parse_document_content(&mut self) -> Result<()> {
        match self.peek() {
            None
            | Some(TokenKind::StreamEnd)
            | Some(TokenKind::DocumentStart)
            | Some(TokenKind::DocumentEnd) => {
                self.emit_empty_scalar();
                Ok(())
            }
            _ => self.parse_node(),
        }
    }

    /// Emits `DocumentEnd`, consuming an optional `...` token.
    fn finish_document(&mut self) {
        let span = self.span();
        if matches!(self.peek(), Some(TokenKind::DocumentEnd)) {
            self.bump();
        }
        self.emit(EventKind::DocumentEnd, span);
    }

    /// Parses a single node. Replaced/extended in later tasks; for now any node
    /// position yields an empty scalar.
    fn parse_node(&mut self) -> Result<()> {
        self.emit_empty_scalar();
        Ok(())
    }
}
```

Note: `parse_node` is a temporary stub (emits empty scalar) so framing compiles and the Task-2 tests (which never reach a real node) pass. Tasks 3–10 build the real `parse_node`.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p yaml2 parser::`
Expected: `3 passed`. (`max_depth`/`depth` fields are unused so far — that's fine; they're consumed in Task 11. If `cargo build` warns `unused`, it will not fail tests; clippy is checked at the end of Task 3 onward. To keep clippy green now, add `#[allow(dead_code)]` to the `depth` and `max_depth` fields with a `// used in Task 11` comment, then remove it in Task 11.)

- [ ] **Step 6: Verify clippy**

Run: `cargo clippy -p yaml2 --all-targets -- -D warnings`
Expected: clean (with the targeted `#[allow(dead_code)]` on `depth`/`max_depth` if needed).

- [ ] **Step 7: Commit**

```bash
git add yaml2/src/parser.rs yaml2/src/lib.rs
git commit -m "feat(yaml2): add parser scaffold with stream/document framing"
```

---

## Task 3: Scalar and alias nodes

**Files:** modify `yaml2/src/parser.rs`.

- [ ] **Step 1: Write the failing tests** (add to the `tests` module)

```rust
    #[test]
    fn bare_scalar_document() {
        assert_eq!(
            kinds("hello\n"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::Scalar {
                    value: "hello".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None,
                },
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn explicit_document_marker() {
        assert_eq!(
            kinds("--- hello\n"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::Scalar {
                    value: "hello".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: None,
                    tag: None,
                },
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn two_documents() {
        assert_eq!(
            kinds("--- a\n--- b\n"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::Scalar { value: "a".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::DocumentEnd,
                EventKind::DocumentStart,
                EventKind::Scalar { value: "b".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn alias_node() {
        // `*x` as a document root. (The anchor `&x` is defined then referenced.)
        assert_eq!(
            kinds("[&x a, *x]"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::SequenceStart { anchor: None, tag: None },
                EventKind::Scalar { value: "a".to_string(), style: ScalarStyle::Plain, anchor: Some("x".to_string()), tag: None },
                EventKind::Alias("x".to_string()),
                EventKind::SequenceEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }
```

(`alias_node` also exercises flow sequences and anchors — it will pass once Tasks 7 and 9 are done. For THIS task, replace the `alias_node` test body with a flow-free version and re-add the full one in Task 9. Use this for Task 3:)

```rust
    #[test]
    fn alias_node() {
        // A bare alias as a document root.
        assert_eq!(
            kinds("*x\n"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::Alias("x".to_string()),
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p yaml2 parser::tests::bare_scalar_document`
Expected: FAIL — the stub `parse_node` emits an empty scalar, not `"hello"`.

- [ ] **Step 3: Replace the stub `parse_node`** with the real dispatch (scalars and aliases for now; collection arms are added in later tasks). Also add `parse_node_inner`:

```rust
    fn parse_node(&mut self) -> Result<()> {
        self.depth += 1;
        if self.depth > self.max_depth {
            self.depth -= 1;
            return Err(Error::new(ErrorKind::LimitExceeded, "maximum nesting depth exceeded")
                .with_span(self.span()));
        }
        let result = self.parse_node_inner();
        self.depth -= 1;
        result
    }

    fn parse_node_inner(&mut self) -> Result<()> {
        // Node properties (anchor `&`, tag `!`) in any order.
        let mut anchor: Option<String> = None;
        let mut tag: Option<String> = None;
        loop {
            match self.peek() {
                Some(TokenKind::Anchor(_)) => {
                    if anchor.is_some() {
                        return Err(self.error("a node may have at most one anchor"));
                    }
                    if let TokenKind::Anchor(name) = self.bump().kind {
                        anchor = Some(name);
                    }
                }
                Some(TokenKind::Tag(_)) => {
                    if tag.is_some() {
                        return Err(self.error("a node may have at most one tag"));
                    }
                    if let TokenKind::Tag(t) = self.bump().kind {
                        tag = Some(t);
                    }
                }
                _ => break,
            }
        }

        let span = self.span();
        match self.peek() {
            Some(TokenKind::Scalar { .. }) => {
                let token = self.bump();
                if let TokenKind::Scalar { value, style } = token.kind {
                    self.emit(
                        EventKind::Scalar { value, style, anchor, tag },
                        token.span,
                    );
                }
                Ok(())
            }
            Some(TokenKind::Alias(_)) => {
                if anchor.is_some() || tag.is_some() {
                    return Err(self.error("an alias node cannot have an anchor or tag"));
                }
                let token = self.bump();
                if let TokenKind::Alias(name) = token.kind {
                    self.emit(EventKind::Alias(name), token.span);
                }
                Ok(())
            }
            _ => {
                // Implicit empty (null) node — carries any collected properties.
                self.emit(
                    EventKind::Scalar {
                        value: String::new(),
                        style: ScalarStyle::Plain,
                        anchor,
                        tag,
                    },
                    span,
                );
                Ok(())
            }
        }
    }
```

(If you added `#[allow(dead_code)]` to `depth`/`max_depth` in Task 2, you may now remove it from `depth` since `parse_node` uses it; keep it on `max_depth` only if clippy still flags it — it is read in the depth check, so it should be fine to remove both now.)

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p yaml2 parser::`
Expected: all parser tests pass.

- [ ] **Step 5: Verify clippy**

Run: `cargo clippy -p yaml2 --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add yaml2/src/parser.rs
git commit -m "feat(yaml2): parse scalar and alias nodes with properties"
```

---

## Task 4: Block sequences

**Files:** modify `yaml2/src/parser.rs`.

- [ ] **Step 1: Write the failing test** (add to the `tests` module)

```rust
    #[test]
    fn block_sequence() {
        assert_eq!(
            kinds("- a\n- b\n"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::SequenceStart { anchor: None, tag: None },
                EventKind::Scalar { value: "a".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::Scalar { value: "b".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::SequenceEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn nested_block_sequence() {
        assert_eq!(
            kinds("- - a\n"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::SequenceStart { anchor: None, tag: None },
                EventKind::SequenceStart { anchor: None, tag: None },
                EventKind::Scalar { value: "a".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::SequenceEnd,
                EventKind::SequenceEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p yaml2 parser::tests::block_sequence`
Expected: FAIL — `parse_node` has no `BlockSequenceStart` arm (falls to empty scalar).

- [ ] **Step 3: Add the `BlockSequenceStart` arm and `parse_block_sequence`.** In `parse_node_inner`'s `match self.peek()`, add this arm before the `_` catch-all:

```rust
            Some(TokenKind::BlockSequenceStart) => self.parse_block_sequence(anchor, tag),
```

Add the method:

```rust
    fn parse_block_sequence(
        &mut self,
        anchor: Option<String>,
        tag: Option<String>,
    ) -> Result<()> {
        let start = self.bump(); // BlockSequenceStart
        self.emit(EventKind::SequenceStart { anchor, tag }, start.span);
        loop {
            match self.peek() {
                Some(TokenKind::BlockEntry) => {
                    self.bump();
                    if matches!(
                        self.peek(),
                        Some(TokenKind::BlockEntry) | Some(TokenKind::BlockEnd)
                    ) {
                        self.emit_empty_scalar();
                    } else {
                        self.parse_node()?;
                    }
                }
                Some(TokenKind::BlockEnd) => {
                    let end = self.bump();
                    self.emit(EventKind::SequenceEnd, end.span);
                    return Ok(());
                }
                _ => return Err(self.error("expected a block sequence entry or end")),
            }
        }
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p yaml2 parser::`
Expected: all pass.

- [ ] **Step 5: Verify clippy**

Run: `cargo clippy -p yaml2 --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add yaml2/src/parser.rs
git commit -m "feat(yaml2): parse block sequences"
```

---

## Task 5: Block mappings

**Files:** modify `yaml2/src/parser.rs`.

- [ ] **Step 1: Write the failing test** (add to the `tests` module)

```rust
    #[test]
    fn block_mapping() {
        assert_eq!(
            kinds("a: 1\nb: 2\n"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::MappingStart { anchor: None, tag: None },
                EventKind::Scalar { value: "a".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::Scalar { value: "1".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::Scalar { value: "b".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::Scalar { value: "2".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::MappingEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn nested_block_mapping() {
        assert_eq!(
            kinds("outer:\n  inner: v\n"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::MappingStart { anchor: None, tag: None },
                EventKind::Scalar { value: "outer".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::MappingStart { anchor: None, tag: None },
                EventKind::Scalar { value: "inner".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::Scalar { value: "v".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::MappingEnd,
                EventKind::MappingEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p yaml2 parser::tests::block_mapping`
Expected: FAIL — no `BlockMappingStart` arm.

- [ ] **Step 3: Add the `BlockMappingStart` arm and `parse_block_mapping`.** In `parse_node_inner`, add before the `_` catch-all:

```rust
            Some(TokenKind::BlockMappingStart) => self.parse_block_mapping(anchor, tag),
```

Add the method:

```rust
    fn parse_block_mapping(
        &mut self,
        anchor: Option<String>,
        tag: Option<String>,
    ) -> Result<()> {
        let start = self.bump(); // BlockMappingStart
        self.emit(EventKind::MappingStart { anchor, tag }, start.span);
        loop {
            match self.peek() {
                Some(TokenKind::Key) => {
                    self.bump();
                    // Key node (empty if directly followed by Value/Key/BlockEnd).
                    if matches!(
                        self.peek(),
                        Some(TokenKind::Value) | Some(TokenKind::Key) | Some(TokenKind::BlockEnd)
                    ) {
                        self.emit_empty_scalar();
                    } else {
                        self.parse_node()?;
                    }
                    // Value node.
                    if matches!(self.peek(), Some(TokenKind::Value)) {
                        self.bump();
                        if matches!(
                            self.peek(),
                            Some(TokenKind::Key) | Some(TokenKind::BlockEnd)
                        ) {
                            self.emit_empty_scalar();
                        } else {
                            self.parse_node()?;
                        }
                    } else {
                        // Key with no value indicator → empty value.
                        self.emit_empty_scalar();
                    }
                }
                Some(TokenKind::BlockEnd) => {
                    let end = self.bump();
                    self.emit(EventKind::MappingEnd, end.span);
                    return Ok(());
                }
                _ => return Err(self.error("expected a block mapping key or end")),
            }
        }
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p yaml2 parser::`
Expected: all pass.

- [ ] **Step 5: Verify clippy**

Run: `cargo clippy -p yaml2 --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add yaml2/src/parser.rs
git commit -m "feat(yaml2): parse block mappings"
```

---

## Task 6: Indentless sequences

A block sequence whose entries are at the same column as the parent mapping key has **no** `BlockSequenceStart`/`BlockEnd` tokens — just bare `BlockEntry` tokens after a `Value`. The parser recognizes this when `parse_node` is called in a value position and the next token is `BlockEntry`.

**Files:** modify `yaml2/src/parser.rs`.

- [ ] **Step 1: Write the failing test** (add to the `tests` module)

```rust
    #[test]
    fn indentless_sequence_value() {
        assert_eq!(
            kinds("items:\n- a\n- b\n"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::MappingStart { anchor: None, tag: None },
                EventKind::Scalar { value: "items".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::SequenceStart { anchor: None, tag: None },
                EventKind::Scalar { value: "a".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::Scalar { value: "b".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::SequenceEnd,
                EventKind::MappingEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p yaml2 parser::tests::indentless_sequence_value`
Expected: FAIL — the value position sees `BlockEntry`, which has no arm (falls to empty scalar, then the mapping loop errors).

- [ ] **Step 3: Add the `BlockEntry` arm and `parse_indentless_sequence`.** In `parse_node_inner`, add before the `_` catch-all:

```rust
            Some(TokenKind::BlockEntry) => self.parse_indentless_sequence(anchor, tag),
```

Add the method:

```rust
    /// Parses an indentless block sequence (bare `BlockEntry` tokens with no
    /// `BlockSequenceStart`/`BlockEnd`). It ends when the next token is not a
    /// `BlockEntry`.
    fn parse_indentless_sequence(
        &mut self,
        anchor: Option<String>,
        tag: Option<String>,
    ) -> Result<()> {
        let span = self.span();
        self.emit(EventKind::SequenceStart { anchor, tag }, span);
        loop {
            match self.peek() {
                Some(TokenKind::BlockEntry) => {
                    self.bump();
                    if matches!(
                        self.peek(),
                        Some(TokenKind::BlockEntry)
                            | Some(TokenKind::Key)
                            | Some(TokenKind::Value)
                            | Some(TokenKind::BlockEnd)
                    ) {
                        self.emit_empty_scalar();
                    } else {
                        self.parse_node()?;
                    }
                }
                _ => {
                    let end_span = self.span();
                    self.emit(EventKind::SequenceEnd, end_span);
                    return Ok(());
                }
            }
        }
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p yaml2 parser::`
Expected: all pass. (The enclosing mapping's value calls `parse_node` → sees `BlockEntry` → indentless sequence; it ends at the `BlockEnd` of the mapping, which the mapping loop then consumes.)

- [ ] **Step 5: Verify clippy**

Run: `cargo clippy -p yaml2 --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add yaml2/src/parser.rs
git commit -m "feat(yaml2): parse indentless block sequences"
```

---

## Task 7: Flow sequences

**Files:** modify `yaml2/src/parser.rs`.

- [ ] **Step 1: Write the failing tests** (add to the `tests` module)

```rust
    #[test]
    fn flow_sequence() {
        assert_eq!(
            kinds("[a, b]"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::SequenceStart { anchor: None, tag: None },
                EventKind::Scalar { value: "a".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::Scalar { value: "b".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::SequenceEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn flow_sequence_single_pair_mapping() {
        // `[a: b]` — the entry is a single-pair mapping.
        assert_eq!(
            kinds("[a: b]"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::SequenceStart { anchor: None, tag: None },
                EventKind::MappingStart { anchor: None, tag: None },
                EventKind::Scalar { value: "a".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::Scalar { value: "b".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::MappingEnd,
                EventKind::SequenceEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn empty_flow_sequence() {
        assert_eq!(
            kinds("[]"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::SequenceStart { anchor: None, tag: None },
                EventKind::SequenceEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p yaml2 parser::tests::flow_sequence`
Expected: FAIL — no `FlowSequenceStart` arm.

- [ ] **Step 3: Add the `FlowSequenceStart` arm and methods.** In `parse_node_inner`, add before the `_` catch-all:

```rust
            Some(TokenKind::FlowSequenceStart) => self.parse_flow_sequence(anchor, tag),
```

Add the methods:

```rust
    fn parse_flow_sequence(
        &mut self,
        anchor: Option<String>,
        tag: Option<String>,
    ) -> Result<()> {
        let start = self.bump(); // FlowSequenceStart
        self.emit(EventKind::SequenceStart { anchor, tag }, start.span);
        loop {
            match self.peek() {
                Some(TokenKind::FlowSequenceEnd) => {
                    let end = self.bump();
                    self.emit(EventKind::SequenceEnd, end.span);
                    return Ok(());
                }
                Some(TokenKind::FlowEntry) => {
                    self.bump(); // entry separator
                }
                None => return Err(self.error("unterminated flow sequence")),
                _ => self.parse_flow_sequence_entry()?,
            }
        }
    }

    /// A flow-sequence entry is a node, or a single-pair mapping (`a: b`) when
    /// a `Value` follows the entry's node.
    fn parse_flow_sequence_entry(&mut self) -> Result<()> {
        let mark = self.events.len();
        let span = self.span();
        self.parse_node()?;
        if matches!(self.peek(), Some(TokenKind::Value)) {
            // Wrap the just-parsed key node in a single-pair mapping.
            self.events.insert(
                mark,
                Event::new(EventKind::MappingStart { anchor: None, tag: None }, span),
            );
            self.bump(); // Value
            if matches!(
                self.peek(),
                Some(TokenKind::FlowEntry) | Some(TokenKind::FlowSequenceEnd)
            ) {
                self.emit_empty_scalar();
            } else {
                self.parse_node()?;
            }
            let end_span = self.span();
            self.emit(EventKind::MappingEnd, end_span);
        }
        Ok(())
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p yaml2 parser::`
Expected: all pass.

- [ ] **Step 5: Verify clippy**

Run: `cargo clippy -p yaml2 --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add yaml2/src/parser.rs
git commit -m "feat(yaml2): parse flow sequences with single-pair mappings"
```

---

## Task 8: Flow mappings

**Files:** modify `yaml2/src/parser.rs`.

- [ ] **Step 1: Write the failing tests** (add to the `tests` module)

```rust
    #[test]
    fn flow_mapping() {
        assert_eq!(
            kinds("{a: 1, b: 2}"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::MappingStart { anchor: None, tag: None },
                EventKind::Scalar { value: "a".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::Scalar { value: "1".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::Scalar { value: "b".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::Scalar { value: "2".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::MappingEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn flow_mapping_null_values() {
        // `{a, b}` — keys with implicit null values.
        assert_eq!(
            kinds("{a, b}"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::MappingStart { anchor: None, tag: None },
                EventKind::Scalar { value: "a".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::Scalar { value: "".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::Scalar { value: "b".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::Scalar { value: "".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::MappingEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn empty_flow_mapping() {
        assert_eq!(
            kinds("{}"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::MappingStart { anchor: None, tag: None },
                EventKind::MappingEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p yaml2 parser::tests::flow_mapping`
Expected: FAIL — no `FlowMappingStart` arm.

- [ ] **Step 3: Add the `FlowMappingStart` arm and methods.** In `parse_node_inner`, add before the `_` catch-all:

```rust
            Some(TokenKind::FlowMappingStart) => self.parse_flow_mapping(anchor, tag),
```

Add the methods:

```rust
    fn parse_flow_mapping(
        &mut self,
        anchor: Option<String>,
        tag: Option<String>,
    ) -> Result<()> {
        let start = self.bump(); // FlowMappingStart
        self.emit(EventKind::MappingStart { anchor, tag }, start.span);
        loop {
            match self.peek() {
                Some(TokenKind::FlowMappingEnd) => {
                    let end = self.bump();
                    self.emit(EventKind::MappingEnd, end.span);
                    return Ok(());
                }
                Some(TokenKind::FlowEntry) => {
                    self.bump(); // entry separator
                }
                None => return Err(self.error("unterminated flow mapping")),
                _ => self.parse_flow_mapping_entry()?,
            }
        }
    }

    /// A flow-mapping entry: an (implicit or explicit `?`) key, then an optional
    /// `Value` and value node (else an implicit null value).
    fn parse_flow_mapping_entry(&mut self) -> Result<()> {
        // Explicit-key indicator `?` is consumed if present.
        if matches!(self.peek(), Some(TokenKind::Key)) {
            self.bump();
        }
        // Key node (empty if a Value/separator/end follows directly).
        if matches!(
            self.peek(),
            Some(TokenKind::Value)
                | Some(TokenKind::FlowEntry)
                | Some(TokenKind::FlowMappingEnd)
        ) {
            self.emit_empty_scalar();
        } else {
            self.parse_node()?;
        }
        // Value node.
        if matches!(self.peek(), Some(TokenKind::Value)) {
            self.bump();
            if matches!(
                self.peek(),
                Some(TokenKind::FlowEntry) | Some(TokenKind::FlowMappingEnd)
            ) {
                self.emit_empty_scalar();
            } else {
                self.parse_node()?;
            }
        } else {
            // Key with no value indicator → implicit null value.
            self.emit_empty_scalar();
        }
        Ok(())
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p yaml2 parser::`
Expected: all pass.

- [ ] **Step 5: Verify clippy**

Run: `cargo clippy -p yaml2 --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add yaml2/src/parser.rs
git commit -m "feat(yaml2): parse flow mappings"
```

---

## Task 9: Anchors and tags on collections

`parse_node_inner` already collects `anchor`/`tag` and threads them into scalars and all collection starts. This task verifies anchors/tags on collections and re-adds the full alias test.

**Files:** modify `yaml2/src/parser.rs`.

- [ ] **Step 1: Write the tests** (add to the `tests` module; also update the Task-3 `alias_node` test to the flow version below)

```rust
    #[test]
    fn anchored_tagged_scalar() {
        assert_eq!(
            kinds("&a !t hello\n"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::Scalar {
                    value: "hello".to_string(),
                    style: ScalarStyle::Plain,
                    anchor: Some("a".to_string()),
                    tag: Some("!t".to_string()),
                },
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn anchored_block_sequence() {
        assert_eq!(
            kinds("&seq\n- a\n"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::SequenceStart { anchor: Some("seq".to_string()), tag: None },
                EventKind::Scalar { value: "a".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::SequenceEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn anchor_and_alias_in_flow_sequence() {
        assert_eq!(
            kinds("[&x a, *x]"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::SequenceStart { anchor: None, tag: None },
                EventKind::Scalar { value: "a".to_string(), style: ScalarStyle::Plain, anchor: Some("x".to_string()), tag: None },
                EventKind::Alias("x".to_string()),
                EventKind::SequenceEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p yaml2 parser::tests::anchored_tagged_scalar parser::tests::anchored_block_sequence parser::tests::anchor_and_alias_in_flow_sequence`
Expected: PASS already — `parse_node_inner` collects properties before dispatching and threads them into every collection start and the scalar. If `anchored_block_sequence` fails, confirm the `&seq\n- a` token stream has the `Anchor` token before `BlockSequenceStart`; the property loop consumes it, then the `BlockSequenceStart` arm receives it. Debug if needed; do not change expectations.

- [ ] **Step 3: Run the full suite + clippy**

Run: `cargo test -p yaml2`
Expected: all pass.

Run: `cargo clippy -p yaml2 --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add yaml2/src/parser.rs
git commit -m "test(yaml2): anchors and tags on collections and aliases"
```

---

## Task 10: Empty and null nodes

**Files:** modify `yaml2/src/parser.rs`.

- [ ] **Step 1: Write the tests** (add to the `tests` module)

```rust
    #[test]
    fn mapping_with_empty_value() {
        assert_eq!(
            kinds("a:\n"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::MappingStart { anchor: None, tag: None },
                EventKind::Scalar { value: "a".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::Scalar { value: "".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::MappingEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn sequence_with_empty_entry() {
        assert_eq!(
            kinds("-\n- a\n"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::SequenceStart { anchor: None, tag: None },
                EventKind::Scalar { value: "".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::Scalar { value: "a".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::SequenceEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn empty_document() {
        assert_eq!(
            kinds("---\n"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::Scalar { value: "".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p yaml2 parser::tests::mapping_with_empty_value parser::tests::sequence_with_empty_entry parser::tests::empty_document`
Expected: PASS already — `parse_block_mapping`/`parse_block_sequence` emit empty scalars for missing values/entries, and `parse_document_content` emits an empty scalar for an empty `---` document. If any fails, capture actual-vs-expected and debug the empty-node checks; do not change expectations.

- [ ] **Step 3: Run the full suite + clippy**

Run: `cargo test -p yaml2`
Expected: all pass.

Run: `cargo clippy -p yaml2 --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add yaml2/src/parser.rs
git commit -m "test(yaml2): empty values, entries, and documents"
```

---

## Task 11: Depth limit and malformed-stream rejection

**Files:** modify `yaml2/src/parser.rs`.

- [ ] **Step 1: Write the tests** (add to the `tests` module)

```rust
    #[test]
    fn deeply_nested_flow_exceeds_max_depth() {
        let opts = ParseOptions {
            limits: crate::options::Limits { max_depth: 16, ..Default::default() },
            ..Default::default()
        };
        let input = "[".repeat(50) + &"]".repeat(50);
        let err = parse_events(&input, &opts).unwrap_err();
        assert_eq!(err.kind(), crate::error::ErrorKind::LimitExceeded);
    }

    #[test]
    fn nesting_within_limit_is_ok() {
        let opts = ParseOptions {
            limits: crate::options::Limits { max_depth: 16, ..Default::default() },
            ..Default::default()
        };
        // 5 levels of flow sequence nesting, well within the limit.
        assert!(parse_events("[[[[[x]]]]]", &opts).is_ok());
    }

    #[test]
    fn malformed_double_colon_is_rejected() {
        // `a: b: c` yields a token stream with a mapping start followed by a
        // Value with no Key — a Parse error, not garbage events.
        let err = parse_events("a: b: c\n", &ParseOptions::default()).unwrap_err();
        assert_eq!(err.kind(), crate::error::ErrorKind::Parse);
    }
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p yaml2 parser::tests::deeply_nested_flow_exceeds_max_depth parser::tests::malformed_double_colon_is_rejected`
Expected: `deeply_nested_flow_exceeds_max_depth` should PASS (the `parse_node` depth check from Task 3 already enforces `max_depth`). `nesting_within_limit_is_ok` should PASS. `malformed_double_colon_is_rejected` — likely PASS already because `a: b: c`'s token stream puts a `Value` where `parse_block_mapping` expects a `Key`, hitting its `_ => Err(...)` arm. Run all three and confirm.

- [ ] **Step 3: Only if `malformed_double_colon_is_rejected` does NOT error**, inspect the actual token stream and events. The scanner produces (per the Plan 3 analysis) a `BlockMappingStart` followed eventually by a `Value` with no preceding `Key`. If the parser currently produces events instead of erroring, ensure the relevant collection loop's catch-all (`_ => Err(...)`) covers the unexpected token. The `parse_block_mapping`, `parse_block_sequence`, `parse_flow_sequence`, and `parse_flow_mapping` loops should each error on tokens they don't expect. Do NOT change the test to accept malformed output — make the parser reject it. If the scanner's actual output for `a: b: c` turns out to be well-formed (a nested mapping), then replace this test's input with a token stream that is genuinely malformed at the parser level (e.g. via a different construct) and report the finding; but first confirm what `a: b: c` actually tokenizes to.

- [ ] **Step 4: Remove any remaining `#[allow(dead_code)]`** on `depth`/`max_depth` (they are now exercised by tests). Run `cargo clippy -p yaml2 --all-targets -- -D warnings` — clean.

- [ ] **Step 5: Run the full suite**

Run: `cargo test -p yaml2`
Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add yaml2/src/parser.rs
git commit -m "feat(yaml2): enforce max_depth and reject malformed token streams"
```

---

## Task 12: Integration and full verification

**Files:** modify `yaml2/src/parser.rs`.

- [ ] **Step 1: Write the integration tests** (add to the `tests` module)

```rust
    #[test]
    fn realistic_document_events() {
        let input = "name: Ada\njobs:\n  - lang: rust\n    years: 3\n  - lang: yaml\n";
        assert_eq!(
            kinds(input),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::MappingStart { anchor: None, tag: None },
                EventKind::Scalar { value: "name".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::Scalar { value: "Ada".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::Scalar { value: "jobs".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::SequenceStart { anchor: None, tag: None },
                EventKind::MappingStart { anchor: None, tag: None },
                EventKind::Scalar { value: "lang".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::Scalar { value: "rust".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::Scalar { value: "years".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::Scalar { value: "3".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::MappingEnd,
                EventKind::MappingStart { anchor: None, tag: None },
                EventKind::Scalar { value: "lang".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::Scalar { value: "yaml".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::MappingEnd,
                EventKind::SequenceEnd,
                EventKind::MappingEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn block_scalar_value_event() {
        assert_eq!(
            kinds("text: |\n  a\n  b\n"),
            vec![
                EventKind::StreamStart,
                EventKind::DocumentStart,
                EventKind::MappingStart { anchor: None, tag: None },
                EventKind::Scalar { value: "text".to_string(), style: ScalarStyle::Plain, anchor: None, tag: None },
                EventKind::Scalar { value: "a\nb\n".to_string(), style: ScalarStyle::Literal, anchor: None, tag: None },
                EventKind::MappingEnd,
                EventKind::DocumentEnd,
                EventKind::StreamEnd,
            ]
        );
    }
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p yaml2 parser::tests::realistic_document_events parser::tests::block_scalar_value_event`
Expected: both pass (assembled behavior). The `realistic_document_events` test covers a mapping containing an indentless... no — here `jobs:` is followed by `  - ` at column 2 (indented), so it's a regular block sequence with `BlockSequenceStart`/`BlockEnd`. Confirm the event stream matches. If it fails, capture actual-vs-expected and debug; do not change expectations.

- [ ] **Step 3: Run the full crate suite**

Run: `cargo test -p yaml2`
Expected: all foundation + scanner + parser tests pass.

- [ ] **Step 4: Verify clippy and formatting**

Run: `cargo clippy -p yaml2 --all-targets -- -D warnings`
Expected: clean.

Run: `cargo fmt` then `cargo fmt --check`
Expected: no diff. Confirm `git status --short` is clean after the commit.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "test(yaml2): integration tests for the event parser"
```

---

## Self-Review

**Spec coverage (this plan's portion — the parser layer):**
- Event parser as the pipeline stage `tokens → events` — all tasks. ✔
- Public `Event`/`EventKind` streaming model + `Parser::next_event` pull API + `parse_events` — Tasks 1, 2. ✔
- Stream/document framing, including implicit and explicit (`---`/`...`) documents and multi-document streams — Tasks 2, 3. ✔
- Scalars (with style + anchor + tag), aliases — Tasks 3, 9. ✔
- Block sequences, block mappings, indentless sequences — Tasks 4, 5, 6. ✔
- Flow sequences (incl. single-pair `[a: b]` mappings), flow mappings (incl. `{a, b}` null values, `?` explicit keys) — Tasks 7, 8. ✔
- Anchors/tags on collections — Task 9. ✔
- Empty/null nodes (empty value, empty entry, empty document) — Task 10. ✔
- `max_depth` enforcement (tracked from Plan 3) — Tasks 3, 11. ✔
- Malformed token-stream rejection (tracked from Plan 3) — Task 11. ✔
- Source spans on events — every `emit` carries a span. ✔
- Pure Rust, no `unsafe`. ✔

**Deferred to later plans (correctly out of scope):** the composer (`events → Value` tree) — Plan 6; tag *resolution* (events carry raw tag text) — composer/Plan 6; alias *resolution* and the alias-expansion (`max_aliases`) limit — composer (Plan 6), since cycle/expansion detection happens when building the tree; emitter — Plan 7; the `yaml-test-suite` event-comparison harness — Plan 9 (the event stream is the natural comparison point, so the suite slots in there). True lazy/streaming parsing (currently eager) — possible later optimization, no API change.

**Placeholder scan:** No "TBD/TODO". Task 2's `parse_node` is an explicit temporary stub replaced in Task 3 (Task 3's tests fail against the stub first). Task 3 notes replacing the Task-3 `alias_node` test with the flow version in Task 9. ✔

**Type consistency:** `Event { kind: EventKind, span: Span }` and `EventKind` variants (Task 1) are used identically throughout. `parse_events(input, &ParseOptions) -> Result<Vec<Event>>` and `Parser::new`/`next_event` (Task 2) are stable. `ParserState` helpers (`peek`/`span`/`bump`/`emit`/`emit_empty_scalar`/`error`) defined in Task 2 are used by every parse method. The dispatch methods — `parse_node`/`parse_node_inner` (Task 3), `parse_block_sequence` (4), `parse_block_mapping` (5), `parse_indentless_sequence` (6), `parse_flow_sequence`/`parse_flow_sequence_entry` (7), `parse_flow_mapping`/`parse_flow_mapping_entry` (8) — have stable signatures; the `parse_node_inner` match gains one arm per collection task before its `_` catch-all. Scanner items (`tokenize`, `Token`, `TokenKind`) are `pub(crate)` and imported once (Task 2). `ScalarStyle` variants (Plain/Literal/Folded/SingleQuoted/DoubleQuoted) come from the foundation.

**Execution note:** The single-pair flow-sequence mapping (Task 7, `[a: b]`) uses event-buffer `insert` to place `MappingStart` before the already-emitted key — the one non-linear move; the test pins the exact event order. Tasks 9 and 10 are largely verification (the property-collection and empty-node logic was built into earlier tasks), so several of their tests may pass on first run — that is expected and noted.
