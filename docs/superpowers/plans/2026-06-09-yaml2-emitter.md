# yaml2 Emitter (Value tree → YAML text) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Serialize an owned `Value` tree back to valid YAML text that round-trips (`parse → emit → parse` preserves the tree), and expose the public `to_string` / `to_string_documents` entry points.

**Architecture:** A new `emitter` module renders a `Value` recursively. Leaf scalars are emitted plain when safe and double-quoted (with escapes) otherwise, decided by a Core-1.2 resolution oracle plus structural-hazard checks so the text re-parses to the identical value. Collections render in block style by default with configurable indent; empty collections and non-scalar mapping keys use a compact flow form. The `api` module wires `emit` into the crate's public surface, mirroring the existing `parse` functions.

**Tech Stack:** Rust 2021. Reuses `crate::scalar::resolve` as the quoting oracle. No new dependencies.

---

## Context for the engineer

You are extending the `yaml2` crate. The read pipeline is complete: `bytes → scanner → parser → composer → Value`. This plan adds the inverse leaf: `Value → text`.

**Read these first** (do not re-derive — use them):

- `yaml2/src/value.rs` — what you serialize. `Value::data() -> &ValueData`. `ValueData::{Null, Bool(bool), Int(i64), Float(f64), String(String), Sequence(Vec<Value>), Mapping(Mapping)}`. `Mapping::iter() -> impl Iterator<Item = (&Value, &Value)>`, `Mapping::len()`, `Mapping::is_empty()`. Constructors `Value::{null,bool,int,float,string,sequence,mapping}` (used in tests). `Value` implements `PartialEq`/`Eq` **by data, order-sensitive for mappings** — that is the round-trip oracle.
- `yaml2/src/options.rs` — `EmitOptions { round_trip: bool, indent: usize }`, `Default` = `{ round_trip: false, indent: 2 }`, plus `EmitOptions::round_trip()`. `Schema::Core1_2`.
- `yaml2/src/scalar.rs` — `pub(crate) fn resolve(raw, style, schema) -> ValueData`. Empty plain string resolves to `Null`; quoted styles are always `String`. You use `resolve(s, ScalarStyle::Plain, Schema::Core1_2)` to decide whether a string can be emitted plain without changing type on reparse.
- `yaml2/src/meta.rs` — `ScalarStyle::Plain` (needed only as the argument to `resolve`).
- `yaml2/src/error.rs` — `Result<T>`. The public API returns `Result<String>` for signature symmetry with `parse`, even though emission does not currently fail.
- `yaml2/src/api.rs` — the existing `parse`/`parse_with`/`parse_documents`/`parse_documents_with`. You will add the `to_string*` siblings here.
- `yaml2/src/composer.rs` / `yaml2/src/scanner/mod.rs` — for reference only. The scanner's double-quoted reader supports the escapes `\" \\ \n \t \r \xXX \uXXXX` (among others), so the emitter's double-quoting is safe to reparse.

### Design decisions locked for this plan

- **Round-trip target is *semantic*, not byte-stable.** `parse(emit(parse(x))) == parse(x)`. Byte-stable, format-preserving round-trip needs per-node `Meta` (styles, comments) which the composer does not yet populate and the scanner does not yet capture; that is deferred to a later plan. The `EmitOptions.round_trip` flag therefore has no behavioural effect yet — it is reserved. Do not try to read `Meta` in this plan.
- **Quoting oracle is Core 1.2.** A string is emitted plain only if `resolve(s, Plain, Core1_2)` returns exactly `String(s)` *and* `s` has no structural hazard. This guarantees stability when the emitted text is re-parsed under the default (Core) schema. Cross-schema round-trip (e.g. emitting for a `Yaml1_1` reader) would need extra quoting and is out of scope; tests parse and emit under default options.
- **Block style by default**, indent width from `EmitOptions.indent`. Nested non-empty collections are placed on the following line, indented one level. Empty collections render as `[]` / `{}`. Non-scalar mapping keys render inline in flow form.
- **Strings are quoted with double quotes** (never single) when quoting is needed, escaping `" \ \n \t \r` and other control characters as `\xXX`.
- **`to_string` is single-document.** Multi-document output uses `to_string_documents`, separating documents with a `---` line.

