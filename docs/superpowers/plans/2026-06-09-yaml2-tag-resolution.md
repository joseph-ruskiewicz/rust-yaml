# yaml2 Tag Resolution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve YAML tags fully — verbatim `!<uri>`, shorthand handles (`!`, `!!`, named `!h!`), and the non-specific `!` — using document-scoped `%TAG`/`%YAML` directives, so tagged nodes carry their resolved tag.

**Architecture:** Tag resolution is lexical and document-scoped, so it lives in the **scanner**. The scanner keeps a handle→prefix map (seeded with the defaults `!`→`!` and `!!`→`tag:yaml.org,2002:`), updated by `%TAG` directives and reset per document. `scan_tag` expands every tag to its resolved form before emitting the `Tag` token. The parser passes tags through unchanged, and the composer's existing `classify_tag` already understands the resolved core-tag URIs (`tag:yaml.org,2002:*`); custom/local tags resolve to strings as before.

**Tech Stack:** Rust 2021. Scanner-only change plus a few test updates downstream. No new dependencies.

---

## Context for the engineer

You are extending the `yaml2` crate's scanner (`yaml2/src/scanner/mod.rs`). Today `scan_tag` keeps the raw tag text (`!c`, `!!str`, `!lang`) and there is no `%TAG`/`%YAML` directive handling. This plan makes the scanner resolve tags against a directive-driven handle map.

**Read these first** (do not re-derive — use them):

- `yaml2/src/scanner/mod.rs`:
  - `Scanner` struct and `Scanner::new` (you add two fields: `tag_handles`, `next_doc_needs_reset`).
  - `fetch_more_tokens` (~line 112): calls `skip_to_next_token`, `stale_simple_key`, then handles indentation and dispatches `scan_content`. You add a directive check here.
  - `scan_content` (~line 195): the `'!' => ... scan_tag` arm, and the `---`/`...` arms (`DocumentStart`/`DocumentEnd`) where you add the per-document reset.
  - `scan_tag` (~line 475) and `take_name` (~line 491): the current tag scanner. You replace `scan_tag` and add `take_tag_word`.
  - Helpers: `indicator_terminator_next()`, `col0`, `consume_line_break()`. `Reader` has `peek()`, `peek_nth(n)`, `advance()`, `position()`.
  - `TokenKind::Tag(String)` is the tag token (the string becomes the resolved tag).
- `yaml2/src/parser.rs`: collects a node's `tag` into events verbatim (no change needed except two test expectations). `yaml2/src/composer.rs`: `classify_tag` already matches `!!str`/`tag:yaml.org,2002:str` etc.; resolved core URIs flow straight through. Non-core/local tags → `None` → string (unchanged behavior).

### Resolution rules (what "correct" means)

The handle map starts each document as `{ "!" → "!", "!!" → "tag:yaml.org,2002:" }`.

- **Verbatim** `!<URI>` → `URI` (the text between `<` and `>`, used as-is). Empty or unterminated → scan error.
- **Secondary handle** `!!suffix` → `tag:yaml.org,2002:` + `suffix` (e.g. `!!str` → `tag:yaml.org,2002:str`).
- **Named handle** `!h!suffix` → lookup `!h!` in the map → `prefix` + `suffix`. Undefined handle → scan error.
- **Primary handle** `!suffix` → `prefix("!")` + `suffix` = `!suffix` (a local tag; e.g. `!lang` → `!lang`).
- **Non-specific** `!` (followed by whitespace/flow/EOF) → `!`.

`%TAG !h! prefix` registers handle `!h!` → `prefix` for the current document. `%YAML major.minor` is accepted and ignored (version compatibility checks are out of scope). Directives apply to the next document only and reset between documents.

Examples:
- `!!str 123` → tag `tag:yaml.org,2002:str` → composer forces a string `"123"`.
- `!<tag:yaml.org,2002:int> 5` → tag `tag:yaml.org,2002:int` → int `5`.
- `!lang x` → tag `!lang` → composer leaves it a string `"x"`.
- `%TAG !e! tag:example.com,2000:` / `--- !e!foo v` → tag `tag:example.com,2000:foo` → string `"v"`.

### Scope / deferrals (locked)

