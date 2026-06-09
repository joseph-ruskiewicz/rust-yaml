# yaml2 Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the foundational data types of the `yaml2` crate — the `Value` tree, configuration options, error/span types, and scalar resolution — fully unit-tested, with no parser yet.

**Architecture:** This is Plan 1 of 6 for the `yaml2` crate (see `docs/superpowers/specs/2026-06-08-yaml2-parser-design.md`). It establishes the owned, unified `Value` tree (ordered mappings, optional per-node formatting metadata), the `ParseOptions`/`Schema`/`MergeKeys`/`Limits` configuration, the span-carrying `Error` type, and the pure scalar-resolution logic (string → typed scalar per schema, including the YAML 1.1 "Norway problem" vs 1.2 differences). Later plans add the scanner, parser/events, composer, emitter, serde layer, and hardening.

**Tech Stack:** Rust (2021 edition), `indexmap` (insertion-ordered mappings). No `unsafe`. Pure Rust, no C FFI.

---

## Context for the engineer

You are building a YAML library. You do not need to know YAML's grammar for this plan — only its **scalar resolution rules**, which decide whether an unquoted string like `true`, `0o17`, or `NO` becomes a bool/int/string. The rules differ by *schema*:

- **Core 1.2** (default): `true/false` are bools; `null/~` are null; `0o17` is octal int; `0xFF` is hex int; plain decimals are ints; `1.5`/`.inf`/`.nan` are floats. Crucially, `NO`, `yes`, `on` are **strings** (no "Norway problem").
- **YAML 1.1**: `yes/no/on/off/y/n` (any case) are **bools** (the "Norway problem"); a leading-zero `0777` is **octal** (= 511), not decimal.
- **JSON 1.2**: strict — only `null`, `true`, `false`, JSON-shaped numbers; everything else is a string.
- **Failsafe**: everything is a string.

Quoted/literal/folded scalars are **always strings** regardless of schema. Only *plain* (unquoted) scalars get resolved.

These are pure functions and the meatiest, most testable part of the foundation.

---

## File structure

All paths relative to repo root `/Users/joseph/Projects/rust-yaml`.

| File | Responsibility |
|------|----------------|
| `Cargo.toml` | Workspace root, members = `["yaml2"]` |
| `yaml2/Cargo.toml` | Crate manifest, `indexmap` dependency |
| `yaml2/src/lib.rs` | Module declarations + public re-exports |
| `yaml2/src/error.rs` | `Position`, `Span`, `Error`, `ErrorKind`, `Result` |
| `yaml2/src/meta.rs` | `ScalarStyle`, `Comments`, `Meta` (per-node formatting metadata) |
| `yaml2/src/value.rs` | `ValueData`, `Value`, `Mapping` + traits, constructors, accessors |
| `yaml2/src/options.rs` | `Schema`, `MergeKeys`, `Limits`, `ParseOptions`, `EmitOptions` |
| `yaml2/src/scalar.rs` | `resolve(raw, style, schema) -> ValueData` |

---

## Task 0: Workspace and crate scaffold

**Files:**
- Create: `Cargo.toml`
- Create: `yaml2/Cargo.toml`
- Create: `yaml2/src/lib.rs`
- Create: `.gitignore`

- [ ] **Step 1: Create the workspace root `Cargo.toml`**

```toml
[workspace]
members = ["yaml2"]
resolver = "2"
```

- [ ] **Step 2: Create `yaml2/Cargo.toml`**

```toml
[package]
name = "yaml2"
version = "0.0.0"
edition = "2021"
license = "MIT OR Apache-2.0"
description = "A modern, YAML 1.2-compliant parser and emitter for Rust"
repository = "https://github.com/USER/rust-yaml"
categories = ["parser-implementations", "encoding"]
keywords = ["yaml", "serde", "parser"]

[dependencies]
indexmap = "2"

[lints.rust]
unsafe_code = "forbid"
```

- [ ] **Step 3: Create `.gitignore`**

```gitignore
/target
Cargo.lock
```

- [ ] **Step 4: Create a minimal `yaml2/src/lib.rs`**

```rust
//! A modern, YAML 1.2-compliant parser and emitter for Rust.

#[cfg(test)]
mod smoke {
    #[test]
    fn workspace_builds() {
        assert_eq!(2 + 2, 4);
    }
}
```

- [ ] **Step 5: Verify it builds and the smoke test passes**

Run: `cargo test -p yaml2`
Expected: compiles; `1 passed`.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml yaml2/Cargo.toml yaml2/src/lib.rs .gitignore
git commit -m "chore: scaffold yaml2 workspace and crate"
```

---

## Task 1: Position and Span

**Files:**
- Create: `yaml2/src/error.rs`
- Modify: `yaml2/src/lib.rs`

- [ ] **Step 1: Declare the module in `yaml2/src/lib.rs`**

Add at the top of `yaml2/src/lib.rs` (above the `smoke` module):

```rust
mod error;

pub use error::{Position, Span};
```

- [ ] **Step 2: Write the failing test in `yaml2/src/error.rs`**

```rust
//! Error, source-position, and span types.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_start_is_line_one_column_one() {
        let p = Position::start();
        assert_eq!(p.offset, 0);
        assert_eq!(p.line, 1);
        assert_eq!(p.column, 1);
    }

    #[test]
    fn span_exposes_start_and_end() {
        let span = Span::new(Position::new(0, 1, 1), Position::new(5, 1, 6));
        assert_eq!(span.start.column, 1);
        assert_eq!(span.end.offset, 5);
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p yaml2 error::`
Expected: FAIL — `cannot find type Position`.

- [ ] **Step 4: Implement `Position` and `Span`** (add above the `tests` module in `yaml2/src/error.rs`)

```rust
/// A location in the source: byte offset (0-based), line and column (1-based).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Position {
    pub offset: usize,
    pub line: usize,
    pub column: usize,
}

impl Position {
    pub fn new(offset: usize, line: usize, column: usize) -> Self {
        Self { offset, line, column }
    }

    /// The position at the very start of any input.
    pub fn start() -> Self {
        Self { offset: 0, line: 1, column: 1 }
    }
}

/// A half-open range in the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    pub start: Position,
    pub end: Position,
}

