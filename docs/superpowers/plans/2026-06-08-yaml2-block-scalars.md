# yaml2 Block Scalars & Multi-line Quoted Folding Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete the `yaml2` scanner's scalar handling: literal (`|`) and folded (`>`) block scalars with chomping and indentation indicators, plus multi-line folding for single- and double-quoted scalars.

**Architecture:** Plan 4 of 9 for `yaml2` (see `docs/superpowers/specs/2026-06-08-yaml2-parser-design.md`). Builds on the completed flow+block scanner (`scanner/mod.rs`). Block scalars are content scanned char-by-char into a value `String` with style `Literal`/`Folded`; they read indentation-delimited content lines after a header, then assemble the value applying folding and chomping. Multi-line quoted folding extends the existing quoted-scalar scanners with line-break folding. No structural (queue/indent/simple-key) changes are needed — block scalars are scalar *values*, not collections.

**Tech Stack:** Rust 2021. Builds on Plans 1–3. No new dependencies. No `unsafe`.

---

## Context for the engineer

`yaml2/src/scanner/mod.rs` has the full flow+block scanner: `Scanner` with a token queue, `flow_depth`, indentation stack (`indent: i64` 0-based, -1 root), simple keys, and `scan_content` dispatching on the first char. Scalars: `scan_plain` (single-line), `scan_single_quoted`, `scan_double_quoted` (with `scan_escape`/`scan_hex_escape`). `ScalarStyle` (in `crate::meta`) has `Plain`, `SingleQuoted`, `DoubleQuoted`, `Literal`, `Folded` — the last two are produced for the first time in this plan. `Token`/`TokenKind` in `token.rs`.

### Block scalars

```yaml
key: |
  line one
  line two
```
A block scalar starts with `|` (literal) or `>` (folded), optionally followed by a **chomping indicator** (`-` strip, `+` keep, default *clip*) and/or an **indentation indicator** (a digit `1`–`9`), in either order. The remainder of the header line is whitespace/comment. Then come the **content lines**, indented more than the block's parent. The content indentation is auto-detected from the first non-empty line (or set by the indicator). Lines less-indented than that (and non-empty) end the scalar.

- **Literal (`|`):** line breaks are kept verbatim.
- **Folded (`>`):** a single line break between two normal lines folds to a space; blank lines become line breaks; *more-indented* lines (extra leading spaces beyond the content indent) keep their breaks literally.
- **Chomping** acts on the *trailing* line breaks: clip = exactly one `\n`; strip = none; keep = all.

### Multi-line quoted folding