### File structure

- Create `yaml2/src/emitter.rs` — the `Value → String` renderer. Owns `pub(crate) fn emit(&Value, &EmitOptions) -> String`, `pub(crate) fn emit_documents(&[Value], &EmitOptions) -> String`, and all rendering helpers. One responsibility: serialization.
- Modify `yaml2/src/api.rs` — add `to_string`, `to_string_with`, `to_string_documents`, `to_string_documents_with`.
- Modify `yaml2/src/lib.rs` — declare `mod emitter;` (alphabetical: between `composer` and `error`) and re-export the four `to_string*` functions.

---

## Task 1: Emitter scaffold — scalar and flow emission, single-document API

This task produces a working emitter that renders **everything in flow style** (collections on one line). Block style for collections is layered on in Task 2. Flow output already round-trips, so each task ships stable behaviour.

**Files:**
- Create: `yaml2/src/emitter.rs`
- Modify: `yaml2/src/api.rs`
- Modify: `yaml2/src/lib.rs`

- [ ] **Step 1: Create `yaml2/src/emitter.rs`:**

```rust
//! The emitter: owned `Value` tree -> YAML text.

use crate::meta::ScalarStyle;
use crate::options::{EmitOptions, Schema};
use crate::value::{Value, ValueData};

/// Renders a single value as a YAML document (always ends with a newline).
pub(crate) fn emit(value: &Value, options: &EmitOptions) -> String {
    let mut out = String::new();
    emit_block_value(value, 0, options, &mut out);
    out
}

/// Indentation for `level` nesting levels, `options.indent` spaces each.
fn pad(level: usize, options: &EmitOptions) -> String {
    " ".repeat(level * options.indent)
}

/// Emits a value occupying its own line(s) starting at indentation `level`.
/// Task 1 renders every value as a single flow line; Task 2 adds block forms.
fn emit_block_value(value: &Value, level: usize, options: &EmitOptions, out: &mut String) {
    out.push_str(&pad(level, options));
    out.push_str(&scalar_or_flow(value));
    out.push('\n');
}

/// Renders a value to a single-line string: scalars via `emit_scalar`,
/// collections via `emit_flow`.
fn scalar_or_flow(value: &Value) -> String {
    match value.data() {
        ValueData::Sequence(_) | ValueData::Mapping(_) => emit_flow(value),
        _ => emit_scalar(value),
    }
}

/// Renders any value to single-line flow form (recursive). Used for empty and
/// inline collections and for non-scalar mapping keys.
fn emit_flow(value: &Value) -> String {
    match value.data() {
        ValueData::Sequence(items) => {
            let inner: Vec<String> = items.iter().map(emit_flow).collect();
            format!("[{}]", inner.join(", "))
        }
        ValueData::Mapping(map) => {
            let inner: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", emit_flow(k), emit_flow(v)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
        _ => emit_scalar(value),
    }
}

/// Renders a scalar value: plain when it re-parses to itself, otherwise
/// double-quoted with escapes. Panics-free: collection inputs are routed
/// through `emit_flow` by callers, but are handled here defensively too.
fn emit_scalar(value: &Value) -> String {
    match value.data() {
        ValueData::Null => "null".to_string(),
        ValueData::Bool(true) => "true".to_string(),
        ValueData::Bool(false) => "false".to_string(),
        ValueData::Int(i) => i.to_string(),
        ValueData::Float(f) => format_float(*f),
        ValueData::String(s) => {
            if needs_quoting(s) {
                double_quote(s)
            } else {
                s.clone()
            }
        }
        ValueData::Sequence(_) | ValueData::Mapping(_) => emit_flow(value),
    }
}

/// Formats a float so it always re-parses as a float (never an int).
fn format_float(f: f64) -> String {
    if f.is_nan() {
        return ".nan".to_string();
    }
    if f.is_infinite() {
        return if f < 0.0 { "-.inf" } else { ".inf" }.to_string();
    }
    let s = format!("{f}");
    if s.contains('.') || s.contains('e') || s.contains('E') {
        s
    } else {
        format!("{s}.0")
    }
}

/// True when a string cannot be safely emitted as a plain scalar — either it
/// would change type on reparse, or it contains a structural hazard.
fn needs_quoting(s: &str) -> bool {
    if s.is_empty() {
        return true; // empty plain scalar parses as null
    }
    // Would a plain emission re-resolve to a different type?
    match crate::scalar::resolve(s, ScalarStyle::Plain, Schema::Core1_2) {
        ValueData::String(ref t) if t == s => {}
        _ => return true,
    }
    let first = s.as_bytes()[0];
    let leading_indicator = matches!(
        first,
        b'!' | b'&'
            | b'*'
            | b'?'
            | b'-'
            | b':'
            | b','
            | b'['
            | b']'
            | b'{'
            | b'}'
            | b'#'
            | b'|'
            | b'>'
            | b'@'
            | b'`'
            | b'"'
            | b'\''
            | b'%'
    );
    if leading_indicator {
        return true;
    }
    if s.starts_with(' ') || s.ends_with(' ') {
        return true;
    }
    if s.ends_with(':') || s.contains(": ") || s.contains(" #") {
        return true;
    }
    // Flow-indicator characters are unsafe inside flow context.
    if s.contains([',', '[', ']', '{', '}']) {
        return true;
    }
    if s.bytes().any(|b| b.is_ascii_control()) {
        return true;
    }
    false
}

