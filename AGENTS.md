# SpaceTerm

SpaceTerm is a native macOS terminal written in Rust with GPUI and one internal UI library.

## Context

- Read [`CONTEXT.md`](CONTEXT.md) before changing product or architecture decisions.
- Read [`docs/UBIQUITOUS_LANGUAGE.md`](docs/UBIQUITOUS_LANGUAGE.md) before changing domain behavior
  or product terminology.

Inspect the code for implementation details. Keep each durable decision in one canonical document
and link to it instead of repeating it.

## Work

- Use the `Justfile` as the command authority. Start with a focused check and finish with
  `just validate`.
- Add focused tests for changed behavior and preserve unrelated changes.
- Let Cargo generate `Cargo.lock`; keep generated `target/` and `dist/` contents out of source
  edits.
- Debug the source build; `/Applications/SpaceTerm.app` may be stale.
- Run `just doctor` before packaging when tool availability is uncertain.
