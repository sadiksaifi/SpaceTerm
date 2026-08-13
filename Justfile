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
    bash -n scripts/acceptance-identity.sh scripts/test-acceptance-identity.sh \
        scripts/package-macos.sh scripts/verify-macos-package.sh \
        scripts/release-performance-workload.sh \
        scripts/sample-release-performance-rss.sh \
        scripts/record-release-performance-trace.sh \
        scripts/test-release-performance-tools.sh \
        scripts/acceptance/analyze-release-performance-case.sh \
        scripts/acceptance/failure-action-driver.sh \
        scripts/acceptance/assemble-release-performance-rss-v3.sh \
        scripts/acceptance/freeze-performance-pair.sh \
        scripts/acceptance/freeze-performance-run.sh \
        scripts/acceptance/freeze-performance-subject.sh \
        scripts/acceptance/performance-workload.sh \
        scripts/acceptance/performance-plan.sh \
        scripts/acceptance/test-release-performance-campaign.sh
    shellcheck -x scripts/acceptance-identity.sh scripts/test-acceptance-identity.sh \
        scripts/package-macos.sh scripts/verify-macos-package.sh \
        scripts/release-performance-workload.sh \
        scripts/sample-release-performance-rss.sh \
        scripts/record-release-performance-trace.sh \
        scripts/test-release-performance-tools.sh \
        scripts/acceptance/analyze-release-performance-case.sh \
        scripts/acceptance/failure-action-driver.sh \
        scripts/acceptance/assemble-release-performance-rss-v3.sh \
        scripts/acceptance/freeze-performance-pair.sh \
        scripts/acceptance/freeze-performance-run.sh \
        scripts/acceptance/freeze-performance-subject.sh \
        scripts/acceptance/performance-workload.sh \
        scripts/acceptance/performance-plan.sh \
        scripts/acceptance/test-release-performance-campaign.sh
    ./scripts/test-acceptance-identity.sh
    xcrun clang -fobjc-arc -fblocks -fsyntax-only -Wall -Wextra -Werror -Wpedantic \
        -mmacosx-version-min=11.0 \
        scripts/acceptance/performance-driver.m
    xcrun clang -fobjc-arc -fblocks -fsyntax-only -Wall -Wextra -Werror -Wpedantic \
        -mmacosx-version-min=11.0 \
        scripts/acceptance/performance-rss-sampler.m
    xcrun clang -std=c17 -fsyntax-only -Wall -Wextra -Werror -Wpedantic \
        -mmacosx-version-min=11.0 \
        scripts/acceptance/performance-workload.c
    python3 -c 'import pathlib; [compile(path.read_text(), path.name, "exec") for path in map(pathlib.Path, ["scripts/inspect-release-performance-process.py", "scripts/run-release-performance-command.py", "scripts/verify-release-performance-trace.py", "scripts/acceptance/verify-performance-workload-auth.py", "scripts/acceptance/verify-performance-workload-ready.py"])]'
    plutil -lint packaging/macos/Info.plist

# Run focused checks for the release-performance workload and evidence tools.
performance-tools-check:
    ./scripts/test-release-performance-tools.sh
    ./scripts/acceptance/test-release-performance-campaign.sh

# Check the local Instruments and RSS prerequisites used for release acceptance.
performance-doctor:
    @./scripts/sample-release-performance-rss.sh --doctor
    @./scripts/record-release-performance-trace.sh --doctor

# Check patches for whitespace errors.
diff-check:
    git diff --check

# Run every repository validation required before committing.
validate: fmt-check test clippy scripts-check performance-tools-check diff-check

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

# Create the immutable identity directory for one acceptance run.
acceptance-identity run_dir origin="app-bundle":
    ./scripts/acceptance-identity.sh collect --run-dir "{{ run_dir }}" --origin "{{ origin }}" --app "{{ app_bundle }}" --dmg "{{ disk_image }}"

# Launch the exact mounted DMG app and create a final-capable identity from its runtime proof.
acceptance-mounted-dmg-identity run_dir:
    ./scripts/acceptance-identity.sh collect --run-dir "{{ run_dir }}" --origin mounted-dmg --app "{{ app_bundle }}" --dmg "{{ disk_image }}"

# Launch an authenticated mounted-DMG campaign with a private fixed-command failure FIFO.
acceptance-mounted-dmg-failure-identity run_dir control_path:
    ./scripts/acceptance-identity.sh collect --run-dir "{{ run_dir }}" --origin mounted-dmg --app "{{ app_bundle }}" --dmg "{{ disk_image }}" --failure-control "{{ control_path }}"

# Verify an existing acceptance-run identity against its source and package artifacts.
verify-acceptance-identity run_dir:
    ./scripts/acceptance-identity.sh verify --run-dir "{{ run_dir }}"

# Verify that an acceptance identity is complete enough for final evidence.
verify-final-acceptance-identity run_dir:
    ./scripts/acceptance-identity.sh verify --run-dir "{{ run_dir }}" --final