/// Double-quotes a string, escaping quotes, backslashes, and control chars.
fn double_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
```

- [ ] **Step 2: Add the `to_string` entry points to `yaml2/src/api.rs`.** Append to the existing `use` block and functions:

Add this import near the top of `api.rs` (with the other `use crate::...` lines):

```rust
use crate::emitter::emit;
use crate::options::EmitOptions;
```

Add these functions at the end of `api.rs`:

```rust
/// Serializes a single value to a YAML document string using default options.
pub fn to_string(value: &Value) -> Result<String> {
    to_string_with(value, &EmitOptions::default())
}

/// Like [`to_string`], with explicit options.
pub fn to_string_with(value: &Value, options: &EmitOptions) -> Result<String> {
    Ok(emit(value, options))
}
```

(`Value` and `Result` are already imported in `api.rs`.)

- [ ] **Step 3: Wire `yaml2/src/lib.rs`.** Add `mod emitter;` between `mod composer;` and `mod error;`, and extend the `api` re-export. The two changed lines become:

```rust
mod composer;
mod emitter;
mod error;
```

and

```rust
pub use api::{parse, parse_documents, parse_documents_with, parse_with, to_string, to_string_with};
```

- [ ] **Step 4: Add the test module to the end of `yaml2/src/emitter.rs`:**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn to_string(value: &Value) -> String {
        crate::api::to_string(value).unwrap()
    }

    /// parse -> emit -> parse must preserve the value.
    fn roundtrip(input: &str) {
        let v = crate::api::parse(input).unwrap();
        let text = crate::api::to_string(&v).unwrap();
        let v2 = crate::api::parse(&text).unwrap();
        assert_eq!(v, v2, "roundtrip changed value; emitted:\n{text}");
    }

    #[test]
    fn emits_plain_string() {
        assert_eq!(to_string(&Value::string("hello")), "hello\n");
    }

    #[test]
    fn emits_int_and_null_and_bool() {
        assert_eq!(to_string(&Value::int(42)), "42\n");
        assert_eq!(to_string(&Value::null()), "null\n");
        assert_eq!(to_string(&Value::bool(true)), "true\n");
    }

    #[test]
    fn float_always_has_point() {
        assert_eq!(to_string(&Value::float(5.0)), "5.0\n");
        assert_eq!(to_string(&Value::float(1.5)), "1.5\n");
        assert_eq!(to_string(&Value::float(f64::INFINITY)), ".inf\n");
    }

    #[test]
    fn quotes_strings_that_would_change_type() {
        assert_eq!(to_string(&Value::string("42")), "\"42\"\n");
        assert_eq!(to_string(&Value::string("true")), "\"true\"\n");
        assert_eq!(to_string(&Value::string("null")), "\"null\"\n");
        assert_eq!(to_string(&Value::string("")), "\"\"\n");
    }

    #[test]
    fn quotes_structurally_hazardous_strings() {
        assert_eq!(to_string(&Value::string("a: b")), "\"a: b\"\n");
        assert_eq!(to_string(&Value::string("# c")), "\"# c\"\n");
        assert_eq!(to_string(&Value::string(" x")), "\" x\"\n");
    }

    #[test]
    fn flow_collections_roundtrip() {
        roundtrip("[1, 2, 3]\n");
        roundtrip("{a: 1, b: 2}\n");
        roundtrip("a: 1\nb: 2\n");
        roundtrip("- a\n- b\n");
    }

    #[test]
    fn scalar_strings_roundtrip() {
        roundtrip("plain text\n");
        roundtrip("\"quoted: value\"\n");
        roundtrip("\"line\\nbreak\"\n");
    }
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p yaml2 emitter::tests`
Expected: all pass. (Collections currently emit in flow, e.g. `to_string(parse("a: 1\nb: 2"))` yields `"{a: 1, b: 2}\n"`, which round-trips.)

