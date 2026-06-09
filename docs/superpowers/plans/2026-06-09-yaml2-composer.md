# yaml2 Composer (events → Value tree) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the composer that turns the parser's event stream into owned `Value` trees, resolving scalars per schema, anchors/aliases (with the billion-laughs limit), core tags, and merge keys, and expose the public `parse` / `parse_documents` entry points.

**Architecture:** A new `composer` module walks `&[Event]` with a cursor and recursively builds `Value` nodes. Recursion depth is bounded by the parser's `max_depth` (events are only produced for nesting within that limit), so no separate compose-time depth guard is needed. A thin `api` module wires `parse_events → compose` into the crate's public surface. The composer is the last layer before serde; `yscript` will consume its `Value` output directly.

**Tech Stack:** Rust 2021, `indexmap` (via the existing `Mapping`), `std::collections::HashMap` for the anchor table.

---

## Context for the engineer

You are extending the `yaml2` crate. The pipeline so far is `bytes → scanner (tokens) → parser (events)`. This plan adds the next layer: `events → composer → Value tree`.

**Read these first** (do not re-derive their contents — use them):

- `yaml2/src/event.rs` — the event model you consume. Key shapes:
  - `Event { pub kind: EventKind, pub span: Span }`
  - `EventKind::{StreamStart, StreamEnd, DocumentStart, DocumentEnd, Scalar { value: String, style: ScalarStyle, anchor: Option<String>, tag: Option<String> }, Alias(String), SequenceStart { anchor, tag }, SequenceEnd, MappingStart { anchor, tag }, MappingEnd }`
- `yaml2/src/value.rs` — the tree you build. Use the constructors `Value::null()`, `Value::bool(b)`, `Value::int(i)`, `Value::float(f)`, `Value::string(s)`, `Value::sequence(Vec<Value>)`, `Value::mapping(Mapping)`, and `Value::from_scalar(raw, style, schema)`. `Mapping::new()`, `Mapping::insert(k, v)`, `Mapping::get(&Value)`, `Mapping::contains_key(&Value)`, `Mapping::len()`, `Mapping::iter()`. `Value::data()`/`into_data()` expose `ValueData`. **`Value` equality/hashing ignore metadata and are by-data**, so `Value::string("<<")` is a valid `Mapping` key/lookup.
- `yaml2/src/scalar.rs` — `pub(crate) fn resolve(raw, style, schema) -> ValueData`. `Value::from_scalar` wraps it. Empty plain scalar `""` resolves to `ValueData::Null` under Core/1.1; quoted/literal/folded styles are always `String`. You will reuse `crate::scalar::resolve(raw, ScalarStyle::Plain, Schema::Core1_2)` for tag-driven typed parsing.
- `yaml2/src/options.rs` — `ParseOptions { schema, merge_keys, preserve_formatting, limits }`. `Schema::{Core1_2, Json1_2, Yaml1_1, Failsafe}`. `MergeKeys::{Auto, On, Off}` with `MergeKeys::enabled_for(schema) -> bool` (Auto = on under `Yaml1_1`, off otherwise). `Limits { max_aliases, max_depth, max_input_bytes }` (defaults 10_000 / 128 / 64 MiB).
- `yaml2/src/error.rs` — `Error::new(ErrorKind, msg).with_span(span)`, `ErrorKind::{Scan, Parse, Compose, LimitExceeded}`, `Result<T>`, `Span`.
- `yaml2/src/parser.rs` — `pub fn parse_events(input, &ParseOptions) -> Result<Vec<Event>>`.

### Design decisions locked for this plan

- **Anchor scope is per-document.** Clear the anchor table at each `DocumentStart`.
- **No forward / recursive references.** A collection's anchor is registered *after* the collection is fully composed, so `&a [*a]` yields an "unknown anchor" error. This is intended: the owned tree cannot represent cycles.
- **Alias = clone.** Resolving `*a` clones the anchored value.
- **Billion-laughs guard.** `Limits.max_aliases` is a budget on the **total number of nodes materialized by alias expansion**. Each alias resolution adds the cloned subtree's node count to a running total; exceeding `max_aliases` is a `LimitExceeded` error. (Counting alias *events* would not stop the exponential blow-up, which is in tree size, not event count.)
- **Tag handling (this plan):** the seven core tags in shorthand (`!!str`, `!!null`, `!!bool`, `!!int`, `!!float`, `!!seq`, `!!map`) and their verbatim `tag:yaml.org,2002:*` forms, on **scalar** nodes. The non-specific `!` and any unknown/custom tag on a scalar resolve to a string. Collection tags are accepted and ignored (no validation). Full handle/URI resolution and custom-tag semantics are deferred to Plan 9 (hardening).
- **Merge keys (`<<`):** active only when `merge_keys.enabled_for(schema)`. A merge key is detected at the **event** level — a `Scalar { value: "<<", style: Plain, tag: None, .. }` key — so a quoted `"<<"` is never a merge. Merge value must be a mapping or a sequence of mappings. Explicit keys always win; among multiple merge sources, earlier wins. Non-overridden merged keys are appended after the explicit keys. (Exact in-place ordering for round-trip is out of scope; tests assert by lookup, not key order.)
- **`preserve_formatting` metadata population is deferred to Plan 7** (emitter + round-trip), where it is actually consumed. The composer ignores the flag for now; with it on, values simply carry no `Meta`.
- **`parse_documents` returns `Result<Vec<Value>>`** (eager), not the streaming iterator sketched in the design spec. The parser is already eager, so a streaming wrapper buys nothing yet; it can be added later without breaking this surface.

---

## File structure

