# yaml2 Scalar Style Preservation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `preserve_formatting` and `EmitOptions::round_trip` meaningful for scalar quote style — a scalar parsed as single- or double-quoted re-emits in that same style — by populating per-node `Meta` in the composer and honoring it in the emitter.

**Architecture:** This is the first slice of format-preserving round-trip. When `ParseOptions.preserve_formatting` is set, the composer attaches a `Meta { style, span, anchor, tag }` to each scalar `Value`. When `EmitOptions.round_trip` is set, the emitter consults `Meta.style` for string scalars and reproduces single-/double-quoted forms. Default (non-round-trip) emission and non-preserve parsing are byte-for-byte unchanged. Comments, blank lines, block-scalar (`|`/`>`) re-emission, and anchor/alias structural round-trip are deferred to later plans.

**Tech Stack:** Rust 2021. Composer + emitter change. No new dependencies. Builds on the existing `Meta` type, `Value::with_meta`/`meta`, and `ScalarStyle`.

---

## Context for the engineer

You are extending the `yaml2` crate. The read/write core is complete: `parse* → Value → to_string*`. The `Value` type already supports optional per-node metadata (`Value::with_meta(Meta)`, `Value::meta() -> Option<&Meta>`), and `ParseOptions { preserve_formatting: bool }` / `EmitOptions { round_trip: bool }` exist but currently have no effect. This plan wires them up for scalar style.

**Read these first** (do not re-derive — use them):

- `yaml2/src/meta.rs` — `ScalarStyle::{Plain, SingleQuoted, DoubleQuoted, Literal, Folded}` and `Meta { comments, blank_lines_before, style: ScalarStyle, span: Option<Span>, anchor: Option<String>, tag: Option<String> }` (all fields public; `Meta::default()` zeroes them with `style: Plain`).
- `yaml2/src/value.rs` — `Value::with_meta(self, Meta) -> Value`, `Value::meta(&self) -> Option<&Meta>`. **`Value` equality/hashing ignore metadata** — so existing value-equality round-trip tests are unaffected.
- `yaml2/src/composer.rs` — `Composer` holds `options: &ParseOptions`. `compose_node` (~line 104) handles the `EventKind::Scalar { value, style, anchor, tag }` arm; `event.span` is available. Imports currently include `use crate::meta::ScalarStyle;`.
- `yaml2/src/emitter.rs` — `emit`/`emit_documents` take `&EmitOptions`. The block emitters (`emit_block_value`, `emit_block_sequence`, `emit_block_mapping`) thread `options`; the leaf helpers (`scalar_or_flow`, `emit_flow`, `emit_scalar`) currently do **not**. `EmitOptions.round_trip` is the flag to consult. Helpers `needs_quoting`, `double_quote` exist.
- `yaml2/src/options.rs` — `ParseOptions::preserve_formatting()` and `EmitOptions::round_trip()` constructors already exist for tests.

### Behaviour (what "correct" means)

- Parse with `preserve_formatting` off → no `Meta` attached (unchanged). Parse with it on → every scalar `Value` carries `Meta` recording its source `style` (and `span`, `anchor`, `tag`).
- Emit with `round_trip` off → `Meta` ignored; output identical to today. Emit with it on → a **String** scalar whose `Meta.style` is:
  - `SingleQuoted` (and the value has no newline) → `'...'` (single quotes, `'` doubled).
  - `DoubleQuoted` → `"..."` (double quotes with escapes).
  - `Plain`, `Literal`, `Folded`, or no `Meta` → the existing default logic (plain when safe, else double-quoted).
- Non-string scalars (int/bool/float/null) ignore `Meta.style` (they have no meaningful quoting).
- Round-trip: parsing with `preserve_formatting` then emitting with `round_trip` reproduces single-/double-quoted scalars byte-for-byte (modulo escape normalization and the deferred block/comment cases).

### Scope / deferrals (locked)