impl Span {
    pub fn new(start: Position, end: Position) -> Self {
        Self { start, end }
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p yaml2 error::`
Expected: `2 passed`.

- [ ] **Step 6: Commit**

```bash
git add yaml2/src/error.rs yaml2/src/lib.rs
git commit -m "feat(yaml2): add Position and Span source-location types"
```

---

## Task 2: Error type

**Files:**
- Modify: `yaml2/src/error.rs`
- Modify: `yaml2/src/lib.rs`

- [ ] **Step 1: Update the re-export in `yaml2/src/lib.rs`**

Replace the `pub use error::{Position, Span};` line with:

```rust
pub use error::{Error, ErrorKind, Position, Result, Span};
```

- [ ] **Step 2: Write the failing test** (add these tests inside the existing `tests` module in `yaml2/src/error.rs`)

```rust
    #[test]
    fn error_without_span_displays_message_only() {
        let e = Error::new(ErrorKind::Parse, "unexpected token");
        assert_eq!(e.to_string(), "unexpected token");
        assert_eq!(e.kind(), ErrorKind::Parse);
        assert!(e.span().is_none());
    }

    #[test]
    fn error_with_span_displays_location() {
        let span = Span::new(Position::new(3, 2, 1), Position::new(4, 2, 2));
        let e = Error::new(ErrorKind::Scan, "bad indent").with_span(span);
        assert_eq!(e.to_string(), "bad indent at line 2 column 1");
        assert_eq!(e.span(), Some(span));
    }
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p yaml2 error::`
Expected: FAIL — `cannot find type Error`.

- [ ] **Step 4: Implement `Error`, `ErrorKind`, `Result`** (add to `yaml2/src/error.rs`, above the `tests` module; add `use core::fmt;` at the top of the file)

```rust
use core::fmt;

/// The category of a parse failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Scan,
    Parse,
    Compose,
    LimitExceeded,
}

/// A YAML processing error, optionally carrying a source span.
#[derive(Debug, Clone)]
pub struct Error {
    kind: ErrorKind,
    message: String,
    span: Option<Span>,
}

impl Error {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into(), span: None }
    }

    pub fn with_span(mut self, span: Span) -> Self {
        self.span = Some(span);
        self
    }

    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    pub fn span(&self) -> Option<Span> {
        self.span
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.span {
            Some(s) => write!(f, "{} at line {} column {}", self.message, s.start.line, s.start.column),
            None => write!(f, "{}", self.message),
        }
    }
}

impl std::error::Error for Error {}

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p yaml2 error::`
Expected: `4 passed`.

- [ ] **Step 6: Commit**

```bash
git add yaml2/src/error.rs yaml2/src/lib.rs
git commit -m "feat(yaml2): add span-carrying Error type"
```

---

## Task 3: Formatting metadata (`Meta`)

**Files:**
- Create: `yaml2/src/meta.rs`
- Modify: `yaml2/src/lib.rs`

- [ ] **Step 1: Declare the module and re-exports in `yaml2/src/lib.rs`**

Add after the `mod error;` line:

```rust
mod meta;
```

Add after the existing `pub use error::...` line:

```rust
pub use meta::{Comments, Meta, ScalarStyle};
```

- [ ] **Step 2: Write the failing test in `yaml2/src/meta.rs`**

```rust
//! Per-node formatting metadata, populated only when `preserve_formatting` is on.

use crate::error::Span;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_style_defaults_to_plain() {
        assert_eq!(ScalarStyle::default(), ScalarStyle::Plain);
    }

    #[test]
    fn meta_default_is_empty() {
        let m = Meta::default();
        assert!(m.comments.leading.is_empty());
        assert!(m.comments.trailing.is_none());
        assert_eq!(m.blank_lines_before, 0);
        assert_eq!(m.style, ScalarStyle::Plain);
        assert!(m.span.is_none());
        assert!(m.anchor.is_none());
        assert!(m.tag.is_none());
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p yaml2 meta::`
Expected: FAIL — `cannot find type ScalarStyle`.

- [ ] **Step 4: Implement the metadata types** (add above the `tests` module in `yaml2/src/meta.rs`)

```rust
/// How a scalar was written in the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScalarStyle {
    #[default]
    Plain,
    SingleQuoted,
    DoubleQuoted,
    Literal,
    Folded,
}

/// Comments attached to a node.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Comments {
    /// Full-line comments appearing immediately before the node.
    pub leading: Vec<String>,
    /// An inline comment on the same line as the node.
    pub trailing: Option<String>,
}

/// Optional formatting metadata for a node. Present only when round-tripping.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Meta {
    pub comments: Comments,
    pub blank_lines_before: usize,
    pub style: ScalarStyle,
    pub span: Option<Span>,
    pub anchor: Option<String>,
    pub tag: Option<String>,
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p yaml2 meta::`
Expected: `2 passed`.

- [ ] **Step 6: Commit**

```bash
git add yaml2/src/meta.rs yaml2/src/lib.rs
git commit -m "feat(yaml2): add per-node formatting metadata types"
```

---

## Task 4: Value, ValueData, Mapping (types + equality)

This task defines the three mutually-referential core types together (they cannot compile separately) plus the trait impls that let `Value` be used as a mapping key. Equality, ordering, and hashing **ignore `meta`** — two nodes are equal iff their data is equal.

**Files:**
- Create: `yaml2/src/value.rs`
- Modify: `yaml2/src/lib.rs`

- [ ] **Step 1: Declare the module and re-exports in `yaml2/src/lib.rs`**

Add after `mod meta;`:

```rust
mod value;
```

Add after the `pub use meta::...` line:

```rust
pub use value::{Mapping, Value, ValueData};
```

- [ ] **Step 2: Write the failing test in `yaml2/src/value.rs`**

```rust
//! The owned, unified YAML value tree.

use crate::meta::Meta;
use core::cmp::Ordering;
use core::hash::{Hash, Hasher};
use indexmap::IndexMap;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_constructors_build_expected_data() {
        assert!(matches!(Value::null().data(), ValueData::Null));
        assert!(matches!(Value::bool(true).data(), ValueData::Bool(true)));
        assert!(matches!(Value::int(7).data(), ValueData::Int(7)));
        assert!(matches!(Value::string("hi").data(), ValueData::String(s) if s == "hi"));
    }

    #[test]
    fn equality_ignores_metadata() {
        let mut a = Value::int(1);
        a.meta_mut(); // force-allocate metadata
        let b = Value::int(1);
        assert_eq!(a, b);
    }

    #[test]
    fn distinct_scalars_are_not_equal() {
        assert_ne!(Value::int(1), Value::int(2));
        assert_ne!(Value::int(1), Value::string("1"));
    }

    #[test]
    fn float_zero_signs_are_distinct_keys() {
        // Consistent Eq/Hash: +0.0 and -0.0 differ by bits and must not collide.
        assert_ne!(Value::float(0.0), Value::float(-0.0));
    }
}
```

(`meta_mut` is implemented in Task 7; this test compiles only after Task 7. To keep this task self-contained, the `equality_ignores_metadata` test body uses `meta_mut`, which does not yet exist — so for THIS task, temporarily write that test as below and replace it in Task 7.)

Use this version of the second test for Task 4:

```rust
    #[test]
    fn equality_ignores_metadata() {
        let a = Value::int(1).with_meta_for_test();
        let b = Value::int(1);
        assert_eq!(a, b);
    }