A quoted scalar (`'…'` or `"…"`) may span lines. At a line break inside the quotes: trailing whitespace of the line is trimmed, leading whitespace of the continuation is trimmed, a single break folds to a space, and blank lines become `\n`. Double-quoted also supports an **escaped line break** (`\` at end of line) = line continuation with no inserted space.

### Out of scope (Plan 9 hardening)

**Multi-line *plain* scalar folding** (a plain scalar spanning lines) — single-line plain already works; multi-line plain's continuation rules entangle with block indentation/key detection and are deferred. Also deferred: tabs as content-indent, exotic explicit-indent-at-root edge cases, and `\`-continuation inside single-quoted (single-quoted has no escapes).

### Conventions
- Block scalars are valid only in block context (`flow_depth == 0`).
- A block scalar cannot be a mapping key, so `|`/`>` must be excluded from simple-key candidates.
- 0-based indentation columns (`col0 = column - 1`); `self.indent` is the parent block's indent.

---

## File structure

| File | Change |
|------|--------|
| `yaml2/src/scanner/mod.rs` | `Chomping` enum; `scan_block_scalar` + helpers (`scan_block_scalar_header`, `assemble_literal`, `assemble_folded`, `apply_chomping`, `consume_line_break`, `scan_flow_folded_breaks`); `|`/`>` arms in `scan_content`; multi-line folding in the quoted scanners; exclude `|`/`>` from `can_start_simple_key` |

`mod.rs` grows further. A split (e.g. `scanner/scalar.rs`) may be worthwhile after this plan, but do not split mid-plan.

---

## Task 1: Literal block scalars

**Files:** modify `yaml2/src/scanner/mod.rs`.

- [ ] **Step 1: Write the failing tests** (add to the `tests` module in `mod.rs`)

```rust
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
        // clip (default): single trailing newline.
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
        // keep: all trailing newlines, including the blank line.
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p yaml2 scanner::tests::literal_block_scalar_basic`
Expected: FAIL — `|` is currently scanned as a plain scalar.

- [ ] **Step 3: Add the `Chomping` enum** near the `SimpleKey` struct in `mod.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Chomping {
    Clip,
    Strip,
    Keep,
}
```

- [ ] **Step 4: Exclude `|`/`>` from simple-key candidates.** Update `can_start_simple_key`:

```rust
    fn can_start_simple_key(c: char) -> bool {
        !matches!(
            c,
            '-' | '?' | ':' | ',' | '[' | ']' | '{' | '}' | '#' | '|' | '>'
        )
    }
```

- [ ] **Step 5: Add the `|`/`>` arms to `scan_content`**, before the `_ => self.scan_plain(start)` arm (and after the block-entry arm). Both only at block level:

```rust
            '|' if self.flow_depth == 0 => self.scan_block_scalar(true, start),
            '>' if self.flow_depth == 0 => self.scan_block_scalar(false, start),
```

- [ ] **Step 6: Add the block-scalar scanner and helpers** (add these methods to `impl Scanner`):

```rust
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
            let base = if parent < 0 { 0 } else { (parent + 1) as usize };
            base + n - 1
        });

        // Each line: (text after the stripped indentation, is_more_indented).
        let mut lines: Vec<(String, bool)> = Vec::new();
        loop {
            let mut sp = 0usize;
            while self.reader.peek_nth(sp) == Some(' ') {
                sp += 1;
            }
            let after = self.reader.peek_nth(sp);
            if after.is_none() {
                break; // EOF
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
                        break; // not indented enough → empty content
                    }
                    content_indent = Some(sp);
                    sp
                }
            };
            if sp < ci {
                break; // dedent → line belongs to the parent
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
```

- [ ] **Step 7: Add a stub for `assemble_folded`** so the file compiles (real implementation in Task 4):

```rust
    /// Folded assembly — implemented in Task 4.
    fn assemble_folded(lines: &[(String, bool)], chomp: Chomping) -> String {
        // Temporary: behave like literal until Task 4 adds folding.
        Self::assemble_literal(lines, chomp)
    }
```

- [ ] **Step 8: Run the tests**

Run: `cargo test -p yaml2 scanner::tests`
Expected: the 4 literal tests pass; all prior tests still pass.

- [ ] **Step 9: Verify clippy**

Run: `cargo clippy -p yaml2 --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 10: Commit**

```bash
git add yaml2/src/scanner/mod.rs
git commit -m "feat(yaml2): scan literal block scalars with chomping"
```

---

## Task 2: Explicit indentation indicator

**Files:** modify `yaml2/src/scanner/mod.rs`.

- [ ] **Step 1: Write the failing tests** (add to the `tests` module)

```rust
    #[test]
    fn block_scalar_explicit_indent() {
        // `|2` at root: content indent is exactly 2 spaces, so the extra spaces
        // on the second line are content.
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
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p yaml2 scanner::tests::block_scalar_explicit_indent scanner::tests::block_scalar_indent_and_chomp_combined`
Expected: These likely already PASS — `scan_block_scalar_header` already parses the indicator and `scan_block_scalar` already maps it to `content_indent` (Task 1, Step 6). Run to confirm.

`|2\n  a\n    b\n` at root: parent = -1, so `base = 0`, `content_indent = 0 + 2 - 1 = 1`? That gives 1, but we want 2. Verify the actual output. If it is wrong, fix the explicit-indent base in Task 1's mapping closure to:

```rust
        let mut content_indent: Option<usize> = explicit_indent.map(|n| {
            let base = if parent < 0 { 0 } else { (parent + 1) as usize };
            base + n
        });
```

i.e. `base + n` (not `base + n - 1`). For root `|2`: base 0 + 2 = 2 content-indent spaces. For a nested `key: |2` with the mapping at indent 0: base = (0+1) = 1, + 2 = 3 — content indented 3 from column 0. (The indicator is relative to the parent node's indentation per YAML 1.2 §8.1.1.1; this `base + n` form matches the common interpretation. Exotic root/relative edge cases are deferred to Plan 9.)

Apply whichever of `base + n - 1` / `base + n` makes `|2\n  a\n    b\n` → `"a\n  b\n"` (content indent 2 at root). Then re-run.

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cargo test -p yaml2 scanner::tests`
Expected: both explicit-indent tests pass; all prior pass.

- [ ] **Step 4: Verify clippy**

Run: `cargo clippy -p yaml2 --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add yaml2/src/scanner/mod.rs
git commit -m "feat(yaml2): support block-scalar explicit indentation indicator"
```

---

## Task 3: Block scalars as mapping values and sequence items

**Files:** modify `yaml2/src/scanner/mod.rs`.

- [ ] **Step 1: Write the failing tests** (add to the `tests` module)

```rust
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
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p yaml2 scanner::tests::block_scalar_as_mapping_value scanner::tests::block_scalar_then_next_key scanner::tests::block_scalar_as_sequence_item`
Expected: likely PASS already — after `key:`/`- `, `skip_to_next_token` skips the space to `|`, and `scan_block_scalar` reads the indented content and stops at the dedent (`b: 2` at col 0), leaving the reader at line start so the next fetch's `unroll_indent` is correct. Run to confirm.

- [ ] **Step 3: Only if a test fails**, debug the interaction between `scan_block_scalar` and the surrounding block machinery. The most likely issue: after the block scalar stops at a dedented line, the reader must be at that line's start (column 1) so `fetch_more_tokens`'s `unroll_indent(col0(...))` sees the correct column. The content loop breaks *without consuming* the dedented line's leading spaces (the `if sp < ci { break; }` and auto-detect `break` paths peek but don't consume), so the reader is at the line's true start. Confirm this; if the reader is mid-line, adjust the loop to not consume on the terminating line. Do NOT change test expectations.

- [ ] **Step 4: Run the full suite**

Run: `cargo test -p yaml2`
Expected: all pass.

- [ ] **Step 5: Verify clippy**

Run: `cargo clippy -p yaml2 --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add yaml2/src/scanner/mod.rs
git commit -m "test(yaml2): block scalars as mapping values and sequence items"
```

---

## Task 4: Folded block scalars

**Files:** modify `yaml2/src/scanner/mod.rs`.

- [ ] **Step 1: Write the failing tests** (add to the `tests` module)

```rust
    #[test]
    fn folded_block_scalar_basic() {
        // single breaks fold to spaces.
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
        // a line with extra indentation is not folded.
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p yaml2 scanner::tests::folded_block_scalar_basic`
Expected: FAIL — `assemble_folded` is currently the literal stub (`"a\nb\n"` not `"a b\n"`).

- [ ] **Step 3: Replace the `assemble_folded` stub** with the real folding implementation:

```rust
    /// Joins folded block-scalar lines: a single break between two normal lines
    /// becomes a space; blank lines become breaks; more-indented lines keep
    /// their breaks. Then chomps.
    fn assemble_folded(lines: &[(String, bool)], chomp: Chomping) -> String {
        let mut value = String::new();
        // True when the previous line forces the next break to stay literal
        // (previous line was blank or more-indented).
        let mut prev_forces_break = false;
        for (i, (text, more)) in lines.iter().enumerate() {
            if i == 0 {
                value.push_str(text);
                prev_forces_break = *more || text.is_empty();
                continue;
            }
            if text.is_empty() {
                value.push('\n');
                prev_forces_break = true;
            } else if *more || prev_forces_break {
                value.push('\n');
                value.push_str(text);
                prev_forces_break = *more;
            } else {
                value.push(' ');
                value.push_str(text);
                prev_forces_break = false;
            }
        }
        value.push('\n');
        Self::apply_chomping(value, chomp)
    }
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p yaml2 scanner::tests`
Expected: the 4 folded tests pass; all prior (including literal) pass.

- [ ] **Step 5: Verify clippy**

Run: `cargo clippy -p yaml2 --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add yaml2/src/scanner/mod.rs
git commit -m "feat(yaml2): scan folded block scalars"
```

---

## Task 5: Multi-line double-quoted folding

**Files:** modify `yaml2/src/scanner/mod.rs`.

- [ ] **Step 1: Write the failing tests** (add to the `tests` module)

```rust
    #[test]
    fn double_quoted_multiline_folds_to_space() {
        assert_eq!(
            one_scalar("\"a\nb\""),
            ("a b".to_string(), ScalarStyle::DoubleQuoted)
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
        // backslash at end of line joins with no space.
        assert_eq!(
            one_scalar("\"a\\\n   b\""),
            ("ab".to_string(), ScalarStyle::DoubleQuoted)
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p yaml2 scanner::tests::double_quoted_multiline_folds_to_space`
Expected: FAIL — the current scanner includes the raw `\n` in the value.

- [ ] **Step 3: Add the shared fold-break helper** to `impl Scanner`:

```rust
    /// At a line break inside a flow (quoted) scalar, consumes the break, any
    /// following blank lines, and the leading whitespace of the continuation.
    /// Returns the folded text: a single break → one space; N blank lines → N
    /// newlines. The caller is responsible for trimming trailing whitespace
    /// before the break.
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
```

- [ ] **Step 4: Update `scan_double_quoted`** to fold line breaks and handle escaped continuations. Replace the `loop { match self.reader.peek() { ... } }` body so the arms are:

```rust
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
                    value.push_str(&folded);
                }
                Some(c) => {
                    self.reader.advance();
                    value.push(c);
                }
            }
        }
```

(The opening-quote `advance` and the `let mut value = String::new();` at the top of `scan_double_quoted` are unchanged.)

- [ ] **Step 5: Run the tests**

Run: `cargo test -p yaml2 scanner::tests`
Expected: the 4 multi-line double-quoted tests pass; all prior double-quoted tests (basic, escapes, hex, unterminated, invalid-escape) still pass.

- [ ] **Step 6: Verify clippy**

Run: `cargo clippy -p yaml2 --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add yaml2/src/scanner/mod.rs
git commit -m "feat(yaml2): fold multi-line double-quoted scalars"
```

---

## Task 6: Multi-line single-quoted folding

**Files:** modify `yaml2/src/scanner/mod.rs`.

- [ ] **Step 1: Write the failing tests** (add to the `tests` module)

```rust
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p yaml2 scanner::tests::single_quoted_multiline_folds_to_space`
Expected: FAIL — current scanner includes raw `\n`.

- [ ] **Step 3: Update `scan_single_quoted`** to fold line breaks. Add a line-break arm to its `match self.reader.peek()` (before the catch-all `Some(c)` arm):

```rust
                Some('\n') | Some('\r') => {
                    let trimmed = value.trim_end_matches([' ', '\t']).len();
                    value.truncate(trimmed);
                    let folded = self.scan_flow_folded_breaks();
                    value.push_str(&folded);
                }
```

The existing `None` (unterminated error), `Some('\'')` (closing/doubled-quote), and `Some(c)` (literal char) arms are unchanged. Place the new arm before `Some(c)`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p yaml2 scanner::tests`
Expected: the 4 multi-line single-quoted tests pass; all prior single-quoted tests (basic, doubled-quote, unterminated) still pass.

- [ ] **Step 5: Verify clippy**

Run: `cargo clippy -p yaml2 --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add yaml2/src/scanner/mod.rs
git commit -m "feat(yaml2): fold multi-line single-quoted scalars"
```

---

## Task 7: Integration and full verification

**Files:** modify `yaml2/src/scanner/mod.rs`.

- [ ] **Step 1: Write the integration tests** (add to the `tests` module)

```rust
    #[test]
    fn block_scalars_in_a_mapping() {
        let input = "literal: |\n  a\n  b\nfolded: >\n  c\n  d\n";
        assert_eq!(
            kinds(input),
            vec![
                TokenKind::StreamStart,
                TokenKind::BlockMappingStart,
                TokenKind::Key,
                TokenKind::Scalar { value: "literal".to_string(), style: ScalarStyle::Plain },
                TokenKind::Value,
                TokenKind::Scalar { value: "a\nb\n".to_string(), style: ScalarStyle::Literal },
                TokenKind::Key,
                TokenKind::Scalar { value: "folded".to_string(), style: ScalarStyle::Plain },
                TokenKind::Value,
                TokenKind::Scalar { value: "c d\n".to_string(), style: ScalarStyle::Folded },
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
                TokenKind::Scalar { value: "msg".to_string(), style: ScalarStyle::Plain },
                TokenKind::Value,
                TokenKind::Scalar { value: "hello world".to_string(), style: ScalarStyle::DoubleQuoted },
                TokenKind::BlockEnd,
                TokenKind::StreamEnd,
            ]
        );
    }

    #[test]
    fn flow_and_block_scalars_unaffected() {
        // Regression: single-line scalars and flow still work.
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

Run: `cargo test -p yaml2 scanner::tests::block_scalars_in_a_mapping scanner::tests::multiline_quoted_value_in_mapping scanner::tests::flow_and_block_scalars_unaffected`
Expected: all pass (assembled behavior; no new code). If `block_scalars_in_a_mapping` fails, the most likely cause is the block scalar not stopping cleanly at the next key line — debug per Task 3 Step 3; do not change expectations.

- [ ] **Step 3: Run the full crate suite**

Run: `cargo test -p yaml2`
Expected: all foundation + scanner tests pass.

- [ ] **Step 4: Verify clippy and formatting**

Run: `cargo clippy -p yaml2 --all-targets -- -D warnings`
Expected: clean.

Run: `cargo fmt` then `cargo fmt --check`
Expected: no diff. Confirm `git status --short` clean after commit.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "test(yaml2): integration tests for block and multi-line scalars"
```

---

## Self-Review

**Spec coverage (this plan's portion of the scanner):**
- Literal block scalars (`|`) with all chomping modes — Task 1. ✔
- Folded block scalars (`>`) with folding + more-indented-line rule + chomping — Task 4. ✔
- Block-scalar header: chomping + indentation indicator (either order, comment/blank tail) — Tasks 1, 2. ✔
- Block scalars as mapping values and sequence items, with correct dedent handoff — Task 3. ✔
- Multi-line double-quoted folding + escaped line continuation — Task 5. ✔
- Multi-line single-quoted folding (with `''` still working) — Task 6. ✔
- `ScalarStyle::Literal`/`Folded` produced (completing the foundation enum's usage). ✔
- Block scalars excluded from simple-key candidates — Task 1. ✔
- Source spans on block/quoted scalars (start to post-content position). ✔
- Pure Rust, no `unsafe`. ✔

**Deferred to later plans (correctly out of scope):** multi-line *plain* scalar folding (Plan 9 hardening — single-line plain works); tabs as content indentation; exotic explicit-indent base cases at root; the event parser (Plan 5). The `max_depth` enforcement and malformed-input rejection tracked from Plan 3 remain Plan 5 (parser) items.

**Placeholder scan:** No "TBD/TODO". Task 1 Step 7 adds an explicit *temporary* `assemble_folded` stub (literal behavior) that Task 4 replaces with the real folding — Task 4's tests fail against the stub first, confirming the replacement. ✔

**Type consistency:** `Chomping` (Clip/Strip/Keep) defined in Task 1, used by `apply_chomping`/`assemble_literal`/`assemble_folded`/`scan_block_scalar_header`. `scan_block_scalar(literal: bool, start)`, `scan_block_scalar_header() -> (Chomping, Option<usize>)`, `assemble_literal(&[(String,bool)], Chomping) -> String`, `assemble_folded(...)` (stub→real), `apply_chomping(String, Chomping) -> String`, `consume_line_break()`, `scan_flow_folded_breaks() -> String` have stable signatures across tasks. The `|`/`>` arms (Task 1) and the quoted-scanner edits (Tasks 5, 6) all rely on `consume_line_break`/`scan_flow_folded_breaks`. `ScalarStyle::Literal`/`Folded`/`SingleQuoted`/`DoubleQuoted` are the foundation enum variants. `one_scalar` test helper (Task 1) reused by Tasks 2, 4, 5, 6.

**Execution note:** Block-scalar indentation math (Task 2's explicit-indent base) and the folded more-indented-line rule (Task 4) are the subtlest parts — the tests pin concrete expected values; if an implementer hits a mismatch, debug the indentation/folding logic against the YAML 1.2 §8.1 examples rather than altering expectations.
