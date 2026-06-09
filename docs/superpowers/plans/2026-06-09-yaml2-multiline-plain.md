# yaml2 Multi-line Plain Scalar Folding Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the confirmed correctness bug where plain scalars spanning multiple lines are silently truncated, by folding continuation lines per the YAML 1.2 rules in both block and flow context.

**Architecture:** Rewrite the scanner's `scan_plain` to fold across line breaks: a single break becomes a space, blank lines become newlines, and per-line leading/trailing whitespace is stripped. A continuation line is accepted only if it is more indented than the current block indent (block context) or simply present (flow context) and does not begin a document marker, comment, or new construct. Deciding this requires lookahead past the break(s); since rejecting a candidate must not consume the next line's indentation, the `Reader` gains a cheap checkpoint (`mark`/`reset`).

**Tech Stack:** Rust 2021. Scanner-only change plus reader support. No new dependencies.

---

## Context for the engineer

You are extending the `yaml2` crate's scanner (`yaml2/src/scanner/`). Today `scan_plain` stops at the first line break, so `key: one\n  two` yields the scalar `one` instead of `one two`. This plan makes plain scalars fold across lines.

**Read these first** (do not re-derive — use them):

- `yaml2/src/scanner/reader.rs` — `Reader { input: &str, offset, line, column }`. Methods: `position() -> Position`, `peek() -> Option<char>`, `peek_nth(n) -> Option<char>`, `advance() -> Option<char>` (tracks line/column, recognizes `\n`/`\r\n`/`\r`), `count_leading_spaces() -> usize`, `starts_with(&str) -> bool`. **You will add `mark`/`reset`** in Task 1.
- `yaml2/src/scanner/mod.rs` — the `Scanner`. Relevant state: `flow_depth: usize` (0 = block context), `indent: i64` (current block indentation, 0-based; -1 = root), and the simple-key fields. Relevant methods you will reuse: `consume_line_break()`, `marker_ahead("---")`, `indicator_terminator_next()` (true when the char after the current one makes `:`/`?` an indicator), `block_entry_next()` (true when the char after `-` makes it a block entry), `col0(pos)`. The current `scan_plain` is around line 342.
- How `scan_plain` is reached: `fetch_more_tokens` → `skip_to_next_token` → `scan_content` (dispatch) → `_ => self.scan_plain(start)`. The caller has already saved a potential simple key for the scalar's start line. The existing `stale_simple_key` (dropped when the reader's line advances past the key's line) correctly invalidates a multi-line plain scalar as an implicit key — **you do not need to touch the simple-key machinery**. After `scan_plain` returns, a multi-line scalar leaves the reader either at end-of-input, at a terminator mid-line, or at the un-consumed line break that ended it (see `plain_continues` below); the next `fetch_more_tokens` handles indentation/keys normally.
- The quoted scanners (`scan_single_quoted`, `scan_double_quoted`) and `scan_flow_folded_breaks` (around line 595) show the existing fold convention: trim trailing whitespace before a break, then a single break → one space and N blank lines → N newlines.

### Folding rules (what "correct" means)

For a plain scalar spanning lines:
- Trailing spaces/tabs on each line are stripped; leading spaces/tabs on each continuation line are stripped.
- A single line break between content folds to a single space.
- `k` consecutive line breaks (i.e. `k-1` blank lines between content) fold to `k-1` newline characters.
- A continuation line is part of the scalar only if, after the break(s):
  - it is not end-of-input,
  - it does not begin a document marker (`---` / `...`),
  - its first non-whitespace char is not `#` (a comment),
  - in **flow** context: its first char is not a flow indicator (`,` `[` `]` `{` `}`) and not a `:` acting as an indicator,
  - in **block** context: it is indented more than `self.indent` (`col0 > self.indent`), and its first char does not begin a new construct (`-`+space block entry, `?`+terminator explicit key, `:`+terminator value).

Examples (all in block context unless noted):
- `one\ntwo` → `one two` (root: indent -1, col 0 > -1).
- `key: one\n  two` → value `one two`.
- `a\n\n  b` → `a\nb` (one blank line → one newline).
- `key: one\n  two\nnext: x` → `{key: "one two", next: "x"}` (`next` at col 0 is not > 0, so it ends the scalar).
- `[one\n two]` (flow) → sequence with the single scalar `one two`.
- `a\n# c\nb` → scalar `a` (comment line ends it), then scalar `b`.

### Scope / deferrals (locked)