- [ ] **Step 6: Full suite + clippy + fmt**

Run: `cargo test -p yaml2` — all pass.
Run: `cargo clippy -p yaml2 --all-targets -- -D warnings` — clean.
Run: `cargo fmt` then `cargo fmt --check` — no diff.

- [ ] **Step 7: Commit**

```bash
git add yaml2/src/emitter.rs yaml2/src/api.rs yaml2/src/lib.rs
git commit -m "feat(yaml2): emit scalars and flow collections with to_string"
```

---

## Task 2: Block-style collections

Switch non-empty sequences and mappings from flow to block style, indented per `EmitOptions.indent`. Empty collections stay flow (`[]`/`{}`); non-scalar keys stay flow inline.

**Files:**
- Modify: `yaml2/src/emitter.rs`

- [ ] **Step 1: Write the failing tests** (add to the `tests` module):

```rust
    #[test]
    fn block_sequence_layout() {
        let v = crate::api::parse("- a\n- b\n").unwrap();
        assert_eq!(to_string(&v), "- a\n- b\n");
    }

    #[test]
    fn block_mapping_layout() {
        let v = crate::api::parse("a: 1\nb: 2\n").unwrap();
        assert_eq!(to_string(&v), "a: 1\nb: 2\n");
    }

    #[test]
    fn nested_mapping_layout() {
        let v = crate::api::parse("outer:\n  inner: 7\n").unwrap();
        assert_eq!(to_string(&v), "outer:\n  inner: 7\n");
    }

    #[test]
    fn mapping_in_sequence_layout() {
        let v = crate::api::parse("- a: 1\n  b: 2\n").unwrap();
        // Collection entries are placed on the following line, indented.
        assert_eq!(to_string(&v), "-\n  a: 1\n  b: 2\n");
    }

    #[test]
    fn empty_collections_stay_flow() {
        assert_eq!(to_string(&Value::sequence(vec![])), "[]\n");
        assert_eq!(to_string(&Value::mapping(crate::value::Mapping::new())), "{}\n");
    }

    #[test]
    fn complex_key_is_flow_inline() {
        // A sequence used as a mapping key renders in flow form before the colon.
        let mut m = crate::value::Mapping::new();
        m.insert(
            Value::sequence(vec![Value::int(1), Value::int(2)]),
            Value::string("v"),
        );
        assert_eq!(to_string(&Value::mapping(m)), "[1, 2]: v\n");
    }

    #[test]
    fn deep_structure_roundtrips() {
        roundtrip("a:\n  - 1\n  - 2\nb:\n  c: d\n");
        roundtrip("- - 1\n  - 2\n- x\n");
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p yaml2 emitter::tests::block_sequence_layout`
Expected: FAIL — current output is flow (`[a, b]\n`), not block.

