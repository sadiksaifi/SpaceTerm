app_bundle := "dist/SpaceTerm.app"
disk_image := "dist/SpaceTerm.dmg"

# List the available project commands.
default:
    @just --list

# Check that the development and packaging tools are available.
doctor:
    @cargo --version
    @cargo clippy --version
    @rustfmt --version
    @zig version
    @just --version
    @shellcheck --version | head -n 1
    @xcrun --find codesign
    @xcrun --find hdiutil
    @xcrun --find iconutil
    @xcrun --find tic

# Download locked Rust dependencies.
fetch:
    cargo fetch --locked

# Run SpaceTerm from source.
run:
    cargo run --locked

# Compile all targets and features without running tests.
check:
    cargo check --all-targets --all-features --locked

# Format all Rust sources.
fmt:
    cargo fmt --all

# Check Rust formatting without changing files.
fmt-check:
    cargo fmt --all -- --check

# Run the complete test suite.
test:
    cargo test --all-targets --all-features --locked

# Run tests whose names contain the supplied filter.
test-one filter:
    cargo test --all-targets --all-features --locked "{{ filter }}"

# Run the conventional terminal capability and protocol conformance corpus.
conformance:
    cargo test --all-targets --all-features --locked "terminal::conformance"

# Run Clippy with warnings treated as errors.
clippy:
    cargo clippy --all-targets --all-features --locked -- -D warnings

# Validate the macOS packaging scripts.
scripts-check:
    bash -n scripts/package-macos.sh scripts/verify-macos-package.sh
    shellcheck -x scripts/package-macos.sh scripts/verify-macos-package.sh
    plutil -lint packaging/macos/Info.plist

# Check patches for whitespace errors.
diff-check:
    git diff --check

# Run every repository validation required before committing.
validate: fmt-check test clippy scripts-check diff-check

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
