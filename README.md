<div align="center">
  <h1>Vanta</h1>
  <p><strong>A native text editor built from a small, dependable core.</strong></p>
  <p>Fast editing, explicit text semantics, safe file handling, and development tools that stay out of the way.</p>

  <a href="https://github.com/Amppire/vanta/stargazers"><img src="https://img.shields.io/github/stars/Amppire/vanta?style=flat-square" alt="GitHub stars"></a>
  <img src="https://img.shields.io/badge/status-experimental-orange?style=flat-square" alt="Experimental status">
  <img src="https://img.shields.io/badge/Rust-1.89%2B-dea584?style=flat-square&logo=rust" alt="Rust 1.89+">
  <img src="https://img.shields.io/badge/license-Apache--2.0-blue?style=flat-square" alt="Apache 2.0 license">
</div>

> [!IMPORTANT]
> Vanta is under active development. The document and file-safety foundation is
> functional, and v0.3 begins the editing-workflow layer on top of the native
> GPU editor. The application remains experimental and intentionally incomplete.

## Why Vanta?

Editors become difficult to trust when text positions are ambiguous, background
work races with user input, or a save can silently overwrite another process.
Vanta starts with those failure modes instead of treating them as later polish.

- **Correct text semantics.** Byte offsets, text points, anchors, selections, and
  document versions are distinct types with explicit behavior.
- **Atomic editing.** A transaction applies completely or leaves the document
  unchanged; stale snapshots and invalid ranges are rejected.
- **Safe files.** Loads reject binary or invalid UTF-8 content, while saves use a
  temporary sibling file and detect external changes before replacing anything.
- **Recoverable work.** Undo, redo, immutable snapshots, and recovery journals are
  part of the foundation rather than UI-level afterthoughts.
- **Storage independence.** The reference `String` backend defines behavior first,
  so a rope, piece tree, or hybrid store can be evaluated without changing callers.

## Features at a glance

| Area | What is available today |
|---|---|
| **Document model** | Versioned UTF-8 text, immutable snapshots, byte/point conversion, line indexing, and exact search |
| **Transactions** | Atomic multi-edit application, stale-version detection, range validation, and edit provenance |
| **Positions** | Deterministic anchors and directed selections with left/right insertion affinity |
| **History** | Undo and redo with restored selections and monotonically increasing versions |
| **File safety** | UTF-8/BOM detection, line-ending inspection, content fingerprints, conflict-aware atomic saves, and permission preservation |
| **Recovery** | Durable recovery records with discovery, validation, and removal APIs |
| **CLI** | Headless file inspection for encoding, line endings, document size, and end position |
| **Desktop** | Native winit/wgpu/glyphon window, keyboard and IME input, grapheme editing, mouse/drag selection, scrolling, clipboard, dirty state, and conflict-aware save |
| **Rendering** | GPU text plus caret/selection overlays, shaped-text hit-testing, resize and scale-factor handling, and surface recovery |
| **Commands** | Stable IDs, metadata, contextual enablement, deterministic palette search, and built-in editor command dispatch |
| **Correctness** | 24 deterministic and randomized tests covering edits, Unicode boundaries, history, saves, conflicts, recovery, editor state, and commands |

## Quick start

Vanta requires the stable Rust toolchain with Rust 1.89 or newer.

```bash
git clone https://github.com/Amppire/vanta.git
cd vanta
cargo test --workspace
```

Inspect a UTF-8 text file with the headless CLI:

```bash
cargo run -p vanta-cli -- inspect README.md
```

Launch the v0.3 desktop editor with a file:

```bash
cargo run -p vanta -- README.md
```

You can also launch without a path to create an untitled buffer:

```bash
cargo run -p vanta
```

### Desktop controls

| Input | Action |
|---|---|
| Text input / IME commit | Insert text at the logical cursor |
| `Enter` | Insert a line feed |
| `Tab` | Insert a tab |
| `Backspace` / `Delete` | Delete an extended grapheme or the active selection |
| `Left` / `Right` | Move across extended grapheme boundaries |
| `Shift+Left` / `Shift+Right` | Extend the selection |
| Mouse click / drag | Place the caret or select shaped text |
| Mouse wheel / trackpad | Scroll the text viewport |
| `Cmd+A/C/X/V` / `Ctrl+A/C/X/V` | Select all, copy, cut, or paste |
| `Cmd+S` / `Ctrl+S` | Save an existing file if it has not changed externally |

Vertical movement, undo/redo commands, file dialogs, Save As, IME preedit
display, accessibility semantics, and unsaved-close confirmation are not yet
wired into the desktop UI. An untitled buffer therefore cannot currently be
saved.

## How the core works

### Versioned transactions

Every edit targets one exact `DocumentVersion`. Multiple replacements are
validated in their original source coordinates and committed together. Vanta
rejects stale versions, overlapping edits, reversed ranges, invalid UTF-8
boundaries, and mismatched selection state before changing the document.

```rust
use vanta_document::{ByteOffset, Document, EditOrigin, TextEdit, Transaction};

let mut document = Document::new("alpha beta gamma");
let transaction = Transaction::new(document.version(), EditOrigin::User)
    .with_edit(TextEdit::replace(ByteOffset(0)..ByteOffset(5), "A"))
    .with_edit(TextEdit::replace(ByteOffset(11)..ByteOffset(16), "G"));

document.apply(transaction).unwrap();
assert_eq!(document.text(), "A beta G");
```

### Stable positions

An `Anchor` carries a byte offset and an affinity. At an insertion boundary,
left-affinity anchors stay before the inserted text while right-affinity anchors
follow it. Selections use two anchors, so their direction is preserved through
edits without relying on loosely defined integer offsets.

### Conflict-aware saves

`vanta-file-io` fingerprints the exact bytes loaded from disk. A later save can
require that fingerprint, refusing to overwrite a file changed by another
process. Successful saves are flushed through a temporary sibling file, preserve
existing permissions, replace the destination, and synchronize its directory.

## Architecture

```text
Vanta
├── crates/app        Native GPU-rendered desktop editor
├── crates/commands   Command identities, metadata, enablement, and discovery
├── crates/document   Versioned text model and transaction semantics
├── crates/file-io    Safe loading, conflict-aware saves, and recovery journals
└── crates/cli        Headless inspection and development harness
```

The filesystem layer stays separate from the document engine. This keeps the
core deterministic and makes storage, UI, language tooling, and collaboration
layers replaceable around a stable editing contract.

## Development status

The project is implementing **v0.3: editing workflow, workspaces, and
navigation**. The current priorities are:

1. Add a command palette and configurable, context-aware keybindings.
2. Implement workspace folders, file discovery, ignore rules, and quick open.
3. Add vertical navigation, undo/redo dispatch, sessions, and split panes.
4. Complete file dialogs, recovery lifecycle, accessibility, and IME preedit.

## Development

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Keep changes focused and preserve the behavioral contract: new storage or UI
work should not weaken transaction atomicity, Unicode-boundary validation,
external-change detection, or recovery guarantees.

## Contributing

Vanta is early, and focused contributions are welcome. Open an issue before a
large architectural change, keep pull requests narrow, and include tests for
new document or filesystem behavior.

## License

Vanta is licensed under the Apache License 2.0.
