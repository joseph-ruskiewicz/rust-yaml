# yaml2 — Design Spec

**Date:** 2026-06-08
**Status:** Approved (design); pending implementation plan
**Scope:** The `yaml2` crate only. The `yscript` language crate is deferred to its own brainstorm → spec → plan cycle.

## Summary

A modern, production-grade YAML library for Rust, built as a Cargo workspace with two crates:

- **`yaml2`** — a standalone, YAML 1.2-compliant parser/emitter library with a native `Value` tree and a feature-gated serde layer. Intended as a successor to the unmaintained `serde_yaml`.
- **`yscript`** — a YamlScript language runtime that depends on `yaml2` and consumes its `Value` tree directly. **Out of scope for this spec**; designed for later.

The dependency direction is strictly `yscript → yaml2`, never the reverse. The crates version and release independently.

This spec covers `yaml2`. The `Value` tree is designed now so it cleanly serves `yscript` later.

## Goals

- Full **YAML 1.2 core schema** compliance, validated against the official `yaml-test-suite`.
- A native, owned **`Value` tree** as the foundation, with serde `Serialize`/`Deserialize` as a feature-gated layer on top.
- **Format-preserving round-trip** (comments, key order, quoting style, blank lines) available via a parse option — a real gap in the Rust ecosystem (ruamel.yaml-style).
- **Rich diagnostics** with source spans.
- **Pure Rust**, memory-safe, no C/`libyaml` FFI, `unsafe` avoided where practical.
- **Secure by default** — alias-expansion, nesting-depth, and input-size limits enabled out of the box.

## Non-Goals (deferred)

- The entire `yscript` crate.
- Error recovery (collecting multiple errors / continuing past first error).
- `no_std` support (core is designed to keep it *possible*, but it is not a v1 goal).
- Async I/O.

## Architecture

A layered pipeline; each layer is independently testable.

```
bytes → scanner (tokens) → parser (events) → composer (Value tree) → serde layer
                                  │                                   (feature = "serde")
                                  └── public streaming API (events)
        tree → emitter (events → bytes)   ← round-trip / serialization
```

- **Scanner** — bytes to tokens. Zero-copy internally where cheap (slices into the input buffer).
- **Parser** — tokens to a stream of **events** (document start/end, mapping/sequence start/end, scalar, alias). This event stream is the public streaming API.
- **Composer** — events to the owned `Value` tree; resolves anchors/aliases, applies the schema's scalar resolution, applies merge keys when enabled.
- **Emitter** — the inverse: a `Value` tree (or event stream) back to bytes. With round-trip metadata present, output is byte-stable except for edits.

Rationale: the event layer is what enables streaming of large documents, accurate source spans, and a clean emitter. This is the architecture every serious YAML library converges on (libyaml, Go yaml, yaml-rust2).

## The `Value` model

One unified, **owned** tree (no input lifetimes), so `yscript` can build and transform trees freely.

- **Variants:** null, bool, int, float, string, sequence, mapping. Scalars also carry their resolved **tag** and original **anchor** identity where relevant.
- **Mappings are insertion-ordered** (`IndexMap`-style). **Key order is always preserved** — no flag needed; the cost is negligible and it matches modern expectations.
- **Optional formatting metadata** per node: comments (leading/trailing/inline), surrounding blank lines, original scalar/quoting style, and source span. This is `None` on the fast path (a few bytes of overhead, no allocation) and is populated only when `preserve_formatting` is enabled.

A single `Value` type serves all three consumers: the serde layer, round-trip editing, and `yscript`.

## Configuration

```rust
pub struct ParseOptions {
    /// Controls scalar resolution (booleans, ints, octal, the "Norway problem").
    pub schema: Schema,            // Core1_2 (default) | Json1_2 | Yaml1_1 | Failsafe
    /// Merge-key (`<<`) handling. Independent of schema.
    pub merge_keys: MergeKeys,     // Auto (on@1.1, off@1.2) | On | Off
    /// Populate formatting metadata for byte-stable round-trip.
    pub preserve_formatting: bool, // default: false
    /// Security guards — enabled by default.
    pub limits: Limits,            // alias-expansion / nesting-depth / input-size
}

pub struct EmitOptions {
    pub round_trip: bool,          // re-emit preserved comments/styles
    pub indent: usize,
    // ... (line width, scalar style preferences, etc.)
}
```

### Two-knob version model

Version/schema and merge keys are **decoupled** because they are orthogonal in the real world:

- **`Schema`** controls *scalar resolution*. The practical 1.1 vs 1.2 differences that bite users live here:
  - The **"Norway problem"**: 1.1 resolves `yes/no/on/off/y/n` (any case) as booleans; 1.2 core only `true/false`.
  - **Number formats**: `0777` is octal (511) under 1.1, decimal (777) under 1.2 core (which wants `0o777`); sexagesimal (`1:2:3`) is a number under 1.1, gone in 1.2.
- **`MergeKeys`** is an independent toggle. Merge keys originated as a YAML 1.1 *optional type* (the type repository), never part of core grammar, and were not carried into any standard 1.2 schema. They are common in the wild (Docker Compose, CI configs). `Auto` defaults them on for the 1.1 schema and off for 1.2, but either can be forced. This lets a caller say "strict 1.2 scalar resolution but tolerate merge keys."

## Public API (sketch)

```rust
// serde path (feature = "serde", on by default)
yaml2::from_str::<T>(s) -> Result<T>
yaml2::to_string<T>(&value) -> Result<String>

// native tree
yaml2::parse(s) -> Result<Value>                 // default options
yaml2::parse_with(s, &ParseOptions) -> Result<Value>
yaml2::parse_documents(s) -> impl Iterator<Item = Result<Value>>  // multi-doc, streaming
yaml2::to_string_with(&Value, &EmitOptions) -> Result<String>

// low-level streaming
yaml2::Parser / yaml2::Events                    // event stream
```

## Errors & diagnostics

- Own `Error` type carrying **byte offset + line/column + source span** and clear, specific messages.
- Span data is exposed so callers can render however they like.
- **Feature-gated `miette` integration** for pretty terminal output with carets (`^^^`).
- **Fail-fast** (stop at first error). Recovery is deferred.

## Compliance & testing

- **Compliance bar:** full YAML 1.2 core schema, validated against the official **`yaml-test-suite`** (~350 cases), wired into CI as the gate.
- **Round-trip property tests:** `parse → emit → parse` is stable; with `preserve_formatting`, output is byte-stable except for intentional edits.
- **Fuzzing** (`cargo-fuzz`) on the scanner/parser for panic-safety and to confirm the security limits hold.

## Pinned decisions

| Decision | Choice |
|---|---|
| Workspace | Two crates: `yaml2`, `yscript` (this spec: `yaml2` only) |
| Crate names | `yaml2` (available), `yscript` (available); CLI binary `ys` later |
| Goal | Production-grade serde replacement |
| Core | Owned, unified `Value` tree; ordered mappings always |
| Round-trip | Optional per-node metadata, gated by `preserve_formatting` |
| Pipeline | scanner → parser (events) → composer → tree; emitter inverse |
| Version model | `Schema` (scalar resolution) + independent `MergeKeys` toggle |
| Implementation | Pure Rust, no C FFI, `unsafe` avoided |
| serde | Both directions, `feature = "serde"`, on by default |
| Errors | Rich spans, own `Error`, `miette` feature-gated, fail-fast |
| std | std-only v1; `no_std` kept possible, not a goal |
| Testing | `yaml-test-suite` + property round-trip + `cargo-fuzz` |
