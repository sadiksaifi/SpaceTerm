# SpaceTerm

A native macOS terminal built with Rust, GPUI, `libghostty-vt`, and a macOS PTY.

## Requirements

- macOS
- Rust
- Xcode 26 or newer
- Zig 0.15.2
- Just
- ShellCheck
- `cargo-packager` 0.11.8

Install the Homebrew Zig build required by `libghostty-vt`:

```sh
brew install just shellcheck zig@0.15
brew link --force zig@0.15
```

Install the pinned Rust packaging tool:

```sh
just install-packager
```

Run `just doctor` to verify the Rust, Xcode, packaging, and native build tools.

Run `just` to list the common development, validation, and packaging commands.

## Run

```sh
just run
```

## Documentation

- [Remote Project Workspaces](docs/REMOTE_PROJECT_WORKSPACES.md) covers Remote over SSH setup,
  authentication, lifecycle, reconnect, security, and troubleshooting.

## Package for macOS

Create a release application bundle and installer disk image:

```sh
just package
```

Packaging uses pinned `cargo-packager` for application-bundle and DMG construction. Xcode compiles
the tracked `assets/macos/SpaceTerm.icon` into the layered icon catalog and legacy macOS fallback.

Pass a build number when producing another build of the same Cargo version, for example `just package 2`.

The native-architecture artifacts are written to:

- `dist/SpaceTerm.app`
- `dist/SpaceTerm.dmg`

The DMG contains `SpaceTerm.app` and an Applications shortcut. To build a universal Apple Silicon and Intel package, install both Rust targets and run the universal recipe:

```sh
rustup target add aarch64-apple-darwin x86_64-apple-darwin
just package-universal
```

Verify existing artifacts without rebuilding them:

```sh
just verify-package
```

Packaging uses an ad-hoc signature, so it does not require an Apple Developer Program membership.
It works for local development and testing, but it cannot provide Developer ID trust or Apple
notarization. A downloaded copy may require the explicit **Open Anyway** action in **System
Settings → Privacy & Security** before its first launch.

## Validate

```sh
just validate
```