- This plan resolves tags and tracks directives in the scanner. **It does not preserve custom tags in the `Value`** — a custom/local-tagged scalar still composes to a plain string (its tag is dropped), exactly as today. Storing tags on `Value` for round-trip is part of the deferred format-preserving plan.
- `%YAML` version compatibility is not enforced (accepted and ignored).
- Directives produce no tokens (consumed like comments); preserving them for round-trip is deferred.
- No `unsafe` (crate forbids it).

### File structure

- Modify `yaml2/src/scanner/mod.rs` — handle-map state, `scan_tag` rewrite + `take_tag_word` (Task 1); `%TAG`/`%YAML` directives + per-document reset (Task 2).
- Modify `yaml2/src/parser.rs` — two test expectations (Task 1).
- Modify `yaml2/src/composer.rs` — integration tests (Task 3).

---

## Task 1: Structured tag scanning with default handles

**Files:**
- Modify: `yaml2/src/scanner/mod.rs`
- Modify: `yaml2/src/parser.rs`

- [ ] **Step 1: Add the handle-map state.** At the top of `mod.rs`, ensure `HashMap` is imported. The existing line is `use std::collections::VecDeque;`; change it to:

```rust
use std::collections::{HashMap, VecDeque};
```

Add a field to the `Scanner` struct (after `simple_key: Option<SimpleKey>,`):

```rust
    /// Tag handle -> prefix map for the current document (`%TAG` directives).
    tag_handles: HashMap<String, String>,
```

In `Scanner::new`, initialize it (in the struct literal, after `simple_key: None,`):

```rust
            tag_handles: Self::default_tag_handles(),
```

Add this associated function in `impl<'a> Scanner<'a>` (near the other small helpers, e.g. just above `scan_tag`):

```rust
    /// The default tag handle map: primary `!` (local) and secondary `!!`
    /// (the YAML core tag namespace).
    fn default_tag_handles() -> HashMap<String, String> {
        let mut map = HashMap::new();
        map.insert("!".to_string(), "!".to_string());
        map.insert("!!".to_string(), "tag:yaml.org,2002:".to_string());
        map
    }
```

- [ ] **Step 2: Replace `scan_tag` and add `take_tag_word`.** Replace the entire existing `scan_tag` method with:

```rust
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
```

- [ ] **Step 3: Update the `scan_content` dispatch for the new `Result` return.** `scan_tag` now returns `Result<Token>`. Find the `'!'` arm in `scan_content`:

```rust
            '!' => Ok(self.scan_tag(start)),
```
and change it to:

```rust
            '!' => self.scan_tag(start),
```

- [ ] **Step 4: Update the affected scanner test.** In `mod.rs` tests, `double_bang_tag` currently expects `TokenKind::Tag("!!str".to_string())`. Replace that expected value with the resolved URI:

```rust
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
```

(Leave `anchor_alias_tag` (`!c`) and `full_flow_document_tokenizes` (`!lang`) unchanged — local tags with the primary handle resolve to themselves.)

- [ ] **Step 5: Add new scanner tests** (to the `mod.rs` tests module):

```rust
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
```

- [ ] **Step 6: Update the two parser tag-test expectations.** In `yaml2/src/parser.rs` tests, `tag_on_flow_mapping` and `anchor_and_tag_on_block_mapping` assert `tag: Some("!!map".to_string())`. Replace each occurrence of `Some("!!map".to_string())` in those two tests with `Some("tag:yaml.org,2002:map".to_string())`. (There are two occurrences total, one per test.)

- [ ] **Step 7: Run the tests**

Run: `cargo test -p yaml2 scanner::tests` then `cargo test -p yaml2 parser::tests`
Expected: all pass.

- [ ] **Step 8: Full suite + clippy + fmt**

Run: `cargo test -p yaml2` — all pass.
Run: `cargo clippy -p yaml2 --all-targets -- -D warnings` — clean. (`next_doc_needs_reset` is added in Task 2; `tag_handles` is used by `scan_tag` now, so no dead-code issue this task.)
Run: `cargo fmt` then `cargo fmt --check` — no diff.

- [ ] **Step 9: Commit**

```bash
git add yaml2/src/scanner/mod.rs yaml2/src/parser.rs
git commit -m "feat(yaml2): resolve tag shorthand and verbatim tags in the scanner"
```