- [ ] **Step 3: Replace `emit_block_value` and add the block renderers.** Replace the entire `emit_block_value` function with:

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
            out.push_str(&scalar_or_flow(value));
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
            out.push_str(&scalar_or_flow(item));
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
        out.push_str(&scalar_or_flow(k));
        out.push(':');
        if is_block_collection(v) {
            out.push('\n');
            emit_block_value(v, level + 1, options, out);
        } else {
            out.push(' ');
            out.push_str(&scalar_or_flow(v));
            out.push('\n');
        }
    }
}
```

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p yaml2 emitter::tests`
Expected: all pass (the Task 1 `flow_collections_roundtrip` test still passes because block output also round-trips).

- [ ] **Step 5: Full suite + clippy + fmt**

Run: `cargo test -p yaml2` — all pass.
Run: `cargo clippy -p yaml2 --all-targets -- -D warnings` — clean.
Run: `cargo fmt` then `cargo fmt --check` — no diff.

- [ ] **Step 6: Commit**

```bash
git add yaml2/src/emitter.rs
git commit -m "feat(yaml2): emit collections in block style"
```

---

## Task 3: Multi-document output and the indent option

**Files:**
- Modify: `yaml2/src/emitter.rs`
- Modify: `yaml2/src/api.rs`
- Modify: `yaml2/src/lib.rs`

- [ ] **Step 1: Write the failing tests** (add to the `tests` module in `emitter.rs`):

```rust
    #[test]
    fn custom_indent_width() {
        let v = crate::api::parse("outer:\n  inner: 7\n").unwrap();
        let opts = EmitOptions {
            indent: 4,
            ..Default::default()
        };
        let text = crate::api::to_string_with(&v, &opts).unwrap();
        assert_eq!(text, "outer:\n    inner: 7\n");
    }

    #[test]
    fn documents_are_separated() {
        let docs = vec![Value::int(1), Value::int(2)];
        let text = crate::api::to_string_documents(&docs).unwrap();
        assert_eq!(text, "1\n---\n2\n");
    }

    #[test]
    fn empty_document_list_is_empty_string() {
        assert_eq!(crate::api::to_string_documents(&[]).unwrap(), "");
    }

    #[test]
    fn documents_roundtrip() {
        let docs = crate::api::parse_documents("--- a\n--- b\n").unwrap();
        let text = crate::api::to_string_documents(&docs).unwrap();
        let docs2 = crate::api::parse_documents(&text).unwrap();
        assert_eq!(docs, docs2);
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p yaml2 emitter::tests::documents_are_separated`
Expected: FAIL — `to_string_documents` does not exist yet (compile error).

- [ ] **Step 3: Add `emit_documents` to `emitter.rs`** (after `emit`):

```rust
/// Renders multiple values as a multi-document stream, separating documents
/// with a `---` line. An empty slice yields an empty string.
pub(crate) fn emit_documents(values: &[Value], options: &EmitOptions) -> String {
    let mut out = String::new();
    for (i, value) in values.iter().enumerate() {
        if i > 0 {
            out.push_str("---\n");
        }
        emit_block_value(value, 0, options, &mut out);
    }
    out
}
```

- [ ] **Step 4: Add the public functions to `yaml2/src/api.rs`.** Extend the emitter import:

```rust
use crate::emitter::{emit, emit_documents};
```

Add at the end of `api.rs`:

```rust
/// Serializes multiple values to a multi-document YAML stream using default options.
pub fn to_string_documents(values: &[Value]) -> Result<String> {
    to_string_documents_with(values, &EmitOptions::default())
}

/// Like [`to_string_documents`], with explicit options.
pub fn to_string_documents_with(values: &[Value], options: &EmitOptions) -> Result<String> {
    Ok(emit_documents(values, options))
}
```

- [ ] **Step 5: Update the `api` re-export in `yaml2/src/lib.rs`** to include the new functions:

```rust
pub use api::{
    parse, parse_documents, parse_documents_with, parse_with, to_string, to_string_documents,
    to_string_documents_with, to_string_with,
};
```

- [ ] **Step 6: Run to verify they pass**

Run: `cargo test -p yaml2 emitter::tests`
Expected: all pass.

- [ ] **Step 7: Full suite + clippy + fmt**