- Style preservation for **inline** scalar styles (plain/single/double). `Literal`/`Folded` block scalars are recorded in `Meta` but re-emitted via the default logic (not as `|`/`>`); true block re-emission is deferred.
- Comments, blank-line preservation, anchor/alias structural round-trip, and tag re-emission are deferred (aliases are resolved/cloned by the composer, so structural anchor round-trip needs a different `Value` model).
- `Meta` is attached to **scalar** nodes only in this plan; collection-node `Meta` is deferred.
- No `unsafe` (crate forbids it).

### File structure

- Modify `yaml2/src/composer.rs` — populate scalar `Meta` under `preserve_formatting` (Task 1).
- Modify `yaml2/src/emitter.rs` — thread `options` into leaf emitters; honor `Meta.style` under `round_trip` (Task 2).

---

## Task 1: Populate scalar `Meta` in the composer

**Files:**
- Modify: `yaml2/src/composer.rs`

- [ ] **Step 1: Import `Meta`.** Change the meta import line:

```rust
use crate::meta::ScalarStyle;
```
to:

```rust
use crate::meta::{Meta, ScalarStyle};
```

- [ ] **Step 2: Attach `Meta` to scalars when preserving.** Replace the `EventKind::Scalar { .. }` arm of `compose_node` with:

```rust
            EventKind::Scalar {
                value,
                style,
                anchor,
                tag,
            } => {
                let mut composed = self.resolve_scalar(&value, style, tag.as_deref(), event.span)?;
                if self.options.preserve_formatting {
                    composed = composed.with_meta(Meta {
                        style,
                        span: Some(event.span),
                        anchor: anchor.clone(),
                        tag: tag.clone(),
                        ..Meta::default()
                    });
                }
                if let Some(name) = anchor {
                    self.anchors.insert(name, composed.clone());
                }
                Ok(composed)
            }
```

- [ ] **Step 3: Write the tests** (add to the `tests` module in `composer.rs`, which has `parse(input) -> Value` and `key(s) -> Value`):

```rust
    use crate::meta::ScalarStyle;
    use crate::options::ParseOptions;

    fn parse_preserving(input: &str) -> Value {
        crate::api::parse_with(input, &ParseOptions::preserve_formatting()).unwrap()
    }

    #[test]
    fn preserve_records_scalar_style() {
        let v = parse_preserving("a: 'one'\nb: \"two\"\nc: three\n");
        let m = v.as_mapping().unwrap();
        assert_eq!(
            m.get(&key("a")).unwrap().meta().unwrap().style,
            ScalarStyle::SingleQuoted
        );
        assert_eq!(
            m.get(&key("b")).unwrap().meta().unwrap().style,
            ScalarStyle::DoubleQuoted
        );
        assert_eq!(
            m.get(&key("c")).unwrap().meta().unwrap().style,
            ScalarStyle::Plain
        );
    }

    #[test]
    fn preserve_records_span() {
        let v = parse_preserving("hello\n");
        assert!(v.meta().unwrap().span.is_some());
    }

    #[test]
    fn default_parse_attaches_no_meta() {
        // Without preserve_formatting, scalars carry no metadata.
        let v = parse("hello\n");
        assert!(v.meta().is_none());
    }

    #[test]
    fn preserve_does_not_change_value_equality() {
        // Meta is ignored by equality, so the composed values match.
        assert_eq!(parse_preserving("x: 1\n"), parse("x: 1\n"));
    }
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p yaml2 composer::tests::preserve_records_scalar_style composer::tests::preserve_records_span composer::tests::default_parse_attaches_no_meta composer::tests::preserve_does_not_change_value_equality`
Expected: all pass.

- [ ] **Step 5: Full suite + clippy + fmt**

Run: `cargo test -p yaml2` — all pass.
Run: `cargo clippy -p yaml2 --all-targets -- -D warnings` — clean.
Run: `cargo fmt` then `cargo fmt --check` — no diff.

- [ ] **Step 6: Commit**

