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
    cargo check --workspace --all-targets --all-features --locked

# Format all Rust sources.
fmt:
    cargo fmt --all

# Check Rust formatting without changing files.
fmt-check:
    cargo fmt --all -- --check

# Run the complete test suite.
test:
    cargo test --workspace --all-targets --all-features --locked

# Run tests whose names contain the supplied filter.
test-one filter:
    cargo test --workspace --all-targets --all-features --locked "{{ filter }}"

# Run the conventional terminal capability and protocol conformance corpus.
conformance:
    cargo test --all-targets --all-features --locked "terminal::conformance"

# Run Clippy with warnings treated as errors.
clippy:
    cargo clippy --workspace --all-targets --all-features --locked -- -D warnings

# Validate the macOS packaging scripts.
scripts-check:
    bash -n scripts/acceptance-identity.sh scripts/test-acceptance-identity.sh \
        scripts/package-macos.sh scripts/verify-macos-package.sh \
        scripts/release-performance-workload.sh \
        scripts/sample-release-performance-rss.sh \
        scripts/record-release-performance-trace.sh \
        scripts/test-release-performance-tools.sh \
        scripts/acceptance/analyze-release-render-profile-case.sh \
        scripts/acceptance/analyze-release-performance-case.sh \
        scripts/acceptance/analyze-release-performance-pair.sh \
        scripts/acceptance/failure-action-driver.sh \
        scripts/acceptance/assemble-release-performance-rss-v3.sh \
        scripts/acceptance/build-native-performance-tools.sh \
        scripts/acceptance/freeze-performance-pair.sh \
        scripts/acceptance/freeze-performance-run-intent.sh \
        scripts/acceptance/freeze-performance-run.sh \
        scripts/acceptance/freeze-performance-subject.sh \
        scripts/acceptance/finalize-render-profile-evidence.sh \
        scripts/acceptance/freeze-render-profile-intent.sh \
        scripts/acceptance/freeze-render-profile-tool-bundle.sh \
        scripts/acceptance/freeze-render-profile-workload.sh \
        scripts/acceptance/native-ax-probe.sh \
        scripts/acceptance/test-native-ax-probe.sh \
        scripts/acceptance/performance-workload.sh \
        scripts/acceptance/performance-plan.sh \
        scripts/acceptance/run-native-performance-scenario.sh \
        scripts/acceptance/test-performance-driver-receipt.sh \
        scripts/acceptance/test-performance-pair-result.sh \
        scripts/acceptance/test-performance-run-metadata-v3.sh \
        scripts/acceptance/test-performance-tail-receipt.sh \
        scripts/acceptance/test-native-performance-runner.sh \
        scripts/acceptance/test-render-evidence-validators.sh \
        scripts/acceptance/test-render-profile-evidence-protocol.sh \
        scripts/acceptance/test-render-profile-tool-bundle.sh \
        scripts/acceptance/test-release-render-profile-campaign.sh \
        scripts/acceptance/test-release-performance-campaign.sh \
        scripts/acceptance/test-issue-43-campaign-evidence.sh
    shellcheck -x scripts/acceptance-identity.sh scripts/test-acceptance-identity.sh \
        scripts/package-macos.sh scripts/verify-macos-package.sh \
        scripts/release-performance-workload.sh \
        scripts/sample-release-performance-rss.sh \
        scripts/record-release-performance-trace.sh \
        scripts/test-release-performance-tools.sh \
        scripts/acceptance/analyze-release-render-profile-case.sh \
        scripts/acceptance/analyze-release-performance-case.sh \
        scripts/acceptance/analyze-release-performance-pair.sh \
        scripts/acceptance/failure-action-driver.sh \
        scripts/acceptance/assemble-release-performance-rss-v3.sh \
        scripts/acceptance/build-native-performance-tools.sh \
        scripts/acceptance/freeze-performance-pair.sh \
        scripts/acceptance/freeze-performance-run-intent.sh \
        scripts/acceptance/freeze-performance-run.sh \
        scripts/acceptance/freeze-performance-subject.sh \
        scripts/acceptance/finalize-render-profile-evidence.sh \
        scripts/acceptance/freeze-render-profile-intent.sh \
        scripts/acceptance/freeze-render-profile-tool-bundle.sh \
        scripts/acceptance/freeze-render-profile-workload.sh \
        scripts/acceptance/native-ax-probe.sh \
        scripts/acceptance/test-native-ax-probe.sh \
        scripts/acceptance/performance-workload.sh \
        scripts/acceptance/performance-plan.sh \
        scripts/acceptance/run-native-performance-scenario.sh \
        scripts/acceptance/test-performance-driver-receipt.sh \
        scripts/acceptance/test-performance-pair-result.sh \
        scripts/acceptance/test-performance-run-metadata-v3.sh \
        scripts/acceptance/test-performance-tail-receipt.sh \
        scripts/acceptance/test-native-performance-runner.sh \
        scripts/acceptance/test-render-evidence-validators.sh \
        scripts/acceptance/test-render-profile-evidence-protocol.sh \
        scripts/acceptance/test-render-profile-tool-bundle.sh \
        scripts/acceptance/test-release-render-profile-campaign.sh \
        scripts/acceptance/test-release-performance-campaign.sh \
        scripts/acceptance/test-issue-43-campaign-evidence.sh
    ./scripts/test-acceptance-identity.sh
    ./scripts/acceptance/test-native-ax-probe.sh
    xcrun clang -fobjc-arc -fblocks -std=c17 -fsyntax-only \
        -Wall -Wextra -Werror -Wpedantic -mmacosx-version-min=11.0 \
        scripts/acceptance/native-ax-probe.m
    xcrun clang -fobjc-arc -fblocks -fsyntax-only -Wall -Wextra -Werror -Wpedantic \
        -mmacosx-version-min=11.0 \
        scripts/acceptance/performance-driver.m
    xcrun clang -fobjc-arc -fblocks -fsyntax-only -Wall -Wextra -Werror -Wpedantic \
        -mmacosx-version-min=11.0 \
        scripts/acceptance/performance-rss-sampler.m
    xcrun clang -fobjc-arc -fblocks -fsyntax-only -Wall -Wextra -Werror -Wpedantic \
        -mmacosx-version-min=11.0 \
        scripts/acceptance/performance-window-resolver.m
    xcrun clang -fobjc-arc -fblocks -fsyntax-only -Wall -Wextra -Werror -Wpedantic \
        -mmacosx-version-min=11.0 \
        scripts/acceptance/performance-appkit-terminate.m
    xcrun clang -std=c17 -fsyntax-only -Wall -Wextra -Werror -Wpedantic \
        -mmacosx-version-min=11.0 \
        scripts/acceptance/verify-mounted-filesystem.c
    xcrun clang -std=c17 -fsyntax-only -Wall -Wextra -Werror -Wpedantic \
        -mmacosx-version-min=11.0 \
        scripts/acceptance/performance-workload.c
    python3 -c 'import pathlib; [compile(path.read_text(), path.name, "exec") for path in map(pathlib.Path, ["scripts/acceptance/performance-driver-receipt.py", "scripts/acceptance/performance-pair-result.py", "scripts/acceptance/performance-subject-lifecycle.py", "scripts/acceptance/performance-tail-receipt.py", "scripts/acceptance/run-performance-process-group.py", "scripts/inspect-release-performance-process.py", "scripts/run-release-performance-command.py", "scripts/verify-release-performance-trace.py", "scripts/acceptance/verify-performance-lifecycle-receipts.py", "scripts/acceptance/verify-performance-native-closure.py", "scripts/acceptance/verify-performance-subject-exit.py", "scripts/acceptance/verify-performance-workload-auth.py", "scripts/acceptance/verify-performance-workload-ready.py", "scripts/acceptance/archive-render-trace.py", "scripts/acceptance/render-profile-hmac.py", "scripts/acceptance/render-trace-receipt.py", "scripts/acceptance/test-archive-render-trace.py", "scripts/acceptance/test-render-trace-receipt.py", "scripts/acceptance/verify-render-action-video.py", "scripts/acceptance/verify-render-trace-archive.py", "scripts/acceptance/issue-43-campaign-evidence.py"])]'
    ./scripts/acceptance/test-issue-43-campaign-evidence.sh
    plutil -lint packaging/macos/Info.plist

