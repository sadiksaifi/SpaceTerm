# Termspace

A native macOS terminal built with Rust, GPUI, `libghostty-vt`, and a macOS PTY.

## Requirements

- macOS
- Rust
- Zig 0.15.2

Install the Homebrew Zig build required by `libghostty-vt`:

```sh
brew install zig@0.15
brew link --force zig@0.15
```

## Run

```sh
cargo run
```

## Validate

```sh
cargo test
cargo clippy --all-targets --all-features --locked -- -D warnings
```
