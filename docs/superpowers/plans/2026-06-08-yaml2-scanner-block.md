# yaml2 Scanner (Block Structure) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend the `yaml2` scanner with indentation-driven block structure — block sequences (`- item`), block mappings (`key: value`), nesting, and dedent — by re-architecting it into a token-queue + simple-key + indentation-stack model.

**Architecture:** Plan 3 of ~9 for `yaml2` (see `docs/superpowers/specs/2026-06-08-yaml2-parser-design.md`). The flow scanner (Plan 2) is forward-only: one token per `next_token` call. Block mappings break that model — when the scanner reaches `:`, it must insert `Key` (and possibly `BlockMappingStart`) tokens *before* the key scalar it already produced. So this plan introduces a **token queue** (`VecDeque<Token>`) drained by `next_token`, a **simple-key** record (a buffered "this scalar might be a mapping key" marker), and an **indentation stack** that emits `BlockMappingStart`/`BlockSequenceStart`/`BlockEnd` as indentation rises and falls. This is the libyaml scanner model, scoped to single-line block keys.

**Tech Stack:** Rust 2021, `std::collections::VecDeque`. Builds on Plan 2 (`Reader`, `Token`/`TokenKind`, flow scanning). No new dependencies. No `unsafe`.

---

## Context for the engineer

After Plan 2, `yaml2/src/scanner/mod.rs` has a `Scanner` whose `next_token(&mut self) -> Result<Option<Token>>` scans one token per call: it emits `StreamStart`, then on each call runs `skip_to_next_token` (skips spaces/tabs/newlines/comments) and `scan_content` (dispatches on the first char to flow/scalar/marker scanners), then `StreamEnd`, then `None`. It tracks `flow_depth` (nesting inside `[`/`{`). All scalar/flow scanning works. **94+ flow tests pass.**

This plan adds **block structure**, which only applies *outside* flow (`flow_depth == 0`):

- **Block sequence:** lines like `- a` / `- b`. Tokens: `BlockSequenceStart`, then per item `BlockEntry` + the item's node tokens, then `BlockEnd` on dedent.
- **Block mapping:** lines like `key: value`. Tokens: `BlockMappingStart`, then per pair `Key` + key-node tokens + `Value` + value-node tokens, then `BlockEnd` on dedent.
- **Nesting** via indentation: deeper indent opens a child collection; shallower indent closes collections (emitting `BlockEnd`).

### Why a queue + simple keys

Consider `name: Ada`. The scanner reads the plain scalar `name` and produces a `Scalar` token. *Then* it sees `:` and realizes `name` was a mapping key — but the `Scalar` token is already produced. The parser needs `BlockMappingStart, Key, Scalar(name), Value, Scalar(Ada)`. So the scanner records, when it produces a scalar at a position where a key is possible, a **simple key**: `{ token_number, mark }`. When `:` arrives on the same line, it **inserts** `Key` (and `BlockMappingStart` if this opens a new mapping) into the queue at the recorded position. `token_number` is the scalar's index in the overall token stream; the insertion index into the current queue is `token_number - tokens_parsed` (where `tokens_parsed` counts tokens already dequeued by `next_token`).

### Indentation model

`indent: i64` is the current block indentation as a **0-based column** (`column - 1`); `-1` means "root / none". `indents: Vec<i64>` is the stack of enclosing indents. `roll_indent(col)` opens a level (push old `indent`, set `indent = col`, emit a `Block*Start`). `unroll_indent(col)` closes levels while `indent > col` (emit `BlockEnd`, pop). Indentation is processed at the **start of each line** (after leading spaces) and at EOF (unroll to `-1`).

### Out of scope (Plan 4 and later)

Block scalars `|`/`>`, multi-line plain/quoted folding (so a plain scalar still ends at the line break — a mapping value or sequence item is single-line here), explicit `?` complex keys spanning lines, `%YAML`/`%TAG` directives. Keys are single-line plain/quoted/flow scalars. Multi-line keys are rejected (the simple key goes stale at the line break).

### Conventions
- `flow_depth > 0` ⇒ **flow context**: block logic is inert (indentation insignificant), existing Plan 2 behavior unchanged.
- A "block-level" token is one produced while `flow_depth == 0`.
- 0-based column for indentation math: `col0(pos) = pos.column - 1`.

---

## File structure

| File | Change |
|------|--------|
| `yaml2/src/scanner/token.rs` | Add `BlockSequenceStart`, `BlockMappingStart`, `BlockEnd`, `BlockEntry` variants |
| `yaml2/src/scanner/mod.rs` | Re-architect `Scanner` to a token queue; add simple-key + indentation machinery; block dispatch |

The `Scanner` grows; that's inherent to the algorithm. If `mod.rs` becomes unwieldy after this plan, a follow-up split (e.g. `scanner/block.rs`) can be considered, but do not split mid-plan.

---

## Task 1: Add block token variants

**Files:** modify `yaml2/src/scanner/token.rs`.

- [ ] **Step 1: Write the failing test** (add to the `tests` module in `token.rs`)

```rust
    #[test]
    fn block_token_kinds_exist() {
        let span = Span::new(Position::new(0, 1, 1), Position::new(0, 1, 1));
        // These must construct and compare.
        assert_eq!(
            Token::new(TokenKind::BlockSequenceStart, span).kind,
            TokenKind::BlockSequenceStart
        );
        assert_eq!(Token::new(TokenKind::BlockMappingStart, span).kind, TokenKind::BlockMappingStart);
        assert_eq!(Token::new(TokenKind::BlockEnd, span).kind, TokenKind::BlockEnd);
        assert_eq!(Token::new(TokenKind::BlockEntry, span).kind, TokenKind::BlockEntry);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p yaml2 scanner::token`