# Run focused checks for the release-performance workload and evidence tools.
performance-tools-check:
    ./scripts/test-release-performance-tools.sh
    ./scripts/acceptance/test-release-performance-campaign.sh
    ./scripts/acceptance/test-performance-driver-receipt.sh
    ./scripts/acceptance/test-performance-pair-result.sh
    ./scripts/acceptance/test-performance-subject-lifecycle.py
    ./scripts/acceptance/test-performance-run-metadata-v3.sh
    ./scripts/acceptance/test-performance-tail-receipt.sh
    ./scripts/acceptance/test-native-performance-runner.sh
    ./scripts/acceptance/test-render-evidence-validators.sh
    ./scripts/acceptance/test-render-profile-evidence-protocol.sh
    ./scripts/acceptance/test-render-profile-tool-bundle.sh
    ./scripts/acceptance/test-archive-render-trace.py
    ./scripts/acceptance/test-render-trace-receipt.py
    ./scripts/acceptance/test-release-render-profile-campaign.sh

# Compile the native performance tools into one absent run-owned directory.
performance-native-tools run_dir output_dir architecture:
    ./scripts/acceptance/build-native-performance-tools.sh \
        --run-directory "{{ run_dir }}" --output-directory "{{ output_dir }}" \
        --architecture "{{ architecture }}"