- This plan changes **only** plain-scalar scanning (and adds reader `mark`/`reset`). Quoted and block scalars already fold and are untouched.
- **Tabs used as indentation** are treated leniently here (a continuation's leading tabs are stripped as whitespace); strict tab rejection is a separate deferred plan.
- The pathological case where a more-indented continuation line itself contains a `: ` (e.g. `key: one\n  two: three`) ends the scalar at the `:` and yields a token stream the parser rejects — this matches YAML's treatment of it as malformed and is acceptable.
- No `unsafe` (crate forbids it).

### File structure

- Modify `yaml2/src/scanner/reader.rs` — add an opaque `Mark` plus `mark()` / `reset()`.
- Modify `yaml2/src/scanner/mod.rs` — rewrite `scan_plain`; add the `plain_continues` helper.

---

## Task 1: Reader checkpoint (`mark` / `reset`)

**Files:**
- Modify: `yaml2/src/scanner/reader.rs`

- [ ] **Step 1: Write the failing test** (add to the `tests` module in `reader.rs`):

```rust
    #[test]
    fn mark_and_reset_restore_position() {
        let mut r = Reader::new("ab\ncd");
        assert_eq!(r.advance(), Some('a'));
        let m = r.mark();
        assert_eq!(r.advance(), Some('b'));
        r.advance(); // '\n' -> line 2
        assert_eq!(r.advance(), Some('c'));
        // Restore to right after 'a'.
        r.reset(m);
        assert_eq!(r.position().offset, 1);
        assert_eq!(r.position().line, 1);
        assert_eq!(r.position().column, 2);
        assert_eq!(r.advance(), Some('b'));
    }
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test -p yaml2 scanner::reader::tests::mark_and_reset_restore_position`
Expected: FAIL (compile error — `mark`/`reset` do not exist).

- [ ] **Step 3: Add the `Mark` type and methods.** Add near the top of `reader.rs` (after the `Reader` struct definition):

```rust
/// An opaque snapshot of a `Reader`'s position, for cheap backtracking.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Mark {
    offset: usize,
    line: usize,
    column: usize,
}
```

And add these methods inside `impl<'a> Reader<'a>`:

```rust
    /// Snapshots the current position for later `reset`.
    pub(crate) fn mark(&self) -> Mark {
        Mark {
            offset: self.offset,
            line: self.line,
            column: self.column,
        }
    }

    /// Restores a position captured by `mark`.
    pub(crate) fn reset(&mut self, mark: Mark) {
        self.offset = mark.offset;
        self.line = mark.line;
        self.column = mark.column;
    }
```

- [ ] **Step 4: Run the test to confirm it passes**

Run: `cargo test -p yaml2 scanner::reader::tests::mark_and_reset_restore_position`
Expected: PASS.

- [ ] **Step 5: Full suite + clippy + fmt**

Run: `cargo test -p yaml2` — all pass.
Run: `cargo clippy -p yaml2 --all-targets -- -D warnings` — clean. (`Mark` is used by `scan_plain` in Task 2; if clippy flags it as unused in this task alone, that is expected to clear in Task 2. If `-D warnings` fails on `dead_code` for `Mark`/`mark`/`reset` now, note it and proceed — `reader.rs` is part of the scanner module which has `#![allow(dead_code)]` at the top of `mod.rs`, but that attribute does not cover the `reader` submodule. If it fails, add `#[allow(dead_code)]` ONLY on the three new items with a comment "used by scan_plain in the next task", and remove it in Task 2.)

Run: `cargo fmt` then `cargo fmt --check` — no diff.

- [ ] **Step 6: Commit**

```bash
git add yaml2/src/scanner/reader.rs
git commit -m "feat(yaml2): add Reader mark/reset for scanner backtracking"
```

---

## Task 2: Multi-line plain scalar folding

**Files:**
- Modify: `yaml2/src/scanner/mod.rs`

- [ ] **Step 1: Write the failing tests** (add to the `tests` module in `scanner/mod.rs`). These use the existing `scalars(input) -> Vec<(String, ScalarStyle)>` and `kinds(input) -> Vec<TokenKind>` test helpers:

```rust
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
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p yaml2 scanner::tests::plain_folds_single_break_to_space`
Expected: FAIL — current `scan_plain` stops at the line break, yielding `one` not `one two`.

- [ ] **Step 3: Replace `scan_plain` and add `plain_continues`.** Replace the entire existing `scan_plain` method with the two methods below:

```rust
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
                    Some(',') | Some('[') | Some(']') | Some('{') | Some('}')
                        if self.flow_depth > 0 =>
                    {
                        break 'scan
                    }
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
```

(If you added a temporary `#[allow(dead_code)]` to `Mark`/`mark`/`reset` in Task 1, remove it now — they are used here.)

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p yaml2 scanner::tests`
Expected: all pass — the new tests and all pre-existing scanner tests. If a pre-existing single-line plain test regresses, the folding loop's single-line path is wrong (check that the whitespace-buffer flush reproduces the old internal-space behavior); fix `scan_plain`, do not change the old tests.

- [ ] **Step 5: Full suite + clippy + fmt**

Run: `cargo test -p yaml2` — all pass.
Run: `cargo clippy -p yaml2 --all-targets -- -D warnings` — clean.
Run: `cargo fmt` then `cargo fmt --check` — no diff.

- [ ] **Step 6: Commit**

```bash
git add yaml2/src/scanner/mod.rs yaml2/src/scanner/reader.rs
git commit -m "feat(yaml2): fold multi-line plain scalars in block and flow context"
```

---

## Task 3: End-to-end integration and round-trip

**Files:**
- Modify: `yaml2/src/composer.rs` (tests only)

- [ ] **Step 1: Write the integration tests** (add to the `tests` module in `composer.rs`, which already has the `parse(input) -> Value` and `key(s) -> Value` helpers):

```rust
    #[test]
    fn multiline_plain_value_composes_folded() {
        let v = parse("summary: line one\n  line two\n");
        let m = v.as_mapping().unwrap();
        assert_eq!(
            m.get(&key("summary")).unwrap().as_str(),
            Some("line one line two")
        );
    }

    #[test]
    fn multiline_plain_blank_line_becomes_newline() {
        let v = parse("text: para one\n\n  para two\n");
        assert_eq!(
            v.as_mapping().unwrap().get(&key("text")).unwrap().as_str(),
            Some("para one\npara two")
        );
    }

    #[test]
    fn multiline_plain_root_document() {
        assert_eq!(parse("one\ntwo\nthree\n").as_str(), Some("one two three"));
    }

    #[test]
    fn multiline_plain_in_flow_sequence_composes() {
        let v = parse("[one\n two, three]\n");
        let items = v.as_sequence().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].as_str(), Some("one two"));
        assert_eq!(items[1].as_str(), Some("three"));
    }