---

## Task 2: `%TAG` / `%YAML` directives and per-document reset

**Files:**
- Modify: `yaml2/src/scanner/mod.rs`

- [ ] **Step 1: Add the reset-tracking field.** Add to the `Scanner` struct (after `tag_handles`):

```rust
    /// True when the next directive or document marker should reset the handle
    /// map (directives apply to one document only).
    next_doc_needs_reset: bool,
```

Initialize it in `Scanner::new` (after `tag_handles: ...,`):

```rust
            next_doc_needs_reset: true,
```

- [ ] **Step 2: Add the directive scanner and reset helper.** Add these methods in `impl<'a> Scanner<'a>` (near `scan_tag`):

```rust
    /// Resets the tag handle map to its document defaults.
    fn reset_tag_handles(&mut self) {
        self.tag_handles = Self::default_tag_handles();
    }

    /// Scans a directive line (`%YAML ...` or `%TAG ...`) at column 1, updating
    /// the handle map. Directives apply to the upcoming document only, so the
    /// first directive of a new document block resets the map. Produces no token.
    fn scan_directive(&mut self) -> Result<()> {
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
        }
        // `%YAML` and any unknown directive are accepted and ignored.
        // Consume the rest of the line (the line break is left for the caller).
        while !matches!(self.reader.peek(), None | Some('\n') | Some('\r')) {
            self.reader.advance();
        }
        Ok(())
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
```

- [ ] **Step 3: Dispatch directives from `fetch_more_tokens`.** After the `self.skip_to_next_token();` and `self.stale_simple_key();` calls, and **before** the `// Block context: process indentation` block, insert:

```rust
        // A `%` at column 1 in block context begins a directive line.
        if self.flow_depth == 0
            && self.reader.peek() == Some('%')
            && self.reader.position().column == 1
        {
            self.scan_directive()?;
            return Ok(());
        }
```

- [ ] **Step 4: Reset the handle map at document boundaries.** In `scan_content`, the `---` arm (DocumentStart) currently is:

```rust
            '-' if self.marker_ahead("---") => {
                self.unroll_indent(-1);
                self.remove_simple_key();
                self.simple_key_allowed = true;
                Ok(self.scan_marker(TokenKind::DocumentStart, start))
            }
```
Change its body to reset-if-needed and then re-arm for the following document:

```rust
            '-' if self.marker_ahead("---") => {
                self.unroll_indent(-1);
                self.remove_simple_key();
                self.simple_key_allowed = true;
                if self.next_doc_needs_reset {
                    self.reset_tag_handles();
                }
                self.next_doc_needs_reset = true;
                Ok(self.scan_marker(TokenKind::DocumentStart, start))
            }
```

The `...` arm (DocumentEnd) currently is:

```rust
            '.' if self.marker_ahead("...") => {
                self.unroll_indent(-1);
                self.remove_simple_key();
                self.simple_key_allowed = true;
                Ok(self.scan_marker(TokenKind::DocumentEnd, start))
            }
```
Change its body to reset:

```rust
            '.' if self.marker_ahead("...") => {
                self.unroll_indent(-1);
                self.remove_simple_key();
                self.simple_key_allowed = true;
                self.reset_tag_handles();
                self.next_doc_needs_reset = true;
                Ok(self.scan_marker(TokenKind::DocumentEnd, start))
            }
```

- [ ] **Step 5: Add directive tests** (to the `mod.rs` tests module):

```rust
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
```

- [ ] **Step 6: Run the tests**

Run: `cargo test -p yaml2 scanner::tests`
Expected: all pass. If `handles_reset_between_documents` does NOT error, the reset logic is wrong — the `!e!` handle is leaking into the second document; verify the `---` arm resets when `next_doc_needs_reset` is true. Do not weaken the test.

- [ ] **Step 7: Full suite + clippy + fmt**

Run: `cargo test -p yaml2` — all pass.
Run: `cargo clippy -p yaml2 --all-targets -- -D warnings` — clean.
Run: `cargo fmt` then `cargo fmt --check` — no diff.

- [ ] **Step 8: Commit**

```bash
git add yaml2/src/scanner/mod.rs
git commit -m "feat(yaml2): parse %TAG/%YAML directives with per-document scope"
```