```

And add this test-only helper inside the `tests` module:

```rust
    impl Value {
        fn with_meta_for_test(mut self) -> Self {
            self.set_meta_box(Some(Box::new(Meta::default())));
            self
        }
    }
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p yaml2 value::`
Expected: FAIL — `cannot find type Value`.

- [ ] **Step 4: Implement the types and trait impls** (add above the `tests` module in `yaml2/src/value.rs`)

```rust
/// The data payload of a node, without metadata.
#[derive(Debug, Clone)]
pub enum ValueData {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Sequence(Vec<Value>),
    Mapping(Mapping),
}

/// A YAML node: data plus optional, lazily-allocated formatting metadata.
#[derive(Debug, Clone)]
pub struct Value {
    data: ValueData,
    meta: Option<Box<Meta>>,
}

/// An insertion-ordered YAML mapping. Key order is always preserved.
#[derive(Debug, Clone, Default)]
pub struct Mapping {
    entries: IndexMap<Value, Value>,
}

impl Value {
    pub fn new(data: ValueData) -> Self {
        Self { data, meta: None }
    }

    pub fn null() -> Self {
        Self::new(ValueData::Null)
    }

    pub fn bool(b: bool) -> Self {
        Self::new(ValueData::Bool(b))
    }

    pub fn int(i: i64) -> Self {
        Self::new(ValueData::Int(i))
    }

    pub fn float(f: f64) -> Self {
        Self::new(ValueData::Float(f))
    }

    pub fn string(s: impl Into<String>) -> Self {
        Self::new(ValueData::String(s.into()))
    }

    pub fn sequence(items: Vec<Value>) -> Self {
        Self::new(ValueData::Sequence(items))
    }

    pub fn mapping(m: Mapping) -> Self {
        Self::new(ValueData::Mapping(m))
    }

    pub fn data(&self) -> &ValueData {
        &self.data
    }

    pub fn data_mut(&mut self) -> &mut ValueData {
        &mut self.data
    }

    pub fn into_data(self) -> ValueData {
        self.data
    }

    // Internal metadata plumbing. Public metadata accessors arrive in Task 7.
    pub(crate) fn set_meta_box(&mut self, meta: Option<Box<Meta>>) {
        self.meta = meta;
    }

    pub(crate) fn meta_box(&self) -> Option<&Meta> {
        self.meta.as_deref()
    }

    pub(crate) fn meta_box_mut(&mut self) -> &mut Box<Meta> {
        self.meta.get_or_insert_with(|| Box::new(Meta::default()))
    }

    pub(crate) fn take_meta_box(&mut self) -> Option<Box<Meta>> {
        self.meta.take()
    }
}

// --- Equality, ordering, hashing: data only, metadata ignored ---

impl Ord for ValueData {
    fn cmp(&self, other: &Self) -> Ordering {
        use ValueData::*;
        fn rank(v: &ValueData) -> u8 {
            match v {
                Null => 0,
                Bool(_) => 1,
                Int(_) => 2,
                Float(_) => 3,
                String(_) => 4,
                Sequence(_) => 5,
                Mapping(_) => 6,
            }
        }
        match (self, other) {
            (Null, Null) => Ordering::Equal,
            (Bool(a), Bool(b)) => a.cmp(b),
            (Int(a), Int(b)) => a.cmp(b),
            (Float(a), Float(b)) => a.total_cmp(b),
            (String(a), String(b)) => a.cmp(b),
            (Sequence(a), Sequence(b)) => a.cmp(b),
            (Mapping(a), Mapping(b)) => a.cmp(b),
            _ => rank(self).cmp(&rank(other)),
        }
    }
}

