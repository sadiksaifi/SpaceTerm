# SpaceTerm

A native macOS terminal implemented as a Rust application with one internal UI library.

## Required Context

- Read `CONTEXT.md` completely before changing application code, UI, platform integration,
  packaging, or architecture. It is canonical for product intent, technology decisions, UI
  constraints, and architectural principles.
- Also read `docs/UBIQUITOUS_LANGUAGE.md` completely before changing domain behavior, hierarchy,
  lifecycle, focus state, terminal ownership, or product-facing terminology.
- When a design or domain decision changes, update its canonical document in the same change; do
  not copy its definitions into this file.

## Commands

- `just run` — primary development loop.
- `just check` — compile every target and feature.
- `just test` — run the complete test suite.
- `just test-one <filter>` — run focused tests.
- `just validate` — run all required pre-commit validation.
- `just package` — build and verify native macOS artifacts.

## Architecture

- Shape: one macOS-only Rust application crate plus `crates/spaceterm-ui` for reusable controls.
- Entry points: `src/main.rs` and `src/app.rs`.
- Seams: `src/domain`, `src/ui`, `src/terminal`, `src/platform`, and `crates/spaceterm-ui`.
- Packaging: `Justfile`, `scripts/`, `packaging/macos`, and `assets/macos`.

## Working Rules

- Use the `Justfile` commands instead of reproducing validation or packaging pipelines.
- Treat `Cargo.lock` as Cargo-generated; do not edit generated `target/` or `dist/` contents.
- Add focused tests for changed behavior and keep unrelated user changes intact.

## Verification

- Run `just validate` before handoff.
- During iteration, run `just test-one <filter>` plus the narrowest relevant checks.

## Sharp Edges

- Run `just doctor` before packaging when local tool availability is uncertain.
- Product terminology intentionally differs from tmux, browser, and GPUI terminology; follow the
  ubiquitous language rather than visual analogy.
