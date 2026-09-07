app_bundle := "dist/SpaceTerm.app"
disk_image := "dist/SpaceTerm.dmg"
packager_version := "0.11.8"
minimum_xcode_major := "26"

# List the available project commands.
default:
    @just --list

# Check that the development and packaging tools are available.
doctor:
    @cargo --version
    @cargo clippy --version
    @rustfmt --version
    @version="$(cargo packager --version 2>/dev/null || true)"; test "$version" = "cargo-packager {{ packager_version }}" || { echo "cargo-packager {{ packager_version }} is required; run: just install-packager" >&2; exit 1; }
    @zig version
    @just --version
    @shellcheck --version | head -n 1
    @version="$(xcodebuild -version | awk 'NR == 1 { print $2 }')"; major="${version%%.*}"; test "$major" -ge "{{ minimum_xcode_major }}" || { echo "Xcode {{ minimum_xcode_major }} or newer is required, got: $version" >&2; exit 1; }
    @xcrun --find codesign
    @xcrun --find actool
    @xcrun --find assetutil
    @xcrun --find hdiutil
    @xcrun --find iconutil
    @xcrun --find tic

# Install the pinned macOS application and DMG packager.
install-packager:
    cargo install cargo-packager --version "{{ packager_version }}" --locked

# Download locked Rust dependencies.
fetch:
    cargo fetch --locked

# Run SpaceTerm from source.
run:
    cargo run --locked

# Compile all targets and features without running tests.
check:
    cargo check --workspace --all-targets --all-features --locked

# Compile shared targets without enabling native Adapter test suites.
portable-check:
    cargo check --workspace --all-targets --no-default-features --locked

# Format all Rust sources.
fmt:
    cargo fmt --all
    rustfmt --edition 2024 src/platform/macos_adapter_tests/*.rs

# Check Rust formatting without changing files.
portable-fmt-check:
    cargo fmt --all -- --check

# Check formatting for isolated macOS Adapter suite sources mounted with include/path attributes.
macos-fmt-check:
    rustfmt --edition 2024 --check src/platform/macos_adapter_tests/*.rs

# Check formatting for every Rust source set.
fmt-check: portable-fmt-check macos-fmt-check

# Run the complete test suite.
test:
    cargo test --workspace --all-targets --all-features --locked

# Run shared tests, the Conformance Corpus, and structural architecture tests only.
portable-test:
    cargo test --workspace --all-targets --no-default-features --locked

# Run tests whose names contain the supplied filter.
test-one filter:
    cargo test --workspace --all-targets --all-features --locked "{{ filter }}"

# Run the conventional terminal capability and protocol conformance corpus.
conformance:
    cargo test --all-targets --no-default-features --locked "terminal::conformance"

# Run isolated native Adapter suites and existing macOS capability integration tests.
macos-adapter-tests:
    cargo test --all-targets --features macos-native-tests --locked "macos"

# Run Clippy with warnings treated as errors.
clippy:
    cargo clippy --workspace --all-targets --all-features --locked -- -D warnings

# Run Clippy without enabling native Adapter test suites.
portable-clippy:
    cargo clippy --workspace --all-targets --no-default-features --locked -- -D warnings

# Run Clippy across the native Adapter source and test suites.
macos-clippy:
    cargo clippy --workspace --all-targets --all-features --locked -- -D warnings

# Validate the retained macOS scripts and package metadata.
scripts-check:
    bash -n scripts/package-macos.sh scripts/verify-macos-package.sh \
        scripts/kitty-graphics-smoke.sh
    shellcheck -x scripts/package-macos.sh scripts/verify-macos-package.sh \
        scripts/kitty-graphics-smoke.sh
    plutil -lint packaging/macos/Info.plist

# Check patches for whitespace errors.
diff-check:
    git diff --check

# Validate portable ownership without invoking native test or tooling prerequisites.
portable-validate: portable-fmt-check portable-check portable-test portable-clippy diff-check

# Validate macOS Adapter suites, native linting, retained scripts, and packaging contracts.
macos-validate: macos-fmt-check macos-adapter-tests macos-clippy scripts-check

# Run every portable and macOS validation required before committing.
validate: portable-validate macos-validate

# Build the optimized native executable.
release:
    cargo build --release --locked

# Build and verify native SpaceTerm.app and SpaceTerm.dmg artifacts.
package build_number="1":
    ./scripts/package-macos.sh --build-number "{{ build_number }}"

# Build and verify universal Apple Silicon and Intel artifacts.
package-universal build_number="1":
    ./scripts/package-macos.sh --universal --build-number "{{ build_number }}"

# Verify existing app and DMG artifacts without rebuilding them.
verify-package:
    ./scripts/verify-macos-package.sh

# Launch the packaged application as a new process.
open-app:
    open -n "{{ app_bundle }}"

# Open the installer disk image in Finder.
open-dmg:
    open "{{ disk_image }}"

# Show metadata, architecture, signature, and artifact sizes.
package-info:
    @plutil -p "{{ app_bundle }}/Contents/Info.plist"
    @lipo -archs "{{ app_bundle }}/Contents/MacOS/SpaceTerm"
    @codesign --display --verbose=2 "{{ app_bundle }}"
    @ls -lh "{{ disk_image }}" "{{ app_bundle }}/Contents/MacOS/SpaceTerm"