Expected: FAIL — no variant `BlockSequenceStart`.

- [ ] **Step 3: Add the variants** to the `TokenKind` enum in `token.rs` (insert after `DocumentEnd`, before `FlowSequenceStart`, so block and document structure group together)

```rust
    /// Start of an indentation-based block sequence.
    BlockSequenceStart,
    /// Start of an indentation-based block mapping.
    BlockMappingStart,
    /// End of a block sequence or mapping (one per opened block level).
    BlockEnd,
    /// A block sequence entry indicator (`-`).
    BlockEntry,
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p yaml2 scanner::token`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add yaml2/src/scanner/token.rs
git commit -m "feat(yaml2): add block-structure token variants"
```

---

## Task 2: Re-architect the scanner to a token queue

This is a **pure refactor**: convert the one-token-per-call scanner into a queue-draining model, preserving every existing behavior. All existing scanner tests must still pass with zero changes to their expectations.

**Files:** modify `yaml2/src/scanner/mod.rs`.

- [ ] **Step 1: Replace the `Scanner` struct and `new`** with the queue-based fields (add `use std::collections::VecDeque;` at the top of `mod.rs` with the other imports)

```rust
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
```

- [ ] **Step 2: Replace `next_token`** with the queue-draining version

```rust
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
```

- [ ] **Step 3: Add `fetch_more_tokens`** — this replaces the body of the old `next_token`. It enqueues at least one token per call. For now it preserves the exact Plan 2 behavior (flow/scalar scanning); block handling is layered on in later tasks.

```rust
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
            self.tokens.push_back(Token::new(TokenKind::StreamStart, Span::new(pos, pos)));
            return Ok(());
        }

        self.skip_to_next_token();
        let start = self.reader.position();
        match self.reader.peek() {
            None => {
                self.tokens.push_back(Token::new(TokenKind::StreamEnd, Span::new(start, start)));
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
```

- [ ] **Step 4: Delete the old `next_token` body**. The old `next_token` (the one with `if !self.started { ... }` directly returning tokens) is fully replaced by Steps 2–3. Ensure only the new `next_token` and `fetch_more_tokens` remain. Keep `skip_to_next_token`, `scan_content`, and all `scan_*` helpers exactly as they are. The `tokenize` free function is unchanged.

Note: `scan_content`'s flow arms still mutate `self.flow_depth` for `[`/`]`/`{`/`}` and the `:` arm still checks `self.flow_depth > 0 || self.indicator_terminator_next()`. Leave all of that intact.

- [ ] **Step 5: Run the full scanner suite**

Run: `cargo test -p yaml2`
Expected: ALL existing tests pass unchanged (this is a behavior-preserving refactor). If any flow test fails, the refactor changed behavior — fix the refactor, do not change test expectations.

- [ ] **Step 6: Verify clippy**

Run: `cargo clippy -p yaml2 --all-targets -- -D warnings`
Expected: clean. (The new `indent`/`indents`/`simple_key`/`simple_key_allowed` fields and `SimpleKey` are not yet read; the module-level `#![allow(dead_code)]` covers that.)

- [ ] **Step 7: Commit**

```bash
git add yaml2/src/scanner/mod.rs
git commit -m "refactor(yaml2): drain scanner output through a token queue"
```

---

## Task 3: Block line tracking and indentation unroll

Introduce block-context awareness into the whitespace skip: in block context, track when we're at the start of a line and run `unroll_indent`/EOF-unroll. Bare scalars and flow input keep working.

**Files:** modify `yaml2/src/scanner/mod.rs`.

- [ ] **Step 1: Write the failing tests** (add to the `tests` module in `mod.rs`)

```rust
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
        // With no block constructs yet, a plain scalar then EOF must not emit
        // spurious BlockEnd tokens (indent stays at root).
        let toks = tokenize("hello\n", Limits::default()).unwrap();
        assert!(!toks.iter().any(|t| t.kind == TokenKind::BlockEnd));
    }
```

- [ ] **Step 2: Run the tests to verify they pass already (regression guard)**

Run: `cargo test -p yaml2 scanner::tests::bare_scalar_document_still_works scanner::tests::block_indent_helpers_unroll_to_root_at_eof`
Expected: PASS (these document current behavior before the indent machinery is wired; they guard against regressions in later steps).

- [ ] **Step 3: Add the indentation helpers and line-start tracking** to `mod.rs`.

First, add helper methods to the `impl Scanner`:

```rust
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
            self.tokens.push_back(Token::new(TokenKind::BlockEnd, Span::new(pos, pos)));
            self.indent = self.indents.pop().unwrap_or(-1);
        }
    }

    /// Opens a new block level at `col`, inserting `start_kind` at queue index
    /// `at` (or appending if `None`). Returns true if a level was opened.
    /// Inert in flow context.
    fn roll_indent(&mut self, col: i64, start_kind: TokenKind, mark: Position, at: Option<usize>) -> bool {
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
```

- [ ] **Step 4: Make `fetch_more_tokens` do block line-start processing.** Replace the post-`started` portion of `fetch_more_tokens` (everything after the `if !self.started { ... }` block) with:

```rust
        self.skip_to_next_token();

        // Block context: at the start of a line we process indentation.
        if self.flow_depth == 0 {
            self.unroll_indent(Self::col0(self.reader.position()));
        }

        let start = self.reader.position();
        match self.reader.peek() {
            None => {
                if self.flow_depth == 0 {
                    self.unroll_indent(-1);
                }
                self.tokens.push_back(Token::new(TokenKind::StreamEnd, Span::new(start, start)));
                self.stream_end_produced = true;
                Ok(())
            }
            Some(c) => {
                let token = self.scan_content(c, start)?;
                self.tokens.push_back(token);
                Ok(())
            }
        }
```

Note: `skip_to_next_token` still skips newlines (so for the common single-token-per-line case the column after skipping is the next content's column). Because `indent` is still `-1` everywhere (nothing calls `roll_indent` yet), `unroll_indent` is a no-op in this task — these calls become live once Tasks 4–5 push indents. The EOF `unroll_indent(-1)` ensures any open blocks close before `StreamEnd`.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p yaml2`
Expected: all pass (behavior unchanged — `indent` never rises yet).

- [ ] **Step 6: Verify clippy**

Run: `cargo clippy -p yaml2 --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add yaml2/src/scanner/mod.rs
git commit -m "feat(yaml2): add block indentation stack and unroll processing"
```

---

## Task 4: Block sequences

Recognize `- ` (dash followed by space, tab, or line break) at block level as a sequence entry: open a `BlockSequenceStart` on a deeper indent, emit `BlockEntry`, and close with `BlockEnd` on dedent.

**Files:** modify `yaml2/src/scanner/mod.rs`.

- [ ] **Step 1: Write the failing tests** (add to the `tests` module in `mod.rs`)

```rust
    #[test]
    fn simple_block_sequence() {
        assert_eq!(
            kinds("- a\n- b\n"),
            vec![
                TokenKind::StreamStart,
                TokenKind::BlockSequenceStart,
                TokenKind::BlockEntry,
                TokenKind::Scalar { value: "a".to_string(), style: ScalarStyle::Plain },
                TokenKind::BlockEntry,
                TokenKind::Scalar { value: "b".to_string(), style: ScalarStyle::Plain },
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
                TokenKind::Scalar { value: "a".to_string(), style: ScalarStyle::Plain },
                TokenKind::BlockEnd,
                TokenKind::BlockEnd,
                TokenKind::StreamEnd,
            ]
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p yaml2 scanner::tests::simple_block_sequence`
Expected: FAIL — `-` is currently scanned as a plain scalar.

- [ ] **Step 3: Add a block-entry arm to `scan_content`.** The `-` must be recognized as a block entry only at block level (`flow_depth == 0`) when followed by a space/tab/line-break/EOF (otherwise it's a plain scalar like `-5`). Add this arm to `scan_content` **before** the `_ => self.scan_plain(start)` arm (and after the document-marker arms):

```rust
            '-' if self.flow_depth == 0 && self.block_entry_next() => {
                self.fetch_block_entry(start)
            }
```

Add the helper and fetcher methods:

```rust
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
        // Opening a sequence at a deeper indent emits BlockSequenceStart first.
        self.roll_indent(col, TokenKind::BlockSequenceStart, start, None);
        self.reader.advance(); // consume '-'
        self.simple_key_allowed = true;
        Ok(Token::new(TokenKind::BlockEntry, Span::new(start, self.reader.position())))
    }
```

Note on ordering: `scan_content` returns exactly one token, but `fetch_block_entry` may also have pushed a `BlockSequenceStart` into the queue via `roll_indent`. Because `roll_indent` appends with `at == None`, the `BlockSequenceStart` is pushed to the queue *before* `fetch_more_tokens` pushes the returned `BlockEntry`. Wait — `scan_content` returns the `BlockEntry` and the caller (`fetch_more_tokens`) pushes it *after* the `BlockSequenceStart` already enqueued by `roll_indent`. So queue order is `[BlockSequenceStart, BlockEntry]`. Correct.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p yaml2 scanner::tests`
Expected: `simple_block_sequence` and `nested_block_sequence` pass; all prior tests still pass. (`nested_block_sequence` works because `- - a` opens two sequence levels at columns 0 and 2, and EOF unrolls both.)

- [ ] **Step 5: Verify clippy**

Run: `cargo clippy -p yaml2 --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add yaml2/src/scanner/mod.rs
git commit -m "feat(yaml2): scan block sequences with indentation"
```

---

## Task 5: Block mappings with inline values (simple keys)

Recognize `key: value` at block level. When a block-level scalar is produced, record a simple key; when `:` is found on the same line at block level, insert `Key` (and `BlockMappingStart` if opening a new mapping) before the key scalar and emit `Value`.

**Files:** modify `yaml2/src/scanner/mod.rs`.

- [ ] **Step 1: Write the failing tests** (add to the `tests` module in `mod.rs`)

```rust
    #[test]
    fn simple_block_mapping() {
        assert_eq!(
            kinds("key: value\n"),
            vec![
                TokenKind::StreamStart,
                TokenKind::BlockMappingStart,
                TokenKind::Key,
                TokenKind::Scalar { value: "key".to_string(), style: ScalarStyle::Plain },
                TokenKind::Value,
                TokenKind::Scalar { value: "value".to_string(), style: ScalarStyle::Plain },
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
                TokenKind::Scalar { value: "a".to_string(), style: ScalarStyle::Plain },
                TokenKind::Value,
                TokenKind::Scalar { value: "1".to_string(), style: ScalarStyle::Plain },
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
    fn quoted_key_block_mapping() {
        assert_eq!(
            kinds("\"k\": v\n"),
            vec![
                TokenKind::StreamStart,
                TokenKind::BlockMappingStart,
                TokenKind::Key,
                TokenKind::Scalar { value: "k".to_string(), style: ScalarStyle::DoubleQuoted },
                TokenKind::Value,
                TokenKind::Scalar { value: "v".to_string(), style: ScalarStyle::Plain },
                TokenKind::BlockEnd,
                TokenKind::StreamEnd,
            ]
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p yaml2 scanner::tests::simple_block_mapping`
Expected: FAIL — currently `key` is a scalar, `:` is value-or-plain, no `BlockMappingStart`/`Key`.

- [ ] **Step 3: Record a simple key when producing a block-level node.** Add a `save_simple_key` helper and call it from `fetch_more_tokens` *before* pushing a content token, when the token is a scalar/anchor/alias/tag at block level. The cleanest place is in `fetch_more_tokens`: capture whether a simple key should be saved based on the upcoming char.

Add the helper:

```rust
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
```

Modify `fetch_more_tokens`'s `Some(c)` arm so that, at block level, scalar/anchor/alias/tag starts save a simple key first. Replace the `Some(c) => { ... }` arm with:

```rust
            Some(c) => {
                if self.flow_depth == 0 && Self::can_start_simple_key(c) {
                    self.save_simple_key(start);
                }
                let token = self.scan_content(c, start)?;
                self.tokens.push_back(token);
                Ok(())
            }
```

Add the classifier (a node that could be a mapping key starts with a scalar/anchor/alias/tag char — not a structural indicator):

```rust
    /// Whether `c` begins a node that could serve as a block mapping key
    /// (scalar, anchor, alias, or tag — i.e. not a structural indicator).
    fn can_start_simple_key(c: char) -> bool {
        !matches!(c, '-' | '?' | ':' | ',' | '[' | ']' | '{' | '}' | '#')
    }
```

Note: `'&'`, `'*'`, `'!'`, `'\''`, `'"'`, and plain-scalar starts all pass `can_start_simple_key` and so save a simple key. Structural chars do not. (`-` at block level is a block entry; `:`/`?` are indicators.)

- [ ] **Step 4: Handle `:` at block level by resolving the simple key.** Currently `scan_content`'s `:` arm is:

```rust
            ':' if self.flow_depth > 0 || self.indicator_terminator_next() => {
                Ok(self.single_char(TokenKind::Value, start))
            }
```

Replace it with a version that, at block level, resolves a buffered simple key by inserting `Key`/`BlockMappingStart`:

```rust
            ':' if self.flow_depth > 0 || self.indicator_terminator_next() => {
                if self.flow_depth == 0 {
                    self.fetch_block_value(start)
                } else {
                    Ok(self.single_char(TokenKind::Value, start))
                }
            }
```

Add the fetcher:

```rust
    /// Handles `:` in block context: converts a buffered simple key into a
    /// `Key` token (opening a `BlockMappingStart` if needed) and emits `Value`.
    fn fetch_block_value(&mut self, start: Position) -> Result<Token> {
        if let Some(key) = self.simple_key.take() {
            // Insertion index into the current queue for the key's position.
            let index = key.token_number - self.tokens_parsed;
            let mut at = index;
            // Open a mapping at the key's column if this starts a new one.
            if self.roll_indent(Self::col0(key.mark), TokenKind::BlockMappingStart, key.mark, Some(at)) {
                at += 1;
            }
            self.tokens
                .insert(at, Token::new(TokenKind::Key, Span::new(key.mark, key.mark)));
            // A value indicator forbids another simple key until the next line.
            self.simple_key_allowed = false;
        } else {
            // `:` with no buffered key — a mapping entry with an empty/complex
            // key. Open a mapping here and let the (absent) key be implicit.
            self.roll_indent(Self::col0(start), TokenKind::BlockMappingStart, start, None);
            self.simple_key_allowed = true;
        }
        self.reader.advance(); // consume ':'
        Ok(Token::new(TokenKind::Value, Span::new(start, self.reader.position())))
    }
```

- [ ] **Step 5: Reset `simple_key_allowed` appropriately at line breaks.** A new simple key is allowed at the start of each line. In `skip_to_next_token`, when in block context and a line break is consumed, set `self.simple_key_allowed = true`. Modify the whitespace arm of `skip_to_next_token` so newlines flip the flag (only matters in block context):

Replace the body of `skip_to_next_token` with:

```rust
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
```

- [ ] **Step 6: Run the tests**

Run: `cargo test -p yaml2 scanner::tests`
Expected: the three new mapping tests pass; all prior tests (flow, sequences) still pass.

Reasoning to confirm `key: value`: `fetch_more_tokens` saves a simple key at `key` (token_number = 1, since StreamStart was token 0 and already dequeued? No — StreamStart is still in the queue until dequeued). Trace: after StreamStart is produced and dequeued (`tokens_parsed = 1`), `fetch_more_tokens` runs for `key`: `save_simple_key` records `token_number = tokens_parsed(1) + tokens.len(0) = 1`; pushes `Scalar(key)` (queue = [Scalar], this is token index 1). `next_token` returns `Scalar`? No — `next_token` loops: after fetch, it pops `Scalar(key)`... but we need `BlockMappingStart, Key` BEFORE it. They get inserted when `:` is scanned, which happens on a *later* fetch. **Problem:** `Scalar(key)` would be dequeued before `:` is seen.

**FIX (important):** the simple-key insertion must happen before the key scalar is dequeued. Because `next_token` dequeues as soon as the queue is non-empty, we must not let the key scalar leave the queue before its `:` is processed. Solution: when a simple key is *possible/pending*, `next_token` must keep fetching until the key is resolved (or goes stale) before returning the buffered key token. Implement by having `next_token` not return a token while `self.simple_key` is `Some` AND the front of the queue is at or before the pending key's index. The robust libyaml approach: `next_token` only returns a token when "the token is ready" — i.e. no pending simple key could still insert before it.

Implement this guard. Replace `next_token` with:

```rust
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
        match &self.simple_key {
            // A pending key might still insert at/before the front token index.
            Some(key) => key.token_number > self.tokens_parsed,
            None => true,
        }
    }
```

Wait — `key.token_number > self.tokens_parsed` means the key hasn't been dequeued yet, so we must hold. That blocks ALL output while a key is pending, which is correct: we hold the scalar until `:` resolves it or it goes stale. But we must also ensure fetching continues (it does: the loop calls `fetch_more_tokens`). And staleness (Task 7) will clear `simple_key` at the line break so a key that never gets a `:` is released.

Re-trace `key: value` with the guard:
1. `next_token` → queue empty, not ended → `fetch_more_tokens`: StreamStart pushed. `token_ready`: simple_key None → true → return StreamStart (`tokens_parsed=1`).
2. `next_token` → queue empty → fetch: saves simple_key{token_number=1}, pushes Scalar(key) [queue=[Scalar@idx1]]. `token_ready`: simple_key Some, token_number(1) > tokens_parsed(1)? No (1>1 false) → NOT ready. Loop. Not ended → fetch again: `skip_to_next_token` (no newline; consumes nothing, we're at `:`), block `:` → `fetch_block_value`: simple_key taken, index = 1 - 1 = 0; roll_indent(col0=0) opens mapping → insert BlockMappingStart at 0, at=1; insert Key at 1; queue=[BMS, Key, Scalar(key)]; consume `:`; returns Value → pushed: queue=[BMS,Key,Scalar(key),Value]. simple_key now None. `token_ready`: None → true → return BMS. Then Key, Scalar(key), Value drain.
3. Next fetch scans ` value` → Scalar(value). etc. EOF → unroll_indent(-1) emits BlockEnd, then StreamEnd.

Result: StreamStart, BlockMappingStart, Key, Scalar(key), Value, Scalar(value), BlockEnd, StreamEnd. Correct.

- [ ] **Step 7: Run the full suite**

Run: `cargo test -p yaml2`
Expected: all pass.

- [ ] **Step 8: Verify clippy**

Run: `cargo clippy -p yaml2 --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 9: Commit**

```bash
git add yaml2/src/scanner/mod.rs
git commit -m "feat(yaml2): scan block mappings via simple keys"
```

---

## Task 6: Nested block collections and values on following lines

Support a mapping value that is itself a block collection on the following (more-indented) lines, and mappings nested in sequences and vice-versa.

**Files:** modify `yaml2/src/scanner/mod.rs`.

- [ ] **Step 1: Write the failing tests** (add to the `tests` module in `mod.rs`)

```rust
    #[test]
    fn nested_mapping_value_on_following_lines() {
        assert_eq!(
            kinds("outer:\n  inner: v\n"),
            vec![
                TokenKind::StreamStart,
                TokenKind::BlockMappingStart,
                TokenKind::Key,
                TokenKind::Scalar { value: "outer".to_string(), style: ScalarStyle::Plain },
                TokenKind::Value,
                TokenKind::BlockMappingStart,
                TokenKind::Key,
                TokenKind::Scalar { value: "inner".to_string(), style: ScalarStyle::Plain },
                TokenKind::Value,
                TokenKind::Scalar { value: "v".to_string(), style: ScalarStyle::Plain },
                TokenKind::BlockEnd,
                TokenKind::BlockEnd,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn sequence_under_mapping_key() {
        assert_eq!(
            kinds("items:\n- a\n- b\n"),
            vec![
                TokenKind::StreamStart,
                TokenKind::BlockMappingStart,
                TokenKind::Key,
                TokenKind::Scalar { value: "items".to_string(), style: ScalarStyle::Plain },
                TokenKind::Value,
                TokenKind::BlockSequenceStart,
                TokenKind::BlockEntry,
                TokenKind::Scalar { value: "a".to_string(), style: ScalarStyle::Plain },
                TokenKind::BlockEntry,
                TokenKind::Scalar { value: "b".to_string(), style: ScalarStyle::Plain },
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
                TokenKind::Scalar { value: "k".to_string(), style: ScalarStyle::Plain },
                TokenKind::Value,
                TokenKind::Scalar { value: "v".to_string(), style: ScalarStyle::Plain },
                TokenKind::BlockEnd,
                TokenKind::BlockEnd,
                TokenKind::StreamEnd,
            ]
        );
    }
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p yaml2 scanner::tests::nested_mapping_value_on_following_lines scanner::tests::sequence_under_mapping_key scanner::tests::mapping_in_sequence_entry`
Expected: Most or all may **already pass** thanks to the indent stack built in Tasks 3–5. Run them and see.

Reasoning: `outer:\n  inner: v` — `outer` saves a key at col 0; `:` opens mapping at indent 0, emits Key/Value; newline sets simple_key_allowed; `  inner` is at col 2 — `save_simple_key` records it; `:` → `fetch_block_value`: roll_indent(col0=2) opens a nested BlockMappingStart (indent 0 < 2); Key/Value; `v` scalar; EOF unrolls 2→0→-1 emitting two BlockEnd. Correct. The other two cases follow the same indent logic.

- [ ] **Step 3: Only if a test fails, fix the indentation logic.** The most likely gap is `mapping_in_sequence_entry` (`- k: v`): after `BlockEntry` at col 0, `k` begins at col 2. `save_simple_key` must run for `k` — confirm `fetch_block_entry` left `simple_key_allowed = true` (it does). When `:` resolves, `roll_indent(col0=2)` opens the mapping at indent 2 (seq is at indent 0). On EOF, unroll 2→0→-1 emits BlockEnd (mapping) then BlockEnd (sequence). If the observed output differs, debug `roll_indent`/`unroll_indent` column math; do NOT change the test expectation (the expectations above are correct YAML token streams). If genuinely stuck, report DONE_WITH_CONCERNS with the actual vs expected token stream.

- [ ] **Step 4: Run the full suite**

Run: `cargo test -p yaml2`
Expected: all pass.

- [ ] **Step 5: Verify clippy**

Run: `cargo clippy -p yaml2 --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add yaml2/src/scanner/mod.rs
git commit -m "test(yaml2): nested block collections and following-line values"
```

---

## Task 7: Stale simple keys and structural errors

A simple key that never gets a `:` on its own line must be released (the node was just a value, e.g. a bare scalar or sequence item). Also enforce the single-line-key rule.

**Files:** modify `yaml2/src/scanner/mod.rs`.

- [ ] **Step 1: Write the failing tests** (add to the `tests` module in `mod.rs`)

```rust
    #[test]
    fn bare_scalar_is_not_a_key() {
        // A lone scalar with no colon must be released as a plain value, not
        // held forever waiting for a ':' (which would hang or drop it).
        assert_eq!(
            kinds("hello\nworld\n"),
            vec![
                TokenKind::StreamStart,
                TokenKind::Scalar { value: "hello".to_string(), style: ScalarStyle::Plain },
                TokenKind::Scalar { value: "world".to_string(), style: ScalarStyle::Plain },
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
                TokenKind::Scalar { value: "a".to_string(), style: ScalarStyle::Plain },
                TokenKind::BlockEntry,
                TokenKind::Scalar { value: "b".to_string(), style: ScalarStyle::Plain },
                TokenKind::BlockEnd,
                TokenKind::StreamEnd,
            ]
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail (hang risk)**

Run: `cargo test -p yaml2 scanner::tests::bare_scalar_is_not_a_key`
Expected: FAIL — without staleness, `token_ready` holds `hello` forever (the pending simple key is never cleared), so `tokenize` loops fetching until EOF then... actually it would eventually hit EOF and `stream_end_produced`, but `token_ready` stays false because `simple_key` is still `Some`. This would drop buffered tokens or loop. The test exposes it. (If it hangs, the staleness fix in Step 3 resolves it; the test harness has a default timeout.)

NOTE: `sequence_item_scalar_is_not_held_as_key` is the same `- a\n- b` from Task 4; re-asserting it here guards that adding staleness didn't regress sequences.

- [ ] **Step 3: Add simple-key staleness.** A buffered simple key is stale once we leave its line. Add a `stale_simple_key` check and call it at the start of `fetch_more_tokens` (after `started`), and clear the key when its line is left.

Add the helper:

```rust
    /// Releases a buffered simple key once scanning has moved past its line.
    /// (Single-line key rule: a block key must be followed by `:` on its own
    /// line.) The held node is thereby treated as a plain value, not a key.
    fn stale_simple_key(&mut self) {
        if let Some(key) = &self.simple_key {
            if key.line != self.reader.position().line {
                self.simple_key = None;
                // Past a key position with no ':', a new key is not allowed
                // until the next line break re-enables it.
            }
        }
    }
```

Call it in `fetch_more_tokens` right after `skip_to_next_token()` and before the block `unroll_indent`:

```rust
        self.skip_to_next_token();
        self.stale_simple_key();

        if self.flow_depth == 0 {
            self.unroll_indent(Self::col0(self.reader.position()));
        }
        // ... rest unchanged ...
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p yaml2 scanner::tests`
Expected: `bare_scalar_is_not_a_key` and the sequence guard pass; all prior tests pass.

Re-trace `hello\nworld`: StreamStart dequeued. fetch: save key{line=1,token_number=1}, push Scalar(hello). token_ready false (key pending, token_number 1 == tokens_parsed 1, not >). Loop → fetch: skip_to_next_token consumes `\n` (now at line 2), stale_simple_key: key.line(1) != 2 → clear simple_key. unroll(-... ). Then save key for `world`? `world` is at line 2 col 1 → save key{line=2,token_number=2}, push Scalar(world). Now token_ready: simple_key Some(line2,token_number2), 2 > tokens_parsed(1) → true → return Scalar(hello) (the front, index... queue=[Scalar(hello),Scalar(world)], pop hello, tokens_parsed=2). Next: token_ready: key token_number2 > tokens_parsed2? no → not ready → fetch: skip newline, stale (key.line2 != line3) clear. EOF unroll, StreamEnd. token_ready: None → return Scalar(world), then StreamEnd. Result: StreamStart, Scalar(hello), Scalar(world), StreamEnd. Correct.

- [ ] **Step 5: Run the full suite + clippy**

Run: `cargo test -p yaml2`
Expected: all pass.

Run: `cargo clippy -p yaml2 --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add yaml2/src/scanner/mod.rs
git commit -m "feat(yaml2): release stale block simple keys past their line"
```

---

## Task 8: Block comments, blank lines, and integration

**Files:** modify `yaml2/src/scanner/mod.rs`.

- [ ] **Step 1: Write the tests** (add to the `tests` module in `mod.rs`)

```rust
    #[test]
    fn block_mapping_with_comments_and_blanks() {
        let input = "# header\na: 1\n\n# mid\nb: 2\n";
        assert_eq!(
            kinds(input),
            vec![
                TokenKind::StreamStart,
                TokenKind::BlockMappingStart,
                TokenKind::Key,
                TokenKind::Scalar { value: "a".to_string(), style: ScalarStyle::Plain },
                TokenKind::Value,
                TokenKind::Scalar { value: "1".to_string(), style: ScalarStyle::Plain },
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
    fn mapping_value_is_flow_collection() {
        assert_eq!(
            kinds("nums: [1, 2]\n"),
            vec![
                TokenKind::StreamStart,
                TokenKind::BlockMappingStart,
                TokenKind::Key,
                TokenKind::Scalar { value: "nums".to_string(), style: ScalarStyle::Plain },
                TokenKind::Value,
                TokenKind::FlowSequenceStart,
                TokenKind::Scalar { value: "1".to_string(), style: ScalarStyle::Plain },
                TokenKind::FlowEntry,
                TokenKind::Scalar { value: "2".to_string(), style: ScalarStyle::Plain },
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
                TokenKind::Scalar { value: "a".to_string(), style: ScalarStyle::Plain },
                TokenKind::Value,
                TokenKind::Scalar { value: "1".to_string(), style: ScalarStyle::Plain },
                TokenKind::BlockEnd,
                TokenKind::DocumentStart,
                TokenKind::BlockMappingStart,
                TokenKind::Key,
                TokenKind::Scalar { value: "b".to_string(), style: ScalarStyle::Plain },
                TokenKind::Value,
                TokenKind::Scalar { value: "2".to_string(), style: ScalarStyle::Plain },
                TokenKind::BlockEnd,
                TokenKind::StreamEnd,
            ]
        );
    }
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p yaml2 scanner::tests::block_mapping_with_comments_and_blanks scanner::tests::mapping_value_is_flow_collection scanner::tests::document_marker_resets_block_indent`
Expected: `block_mapping_with_comments_and_blanks` and `mapping_value_is_flow_collection` likely pass already (comments/blanks are handled by `skip_to_next_token`; the flow value works because `[` increments `flow_depth` and block logic goes inert). `document_marker_resets_block_indent` likely FAILS — the document-marker arms in `scan_content` don't unroll the block indent.

- [ ] **Step 3: Make document markers unroll the block indent.** The `---`/`...` arms currently call `scan_marker`. They must first close any open block levels. Update `fetch_more_tokens` so that document markers trigger an unroll: the cleanest fix is in the document-marker handling. Change the `scan_marker` callers by unrolling before producing the marker. In `scan_content`, replace the two marker arms:

```rust
            '-' if self.marker_ahead("---") => {
                Ok(self.scan_marker(TokenKind::DocumentStart, start))
            }
            '.' if self.marker_ahead("...") => {
                Ok(self.scan_marker(TokenKind::DocumentEnd, start))
            }
```

with versions that unroll the block indent and reset simple-key state first:

```rust
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
```

Note: `unroll_indent(-1)` here pushes `BlockEnd`(s) into the queue, and `scan_content` then returns the `DocumentStart` token which `fetch_more_tokens` pushes *after* them. Queue order: `[BlockEnd..., DocumentStart]`. Correct. But `token_ready`: ensure the marker arms aren't reached while a simple key is pending mid-line — they're at line start where staleness has cleared keys, so `simple_key` is `None`. Confirm with the test.

- [ ] **Step 4: Run the full suite**

Run: `cargo test -p yaml2`
Expected: all pass.

- [ ] **Step 5: Verify clippy**

Run: `cargo clippy -p yaml2 --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add yaml2/src/scanner/mod.rs
git commit -m "feat(yaml2): block comments, flow values, and document-marker unroll"
```

---

## Task 9: Mixed integration and full verification

**Files:** modify `yaml2/src/scanner/mod.rs`.

- [ ] **Step 1: Write the integration test** (add to the `tests` module in `mod.rs`)

```rust
    #[test]
    fn realistic_block_document() {
        let input = "name: Ada\njobs:\n  - lang: rust\n    years: 3\n  - lang: yaml\n";
        assert_eq!(
            kinds(input),
            vec![
                TokenKind::StreamStart,
                TokenKind::BlockMappingStart,
                TokenKind::Key,
                TokenKind::Scalar { value: "name".to_string(), style: ScalarStyle::Plain },
                TokenKind::Value,
                TokenKind::Scalar { value: "Ada".to_string(), style: ScalarStyle::Plain },
                TokenKind::Key,
                TokenKind::Scalar { value: "jobs".to_string(), style: ScalarStyle::Plain },
                TokenKind::Value,
                TokenKind::BlockSequenceStart,
                TokenKind::BlockEntry,
                TokenKind::BlockMappingStart,
                TokenKind::Key,
                TokenKind::Scalar { value: "lang".to_string(), style: ScalarStyle::Plain },
                TokenKind::Value,
                TokenKind::Scalar { value: "rust".to_string(), style: ScalarStyle::Plain },
                TokenKind::Key,
                TokenKind::Scalar { value: "years".to_string(), style: ScalarStyle::Plain },
                TokenKind::Value,
                TokenKind::Scalar { value: "3".to_string(), style: ScalarStyle::Plain },
                TokenKind::BlockEnd,
                TokenKind::BlockEntry,
                TokenKind::BlockMappingStart,
                TokenKind::Key,
                TokenKind::Scalar { value: "lang".to_string(), style: ScalarStyle::Plain },
                TokenKind::Value,
                TokenKind::Scalar { value: "yaml".to_string(), style: ScalarStyle::Plain },
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
                TokenKind::Scalar { value: "a".to_string(), style: ScalarStyle::DoubleQuoted },
                TokenKind::Value,
                TokenKind::Scalar { value: "b".to_string(), style: ScalarStyle::DoubleQuoted },
                TokenKind::FlowMappingEnd,
                TokenKind::StreamEnd,
            ]
        );
    }
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p yaml2 scanner::tests::realistic_block_document scanner::tests::flow_still_works_after_block_changes`
Expected: both pass (no implementation changes expected — they exercise the assembled scanner). The `realistic_block_document` covers a mapping containing a sequence of mappings with mixed dedent (the triple `BlockEnd` at the end closes the inner mapping, the sequence, and the outer mapping).

If `realistic_block_document` fails, do NOT edit the expectation. Investigate column math in `roll_indent`/`unroll_indent` and report the actual vs expected stream if you cannot resolve it.

- [ ] **Step 3: Run the full crate suite**

Run: `cargo test -p yaml2`
Expected: all foundation + scanner tests pass.

- [ ] **Step 4: Verify clippy and formatting**

Run: `cargo clippy -p yaml2 --all-targets -- -D warnings`
Expected: clean.

Run: `cargo fmt` then `cargo fmt --check`
Expected: no diff. Confirm `git status --short` is clean after the commit.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "test(yaml2): integration tests for block structure"
```

---

## Self-Review

**Spec coverage (this plan's portion of the scanner):**
- Block sequences (`- item`) with `BlockSequenceStart`/`BlockEntry`/`BlockEnd` — Task 4. ✔
- Block mappings (`key: value`) with `BlockMappingStart`/`Key`/`Value`/`BlockEnd` via simple keys — Tasks 5, 7. ✔
- Indentation-driven nesting and dedent — Tasks 3, 5, 6. ✔
- Mapping values on following lines, mappings-in-sequences, sequences-under-keys — Task 6. ✔
- Flow values inside block (`key: [1,2]`) and comments/blank lines — Task 8. ✔
- Document markers reset block indent for multi-document streams — Task 8. ✔
- All Plan 2 flow behavior preserved (queue refactor is behavior-preserving) — Tasks 2, 9. ✔
- Source spans on every token (markers/keys are zero-width at their mark; `BlockEnd` zero-width at the dedent position) — throughout. ✔
- Pure Rust, no `unsafe`. ✔

**Deferred to later plans (correctly out of scope):** block scalars `|`/`>` and multi-line plain/quoted folding (Plan 4 — so a plain value/key is single-line here); explicit `?` complex keys; `%YAML`/`%TAG` directives; tab-as-indentation rejection nuance and exhaustive indentation-error diagnostics (hardening, Plan 9); the event parser and public streaming API (Plan 5).

**Placeholder scan:** No "TBD/TODO". Task 6 and Task 9 steps explicitly note that some tests may pass without new code (they validate assembled behavior) and instruct *not* to weaken expectations if a test fails — investigate instead. ✔

**Type consistency:** New `TokenKind` variants (`BlockSequenceStart`/`BlockMappingStart`/`BlockEnd`/`BlockEntry`) defined in Task 1, used consistently in Tasks 4–9. `SimpleKey { token_number, mark, line }` defined in Task 2, used in `save_simple_key`/`fetch_block_value`/`stale_simple_key` (Tasks 5, 7). `Scanner` fields (`tokens`, `tokens_parsed`, `flow_depth`, `indent`, `indents`, `simple_key`, `simple_key_allowed`, `stream_end_produced`) defined in Task 2 and used thereafter. Helpers `col0`, `roll_indent`, `unroll_indent` (Task 3), `block_entry_next`/`fetch_block_entry` (Task 4), `save_simple_key`/`remove_simple_key`/`can_start_simple_key`/`fetch_block_value` (Task 5), `stale_simple_key` (Task 7), `token_ready` (Task 5) have stable signatures across tasks. The `:` arm and document-marker arms in `scan_content` are revised in Tasks 5 and 8 respectively; the final forms are the ones to keep.

**Execution note for the orchestrator:** This is the highest-risk plan in the project (the simple-key + indentation algorithm has many interacting edge cases). The bite-sized traces in Tasks 5 and 7 are included specifically to validate the queue/`token_ready`/staleness interplay. If an implementer hits a discrepancy, prefer debugging the column math and `token_ready`/staleness timing over altering token-stream expectations, which are believed correct per the YAML token model.
