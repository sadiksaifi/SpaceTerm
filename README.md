# Termspace

A native macOS terminal prototype built with Rust, GPUI, libghostty-vt, and a macOS PTY.

## Prerequisites

`libghostty-vt` currently requires Zig 0.15.2. With recent Xcode 26 SDKs, use Homebrew's patched build:

```sh
brew install zig@0.15
```

Because this formula is keg-only, expose it on your normal Homebrew path once:

```sh
brew link --force zig@0.15
```

Verify that the patched version is active:

```sh
zig version
# 0.15.2
```

## Run

```sh
cargo run
```

The current milestone opens one terminal Pane, launches `$SHELL` in a real PTY, handles keyboard input and Pane-bound resize events, and parses output with libghostty-vt. `TerminalPane` is the reusable leaf for future layouts: it shares only dirty terminal rows, coalesces stale screen updates, and paints cached row runs through a custom GPUI element instead of creating one UI element per cell.

## Validate

```sh
cargo test
cargo clippy --all-targets -- -D warnings
```