```

- [ ] **Step 2: Add a round-trip test to `yaml2/src/emitter.rs`** (the `tests` module has a `roundtrip(&str)` helper):

```rust
    #[test]
    fn multiline_plain_roundtrips() {
        // The folded value re-emits (as a single line) and re-parses to the same value.
        roundtrip("summary: line one\n  line two\n");
        roundtrip("text: para one\n\n  para two\n");
    }
```

- [ ] **Step 3: Run the tests**

Run: `cargo test -p yaml2 composer::tests::multiline_plain_value_composes_folded composer::tests::multiline_plain_blank_line_becomes_newline composer::tests::multiline_plain_root_document composer::tests::multiline_plain_in_flow_sequence_composes emitter::tests::multiline_plain_roundtrips`
Expected: all pass. If a round-trip fails, print the emitted text (the helper includes it on failure) and diagnose — but no production change should be needed; the emitter already quotes strings containing newlines, and space-joined strings emit plain.

- [ ] **Step 4: Full crate suite (both feature configs)**

Run: `cargo test -p yaml2` — all pass.
Run: `cargo test -p yaml2 --no-default-features` — all pass.

- [ ] **Step 5: Clippy and formatting**

Run: `cargo clippy -p yaml2 --all-targets -- -D warnings` — clean.
Run: `cargo fmt` then `cargo fmt --check` — no diff.
Run: `git status --short` — empty after commit.

- [ ] **Step 6: Commit**

```bash
git add yaml2/src/composer.rs yaml2/src/emitter.rs
git commit -m "test(yaml2): end-to-end multi-line plain scalar folding"
```

---

## Self-Review

**Spec coverage** (against `docs/superpowers/specs/2026-06-08-yaml2-parser-design.md`):
- The design spec lists "multi-line plain folding" among the hardening items; this plan delivers it as a focused, self-contained piece. ✓
- YAML 1.2 plain-scalar folding semantics (break→space, blank→newline, per-line whitespace stripped, indentation-aware continuation) → Task 2. ✓
- Block and flow context both covered → Task 2 (`flow_depth` branches), verified end-to-end in Task 3. ✓
- Deferred and called out: strict tab-as-indentation rejection, yaml-test-suite gate, tag resolution, fuzzing, format-preserving round-trip — each its own follow-on plan. ✓

**Placeholder scan:** every code step contains complete code. No TBD/TODO. ✓

**Type consistency:** `Mark`/`mark`/`reset` are introduced in Task 1 and consumed by `plain_continues` in Task 2. `scan_plain` keeps its signature `(&mut self, start: Position) -> Result<Token>`; the new `plain_continues(&mut self, &mut bool, &mut usize) -> bool` helper is self-consistent and reuses existing `consume_line_break`, `marker_ahead`, `indicator_terminator_next`, `block_entry_next`. Test helpers `scalars`/`kinds` (scanner), `parse`/`key` (composer), `roundtrip` (emitter) already exist. ✓
