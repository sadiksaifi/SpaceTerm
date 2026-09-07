app_bundle := "dist/SpaceTerm.app"
disk_image := "dist/SpaceTerm.dmg"
packager_version := "0.11.8"
minimum_xcode_major := "26"

# List project commands.
default:
    @just --list

# Verify development and packaging tools.
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

# Install the pinned macOS packager.
install-packager:
    cargo install cargo-packager --version "{{ packager_version }}" --locked

# Download locked Rust dependencies.
fetch:
    cargo fetch --locked

# Run SpaceTerm from source.
run:
    cargo run --locked

# Check every target and feature.
check:
    cargo check --workspace --all-targets --all-features --locked

# Check shared targets without native features.
portable-check:
    cargo check --workspace --all-targets --no-default-features --locked

# Format all Rust sources.
fmt:
    cargo fmt --all
    rustfmt --edition 2024 src/platform/macos_adapter_tests/*.rs

# Check workspace Rust formatting.
portable-fmt-check:
    cargo fmt --all -- --check

# Check isolated macOS Adapter formatting.
macos-fmt-check:
    rustfmt --edition 2024 --check src/platform/macos_adapter_tests/*.rs

# Check all Rust formatting.
fmt-check: portable-fmt-check macos-fmt-check

# Run all tests with every feature.
test:
    cargo test --workspace --all-targets --all-features --locked

# Run shared tests without native features.
portable-test:
    cargo test --workspace --all-targets --no-default-features --locked

# Run tests matching a filter.
test-one filter:
    cargo test --workspace --all-targets --all-features --locked "{{ filter }}"

# Run terminal protocol conformance tests.
conformance:
    cargo test --all-targets --no-default-features --locked "terminal::conformance"

# Run native macOS Adapter tests.
macos-adapter-tests:
    cargo test --all-targets --features macos-native-tests --locked "macos"

# Lint every target and feature.
clippy:
    cargo clippy --workspace --all-targets --all-features --locked -- -D warnings

# Lint shared targets without native features.
portable-clippy:
    cargo clippy --workspace --all-targets --no-default-features --locked -- -D warnings

# Lint with native features.
macos-clippy:
    cargo clippy --workspace --all-targets --all-features --locked -- -D warnings

# Check macOS scripts and package metadata.
scripts-check:
    bash -n scripts/package-macos.sh scripts/verify-macos-package.sh \
        scripts/kitty-graphics-smoke.sh
    shellcheck -x scripts/package-macos.sh scripts/verify-macos-package.sh \
        scripts/kitty-graphics-smoke.sh
    plutil -lint packaging/macos/Info.plist

# Check the diff for whitespace errors.
diff-check:
    git diff --check

# Run checks without native tooling.
portable-validate: portable-fmt-check portable-check portable-test portable-clippy diff-check

# Run macOS-specific checks.
macos-validate: macos-fmt-check macos-adapter-tests macos-clippy scripts-check

# Run the full pre-push or handoff gate.
validate: portable-validate macos-validate

# Build the optimized executable.
release:
    cargo build --release --locked

# Build and verify the macOS app and disk image.
package build_number="1":
    ./scripts/package-macos.sh --build-number "{{ build_number }}"

# Build, verify, and install the macOS app.
install-macos: package
    rm -rf -- "/Applications/SpaceTerm.app"
    /usr/bin/ditto "{{ app_bundle }}" "/Applications/SpaceTerm.app"

# Build universal Apple Silicon and Intel artifacts.
package-universal build_number="1":
    ./scripts/package-macos.sh --universal --build-number "{{ build_number }}"

# Verify existing macOS artifacts.
verify-package:
    ./scripts/verify-macos-package.sh

# Open the packaged application.
open-app:
    open -n "{{ app_bundle }}"

# Open the installer disk image.
open-dmg:
    open "{{ disk_image }}"

# Show package metadata, architectures, signature, and sizes.
package-info:
    @plutil -p "{{ app_bundle }}/Contents/Info.plist"
    @lipo -archs "{{ app_bundle }}/Contents/MacOS/SpaceTerm"
    @codesign --display --verbose=2 "{{ app_bundle }}"
    @ls -lh "{{ disk_image }}" "{{ app_bundle }}/Contents/MacOS/SpaceTerm"