Run: `cargo test -p yaml2` — all pass.
Run: `cargo clippy -p yaml2 --all-targets -- -D warnings` — clean.
Run: `cargo fmt` then `cargo fmt --check` — no diff.

- [ ] **Step 8: Commit**

```bash
git add yaml2/src/emitter.rs yaml2/src/api.rs yaml2/src/lib.rs
git commit -m "feat(yaml2): emit multi-document streams and honor indent option"
```

---

## Task 4: Round-trip integration and full verification

**Files:**
- Modify: `yaml2/src/emitter.rs`

- [ ] **Step 1: Write the integration tests** (add to the `tests` module):

```rust
    #[test]
    fn realistic_document_roundtrips() {
        let input = "\
name: Ada
active: true
scores:
  - 10
  - 20
address:
  city: Portland
  zip: \"97201\"
tags: [x, y, z]
";
        roundtrip(input);
    }

    #[test]
    fn mapping_with_empty_and_null_roundtrips() {
        roundtrip("a:\nb: []\nc: {}\n");
    }

    #[test]
    fn special_strings_roundtrip() {
        roundtrip("- \"\"\n- \"true\"\n- \"42\"\n- \"a: b\"\n- \" leading\"\n");
    }

    #[test]
    fn key_order_is_preserved() {
        let v = crate::api::parse("z: 1\na: 2\nm: 3\n").unwrap();
        let text = crate::api::to_string(&v).unwrap();
        assert_eq!(text, "z: 1\na: 2\nm: 3\n");
    }

    #[test]
    fn deeply_nested_roundtrips() {
        roundtrip("a:\n  b:\n    c:\n      - 1\n      - d: e\n");
    }
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p yaml2 emitter::tests::realistic_document_roundtrips emitter::tests::mapping_with_empty_and_null_roundtrips emitter::tests::special_strings_roundtrip emitter::tests::key_order_is_preserved emitter::tests::deeply_nested_roundtrips`
Expected: all pass. If any round-trip fails, print the emitted text (the `roundtrip` helper already includes it in the assert message), diagnose whether it is a quoting gap or a layout gap, and fix the emitter — do not weaken the test.

- [ ] **Step 3: Full crate suite**

Run: `cargo test -p yaml2`
Expected: all pass.

- [ ] **Step 4: Clippy and formatting**

Run: `cargo clippy -p yaml2 --all-targets -- -D warnings` — clean.
Run: `cargo fmt` then `cargo fmt --check` — no diff.
Run: `git status --short` — empty after commit.

- [ ] **Step 5: Commit**

```bash
git add yaml2/src/emitter.rs
git commit -m "test(yaml2): round-trip integration tests for the emitter"
```

---

## Self-Review

**Spec coverage** (against `docs/superpowers/specs/2026-06-08-yaml2-parser-design.md`):
- "emitter inverse" of the pipeline; `to_string_with(&Value, &EmitOptions) -> Result<String>` → Tasks 1–3. ✓
- `EmitOptions` (`round_trip`, `indent`) honored: `indent` in Task 2/3; `round_trip` reserved (documented — needs `Meta`, deferred). ✓
- Round-trip property `parse → emit → parse` stable → Tasks 1–4 via the `roundtrip` helper; ordered mappings preserve key order (Task 4 pins it). ✓
- Deferred and called out: byte-stable / format-preserving round-trip with comments and original styles (needs `Meta` population + comment capture), single-quoted/literal/folded emission, `serde` `to_string` (Plan 8), `miette` rendering. ✓

**Placeholder scan:** every code step is complete and compilable. No TBD/TODO. ✓

**Type consistency:** `emit`, `emit_documents`, `emit_block_value`, `emit_block_sequence`, `emit_block_mapping`, `scalar_or_flow`, `emit_flow`, `emit_scalar`, `format_float`, `needs_quoting`, `double_quote`, `pad`, `is_block_collection` names are stable across tasks. `emit_block_value` is introduced in Task 1 (flow-only) and replaced in Task 2 (block-aware) — both versions share the same signature `(&Value, usize, &EmitOptions, &mut String)`. Public API names `to_string`/`to_string_with`/`to_string_documents`/`to_string_documents_with` match the lib re-export and the spec. ✓