impl PartialOrd for ValueData {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for ValueData {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for ValueData {}

impl Hash for ValueData {
    fn hash<H: Hasher>(&self, state: &mut H) {
        core::mem::discriminant(self).hash(state);
        match self {
            ValueData::Null => {}
            ValueData::Bool(b) => b.hash(state),
            ValueData::Int(i) => i.hash(state),
            ValueData::Float(f) => f.to_bits().hash(state),
            ValueData::String(s) => s.hash(state),
            ValueData::Sequence(s) => s.hash(state),
            ValueData::Mapping(m) => m.hash(state),
        }
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        self.data == other.data
    }
}

impl Eq for Value {}

impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Value {
    fn cmp(&self, other: &Self) -> Ordering {
        self.data.cmp(&other.data)
    }
}

impl Hash for Value {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.data.hash(state);
    }
}

// Mapping equality/order/hash are entry-order-sensitive over its (key, value) pairs.

impl PartialEq for Mapping {
    fn eq(&self, other: &Self) -> bool {
        self.entries.len() == other.entries.len()
            && self.entries.iter().eq(other.entries.iter())
    }
}

impl Eq for Mapping {}

impl Ord for Mapping {
    fn cmp(&self, other: &Self) -> Ordering {
        self.entries.iter().cmp(other.entries.iter())
    }
}

impl PartialOrd for Mapping {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Hash for Mapping {
    fn hash<H: Hasher>(&self, state: &mut H) {
        for (k, v) in &self.entries {
            k.hash(state);
            v.hash(state);
        }
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p yaml2 value::`
Expected: `4 passed`.

- [ ] **Step 6: Commit**

```bash
git add yaml2/src/value.rs yaml2/src/lib.rs
git commit -m "feat(yaml2): add Value, ValueData, Mapping with metadata-blind equality"
```

---

## Task 5: Mapping operations

**Files:**
- Modify: `yaml2/src/value.rs`

- [ ] **Step 1: Write the failing test** (add to the `tests` module in `yaml2/src/value.rs`)

```rust
    #[test]
    fn mapping_preserves_insertion_order() {
        let mut m = Mapping::new();
        m.insert(Value::string("b"), Value::int(2));
        m.insert(Value::string("a"), Value::int(1));
        m.insert(Value::string("c"), Value::int(3));

        let keys: Vec<&str> = m
            .iter()
            .map(|(k, _)| k.as_str_for_test())
            .collect();
        assert_eq!(keys, ["b", "a", "c"]);
    }

    #[test]
    fn mapping_get_and_len() {
        let mut m = Mapping::new();
        assert!(m.is_empty());
        m.insert(Value::string("k"), Value::int(9));
        assert_eq!(m.len(), 1);
        assert_eq!(m.get(&Value::string("k")), Some(&Value::int(9)));
        assert_eq!(m.get(&Value::string("missing")), None);
    }

    impl Value {
        // Test-only helper until accessors land in Task 6.
        fn as_str_for_test(&self) -> &str {
            match self.data() {
                ValueData::String(s) => s,
                _ => panic!("not a string"),
            }
        }
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p yaml2 value::`
Expected: FAIL — `no method named insert found for struct Mapping`.

- [ ] **Step 3: Implement the Mapping operations** (add a new `impl Mapping` block above the `tests` module)

```rust
impl Mapping {
    pub fn new() -> Self {
        Self { entries: IndexMap::new() }
    }

    pub fn insert(&mut self, key: Value, value: Value) -> Option<Value> {
        self.entries.insert(key, value)
    }

    pub fn get(&self, key: &Value) -> Option<&Value> {
        self.entries.get(key)
    }

    pub fn get_mut(&mut self, key: &Value) -> Option<&mut Value> {
        self.entries.get_mut(key)
    }

    pub fn contains_key(&self, key: &Value) -> bool {
        self.entries.contains_key(key)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> indexmap::map::Iter<'_, Value, Value> {
        self.entries.iter()
    }

    pub fn iter_mut(&mut self) -> indexmap::map::IterMut<'_, Value, Value> {
        self.entries.iter_mut()
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p yaml2 value::`
Expected: all value tests pass (6 passed).

- [ ] **Step 5: Commit**

```bash
git add yaml2/src/value.rs
git commit -m "feat(yaml2): add ordered Mapping operations"
```

---

## Task 6: Value accessors and `From` conversions

**Files:**
- Modify: `yaml2/src/value.rs`

- [ ] **Step 1: Write the failing test** (add to the `tests` module in `yaml2/src/value.rs`)

```rust
    #[test]
    fn accessors_return_typed_views() {
        assert!(Value::null().is_null());
        assert_eq!(Value::bool(true).as_bool(), Some(true));
        assert_eq!(Value::int(5).as_int(), Some(5));
        assert_eq!(Value::int(5).as_float(), Some(5.0));
        assert_eq!(Value::float(1.5).as_float(), Some(1.5));
        assert_eq!(Value::string("x").as_str(), Some("x"));
        assert_eq!(Value::int(5).as_str(), None);
    }

    #[test]
    fn sequence_and_mapping_accessors() {
        let seq = Value::sequence(vec![Value::int(1), Value::int(2)]);
        assert_eq!(seq.as_sequence().unwrap().len(), 2);

        let mut m = Mapping::new();
        m.insert(Value::string("k"), Value::int(1));
        let map = Value::mapping(m);
        assert_eq!(map.as_mapping().unwrap().len(), 1);
    }

    #[test]
    fn from_conversions() {
        assert_eq!(Value::from(true), Value::bool(true));
        assert_eq!(Value::from(3_i64), Value::int(3));
        assert_eq!(Value::from("s"), Value::string("s"));
        assert_eq!(Value::from(String::from("s")), Value::string("s"));
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p yaml2 value::`
Expected: FAIL — `no method named is_null found`.

- [ ] **Step 3: Implement accessors and conversions** (add a new `impl Value` block and `From` impls above the `tests` module)

```rust
impl Value {
    pub fn is_null(&self) -> bool {
        matches!(self.data, ValueData::Null)
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self.data {
            ValueData::Bool(b) => Some(b),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self.data {
            ValueData::Int(i) => Some(i),
            _ => None,
        }
    }

    pub fn as_float(&self) -> Option<f64> {
        match self.data {
            ValueData::Float(f) => Some(f),
            ValueData::Int(i) => Some(i as f64),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match &self.data {
            ValueData::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_sequence(&self) -> Option<&[Value]> {
        match &self.data {
            ValueData::Sequence(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_mapping(&self) -> Option<&Mapping> {
        match &self.data {
            ValueData::Mapping(m) => Some(m),
            _ => None,
        }
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::bool(b)
    }
}

impl From<i64> for Value {
    fn from(i: i64) -> Self {
        Value::int(i)
    }
}

impl From<f64> for Value {
    fn from(f: f64) -> Self {
        Value::float(f)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::string(s)
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::string(s)
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p yaml2 value::`
Expected: all value tests pass.

- [ ] **Step 5: Commit**

```bash
git add yaml2/src/value.rs
git commit -m "feat(yaml2): add Value accessors and From conversions"
```

---

## Task 7: Public metadata accessors

This replaces the test-only metadata helpers with the real public API.

**Files:**
- Modify: `yaml2/src/value.rs`

- [ ] **Step 1: Remove the test-only helper from Task 4**

In the `tests` module, delete the `with_meta_for_test` helper `impl Value { ... }` block, and change the `equality_ignores_metadata` test to:

```rust
    #[test]
    fn equality_ignores_metadata() {
        let mut a = Value::int(1);
        a.meta_mut(); // force-allocate metadata
        let b = Value::int(1);
        assert_eq!(a, b);
    }
```

- [ ] **Step 2: Write the failing test** (add to the `tests` module)

```rust
    #[test]
    fn meta_is_absent_until_requested() {
        let v = Value::int(1);
        assert!(v.meta().is_none());
    }

    #[test]
    fn meta_mut_lazily_allocates() {
        let mut v = Value::int(1);
        v.meta_mut().anchor = Some("a1".to_string());
        assert_eq!(v.meta().unwrap().anchor.as_deref(), Some("a1"));
    }

    #[test]
    fn with_meta_sets_and_take_meta_removes() {
        let mut meta = Meta::default();
        meta.tag = Some("!!str".to_string());
        let mut v = Value::string("x").with_meta(meta);
        assert!(v.meta().is_some());
        let taken = v.take_meta().unwrap();
        assert_eq!(taken.tag.as_deref(), Some("!!str"));
        assert!(v.meta().is_none());
    }
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p yaml2 value::`
Expected: FAIL — `no method named meta found`.

- [ ] **Step 4: Implement the public metadata accessors** (add a new `impl Value` block above the `tests` module)

```rust
impl Value {
    /// Returns the formatting metadata if any has been attached.
    pub fn meta(&self) -> Option<&Meta> {
        self.meta_box()
    }

    /// Returns the metadata mutably, allocating an empty `Meta` on first access.
    pub fn meta_mut(&mut self) -> &mut Meta {
        self.meta_box_mut()
    }

    /// Attaches metadata, returning the value for chaining.
    pub fn with_meta(mut self, meta: Meta) -> Self {
        self.set_meta_box(Some(Box::new(meta)));
        self
    }

    /// Removes and returns any attached metadata.
    pub fn take_meta(&mut self) -> Option<Meta> {
        self.take_meta_box().map(|b| *b)
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p yaml2 value::`
Expected: all value tests pass.

- [ ] **Step 6: Commit**

```bash
git add yaml2/src/value.rs
git commit -m "feat(yaml2): add public node metadata accessors"
```

---

## Task 8: Configuration options

**Files:**
- Create: `yaml2/src/options.rs`
- Modify: `yaml2/src/lib.rs`

- [ ] **Step 1: Declare the module and re-exports in `yaml2/src/lib.rs`**

Add after `mod value;`:

```rust
mod options;
```

Add after the `pub use value::...` line:

```rust
pub use options::{EmitOptions, Limits, MergeKeys, ParseOptions, Schema};
```

- [ ] **Step 2: Write the failing test in `yaml2/src/options.rs`**

```rust
//! Parse- and emit-time configuration.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_options_defaults() {
        let o = ParseOptions::default();
        assert_eq!(o.schema, Schema::Core1_2);
        assert_eq!(o.merge_keys, MergeKeys::Auto);
        assert!(!o.preserve_formatting);
        assert_eq!(o.limits, Limits::default());
    }

    #[test]
    fn preserve_formatting_constructor() {
        let o = ParseOptions::preserve_formatting();
        assert!(o.preserve_formatting);
        assert_eq!(o.schema, Schema::Core1_2);
    }

    #[test]
    fn builder_methods() {
        let o = ParseOptions::default()
            .with_schema(Schema::Yaml1_1)
            .with_merge_keys(MergeKeys::On);
        assert_eq!(o.schema, Schema::Yaml1_1);
        assert_eq!(o.merge_keys, MergeKeys::On);
    }

    #[test]
    fn merge_keys_auto_follows_schema() {
        assert!(!MergeKeys::Auto.enabled_for(Schema::Core1_2));
        assert!(MergeKeys::Auto.enabled_for(Schema::Yaml1_1));
        assert!(MergeKeys::On.enabled_for(Schema::Core1_2));
        assert!(!MergeKeys::Off.enabled_for(Schema::Yaml1_1));
    }

    #[test]
    fn emit_options_round_trip_constructor() {
        assert!(!EmitOptions::default().round_trip);
        assert!(EmitOptions::round_trip().round_trip);
        assert_eq!(EmitOptions::default().indent, 2);
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p yaml2 options::`
Expected: FAIL — `cannot find type ParseOptions`.

- [ ] **Step 4: Implement the configuration types** (add above the `tests` module)

```rust
/// Selects scalar-resolution rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Schema {
    /// YAML 1.2 core schema (default).
    #[default]
    Core1_2,
    /// YAML 1.2 JSON schema (strict).
    Json1_2,
    /// YAML 1.1 resolution (the "Norway problem", leading-zero octal).
    Yaml1_1,
    /// Everything resolves to a string.
    Failsafe,
}

/// Whether `<<` merge keys are honored. Independent of schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MergeKeys {
    /// On for the 1.1 schema, off otherwise.
    #[default]
    Auto,
    On,
    Off,
}

impl MergeKeys {
    /// Resolves whether merge keys are active under the given schema.
    pub fn enabled_for(self, schema: Schema) -> bool {
        match self {
            MergeKeys::On => true,
            MergeKeys::Off => false,
            MergeKeys::Auto => matches!(schema, Schema::Yaml1_1),
        }
    }
}

/// Security guards, enabled by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Max number of alias expansions (billion-laughs guard).
    pub max_aliases: usize,
    /// Max nesting depth of collections.
    pub max_depth: usize,
    /// Max accepted input size in bytes.
    pub max_input_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_aliases: 10_000,
            max_depth: 128,
            max_input_bytes: 64 * 1024 * 1024,
        }
    }
}

/// Options controlling parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseOptions {
    pub schema: Schema,
    pub merge_keys: MergeKeys,
    pub preserve_formatting: bool,
    pub limits: Limits,
}

impl Default for ParseOptions {
    fn default() -> Self {
        Self {
            schema: Schema::Core1_2,
            merge_keys: MergeKeys::Auto,
            preserve_formatting: false,
            limits: Limits::default(),
        }
    }
}

impl ParseOptions {
    /// Default options with formatting preservation enabled (round-trip).
    pub fn preserve_formatting() -> Self {
        Self { preserve_formatting: true, ..Self::default() }
    }

    pub fn with_schema(mut self, schema: Schema) -> Self {
        self.schema = schema;
        self
    }

    pub fn with_merge_keys(mut self, merge_keys: MergeKeys) -> Self {
        self.merge_keys = merge_keys;
        self
    }

    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }
}

/// Options controlling emission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmitOptions {
    pub round_trip: bool,
    pub indent: usize,
}

impl Default for EmitOptions {
    fn default() -> Self {
        Self { round_trip: false, indent: 2 }
    }
}

impl EmitOptions {
    /// Default options with round-trip emission enabled.
    pub fn round_trip() -> Self {
        Self { round_trip: true, ..Self::default() }
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p yaml2 options::`
Expected: `5 passed`.

- [ ] **Step 6: Commit**

```bash
git add yaml2/src/options.rs yaml2/src/lib.rs
git commit -m "feat(yaml2): add parse and emit configuration options"
```

---

## Task 9: Scalar resolution — Core 1.2

**Files:**
- Create: `yaml2/src/scalar.rs`
- Modify: `yaml2/src/lib.rs`

- [ ] **Step 1: Declare the module in `yaml2/src/lib.rs`** (no public re-export — `resolve` is crate-internal)

Add after `mod options;`:

```rust
mod scalar;
```

- [ ] **Step 2: Write the failing test in `yaml2/src/scalar.rs`**

```rust
//! Plain-scalar resolution: raw text -> typed `ValueData`, per schema.

use crate::meta::ScalarStyle;
use crate::options::Schema;
use crate::value::ValueData;

#[cfg(test)]
mod tests {
    use super::*;

    fn core(raw: &str) -> ValueData {
        resolve(raw, ScalarStyle::Plain, Schema::Core1_2)
    }

    #[test]
    fn core_null_bool() {
        assert!(matches!(core("null"), ValueData::Null));
        assert!(matches!(core("~"), ValueData::Null));
        assert!(matches!(core(""), ValueData::Null));
        assert!(matches!(core("true"), ValueData::Bool(true)));
        assert!(matches!(core("False"), ValueData::Bool(false)));
    }

    #[test]
    fn core_integers() {
        assert!(matches!(core("42"), ValueData::Int(42)));
        assert!(matches!(core("-7"), ValueData::Int(-7)));
        assert!(matches!(core("0x1F"), ValueData::Int(31)));
        assert!(matches!(core("0o17"), ValueData::Int(15)));
    }

    #[test]
    fn core_leading_zero_is_decimal_not_octal() {
        // The key Core-1.2 difference from YAML 1.1.
        assert!(matches!(core("0777"), ValueData::Int(777)));
    }

    #[test]
    fn core_floats() {
        assert!(matches!(core("1.5"), ValueData::Float(f) if f == 1.5));
        assert!(matches!(core(".5"), ValueData::Float(f) if f == 0.5));
        assert!(matches!(core("1e3"), ValueData::Float(f) if f == 1000.0));
        assert!(matches!(core(".inf"), ValueData::Float(f) if f == f64::INFINITY));
        assert!(matches!(core(".nan"), ValueData::Float(f) if f.is_nan()));
    }

    #[test]
    fn core_norway_problem_absent() {
        // These are strings in Core 1.2, not booleans.
        assert!(matches!(core("NO"), ValueData::String(s) if s == "NO"));
        assert!(matches!(core("yes"), ValueData::String(s) if s == "yes"));
        assert!(matches!(core("on"), ValueData::String(s) if s == "on"));
    }

    #[test]
    fn core_plain_words_are_strings() {
        assert!(matches!(core("hello"), ValueData::String(s) if s == "hello"));
        assert!(matches!(core("1.2.3"), ValueData::String(s) if s == "1.2.3"));
    }

    #[test]
    fn non_plain_styles_are_always_strings() {
        assert!(matches!(
            resolve("true", ScalarStyle::SingleQuoted, Schema::Core1_2),
            ValueData::String(s) if s == "true"
        ));
        assert!(matches!(
            resolve("42", ScalarStyle::DoubleQuoted, Schema::Core1_2),
            ValueData::String(s) if s == "42"
        ));
    }

    #[test]
    fn integer_overflow_falls_back_to_string() {
        // Shaped like an int but does not fit i64: keep losslessly as string.
        assert!(matches!(
            core("99999999999999999999999"),
            ValueData::String(_)
        ));
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p yaml2 scalar::`
Expected: FAIL — `cannot find function resolve`.

- [ ] **Step 4: Implement `resolve` and the Core 1.2 path** (add above the `tests` module)

```rust
/// Resolves a scalar's raw text into typed data according to `style` and `schema`.
///
/// Non-plain scalars (quoted, literal, folded) are always strings.
pub(crate) fn resolve(raw: &str, style: ScalarStyle, schema: Schema) -> ValueData {
    if style != ScalarStyle::Plain {
        return ValueData::String(raw.to_string());
    }
    match schema {
        Schema::Failsafe => ValueData::String(raw.to_string()),
        Schema::Core1_2 => resolve_core(raw),
        Schema::Json1_2 => resolve_json(raw),
        Schema::Yaml1_1 => resolve_yaml11(raw),
    }
}

fn resolve_core(raw: &str) -> ValueData {
    match raw {
        "" | "~" | "null" | "Null" | "NULL" => return ValueData::Null,
        "true" | "True" | "TRUE" => return ValueData::Bool(true),
        "false" | "False" | "FALSE" => return ValueData::Bool(false),
        ".inf" | ".Inf" | ".INF" | "+.inf" | "+.Inf" | "+.INF" => {
            return ValueData::Float(f64::INFINITY)
        }
        "-.inf" | "-.Inf" | "-.INF" => return ValueData::Float(f64::NEG_INFINITY),
        ".nan" | ".NaN" | ".NAN" => return ValueData::Float(f64::NAN),
        _ => {}
    }
    if is_core_int_shape(raw) {
        // Shape matched; parse, falling back to String on overflow (lossless).
        return match parse_core_int(raw) {
            Some(i) => ValueData::Int(i),
            None => ValueData::String(raw.to_string()),
        };
    }
    if let Some(f) = parse_core_float(raw) {
        return ValueData::Float(f);
    }
    ValueData::String(raw.to_string())
}

/// True if `raw` matches the Core int grammar: 0x..., 0o..., or [-+]?[0-9]+.
fn is_core_int_shape(raw: &str) -> bool {
    if let Some(hex) = raw.strip_prefix("0x") {
        return !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit());
    }
    if let Some(oct) = raw.strip_prefix("0o") {
        return !oct.is_empty() && oct.bytes().all(|b| (b'0'..=b'7').contains(&b));
    }
    let body = raw.strip_prefix(['+', '-']).unwrap_or(raw);
    !body.is_empty() && body.bytes().all(|b| b.is_ascii_digit())
}

fn parse_core_int(raw: &str) -> Option<i64> {
    if let Some(hex) = raw.strip_prefix("0x") {
        return i64::from_str_radix(hex, 16).ok();
    }
    if let Some(oct) = raw.strip_prefix("0o") {
        return i64::from_str_radix(oct, 8).ok();
    }
    raw.parse::<i64>().ok()
}

/// Parses a Core-1.2 float. Requires a '.' or exponent to distinguish from ints,
/// and only the float character set (rejecting Rust-accepted forms like "inf").
fn parse_core_float(raw: &str) -> Option<f64> {
    let has_dot_or_exp = raw.contains('.') || raw.contains('e') || raw.contains('E');
    if !has_dot_or_exp {
        return None;
    }
    let allowed = |b: u8| {
        b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-')
    };
    if !raw.bytes().all(allowed) {
        return None;
    }
    raw.parse::<f64>().ok()
}
```

- [ ] **Step 5: Add temporary stubs so the file compiles** (the JSON and 1.1 paths are implemented in Tasks 10–11; add these stubs above the `tests` module for now)

```rust
fn resolve_json(raw: &str) -> ValueData {
    // Implemented in Task 11.
    ValueData::String(raw.to_string())
}

fn resolve_yaml11(raw: &str) -> ValueData {
    // Implemented in Task 10.
    ValueData::String(raw.to_string())
}
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p yaml2 scalar::`
Expected: all Core tests pass.

- [ ] **Step 7: Commit**

```bash
git add yaml2/src/scalar.rs yaml2/src/lib.rs
git commit -m "feat(yaml2): add Core 1.2 scalar resolution"
```

---

## Task 10: Scalar resolution — YAML 1.1 (the Norway problem)

**Files:**
- Modify: `yaml2/src/scalar.rs`

- [ ] **Step 1: Write the failing test** (add to the `tests` module in `yaml2/src/scalar.rs`)

```rust
    fn y11(raw: &str) -> ValueData {
        resolve(raw, ScalarStyle::Plain, Schema::Yaml1_1)
    }

    #[test]
    fn yaml11_norway_problem_present() {
        assert!(matches!(y11("NO"), ValueData::Bool(false)));
        assert!(matches!(y11("no"), ValueData::Bool(false)));
        assert!(matches!(y11("yes"), ValueData::Bool(true)));
        assert!(matches!(y11("on"), ValueData::Bool(true)));
        assert!(matches!(y11("off"), ValueData::Bool(false)));
        assert!(matches!(y11("y"), ValueData::Bool(true)));
        assert!(matches!(y11("N"), ValueData::Bool(false)));
    }

    #[test]
    fn yaml11_leading_zero_is_octal() {
        // The key YAML-1.1 difference from Core 1.2.
        assert!(matches!(y11("0777"), ValueData::Int(511)));
        assert!(matches!(y11("0"), ValueData::Int(0)));
    }

    #[test]
    fn yaml11_decimal_and_string() {
        assert!(matches!(y11("42"), ValueData::Int(42)));
        assert!(matches!(y11("hello"), ValueData::String(s) if s == "hello"));
    }

    #[test]
    fn yaml11_null() {
        assert!(matches!(y11("~"), ValueData::Null));
        assert!(matches!(y11("null"), ValueData::Null));
        assert!(matches!(y11(""), ValueData::Null));
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p yaml2 scalar::tests::yaml11`
Expected: FAIL — `NO` resolves to String (the stub), not Bool.

- [ ] **Step 3: Replace the `resolve_yaml11` stub with the real implementation**

```rust
fn resolve_yaml11(raw: &str) -> ValueData {
    match raw {
        "" | "~" | "null" | "Null" | "NULL" => return ValueData::Null,
        _ => {}
    }
    if let Some(b) = yaml11_bool(raw) {
        return ValueData::Bool(b);
    }
    // Leading-zero octal: 0[0-7]+ (but plain "0" is decimal zero).
    if let Some(rest) = raw.strip_prefix('0') {
        if !rest.is_empty() && rest.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
            if let Ok(i) = i64::from_str_radix(rest, 8) {
                return ValueData::Int(i);
            }
        }
    }
    // Decimal int: [-+]?[0-9]+
    {
        let body = raw.strip_prefix(['+', '-']).unwrap_or(raw);
        if !body.is_empty() && body.bytes().all(|b| b.is_ascii_digit()) {
            return match raw.parse::<i64>() {
                Ok(i) => ValueData::Int(i),
                Err(_) => ValueData::String(raw.to_string()),
            };
        }
    }
    if let Some(f) = parse_core_float(raw) {
        return ValueData::Float(f);
    }
    ValueData::String(raw.to_string())
}

fn yaml11_bool(raw: &str) -> Option<bool> {
    match raw {
        "y" | "Y" | "yes" | "Yes" | "YES" | "true" | "True" | "TRUE" | "on" | "On" | "ON" => {
            Some(true)
        }
        "n" | "N" | "no" | "No" | "NO" | "false" | "False" | "FALSE" | "off" | "Off" | "OFF" => {
            Some(false)
        }
        _ => None,
    }
}
```

> **Note for the engineer:** YAML 1.1 also defines binary (`0b...`), sexagesimal (`1:2:3`), and underscore digit separators. These are deferred to the Plan 6 hardening pass; this task covers the user-facing differences called out in the spec (the Norway problem and leading-zero octal).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p yaml2 scalar::`
Expected: all Core and 1.1 tests pass.

- [ ] **Step 5: Commit**

```bash
git add yaml2/src/scalar.rs
git commit -m "feat(yaml2): add YAML 1.1 scalar resolution (Norway problem, octal)"
```

---

## Task 11: Scalar resolution — JSON 1.2 and Failsafe

**Files:**
- Modify: `yaml2/src/scalar.rs`

- [ ] **Step 1: Write the failing test** (add to the `tests` module in `yaml2/src/scalar.rs`)

```rust
    fn json(raw: &str) -> ValueData {
        resolve(raw, ScalarStyle::Plain, Schema::Json1_2)
    }

    #[test]
    fn json_strict_keywords() {
        assert!(matches!(json("null"), ValueData::Null));
        assert!(matches!(json("true"), ValueData::Bool(true)));
        assert!(matches!(json("false"), ValueData::Bool(false)));
        // Case variants are NOT keywords in JSON schema.
        assert!(matches!(json("Null"), ValueData::String(s) if s == "Null"));
        assert!(matches!(json("True"), ValueData::String(s) if s == "True"));
    }

    #[test]
    fn json_numbers() {
        assert!(matches!(json("0"), ValueData::Int(0)));
        assert!(matches!(json("-12"), ValueData::Int(-12)));
        assert!(matches!(json("1.5"), ValueData::Float(f) if f == 1.5));
        assert!(matches!(json("1e3"), ValueData::Float(f) if f == 1000.0));
        // Leading zeros are not valid JSON numbers -> string.
        assert!(matches!(json("01"), ValueData::String(s) if s == "01"));
        // Hex is not JSON -> string.
        assert!(matches!(json("0x1F"), ValueData::String(s) if s == "0x1F"));
    }

    #[test]
    fn failsafe_everything_is_string() {
        assert!(matches!(
            resolve("true", ScalarStyle::Plain, Schema::Failsafe),
            ValueData::String(s) if s == "true"
        ));
        assert!(matches!(
            resolve("42", ScalarStyle::Plain, Schema::Failsafe),
            ValueData::String(s) if s == "42"
        ));
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p yaml2 scalar::tests::json`
Expected: FAIL — `null` resolves to String (the stub), not Null.

- [ ] **Step 3: Replace the `resolve_json` stub with the real implementation**

```rust
fn resolve_json(raw: &str) -> ValueData {
    match raw {
        "null" => return ValueData::Null,
        "true" => return ValueData::Bool(true),
        "false" => return ValueData::Bool(false),
        _ => {}
    }
    match classify_json_number(raw) {
        JsonNumber::Int => match raw.parse::<i64>() {
            Ok(i) => ValueData::Int(i),
            Err(_) => ValueData::String(raw.to_string()),
        },
        JsonNumber::Float => match raw.parse::<f64>() {
            Ok(f) => ValueData::Float(f),
            Err(_) => ValueData::String(raw.to_string()),
        },
        JsonNumber::No => ValueData::String(raw.to_string()),
    }
}

enum JsonNumber {
    Int,
    Float,
    No,
}

/// Classifies `raw` against the JSON number grammar:
/// `-? (0 | [1-9][0-9]*) (. [0-9]+)? ([eE] [+-]? [0-9]+)?`
fn classify_json_number(raw: &str) -> JsonNumber {
    let bytes = raw.as_bytes();
    let mut i = 0;

    if i < bytes.len() && bytes[i] == b'-' {
        i += 1;
    }

    // Integer part.
    let int_start = i;
    if i < bytes.len() && bytes[i] == b'0' {
        i += 1;
    } else {
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
    }
    if i == int_start {
        return JsonNumber::No; // no integer digits
    }

    let mut is_float = false;

    // Fraction.
    if i < bytes.len() && bytes[i] == b'.' {
        is_float = true;
        i += 1;
        let frac_start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == frac_start {
            return JsonNumber::No; // '.' with no digits
        }
    }

    // Exponent.
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        is_float = true;
        i += 1;
        if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
            i += 1;
        }
        let exp_start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == exp_start {
            return JsonNumber::No; // exponent with no digits
        }
    }

    if i != bytes.len() {
        return JsonNumber::No; // trailing junk
    }
    if is_float {
        JsonNumber::Float
    } else {
        JsonNumber::Int
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p yaml2 scalar::`
Expected: all scalar tests pass.

- [ ] **Step 5: Commit**

```bash
git add yaml2/src/scalar.rs
git commit -m "feat(yaml2): add JSON 1.2 and Failsafe scalar resolution"
```

---

## Task 12: Public `Value::from_scalar` constructor and full-crate verification

This wires scalar resolution to `Value`, giving the rest of the crate (the composer, in Plan 3) a single entry point, and verifies the whole foundation.

**Files:**
- Modify: `yaml2/src/value.rs`
- Modify: `yaml2/src/lib.rs`

- [ ] **Step 1: Write the failing test in `yaml2/src/value.rs`** (add to the `tests` module)

```rust
    #[test]
    fn from_scalar_resolves_per_schema() {
        use crate::meta::ScalarStyle;
        use crate::options::Schema;

        let core = Value::from_scalar("0777", ScalarStyle::Plain, Schema::Core1_2);
        assert_eq!(core.as_int(), Some(777));

        let y11 = Value::from_scalar("0777", ScalarStyle::Plain, Schema::Yaml1_1);
        assert_eq!(y11.as_int(), Some(511));

        let quoted = Value::from_scalar("true", ScalarStyle::SingleQuoted, Schema::Core1_2);
        assert_eq!(quoted.as_str(), Some("true"));
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p yaml2 value::tests::from_scalar`
Expected: FAIL — `no function from_scalar`.

- [ ] **Step 3: Implement `Value::from_scalar`** (add to an `impl Value` block in `yaml2/src/value.rs`; add the imports `use crate::meta::ScalarStyle;` and `use crate::options::Schema;` at the top of the file)

```rust
impl Value {
    /// Builds a scalar value by resolving raw source text under the given style
    /// and schema. Quoted/literal/folded styles always yield a string.
    pub fn from_scalar(raw: &str, style: ScalarStyle, schema: Schema) -> Value {
        Value::new(crate::scalar::resolve(raw, style, schema))
    }
}
```

- [ ] **Step 4: Run the full test suite**

Run: `cargo test -p yaml2`
Expected: every test passes.

- [ ] **Step 5: Verify lints and formatting are clean**

Run: `cargo clippy -p yaml2 --all-targets -- -D warnings`
Expected: no warnings.

Run: `cargo fmt --check`
Expected: no diff (run `cargo fmt` first if needed, then re-check).

- [ ] **Step 6: Commit**

```bash
git add yaml2/src/value.rs yaml2/src/lib.rs
git commit -m "feat(yaml2): add Value::from_scalar resolution entry point"
```

---

## Self-Review

**Spec coverage (this plan's portion):**
- Unified owned `Value` tree (variants, owned strings) — Tasks 4, 6. ✔
- Ordered mappings always — Task 5 (`IndexMap`, insertion-order test). ✔
- Optional per-node formatting metadata (`None` on fast path) — Tasks 3, 7. ✔
- Tags/anchors carried on nodes — `Meta` fields (Task 3). ✔
- `Schema` (Core1_2/Json1_2/Yaml1_1/Failsafe) controlling scalar resolution — Tasks 8–11. ✔
- Independent `MergeKeys` toggle defaulting per schema — Task 8 (`enabled_for`). ✔
- Norway problem present in 1.1, absent in Core 1.2; leading-zero octal difference — Tasks 9, 10. ✔
- Security `Limits` on by default — Task 8 (enforcement wired in Plan 2/3). ✔
- Span-carrying `Error` type — Tasks 1, 2. ✔
- `EmitOptions`/`ParseOptions` with `preserve_formatting`/`round_trip` constructors — Task 8. ✔
- Pure Rust, no `unsafe` — `unsafe_code = "forbid"` lint (Task 0). ✔

**Deferred to later plans (correctly out of scope here):** scanner, parser/events, composer, anchors/alias *resolution*, merge-key *application*, emitter, serde layer, `yaml-test-suite`, `cargo-fuzz`, `miette` feature, 1.1 binary/sexagesimal/underscore numbers.

**Placeholder scan:** No "TBD/TODO" left in code. The Task 9 stubs for `resolve_json`/`resolve_yaml11` are explicit, temporary, and replaced in Tasks 10–11 with verification steps. ✔

**Type consistency:** `resolve(raw, style, schema) -> ValueData` used identically in Tasks 9–12. `Value::from_scalar` signature matches its test. Metadata plumbing (`set_meta_box`/`meta_box`/`meta_box_mut`/`take_meta_box`, Task 4) is consumed by the public accessors (`meta`/`meta_mut`/`with_meta`/`take_meta`, Task 7). `Mapping` method names (`insert`/`get`/`len`/`is_empty`/`iter`) are consistent across Tasks 5, 6. ✔
