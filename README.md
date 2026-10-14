# Vanta

Vanta is an experimental native text editor built around a small, dependable
core and optional development capabilities.

The project is currently implementing **v0.1: the headless text engine and
file-safety foundation**. The full evidence-gated roadmap is in
[`IMPLEMENTATION_PLAN.md`](IMPLEMENTATION_PLAN.md).

## Current slice

The first executable slice establishes:

- explicit byte, line/byte, grapheme-facing, UTF-16-facing, and anchor types;
- versioned, atomic multi-edit transactions;
- deterministic anchor transformation;
- document snapshots;
- undo and redo with monotonically increasing document versions;
- exact search and line indexing;
- a headless inspection CLI;
- correctness tests, including randomized edit/undo sequences.

The current `String` storage is intentionally a reference implementation. It is
not the final large-file representation. Rope, piece-tree, and hybrid candidates
will be evaluated against this behavior before a backend is selected.

## Build and test

```sh
cargo test --workspace
cargo run -p vanta-cli -- inspect README.md
```

## Repository layout

```text
crates/document  Core text model and transaction semantics
crates/cli       Headless harness for inspecting the engine
docs/rfcs        Architectural decisions, beginning with text semantics
```