- Create `yaml2/src/composer.rs` — the event→tree composer. Owns `pub(crate) fn compose(&[Event], &ParseOptions) -> Result<Vec<Value>>`, the internal `Composer` cursor struct, and tag/merge/alias helpers. One responsibility: turning a validated event stream into `Value` documents.
- Create `yaml2/src/api.rs` — the public ergonomic entry points (`parse`, `parse_with`, `parse_documents`, `parse_documents_with`). One responsibility: composing the pipeline stages for callers.
- Modify `yaml2/src/lib.rs` — declare the two modules (alphabetical order: `api` before `composer`) and re-export the four `api` functions.

---

## Task 1: Composer scaffold — stream/document framing and scalar nodes

**Files:**
- Create: `yaml2/src/composer.rs`
- Create: `yaml2/src/api.rs`
- Modify: `yaml2/src/lib.rs`

- [ ] **Step 1: Create `yaml2/src/composer.rs`** with the cursor, stream/document framing, scalar composition, and the `compose` entry point.

```rust
//! The composer: events -> owned `Value` documents.

use std::collections::HashMap;

use crate::error::{Error, ErrorKind, Result, Span};
use crate::event::{Event, EventKind};
use crate::meta::ScalarStyle;
use crate::options::{ParseOptions, Schema};
use crate::value::{Mapping, Value, ValueData};

/// Composes a validated event stream into one `Value` per document.
pub(crate) fn compose(events: &[Event], options: &ParseOptions) -> Result<Vec<Value>> {
    let mut composer = Composer {
        events,
        pos: 0,
        options,
        anchors: HashMap::new(),
        alias_nodes: 0,
    };
    composer.compose_stream()
}

/// Cursor-based recursive composer over a borrowed event slice.
struct Composer<'a> {
    events: &'a [Event],
    pos: usize,
    options: &'a ParseOptions,
    /// Anchor name -> composed value, valid within the current document.
    anchors: HashMap<String, Value>,
    /// Running count of nodes materialized via alias expansion (billion-laughs guard).
    alias_nodes: usize,
}

impl Composer<'_> {
    fn peek(&self) -> Option<&EventKind> {
        self.events.get(self.pos).map(|e| &e.kind)
    }

    /// Consumes and returns the next event, or a `Compose` error if the stream
    /// ends unexpectedly (the parser should never hand us a truncated stream).
    fn bump(&mut self) -> Result<Event> {
        let event = self
            .events
            .get(self.pos)
            .cloned()
            .ok_or_else(|| Error::new(ErrorKind::Compose, "unexpected end of event stream"))?;
        self.pos += 1;
        Ok(event)
    }

    fn error(&self, span: Span, message: impl Into<String>) -> Error {
        Error::new(ErrorKind::Compose, message).with_span(span)
    }

    fn compose_stream(&mut self) -> Result<Vec<Value>> {
        match self.bump()?.kind {
            EventKind::StreamStart => {}
            other => {
                return Err(Error::new(
                    ErrorKind::Compose,
                    format!("expected stream start, found {other:?}"),
                ))
            }
        }
        let mut documents = Vec::new();
        loop {
            match self.peek() {
                Some(EventKind::StreamEnd) => {
                    self.bump()?;
                    break;
                }
                Some(EventKind::DocumentStart) => documents.push(self.compose_document()?),
                None => break,
                Some(other) => {
                    let span = self.events[self.pos].span;
                    return Err(self.error(span, format!("expected document or stream end, found {other:?}")));
                }
            }
        }
        Ok(documents)
    }

    fn compose_document(&mut self) -> Result<Value> {
        self.anchors.clear();
        self.bump()?; // DocumentStart
        let value = self.compose_node()?;
        match self.bump()?.kind {
            EventKind::DocumentEnd => {}
            other => {
                return Err(Error::new(
                    ErrorKind::Compose,
                    format!("expected document end, found {other:?}"),
                ))
            }
        }
        Ok(value)
    }

    fn compose_node(&mut self) -> Result<Value> {
        let event = self.bump()?;
        match event.kind {
            EventKind::Scalar {
                value,
                style,
                anchor,
                tag,
            } => {
                let composed = self.resolve_scalar(&value, style, tag.as_deref(), event.span)?;
                if let Some(name) = anchor {
                    self.anchors.insert(name, composed.clone());
                }
                Ok(composed)
            }
            other => Err(Error::new(
                ErrorKind::Compose,
                format!("unexpected event while composing a node: {other:?}"),
            )),
        }
    }

    /// Resolves a scalar event into a typed `Value`. Untagged scalars use the
    /// configured schema; tagged scalars are handled in Task 6.
    fn resolve_scalar(
        &self,
        raw: &str,
        style: ScalarStyle,
        tag: Option<&str>,
        _span: Span,
    ) -> Result<Value> {
        let _ = tag; // tag handling added in Task 6
        Ok(Value::from_scalar(raw, style, self.options.schema))
    }
}
```

- [ ] **Step 2: Create `yaml2/src/api.rs`** with the public entry points.

```rust
//! Public, ergonomic entry points: source text -> `Value`.

use crate::composer::compose;
use crate::error::{Error, ErrorKind, Result};
use crate::options::ParseOptions;
use crate::parser::parse_events;
use crate::value::Value;

/// Parses a single-document YAML string into a `Value` using default options.
///
/// An empty stream yields `Value::null()`. More than one document is an error;
/// use [`parse_documents`] for multi-document streams.
pub fn parse(input: &str) -> Result<Value> {
    parse_with(input, &ParseOptions::default())
}

/// Like [`parse`], with explicit options.
pub fn parse_with(input: &str, options: &ParseOptions) -> Result<Value> {
    let mut documents = parse_documents_with(input, options)?;
    match documents.len() {
        0 => Ok(Value::null()),
        1 => Ok(documents.pop().expect("len checked == 1")),
        n => Err(Error::new(
            ErrorKind::Compose,
            format!("expected a single document, found {n}; use parse_documents"),
        )),
    }
}

/// Parses every document in a YAML stream using default options.
pub fn parse_documents(input: &str) -> Result<Vec<Value>> {
    parse_documents_with(input, &ParseOptions::default())
}

/// Like [`parse_documents`], with explicit options.
pub fn parse_documents_with(input: &str, options: &ParseOptions) -> Result<Vec<Value>> {
    let events = parse_events(input, options)?;
    compose(&events, options)
}
```

