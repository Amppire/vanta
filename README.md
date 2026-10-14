# Vanta

Vanta is restarting from the smallest useful product: a trustworthy native text
editor.

The old project proved many individual ideas, but it tried to design the final
platform before proving the daily editing experience. This codebase now grows
from working vertical slices instead.

## First milestone

One native window that can:

1. open a UTF-8 text file;
2. edit it correctly with keyboard, mouse, clipboard, and IME input;
3. save it without silently overwriting external changes;
4. recover unsaved work after a crash;
5. expose a usable accessibility tree.

Nothing else is in scope until that loop feels good and is measured. In
particular, there are no plugins, LSP, public framework crates, workspace docks,
release infrastructure, or speculative service boundaries yet.

## Principles

- The document is the product; chrome is secondary.
- Correctness and data safety are not optional simplifications.
- Background work never owns mutable document state.
- Add a dependency or crate only after a working slice demonstrates the need.
- Accessibility, IME behavior, and performance are acceptance criteria, not
  cleanup work.

## Run

```sh
cargo run
```

