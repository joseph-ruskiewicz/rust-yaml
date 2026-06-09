# yaml2 Tab-as-Indentation Rejection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reject tab characters used as indentation in block context (which YAML 1.2 forbids), while continuing to accept tabs as inter-token separation, inside flow context, on blank/comment lines, and within scalar content.

**Architecture:** The scanner already skips tabs as generic whitespace. Add two pieces of state — whether the cursor is in a line's *leading* whitespace (`at_line_start`) and whether a tab has appeared in that leading run (`tab_in_indent`) — updated in `skip_to_next_token`. Before scanning a block-context content token, if a leading tab was seen, return a `Scan` error. Tabs encountered after the first token on a line (separation), in flow context, or on lines that resolve to blanks/comments never trigger the error.

**Tech Stack:** Rust 2021. Scanner-only change. No new dependencies.

---

## Context for the engineer

You are extending the `yaml2` crate's scanner (`yaml2/src/scanner/mod.rs`). Today tabs are treated exactly like spaces in `skip_to_next_token`, so `\tkey: value` (a tab-indented line) is silently accepted. YAML forbids tabs for indentation.

**Read these first** (do not re-derive — use them):

- `yaml2/src/scanner/mod.rs`:
  - `Scanner` struct + `Scanner::new` — you add two `bool` fields.
  - `fetch_more_tokens` (~line 119): after `skip_to_next_token()` / `stale_simple_key()` there is a `%`-directive check, then `// Block context: process indentation`. You insert the tab check between the directive check and the indentation block.
  - `skip_to_next_token` (~line 185): the loop matching `Some(' ') | Some('\t')`, line breaks, `#` comments. You split the space/tab arm and update the new state.
  - `flow_depth: usize` (0 = block context). `Reader`: `peek()`, `advance()`, `position()`. `Error::new(ErrorKind::Scan, msg).with_span(Span::new(pos, pos))`.

### Rules (what "correct" means)

A tab is rejected **only** when it is part of the leading whitespace of a block-context line that then begins a content token. Specifically:
- Reject: `\tkey: v`, `  \tkey: v` (tab in the indentation run before content).
- Allow: `key:\tvalue` (tab as separation after a token on the same line).
- Allow: tabs anywhere in **flow** context (`[\n\ta, b]`).
- Allow: a tab on a **blank** line (`a: 1\n\t\nb: 2`) — the trailing break resets the state.
- Allow: a tab before a **comment** (`a: 1\n\t# c\nb: 2`) — the comment consumes to the break, which resets the state.
- Allow: tabs inside scalar content and block scalars (those are not scanned via `skip_to_next_token`, so they are unaffected).

### Scope / deferrals (locked)

- Block-context leading-tab rejection only. Tabs in other positions remain accepted (matching YAML, which only forbids tabs *for indentation*).
- This does not attempt to reject every spec-illegal tab placement (e.g. a tab between `-` and a value); those are rare and out of scope.
- No `unsafe` (crate forbids it).

### File structure

- Modify `yaml2/src/scanner/mod.rs` — add state, update `skip_to_next_token`, add the rejection check in `fetch_more_tokens` (Task 1); composer-level integration test (Task 2).

---

## Task 1: Reject leading tabs in block context

**Files:**
- Modify: `yaml2/src/scanner/mod.rs`

- [ ] **Step 1: Add the state fields.** Add to the `Scanner` struct (after the existing fields, e.g. after `next_doc_needs_reset: bool,`):

```rust
    /// True while the cursor is in the leading whitespace of a line (before any
    /// token on that line). Used to detect tab indentation.
    at_line_start: bool,
    /// True when a tab has appeared in the current line's leading whitespace.
    tab_in_indent: bool,
```

Initialize them in `Scanner::new` (after the corresponding field initializers):

```rust
            at_line_start: true,
            tab_in_indent: false,
```

- [ ] **Step 2: Update `skip_to_next_token`** to track the new state. Replace the body of the loop's match so the four arms read:

```rust
            match self.reader.peek() {
                Some(' ') => {
                    self.reader.advance();
                }
                Some('\t') => {
                    if self.flow_depth == 0 && self.at_line_start {
                        self.tab_in_indent = true;
                    }
                    self.reader.advance();
                }
                Some('\n') | Some('\r') => {
                    self.reader.advance();
                    if self.flow_depth == 0 {
                        self.simple_key_allowed = true;
                    }
                    self.at_line_start = true;
                    self.tab_in_indent = false;
                }
                Some('#') => {
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
```

(The only changes from the current code: the `Some(' ') | Some('\t')` arm is split so the tab arm can set `tab_in_indent`; the line-break arm now also sets `at_line_start = true` and `tab_in_indent = false`; the final `_ => break` arm now sets `at_line_start = false` before breaking.)

- [ ] **Step 3: Add the rejection check in `fetch_more_tokens`.** Immediately after the `%`-directive `if` block and **before** the `// Block context: process indentation` comment, insert:

```rust
        // Tabs may not be used for indentation in block context.
        if self.flow_depth == 0 && self.tab_in_indent && self.reader.peek().is_some() {
            let pos = self.reader.position();
            return Err(Error::new(
                ErrorKind::Scan,
                "tabs cannot be used for indentation",
            )
            .with_span(Span::new(pos, pos)));
        }
```

- [ ] **Step 4: Write the tests** (add to the `tests` module in `mod.rs`):

```rust
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
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p yaml2 scanner::tests`
Expected: all pass — the new tab tests and all pre-existing scanner tests. If a pre-existing test regresses, it likely used a literal tab as indentation in its input (unlikely); investigate the input and report — do not blindly change it.

- [ ] **Step 6: Full suite + clippy + fmt**

Run: `cargo test -p yaml2` — all pass.
Run: `cargo clippy -p yaml2 --all-targets -- -D warnings` — clean.
Run: `cargo fmt` then `cargo fmt --check` — no diff.

- [ ] **Step 7: Commit**

```bash
git add yaml2/src/scanner/mod.rs
git commit -m "feat(yaml2): reject tab characters used as block indentation"
```

---

## Task 2: Integration and verification

**Files:**
- Modify: `yaml2/src/composer.rs`

- [ ] **Step 1: Add integration tests** (to the `tests` module in `composer.rs`, which has the `parse(input) -> Value`/`key(s) -> Value` helpers; these tests call the public API directly):

```rust
    #[test]
    fn tab_indented_document_is_an_error() {
        let err = crate::api::parse("root:\n\tchild: v\n").unwrap_err();
        assert_eq!(err.kind(), crate::error::ErrorKind::Scan);
    }

    #[test]
    fn space_indented_document_still_parses() {
        // Regression: ordinary space indentation is unaffected.
        let v = crate::api::parse("root:\n  child: v\n");
        let m = v.as_mapping().unwrap();
        let child = m.get(&key("root")).unwrap().as_mapping().unwrap();
        assert_eq!(child.get(&key("child")).unwrap().as_str(), Some("v"));
    }
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p yaml2 composer::tests::tab_indented_document_is_an_error composer::tests::space_indented_document_still_parses`
Expected: both pass.

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
git commit -m "test(yaml2): reject tab-indented documents end to end"
```

---

## Self-Review

**Spec coverage** (against `docs/superpowers/specs/2026-06-08-yaml2-parser-design.md`):
- Tab-as-indentation rejection was listed among the hardening items; delivered here as a focused, self-contained piece. ✓
- YAML 1.2 rule "indentation uses spaces, never tabs" enforced in block context, with tabs still allowed as separation / in flow / in content → Task 1, verified end to end in Task 2. ✓
- Deferred and called out: exhaustive illegal-tab-placement detection (e.g. tab after `-`), the yaml-test-suite gate, fuzzing, format-preserving round-trip. ✓

**Placeholder scan:** every code step contains complete code. No TBD/TODO. ✓

**Type consistency:** the two new `Scanner` fields `at_line_start`/`tab_in_indent` are introduced and used together in Task 1 (set in `skip_to_next_token`, read in `fetch_more_tokens`) — no dead-code window. No signatures change. Test helpers `kinds`/`tokenize` (scanner) and `parse`/`key` (composer) already exist. ✓