---

## Task 3: Composer integration and verification

**Files:**
- Modify: `yaml2/src/composer.rs`

- [ ] **Step 1: Add integration tests** (to the `tests` module in `composer.rs`, which has the `parse(input) -> Value` and `key(s) -> Value` helpers):

```rust
    #[test]
    fn core_shorthand_tag_still_types() {
        // Regression: the resolved `tag:yaml.org,2002:int` types as an int.
        assert_eq!(parse("!!int 5\n").as_int(), Some(5));
    }

    #[test]
    fn verbatim_core_tag_types() {
        assert_eq!(parse("!<tag:yaml.org,2002:int> 5\n").as_int(), Some(5));
        assert_eq!(parse("!<tag:yaml.org,2002:str> 5\n").as_str(), Some("5"));
    }

    #[test]
    fn local_tag_resolves_to_string() {
        // A local `!lang` tag leaves the scalar a string.
        assert_eq!(parse("!lang hello\n").as_str(), Some("hello"));
    }

    #[test]
    fn custom_directive_tag_resolves_to_string() {
        let input = "%TAG !e! tag:example.com,2000:\n--- !e!color red\n";
        assert_eq!(parse(input).as_str(), Some("red"));
    }

    #[test]
    fn directive_scoped_typing_in_mapping() {
        // A core tag via the default secondary handle still types inside a map.
        let v = parse("count: !!int 3\nname: !!str 7\n");
        let m = v.as_mapping().unwrap();
        assert_eq!(m.get(&key("count")).unwrap().as_int(), Some(3));
        assert_eq!(m.get(&key("name")).unwrap().as_str(), Some("7"));
    }
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p yaml2 composer::tests::core_shorthand_tag_still_types composer::tests::verbatim_core_tag_types composer::tests::local_tag_resolves_to_string composer::tests::custom_directive_tag_resolves_to_string composer::tests::directive_scoped_typing_in_mapping`
Expected: all pass. These exercise the full pipeline (scanner resolution → parser → composer typing). If a core-tag typing test fails, confirm `classify_tag` matches the `tag:yaml.org,2002:*` form (it does); do not change the tests.

- [ ] **Step 3: Full crate suite (both feature configs)**

Run: `cargo test -p yaml2` — all pass.
Run: `cargo test -p yaml2 --no-default-features` — all pass.

- [ ] **Step 4: Clippy and formatting**

Run: `cargo clippy -p yaml2 --all-targets -- -D warnings` — clean.
Run: `cargo fmt` then `cargo fmt --check` — no diff.
Run: `git status --short` — empty after commit.

- [ ] **Step 5: Commit**

```bash
git add yaml2/src/composer.rs
git commit -m "test(yaml2): end-to-end tag resolution through the composer"
```

---

## Self-Review

**Spec coverage** (against `docs/superpowers/specs/2026-06-08-yaml2-parser-design.md`):
- Tag handling is part of the composer's stated job ("resolves anchors/aliases ... the schema's scalar resolution"); full tag resolution (handles, verbatim, directives) is delivered here as a focused piece. ✓
- Verbatim, secondary, named, primary, and non-specific tag forms → Task 1. ✓
- `%TAG`/`%YAML` directives with per-document scope → Task 2. ✓
- Core-tag typing preserved end to end; custom/local tags → strings (current behavior) → Task 3. ✓
- Deferred and called out: preserving custom tags on `Value` (format-preserving round-trip plan), `%YAML` version enforcement, directive tokens for round-trip. ✓

**Placeholder scan:** every code step contains complete code; the only test edits are explicit string replacements. No TBD/TODO. ✓

**Type consistency:** `scan_tag` changes return type to `Result<Token>`; its sole caller (the `'!'` arm) is updated in the same task. New helpers `take_tag_word`, `scan_directive`, `take_directive_word`, `skip_inline_blanks`, `default_tag_handles`, `reset_tag_handles` are self-consistent. The `tag_handles` field is introduced and used in Task 1; `next_doc_needs_reset` is introduced and used in Task 2 (no dead-code window). Downstream, the resolved tag strings (`tag:yaml.org,2002:*`) match the composer's existing `classify_tag` arms; only `double_bang_tag` (scanner) and the two parser tag tests change their expected strings. ✓