```bash
git add yaml2/src/composer.rs
git commit -m "feat(yaml2): record scalar style metadata when preserve_formatting is set"
```

---

## Task 2: Honor `Meta.style` in the emitter under `round_trip`

**Files:**
- Modify: `yaml2/src/emitter.rs`

- [ ] **Step 1: Write the failing tests** (add to the `tests` module in `emitter.rs`, which has `to_string(&Value) -> String` and `roundtrip(&str)`):

```rust
    fn to_string_round_trip(input: &str) -> String {
        let opts = crate::options::ParseOptions::preserve_formatting();
        let v = crate::api::parse_with(input, &opts).unwrap();
        crate::api::to_string_with(&v, &EmitOptions::round_trip()).unwrap()
    }

    #[test]
    fn round_trip_preserves_single_quotes() {
        assert_eq!(to_string_round_trip("a: 'hello'\n"), "a: 'hello'\n");
    }

    #[test]
    fn round_trip_preserves_double_quotes() {
        assert_eq!(to_string_round_trip("a: \"hello\"\n"), "a: \"hello\"\n");
    }

    #[test]
    fn round_trip_single_quote_doubles_inner_quote() {
        assert_eq!(to_string_round_trip("a: 'it''s'\n"), "a: 'it''s'\n");
    }

    #[test]
    fn round_trip_leaves_plain_plain() {
        assert_eq!(to_string_round_trip("a: hello\n"), "a: hello\n");
    }

    #[test]
    fn non_round_trip_ignores_style_meta() {
        // Default emit (round_trip off) drops the quotes for a safe plain string,
        // even when the value carries SingleQuoted metadata.
        let opts = crate::options::ParseOptions::preserve_formatting();
        let v = crate::api::parse_with("a: 'hello'\n", &opts).unwrap();
        assert_eq!(crate::api::to_string(&v).unwrap(), "a: hello\n");
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p yaml2 emitter::tests::round_trip_preserves_single_quotes`
Expected: FAIL — the emitter currently ignores `Meta.style`, so `'hello'` emits as plain `hello`.

- [ ] **Step 3: Thread `options` into the leaf emitters and honor `Meta.style`.** Replace the functions `emit_block_value` through `emit_scalar` (the contiguous block) with the versions below. The changes: `emit_block_sequence`/`emit_block_mapping` pass `options` to `scalar_or_flow`; `scalar_or_flow`, `emit_flow`, and `emit_scalar` take `options`; `emit_scalar`'s String case delegates to a new `emit_string` that consults `Meta.style` under `round_trip`.