# Run one authenticated native performance scenario against a frozen subject.
performance-native-run *arguments:
    ./scripts/acceptance/run-native-performance-scenario.sh {{ arguments }}

# Finalize one authenticated pair from two complete production subject runs.
performance-pair-finalize *arguments:
    python3 scripts/acceptance/performance-pair-result.py create {{ arguments }}

# Emit the only release-performance PASS after both complete cases and pair replay.
performance-pair-analyze *arguments:
    ./scripts/acceptance/analyze-release-performance-pair.sh {{ arguments }}
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

# Bind the unique authenticated hidden collector root to an issue #43 campaign.
issue-43-campaign-init run_id:
    ./scripts/acceptance/issue-43-campaign-evidence.py init --run-id "{{ run_id }}"

# Freeze the privacy-normalized issue #43 campaign metadata document.
issue-43-campaign-set-metadata run_id input:
    ./scripts/acceptance/issue-43-campaign-evidence.py set-metadata --run-id "{{ run_id }}" --input "{{ input }}"

# Append one immutable subject-scoped issue #43 case record.
issue-43-campaign-record run_id input:
    ./scripts/acceptance/issue-43-campaign-evidence.py record --run-id "{{ run_id }}" --input "{{ input }}"

# Register one immutable, already-uploaded issue #43 payload artifact.
issue-43-campaign-add-artifact run_id input:
    ./scripts/acceptance/issue-43-campaign-evidence.py add-artifact --run-id "{{ run_id }}" --input "{{ input }}"

# Register one artifact in the authenticated privacy-review batch.
issue-43-campaign-review-artifact run_id artifact_id reviewer review_url reviewed_utc:
    ./scripts/acceptance/issue-43-campaign-evidence.py review-artifact --run-id "{{ run_id }}" --artifact-id "{{ artifact_id }}" --decision PASS --reviewer-role artifact-privacy-reviewer --reviewer "github:{{ reviewer }}" --review-url "{{ review_url }}" --reviewed-utc "{{ reviewed_utc }}" --attestation "I manually inspected the exact published bytes and found no prohibited content"

# Register one case in the authenticated observation-review batch.
issue-43-campaign-review-record run_id record_id reviewer review_url reviewed_utc:
    ./scripts/acceptance/issue-43-campaign-evidence.py review-record --run-id "{{ run_id }}" --record-id "{{ record_id }}" --decision PASS --reviewer-role case-observation-reviewer --reviewer "github:{{ reviewer }}" --review-url "{{ review_url }}" --reviewed-utc "{{ reviewed_utc }}" --attestation "I manually opened the exact reviewed artifact bytes, verified the canonical record and full artifact manifest, and checked every named issue 43 requirement clause and interaction"

# Render the exact GitHub comment to post before registering a complete review batch.
issue-43-campaign-review-batch-proposal run_id kind reviewer:
    ./scripts/acceptance/issue-43-campaign-evidence.py review-batch-proposal --run-id "{{ run_id }}" --kind "{{ kind }}" --reviewer "github:{{ reviewer }}"

# Capture publishable authenticated identity evidence after the collector's final rename.
issue-43-campaign-capture-identity run_id:
    ./scripts/acceptance/issue-43-campaign-evidence.py capture-identity-evidence --run-id "{{ run_id }}"

# Freeze the acyclic issue #43 payload manifest and control digests after collector rename.
issue-43-campaign-finalize run_id:
    ./scripts/acceptance/issue-43-campaign-evidence.py finalize --run-id "{{ run_id }}"

# Generate the final issue comment after uploading all three control files.
issue-43-campaign-comment run_id campaign_url artifacts_url control_url:
    ./scripts/acceptance/issue-43-campaign-evidence.py comment --run-id "{{ run_id }}" --campaign-url "{{ campaign_url }}" --artifacts-url "{{ artifacts_url }}" --control-url "{{ control_url }}"

# Replay against the expected detached digest copied from the posted GitHub issue comment.
verify-issue-43-campaign run_dir expected_control_sha256 issue_comment_url:
    ./scripts/acceptance/issue-43-campaign-evidence.py verify --run-dir "{{ run_dir }}" --require-comment --expected-control-sha256 "{{ expected_control_sha256 }}" --issue-comment-url "{{ issue_comment_url }}" --fetch-public