- [ ] **Step 3: Wire the modules in `yaml2/src/lib.rs`.** Add the module declarations in alphabetical position and re-export the API. After editing, the module block reads:

```rust
mod api;
mod composer;
mod error;
mod event;
mod meta;
mod options;
mod parser;
mod scalar;
mod scanner;
mod value;

pub use api::{parse, parse_documents, parse_documents_with, parse_with};
pub use error::{Error, ErrorKind, Position, Result, Span};
pub use event::{Event, EventKind};
pub use meta::{Comments, Meta, ScalarStyle};
pub use options::{EmitOptions, Limits, MergeKeys, ParseOptions, Schema};
pub use parser::{parse_events, Parser};
pub use value::{Mapping, Value, ValueData};
```

- [ ] **Step 4: Add the test module to `yaml2/src/composer.rs`** (append at end of file).

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::ValueData;

    fn parse(input: &str) -> Value {
        crate::api::parse(input).unwrap()
    }

    #[test]
    fn scalar_string_document() {
        assert_eq!(parse("hello\n").as_str(), Some("hello"));
    }

    #[test]
    fn scalar_int_document() {
        assert_eq!(parse("42\n").as_int(), Some(42));
    }

    #[test]
    fn scalar_bool_document() {
        assert_eq!(parse("true\n").as_bool(), Some(true));
    }

    #[test]
    fn empty_explicit_document_is_null() {
        assert!(parse("---\n").is_null());
    }

    #[test]
    fn empty_input_is_null() {
        assert!(parse("").is_null());
    }

    #[test]
    fn quoted_scalar_stays_string() {
        assert_eq!(parse("\"42\"\n").as_str(), Some("42"));
    }

    #[test]
    fn multiple_documents_compose_each() {
        let docs = crate::api::parse_documents("--- a\n--- b\n").unwrap();
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].as_str(), Some("a"));
        assert_eq!(docs[1].as_str(), Some("b"));
    }

    #[test]
    fn parse_rejects_multiple_documents() {
        let err = crate::api::parse("--- a\n--- b\n").unwrap_err();
        assert_eq!(err.kind(), crate::error::ErrorKind::Compose);
    }
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p yaml2 composer::tests`
Expected: all pass. If `empty_explicit_document_is_null` fails, confirm `crate::scalar::resolve("", Plain, Core1_2)` returns `Null` (it does per `scalar.rs`) and that the parser emits an empty `Scalar` for `---\n` (it does per the event-parser tests).

- [ ] **Step 6: Full suite + clippy + fmt**

Run: `cargo test -p yaml2`
Expected: all pass (foundation + scanner + parser + composer).

Run: `cargo clippy -p yaml2 --all-targets -- -D warnings`
Expected: clean. (Note: `ValueData` is imported in `composer.rs` for later tasks; if clippy flags it as unused in this task only, remove the top-level `ValueData` from the `use crate::value::...` line and keep the test-module import — re-add it in Task 2.)

Run: `cargo fmt` then `cargo fmt --check`
Expected: no diff.

- [ ] **Step 7: Commit**

```bash
git add yaml2/src/composer.rs yaml2/src/api.rs yaml2/src/lib.rs
git commit -m "feat(yaml2): compose scalar documents and add parse entry points"
```

---

## Task 2: Sequences

**Files:**
- Modify: `yaml2/src/composer.rs`

- [ ] **Step 1: Write the failing tests** (add to the `tests` module).

```rust
    #[test]
    fn flow_sequence_of_scalars() {
        let v = parse("[1, 2, 3]\n");
        let items = v.as_sequence().unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].as_int(), Some(1));
        assert_eq!(items[2].as_int(), Some(3));
    }

    #[test]
    fn block_sequence_of_scalars() {
        let v = parse("- a\n- b\n");
        let items = v.as_sequence().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].as_str(), Some("a"));
        assert_eq!(items[1].as_str(), Some("b"));
    }

    #[test]
    fn nested_sequence() {
        let v = parse("- - 1\n");
        let outer = v.as_sequence().unwrap();
        assert_eq!(outer.len(), 1);
        let inner = outer[0].as_sequence().unwrap();
        assert_eq!(inner[0].as_int(), Some(1));
    }

    #[test]
    fn empty_flow_sequence_is_empty() {
        assert_eq!(parse("[]\n").as_sequence().unwrap().len(), 0);
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p yaml2 composer::tests::flow_sequence_of_scalars`
Expected: FAIL — `compose_node` returns an "unexpected event" error on `SequenceStart`.

- [ ] **Step 3: Add the `SequenceStart` arm and `compose_sequence`.** In `compose_node`, replace the catch-all `other =>` arm so the match becomes:

```rust
        match event.kind {
            EventKind::Scalar {
                value,
                style,
                anchor,
                tag,
            } => {
                let composed = self.resolve_scalar(&value, style, tag.as_deref(), event.span)?;
                if let Some(name) = anchor {
                    self.anchors.insert(name, composed.clone());
                }
                Ok(composed)
            }
            EventKind::SequenceStart { anchor, tag } => self.compose_sequence(anchor, tag),
            other => Err(Error::new(
                ErrorKind::Compose,
                format!("unexpected event while composing a node: {other:?}"),
            )),
        }
```

Then add this method inside `impl Composer<'_>` (after `compose_node`):

```rust
    fn compose_sequence(
        &mut self,
        anchor: Option<String>,
        _tag: Option<String>,
    ) -> Result<Value> {
        let mut items = Vec::new();
        loop {
            match self.peek() {
                Some(EventKind::SequenceEnd) => {
                    self.bump()?;
                    break;
                }
                Some(_) => items.push(self.compose_node()?),
                None => {
                    return Err(Error::new(
                        ErrorKind::Compose,
                        "unterminated sequence in event stream",
                    ))
                }
            }
        }
        let value = Value::sequence(items);
        if let Some(name) = anchor {
            self.anchors.insert(name, value.clone());
        }
        Ok(value)
    }
```

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p yaml2 composer::tests`
Expected: all pass.

- [ ] **Step 5: Full suite + clippy**

Run: `cargo test -p yaml2 && cargo clippy -p yaml2 --all-targets -- -D warnings`
Expected: all pass, clean.

- [ ] **Step 6: Commit**

```bash
git add yaml2/src/composer.rs
git commit -m "feat(yaml2): compose sequence nodes"
```

---

## Task 3: Mappings

**Files:**
- Modify: `yaml2/src/composer.rs`

- [ ] **Step 1: Write the failing tests** (add to the `tests` module).

```rust
    fn key(s: &str) -> Value {
        Value::string(s)
    }

    #[test]
    fn flow_mapping_of_scalars() {
        let v = parse("{a: 1, b: 2}\n");
        let m = v.as_mapping().unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m.get(&key("a")).unwrap().as_int(), Some(1));
        assert_eq!(m.get(&key("b")).unwrap().as_int(), Some(2));
    }

    #[test]
    fn block_mapping_of_scalars() {
        let v = parse("a: 1\nb: 2\n");
        let m = v.as_mapping().unwrap();
        assert_eq!(m.get(&key("a")).unwrap().as_int(), Some(1));
        assert_eq!(m.get(&key("b")).unwrap().as_int(), Some(2));
    }

    #[test]
    fn nested_mapping() {
        let v = parse("outer:\n  inner: 7\n");
        let outer = v.as_mapping().unwrap();
        let inner = outer.get(&key("outer")).unwrap().as_mapping().unwrap();
        assert_eq!(inner.get(&key("inner")).unwrap().as_int(), Some(7));
    }

    #[test]
    fn mapping_empty_value_is_null() {
        let v = parse("a:\n");
        let m = v.as_mapping().unwrap();
        assert!(m.get(&key("a")).unwrap().is_null());
    }

    #[test]
    fn duplicate_keys_last_wins() {
        let v = parse("{a: 1, a: 2}\n");
        let m = v.as_mapping().unwrap();
        assert_eq!(m.len(), 1);
        assert_eq!(m.get(&key("a")).unwrap().as_int(), Some(2));
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p yaml2 composer::tests::flow_mapping_of_scalars`
Expected: FAIL — `compose_node` errors on `MappingStart`.

- [ ] **Step 3: Add the `MappingStart` arm and `compose_mapping`.** In `compose_node`, add this arm just before the catch-all:

```rust
            EventKind::MappingStart { anchor, tag } => self.compose_mapping(anchor, tag),
```

Then add this method inside `impl Composer<'_>` (after `compose_sequence`). Merge handling is added in Task 7; for now every pair is inserted directly:

```rust
    fn compose_mapping(
        &mut self,
        anchor: Option<String>,
        _tag: Option<String>,
    ) -> Result<Value> {
        let mut map = Mapping::new();
        loop {
            match self.peek() {
                Some(EventKind::MappingEnd) => {
                    self.bump()?;
                    break;
                }
                Some(_) => {
                    let k = self.compose_node()?;
                    let val = self.compose_node()?;
                    map.insert(k, val);
                }
                None => {
                    return Err(Error::new(
                        ErrorKind::Compose,
                        "unterminated mapping in event stream",
                    ))
                }
            }
        }
        let value = Value::mapping(map);
        if let Some(name) = anchor {
            self.anchors.insert(name, value.clone());
        }
        Ok(value)
    }
```

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p yaml2 composer::tests`
Expected: all pass.

- [ ] **Step 5: Full suite + clippy**

Run: `cargo test -p yaml2 && cargo clippy -p yaml2 --all-targets -- -D warnings`
Expected: all pass, clean.

- [ ] **Step 6: Commit**

```bash
git add yaml2/src/composer.rs
git commit -m "feat(yaml2): compose mapping nodes"
```

---

## Task 4: Anchors and aliases

**Files:**
- Modify: `yaml2/src/composer.rs`

- [ ] **Step 1: Write the failing tests** (add to the `tests` module).

```rust
    #[test]
    fn alias_clones_scalar_anchor() {
        let v = parse("first: &a 7\nsecond: *a\n");
        let m = v.as_mapping().unwrap();
        assert_eq!(m.get(&key("first")).unwrap().as_int(), Some(7));
        assert_eq!(m.get(&key("second")).unwrap().as_int(), Some(7));
    }

    #[test]
    fn alias_clones_collection_anchor() {
        let v = parse("a: &seq [1, 2]\nb: *seq\n");
        let m = v.as_mapping().unwrap();
        let b = m.get(&key("b")).unwrap().as_sequence().unwrap();
        assert_eq!(b.len(), 2);
        assert_eq!(b[1].as_int(), Some(2));
    }

    #[test]
    fn unknown_alias_is_compose_error() {
        let err = crate::api::parse("*missing\n").unwrap_err();
        assert_eq!(err.kind(), crate::error::ErrorKind::Compose);
    }

    #[test]
    fn anchors_do_not_leak_across_documents() {
        // `*a` in the second document must not see the first document's anchor.
        let err = crate::api::parse_documents("--- &a 1\n--- *a\n").unwrap_err();
        assert_eq!(err.kind(), crate::error::ErrorKind::Compose);
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p yaml2 composer::tests::alias_clones_scalar_anchor`
Expected: FAIL — `compose_node` errors on `Alias`.

- [ ] **Step 3: Add the `Alias` arm, `resolve_alias`, and the `count_nodes` helper.** In `compose_node`, add this arm just before the catch-all:

```rust
            EventKind::Alias(name) => self.resolve_alias(&name, event.span),
```

Add this method inside `impl Composer<'_>` (after `compose_mapping`):

```rust
    fn resolve_alias(&mut self, name: &str, span: Span) -> Result<Value> {
        let value = self
            .anchors
            .get(name)
            .cloned()
            .ok_or_else(|| self.error(span, format!("unknown anchor '{name}'")))?;
        self.alias_nodes += count_nodes(&value);
        if self.alias_nodes > self.options.limits.max_aliases {
            return Err(Error::new(
                ErrorKind::LimitExceeded,
                "alias expansion limit exceeded",
            )
            .with_span(span));
        }
        Ok(value)
    }
```

Add this free function at the end of the file, **before** the `#[cfg(test)] mod tests` block:

```rust
/// Counts the nodes in a value tree (the node itself plus all descendants).
/// Used to budget alias expansion against `Limits::max_aliases`.
fn count_nodes(value: &Value) -> usize {
    1 + match value.data() {
        ValueData::Sequence(items) => items.iter().map(count_nodes).sum(),
        ValueData::Mapping(map) => map
            .iter()
            .map(|(k, v)| count_nodes(k) + count_nodes(v))
            .sum(),
        _ => 0,
    }
}
```

(Note: `count_nodes` uses `ValueData`, which is already imported at the top of the module. If you removed that import in Task 1, re-add `ValueData` to `use crate::value::{...}` now.)

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p yaml2 composer::tests`
Expected: all pass.

- [ ] **Step 5: Full suite + clippy**

Run: `cargo test -p yaml2 && cargo clippy -p yaml2 --all-targets -- -D warnings`
Expected: all pass, clean.

- [ ] **Step 6: Commit**

```bash
git add yaml2/src/composer.rs
git commit -m "feat(yaml2): resolve anchors and aliases by cloning"
```

---

## Task 5: Alias expansion limit (billion-laughs guard)

**Files:**
- Modify: `yaml2/src/composer.rs`

The mechanism landed in Task 4; this task pins its behaviour with tests and confirms the limit is enforced and configurable.

- [ ] **Step 1: Write the tests** (add to the `tests` module).

```rust
    use crate::options::{Limits, ParseOptions};

    #[test]
    fn aliases_within_budget_succeed() {
        // Two aliases of a 2-element sequence: well under the default budget.
        let v = parse("base: &b [1, 2]\nx: *b\ny: *b\n");
        assert!(v.as_mapping().unwrap().get(&key("y")).is_some());
    }

    #[test]
    fn nested_alias_expansion_exceeds_small_budget() {
        // Classic billion-laughs shape: each level references the previous one
        // several times, so materialized node count explodes.
        let input = "\
a: &a [x, x, x, x, x]
b: &b [*a, *a, *a, *a, *a]
c: &c [*b, *b, *b, *b, *b]
d: &d [*c, *c, *c, *c, *c]
e: [*d, *d, *d, *d, *d]
";
        let opts = ParseOptions {
            limits: Limits {
                max_aliases: 1_000,
                ..Default::default()
            },
            ..Default::default()
        };
        let err = crate::api::parse_with(input, &opts).unwrap_err();
        assert_eq!(err.kind(), crate::error::ErrorKind::LimitExceeded);
    }

    #[test]
    fn generous_budget_allows_the_same_input() {
        let input = "\
a: &a [x, x]
b: [*a, *a]
";
        let opts = ParseOptions {
            limits: Limits {
                max_aliases: 1_000_000,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(crate::api::parse_with(input, &opts).is_ok());
    }
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p yaml2 composer::tests::nested_alias_expansion_exceeds_small_budget composer::tests::aliases_within_budget_succeed composer::tests::generous_budget_allows_the_same_input`
Expected: all pass (the guard from Task 4 enforces the budget). If `nested_alias_expansion_exceeds_small_budget` does NOT error, the `alias_nodes` accumulation is wrong — verify `count_nodes` sums children and that `resolve_alias` adds the cloned tree's count on every alias. Do not weaken the test.

- [ ] **Step 3: Full suite + clippy**

Run: `cargo test -p yaml2 && cargo clippy -p yaml2 --all-targets -- -D warnings`
Expected: all pass, clean. (`Limits`/`ParseOptions` are now imported in the test module; the top-of-file `use crate::options::{ParseOptions, Schema};` is still needed for the composer itself.)

- [ ] **Step 4: Commit**

```bash
git add yaml2/src/composer.rs
git commit -m "test(yaml2): enforce alias expansion budget against billion-laughs"
```

---

## Task 6: Tag resolution for core scalar tags

**Files:**
- Modify: `yaml2/src/composer.rs`

- [ ] **Step 1: Write the failing tests** (add to the `tests` module).

```rust
    #[test]
    fn str_tag_forces_string() {
        assert_eq!(parse("!!str 123\n").as_str(), Some("123"));
    }

    #[test]
    fn int_tag_on_quoted_value() {
        assert_eq!(parse("!!int \"5\"\n").as_int(), Some(5));
    }

    #[test]
    fn float_tag_widens_integer_text() {
        assert_eq!(parse("!!float 5\n").as_float(), Some(5.0));
    }

    #[test]
    fn bool_tag_parses_keyword() {
        assert_eq!(parse("!!bool true\n").as_bool(), Some(true));
    }

    #[test]
    fn null_tag_parses_tilde() {
        assert!(parse("!!null ~\n").is_null());
    }

    #[test]
    fn mismatched_int_tag_is_error() {
        let err = crate::api::parse("!!int notanumber\n").unwrap_err();
        assert_eq!(err.kind(), crate::error::ErrorKind::Compose);
    }

    #[test]
    fn collection_tag_on_scalar_is_error() {
        let err = crate::api::parse("!!map scalar\n").unwrap_err();
        assert_eq!(err.kind(), crate::error::ErrorKind::Compose);
    }

    #[test]
    fn unknown_tag_on_scalar_resolves_to_string() {
        assert_eq!(parse("!custom 42\n").as_str(), Some("42"));
    }

    #[test]
    fn non_specific_tag_forces_string() {
        assert_eq!(parse("! 42\n").as_str(), Some("42"));
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p yaml2 composer::tests::str_tag_forces_string`
Expected: FAIL — `resolve_scalar` currently ignores the tag, so `!!str 123` resolves to `Int(123)` and `.as_str()` is `None`.

- [ ] **Step 3: Implement tag-aware `resolve_scalar`.** Replace the whole `resolve_scalar` method with:

```rust
    /// Resolves a scalar event into a typed `Value`, honoring an explicit core
    /// tag if present. Untagged scalars use the configured schema. The
    /// non-specific `!` and unknown/custom tags resolve to strings.
    fn resolve_scalar(
        &self,
        raw: &str,
        style: ScalarStyle,
        tag: Option<&str>,
        span: Span,
    ) -> Result<Value> {
        let Some(tag) = tag else {
            return Ok(Value::from_scalar(raw, style, self.options.schema));
        };
        match classify_tag(tag) {
            Some(CoreTag::Str) => Ok(Value::string(raw)),
            Some(CoreTag::Null) => match typed(raw) {
                ValueData::Null => Ok(Value::null()),
                _ => Err(self.error(span, format!("value {raw:?} is not a valid !!null"))),
            },
            Some(CoreTag::Bool) => match typed(raw) {
                ValueData::Bool(b) => Ok(Value::bool(b)),
                _ => Err(self.error(span, format!("value {raw:?} is not a valid !!bool"))),
            },
            Some(CoreTag::Int) => match typed(raw) {
                ValueData::Int(i) => Ok(Value::int(i)),
                _ => Err(self.error(span, format!("value {raw:?} is not a valid !!int"))),
            },
            Some(CoreTag::Float) => match typed(raw) {
                ValueData::Float(f) => Ok(Value::float(f)),
                ValueData::Int(i) => Ok(Value::float(i as f64)),
                _ => Err(self.error(span, format!("value {raw:?} is not a valid !!float"))),
            },
            Some(CoreTag::Seq) | Some(CoreTag::Map) => {
                Err(self.error(span, format!("collection tag '{tag}' applied to a scalar node")))
            }
            // Non-specific `!` or any unknown/custom tag: treat as a string.
            None => Ok(Value::string(raw)),
        }
    }
```

Add these free items at the end of the file, **before** the `#[cfg(test)] mod tests` block (next to `count_nodes`):

```rust
/// The seven core schema tags this layer understands on scalar nodes.
enum CoreTag {
    Str,
    Null,
    Bool,
    Int,
    Float,
    Seq,
    Map,
}

/// Maps a raw tag string (shorthand `!!x` or verbatim `tag:yaml.org,2002:x`) to
/// a core tag, or `None` for the non-specific `!` and unknown/custom tags.
fn classify_tag(tag: &str) -> Option<CoreTag> {
    match tag {
        "!!str" | "tag:yaml.org,2002:str" => Some(CoreTag::Str),
        "!!null" | "tag:yaml.org,2002:null" => Some(CoreTag::Null),
        "!!bool" | "tag:yaml.org,2002:bool" => Some(CoreTag::Bool),
        "!!int" | "tag:yaml.org,2002:int" => Some(CoreTag::Int),
        "!!float" | "tag:yaml.org,2002:float" => Some(CoreTag::Float),
        "!!seq" | "tag:yaml.org,2002:seq" => Some(CoreTag::Seq),
        "!!map" | "tag:yaml.org,2002:map" => Some(CoreTag::Map),
        _ => None,
    }
}

/// Resolves raw text under the Core 1.2 schema for tag-driven typing. A core
/// tag explicitly requests its type, so we type the text with core rules
/// regardless of the document's configured schema.
fn typed(raw: &str) -> ValueData {
    crate::scalar::resolve(raw, ScalarStyle::Plain, Schema::Core1_2)
}
```

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p yaml2 composer::tests`
Expected: all pass.

- [ ] **Step 5: Full suite + clippy**

Run: `cargo test -p yaml2 && cargo clippy -p yaml2 --all-targets -- -D warnings`
Expected: all pass, clean. (`Schema` is used by `typed`; keep the top-of-file `use crate::options::{ParseOptions, Schema};`.)

- [ ] **Step 6: Commit**

```bash
git add yaml2/src/composer.rs
git commit -m "feat(yaml2): resolve core scalar tags during composition"
```

---

## Task 7: Merge keys (`<<`)

**Files:**
- Modify: `yaml2/src/composer.rs`

- [ ] **Step 1: Write the failing tests** (add to the `tests` module).

```rust
    use crate::options::MergeKeys;

    fn parse_merge(input: &str) -> Value {
        // Merge keys on, Core scalar resolution (numbers resolve plainly).
        let opts = ParseOptions {
            merge_keys: MergeKeys::On,
            ..Default::default()
        };
        crate::api::parse_with(input, &opts).unwrap()
    }

    #[test]
    fn merge_pulls_in_anchor_entries() {
        let input = "\
base: &b
  a: 1
  b: 2
derived:
  <<: *b
  c: 3
";
        let m = parse_merge(input);
        let d = m.as_mapping().unwrap().get(&key("derived")).unwrap();
        let d = d.as_mapping().unwrap();
        assert_eq!(d.len(), 3);
        assert_eq!(d.get(&key("a")).unwrap().as_int(), Some(1));
        assert_eq!(d.get(&key("b")).unwrap().as_int(), Some(2));
        assert_eq!(d.get(&key("c")).unwrap().as_int(), Some(3));
        assert!(!d.contains_key(&key("<<")));
    }

    #[test]
    fn explicit_key_overrides_merged() {
        let input = "\
base: &b
  a: 1
  b: 2
derived:
  <<: *b
  b: 99
";
        let m = parse_merge(input);
        let d = m.as_mapping().unwrap().get(&key("derived")).unwrap();
        let d = d.as_mapping().unwrap();
        assert_eq!(d.get(&key("b")).unwrap().as_int(), Some(99));
        assert_eq!(d.get(&key("a")).unwrap().as_int(), Some(1));
    }

    #[test]
    fn merge_sequence_earlier_source_wins() {
        let input = "\
one: &one {a: 1, x: 10}
two: &two {a: 2, y: 20}
merged:
  <<: [*one, *two]
";
        let m = parse_merge(input);
        let d = m.as_mapping().unwrap().get(&key("merged")).unwrap();
        let d = d.as_mapping().unwrap();
        // `a` present in both; earlier source (*one) wins.
        assert_eq!(d.get(&key("a")).unwrap().as_int(), Some(1));
        assert_eq!(d.get(&key("x")).unwrap().as_int(), Some(10));
        assert_eq!(d.get(&key("y")).unwrap().as_int(), Some(20));
    }

    #[test]
    fn merge_disabled_keeps_literal_key() {
        // Default options: Core schema, merge keys Auto -> off.
        let v = parse("<<: {a: 1}\nb: 2\n");
        let m = v.as_mapping().unwrap();
        assert!(m.contains_key(&key("<<")));
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn quoted_merge_key_is_literal_even_when_enabled() {
        let v = parse_merge("\"<<\": 5\nb: 2\n");
        let m = v.as_mapping().unwrap();
        assert!(m.contains_key(&key("<<")));
        assert_eq!(m.get(&key("<<")).unwrap().as_int(), Some(5));
    }

    #[test]
    fn merge_non_mapping_value_is_error() {
        let err = {
            let opts = ParseOptions {
                merge_keys: MergeKeys::On,
                ..Default::default()
            };
            crate::api::parse_with("<<: 5\n", &opts).unwrap_err()
        };
        assert_eq!(err.kind(), crate::error::ErrorKind::Compose);
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p yaml2 composer::tests::merge_pulls_in_anchor_entries`
Expected: FAIL — `<<` is currently inserted as a literal string key, so `derived` has a `<<` entry and the merged keys are missing.

- [ ] **Step 3: Implement merge handling.** First, add a `merge_enabled` field to `Composer` and set it in `compose`.

In the struct definition, add the field:

```rust
struct Composer<'a> {
    events: &'a [Event],
    pos: usize,
    options: &'a ParseOptions,
    anchors: HashMap<String, Value>,
    alias_nodes: usize,
    merge_enabled: bool,
}
```

In `compose`, initialize it:

```rust
pub(crate) fn compose(events: &[Event], options: &ParseOptions) -> Result<Vec<Value>> {
    let mut composer = Composer {
        events,
        pos: 0,
        options,
        anchors: HashMap::new(),
        alias_nodes: 0,
        merge_enabled: options.merge_keys.enabled_for(options.schema),
    };
    composer.compose_stream()
}
```

Replace `compose_mapping` with the merge-aware version:

```rust
    fn compose_mapping(
        &mut self,
        anchor: Option<String>,
        _tag: Option<String>,
    ) -> Result<Value> {
        let mut map = Mapping::new();
        // Merge sources, in source order; applied after all explicit keys.
        let mut merges: Vec<Mapping> = Vec::new();
        loop {
            match self.peek() {
                Some(EventKind::MappingEnd) => {
                    self.bump()?;
                    break;
                }
                None => {
                    return Err(Error::new(
                        ErrorKind::Compose,
                        "unterminated mapping in event stream",
                    ))
                }
                Some(_) => {
                    if self.merge_enabled && self.peek_is_merge_key() {
                        let span = self.events[self.pos].span;
                        self.bump()?; // consume the `<<` key event
                        let value = self.compose_node()?;
                        self.collect_merge_sources(value, &mut merges, span)?;
                    } else {
                        let k = self.compose_node()?;
                        let val = self.compose_node()?;
                        map.insert(k, val);
                    }
                }
            }
        }
        // Apply merges: explicit keys (and earlier sources) win.
        for source in merges {
            for (k, v) in source.iter() {
                if !map.contains_key(k) {
                    map.insert(k.clone(), v.clone());
                }
            }
        }
        let value = Value::mapping(map);
        if let Some(name) = anchor {
            self.anchors.insert(name, value.clone());
        }
        Ok(value)
    }

    /// True if the next event is a plain, untagged `<<` scalar key.
    fn peek_is_merge_key(&self) -> bool {
        match self.events.get(self.pos).map(|e| &e.kind) {
            Some(EventKind::Scalar {
                value,
                style: ScalarStyle::Plain,
                tag: None,
                anchor: None,
            }) => value == "<<",
            _ => false,
        }
    }

    /// Flattens a merge value into ordered mapping sources. The value is a
    /// mapping, or a sequence of mappings.
    fn collect_merge_sources(
        &self,
        value: Value,
        merges: &mut Vec<Mapping>,
        span: Span,
    ) -> Result<()> {
        match value.into_data() {
            ValueData::Mapping(m) => merges.push(m),
            ValueData::Sequence(items) => {
                for item in items {
                    match item.into_data() {
                        ValueData::Mapping(m) => merges.push(m),
                        _ => {
                            return Err(self.error(
                                span,
                                "merge sequence entry must be a mapping",
                            ))
                        }
                    }
                }
            }
            _ => {
                return Err(self.error(
                    span,
                    "merge value must be a mapping or a sequence of mappings",
                ))
            }
        }
        Ok(())
    }
```

- [ ] **Step 4: Run to verify they pass**

Run: `cargo test -p yaml2 composer::tests`
Expected: all pass.

- [ ] **Step 5: Full suite + clippy**

Run: `cargo test -p yaml2 && cargo clippy -p yaml2 --all-targets -- -D warnings`
Expected: all pass, clean.

- [ ] **Step 6: Commit**

```bash
git add yaml2/src/composer.rs
git commit -m "feat(yaml2): apply merge keys during mapping composition"
```

---

## Task 8: Integration and full verification

**Files:**
- Modify: `yaml2/src/composer.rs`

- [ ] **Step 1: Write the integration tests** (add to the `tests` module).

```rust
    #[test]
    fn realistic_document_tree() {
        let input = "\
name: Ada
active: true
scores: [10, 20, 30]
address:
  city: Portland
  zip: \"97201\"
";
        let v = parse(input);
        let m = v.as_mapping().unwrap();
        assert_eq!(m.get(&key("name")).unwrap().as_str(), Some("Ada"));
        assert_eq!(m.get(&key("active")).unwrap().as_bool(), Some(true));
        let scores = m.get(&key("scores")).unwrap().as_sequence().unwrap();
        assert_eq!(scores.len(), 3);
        assert_eq!(scores[1].as_int(), Some(20));
        let addr = m.get(&key("address")).unwrap().as_mapping().unwrap();
        assert_eq!(addr.get(&key("city")).unwrap().as_str(), Some("Portland"));
        // Quoted zip stays a string, not an int.
        assert_eq!(addr.get(&key("zip")).unwrap().as_str(), Some("97201"));
    }

    #[test]
    fn anchors_aliases_and_merge_together() {
        let input = "\
defaults: &d
  retries: 3
  timeout: 30
job:
  <<: *d
  timeout: 60
";
        let opts = ParseOptions {
            merge_keys: MergeKeys::On,
            ..Default::default()
        };
        let v = crate::api::parse_with(input, &opts).unwrap();
        let job = v.as_mapping().unwrap().get(&key("job")).unwrap();
        let job = job.as_mapping().unwrap();
        assert_eq!(job.get(&key("retries")).unwrap().as_int(), Some(3));
        assert_eq!(job.get(&key("timeout")).unwrap().as_int(), Some(60));
    }

    #[test]
    fn schema_changes_scalar_typing() {
        // Under Yaml1_1, "yes" is a bool; under Core it is a string.
        let yes_core = parse("yes\n");
        assert_eq!(yes_core.as_str(), Some("yes"));

        let opts = ParseOptions {
            schema: crate::options::Schema::Yaml1_1,
            ..Default::default()
        };
        let yes_11 = crate::api::parse_with("yes\n", &opts).unwrap();
        assert_eq!(yes_11.as_bool(), Some(true));
    }
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p yaml2 composer::tests::realistic_document_tree composer::tests::anchors_aliases_and_merge_together composer::tests::schema_changes_scalar_typing`
Expected: all pass. If any fails, capture actual-vs-expected and debug the relevant composer path; do not change expectations.

- [ ] **Step 3: Full crate suite**

Run: `cargo test -p yaml2`
Expected: all foundation + scanner + parser + composer tests pass.

- [ ] **Step 4: Clippy and formatting**

Run: `cargo clippy -p yaml2 --all-targets -- -D warnings`
Expected: clean.

Run: `cargo fmt` then `cargo fmt --check`
Expected: no diff. Confirm `git status --short` is clean after the commit.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "test(yaml2): integration tests for the composer"
```

---

## Self-Review

**Spec coverage** (against `docs/superpowers/specs/2026-06-08-yaml2-parser-design.md`):
- "Composer — events to the owned `Value` tree; resolves anchors/aliases, applies the schema's scalar resolution, applies merge keys when enabled" → Tasks 1–7. ✓
- Public API `parse`, `parse_with`, `parse_documents` → Task 1 (`parse_documents` returns an eager `Vec` instead of the sketched iterator — divergence documented in Context). ✓
- Security: alias-expansion limit on by default → Tasks 4–5 (`max_aliases` as a materialized-node budget). Nesting-depth is already enforced upstream by the parser. ✓
- `Schema` scalar resolution + independent `MergeKeys` toggle honored → Tasks 6–7, verified in Task 8. ✓
- Deferred and called out: `preserve_formatting` metadata population (Plan 7), full tag handle/URI + custom-tag resolution (Plan 9), streaming `parse_documents` iterator, `serde`/emitter layers (Plans 7–8). ✓

**Placeholder scan:** every code step contains complete, compilable code. No TBD/TODO/"handle edge cases". ✓

**Type consistency:** `compose`/`Composer`/`compose_stream`/`compose_document`/`compose_node`/`compose_sequence`/`compose_mapping`/`resolve_scalar`/`resolve_alias`/`peek_is_merge_key`/`collect_merge_sources` names are stable across tasks. `count_nodes`, `classify_tag`, `typed`, `CoreTag` are free items. `resolve_scalar` keeps the same `(raw, style, tag, span)` signature from Task 1 through Task 6. `Composer` gains the `merge_enabled` field only in Task 7, with both the struct and `compose` initializer updated together. ✓