```rust
/// Emits a value occupying its own line(s) starting at indentation `level`.
/// Non-empty collections render in block style; everything else is a single
/// flow/scalar line.
fn emit_block_value(value: &Value, level: usize, options: &EmitOptions, out: &mut String) {
    match value.data() {
        ValueData::Sequence(items) if !items.is_empty() => {
            emit_block_sequence(items, level, options, out)
        }
        ValueData::Mapping(map) if !map.is_empty() => emit_block_mapping(map, level, options, out),
        _ => {
            out.push_str(&pad(level, options));
            out.push_str(&scalar_or_flow(value, options));
            out.push('\n');
        }
    }
}

/// True for a sequence or mapping with at least one element — these render on
/// their own indented lines rather than inline after a `-` or `key:`.
fn is_block_collection(value: &Value) -> bool {
    match value.data() {
        ValueData::Sequence(items) => !items.is_empty(),
        ValueData::Mapping(map) => !map.is_empty(),
        _ => false,
    }
}

fn emit_block_sequence(items: &[Value], level: usize, options: &EmitOptions, out: &mut String) {
    for item in items {
        out.push_str(&pad(level, options));
        out.push('-');
        if is_block_collection(item) {
            out.push('\n');
            emit_block_value(item, level + 1, options, out);
        } else {
            out.push(' ');
            out.push_str(&scalar_or_flow(item, options));
            out.push('\n');
        }
    }
}

fn emit_block_mapping(
    map: &crate::value::Mapping,
    level: usize,
    options: &EmitOptions,
    out: &mut String,
) {
    for (k, v) in map.iter() {
        out.push_str(&pad(level, options));
        out.push_str(&scalar_or_flow(k, options));
        out.push(':');
        if is_block_collection(v) {
            out.push('\n');
            emit_block_value(v, level + 1, options, out);
        } else {
            out.push(' ');
            out.push_str(&scalar_or_flow(v, options));
            out.push('\n');
        }
    }
}

/// Renders a value to a single-line string: scalars via `emit_scalar`,
/// collections via `emit_flow`.
fn scalar_or_flow(value: &Value, options: &EmitOptions) -> String {
    match value.data() {
        ValueData::Sequence(_) | ValueData::Mapping(_) => emit_flow(value, options),
        _ => emit_scalar(value, options),
    }
}

/// Renders any value to single-line flow form (recursive). Used for empty and
/// inline collections and for non-scalar mapping keys.
fn emit_flow(value: &Value, options: &EmitOptions) -> String {
    match value.data() {
        ValueData::Sequence(items) => {
            let inner: Vec<String> = items.iter().map(|it| emit_flow(it, options)).collect();
            format!("[{}]", inner.join(", "))
        }
        ValueData::Mapping(map) => {
            let inner: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", emit_flow(k, options), emit_flow(v, options)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
        _ => emit_scalar(value, options),
    }
}

/// Renders a scalar value: plain when it re-parses to itself, otherwise
/// double-quoted with escapes. Collection inputs are routed through `emit_flow`
/// by callers, but are handled here defensively too.
fn emit_scalar(value: &Value, options: &EmitOptions) -> String {
    match value.data() {
        ValueData::Null => "null".to_string(),
        ValueData::Bool(true) => "true".to_string(),
        ValueData::Bool(false) => "false".to_string(),
        ValueData::Int(i) => i.to_string(),
        ValueData::Float(f) => format_float(*f),
        ValueData::String(s) => emit_string(s, value, options),
        ValueData::Sequence(_) | ValueData::Mapping(_) => emit_flow(value, options),
    }
}

/// Renders a string scalar. Under `round_trip`, honors a recorded quote style;
/// otherwise (and for plain/block styles) uses the default safe-plain logic.
fn emit_string(s: &str, value: &Value, options: &EmitOptions) -> String {
    if options.round_trip {
        if let Some(meta) = value.meta() {
            match meta.style {
                ScalarStyle::SingleQuoted if !s.contains('\n') => return single_quote(s),
                ScalarStyle::DoubleQuoted => return double_quote(s),
                _ => {}
            }
        }
    }
    if needs_quoting(s) {
        double_quote(s)
    } else {
        s.to_string()
    }
}

/// Single-quotes a string, doubling any embedded single quote.
fn single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}
```

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p yaml2 emitter::tests`
Expected: all pass — the new round-trip tests and all pre-existing emitter tests (which use `round_trip` off and are unaffected).

- [ ] **Step 5: Full suite + clippy + fmt**

Run: `cargo test -p yaml2` — all pass.
Run: `cargo clippy -p yaml2 --all-targets -- -D warnings` — clean.
Run: `cargo fmt` then `cargo fmt --check` — no diff.

- [ ] **Step 6: Commit**

```bash
git add yaml2/src/emitter.rs
git commit -m "feat(yaml2): preserve scalar quote style in round-trip emission"
```

---

## Task 3: Round-trip integration and verification

**Files:**
- Modify: `yaml2/src/emitter.rs`

- [ ] **Step 1: Add integration tests** (to the `tests` module in `emitter.rs`):

```rust
    #[test]
    fn round_trip_mixed_styles_document() {
        let input = "name: 'Ada'\nrole: \"engineer\"\nactive: true\ncount: 3\n";
        assert_eq!(to_string_round_trip(input), input);
    }

    #[test]
    fn round_trip_quoted_inside_sequence() {
        let input = "- 'a'\n- \"b\"\n- c\n";
        assert_eq!(to_string_round_trip(input), input);
    }

    #[test]
    fn round_trip_preserves_quoted_keys() {
        let input = "'a key': 1\n";
        assert_eq!(to_string_round_trip(input), input);
    }

    #[test]
    fn round_trip_value_is_stable_even_for_block_scalars() {
        // Block scalars are not re-emitted as `|` yet (deferred), but the VALUE
        // must still round-trip: parse(emit(parse(x))) == parse(x).
        let opts = crate::options::ParseOptions::preserve_formatting();
        let v1 = crate::api::parse_with("text: |\n  a\n  b\n", &opts).unwrap();
        let emitted = crate::api::to_string_with(&v1, &EmitOptions::round_trip()).unwrap();
        let v2 = crate::api::parse_with(&emitted, &opts).unwrap();
        assert_eq!(v1, v2);
    }
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p yaml2 emitter::tests::round_trip_mixed_styles_document emitter::tests::round_trip_quoted_inside_sequence emitter::tests::round_trip_preserves_quoted_keys emitter::tests::round_trip_value_is_stable_even_for_block_scalars`
Expected: all pass. If `round_trip_preserves_quoted_keys` fails, confirm `Meta` is attached to mapping **keys** too (keys are composed via `compose_node`, so they are) and that `scalar_or_flow(k, options)` is used in `emit_block_mapping`.

- [ ] **Step 3: Full crate suite (both feature configs)**

Run: `cargo test -p yaml2` — all pass.
Run: `cargo test -p yaml2 --no-default-features` — all pass.

- [ ] **Step 4: Clippy and formatting**

Run: `cargo clippy -p yaml2 --all-targets -- -D warnings` — clean.
Run: `cargo fmt` then `cargo fmt --check` — no diff.
Run: `git status --short` — empty after commit.

- [ ] **Step 5: Commit**

```bash
git add yaml2/src/emitter.rs
git commit -m "test(yaml2): round-trip integration for scalar style preservation"
```

---

## Self-Review

**Spec coverage** (against `docs/superpowers/specs/2026-06-08-yaml2-parser-design.md`):
- "Round-trip — optional per-node metadata, gated by `preserve_formatting`" → Task 1 (composer populates `Meta`) and Task 2 (emitter honors it under `round_trip`). ✓
- "with `preserve_formatting`, output is byte-stable except for intentional edits" → delivered for inline scalar quote style (Tasks 2–3); block scalars, comments, and blank lines are explicitly deferred and called out, with value-stability verified for block scalars in Task 3. ✓
- Default behavior (no preserve / no round_trip) unchanged → guaranteed by gating both sides on the flags; verified by the existing suite plus `default_parse_attaches_no_meta` and `non_round_trip_ignores_style_meta`. ✓
- Deferred and called out: block-scalar re-emission, comments, blank lines, anchor/alias structural round-trip, tag re-emission, collection-node `Meta`. ✓

**Placeholder scan:** every code step contains complete code. No TBD/TODO. ✓

**Type consistency:** the composer `Scalar` arm uses `Meta { style, span, anchor, tag, ..Meta::default() }` matching the `Meta` field names/types; `style`/`anchor`/`tag` come from the destructured event, `event.span` is `Span` wrapped in `Some`. In the emitter, `scalar_or_flow`/`emit_flow`/`emit_scalar` gain a `&EmitOptions` parameter consistently, and all call sites (`emit_block_value`, `emit_block_sequence`, `emit_block_mapping`, and the recursive `emit_flow` closures) pass it. `emit_string` and `single_quote` are new self-consistent helpers; `emit_string` reads `value.meta()` (returns `Option<&Meta>`) and matches on `meta.style: ScalarStyle`. Pre-existing emitter tests call `to_string`/`to_string_with` with `round_trip` off, so they see no behavior change. ✓
