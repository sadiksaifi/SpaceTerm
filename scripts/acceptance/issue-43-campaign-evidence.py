#!/usr/bin/env python3
"""Record, freeze, and replay issue #43 native acceptance evidence.

The mounted-DMG identity collector deliberately works in a hidden staging
directory until SpaceTerm exits.  This tool resolves that private root from
HOME, keeps mutable operator state private, and emits only privacy-reviewed,
hash-bound public evidence.
"""

from __future__ import annotations

import argparse
import csv
import datetime as dt
import fcntl
import hashlib
import json
import math
import os
import re
import shutil
import socket
import struct
import subprocess
import stat
import sys
import tempfile
import urllib.error
import urllib.parse
import urllib.request
import unicodedata
import zipfile
import xml.etree.ElementTree as ET
import zlib
import ipaddress
from contextlib import contextmanager
from pathlib import Path
from typing import Any, Iterable


RUN_RE = re.compile(r"^i43-(\d{8}T\d{6}Z)-([0-9a-f]{12})-([0-9a-f]{12})$")
RECORD_RE = re.compile(
    r"^(i43-\d{8}T\d{6}Z-[0-9a-f]{12}-[0-9a-f]{12})-"
    r"([a-z0-9]+(?:-[a-z0-9]+)*)-(spaceterm|ghostty)-a(\d{2})$"
)
HASH_RE = re.compile(r"^[0-9a-f]{64}$")
UTC_RE = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$")
CASE_RE = re.compile(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")
ARTIFACT_FILE_RE = re.compile(
    r"^([a-z0-9]+(?:-[a-z0-9]+)*)--(spaceterm|ghostty)--(\d{2})--"
    r"([a-z0-9]+(?:-[a-z0-9]+)*)\.([a-z0-9]+(?:\.[a-z0-9]+)*)$"
)

MATRIX_CASES: dict[str, tuple[str, ...]] = {
    "native": (
        "native-bash", "native-zsh", "native-vim", "native-neovim",
        "native-tmux", "native-less", "native-fzf", "native-btop",
        "native-yazi-no-previews", "native-claude-code",
        "native-pi-coding-agent",
    ),
    "focus": (
        "focus-pane-switch", "focus-sidebar", "focus-workspace-rename",
        "focus-workspace-context-menu", "focus-pane-menu", "focus-window-menu",
        "focus-top-chrome", "focus-window-selector", "focus-terminal-find",
        "focus-native-panels", "focus-non-key-os-window",
        "focus-app-activation", "focus-hierarchy-switch",
    ),
    "capability": (
        "capability-keyboard", "capability-mouse", "capability-paste",
        "capability-focus-bytes", "capability-styles", "capability-links",
        "capability-resize-scrollback", "capability-accessibility",
        "capability-attention", "capability-macos-services",
        "capability-context-actions", "capability-quick-look",
        "capability-local-diagnostics",
    ),
    "failure": (
        "failure-presentation-recoverable",
        "failure-renderer-resource-recoverable",
        "failure-platform-action-recoverable", "failure-pty-fatal",
        "failure-emulator-session-fatal", "failure-normal-exit",
        "failure-diagnostics-bounded",
    ),
    "performance": (
        "perf-sustained-ascii", "perf-sustained-unicode-styles",
        "perf-sustained-scrolled", "perf-sustained-hidden", "perf-resize",
        "perf-render-idle-cursor-blink", "perf-render-text-blink",
        "perf-render-sustained-output", "perf-render-selection",
        "perf-render-marked-text", "perf-render-live-resize",
    ),
    "package": (
        "package-doctor", "package-build", "package-launch-dmg",
        "package-window-shell", "package-command-output", "package-resize",
        "package-process-reap", "package-identity", "package-final-validate",
    ),
}
CASE_MATRIX = {case: matrix for matrix, cases in MATRIX_CASES.items() for case in cases}
PERFORMANCE_CASES = set(MATRIX_CASES["performance"])
RENDER_CASES = {case for case in PERFORMANCE_CASES if case.startswith("perf-render-")}
NATIVE_CASES = set(MATRIX_CASES["native"])
REQUIRED_SPACETERM = set(CASE_MATRIX)
SUPPLEMENTARY_CASES = {"perf-render-kitty-static"}
ALL_CASES = REQUIRED_SPACETERM | SUPPLEMENTARY_CASES
CONTROL_FILES = {"campaign.yaml", "artifacts.tsv", "control.sha256"}
PAYLOAD_DIRS = set(MATRIX_CASES) | {"identity", "supplementary"}
PROGRAMS = {
    "Bash", "Zsh", "Vim", "Neovim", "tmux", "less", "fzf", "btop",
    "Yazi", "Claude Code", "pi-coding-agent",
}

# Clause IDs are the executable inventory of each unconditional runbook row.
# Finalization requires this exact set, preventing one generic checkbox from
# standing in for a multi-part native observation.
CASE_CLAUSES: dict[str, tuple[str, ...]] = {
    "native-bash": ("interactive-shell", "ordinary-unicode-edit", "control-option-input", "styled-hyperlink-output", "single-multiline-paste", "scrollback", "resize", "interrupt", "clean-exit"),
    "native-zsh": ("bash-equivalent", "shell-integration-directory", "shell-integration-prompt-command", "shell-integration-completion", "shell-integration-title", "temporary-config-only"),
    "native-vim": ("real-file", "alternate-screen", "unicode-insert", "navigation-function-control-mouse", "repeated-resize", "copy-paste", "primary-screen-restore"),
    "native-neovim": ("vim-equivalent", "mouse-tracking", "bracketed-paste", "cursor-shape", "alternate-screen-clean-exit"),
    "native-tmux": ("client-create", "split-panes", "focus-move", "prefix", "resize", "mouse", "detach-exit", "outer-pane-usable"),
    "native-less": ("page-search", "link-text", "resize", "top-bottom", "quit", "primary-scrollback-restore"),
    "native-fzf": ("unicode-filter", "navigate-select", "cancel", "resize", "cursor-application-key"),
    "native-btop": ("keyboard-mouse", "styles-drawing", "live-updates", "resize", "suspend-quit", "responsive-during-updates"),
    "native-yazi-no-previews": ("preview-disabled-config", "navigate-select-scroll", "resize", "open-return", "quit"),
    "native-claude-code": ("text-ui-start", "prompt-submit-edit", "scroll", "resize", "interrupt-generation", "clean-exit"),
    "native-pi-coding-agent": ("text-ui-start", "prompt-submit-edit", "scroll", "resize", "interrupt-generation", "clean-exit"),
    "focus-pane-switch": ("route-pane-switch",),
    "focus-sidebar": ("route-sidebar-return",),
    "focus-workspace-rename": ("route-rename-finish-cancel",),
    "focus-workspace-context-menu": ("route-workspace-menu",),
    "focus-pane-menu": ("route-pane-menu",),
    "focus-window-menu": ("route-window-menu-context",),
    "focus-top-chrome": ("route-top-chrome-drag",),
    "focus-window-selector": ("route-window-selector-switch",),
    "focus-terminal-find": ("route-find-responder-open-close",),
    "focus-native-panels": ("route-all-native-panels",),
    "focus-non-key-os-window": ("route-non-key-while-active",),
    "focus-app-activation": ("route-deactivate-reactivate",),
    "focus-hierarchy-switch": ("route-active-workspace-window",),
    "capability-keyboard": ("ordinary-text", "navigation-function", "control-option-command", "repeat-release", "non-us-layout", "dead-key", "native-ime-compose-commit"),
    "capability-mouse": ("local-selection", "application-tracking", "captured-drag-release", "shift-override", "precision-wheel", "momentum", "alternate-screen-scroll"),
    "capability-paste": ("ordinary", "bracketed-multiline", "unsafe-approve-cancel", "embedded-closing-fence", "file-url", "file-drag-drop"),
    "capability-focus-bytes": ("dec1004-enable-current", "exact-transitions", "duplicate-suppression", "held-key-cleanup"),
    "capability-styles": ("semantic-colors", "reverse-bold-faint-italic", "blink-invisible", "underline-variants-colors", "strike-overline", "drawing-wide-combining-emoji", "fallback-fonts"),
    "capability-links": ("osc8-detected-activate", "validated-local-path", "stale-generation-reject", "malformed-missing-inert"),
    "capability-resize-scrollback": ("cell-pixel-resize", "rapid-live-resize", "reflow", "viewport-anchor-output", "selection-anchor", "primary-alternate-restore"),
    "capability-accessibility": ("inspector-real-pane", "voiceover-real-pane", "editable-role-value", "utf16-ranges", "visible-selection-cursor", "wide-combining-emoji", "soft-wraps-scrollback", "range-bounds-hit-test", "pane-notifications"),
    "capability-attention": ("bel-audio-visual", "pane-window-unread", "dock-rate-cancel", "inactive-notification", "aggregation", "no-focus-steal"),
    "capability-macos-services": ("selection-export", "text-insertion", "paste-sanitizer"),
    "capability-context-actions": ("selection-link-enablement", "stale-inert"),
    "capability-quick-look": ("validated-regular-local", "web-missing-remote-stale-unavailable"),
    "capability-local-diagnostics": ("bounded-typed", "content-free", "explicit-save-export", "no-network-upload"),
    "failure-presentation-recoverable": ("production-seam-trigger", "retain-last-presentation-generation", "retry-success"),
    "failure-renderer-resource-recoverable": ("production-seam-trigger", "retain-last-presentation-generation", "retry-success"),
    "failure-platform-action-recoverable": ("production-seam-trigger", "terminal-session-usable", "transient-replaced-after-retry"),
    "failure-pty-fatal": ("fatal-pty-trigger", "close-required", "close-responsive", "no-owned-process"),
    "failure-emulator-session-fatal": ("fatal-session-trigger", "close-recreate-required", "replacement-command-runs", "no-owned-process"),
    "failure-normal-exit": ("normal-exit-trigger", "distinct-from-failure-classes"),
    "failure-diagnostics-bounded": ("bounded-bytes", "content-audit-pass"),
    "perf-sustained-ascii": ("warmup-60s", "duration-10m", "periodic-input", "bytes-rss-10s", "bounded-ui-backlog", "memory-plateau", "input-responsive", "stall-max-250ms", "final-presentation", "shell-exit", "time-profiler", "allocations"),
    "perf-sustained-unicode-styles": ("warmup-60s", "duration-10m", "unicode-wide-styles-links-drawing", "bytes-rss-10s", "bounded-ui-backlog", "memory-plateau", "input-responsive", "stall-max-250ms", "final-presentation", "shell-exit", "time-profiler", "allocations"),
    "perf-sustained-scrolled": ("warmup-60s", "duration-10m", "viewport-scrolled-away", "bytes-rss-10s", "bounded-ui-backlog", "memory-plateau", "input-responsive", "stall-max-250ms", "final-presentation", "shell-exit", "time-profiler", "allocations"),
    "perf-sustained-hidden": ("warmup-60s", "duration-10m", "hidden-occluded-restore", "newest-generation-no-replay", "bytes-rss-10s", "bounded-ui-backlog", "memory-plateau", "input-responsive", "stall-max-250ms", "final-presentation", "shell-exit", "time-profiler", "allocations"),
    "perf-resize": ("scrollback-10000-mixed-lines", "duration-5m", "resize-rows-columns-both", "output-continues", "reflow-time", "input-responsive", "rss-10s", "pty-cell-pixel", "final-grid", "selection-viewport-anchor", "bounded-resize-backlog", "no-content-corruption", "memory-not-resize-correlated", "time-profiler", "allocations"),
    "perf-render-idle-cursor-blink": ("time-profiler", "no-paint-shaping", "no-paint-path-plan", "no-paint-normal-allocation", "no-unchanged-row-reshape", "changed-row-proportional"),
    "perf-render-text-blink": ("time-profiler", "no-paint-shaping", "no-paint-path-plan", "no-paint-normal-allocation", "no-unchanged-row-reshape", "changed-row-proportional"),
    "perf-render-sustained-output": ("time-profiler", "no-paint-shaping", "no-paint-path-plan", "no-paint-normal-allocation", "no-unchanged-row-reshape", "changed-row-proportional"),
    "perf-render-selection": ("time-profiler", "no-paint-shaping", "no-paint-path-plan", "no-paint-normal-allocation", "no-unchanged-row-reshape", "changed-row-proportional"),
    "perf-render-marked-text": ("time-profiler", "no-paint-shaping", "no-paint-path-plan", "no-paint-normal-allocation", "no-unchanged-row-reshape", "changed-row-proportional"),
    "perf-render-live-resize": ("time-profiler", "no-paint-shaping", "no-paint-path-plan", "no-paint-normal-allocation", "no-unchanged-row-reshape", "changed-row-proportional"),
    "package-doctor": ("tool-availability-known-or-doctor-pass",),
    "package-build": ("just-package", "complete-verification-log"),
    "package-launch-dmg": ("verified-dmg-mounted-readonly", "new-mounted-app-process"),
    "package-window-shell": ("os-window-visible", "interactive-shell-ready"),
    "package-command-output": ("deterministic-command", "exact-visible-output"),
    "package-resize": ("resize", "pty-cell-pixel-update", "presentation-intact"),
    "package-process-reap": ("close-pane-app", "shell-terminated-reaped"),
    "package-identity": ("app-dmg-hashes", "versions-architecture-signature", "launch-command", "screenshot-logs"),
    "package-final-validate": ("just-validate-frozen-sha", "complete-log-pass"),
}

# Ghostty proves that the comparison run was frozen and executed with the same
# inputs.  SpaceTerm-specific correctness and implementation conclusions (for
# example TerminalGridElement::paint stacks) are deliberately absent: Ghostty
# is a reference, not an acceptance authority.
GHOSTTY_PERFORMANCE_CLAUSES: dict[str, tuple[str, ...]] = {
    case_id: (
        "frozen-reference-identity",
        "same-comparison-inputs",
        "workload-completed",
        "measurements-recorded",
        "time-profiler",
        "allocations",
        "reference-observation-recorded",
    )
    for case_id in PERFORMANCE_CASES - RENDER_CASES
}
GHOSTTY_PERFORMANCE_CLAUSES.update({
    case_id: (
        "frozen-reference-identity",
        "same-comparison-inputs",
        "trace-duration-settings",
        "time-profiler",
        "allocations",
        "representative-stack-screenshots",
        "reference-observation-recorded",
    )
    for case_id in RENDER_CASES
})
ARTIFACT_HEADER = (
    "artifact_id", "record_id", "subject", "case_id", "relative_path",
    "sha256", "bytes", "media_type", "created_utc", "run_id", "producer",
    "producer_version", "privacy_review", "redaction_notes", "public_url", "content_class",
)
STATUSES = {"PASS", "FAIL", "NOT-RUN", "SKIPPED-UNAVAILABLE", "NOT-APPLICABLE"}
MATRICES = set(MATRIX_CASES) | {"supplementary"}
ARTIFACT_REVIEW_ATTESTATION = (
    "I manually inspected the exact published bytes and found no prohibited content"
)
RECORD_REVIEW_ATTESTATION = (
    "I manually opened the exact reviewed artifact bytes, verified the canonical record and "
    "full artifact manifest, and checked every named issue 43 requirement clause and interaction"
)
REVIEWER_RE = re.compile(r"^github:[a-z0-9](?:[a-z0-9-]{0,37}[a-z0-9])?$")
ARTIFACT_REVIEWER_ROLE = "artifact-privacy-reviewer"
RECORD_REVIEWER_ROLE = "case-observation-reviewer"

NATIVE_PROVISIONAL_V5_KEYS = (
    "schema", "observation.source", "launch.nonce", "run.id", "package.app.sha256",
    "runtime.schema", "runtime.sample_interval_ms", "runtime.transition_capacity",
    "failure.action.schema", "failure.action.enabled", "process.pid", "process.pidversion",
    "process.executable.path", "process.executable.device", "process.executable.inode",
    "process.executable.fsid", "process.signature.cdhash", "process.signature.identifier",
    "process.signature.team_identifier", "terminal_font_selected", "initial_grid.rows",
    "initial_grid.columns", "initial_grid.logical_width", "initial_grid.logical_height",
    "initial_grid.backing_pixel_width", "initial_grid.backing_pixel_height",
    "observation.complete",
)
NATIVE_FINAL_V5_KEYS = NATIVE_PROVISIONAL_V5_KEYS[:-1] + (
    "provisional.observation.sha256", "runtime.metadata.schema", "runtime.metadata.path",
    "runtime.metadata.sha256", "failure.result.schema", "failure.actions.path",
    "failure.actions.sha256", "failure.request_count", "failure.result_count",
    "observation.complete",
)
RUNTIME_METADATA_V3_KEYS = (
    "schema", "observation.source", "run.id", "package.app.sha256", "process.pid",
    "runtime.samples.path", "runtime.samples.sha256", "runtime.events.path",
    "runtime.events.sha256", "failure.action.schema", "failure.action.enabled",
    "failure.result.schema", "failure.actions.path", "failure.actions.sha256",
    "failure.request_count", "failure.result_count", "observer.started_continuous_ns",
    "observer.ended_continuous_ns", "observer.sample_interval_ms",
    "observer.transition_capacity", "observer.sample_count", "observer.event_count",
    "observer.status", "observation.complete",
)
FAILURE_ACTION_V2_HEADER = (
    "request_id", "sequence", "case_id", "action", "result", "pane_id", "pane_state",
    "failure_class", "failure_recoverability", "failure_operation", "state_revision",
    "latest_generation", "last_valid_generation", "visible_generation", "pending_recovery",
    "terminal_input_usable", "session_attached", "resource_staged_count",
    "resource_staged_bytes", "resource_rolled_back_count", "resource_rolled_back_bytes",
)
RUNTIME_SAMPLES_V1_HEADER = (
    "sequence", "continuous_ns", "worker_generation", "screens_published",
    "screens_enqueued", "screens_superseded", "event_queue_length",
    "event_queue_high_water", "ui_dispatches", "ui_screen_events",
    "ui_drain_high_water", "ui_latest_generation", "render_latest_generation",
    "next_frame_generation", "next_frame_count", "presentable", "minimized",
    "occluded", "workspace_visible", "pane_visible", "live_resize",
    "viewport_total_rows", "viewport_visible_rows", "viewport_offset_rows",
    "selection_present", "resize_requests", "resize_notifications", "resize_applied",
    "resize_coalesced", "pty_rows", "pty_columns", "pty_pixel_width",
    "pty_pixel_height", "terminal_inputs_accepted", "lifecycle", "observer_drops",
)
RUNTIME_EVENTS_V1_HEADER = (
    "sequence", "continuous_ns", "kind", "generation", "aux0", "aux1",
)
RUN_INTENT_V1_KEYS = (
    "format_version", "subject", "subject_identity_sha256", "scenario",
    "scenario_plan_sha256", "workload_sha256", "command_sha256", "environment_sha256",
    "font_sha256", "initial_grid_sha256", "measured_duration_ms", "process_pid",
    "process_start_identity", "campaign_id", "session_id", "nonce",
    "native_provisional_observation_sha256", "evidence_mode", "status",
)
PAIR_METADATA_V1_KEYS = (
    "format_version", "pair_id", "scenario", "plan_sha256", "workload_sha256",
    "command_sha256", "environment_sha256", "font_sha256", "initial_grid_sha256",
    "duration_ms", "spaceterm_subject_identity_sha256", "ghostty_subject_identity_sha256",
)
RUN_METADATA_V4_KEYS = (
    "format_version", "subject", "subject_identity_sha256", "scenario",
    "scenario_plan_sha256", "workload_sha256", "command_sha256", "environment_sha256",
    "font_sha256", "initial_grid_sha256", "measured_duration_ms", "process_pid",
    "process_start_identity", "run_intent_sha256", "native_observation_sha256",
    "native_runtime_metadata_sha256", "native_failure_actions_sha256",
    "native_failure_action_enabled", "native_failure_request_count",
    "native_failure_result_count", "native_failure_resource_staged_count",
    "native_failure_resource_staged_bytes", "native_failure_resource_rolled_back_count",
    "native_failure_resource_rolled_back_bytes", "trace_provisional_receipt_sha256",
    "performance_tail_receipt_sha256", "performance_quit_receipt_sha256",
    "subject_exit_receipt_sha256", "lifecycle_ready_receipt_sha256",
    "lifecycle_registration_receipt_sha256", "lifecycle_helper_sha256",
    "terminator_source_sha256", "terminator_binary_sha256", "evidence_mode", "status",
)
CASE_REPORT_V2_KEYS = (
    "format_version", "subject", "scenario", "session_id", "nonce",
    "run_intent_sha256", "run_metadata_sha256", "trace_metadata_sha256",
    "trace_archive_sha256", "manual_artifacts_sha256", "manual_screenshot_sha256",
    "manual_video_sha256", "result", "reason",
)
PAIR_RESULT_V3_KEYS = tuple(
    "format_version campaign_id pair_metadata_sha256 scenario_plan_sha256 workload_sha256 "
    "command_sha256 environment_sha256 font_sha256 initial_grid_sha256 "
    "spaceterm_session_id spaceterm_nonce spaceterm_run_intent_sha256 "
    "spaceterm_run_metadata_sha256 spaceterm_driver_intent_sha256 "
    "spaceterm_driver_events_sha256 spaceterm_driver_receipt_sha256 "
    "spaceterm_window_identity_sha256 spaceterm_driver_binary_sha256 "
    "spaceterm_driver_source_sha256 spaceterm_driver_controller_sha256 "
    "spaceterm_plan_start_gate_sha256 spaceterm_tail_receipt_sha256 "
    "spaceterm_quit_receipt_sha256 spaceterm_exit_receipt_sha256 "
    "spaceterm_case_report_sha256 spaceterm_trace_metadata_sha256 "
    "spaceterm_trace_archive_sha256 spaceterm_manual_artifacts_sha256 "
    "spaceterm_manual_screenshot_sha256 spaceterm_manual_video_sha256 "
    "ghostty_session_id ghostty_nonce ghostty_run_intent_sha256 "
    "ghostty_run_metadata_sha256 ghostty_driver_intent_sha256 "
    "ghostty_driver_events_sha256 ghostty_driver_receipt_sha256 "
    "ghostty_window_identity_sha256 ghostty_driver_binary_sha256 "
    "ghostty_driver_source_sha256 ghostty_driver_controller_sha256 "
    "ghostty_plan_start_gate_sha256 ghostty_tail_receipt_sha256 "
    "ghostty_quit_receipt_sha256 ghostty_exit_receipt_sha256 "
    "ghostty_case_report_sha256 ghostty_trace_metadata_sha256 "
    "ghostty_trace_archive_sha256 ghostty_manual_artifacts_sha256 "
    "ghostty_manual_screenshot_sha256 ghostty_manual_video_sha256 "
    "spaceterm_lifecycle_ready_receipt_sha256 "
    "spaceterm_lifecycle_registration_receipt_sha256 "
    "ghostty_lifecycle_ready_receipt_sha256 "
    "ghostty_lifecycle_registration_receipt_sha256 lifecycle_helper_sha256 "
    "terminator_source_sha256 terminator_binary_sha256 evidence_mode status "
    "auth_algorithm pair_result_hmac_sha256".split()
)
LIFECYCLE_READY_V1_KEYS = tuple(
    "schema subject campaign_id session_id nonce subject_identity_sha256 process_pid "
    "process_start_identity executable_sha256 ready_continuous_ns registration_control_device "
    "registration_control_inode lifecycle_helper_device lifecycle_helper_inode "
    "lifecycle_helper_sha256 process_inspector_device process_inspector_inode "
    "process_inspector_sha256 appkit_terminator_process_pid "
    "appkit_terminator_process_start_identity appkit_terminator_source_device "
    "appkit_terminator_source_inode appkit_terminator_source_sha256 "
    "appkit_terminator_binary_device appkit_terminator_binary_inode "
    "appkit_terminator_binary_sha256 evidence_mode auth_algorithm receipt_hmac_sha256 status".split()
)
LIFECYCLE_REGISTRATION_V1_KEYS = tuple(
    "format_version campaign_id session_id nonce registration_token subject_identity_sha256 "
    "process_pid process_start_identity run_intent_path run_intent_sha256 tail_receipt_path "
    "workload_metadata_path workload_events_path workload_ready_receipt_path quit_receipt_path "
    "subject_exit_receipt_path native_observation_path lifecycle_helper_device "
    "lifecycle_helper_inode lifecycle_helper_sha256 process_inspector_device "
    "process_inspector_inode process_inspector_sha256 appkit_terminator_process_pid "
    "appkit_terminator_process_start_identity appkit_terminator_source_device "
    "appkit_terminator_source_inode appkit_terminator_source_sha256 "
    "appkit_terminator_binary_device appkit_terminator_binary_inode "
    "appkit_terminator_binary_sha256 evidence_mode auth_algorithm registration_hmac_sha256 status".split()
)
TRACE_METADATA_V3_KEYS = (
    "format_version", "capture_status", "incomplete_reason", "subject_identity_sha256",
    "run_metadata_sha256", "workload_metadata_sha256", "workload_ready_receipt_sha256",
    "supplemental_evidence_sha256", "requested_duration_ms", "actual_duration_ms",
    "capture_started_continuous_ns", "capture_ended_continuous_ns",
    "target_identity_verified", "trace_target_pid_verified", "time_profiler_instrument",
    "allocations_instrument", "hangs_instrument", "time_profiler_target_verified",
    "allocations_target_verified", "hangs_target_verified", "time_profiler_rows",
    "allocations_rows", "hangs_rows", "maximum_main_thread_hang_ms", "status",
)
MANUAL_ARTIFACTS_V1_KEYS = (
    "format_version", "screenshot_sha256", "video_sha256", "final_content_review",
    "anchor_review", "restoration_review", "geometry_review", "reviewer", "result",
)
UINT64_MAX = (1 << 64) - 1


def artifact_review_projection(artifact: dict[str, Any]) -> dict[str, str]:
    projection = {key: str(artifact[key]) for key in ARTIFACT_HEADER if key != "privacy_review"}
    projection["privacy_review"] = "PASS"
    return projection


def artifact_inventory_digest(record: dict[str, Any], artifacts: dict[str, dict[str, Any]]) -> str:
    inventory = [artifact_review_projection(artifacts[artifact_id])
                 for artifact_id in sorted(record["artifacts"]) if artifact_id in artifacts]
    if len(inventory) != len(record["artifacts"]):
        fail("record artifact inventory is incomplete while calculating its review digest")
    return hashlib.sha256(canonical_json(inventory)).hexdigest()

# These are exact conditional subcases, not a vocabulary for arbitrary skips.
ALLOWED_CONDITIONALS = {
    ("capability-keyboard", "numpad input where available", "SKIPPED-UNAVAILABLE"),
    (
        "focus-non-key-os-window",
        "non-key while SpaceTerm remains active where possible",
        "SKIPPED-UNAVAILABLE",
    ),
    (
        "capability-resize-scrollback",
        "backing-scale/display movement when a second display is available",
        "SKIPPED-UNAVAILABLE",
    ),
    (
        "perf-resize",
        "backing-scale/display movement when a second display is available",
        "SKIPPED-UNAVAILABLE",
    ),
    ("native-claude-code", "detected/OSC 8 link if presented", "NOT-APPLICABLE"),
    (
        "native-pi-coding-agent",
        "detected/OSC 8 link if presented",
        "NOT-APPLICABLE",
    ),
    (
        "package-doctor",
        "just doctor when tool availability is known",
        "NOT-APPLICABLE",
    ),
}

FORBIDDEN_KEYS = {
    "serial_number", "hardware_uuid", "device_udid", "account_name", "host_name",
    "hostname", "apple_id", "ip_address", "mac_address", "ssid",
    "notification_contents", "shell_history", "access_token", "cookie",
    "credentials", "credential", "secret", "environment_dump", "clipboard_content",
    "secret_access_key", "aws_secret_access_key", "private_key",
    "terminal_content", "logical_key_content", "typed_key_content",
}
FORBIDDEN_TEXT_PATTERNS = (
    re.compile(r"/Users/[^/$\s]+/"),
    re.compile(r"/(?:private/)?var/folders/"),
    re.compile(r"/(?:private/)?tmp/"),
    re.compile(r"/Volumes/(?!SpaceTerm(?:\.app)?(?:/|$))[^\s]+"),
    re.compile(r"(?i)authorization\s*:\s*(?:bearer|basic)"),
    re.compile(
        r"(?i)(?:access[_-]?token|api[_-]?key|password|secret(?:[_-]?(?:access[_-]?)?key)?)"
        r"\s*[=:]\s*[^ <$]+"
    ),
    re.compile(r"(?i)https?://[^/@\s:]+:[^/@\s]+@"),
    re.compile(r"\bsk-[A-Za-z0-9_-]{16,}\b"),
    re.compile(r"\b(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{20,}\b"),
    re.compile(r"\bgithub_pat_[A-Za-z0-9_]{20,}\b"),
    re.compile(r"\bAKIA[0-9A-Z]{16}\b"),
    re.compile(r"-----BEGIN (?:RSA |DSA |EC |OPENSSH )?PRIVATE KEY-----"),
)

MAX_CAMPAIGN_PAYLOAD_BYTES = 4 * 1024 * 1024 * 1024
NATIVE_FAILURE_PRODUCER_COMMIT = "b74249dc03722dc6083ed32ae5934abdadc07403"
AX_PROBE_PRODUCER_COMMIT = "2099e2d66bf68eaab0fe25615e48d285bdda1cc2"
NATIVE_PUBLIC_EVIDENCE_IDS = (
    "native_closure_replay_artifact_id", "native_runtime_metadata_artifact_id",
    "native_runtime_samples_artifact_id", "native_runtime_events_artifact_id",
    "native_failure_actions_artifact_id",
)


class EvidenceError(RuntimeError):
    pass


def fail(message: str) -> None:
    raise EvidenceError(message)


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def sha256_path(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def byte_count(path: Path) -> int:
    return path.stat().st_size


def read_json(path: Path) -> Any:
    def unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            if key in result:
                fail(f"duplicate JSON key: {key}")
            result[key] = value
        return result

    try:
        if path.stat().st_size > 8 * 1024 * 1024:
            fail(f"JSON input exceeds the bounded 8 MiB size: {path}")
        with path.open("r", encoding="utf-8") as handle:
            return json.load(
                handle,
                object_pairs_hook=unique_object,
                parse_constant=lambda constant: fail(f"non-finite JSON number: {constant}"),
            )
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        fail(f"invalid JSON input {path}: {error}")


def canonical_json(value: Any) -> bytes:
    try:
        text = json.dumps(
            value, ensure_ascii=True, indent=2, sort_keys=True, allow_nan=False
        )
    except (TypeError, ValueError) as error:
        fail(f"value is not strict canonical JSON: {error}")
    return (text + "\n").encode("utf-8")


def write_exclusive(path: Path, payload: bytes, mode: int = 0o444) -> None:
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    try:
        descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, mode)
    except FileExistsError:
        fail(f"immutable output already exists: {path}")
    try:
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(payload)
            handle.flush()
            os.fsync(handle.fileno())
    except Exception:
        path.unlink(missing_ok=True)
        raise


def ensure_regular_file(path: Path, label: str) -> None:
    try:
        info = path.lstat()
    except FileNotFoundError:
        fail(f"{label} is missing: {path}")
    if not stat.S_ISREG(info.st_mode) or path.is_symlink():
        fail(f"{label} must be a non-symlink regular file: {path}")


def require_keys(value: dict[str, Any], keys: Iterable[str], label: str) -> None:
    missing = sorted(set(keys) - set(value))
    if missing:
        fail(f"{label} is missing required fields: {', '.join(missing)}")


def require_nonempty(value: Any, label: str) -> None:
    if value is None or value == "" or value == [] or value == {}:
        fail(f"{label} must not be empty")
    if isinstance(value, str) and re.fullmatch(r"<[^>]+>", value.strip()):
        fail(f"{label} contains an unresolved placeholder")


def validate_utc(value: Any, label: str) -> None:
    if not isinstance(value, str) or not UTC_RE.fullmatch(value):
        fail(f"{label} must be RFC 3339 UTC with whole seconds")
    try:
        dt.datetime.strptime(value, "%Y-%m-%dT%H:%M:%SZ")
    except ValueError:
        fail(f"{label} is not a valid UTC timestamp")


def reject_future_utc(value: str, label: str, *, skew_seconds: int = 300) -> None:
    validate_utc(value, label)
    timestamp = dt.datetime.strptime(value, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=dt.timezone.utc)
    if timestamp > dt.datetime.now(dt.timezone.utc) + dt.timedelta(seconds=skew_seconds):
        fail(f"{label} is implausibly in the future")


def privacy_scan(value: Any, label: str = "document") -> None:
    actual_home = str(Path.home())
    actual_tmp = os.environ.get("TMPDIR", "")
    def visit(item: Any, path: str) -> None:
        if isinstance(item, dict):
            for key, child in item.items():
                normalized = str(key).lower().replace("-", "_")
                if normalized in FORBIDDEN_KEYS:
                    fail(f"{label} contains prohibited field {path}.{key}")
                visit(child, f"{path}.{key}")
        elif isinstance(item, list):
            for index, child in enumerate(item):
                visit(child, f"{path}[{index}]")
        elif isinstance(item, str):
            if "\x00" in item or "\t" in item or "\r" in item or "\n" in item:
                fail(f"{label} contains a prohibited control character at {path}")
            if actual_home != "/" and actual_home in item:
                fail(f"{label} leaks the local home path at {path}; use $HOME")
            if actual_tmp and len(actual_tmp) > 1 and actual_tmp in item:
                fail(f"{label} leaks the local temporary path at {path}; use $TMPDIR")
            for pattern in FORBIDDEN_TEXT_PATTERNS:
                if pattern.search(item):
                    fail(f"{label} contains prohibited private or credential-like text at {path}")

    visit(value, "$")


def reject_forbidden_field_name(value: str, label: str) -> None:
    normalized = value.strip().lower().replace("-", "_")
    if normalized in FORBIDDEN_KEYS:
        fail(f"{label} contains prohibited field name: {value}")


def acceptance_parent() -> Path:
    return Path.home() / "SpaceTerm-Acceptance"


def publication_parent() -> Path:
    return acceptance_parent() / "public"


def publication_root(run_id: str) -> Path:
    if not RUN_RE.fullmatch(run_id):
        fail("run ID must use i43-YYYYMMDDTHHMMSSZ-commit12-dmgsha12")
    return publication_parent() / run_id


def resolve_publication(run_id: str) -> Path:
    root = publication_root(run_id)
    if not root.is_dir() or root.is_symlink() or root.name != run_id:
        fail(f"public evidence bundle is missing or unsafe: {root}")
    return root.resolve()


def create_publication_staging(run_id: str) -> tuple[Path, Path, Path]:
    parent = publication_parent()
    parent.mkdir(mode=0o700, parents=False, exist_ok=True)
    if parent.is_symlink():
        fail("public evidence parent must not be a symlink")
    final = publication_root(run_id)
    if final.exists() or final.is_symlink():
        fail(f"public evidence bundle already exists: {final}")
    staging_parent = Path(tempfile.mkdtemp(prefix=".issue-43-public.", dir=parent))
    staging_parent.chmod(0o700)
    staging = staging_parent / run_id
    staging.mkdir(mode=0o700)
    return staging.resolve(), final, staging_parent


def publish_staging(staging: Path, final: Path, staging_parent: Path) -> Path:
    if final.exists() or final.is_symlink():
        fail(f"public evidence bundle appeared during finalization: {final}")
    try:
        os.rename(staging, final)
        staging_parent.rmdir()
    except OSError as error:
        fail(f"could not atomically publish the verified evidence bundle: {error}")
    return final.resolve()


def copy_public_payloads(source_root: Path, public_root: Path, artifacts: dict[str, dict[str, Any]]) -> None:
    for directory_name in sorted(PAYLOAD_DIRS):
        (public_root / directory_name).mkdir(mode=0o755)
    for artifact in artifacts.values():
        source = source_root / artifact["relative_path"]
        destination = public_root / artifact["relative_path"]
        ensure_regular_file(source, "reviewed source payload")
        try:
            with source.open("rb") as source_handle:
                descriptor = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o444)
                with os.fdopen(descriptor, "wb") as destination_handle:
                    shutil.copyfileobj(source_handle, destination_handle, 1024 * 1024)
                    destination_handle.flush()
                    os.fsync(destination_handle.fileno())
        except Exception:
            destination.unlink(missing_ok=True)
            raise
        if sha256_path(destination) != artifact["sha256"] or byte_count(destination) != artifact["bytes"]:
            fail(f"public payload copy changed bytes: {artifact['artifact_id']}")


def private_dir(root: Path) -> Path:
    return root / ".issue-43-campaign-private"


def read_state_run_id(root: Path) -> str | None:
    path = private_dir(root) / "run-id"
    if not path.is_file() or path.is_symlink():
        return None
    return path.read_text(encoding="ascii").strip()


def collector_shape(root: Path) -> bool:
    if not root.is_dir() or root.is_symlink():
        return False
    info = root.stat()
    return (
        info.st_uid == os.geteuid()
        and stat.S_IMODE(info.st_mode) == 0o700
        and (root / "workspace").is_dir()
        and not (root / "workspace").is_symlink()
        and (root / "identity").is_dir()
        and not (root / "identity").is_symlink()
        and (root / "logs").is_dir()
        and not (root / "logs").is_symlink()
    )


def active_collector_shape(root: Path) -> bool:
    staged_dmg = root / "identity" / "dmg-stage" / "staged-package.dmg"
    mounted_app = root / "identity" / "dmg-mount" / "SpaceTerm.app"
    helper = root / "identity" / "acceptance-launch-verifier"
    stderr = root / "logs" / "native-launch.stderr"
    if not (
        collector_shape(root)
        and staged_dmg.is_file()
        and not staged_dmg.is_symlink()
        and mounted_app.is_dir()
        and not mounted_app.is_symlink()
        and helper.is_file()
        and not helper.is_symlink()
        and os.access(helper, os.X_OK)
        and stderr.is_file()
        and not stderr.is_symlink()
    ):
        return False
    try:
        return "authenticated mounted app is ready" in stderr.read_text(
            encoding="utf-8", errors="strict"
        )
    except (OSError, UnicodeError):
        return False


def resolve_root(run_id: str, *, for_init: bool = False) -> Path:
    if not RUN_RE.fullmatch(run_id):
        fail("run ID must use i43-YYYYMMDDTHHMMSSZ-commit12-dmgsha12")
    parent = acceptance_parent()
    if not parent.is_dir() or parent.is_symlink():
        fail(f"acceptance parent is missing or unsafe: {parent}")
    final = parent / run_id
    if final.exists() or final.is_symlink():
        if not collector_shape(final):
            fail(f"final run root does not have collector shape: {final}")
        live_hidden = [candidate for candidate in parent.glob(".acceptance-identity.*")
                       if active_collector_shape(candidate)]
        if live_hidden:
            fail("a completed final run and a live hidden collector coexist; refusing ambiguous root")
        state = read_state_run_id(final)
        if for_init and state is None:
            fail("first campaign binding must come from one active hidden mounted collector")
        if state is not None and state != run_id:
            fail("final run root is bound to a different campaign")
        return final.resolve()

    hidden = []
    for candidate in parent.glob(".acceptance-identity.*"):
        if not active_collector_shape(candidate):
            continue
        state = read_state_run_id(candidate)
        if state == run_id or (for_init and state is None):
            hidden.append(candidate)
    if len(hidden) != 1:
        fail(
            f"expected exactly one matching hidden mounted collector root below {parent}; "
            f"found {len(hidden)}"
        )
    return hidden[0].resolve()


@contextmanager
def campaign_lock(root: Path):
    state = private_dir(root)
    try:
        state.mkdir(mode=0o700)
    except FileExistsError:
        pass
    try:
        state_info = state.lstat()
    except FileNotFoundError:
        fail("private campaign state disappeared during lock acquisition")
    if (
        not stat.S_ISDIR(state_info.st_mode)
        or state.is_symlink()
        or state_info.st_uid != os.geteuid()
        or stat.S_IMODE(state_info.st_mode) != 0o700
    ):
        fail("private campaign state directory is unsafe")
    lock_path = state / ".lock"
    try:
        descriptor = os.open(
            lock_path,
            os.O_RDWR | os.O_CREAT | getattr(os, "O_NOFOLLOW", 0),
            0o600,
        )
    except OSError as error:
        fail(f"private campaign lock is unsafe: {error}")
    with os.fdopen(descriptor, "a+b") as handle:
        lock_info = os.fstat(handle.fileno())
        if not stat.S_ISREG(lock_info.st_mode) or lock_info.st_nlink != 1:
            fail("private campaign lock is not a unique regular file")
        fcntl.flock(handle.fileno(), fcntl.LOCK_EX)
        yield


def require_initialized(root: Path, run_id: str) -> Path:
    state = private_dir(root)
    if read_state_run_id(root) != run_id:
        fail("campaign evidence root is not initialized for this run ID")
    binding_path = state / "collector-binding.json"
    ensure_regular_file(binding_path, "collector binding")
    binding = read_json(binding_path)
    if not isinstance(binding, dict):
        fail("collector binding is invalid")
    info = root.stat()
    expected = {
        "run_id": run_id,
        "final_directory_name": run_id,
        "root_device": info.st_dev,
        "root_inode": info.st_ino,
    }
    for key, value in expected.items():
        if binding.get(key) != value:
            fail("collector root inode or final-path binding changed")
    if not HASH_RE.fullmatch(str(binding.get("staged_dmg_sha256", ""))):
        fail("collector binding lacks its staged DMG digest")
    run_match = RUN_RE.fullmatch(run_id)
    assert run_match is not None
    if not str(binding["staged_dmg_sha256"]).startswith(run_match.group(3)):
        fail("collector binding staged DMG digest disagrees with run ID")
    return state


def command_init(args: argparse.Namespace) -> None:
    root = resolve_root(args.run_id, for_init=True)
    with campaign_lock(root):
        state = private_dir(root)
        run_file = state / "run-id"
        if run_file.exists():
            if read_state_run_id(root) != args.run_id:
                fail("hidden collector root is already bound to another campaign")
            print(root)
            return
        staged_dmg = root / "identity" / "dmg-stage" / "staged-package.dmg"
        staged_digest = sha256_path(staged_dmg)
        run_match = RUN_RE.fullmatch(args.run_id)
        assert run_match is not None
        if not staged_digest.startswith(run_match.group(3)):
            fail("run ID DMG prefix does not match the active collector's staged DMG")
        for directory in ("records", "artifact-metadata", "artifact-reviews", "record-reviews"):
            (state / directory).mkdir(mode=0o700, exist_ok=False)
        write_exclusive(run_file, f"{args.run_id}\n".encode("ascii"), 0o400)
        root_info = root.stat()
        binding = {
            "run_id": args.run_id,
            "final_directory_name": args.run_id,
            "root_device": root_info.st_dev,
            "root_inode": root_info.st_ino,
            "staged_dmg_sha256": staged_digest,
        }
        write_exclusive(state / "collector-binding.json", canonical_json(binding), 0o400)
        for directory in PAYLOAD_DIRS:
            (root / directory).mkdir(mode=0o700, exist_ok=True)
        print(root)


def validate_metadata(document: Any, run_id: str) -> dict[str, Any]:
    if not isinstance(document, dict):
        fail("campaign metadata must be a JSON object")
    forbidden_generated = {
        "campaign_status", "finished_utc", "case_results", "payload_manifest",
        "control_digest", "control_sha256", "identity_replay",
    }
    collision = sorted(forbidden_generated & set(document))
    if collision:
        fail(f"campaign metadata attempts to supply generated fields: {', '.join(collision)}")
    require_keys(
        document,
        (
            "schema_version", "issue", "run_id", "started_utc", "frozen_artifact",
            "host", "clean_environment", "programs", "ghostty_reference", "drivers",
            "issue_42_conformance", "validation", "identity_evidence", "known_deviations",
        ),
        "campaign metadata",
    )
    if document["schema_version"] != 2 or document["issue"] != 43 or document["run_id"] != run_id:
        fail("campaign metadata schema, issue, or run ID is invalid")
    reject_future_utc(document["started_utc"], "campaign started_utc")
    run_started = RUN_RE.fullmatch(run_id).group(1)  # type: ignore[union-attr]
    run_started_utc = (
        f"{run_started[0:4]}-{run_started[4:6]}-{run_started[6:8]}T"
        f"{run_started[9:11]}:{run_started[11:13]}:{run_started[13:15]}Z"
    )
    if document["started_utc"] < run_started_utc:
        fail("campaign started_utc predates the run ID timestamp")

    frozen = document["frozen_artifact"]
    if not isinstance(frozen, dict):
        fail("frozen_artifact must be an object")
    require_keys(
        frozen,
        (
            "repository", "commit_sha", "cargo_lock_sha256", "working_tree_clean",
            "package_command", "marketing_version", "build_version",
            "executable_architectures", "code_signing_command", "code_signing_result",
            "app_bundle_sha256", "dmg_sha256", "package_verification_artifact",
            "launch_source",
        ),
        "frozen_artifact",
    )
    run_match = RUN_RE.fullmatch(run_id)
    assert run_match is not None
    if not re.fullmatch(r"[0-9a-f]{40}", str(frozen["commit_sha"])):
        fail("frozen commit_sha must be 40 lowercase hex characters")
    if not str(frozen["commit_sha"]).startswith(run_match.group(2)):
        fail("run ID commit prefix disagrees with frozen commit")
    for key in ("cargo_lock_sha256", "app_bundle_sha256", "dmg_sha256"):
        if not HASH_RE.fullmatch(str(frozen[key])):
            fail(f"frozen {key} must be a lowercase SHA-256")
    if not str(frozen["dmg_sha256"]).startswith(run_match.group(3)):
        fail("run ID DMG prefix disagrees with frozen DMG")
    if frozen["working_tree_clean"] is not True:
        fail("frozen source working tree must be clean")
    if frozen["repository"] != "https://github.com/sadiksaifi/SpaceTerm":
        fail("frozen repository is not the issue #43 repository")
    if frozen["package_command"] != "just package":
        fail("frozen package command must be exactly 'just package'")
    if frozen["code_signing_result"] != "PASS":
        fail("frozen code-signing verification must PASS")
    if frozen["launch_source"] != "mounted verified DMG":
        fail("frozen launch source must be mounted verified DMG")
    for key in (
        "repository", "package_command", "marketing_version", "build_version",
        "executable_architectures", "code_signing_command", "package_verification_artifact",
    ):
        require_nonempty(frozen[key], f"frozen_artifact.{key}")

    host = document["host"]
    if not isinstance(host, dict):
        fail("host must be an object")
    require_keys(
        host,
        (
            "macos_version", "macos_build", "machine_model", "model_identifier", "cpu",
            "memory_bytes", "displays", "terminal_font_selected",
            "jetbrains_mono_nerd_font_available", "initial_grid", "input_sources",
            "second_display_available", "numpad_available", "non_key_window_possible",
        ),
        "host",
    )
    for key in ("macos_version", "macos_build", "machine_model", "model_identifier", "cpu",
                "displays", "terminal_font_selected", "initial_grid", "input_sources"):
        require_nonempty(host[key], f"host.{key}")
    if not isinstance(host["memory_bytes"], int) or host["memory_bytes"] <= 0:
        fail("host.memory_bytes must be a positive integer")
    if not isinstance(host["displays"], list) or not host["displays"]:
        fail("host.displays must contain at least one display")
    for index, display in enumerate(host["displays"]):
        if not isinstance(display, dict):
            fail(f"host.displays[{index}] must be an object")
        require_keys(
            display,
            (
                "display_id", "logical_resolution", "backing_resolution", "refresh_hz",
                "backing_scale",
            ),
            f"host.displays[{index}]",
        )
        for key in ("display_id", "logical_resolution", "backing_resolution"):
            require_nonempty(display[key], f"host.displays[{index}].{key}")
        if (
            not isinstance(display["refresh_hz"], (int, float))
            or not math.isfinite(display["refresh_hz"])
            or display["refresh_hz"] <= 0
        ):
            fail("display refresh_hz must be positive")
        if (
            not isinstance(display["backing_scale"], (int, float))
            or not math.isfinite(display["backing_scale"])
            or display["backing_scale"] <= 0
        ):
            fail("display backing_scale must be positive")
    grid = host["initial_grid"]
    if not isinstance(grid, dict):
        fail("host.initial_grid must be an object")
    require_keys(
        grid,
        (
            "rows", "columns", "logical_width", "logical_height", "backing_pixel_width",
            "backing_pixel_height",
        ),
        "host.initial_grid",
    )
    if any(
        not isinstance(grid[key], (int, float))
        or not math.isfinite(grid[key])
        or grid[key] <= 0
        for key in grid
    ):
        fail("every host.initial_grid dimension must be positive")
    input_sources = host["input_sources"]
    if (
        not isinstance(input_sources, list)
        or any(not isinstance(item, str) or not item for item in input_sources)
        or len(set(input_sources)) < 2
    ):
        fail("host input_sources must record distinct ordinary/non-US/IME sources used")
    for key in (
        "jetbrains_mono_nerd_font_available", "second_display_available", "numpad_available"
        , "non_key_window_possible"
    ):
        if not isinstance(host[key], bool):
            fail(f"host.{key} must be boolean")

    clean = document["clean_environment"]
    if not isinstance(clean, dict):
        fail("clean_environment must be an object")
    require_keys(
        clean,
        ("workspace_root", "temporary_configurations", "permanent_user_configuration_used",
         "privacy_review"),
        "clean_environment",
    )
    if clean["permanent_user_configuration_used"] is not False or clean["privacy_review"] != "PASS":
        fail("clean environment must avoid permanent configuration and pass privacy review")
    if not str(clean["workspace_root"]).startswith("$TMPDIR/"):
        fail("clean workspace root must be normalized below $TMPDIR")
    if not isinstance(clean["temporary_configurations"], list) or not clean["temporary_configurations"]:
        fail("temporary configuration inventory is required")
    for index, configuration in enumerate(clean["temporary_configurations"]):
        if not isinstance(configuration, dict):
            fail(f"temporary_configurations[{index}] must be an object")
        require_keys(configuration, ("program", "path", "sha256"),
                     f"temporary_configurations[{index}]")
        require_nonempty(configuration["program"], f"temporary_configurations[{index}].program")
        if not str(configuration["path"]).startswith(("$HOME/", "$TMPDIR/")):
            fail("temporary configuration path must be privacy-normalized")
        if not HASH_RE.fullmatch(str(configuration["sha256"])):
            fail("temporary configuration SHA-256 is invalid")

    programs = document["programs"]
    if not isinstance(programs, list):
        fail("programs must be a list")
    names = []
    for index, program in enumerate(programs):
        if not isinstance(program, dict):
            fail(f"programs[{index}] must be an object")
        require_keys(
            program,
            ("name", "executable", "executable_sha256", "version_command", "version_output"),
            f"programs[{index}]",
        )
        if not isinstance(program["name"], str):
            fail(f"programs[{index}].name must be text")
        names.append(program["name"])
        if not HASH_RE.fullmatch(str(program["executable_sha256"])):
            fail(f"programs[{index}].executable_sha256 is invalid")
        for key in ("executable", "version_command", "version_output"):
            require_nonempty(program[key], f"programs[{index}].{key}")
    if set(names) != PROGRAMS or len(names) != len(PROGRAMS):
        fail("program inventory must contain each required shell/TUI exactly once")

    ghostty = document["ghostty_reference"]
    if not isinstance(ghostty, dict):
        fail("ghostty_reference must be an object")
    require_keys(
        ghostty,
        (
            "original_prd_revision", "embedded_conformance_revision", "runnable_build_source",
            "public_version", "commit_sha", "marketing_version", "build_version",
            "executable", "executable_architectures", "code_signing_result",
            "app_bundle_sha256", "config_path", "config_sha256",
            "selected_font", "initial_grid", "behavior_settings_sha256",
            "relationship_to_recorded_revisions", "ambiguity_notes",
        ),
        "ghostty_reference",
    )
    if ghostty["original_prd_revision"] != "46767b521358200bfe3f268f365ccd2f218db558":
        fail("Ghostty original PRD revision is invalid")
    if ghostty["embedded_conformance_revision"] != "a887df42c56f6de86c0fe6da9c4eeca37931e083":
        fail("Ghostty embedded conformance revision is invalid")
    if ghostty["code_signing_result"] != "PASS":
        fail("Ghostty signing verification must PASS")
    for key in ("app_bundle_sha256", "config_sha256", "behavior_settings_sha256"):
        if not HASH_RE.fullmatch(str(ghostty[key])):
            fail(f"ghostty_reference.{key} is invalid")
    for key in ghostty:
        require_nonempty(ghostty[key], f"ghostty_reference.{key}")
    if ghostty["selected_font"] != host["terminal_font_selected"] \
            or ghostty["initial_grid"] != host["initial_grid"]:
        fail("Ghostty reference font/grid differ from the frozen SpaceTerm comparison inputs")

    if not isinstance(document["drivers"], list) or not document["drivers"]:
        fail("driver inventory is required")
    driver_purposes = set()
    for index, driver in enumerate(document["drivers"]):
        if not isinstance(driver, dict):
            fail(f"drivers[{index}] must be an object")
        require_keys(
            driver,
            ("purpose", "path", "commit_sha", "sha256", "version_or_help", "invocation"),
            f"drivers[{index}]",
        )
        driver_purposes.add(driver["purpose"])
        if not re.fullmatch(r"[0-9a-f]{40}", str(driver["commit_sha"])):
            fail(f"drivers[{index}].commit_sha is invalid")
        if driver["commit_sha"] != frozen["commit_sha"]:
            fail(f"drivers[{index}] is not bound to the frozen campaign commit")
        if not HASH_RE.fullmatch(str(driver["sha256"])):
            fail(f"drivers[{index}].sha256 is invalid")
        driver_path = Path(str(driver["path"]))
        if driver_path.is_absolute() or ".." in driver_path.parts or not driver_path.parts:
            fail(f"drivers[{index}].path must be a repository-relative path")
        for key in ("purpose", "path", "version_or_help", "invocation"):
            require_nonempty(driver[key], f"drivers[{index}].{key}")
    required_driver_purposes = {
        "identity", "native", "focus", "failure", "workload", "rss", "package",
        "payload-manifest", "control-digest",
    }
    if not required_driver_purposes <= driver_purposes:
        fail("driver inventory does not cover every required issue #43 evidence capability")
    required_tool_paths = {
        "scripts/acceptance/issue-43-campaign-evidence.py",
        "scripts/acceptance-identity.sh",
        "scripts/acceptance-launch-verifier.m",
        "scripts/acceptance/failure-action-driver.sh",
        "scripts/acceptance/native-ax-probe.sh",
        "scripts/acceptance/native-ax-probe.m",
        "scripts/acceptance/freeze-performance-pair.sh",
        "scripts/acceptance/freeze-performance-run-intent.sh",
        "scripts/acceptance/freeze-performance-run.sh",
        "scripts/acceptance/freeze-performance-subject.sh",
        "scripts/acceptance/performance-pair-result.py",
        "scripts/acceptance/performance-driver-receipt.py",
        "scripts/acceptance/performance-subject-lifecycle.py",
        "scripts/acceptance/performance-tail-receipt.py",
        "scripts/acceptance/verify-performance-lifecycle-receipts.py",
        "scripts/acceptance/verify-performance-subject-exit.py",
        "scripts/acceptance/verify-performance-workload-ready.py",
        "scripts/acceptance/verify-performance-workload-auth.py",
        "scripts/acceptance/run-native-performance-scenario.sh",
        "scripts/acceptance/build-native-performance-tools.sh",
        "scripts/acceptance/assemble-release-performance-rss-v3.sh",
        "scripts/acceptance/analyze-release-performance-sustained.awk",
        "scripts/acceptance/analyze-release-performance-resize.awk",
        "scripts/acceptance/analyze-release-performance-case.sh",
        "scripts/acceptance/performance-plan.sh",
        "scripts/acceptance/performance-workload.sh",
        "scripts/acceptance/performance-workload.c",
        "scripts/acceptance/performance-rss-sampler.m",
        "scripts/acceptance/performance-window-resolver.m",
        "scripts/acceptance/performance-appkit-terminate.m",
        "scripts/acceptance/performance-driver.m",
        "scripts/acceptance/freeze-render-profile-intent.sh",
        "scripts/acceptance/finalize-render-profile-evidence.sh",
        "scripts/acceptance/render-trace-receipt.py",
        "scripts/acceptance/archive-render-trace.py",
        "scripts/acceptance/verify-render-trace-archive.py",
        "scripts/acceptance/verify-render-action-video.py",
        "scripts/acceptance/analyze-release-render-profile-case.sh",
        "scripts/record-release-performance-trace.sh",
        "scripts/sample-release-performance-rss.sh",
        "scripts/release-performance-workload.sh",
        "scripts/inspect-release-performance-process.py",
        "scripts/run-release-performance-command.py",
        "scripts/verify-release-performance-trace.py",
    }
    driver_paths = [str(driver["path"]) for driver in document["drivers"]]
    if len(driver_paths) != len(set(driver_paths)):
        fail("driver inventory contains duplicate repository paths")
    if not required_tool_paths <= set(driver_paths):
        fail("driver inventory omits a finalizer, identity, RSS, pair, or trace policy tool")
    validation = document["validation"]
    if not isinstance(validation, dict):
        fail("validation must be an object")
    require_keys(validation, ("command", "status", "artifact_id"), "validation")
    if validation["command"] != "just validate":
        fail("final validation command must be exactly 'just validate'")
    if validation["status"] not in {"PASS", "FAIL", "NOT-RUN"}:
        fail("validation status is invalid")
    issue_42 = document["issue_42_conformance"]
    if not isinstance(issue_42, dict):
        fail("issue_42_conformance must be an object")
    require_keys(
        issue_42,
        ("issue_url", "candidate_commit_sha", "command", "status", "artifact_id"),
        "issue_42_conformance",
    )
    if issue_42["issue_url"] != "https://github.com/sadiksaifi/SpaceTerm/issues/42":
        fail("issue #42 conformance prerequisite uses the wrong issue anchor")
    if issue_42["candidate_commit_sha"] != frozen["commit_sha"]:
        fail("issue #42 conformance prerequisite is not bound to the candidate commit")
    if issue_42["command"] != "just validate":
        fail("issue #42 conformance prerequisite must use the complete Just validation gate")
    if issue_42["status"] not in {"PASS", "FAIL", "NOT-RUN"}:
        fail("issue #42 conformance prerequisite status is invalid")
    require_nonempty(issue_42["artifact_id"], "issue_42_conformance.artifact_id")
    identity_evidence = document["identity_evidence"]
    if not isinstance(identity_evidence, dict):
        fail("identity_evidence must be an object")
    require_keys(
        identity_evidence,
        (
            "public_identity_artifact_id", "final_identity_replay_artifact_id",
            "display_summary_artifact_id", "ghostty_identity_artifact_id",
            "host_preconditions_artifact_id", *NATIVE_PUBLIC_EVIDENCE_IDS,
        ),
        "identity_evidence",
    )
    for key in (
        "public_identity_artifact_id", "final_identity_replay_artifact_id",
        "display_summary_artifact_id", "ghostty_identity_artifact_id",
        "host_preconditions_artifact_id", *NATIVE_PUBLIC_EVIDENCE_IDS,
    ):
        require_nonempty(identity_evidence[key], f"identity_evidence.{key}")
    if not isinstance(document["known_deviations"], list):
        fail("known_deviations must be a list")
    privacy_scan(document, "campaign metadata")
    return document


def command_set_metadata(args: argparse.Namespace) -> None:
    root = resolve_root(args.run_id)
    with campaign_lock(root):
        state = require_initialized(root, args.run_id)
        document = validate_metadata(read_json(Path(args.input)), args.run_id)
        write_exclusive(state / "campaign-metadata.json", canonical_json(document), 0o400)


def verify_frozen_repository_tools(metadata: dict[str, Any], *, require_clean_head: bool) -> None:
    repo = Path(__file__).resolve().parents[2]
    commit = metadata["frozen_artifact"]["commit_sha"]
    try:
        head = subprocess.run(
            ["git", "rev-parse", "HEAD"], cwd=repo, check=True,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=30,
        ).stdout.strip()
        status = subprocess.run(
            ["git", "status", "--porcelain", "--untracked-files=all"], cwd=repo, check=True,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=30,
        ).stdout
    except (OSError, subprocess.CalledProcessError, subprocess.TimeoutExpired) as error:
        fail(f"frozen repository identity cannot be verified: {error}")
    if head != commit or status:
        fail("campaign replay must run from the exact clean frozen campaign commit")
    for producer, label in (
        (NATIVE_FAILURE_PRODUCER_COMMIT, "b742 native/failure producer"),
        (AX_PROBE_PRODUCER_COMMIT, "2099 native AX probe producer"),
    ):
        try:
            producer_ancestry = subprocess.run(
                ["git", "merge-base", "--is-ancestor", producer, commit],
                cwd=repo, check=False, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                timeout=30,
            )
        except (OSError, subprocess.TimeoutExpired) as error:
            fail(f"{label} ancestry cannot be authenticated: {error}")
        if producer_ancestry.returncode != 0:
            fail(f"frozen campaign commit does not contain the authenticated {label}")
    for driver in metadata["drivers"]:
        relative = str(driver["path"])
        local = repo / relative
        ensure_regular_file(local, f"campaign driver {relative}")
        try:
            frozen_bytes = subprocess.run(
                ["git", "show", f"{commit}:{relative}"], cwd=repo, check=True,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=30,
            ).stdout
        except (OSError, subprocess.CalledProcessError, subprocess.TimeoutExpired) as error:
            fail(f"campaign driver is absent from the frozen commit: {relative}: {error}")
        digest = hashlib.sha256(frozen_bytes).hexdigest()
        if driver["sha256"] != digest:
            fail(f"campaign driver hash disagrees with the frozen commit: {relative}")
        if sha256_path(local) != digest:
            fail(f"local campaign driver differs from the frozen commit: {relative}")


def validate_conditional_subcases(record: dict[str, Any]) -> None:
    subcases = record.get("conditional_subcases", [])
    if not isinstance(subcases, list):
        fail("conditional_subcases must be a list")
    seen = set()
    allowed_names = {
        case_id: {name for allowed_case, name, _status in ALLOWED_CONDITIONALS
                  if allowed_case == case_id}
        for case_id in ALL_CASES
    }
    for index, item in enumerate(subcases):
        if not isinstance(item, dict):
            fail(f"conditional_subcases[{index}] must be an object")
        require_keys(item, ("name", "status", "availability_or_precondition_evidence"),
                     f"conditional_subcases[{index}]")
        key = (record["case_id"], item["name"], item["status"])
        if item["name"] not in allowed_names.get(record["case_id"], set()):
            fail(f"unapproved conditional subcase for {record['case_id']}: {item['name']}")
        if item["status"] != "PASS" and key not in ALLOWED_CONDITIONALS:
            fail(f"unapproved conditional skip for {record['case_id']}: {item['name']}")
        require_nonempty(
            item["availability_or_precondition_evidence"],
            f"conditional_subcases[{index}].availability_or_precondition_evidence",
        )
        if item["name"] in seen:
            fail("duplicate conditional subcase")
        seen.add(item["name"])
    if seen != allowed_names.get(record["case_id"], set()):
        fail(f"conditional subcase inventory is not exact for {record['case_id']}")


def required_case_clauses(case_id: str, subject: str) -> tuple[str, ...]:
    if subject == "ghostty" and case_id in PERFORMANCE_CASES:
        return GHOSTTY_PERFORMANCE_CLAUSES[case_id]
    return CASE_CLAUSES[case_id]


def validate_record(document: Any, run_id: str) -> dict[str, Any]:
    if not isinstance(document, dict):
        fail("record must be a JSON object")
    require_keys(
        document,
        (
            "record_id", "case_id", "subject", "matrix", "attempt",
            "comparison_record_id", "supersedes_record_id", "status", "started_utc",
            "finished_utc", "frozen_identity_verified", "command",
            "environment_and_config", "interactions", "expected", "authority", "observed",
            "artifacts", "comparison_observation", "smallest_reproduction", "skip_reason",
            "conditional_subcases", "requirement_checks", "notes",
        ),
        "case record",
    )
    match = RECORD_RE.fullmatch(str(document["record_id"]))
    if match is None:
        fail("record_id does not match the issue #43 record identity form")
    parsed_run, parsed_case, parsed_subject, parsed_attempt = match.groups()
    if parsed_run != run_id:
        fail("record_id belongs to another campaign")
    if document["case_id"] != parsed_case or document["subject"] != parsed_subject:
        fail("record_id case or subject disagrees with record fields")
    if document["attempt"] != int(parsed_attempt) or int(parsed_attempt) < 1:
        fail("record attempt disagrees with record_id")
    case_id = document["case_id"]
    if case_id not in ALL_CASES:
        fail(f"unknown issue #43 case ID: {case_id}")
    expected_matrix = CASE_MATRIX.get(case_id, "supplementary")
    if document["matrix"] != expected_matrix or document["matrix"] not in MATRICES:
        fail("record matrix disagrees with the inventory")
    if document["status"] not in STATUSES:
        fail("record status is invalid")
    if document["status"] == "PASS" and document["frozen_identity_verified"] is not True:
        fail("a PASS record requires frozen_identity_verified=true")
    required_clauses = () if case_id == "perf-render-kitty-static" else required_case_clauses(
        case_id, document["subject"]
    )
    if (
        not isinstance(document["artifacts"], list)
        or not document["artifacts"]
        or any(not isinstance(item, str) or not item for item in document["artifacts"])
    ):
        fail("every record requires at least one evidence artifact reference")
    if len(document["artifacts"]) != len(set(document["artifacts"])):
        fail("record contains duplicate artifact references")
    if not isinstance(document["interactions"], list) or not document["interactions"]:
        fail("record requires ordered interactions")
    orders = []
    for index, interaction in enumerate(document["interactions"]):
        if not isinstance(interaction, dict):
            fail(f"interactions[{index}] must be an object")
        require_keys(
            interaction,
            ("order", "action", "timing", "clause_ids"),
            f"interactions[{index}]",
        )
        orders.append(interaction["order"])
        require_nonempty(interaction["action"], f"interactions[{index}].action")
        require_nonempty(interaction["timing"], f"interactions[{index}].timing")
        if (
            not isinstance(interaction["clause_ids"], list)
            or not interaction["clause_ids"]
            or any(not isinstance(item, str) for item in interaction["clause_ids"])
        ):
            fail("each interaction must name the exact requirement clauses it executes")
    if orders != list(range(1, len(orders) + 1)):
        fail("interaction order must be contiguous starting at 1")
    checks = document["requirement_checks"]
    if not isinstance(checks, list) or not checks:
        fail("record requires a clause-by-clause requirement_checks list")
    check_ids = set()
    for index, check in enumerate(checks):
        if not isinstance(check, dict):
            fail(f"requirement_checks[{index}] must be an object")
        require_keys(
            check,
            ("clause_id", "requirement", "status", "evidence_artifact_ids"),
            f"requirement_checks[{index}]",
        )
        if not CASE_RE.fullmatch(str(check["clause_id"])) or check["clause_id"] in check_ids:
            fail("requirement check clause IDs must be unique lowercase identifiers")
        check_ids.add(check["clause_id"])
        require_nonempty(check["requirement"], f"requirement_checks[{index}].requirement")
        if check["status"] not in STATUSES:
            fail("requirement check status is invalid")
        if (
            not isinstance(check["evidence_artifact_ids"], list)
            or not check["evidence_artifact_ids"]
            or any(not isinstance(item, str) or not item for item in check["evidence_artifact_ids"])
        ):
            fail("each requirement check needs evidence artifact IDs")
        if any(item not in document["artifacts"] for item in check["evidence_artifact_ids"]):
            fail("requirement check references evidence outside its record")
        if document["status"] == "PASS" and check["status"] != "PASS":
            fail("a PASS record has an unmet requirement clause")
    if case_id != "perf-render-kitty-static" and check_ids != set(required_clauses):
        missing = sorted(set(required_clauses) - check_ids)
        extra = sorted(check_ids - set(required_clauses))
        fail(
            f"record requirement clause inventory is not exact for {case_id}; "
            f"missing={missing}, extra={extra}"
        )
    if case_id != "perf-render-kitty-static":
        interaction_clauses = {
            clause
            for interaction in document["interactions"]
            for clause in interaction["clause_ids"]
        }
        if interaction_clauses != set(required_clauses):
            fail("ordered interactions do not execute the exact case requirement clause inventory")
    for key in ("command", "environment_and_config", "expected", "authority", "observed",
                "comparison_observation", "notes"):
        require_nonempty(document[key], f"record.{key}")
    if document["matrix"] != "supplementary" and re.search(
        r"\bGhostty\b", str(document["authority"]), re.IGNORECASE
    ):
        fail("Ghostty cannot be recorded as correctness authority")
    reject_future_utc(document["started_utc"], "record.started_utc")
    reject_future_utc(document["finished_utc"], "record.finished_utc")
    if document["finished_utc"] < document["started_utc"]:
        fail("record finished_utc precedes started_utc")
    if document["status"] == "FAIL":
        require_nonempty(document["smallest_reproduction"], "FAIL smallest_reproduction")
    elif document["smallest_reproduction"] != "none":
        fail("non-FAIL smallest_reproduction must be 'none'")
    if document["status"] in {"SKIPPED-UNAVAILABLE", "NOT-APPLICABLE"}:
        require_nonempty(document["skip_reason"], "skipped record skip_reason")
    elif document["skip_reason"] != "none":
        fail("non-skipped record skip_reason must be 'none'")
    validate_conditional_subcases(document)

    if expected_matrix == "focus":
        require_keys(
            document,
            (
                "focused_pane_identity_before", "focused_pane_identity_blocked",
                "terminal_input_focus_before", "terminal_input_focus_blocked",
                "terminal_input_focus_restored", "cursor_negotiated_before", "cursor_blocked",
                "cursor_restored", "hollow_visible_on_next_presented_frame", "dec_1004",
            ),
            "focus record",
        )
        if document["status"] == "PASS":
            cursor_before = str(document["cursor_negotiated_before"])
            cursor_blocked = str(document["cursor_blocked"])
            hidden_cursor = "hidden" in cursor_before.lower()
            expected_blocked = "hidden" if hidden_cursor else "steady hollow"
            expected_next_frame = not hidden_cursor
            if (
                document["terminal_input_focus_before"] is not True
                or document["terminal_input_focus_blocked"] is not False
                or document["terminal_input_focus_restored"] is not True
                or cursor_blocked.lower() != expected_blocked
                or document["cursor_restored"] != document["cursor_negotiated_before"]
                or document["hollow_visible_on_next_presented_frame"] is not expected_next_frame
            ):
                fail("focus PASS contradicts the required focus/cursor transition")
            expected_identity_change = case_id in {
                "focus-pane-switch", "focus-window-selector", "focus-hierarchy-switch",
            }
            identity_changed = (
                document["focused_pane_identity_before"]
                != document["focused_pane_identity_blocked"]
            )
            if identity_changed != expected_identity_change:
                fail("focus PASS contradicts the route-specific Focused Pane identity transition")
        dec = document["dec_1004"]
        if not isinstance(dec, dict):
            fail("focus dec_1004 must be an object")
        require_keys(
            dec,
            (
                "enabled", "enable_current_state_bytes_hex", "loss_bytes_hex",
                "gain_bytes_hex", "duplicate_reports", "held_key_release_bytes_hex",
                "pty_artifact_id",
            ),
            "focus dec_1004",
        )
        for key in (
            "enable_current_state_bytes_hex", "loss_bytes_hex", "gain_bytes_hex",
            "held_key_release_bytes_hex",
        ):
            if not isinstance(dec[key], str) or not re.fullmatch(r"(?:[0-9a-f]{2})+", dec[key]):
                fail(f"focus dec_1004.{key} must be exact lowercase hex bytes")
        if document["status"] == "PASS" and (
            dec["enabled"] is not True
            or dec["enable_current_state_bytes_hex"] != "1b5b49"
            or dec["loss_bytes_hex"] != "1b5b4f"
            or dec["gain_bytes_hex"] != "1b5b49"
            or dec["duplicate_reports"] != 0
        ):
            fail("focus PASS does not contain the exact DEC 1004 transition proof")
    elif expected_matrix == "failure":
        require_keys(
            document,
            (
                "injection_or_trigger", "presentation_generation_before",
                "presentation_generation_visible_during_failure", "visible_state",
                "terminal_input_usable_during_failure", "recovery_action",
                "post_recovery_result", "owned_processes_remaining", "diagnostics_bytes",
                "diagnostics_content_audit",
            ),
            "failure record",
        )
        before = document["presentation_generation_before"]
        visible = document["presentation_generation_visible_during_failure"]
        if not isinstance(before, int) or before < 0:
            fail("failure presentation_generation_before must be non-negative")
        if case_id in {
            "failure-presentation-recoverable", "failure-renderer-resource-recoverable"
        } and document["status"] == "PASS" and visible != before:
            fail("recoverable presentation failure PASS did not retain the prior generation")
        if case_id == "failure-platform-action-recoverable" and document["status"] == "PASS" \
                and document["terminal_input_usable_during_failure"] is not True:
            fail("recoverable platform action PASS did not keep terminal input usable")
        if case_id in {"failure-pty-fatal", "failure-emulator-session-fatal"} \
                and document["status"] == "PASS" and document["owned_processes_remaining"] != 0:
            fail("fatal recovery PASS left an owned process behind")
        if case_id == "failure-diagnostics-bounded" and document["status"] == "PASS":
            if (
                not isinstance(document["diagnostics_bytes"], int)
                or not 0 <= document["diagnostics_bytes"] <= 64 * 1024
                or document["diagnostics_content_audit"] != "PASS"
            ):
                fail("bounded diagnostics PASS exceeds 64 KiB or lacks content audit")
            require_keys(document, ("diagnostics_rows",), "bounded diagnostics record")
            if (
                not isinstance(document["diagnostics_rows"], int)
                or not 0 <= document["diagnostics_rows"] <= 128
            ):
                fail("bounded diagnostics PASS exceeds 128 rows")
    elif expected_matrix == "performance":
        require_keys(
            document,
            ("comparison_inputs", "comparison_inputs_sha256", "subject_identity_sha256"),
            "performance record",
        )
        if not HASH_RE.fullmatch(str(document["subject_identity_sha256"])):
            fail("performance subject_identity_sha256 is invalid")
        comparison_inputs = document["comparison_inputs"]
        if not isinstance(comparison_inputs, dict):
            fail("performance comparison_inputs must be an object")
        require_keys(
            comparison_inputs,
            (
                "workload_sha256", "duration_seconds", "warmup_seconds", "font_sha256",
                "grid_sha256", "configuration_sha256", "shell_process_sha256",
                "input_sha256", "host_identity_sha256", "scenario_settings_sha256",
            ),
            "performance comparison_inputs",
        )
        for key in (
            "workload_sha256", "font_sha256", "grid_sha256", "configuration_sha256",
            "shell_process_sha256", "input_sha256", "host_identity_sha256",
            "scenario_settings_sha256",
        ):
            if not HASH_RE.fullmatch(str(comparison_inputs[key])):
                fail(f"performance comparison_inputs.{key} is invalid")
        if not isinstance(comparison_inputs["duration_seconds"], int) or comparison_inputs["duration_seconds"] < 1:
            fail("performance comparison duration must be positive")
        if not isinstance(comparison_inputs["warmup_seconds"], int) or comparison_inputs["warmup_seconds"] < 0:
            fail("performance comparison warmup must be non-negative")
        if not HASH_RE.fullmatch(str(document["comparison_inputs_sha256"])):
            fail("performance comparison_inputs_sha256 is invalid")
        expected_comparison_digest = hashlib.sha256(canonical_json(comparison_inputs)).hexdigest()
        if document["comparison_inputs_sha256"] != expected_comparison_digest:
            fail("performance comparison_inputs_sha256 does not bind comparison_inputs")
        if case_id in RENDER_CASES:
            require_keys(
                document,
                (
                    "trace_duration_seconds", "sampling_settings", "process_identity",
                    "inspected_call_tree_filters", "time_profiler_artifact_id",
                    "allocations_artifact_id", "screen_artifact_ids",
                ),
                "render performance record",
            )
            if (
                not isinstance(document["trace_duration_seconds"], int)
                or document["trace_duration_seconds"] < 1
                or document["trace_duration_seconds"] != comparison_inputs["duration_seconds"]
            ):
                fail("render trace duration must equal the frozen comparison duration")
            for key in ("sampling_settings", "process_identity", "inspected_call_tree_filters"):
                require_nonempty(document[key], f"render performance record.{key}")
            if (
                not isinstance(document["screen_artifact_ids"], list)
                or not document["screen_artifact_ids"]
                or any(not isinstance(item, str) or not item for item in document["screen_artifact_ids"])
            ):
                fail("render performance record requires representative stack screenshots")
            if document["subject"] == "spaceterm":
                require_keys(
                    document,
                    (
                        "paint_text_shaping_stack_present",
                        "paint_path_or_plan_construction_present",
                        "paint_normal_frame_allocation_stack_present",
                        "cursor_or_blink_reshaped_unchanged_rows",
                        "changed_row_proportionality_result",
                        "exceptional_error_allocations_excluded",
                    ),
                    "SpaceTerm render performance record",
                )
                if document["status"] == "PASS" and (
                    document["paint_text_shaping_stack_present"] is not False
                    or document["paint_path_or_plan_construction_present"] is not False
                    or document["paint_normal_frame_allocation_stack_present"] is not False
                    or document["cursor_or_blink_reshaped_unchanged_rows"] is not False
                    or document["changed_row_proportionality_result"] != "PASS"
                    or document["exceptional_error_allocations_excluded"] is not True
                ):
                    fail("SpaceTerm render PASS contradicts a required render-path conclusion")
            else:
                require_nonempty(
                    document.get("reference_render_observation"),
                    "Ghostty reference_render_observation",
                )
        else:
            require_keys(
                document,
                (
                    "optimization_profile", "workload_command", "workload_input_sha256",
                    "duration_seconds", "warmup_seconds", "bytes_processed", "initial_grid",
                    "rss_samples_artifact_id", "rss_sample_interval_seconds",
                    "first_post_warmup_five_minutes", "final_five_minutes",
                    "allowed_range_delta_bytes", "memory_plateau_result",
                    "maximum_main_thread_stall_ms", "input_responsiveness_observation",
                    "ui_backlog_observation", "final_presentation_observation",
                    "shell_process_exit_observation", "time_profiler_artifact_id",
                    "allocations_artifact_id", "screen_artifact_ids",
                ),
                "workload performance record",
            )
            if document["rss_sample_interval_seconds"] != 10:
                fail("performance RSS sample interval must be 10 seconds")
            if (
                document["duration_seconds"] != comparison_inputs["duration_seconds"]
                or document["warmup_seconds"] != comparison_inputs["warmup_seconds"]
                or document["workload_input_sha256"] != comparison_inputs["workload_sha256"]
            ):
                fail("performance record fields disagree with frozen comparison inputs")
            if document["optimization_profile"] != "release":
                fail("performance evidence must use the optimized release profile")
            if not isinstance(document["bytes_processed"], int) or document["bytes_processed"] <= 0:
                fail("performance bytes_processed must be a positive integer")
            if case_id.startswith("perf-sustained-") and (
                document["duration_seconds"] < 600 or document["warmup_seconds"] < 60
            ):
                fail("sustained performance evidence is shorter than the required 10m/60s protocol")
            if case_id == "perf-resize" and document["duration_seconds"] < 300:
                fail("resize performance evidence is shorter than five minutes")
            windows = []
            for window_key in ("first_post_warmup_five_minutes", "final_five_minutes"):
                window = document[window_key]
                if not isinstance(window, dict):
                    fail(f"performance {window_key} must be an object")
                require_keys(window, ("minimum_bytes", "maximum_bytes", "range_bytes"),
                             f"performance {window_key}")
                if any(not isinstance(window[key], int) or window[key] < 0 for key in window):
                    fail(f"performance {window_key} values must be non-negative integers")
                if window["maximum_bytes"] < window["minimum_bytes"] or (
                    window["range_bytes"] != window["maximum_bytes"] - window["minimum_bytes"]
                ):
                    fail(f"performance {window_key} range calculation is invalid")
                windows.append(window)
            required_delta = max((windows[0]["range_bytes"] + 9) // 10, 64 * 1024 * 1024)
            if document["allowed_range_delta_bytes"] != required_delta:
                fail("performance allowed_range_delta_bytes is not the documented exact threshold")
            plateau_holds = abs(windows[1]["range_bytes"] - windows[0]["range_bytes"]) <= required_delta
            if document["memory_plateau_result"] not in {"PASS", "FAIL"}:
                fail("performance memory_plateau_result must be PASS or FAIL")
            if document["memory_plateau_result"] == "PASS" and not plateau_holds:
                fail("performance memory plateau PASS contradicts the recorded ranges")
            maximum_stall = document["maximum_main_thread_stall_ms"]
            if (
                not isinstance(maximum_stall, (int, float))
                or not math.isfinite(maximum_stall)
                or maximum_stall < 0
            ):
                fail("performance maximum_main_thread_stall_ms must be non-negative")
            if document["subject"] == "spaceterm" and document["status"] == "PASS" and (
                document["memory_plateau_result"] != "PASS" or maximum_stall > 250.0
            ):
                fail("SpaceTerm performance PASS contradicts the memory/stall gate")
            if case_id == "perf-resize":
                require_keys(
                    document,
                    (
                        "resize_count", "reflow_timings", "pty_geometry_samples",
                        "final_grid", "selection_anchoring", "viewport_anchoring",
                        "backing_scale_transition", "second_display_available",
                    ),
                    "resize performance record",
                )
                if not isinstance(document["resize_count"], int) or document["resize_count"] <= 0:
                    fail("resize performance resize_count must be positive")
    if expected_matrix == "native" and (
        document["status"] == "FAIL" or document["comparison_record_id"] is not None
    ):
        require_keys(document, ("comparison_inputs_sha256",), "failed native comparison record")
        if not HASH_RE.fullmatch(str(document["comparison_inputs_sha256"])):
            fail("failed native comparison_inputs_sha256 is invalid")
    privacy_scan(document, "case record")
    return document


def command_record(args: argparse.Namespace) -> None:
    root = resolve_root(args.run_id)
    with campaign_lock(root):
        state = require_initialized(root, args.run_id)
        record = validate_record(read_json(Path(args.input)), args.run_id)
        existing = load_records(state, args.run_id)
        chain = sorted(
            (
                item for item in existing.values()
                if item["case_id"] == record["case_id"] and item["subject"] == record["subject"]
            ),
            key=lambda item: item["attempt"],
        )
        expected_attempt = len(chain) + 1
        expected_supersedes = None if not chain else chain[-1]["record_id"]
        if record["attempt"] != expected_attempt or record["supersedes_record_id"] != expected_supersedes:
            fail("record registration must append the next exact attempt in its scope")
        if list((state / "record-reviews").glob("*.json")) \
                or list((state / "artifact-reviews").glob("*.json")):
            fail("new case records cannot be appended after manual review has begun")
        write_exclusive(
            state / "records" / f"{record['record_id']}.json",
            canonical_json(record),
            0o400,
        )


def load_records(state: Path, run_id: str) -> dict[str, dict[str, Any]]:
    records: dict[str, dict[str, Any]] = {}
    directory = state / "records"
    paths = sorted(directory.glob("*.json"))
    if len(paths) > 256:
        fail("private record inventory exceeds the bounded 256-attempt limit")
    for path in paths:
        ensure_regular_file(path, "private record")
        record = validate_record(read_json(path), run_id)
        if path.name != f"{record['record_id']}.json":
            fail(f"record filename disagrees with record_id: {path}")
        if record["record_id"] in records:
            fail(f"duplicate record_id: {record['record_id']}")
        records[record["record_id"]] = record
    return records


def validate_relative_payload(root: Path, relative: str, record: dict[str, Any]) -> tuple[Path, str]:
    if not isinstance(relative, str) or relative.startswith("/") or "\\" in relative:
        fail("artifact relative_path must be a POSIX relative path")
    pieces = Path(relative).parts
    if len(pieces) != 2 or any(piece in {"", ".", ".."} for piece in pieces):
        fail("artifact relative_path must be <matrix>/<artifact-name>")
    expected_dir = "supplementary" if record["matrix"] == "supplementary" else record["matrix"]
    allowed_dirs = {expected_dir}
    if record["case_id"] == "package-identity" and record["subject"] == "spaceterm":
        allowed_dirs.add("identity")
    if pieces[0] not in allowed_dirs or pieces[0] not in PAYLOAD_DIRS:
        fail("artifact directory disagrees with owning record matrix")
    match = ARTIFACT_FILE_RE.fullmatch(pieces[1])
    if match is None:
        fail("artifact filename violates the deterministic issue #43 naming contract")
    case_id, subject, attempt_text, kind, _extension = match.groups()
    if (
        case_id != record["case_id"]
        or subject != record["subject"]
        or int(attempt_text) != record["attempt"]
    ):
        fail("artifact filename disagrees with the owning record")
    path = root / relative
    try:
        resolved = path.resolve(strict=True)
    except FileNotFoundError:
        fail(f"artifact payload is missing: {relative}")
    if resolved.parent != (root / pieces[0]).resolve():
        fail("artifact path escapes its public payload directory")
    ensure_regular_file(path, "artifact payload")
    if path.stat().st_nlink != 1:
        fail("artifact payload must not be a hardlink")
    if pieces[1] in CONTROL_FILES:
        fail("control files cannot be payload artifacts")
    return path, kind


def parse_unique_tsv_bytes(payload: bytes, label: str) -> dict[str, str]:
    try:
        text = payload.decode("utf-8")
    except UnicodeDecodeError:
        fail(f"{label} is not UTF-8")
    result: dict[str, str] = {}
    for line_number, line in enumerate(text.splitlines(), start=1):
        fields = line.split("\t")
        if len(fields) != 2 or not fields[0] or fields[0] in result:
            fail(f"{label} contains invalid or duplicate TSV fields at line {line_number}")
        reject_forbidden_field_name(fields[0], f"{label} key at line {line_number}")
        privacy_scan(fields[0], f"{label} key at line {line_number}")
        privacy_scan(fields[1], f"{label} value at line {line_number}")
        result[fields[0]] = fields[1]
    return result


def parse_exact_ordered_tsv_bytes(
    payload: bytes, label: str, ordered_keys: tuple[str, ...],
) -> dict[str, str]:
    try:
        text = payload.decode("utf-8")
    except UnicodeDecodeError:
        fail(f"{label} is not UTF-8")
    lines = text.splitlines()
    if len(lines) != len(ordered_keys) or not text.endswith("\n"):
        fail(f"{label} does not contain the exact {len(ordered_keys)}-row schema")
    result: dict[str, str] = {}
    for index, (line, expected_key) in enumerate(zip(lines, ordered_keys), start=1):
        fields = line.split("\t")
        if len(fields) != 2 or fields[0] != expected_key or not fields[1]:
            fail(f"{label} row {index} is not exact field {expected_key}")
        reject_forbidden_field_name(fields[0], f"{label} key at line {index}")
        privacy_scan(fields[1], f"{label} value at line {index}")
        result[fields[0]] = fields[1]
    return result


def validate_native_ax_observation(path: Path, record: dict[str, Any]) -> None:
    payload = path.read_bytes()
    if not payload.endswith(b"\n") or byte_count(path) > 32 * 1024 * 1024:
        fail("native AX observation is not one bounded LF-terminated artifact")
    observed_keys = [line.split(b"\t", 1)[0] for line in payload[:-1].split(b"\n")]
    if observed_keys != sorted(observed_keys):
        fail("native AX observation keys are not in the probe's canonical sorted order")
    values = parse_unique_tsv_bytes(payload, "native AX observation")
    required = {
        "schema": "spaceterm.acceptance.native-ax-observation/v1",
        "run.id": RECORD_RE.fullmatch(record["record_id"]).group(1),  # type: ignore[union-attr]
        "subject.revalidated.before-query": "true",
        "subject.revalidated.after-observation": "true",
        "privacy.axvalue-content-emitted": "false",
        "pane.role": "AXTextArea",
        "pane.label.matches": "true",
        "notifications.clock": "mach-continuous",
        "notifications.target-identity": "pane-parent-and-same-pid-focus",
        "observation.complete": "true",
    }
    if any(values.get(key) != value for key, value in required.items()):
        fail("native AX observation does not bind the authenticated Pane subject")
    for key in (
        "probe.binary.sha256", "subject.package.app.sha256",
        "subject.launch.nonce.sha256", "subject.launch.observation.sha256",
        "subject.signature.identifier.sha256", "pane.label.sha256",
    ):
        if not HASH_RE.fullmatch(values.get(key, "")):
            fail(f"native AX observation has invalid {key}")
    if not re.fullmatch(r"[0-9a-f]+", values.get("subject.signature.cdhash", "")):
        fail("native AX observation has invalid live CDHash")
    if values.get("privacy.mode") not in {"metadata-only", "fixture-sentinel"}:
        fail("native AX observation privacy mode is invalid")
    if values["privacy.mode"] == "fixture-sentinel":
        if not HASH_RE.fullmatch(values.get("privacy.fixture-sha256", "")):
            fail("native AX fixture sentinel hash is invalid")
    elif values.get("privacy.fixture-sha256") != "none":
        fail("metadata-only AX observation claims fixture content")
    for prefix in ("before", "after"):
        for suffix in (
            "frame.x", "frame.y", "frame.width", "frame.height", "focused",
            "utf16-count", "visible-range", "selected-range", "cursor-empty",
            "value-queried", "selected-text-queried",
        ):
            if f"{prefix}.{suffix}" not in values:
                fail(f"native AX observation lacks {prefix}.{suffix}")
    selection_requested = values.get("selection.requested")
    if selection_requested == "true":
        if (
            values.get("subject.revalidated.before-mutation") != "true"
            or values.get("selection.generation-guard") != "pass"
            or values.get("selection.notification-causality")
            != "post-guard-subscription-dispatch"
            or not re.fullmatch(r"[0-9]+:[0-9]+", values.get("selection.requested-range", ""))
        ):
            fail("native AX Selection mutation lacks stale-target and causal notification proof")
    elif selection_requested != "false" or values.get("selection.generation-guard") != "not-applicable":
        fail("native AX Selection state is invalid")
    for kind in ("value", "selection", "focus", "focus-target", "focus-other", "layout"):
        count = canonical_uint64(values.get(f"notifications.{kind}.count", ""), f"AX {kind} count")
        first = canonical_uint64(
            values.get(f"notifications.{kind}.first-continuous-ns", ""), f"AX {kind} first"
        )
        last = canonical_uint64(
            values.get(f"notifications.{kind}.last-continuous-ns", ""), f"AX {kind} last"
        )
        if (count == 0 and (first != 0 or last != 0)) or (count > 0 and not 0 < first <= last):
            fail(f"native AX {kind} notification aggregate is inconsistent")
    if selection_requested == "true" and canonical_uint64(
        values["notifications.selection.count"], "AX Selection count"
    ) < 1:
        fail("native AX Selection mutation lacks its Pane-scoped notification")


def trace_tree_sha256_from_zip(path: Path) -> str:
    digest = hashlib.sha256(b"spaceterm.performance.trace-tree/v1\0")
    entries: dict[str, zipfile.ZipInfo] = {}
    root_name: str | None = None
    total = 0
    try:
        with zipfile.ZipFile(path) as archive:
            for info in archive.infolist():
                name = info.filename.rstrip("/")
                if not name or "\\" in name or "\0" in name:
                    fail("trace archive contains an unsafe member path")
                parts = Path(name).parts
                if any(part in {"", ".", ".."} for part in parts):
                    fail("trace archive contains an unsafe member path")
                root_name = parts[0] if root_name is None else root_name
                if parts[0] != root_name or not root_name.endswith(".trace"):
                    fail("trace archive does not contain exactly one .trace root")
                if any(part.endswith(".trace") for part in parts[1:]):
                    fail("trace archive contains a nested .trace root")
                mode = (info.external_attr >> 16) & 0xFFFF
                if stat.S_IFMT(mode) == stat.S_IFLNK or info.flag_bits & 0x1:
                    fail("trace archive contains a symbolic or encrypted member")
                if info.is_dir():
                    continue
                relative = Path(*parts[1:]).as_posix()
                if not relative or relative in entries or unicodedata.normalize("NFC", relative) != relative:
                    fail("trace archive contains duplicate or noncanonical nested members")
                privacy_scan(relative, "trace archive nested member path")
                total += info.file_size
                if total > MAX_CAMPAIGN_PAYLOAD_BYTES:
                    fail("trace archive uncompressed bytes exceed the campaign bound")
                entries[relative] = info
            if not entries:
                fail("trace archive contains no trace files")
            for relative in sorted(entries):
                encoded = relative.encode("utf-8")
                size = 0
                digest.update(struct.pack(">Q", len(encoded)))
                digest.update(encoded)
                digest.update(struct.pack(">Q", entries[relative].file_size))
                with archive.open(entries[relative]) as source:
                    while block := source.read(1024 * 1024):
                        digest.update(block)
                        size += len(block)
                if size != entries[relative].file_size:
                    fail("trace archive member changed or truncated during replay")
    except (OSError, zipfile.BadZipFile, zipfile.LargeZipFile):
        fail("trace archive is not a readable ZIP")
    return digest.hexdigest()


def validate_performance_protocol_pair(
    case_id: str,
    spaceterm: dict[str, Any],
    ghostty: dict[str, Any],
    artifacts: dict[str, dict[str, Any]],
    root: Path,
) -> None:
    def owned_by_kind(record: dict[str, Any]) -> dict[str, dict[str, Any]]:
        result: dict[str, dict[str, Any]] = {}
        for artifact_id in record["artifacts"]:
            artifact = artifacts[artifact_id]
            match = ARTIFACT_FILE_RE.fullmatch(Path(artifact["relative_path"]).name)
            assert match is not None
            kind = match.group(4)
            if kind in result:
                fail(f"performance protocol has duplicate {kind}: {record['record_id']}")
            result[kind] = artifact
        return result

    expected_scenario = {
        "perf-sustained-ascii": "ascii",
        "perf-sustained-unicode-styles": "unicode-styles",
        "perf-sustained-scrolled": "scrolled",
        "perf-sustained-hidden": "hidden-occluded",
        "perf-resize": "resize",
    }.get(case_id, case_id)
    closures: dict[str, dict[str, Any]] = {}
    required_kinds = {
        "run-intent", "run-metadata", "lifecycle-ready", "case-report",
        "trace-metadata", "trace-archive", "manual-artifacts",
        "manual-screenshot", "manual-video",
    }
    for record in (spaceterm, ghostty):
        subject = record["subject"]
        owned = owned_by_kind(record)
        missing = required_kinds - set(owned)
        if missing:
            fail(f"PASS performance closure is missing {sorted(missing)}: {record['record_id']}")
        paths = {kind: root / artifact["relative_path"] for kind, artifact in owned.items()}
        intent = parse_exact_ordered_tsv_bytes(
            paths["run-intent"].read_bytes(), f"{subject} run intent", RUN_INTENT_V1_KEYS,
        )
        run = parse_exact_ordered_tsv_bytes(
            paths["run-metadata"].read_bytes(), f"{subject} run metadata", RUN_METADATA_V4_KEYS,
        )
        ready = parse_exact_ordered_tsv_bytes(
            paths["lifecycle-ready"].read_bytes(), f"{subject} lifecycle ready receipt",
            LIFECYCLE_READY_V1_KEYS,
        )
        case = parse_exact_ordered_tsv_bytes(
            paths["case-report"].read_bytes(), f"{subject} case report", CASE_REPORT_V2_KEYS,
        )
        trace = parse_exact_ordered_tsv_bytes(
            paths["trace-metadata"].read_bytes(), f"{subject} trace metadata",
            TRACE_METADATA_V3_KEYS,
        )
        manual = parse_exact_ordered_tsv_bytes(
            paths["manual-artifacts"].read_bytes(), f"{subject} manual artifacts",
            MANUAL_ARTIFACTS_V1_KEYS,
        )
        tree_hash = trace_tree_sha256_from_zip(paths["trace-archive"])
        expected_identity = record["subject_identity_sha256"]
        expected_duration_ms = record["comparison_inputs"]["duration_seconds"] * 1000
        if (
            intent["format_version"] != "1"
            or intent["subject"] != subject
            or intent["subject_identity_sha256"] != expected_identity
            or intent["scenario"] != expected_scenario
            or intent["measured_duration_ms"] != str(expected_duration_ms)
            or intent["evidence_mode"] != "production"
            or intent["status"] != "prepared"
            or intent["workload_sha256"] != record["comparison_inputs"]["workload_sha256"]
            or run["format_version"] != "4"
            or run["subject"] != subject
            or run["subject_identity_sha256"] != expected_identity
            or run["scenario"] != expected_scenario
            or run["run_intent_sha256"] != sha256_path(paths["run-intent"])
            or run["evidence_mode"] != "production"
            or run["status"] != "complete"
        ):
            fail(f"{subject} exact19/exact35 production closure is invalid")
        for key in (
            "trace_provisional_receipt_sha256", "performance_tail_receipt_sha256",
            "performance_quit_receipt_sha256", "subject_exit_receipt_sha256",
            "lifecycle_ready_receipt_sha256", "lifecycle_registration_receipt_sha256",
            "lifecycle_helper_sha256", "terminator_source_sha256", "terminator_binary_sha256",
        ):
            if not HASH_RE.fullmatch(run[key]):
                fail(f"{subject} run metadata has invalid {key}")
        native_keys = (
            "native_observation_sha256", "native_runtime_metadata_sha256",
            "native_failure_actions_sha256", "native_failure_action_enabled",
            "native_failure_request_count", "native_failure_result_count",
            "native_failure_resource_staged_count", "native_failure_resource_staged_bytes",
            "native_failure_resource_rolled_back_count",
            "native_failure_resource_rolled_back_bytes",
        )
        if subject == "spaceterm":
            if any(not HASH_RE.fullmatch(run[key]) for key in native_keys[:3]) \
                    or run["native_failure_action_enabled"] != "false" \
                    or any(run[key] != "0" for key in native_keys[4:]):
                fail("SpaceTerm run metadata native failure closure is invalid")
        elif any(run[key] != "not-applicable" for key in native_keys):
            fail("Ghostty run metadata claims SpaceTerm native evidence")
        if (
            ready["schema"] != "spaceterm.acceptance.performance-lifecycle-ready/v1"
            or ready["subject"] != subject
            or ready["subject_identity_sha256"] != expected_identity
            or ready["lifecycle_helper_sha256"] != run["lifecycle_helper_sha256"]
            or ready["evidence_mode"] != "production"
            or ready["auth_algorithm"] != "hmac-sha256"
            or not HASH_RE.fullmatch(ready["receipt_hmac_sha256"])
            or ready["status"] != "ready"
            or sha256_path(paths["lifecycle-ready"]) != run["lifecycle_ready_receipt_sha256"]
        ):
            fail(f"{subject} lifecycle receipt does not bind exact35")
        if (
            case["format_version"] != "2"
            or case["subject"] != subject
            or case["scenario"] != expected_scenario
            or case["session_id"] != intent["session_id"]
            or case["nonce"] != intent["nonce"]
            or case["run_intent_sha256"] != sha256_path(paths["run-intent"])
            or case["run_metadata_sha256"] != sha256_path(paths["run-metadata"])
            or case["trace_metadata_sha256"] != sha256_path(paths["trace-metadata"])
            or case["trace_archive_sha256"] != tree_hash
            or case["manual_artifacts_sha256"] != sha256_path(paths["manual-artifacts"])
            or case["manual_screenshot_sha256"] != sha256_path(paths["manual-screenshot"])
            or case["manual_video_sha256"] != sha256_path(paths["manual-video"])
            or case["result"] != "CASE-COMPLETE"
            or case["reason"] != "all-required-evidence-complete"
        ):
            fail(f"{subject} exact14 case report does not bind current evidence")
        if (
            trace["format_version"] != "3"
            or trace["capture_status"] != "CAPTURED"
            or trace["subject_identity_sha256"] != expected_identity
            or trace["run_metadata_sha256"] != sha256_path(paths["run-metadata"])
            or trace["status"] != "complete"
            or manual["format_version"] != "1"
            or manual["screenshot_sha256"] != sha256_path(paths["manual-screenshot"])
            or manual["video_sha256"] != sha256_path(paths["manual-video"])
            or manual["result"] != "PASS"
        ):
            fail(f"{subject} trace/manual closure is not complete")
        if case_id in RENDER_CASES:
            render = owned.get("render-manual-review")
            if render is None:
                fail(f"render performance PASS lacks receipt-bound manual review: {record['record_id']}")
            render_values = parse_unique_tsv_bytes(
                (root / render["relative_path"]).read_bytes(), "render manual review",
            )
            for key in (
                "run_metadata_sha256", "render_intent_sha256", "render_evidence_sha256",
                "trace_anchor_receipt_sha256", "trace_receipt_sha256",
                "trace_metadata_sha256", "trace_artifact_sha256",
            ):
                if not HASH_RE.fullmatch(render_values.get(key, "")):
                    fail(f"render manual review lacks receipt binding {key}")
            if (
                render_values.get("format_version") != "1"
                or render_values.get("scenario") != case_id
                or render_values.get("subject") != subject
                or render_values["run_metadata_sha256"] != sha256_path(paths["run-metadata"])
                or render_values["trace_metadata_sha256"] != sha256_path(paths["trace-metadata"])
                or render_values["trace_artifact_sha256"] != sha256_path(paths["trace-archive"])
                or render_values.get("result") != "PASS"
            ):
                fail("render receipt/manual projection does not bind current evidence")
        closures[subject] = {
            "record": record, "owned": owned, "paths": paths, "intent": intent,
            "run": run, "case": case, "trace_tree": tree_hash,
        }
    space_owned = closures["spaceterm"]["owned"]
    pair_artifact = space_owned.get("pair-result")
    pair_metadata_artifact = space_owned.get("pair-metadata")
    if pair_artifact is None or pair_metadata_artifact is None:
        fail("performance pair lacks its SpaceTerm-owned exact12 metadata or exact62 result")
    pair_path = root / pair_artifact["relative_path"]
    pair_metadata_path = root / pair_metadata_artifact["relative_path"]
    pair_metadata = parse_exact_ordered_tsv_bytes(
        pair_metadata_path.read_bytes(), "performance pair metadata", PAIR_METADATA_V1_KEYS,
    )
    pair = parse_exact_ordered_tsv_bytes(
        pair_path.read_bytes(), "performance pair result", PAIR_RESULT_V3_KEYS,
    )
    if (
        pair["format_version"] != "3"
        or pair["evidence_mode"] != "production"
        or pair["status"] != "complete"
        or pair["auth_algorithm"] != "hmac-sha256"
        or not HASH_RE.fullmatch(pair["pair_result_hmac_sha256"])
    ):
        fail("exact62 pair result is not authenticated production evidence")
    if (
        pair_metadata["format_version"] != "1"
        or pair_metadata["scenario"] != expected_scenario
        or pair["pair_metadata_sha256"] != sha256_path(pair_metadata_path)
        or pair["scenario_plan_sha256"] != pair_metadata["plan_sha256"]
        or pair["workload_sha256"] != pair_metadata["workload_sha256"]
        or pair["command_sha256"] != pair_metadata["command_sha256"]
        or pair["environment_sha256"] != pair_metadata["environment_sha256"]
        or pair["font_sha256"] != pair_metadata["font_sha256"]
        or pair["initial_grid_sha256"] != pair_metadata["initial_grid_sha256"]
        or pair_metadata["duration_ms"]
            != str(spaceterm["comparison_inputs"]["duration_seconds"] * 1000)
        or pair_metadata["spaceterm_subject_identity_sha256"]
            != spaceterm["subject_identity_sha256"]
        or pair_metadata["ghostty_subject_identity_sha256"]
            != ghostty["subject_identity_sha256"]
    ):
        fail("exact62 pair result does not bind exact12 pair metadata")
    for subject in ("spaceterm", "ghostty"):
        closure = closures[subject]
        paths = closure["paths"]
        run = closure["run"]
        case = closure["case"]
        if (
            pair[f"{subject}_session_id"] != closure["intent"]["session_id"]
            or pair[f"{subject}_nonce"] != closure["intent"]["nonce"]
            or pair[f"{subject}_run_intent_sha256"] != sha256_path(paths["run-intent"])
            or pair[f"{subject}_run_metadata_sha256"] != sha256_path(paths["run-metadata"])
            or pair[f"{subject}_case_report_sha256"] != sha256_path(paths["case-report"])
            or pair[f"{subject}_trace_metadata_sha256"] != sha256_path(paths["trace-metadata"])
            or pair[f"{subject}_trace_archive_sha256"] != closure["trace_tree"]
            or pair[f"{subject}_manual_artifacts_sha256"]
                != sha256_path(paths["manual-artifacts"])
            or pair[f"{subject}_manual_screenshot_sha256"]
                != sha256_path(paths["manual-screenshot"])
            or pair[f"{subject}_manual_video_sha256"] != sha256_path(paths["manual-video"])
            or pair[f"{subject}_lifecycle_ready_receipt_sha256"]
                != run["lifecycle_ready_receipt_sha256"]
            or pair[f"{subject}_lifecycle_registration_receipt_sha256"]
                != run["lifecycle_registration_receipt_sha256"]
            or case["run_metadata_sha256"] != pair[f"{subject}_run_metadata_sha256"]
        ):
            fail(f"exact62 pair result does not bind {subject} closure")
    if (
        pair["lifecycle_helper_sha256"] != closures["spaceterm"]["run"]["lifecycle_helper_sha256"]
        or pair["lifecycle_helper_sha256"] != closures["ghostty"]["run"]["lifecycle_helper_sha256"]
        or pair["terminator_source_sha256"] != closures["spaceterm"]["run"]["terminator_source_sha256"]
        or pair["terminator_binary_sha256"] != closures["spaceterm"]["run"]["terminator_binary_sha256"]
    ):
        fail("exact62 pair result lifecycle toolchain binding is inconsistent")


def parse_encoded_exact_tsv(
    path: Path, label: str, keys: tuple[str, ...],
) -> tuple[dict[str, str], dict[str, str]]:
    ensure_regular_file(path, label)
    if byte_count(path) > 32 * 1024 * 1024:
        fail(f"{label} exceeds its bounded size")
    try:
        text = path.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as error:
        fail(f"{label} is not readable UTF-8: {error}")
    lines = text.splitlines()
    if len(lines) != len(keys) or not text.endswith("\n"):
        fail(f"{label} does not contain the exact {len(keys)}-record schema")
    result: dict[str, str] = {}
    encoded: dict[str, str] = {}
    for index, (line, expected_key) in enumerate(zip(lines, keys), start=1):
        fields = line.split("\t")
        if len(fields) != 2 or fields[0] != expected_key:
            fail(f"{label} row {index} is not exact field {expected_key}")
        if re.search(r"%(?!25|09|0D|0A)", fields[1]):
            fail(f"{label} contains an unsupported or noncanonical percent escape")
        encoded[expected_key] = fields[1]
        result[expected_key] = percent_decode_manifest(fields[1])
    return result, encoded


def canonical_uint64(value: str, label: str) -> int:
    if not re.fullmatch(r"0|[1-9][0-9]*", value):
        fail(f"{label} is not a canonical unsigned decimal")
    parsed = int(value)
    if parsed > UINT64_MAX:
        fail(f"{label} exceeds uint64")
    return parsed


def positive_manifest_number(value: str, label: str) -> None:
    if not re.fullmatch(r"[0-9]+(?:[.][0-9]+)?", value):
        fail(f"{label} is not the frozen positive-number syntax")
    parsed = float(value)
    if not math.isfinite(parsed) or parsed <= 0:
        fail(f"{label} is not a finite positive number")


def parse_exact_runtime_rows(
    path: Path, label: str, header: tuple[str, ...], maximum_bytes: int, maximum_rows: int,
) -> list[list[str]]:
    ensure_regular_file(path, label)
    if byte_count(path) > maximum_bytes:
        fail(f"{label} exceeds its exact byte bound")
    try:
        text = path.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as error:
        fail(f"{label} is not readable UTF-8: {error}")
    if not text.endswith("\n") or "\r" in text:
        fail(f"{label} is not canonical LF-terminated TSV")
    rows = [line.split("\t") for line in text[:-1].split("\n")]
    if not rows or tuple(rows[0]) != header or len(rows) - 1 > maximum_rows:
        fail(f"{label} has an invalid exact header or row bound")
    if any(len(row) != len(header) for row in rows[1:]):
        fail(f"{label} contains a row outside its exact field inventory")
    return rows[1:]


def validate_runtime_stream_exports(
    samples_path: Path, events_path: Path, runtime: dict[str, str],
) -> None:
    sample_rows = parse_exact_runtime_rows(
        samples_path, "runtime samples v1", RUNTIME_SAMPLES_V1_HEADER,
        32 * 1024 * 1024, 43201,
    )
    event_rows = parse_exact_runtime_rows(
        events_path, "runtime events v1", RUNTIME_EVENTS_V1_HEADER,
        16 * 1024 * 1024, 65536,
    )
    if not sample_rows:
        fail("runtime samples v1 is empty")
    if str(len(sample_rows)) != runtime["observer.sample_count"] \
            or str(len(event_rows)) != runtime["observer.event_count"]:
        fail("runtime stream row counts disagree with metadata")

    lifecycles = ("starting", "running", "exited", "failed", "observer-failed")
    lifecycle_codes = {name: index for index, name in enumerate(lifecycles)}
    monotonic_fields = (1, 2, 3, 4, 6, 7, 8, 9, 10, 11, 12, 13, 24, 25, 26, 27, 32, 34)
    previous: list[int] | None = None
    parsed_samples: list[list[int]] = []
    for expected_sequence, row in enumerate(sample_rows):
        if canonical_uint64(row[0], "runtime sample sequence") != expected_sequence:
            fail("runtime sample sequence is not contiguous from zero")
        current: list[int] = []
        for field_index, value in enumerate(row[1:]):
            if field_index == 33:
                if value not in lifecycle_codes:
                    fail("runtime sample lifecycle is outside the exact vocabulary")
                current.append(lifecycle_codes[value])
            elif 14 <= field_index <= 19 or field_index == 23:
                parsed = canonical_uint64(value, RUNTIME_SAMPLES_V1_HEADER[field_index + 1])
                if parsed > 1:
                    fail("runtime sample boolean field is not 0 or 1")
                current.append(parsed)
            else:
                current.append(canonical_uint64(
                    value, RUNTIME_SAMPLES_V1_HEADER[field_index + 1]
                ))
        if current[0] == 0 or current[5] > 2 or current[6] > 2 or current[9] > 2 \
                or current[5] > current[6] or current[21] > current[20] \
                or current[22] > current[20] - current[21]:
            fail("runtime sample contains an impossible bounded queue or viewport state")
        if previous is not None:
            if current[0] < previous[0]:
                fail("runtime sample continuous time regressed")
            interval = current[0] - previous[0]
            if interval < 750_000_000 or interval > 1_250_000_000:
                fail("runtime periodic sample cadence is outside 750-1250 ms")
            before, after = previous[33], current[33]
            lifecycle_valid = (
                before == 0
                or (before == 1 and after in (1, 2, 3, 4))
                or (before == 2 and after in (2, 4))
                or (before == 3 and after in (3, 4))
                or (before == 4 and after == 4)
            )
            if not lifecycle_valid:
                fail("runtime lifecycle transition is invalid")
            if any(current[index] < previous[index] for index in monotonic_fields):
                fail("runtime monotonic counter regressed")
        parsed_samples.append(current)
        previous = current

    first, final = parsed_samples[0], parsed_samples[-1]
    if canonical_uint64(runtime["observer.started_continuous_ns"], "observer start") != first[0] \
            or canonical_uint64(runtime["observer.ended_continuous_ns"], "observer end") != final[0]:
        fail("runtime metadata start/end does not bind the exact sample stream")
    if final[33] not in (2, 3) or any(sample[33] == 4 or sample[34] != 0 for sample in parsed_samples):
        fail("complete runtime stream contains observer failure or is not terminal")
    if (
        final[3] > final[2] or final[4] > final[3] or final[8] > final[3]
        or final[10] > final[1] or final[11] > final[10] or final[12] > final[11]
        or final[12] < final[1] or final[25] > final[24] or final[26] > final[25]
        or final[27] > final[24]
    ):
        fail("complete runtime stream final counters are inconsistent")

    allowed_event_kinds = {
        "visibility-lost", "visibility-restored", "first-next-frame-after-restore",
        "session-exited", "session-failed", "observer-failed",
    }
    previous_time = 0
    sample_cursor = 0
    events_per_sample = [0] * len(parsed_samples)
    for expected_sequence, row in enumerate(event_rows):
        if canonical_uint64(row[0], "runtime event sequence") != expected_sequence:
            fail("runtime event sequence is not contiguous from zero")
        continuous = canonical_uint64(row[1], "runtime event continuous_ns")
        generation = canonical_uint64(row[3], "runtime event generation")
        aux0 = canonical_uint64(row[4], "runtime event aux0")
        aux1 = canonical_uint64(row[5], "runtime event aux1")
        if continuous == 0 or continuous < previous_time or row[2] not in allowed_event_kinds:
            fail("runtime event time or kind is invalid")
        if continuous < first[0] or continuous > final[0]:
            fail("runtime event falls outside the exact observer sample interval")
        if row[2] == "session-exited":
            aux_valid = 1 <= aux0 <= 5 and aux1 == 0
        elif row[2] == "session-failed":
            aux_valid = 1 <= aux0 <= 7 and aux1 == 0
        else:
            aux_valid = aux0 == 0 and aux1 == 0
        if not aux_valid or row[2] == "observer-failed":
            fail("runtime event auxiliary values or completion state are invalid")
        while sample_cursor < len(parsed_samples) and parsed_samples[sample_cursor][0] < continuous:
            sample_cursor += 1
        if sample_cursor == len(parsed_samples) or generation > parsed_samples[sample_cursor][1]:
            fail("runtime event is not bounded by its containing sample")
        events_per_sample[sample_cursor] += 1
        if events_per_sample[sample_cursor] > 64:
            fail("runtime tick contains more than 64 transition events")
        previous_time = continuous


def failure_state_matches(case_id: str, action: str, row: list[str]) -> bool:
    failure_class, recoverability, operation, pending = row[7], row[8], row[9], row[14]
    if action == "armed" or (
        action == "completed" and (case_id == "normal-exit-control" or not case_id.endswith("-fatal"))
    ):
        return (failure_class, recoverability, operation, pending) == (
            "none", "none", "none", "none",
        )
    expected = {
        "presentation-invalid-scale": (
            "presentation", "recoverable", "update-backing-scale", "presentation",
        ),
        "presentation-glyph": (
            "presentation", "recoverable", "paint-terminal-presentation", "presentation",
        ),
        "renderer-image-preflight": (
            "resource", "recoverable", "paint-terminal-graphics", "renderer-resources",
        ),
        "renderer-resource-before-sync": (
            "resource", "recoverable", "prepare-terminal-graphics", "renderer-resources",
        ),
        "renderer-resource-after-staging": (
            "resource", "recoverable", "prepare-terminal-graphics", "renderer-resources",
        ),
        "pasteboard-write": (
            "platform", "recoverable", "write-selection-pasteboard", "copy-selection",
        ),
        "pty-fatal": ("pty", "fatal", "read-shell-output", "none"),
        "emulator-fatal": ("emulator", "fatal", "session-runtime", "none"),
    }.get(case_id)
    return expected is not None and (failure_class, recoverability, operation, pending) == expected


def validate_failure_action_rows(rows: list[list[str]], enabled: str) -> tuple[int, int]:
    cases = {
        "presentation-invalid-scale", "presentation-glyph", "renderer-image-preflight",
        "renderer-resource-before-sync", "renderer-resource-after-staging", "pasteboard-write",
        "pty-fatal", "emulator-fatal", "normal-exit-control",
    }
    vocabularies = {
        3: {"armed", "injected", "retry-requested", "completed"},
        4: {"accepted", "failed-state", "recovered", "closed", "exited"},
        6: {"running", "failed", "exited"},
        7: {"pty", "emulator", "presentation", "platform", "resource", "none"},
        8: {"recoverable", "fatal", "none"},
        9: {
            "read-shell-output", "session-runtime", "update-backing-scale",
            "paint-terminal-presentation", "paint-terminal-graphics",
            "prepare-terminal-graphics", "write-selection-pasteboard", "none",
        },
        14: {"presentation", "renderer-resources", "copy-selection", "none"},
    }
    seen: set[str] = set()
    request_count = 0
    index = 0
    while index < len(rows):
        row = rows[index]
        request_id, sequence, case_id = row[0], row[1], row[2]
        if not HASH_RE.fullmatch(request_id) or request_id in seen \
                or canonical_uint64(sequence, "failure sequence") != request_count \
                or case_id not in cases:
            fail("failure action request identity/order/case is invalid")
        seen.add(request_id)
        pane = canonical_uint64(row[5], "failure pane_id")
        revision = canonical_uint64(row[10], "failure state_revision")
        latest = canonical_uint64(row[11], "failure latest_generation")
        last_valid = canonical_uint64(row[12], "failure last_valid_generation")
        visible = None if row[13] == "unavailable" else canonical_uint64(
            row[13], "failure visible_generation"
        )
        if latest < last_valid or (visible is not None and visible > latest):
            fail("failure action generation ordering is invalid")
        for field, allowed in vocabularies.items():
            if row[field] not in allowed:
                fail("failure action contains a value outside its exact vocabulary")
        if row[15] not in {"0", "1"} or row[16] not in {"0", "1"}:
            fail("failure action boolean field is invalid")
        if not failure_state_matches(case_id, row[3], row):
            fail("failure action class/recovery state does not match its case and phase")

        resource = [canonical_uint64(row[field], "failure resource metric") for field in range(17, 21)]
        if resource[0] > 65536 or resource[2] > 65536 \
                or resource[1] > 402653184 or resource[3] > 402653184:
            fail("failure action resource metrics exceed their exact bounds")
        if any((row[3] != "armed", row[4] != "accepted", row[6] != "running", row[16] != "1")):
            fail("failure action group does not start with armed/accepted/running/attached")
        if any(resource):
            fail("armed failure action fabricates staged resources")

        group = [row]
        index += 1
        while index < len(rows) and rows[index][0] == request_id:
            group.append(rows[index])
            index += 1
        expected_length = 2 if case_id == "normal-exit-control" else (3 if case_id.endswith("-fatal") else 4)
        if len(group) != expected_length:
            fail("failure action group has an incomplete or extra phase")

        injected: tuple[int, int, int | None] | None = None
        injected_resource: tuple[int, int] | None = None
        for phase, item in enumerate(group[1:], start=1):
            if item[0] != request_id or item[1] != sequence or item[2] != case_id \
                    or canonical_uint64(item[5], "failure pane_id") != pane:
                fail("failure action group changed request, sequence, case, or Pane")
            new_revision = canonical_uint64(item[10], "failure state_revision")
            if new_revision < revision:
                fail("failure action state revision regressed")
            revision = new_revision
            for field, allowed in vocabularies.items():
                if item[field] not in allowed:
                    fail("failure action contains a value outside its exact vocabulary")
            if item[15] not in {"0", "1"} or item[16] not in {"0", "1"} \
                    or not failure_state_matches(case_id, item[3], item):
                fail("failure action phase state is invalid")
            current_latest = canonical_uint64(item[11], "failure latest_generation")
            current_last = canonical_uint64(item[12], "failure last_valid_generation")
            current_visible = None if item[13] == "unavailable" else canonical_uint64(
                item[13], "failure visible_generation"
            )
            if current_latest < current_last \
                    or (current_visible is not None and current_visible > current_latest):
                fail("failure action generation ordering is invalid")
            current_resource = [
                canonical_uint64(item[field], "failure resource metric") for field in range(17, 21)
            ]
            if current_resource[0] > 65536 or current_resource[2] > 65536 \
                    or current_resource[1] > 402653184 or current_resource[3] > 402653184:
                fail("failure action resource metrics exceed their exact bounds")
            after_staging = case_id == "renderer-resource-after-staging"
            if after_staging:
                if current_resource[0] == 0 or current_resource[1] == 0 \
                        or current_resource[2:] != current_resource[:2]:
                    fail("after-staging failure lacks exact positive rollback proof")
                if injected_resource is None:
                    injected_resource = (current_resource[0], current_resource[1])
                elif injected_resource != (current_resource[0], current_resource[1]):
                    fail("after-staging resource proof changed across phases")
            elif any(current_resource):
                fail("non-after-staging failure fabricates staged resources")

            if case_id == "normal-exit-control":
                valid_phase = phase == 1 and item[3:5] == ["completed", "exited"] \
                    and item[6] == "exited" and item[16] == "1"
            elif phase == 1:
                fatal = case_id.endswith("-fatal")
                valid_phase = item[3:5] == ["injected", "failed-state"] \
                    and item[6] == "failed" and item[16] == "1" \
                    and (not fatal or item[15] == "0") \
                    and (case_id != "pasteboard-write" or item[15] == "1") \
                    and (fatal or (current_visible is not None and current_visible == current_last))
                injected = (current_latest, current_last, current_visible)
            elif case_id.endswith("-fatal"):
                valid_phase = phase == 2 and item[3:5] == ["completed", "closed"] \
                    and item[6] == "failed" and item[15] == "0" and item[16] == "0"
            elif phase == 2:
                assert injected is not None
                if case_id == "pasteboard-write":
                    generation_valid = current_visible is not None \
                        and current_latest >= injected[0] and current_last >= injected[1] \
                        and injected[2] is not None and current_visible >= injected[2] \
                        and current_visible == current_last and item[15] == "1"
                    injected = (current_latest, current_last, current_visible)
                else:
                    generation_valid = (current_latest, current_last, current_visible) == injected
                valid_phase = item[3:5] == ["retry-requested", "accepted"] \
                    and item[6] == "failed" and item[16] == "1" and generation_valid
            else:
                assert phase == 3 and injected is not None
                if case_id == "pasteboard-write":
                    generation_valid = current_visible is not None \
                        and current_latest >= injected[0] and current_last >= injected[1] \
                        and current_last <= current_latest and injected[2] is not None \
                        and current_visible >= injected[2] and current_visible == current_last \
                        and item[15] == "1"
                else:
                    generation_valid = current_latest == injected[0] \
                        and current_last == current_latest and current_visible == current_latest
                valid_phase = item[3:5] == ["completed", "recovered"] \
                    and item[6] == "running" and item[16] == "1" and generation_valid
            if not valid_phase:
                fail("failure action grouped phase transition is invalid")
        request_count += 1
    if enabled == "false" and rows:
        fail("disabled native failure control contains result rows")
    return request_count, len(rows)


def validate_published_runtime_failure_closure(
    runtime_path: Path, samples_path: Path, events_path: Path, failure_path: Path,
    identity: dict[str, str], run_id: str, app_sha256: str,
) -> dict[str, str]:
    """Replay all frozen b742 runtime/NF21 semantics expressible by published exports."""
    runtime, _runtime_encoded = parse_encoded_exact_tsv(
        runtime_path, "runtime observation metadata v3", RUNTIME_METADATA_V3_KEYS
    )
    if (
        runtime["schema"] != "spaceterm.acceptance.runtime-observation-metadata/v3"
        or runtime["observation.source"] != "production-app"
        or runtime["run.id"] != run_id
        or runtime["package.app.sha256"] != app_sha256
        or canonical_uint64(runtime["process.pid"], "runtime process.pid") == 0
        or runtime["runtime.samples.path"] != "runtime-samples.tsv"
        or runtime["runtime.samples.sha256"] != sha256_path(samples_path)
        or runtime["runtime.events.path"] != "runtime-events.tsv"
        or runtime["runtime.events.sha256"] != sha256_path(events_path)
        or runtime["failure.action.schema"] != "spaceterm.acceptance.failure-action/v1"
        or runtime["failure.action.enabled"] not in {"true", "false"}
        or runtime["failure.result.schema"]
            != "spaceterm.acceptance.failure-action-result/v2"
        or runtime["failure.actions.path"] != "failure-actions.tsv"
        or runtime["failure.actions.sha256"] != sha256_path(failure_path)
        or runtime["observer.sample_interval_ms"] != "1000"
        or runtime["observer.transition_capacity"] != "64"
        or runtime["observer.status"] != "complete"
        or runtime["observation.complete"] != "true"
    ):
        fail("published runtime observation metadata v3 closure is invalid")
    counters = {
        "sample": canonical_uint64(runtime["observer.sample_count"], "observer sample_count"),
        "event": canonical_uint64(runtime["observer.event_count"], "observer event_count"),
        "request": canonical_uint64(runtime["failure.request_count"], "failure request_count"),
        "result": canonical_uint64(runtime["failure.result_count"], "failure result_count"),
    }
    if counters["sample"] == 0 or counters["sample"] > 43201 \
            or counters["event"] > 65536 or counters["request"] > 64 \
            or counters["result"] > 256:
        fail("published runtime observation counters exceed their exact bounds")
    if canonical_uint64(runtime["observer.started_continuous_ns"], "observer start") == 0 \
            or canonical_uint64(runtime["observer.ended_continuous_ns"], "observer end") == 0:
        fail("published runtime observer start/end must be positive")
    validate_runtime_stream_exports(samples_path, events_path, runtime)
    rows = parse_exact_runtime_rows(
        failure_path, "failure action result v2", FAILURE_ACTION_V2_HEADER,
        256 * 1024, 256,
    )
    request_count, result_count = validate_failure_action_rows(
        rows, runtime["failure.action.enabled"]
    )
    if request_count != counters["request"] or result_count != counters["result"]:
        fail("published failure action grouped counts disagree with runtime metadata")
    identity_expected = {
        "schema": "spaceterm.acceptance.run-identity-public/v2",
        "run.id": run_id,
        "package.app.sha256": app_sha256,
        "native.runtime.metadata.schema": runtime["schema"],
        "native.runtime.metadata.path": "identity/runtime-metadata.tsv",
        "native.runtime.metadata.sha256": sha256_path(runtime_path),
        "native.failure.action.enabled": runtime["failure.action.enabled"],
        "native.failure.result.schema": runtime["failure.result.schema"],
        "native.failure.actions.path": "identity/failure-actions.tsv",
        "native.failure.actions.sha256": sha256_path(failure_path),
        "native.failure.request_count": runtime["failure.request_count"],
        "native.failure.result_count": runtime["failure.result_count"],
    }
    for key, expected in identity_expected.items():
        if identity.get(key) != expected:
            fail(f"public run identity v2 does not bind runtime/failure closure: {key}")
    return runtime


def validate_native_failure_closure(
    native_path: Path, runtime_path: Path, samples_path: Path, events_path: Path,
    failure_path: Path, identity: dict[str, str], metadata: dict[str, Any],
) -> None:
    native, native_encoded = parse_encoded_exact_tsv(
        native_path, "native launch proof v5", NATIVE_FINAL_V5_KEYS
    )
    provisional_bytes = b"".join(
        f"{key}\t{native_encoded[key]}\n".encode("utf-8")
        for key in NATIVE_PROVISIONAL_V5_KEYS[:-1]
    ) + b"observation.complete\ttrue\n"
    provisional_hash = hashlib.sha256(provisional_bytes).hexdigest()
    if (
        native["schema"] != "spaceterm.acceptance.native-launch-proof/v5"
        or native["observation.source"] != "production-app"
        or native["observation.complete"] != "true"
        or not HASH_RE.fullmatch(native["launch.nonce"])
        or native["provisional.observation.sha256"] != provisional_hash
        or native["run.id"] != metadata["run_id"]
        or native["package.app.sha256"] != metadata["frozen_artifact"]["app_bundle_sha256"]
        or native["runtime.schema"] != "spaceterm.acceptance.runtime-stream/v1"
        or native["runtime.sample_interval_ms"] != "1000"
        or native["runtime.transition_capacity"] != "64"
        or native["failure.action.schema"] != "spaceterm.acceptance.failure-action/v1"
        or native["failure.action.enabled"] not in {"true", "false"}
    ):
        fail("native launch proof v5 identity/provisional closure is invalid")
    for key in (
        "process.pid", "process.pidversion", "process.executable.device",
        "process.executable.inode", "initial_grid.rows", "initial_grid.columns",
        "initial_grid.backing_pixel_width", "initial_grid.backing_pixel_height",
    ):
        if canonical_uint64(native[key], f"native {key}") == 0:
            fail(f"native {key} must be positive")
    if not native["process.executable.path"].startswith("/") \
            or not re.fullmatch(r"-?[0-9]+:-?[0-9]+", native["process.executable.fsid"]) \
            or not re.fullmatch(r"[0-9A-Fa-f]+", native["process.signature.cdhash"]) \
            or not native["process.signature.identifier"] \
            or not native["terminal_font_selected"] \
            or len(native["terminal_font_selected"]) > 256:
        fail("native process/signature/font identity is invalid")
    positive_manifest_number(native["initial_grid.logical_width"], "native logical width")
    positive_manifest_number(native["initial_grid.logical_height"], "native logical height")
    runtime, _runtime_encoded = parse_encoded_exact_tsv(
        runtime_path, "runtime observation metadata v3", RUNTIME_METADATA_V3_KEYS
    )
    expected_native = {
        "runtime.metadata.schema": "spaceterm.acceptance.runtime-observation-metadata/v3",
        "runtime.metadata.path": "runtime-metadata.tsv",
        "runtime.metadata.sha256": sha256_path(runtime_path),
        "failure.result.schema": "spaceterm.acceptance.failure-action-result/v2",
        "failure.actions.path": "failure-actions.tsv",
        "failure.actions.sha256": sha256_path(failure_path),
    }
    if any(native[key] != value for key, value in expected_native.items()):
        fail("native launch proof v5 runtime/failure artifact hashes are stale")
    if (
        runtime["schema"] != expected_native["runtime.metadata.schema"]
        or runtime["observation.source"] != "production-app"
        or runtime["run.id"] != native["run.id"]
        or runtime["package.app.sha256"] != native["package.app.sha256"]
        or runtime["process.pid"] != native["process.pid"]
        or runtime["runtime.samples.path"] != "runtime-samples.tsv"
        or runtime["runtime.samples.sha256"] != sha256_path(samples_path)
        or runtime["runtime.events.path"] != "runtime-events.tsv"
        or runtime["runtime.events.sha256"] != sha256_path(events_path)
        or runtime["failure.action.schema"] != "spaceterm.acceptance.failure-action/v1"
        or runtime["failure.action.enabled"] not in {"true", "false"}
        or runtime["failure.result.schema"] != expected_native["failure.result.schema"]
        or runtime["failure.actions.path"] != "failure-actions.tsv"
        or runtime["failure.actions.sha256"] != sha256_path(failure_path)
        or runtime["observer.sample_interval_ms"] != "1000"
        or runtime["observer.transition_capacity"] != "64"
        or runtime["observer.status"] != "complete"
        or runtime["observation.complete"] != "true"
    ):
        fail("runtime observation metadata v3 closure is invalid")
    for key in (
        "failure.action.enabled", "failure.result.schema", "failure.request_count",
        "failure.result_count",
    ):
        native_key = key
        if native[native_key] != runtime[key]:
            fail(f"native/runtime failure closure disagrees: {key}")
    sample_count = canonical_uint64(runtime["observer.sample_count"], "observer sample_count")
    event_count = canonical_uint64(runtime["observer.event_count"], "observer event_count")
    request_count = canonical_uint64(runtime["failure.request_count"], "failure request_count")
    result_count = canonical_uint64(runtime["failure.result_count"], "failure result_count")
    if sample_count == 0 or sample_count > 43201 or event_count > 65536 \
            or request_count > 64 or result_count > 256:
        fail("runtime observation counters exceed their exact bounds")
    if canonical_uint64(runtime["observer.started_continuous_ns"], "observer start") == 0 \
            or canonical_uint64(runtime["observer.ended_continuous_ns"], "observer end") == 0:
        fail("runtime observer start/end must be positive")
    validate_runtime_stream_exports(samples_path, events_path, runtime)

    rows = parse_exact_runtime_rows(
        failure_path, "failure action result v2", FAILURE_ACTION_V2_HEADER,
        256 * 1024, 256,
    )
    observed_requests, observed_results = validate_failure_action_rows(
        rows, native["failure.action.enabled"]
    )
    if observed_requests != request_count or observed_results != result_count:
        fail("failure action grouped counts disagree with runtime metadata")

    identity_expected = {
        "schema": "spaceterm.acceptance.run-identity-public/v2",
        "run.id": native["run.id"],
        "run.origin": "mounted-dmg",
        "package.app.sha256": native["package.app.sha256"],
        "native.observation.path": "identity/native-observation.tsv",
        "native.observation.sha256": sha256_path(native_path),
        "native.observation.source": "production-app",
        "native.provisional.observation.sha256": provisional_hash,
        "native.runtime.metadata.schema": runtime["schema"],
        "native.runtime.metadata.path": "identity/runtime-metadata.tsv",
        "native.runtime.metadata.sha256": sha256_path(runtime_path),
        "native.failure.action.enabled": native["failure.action.enabled"],
        "native.failure.result.schema": runtime["failure.result.schema"],
        "native.failure.actions.path": "identity/failure-actions.tsv",
        "native.failure.actions.sha256": sha256_path(failure_path),
        "native.failure.request_count": str(request_count),
        "native.failure.result_count": str(result_count),
        "font.selected.family": native["terminal_font_selected"],
        "font.selected.source": "production-app-observation",
        "host.terminal_font_selected": native["terminal_font_selected"],
    }
    for grid_key in (
        "rows", "columns", "logical_width", "logical_height",
        "backing_pixel_width", "backing_pixel_height",
    ):
        identity_expected[f"host.initial_grid.{grid_key}"] = native[f"initial_grid.{grid_key}"]
    for key, expected in identity_expected.items():
        if identity.get(key) != expected:
            fail(f"run identity v2 does not bind native closure: {key}")
    identity_cdhash = identity.get("package.app.signature.cdhash", "")
    if identity_cdhash.upper() != native["process.signature.cdhash"].upper() \
            or identity.get("package.app.signature.identifier") != native["process.signature.identifier"]:
        fail("run identity v2 package signature disagrees with live native identity")
    observed_team = native["process.signature.team_identifier"]
    packaged_team = identity.get("package.app.signature.team_identifier", "")
    if observed_team != packaged_team and not (
        not observed_team and packaged_team in {"not-set", "not set"}
    ):
        fail("run identity v2 package team identifier disagrees with live native identity")


def native_closure_receipt(
    native_path: Path, runtime_path: Path, samples_path: Path, events_path: Path,
    failure_path: Path, identity: dict[str, str], run_id: str,
) -> bytes:
    fields = {
        "schema": "spaceterm.acceptance.native-closure-replay/v1",
        "run_id": run_id,
        "status": "PASS",
        "producer_commit": NATIVE_FAILURE_PRODUCER_COMMIT,
        "native_observation_sha256": sha256_path(native_path),
        "provisional_observation_sha256": identity["native.provisional.observation.sha256"],
        "runtime_metadata_sha256": sha256_path(runtime_path),
        "runtime_samples_sha256": sha256_path(samples_path),
        "runtime_events_sha256": sha256_path(events_path),
        "failure_actions_sha256": sha256_path(failure_path),
        "failure_result_schema": "spaceterm.acceptance.failure-action-result/v2",
    }
    return b"".join(f"{key}\t{fields[key]}\n".encode("utf-8") for key in sorted(fields))


def xml_text(element: ET.Element | None) -> str:
    return "" if element is None or element.text is None else element.text.strip()


def validate_png(path: Path) -> None:
    if byte_count(path) > 128 * 1024 * 1024:
        fail(f"screenshot payload exceeds the bounded PNG size: {path}")
    data = path.read_bytes()
    if len(data) < 45 or data[:8] != b"\x89PNG\r\n\x1a\n":
        fail(f"screenshot payload is not PNG: {path}")
    offset = 8
    chunks: list[tuple[bytes, bytes]] = []
    while offset < len(data):
        if offset + 12 > len(data):
            fail(f"PNG has a truncated chunk header: {path}")
        length = int.from_bytes(data[offset:offset + 4], "big")
        chunk_type = data[offset + 4:offset + 8]
        end = offset + 12 + length
        if end > len(data):
            fail(f"PNG has a truncated chunk body: {path}")
        payload = data[offset + 8:offset + 8 + length]
        expected_crc = int.from_bytes(data[offset + 8 + length:end], "big")
        if zlib.crc32(chunk_type + payload) & 0xFFFFFFFF != expected_crc:
            fail(f"PNG chunk checksum is invalid: {path}")
        chunks.append((chunk_type, payload))
        offset = end
        if chunk_type == b"IEND":
            break
    if offset != len(data) or not chunks or chunks[0][0] != b"IHDR" \
            or chunks[-1] != (b"IEND", b""):
        fail(f"PNG lacks an exact IHDR-to-IEND structure: {path}")
    if len(chunks[0][1]) != 13 or not any(kind == b"IDAT" for kind, _payload in chunks):
        fail(f"PNG lacks required image data: {path}")
    # Privacy-reviewed screenshots use a canonical metadata-free truecolor PNG
    # shape.  Even standard ancillary chunks are opaque covert channels, so a
    # capture must be normalized before registration rather than allowlisted.
    allowed_chunks = {b"IHDR", b"IDAT", b"IEND"}
    kinds = [kind for kind, _payload in chunks]
    if not set(kinds) <= allowed_chunks:
        fail(f"PNG contains prohibited metadata or unknown ancillary chunks: {path}")
    singleton_chunks = allowed_chunks - {b"IDAT"}
    if any(kinds.count(kind) != 1 for kind in singleton_chunks if kind in kinds):
        fail(f"PNG contains duplicate singleton chunks: {path}")
    idat_indexes = [index for index, kind in enumerate(kinds) if kind == b"IDAT"]
    if idat_indexes != list(range(idat_indexes[0], idat_indexes[-1] + 1)):
        fail(f"PNG IDAT chunks are not contiguous: {path}")
    width = int.from_bytes(chunks[0][1][0:4], "big")
    height = int.from_bytes(chunks[0][1][4:8], "big")
    bit_depth = chunks[0][1][8]
    color_type = chunks[0][1][9]
    if width < 1 or height < 1 or width > 16384 or height > 16384 \
            or bit_depth != 8 or color_type not in {0, 2, 4, 6} \
            or chunks[0][1][10:13] != b"\0\0\0":
        fail(f"PNG dimensions are invalid: {path}")
    channels = {0: 1, 2: 3, 4: 2, 6: 4}[color_type]
    row_bytes = width * channels
    expected_decoded = height * (row_bytes + 1)
    maximum_decoded = 256 * 1024 * 1024
    if expected_decoded > maximum_decoded:
        fail(f"PNG decoded raster exceeds the bounded size: {path}")
    try:
        decoder_state = zlib.decompressobj()
        decoded = decoder_state.decompress(
            b"".join(payload for kind, payload in chunks if kind == b"IDAT"),
            maximum_decoded + 1,
        )
    except zlib.error:
        fail(f"PNG image data cannot be decompressed: {path}")
    if len(decoded) != expected_decoded or decoder_state.unconsumed_tail \
            or decoder_state.unused_data or not decoder_state.eof:
        fail(f"PNG decoded raster does not exactly match its IHDR geometry: {path}")
    if any(decoded[row * (row_bytes + 1)] > 4 for row in range(height)):
        fail(f"PNG contains an invalid scanline filter: {path}")
    decoder = Path("/usr/bin/sips")
    ensure_regular_file(decoder, "macOS image decoder")
    decoded_result = subprocess.run(
        [str(decoder), "-g", "pixelWidth", "-g", "pixelHeight", str(path)],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=30,
    )
    if decoded_result.returncode != 0:
        fail(f"PNG cannot be decoded by macOS ImageIO: {path}")


def validate_quicktime(path: Path) -> None:
    file_size = byte_count(path)
    if file_size < 32 or file_size > 4 * 1024 * 1024 * 1024:
        fail(f"screen recording is not a QuickTime/ISO media container: {path}")
    allowed_children: dict[bytes | None, set[bytes]] = {
        None: {b"ftyp", b"moov", b"mdat", b"wide"},
        b"moov": {b"mvhd", b"trak"},
        b"trak": {b"tkhd", b"edts", b"mdia", b"tapt"},
        b"edts": {b"elst"},
        b"mdia": {b"mdhd", b"hdlr", b"minf"},
        b"minf": {b"vmhd", b"smhd", b"nmhd", b"dinf", b"stbl"},
        b"dinf": {b"dref"},
        b"stbl": {
            b"stsd", b"stts", b"ctts", b"cslg", b"stsc", b"stsz", b"stz2",
            b"stco", b"co64", b"stss", b"stps", b"sdtp", b"padb", b"stdp",
            b"sbgp", b"sgpd", b"subs", b"saiz", b"saio",
        },
        b"tapt": {b"clef", b"prof", b"enof"},
    }
    containers = {b"moov", b"trak", b"edts", b"mdia", b"minf", b"dinf", b"stbl", b"tapt"}
    inventory: list[tuple[bytes | None, bytes, bytes]] = []

    def scan_control_bytes(payload: bytes, label: str) -> None:
        for match in re.finditer(rb"[\x20-\x7e]{4,}", payload):
            privacy_scan(match.group(0).decode("ascii"), label)
        for encoding, pattern in (
            ("utf-16-le", rb"(?:[\x20-\x7e]\x00){4,}"),
            ("utf-16-be", rb"(?:\x00[\x20-\x7e]){4,}"),
        ):
            for match in re.finditer(pattern, payload):
                privacy_scan(match.group(0).decode(encoding), f"{label} {encoding}")

    def parse_child_boxes(payload: bytes, label: str) -> list[tuple[bytes, bytes]]:
        boxes: list[tuple[bytes, bytes]] = []
        offset = 0
        while offset < len(payload):
            if offset + 8 > len(payload):
                fail(f"screen recording has a truncated {label} child box: {path}")
            size = int.from_bytes(payload[offset:offset + 4], "big")
            kind = payload[offset + 4:offset + 8]
            if size < 8 or offset + size > len(payload):
                fail(f"screen recording has an invalid {label} child box: {path}")
            boxes.append((kind, payload[offset + 8:offset + size]))
            offset += size
        return boxes

    def parse_atoms(blob: bytes, parent: bytes | None) -> None:
        offset = 0
        while offset < len(blob):
            if offset + 8 > len(blob):
                fail(f"screen recording has a truncated atom header: {path}")
            size = int.from_bytes(blob[offset:offset + 4], "big")
            kind = blob[offset + 4:offset + 8]
            header = 8
            if size == 1:
                if offset + 16 > len(blob):
                    fail(f"screen recording has a truncated extended atom: {path}")
                size = int.from_bytes(blob[offset + 8:offset + 16], "big")
                header = 16
            elif size == 0:
                if parent is not None or kind != b"mdat" or offset + 8 == len(blob):
                    fail(f"screen recording contains an unsafe zero-sized atom: {path}")
                size = len(blob) - offset
            if size < header or offset + size > len(blob):
                fail(f"screen recording has an invalid atom size: {path}")
            if kind not in allowed_children[parent]:
                fail(f"screen recording contains prohibited or unknown metadata atom {kind!r}: {path}")
            payload = blob[offset + header:offset + size]
            if kind == b"wide" and payload:
                fail(f"screen recording wide atom is not empty: {path}")
            inventory.append((parent, kind, payload))
            if kind in containers:
                parse_atoms(payload, kind)
            offset += size

    with path.open("rb") as handle:
        offset = 0
        while offset < file_size:
            handle.seek(offset)
            header_bytes = handle.read(16)
            if len(header_bytes) < 8:
                fail(f"screen recording has a truncated top-level atom header: {path}")
            size = int.from_bytes(header_bytes[0:4], "big")
            kind = header_bytes[4:8]
            header = 8
            if size == 1:
                if len(header_bytes) < 16:
                    fail(f"screen recording has a truncated extended atom: {path}")
                size = int.from_bytes(header_bytes[8:16], "big")
                header = 16
            elif size == 0:
                if kind != b"mdat":
                    fail(f"screen recording contains an unsafe zero-sized atom: {path}")
                size = file_size - offset
            if size < header or offset + size > file_size or kind not in allowed_children[None]:
                fail(f"screen recording has an invalid or prohibited top-level atom: {path}")
            payload_size = size - header
            if kind == b"mdat":
                inventory.append((None, kind, b"\0" if payload_size else b""))
            else:
                if payload_size > 64 * 1024 * 1024:
                    fail(f"screen recording control atom exceeds the bounded parser size: {path}")
                handle.seek(offset + header)
                payload = handle.read(payload_size)
                if len(payload) != payload_size:
                    fail(f"screen recording control atom is truncated: {path}")
                if kind == b"wide" and payload:
                    fail(f"screen recording wide atom is not empty: {path}")
                inventory.append((None, kind, payload))
                if kind in containers:
                    parse_atoms(payload, kind)
            offset += size
    top_level = [kind for parent, kind, _payload in inventory if parent is None]
    if top_level.count(b"ftyp") != 1 or top_level.count(b"moov") != 1 \
            or not any(kind == b"mdat" and payload for parent, kind, payload in inventory if parent is None):
        fail(f"screen recording lacks ftyp/moov/nonempty mdat atoms: {path}")
    if sum(parent == b"moov" and kind == b"mvhd" for parent, kind, _ in inventory) != 1 \
            or not any(parent == b"moov" and kind == b"trak" for parent, kind, _ in inventory):
        fail(f"screen recording lacks a movie header or media track: {path}")
    for _parent, kind, payload in inventory:
        if kind != b"mdat":
            scan_control_bytes(payload, f"screen recording {kind!r} control metadata")
        if kind == b"hdlr":
            if len(payload) < 24 or payload[0:8] != b"\0" * 8 \
                    or payload[8:12] not in {b"vide", b"soun"} \
                    or payload[12:24] != b"\0" * 12:
                fail(f"screen recording contains a noncanonical media handler: {path}")
            scan_control_bytes(payload[24:], "screen recording media handler name")
        elif kind == b"dref":
            if len(payload) < 8 or payload[:4] != b"\0" * 4 \
                    or int.from_bytes(payload[4:8], "big") != 1:
                fail(f"screen recording contains a noncanonical data reference: {path}")
            references = parse_child_boxes(payload[8:], "data-reference")
            if references != [(b"url ", b"\0\0\0\1")]:
                fail(f"screen recording data reference is not exact self-contained media: {path}")
        elif kind == b"stsd":
            if len(payload) < 8 or payload[:4] != b"\0" * 4 \
                    or int.from_bytes(payload[4:8], "big") != 1:
                fail(f"screen recording contains a noncanonical sample description: {path}")
            entries = parse_child_boxes(payload[8:], "sample-description")
            if len(entries) != 1 or entries[0][0] not in {b"avc1", b"hvc1"}:
                fail(f"screen recording sample description is not normalized video: {path}")
            sample = entries[0][1]
            if len(sample) < 78 or sample[:6] != b"\0" * 6 \
                    or int.from_bytes(sample[6:8], "big") == 0:
                fail(f"screen recording video sample entry is malformed: {path}")
            compressor_length = sample[42]
            compressor = sample[43:43 + compressor_length]
            if compressor_length > 31 or any(sample[43 + compressor_length:74]):
                fail(f"screen recording compressor name is not canonically padded: {path}")
            scan_control_bytes(compressor, "screen recording compressor name")
            extensions = parse_child_boxes(sample[78:], "sample-entry")
            allowed_extensions = {b"avcC", b"hvcC", b"colr", b"pasp", b"fiel", b"clap", b"btrt"}
            if any(extension not in allowed_extensions for extension, _body in extensions) \
                    or len({extension for extension, _body in extensions}) != len(extensions):
                fail(f"screen recording sample entry contains unknown or duplicate extensions: {path}")
            for extension, body in extensions:
                scan_control_bytes(body, f"screen recording {extension!r} sample metadata")
        if kind not in {b"mvhd", b"tkhd", b"mdhd"}:
            continue
        if len(payload) < 12 or payload[0] not in {0, 1}:
            fail(f"screen recording has an invalid timestamp-bearing atom: {path}")
        timestamp_bytes = 8 if payload[0] == 0 else 16
        if len(payload) < 4 + timestamp_bytes or any(payload[4:4 + timestamp_bytes]):
            fail(f"screen recording contains non-normalized creation/modification metadata: {path}")
    metadata_tool = Path("/usr/bin/mdls")
    ensure_regular_file(metadata_tool, "macOS media metadata validator")
    def mdls_value(attribute: str) -> str:
        result = subprocess.run(
            [str(metadata_tool), "-raw", "-name", attribute, str(path)],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=30,
            text=True,
        )
        if result.returncode != 0:
            fail(f"screen recording metadata cannot be decoded: {path}")
        return result.stdout.strip()

    if "movie" not in mdls_value("kMDItemContentType").lower():
        fail(f"screen recording is not recognized as movie media by macOS: {path}")
    duration = mdls_value("kMDItemDurationSeconds")
    if not re.fullmatch(r"[0-9]+(?:\.[0-9]+)?", duration) or float(duration) <= 0:
        fail(f"screen recording has no positive playable duration: {path}")


def validate_text_payload(path: Path) -> None:
    if byte_count(path) > 64 * 1024 * 1024:
        fail(f"text evidence exceeds the bounded 64 MiB size: {path}")
    try:
        text = path.read_text(encoding="utf-8")
    except UnicodeDecodeError:
        fail(f"text payload is not UTF-8: {path}")
    if "\x00" in text or "\r" in text:
        fail(f"text payload contains NUL or non-LF line endings: {path}")
    for line_number, line in enumerate(text.splitlines(), start=1):
        for field_number, field in enumerate(line.split("\t"), start=1):
            if field_number == 1:
                reject_forbidden_field_name(
                    field, f"text payload {path.name} key at line {line_number}"
                )
            privacy_scan(field, f"text payload {path.name} line {line_number} field {field_number}")


def validate_rss_payload(path: Path, record: dict[str, Any]) -> None:
    analyzer_name = (
        "analyze-release-performance-resize.awk"
        if record["case_id"] == "perf-resize"
        else "analyze-release-performance-sustained.awk"
    )
    if record["case_id"] in RENDER_CASES:
        fail("render-path records must not use an RSS artifact")
    analyzer = Path(__file__).resolve().parent / analyzer_name
    ensure_regular_file(analyzer, "release performance RSS analyzer")
    result = subprocess.run(
        ["awk", "-f", str(analyzer), str(path)],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=30,
    )
    if result.returncode == 2 or result.returncode not in {0, 1}:
        fail(f"RSS artifact is not runnable under the repository analyzer: {path}")
    report = parse_unique_tsv_bytes(result.stdout, "RSS analyzer report")
    if report.get("format_version") != "3" or report.get("result") not in {"PASS", "FAIL"}:
        fail("RSS analyzer report is invalid")
    if report["result"] != record["memory_plateau_result"]:
        fail("recorded memory result disagrees with the repository RSS analyzer")

    expected_scenario = {
        "perf-sustained-ascii": "ascii",
        "perf-sustained-unicode-styles": "unicode-styles",
        "perf-sustained-scrolled": "scrolled",
        "perf-sustained-hidden": "hidden-occluded",
        "perf-resize": "resize",
    }[record["case_id"]]
    metadata: dict[str, str] = {}
    samples: list[tuple[int, int, int, int, int]] = []
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except UnicodeDecodeError:
        fail("RSS evidence is not UTF-8")
    if not lines or lines[0] != "elapsed_ms\tcontinuous_ns\trss_kib\tworkload_bytes\tresize_count":
        fail("RSS evidence header is invalid")
    for line in lines[1:]:
        fields = line.split("\t")
        if line.startswith("# "):
            if len(fields) != 2 or fields[0][2:] in metadata:
                fail("RSS evidence metadata is malformed or duplicated")
            metadata[fields[0][2:]] = fields[1]
            continue
        if len(fields) != 5 or any(not re.fullmatch(r"[0-9]+", field) for field in fields):
            fail("RSS evidence contains an invalid sample")
        samples.append(tuple(int(field) for field in fields))  # type: ignore[arg-type]
    if not samples:
        fail("RSS evidence contains no samples")
    inputs = record["comparison_inputs"]
    exact_metadata = {
        "format_version": "3",
        "scenario": expected_scenario,
        "sample_interval_ms": "10000",
        "requested_duration_ms": str(record["duration_seconds"] * 1000),
        "subject_identity_sha256": record["subject_identity_sha256"],
        "workload_events_sha256": inputs["workload_sha256"],
        "driver_events_sha256": inputs["scenario_settings_sha256"],
        "comparison_inputs_sha256": record["comparison_inputs_sha256"],
        "status": "complete",
    }
    for key, value in exact_metadata.items():
        if metadata.get(key) != value:
            fail(f"RSS evidence disagrees with the frozen comparison input: {key}")
    if record["bytes_processed"] != samples[-1][3]:
        fail("recorded bytes_processed disagrees with the raw RSS evidence")
    if record["case_id"] == "perf-resize":
        raw_resize_delta = samples[-1][4] - samples[0][4]
        completed_cycles = metadata.get("completed_resize_cycles", "")
        if (
            not re.fullmatch(r"[0-9]+", completed_cycles)
            or record["resize_count"] != raw_resize_delta
            or record["resize_count"] != int(completed_cycles)
        ):
            fail("recorded resize_count disagrees with raw samples/completed cycles")
    duration_ms = record["duration_seconds"] * 1000
    first_rss = [sample[2] for sample in samples if sample[0] < 300000]
    final_rss = [sample[2] for sample in samples if duration_ms - 300000 < sample[0] <= duration_ms + 1000]
    if not first_rss or not final_rss:
        fail("RSS evidence does not cover both recorded five-minute windows")
    raw_windows = []
    for values in (first_rss, final_rss):
        minimum = min(values) * 1024
        maximum = max(values) * 1024
        raw_windows.append({
            "minimum_bytes": minimum,
            "maximum_bytes": maximum,
            "range_bytes": maximum - minimum,
        })
    if record["first_post_warmup_five_minutes"] != raw_windows[0] \
            or record["final_five_minutes"] != raw_windows[1]:
        fail("recorded RSS window facts disagree with the raw samples")
    exact_threshold = max((raw_windows[0]["range_bytes"] + 9) // 10, 64 * 1024 * 1024)
    if record["allowed_range_delta_bytes"] != exact_threshold:
        fail("recorded RSS threshold disagrees with the raw samples")


def validate_trace_archive(path: Path, record: dict[str, Any]) -> None:
    if not zipfile.is_zipfile(path):
        fail(f"Instruments evidence is not a ZIP archive: {path}")
    required = {
        "trace-metadata.tsv", "trace-toc.xml", "time-profile.xml", "allocations.xml",
        "hangs.xml", "trace-verification.tsv", "render-call-tree-audit.tsv",
    }
    with zipfile.ZipFile(path) as archive:
        infos = archive.infolist()
        if archive.comment or any(info.comment for info in infos):
            fail("Instruments archive contains prohibited ZIP comments")
        if any(info.extra for info in infos):
            fail("Instruments archive contains prohibited ZIP member extra fields")
        if any(info.file_size > 64 * 1024 * 1024 for info in infos) \
                or sum(info.file_size for info in infos) > 192 * 1024 * 1024:
            fail("Instruments archive exceeds bounded uncompressed evidence size")
        if any(info.compress_size and info.file_size > info.compress_size * 200 for info in infos):
            fail("Instruments archive contains an unsafe compression ratio")
        names = [info.filename for info in infos]
        if set(names) != required or len(names) != len(required):
            fail("Instruments archive does not contain the exact reviewed export inventory")
        if any(
            name.startswith("/") or "\\" in name or ".." in Path(name).parts
            or info.is_dir()
            for name, info in zip(names, infos)
        ):
            fail("Instruments archive contains an unsafe member path")
        metadata = parse_unique_tsv_bytes(archive.read("trace-metadata.tsv"), "trace metadata")
        verification = parse_unique_tsv_bytes(
            archive.read("trace-verification.tsv"), "trace verification"
        )
        audit = parse_unique_tsv_bytes(
            archive.read("render-call-tree-audit.tsv"), "render call-tree audit"
        )
        required_metadata = {
            "format_version": "2", "capture_status": "CAPTURED",
            "incomplete_reason": "none", "trace_tables_verified": "true",
            "trace_target_pid_verified": "true", "target_survived_duration": "true",
            "package_frozen_during_capture": "true",
            "subject_identity_sha256": record["subject_identity_sha256"],
            "comparison_inputs_sha256": record["comparison_inputs_sha256"],
        }
        for key, value in required_metadata.items():
            if metadata.get(key) != value:
                fail(f"Instruments trace metadata gate failed: {key}")
        verifier = Path(__file__).resolve().parents[1] / "verify-release-performance-trace.py"
        ensure_regular_file(verifier, "release-performance trace verifier")
        if metadata.get("trace_verifier_sha256") != sha256_path(verifier):
            fail("Instruments trace metadata does not bind the repository verifier")
        for key in ("pid", "requested_duration_seconds", "recorder_elapsed_seconds"):
            if not re.fullmatch(r"[0-9]+(?:\.[0-9]+)?", str(metadata.get(key, ""))):
                fail(f"Instruments trace metadata lacks numeric {key}")
        process_name = audit.get("process_name")
        if not process_name or process_name != metadata.get("application_label"):
            fail("Instruments audit process name is missing or disagrees with metadata")
        with tempfile.TemporaryDirectory(prefix="spaceterm-trace-replay.") as temp_name:
            replay_root = Path(temp_name)
            for member in ("trace-toc.xml", "time-profile.xml", "allocations.xml", "hangs.xml"):
                (replay_root / member).write_bytes(archive.read(member))
            replay = subprocess.run(
                [
                    sys.executable, str(verifier),
                    "--toc", str(replay_root / "trace-toc.xml"),
                    "--time-profile", str(replay_root / "time-profile.xml"),
                    "--allocations", str(replay_root / "allocations.xml"),
                    "--hangs", str(replay_root / "hangs.xml"),
                    "--pid", metadata["pid"],
                    "--process-name", process_name,
                    "--requested-seconds", metadata["requested_duration_seconds"],
                    "--command-elapsed-seconds", metadata["recorder_elapsed_seconds"],
                ],
                check=False,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                timeout=60,
            )
        if replay.returncode != 0 or replay.stdout != archive.read("trace-verification.tsv"):
            fail("Instruments trace does not reproduce the exact repository verifier receipt")
        verification = parse_unique_tsv_bytes(replay.stdout, "replayed trace verification")
        if verification.get("reason") != "none":
            fail("Instruments trace verification is not PASS")
        for key in (
            "time_profiler_sample_count", "allocations_event_count", "actual_record_duration_seconds",
            "maximum_main_thread_hang_ms",
        ):
            if key not in verification:
                fail(f"Instruments verification lacks {key}")
        try:
            sample_count = int(verification["time_profiler_sample_count"])
            allocation_count = int(verification["allocations_event_count"])
            actual_duration = float(verification["actual_record_duration_seconds"])
            maximum_hang_ms = float(verification["maximum_main_thread_hang_ms"])
        except (TypeError, ValueError):
            fail("Instruments trace verification contains non-numeric measurements")
        if sample_count < 2:
            fail("Instruments Time Profiler has insufficient samples")
        if allocation_count < 1:
            fail("Instruments Allocations export is empty")
        required_duration = record.get("duration_seconds", record.get("trace_duration_seconds", 1))
        if not math.isfinite(actual_duration) or actual_duration < required_duration:
            fail("Instruments trace does not cover the recorded duration")
        if not math.isfinite(maximum_hang_ms) or maximum_hang_ms < 0:
            fail("Instruments trace verification has an invalid maximum hang duration")
        if record["case_id"] not in RENDER_CASES:
            if float(record["maximum_main_thread_stall_ms"]) != maximum_hang_ms:
                fail("recorded maximum stall disagrees with the replayed trace")
            if record["subject"] == "spaceterm" and record["status"] == "PASS" \
                    and maximum_hang_ms > 250:
                fail("SpaceTerm PASS trace contains a main-thread stall longer than 250 ms")
        audit_required = {
            "record_id": record["record_id"],
            "trace_metadata_sha256": hashlib.sha256(archive.read("trace-metadata.tsv")).hexdigest(),
            "trace_toc_sha256": hashlib.sha256(archive.read("trace-toc.xml")).hexdigest(),
            "time_profile_sha256": hashlib.sha256(archive.read("time-profile.xml")).hexdigest(),
            "allocations_sha256": hashlib.sha256(archive.read("allocations.xml")).hexdigest(),
            "hangs_sha256": hashlib.sha256(archive.read("hangs.xml")).hexdigest(),
            "trace_verification_sha256": hashlib.sha256(archive.read("trace-verification.tsv")).hexdigest(),
            "manual_review": "PASS",
        }
        for key, value in audit_required.items():
            if audit.get(key) != value:
                fail(f"render call-tree audit is missing or stale: {key}")
        if record["case_id"] in RENDER_CASES and record["subject"] == "spaceterm":
            conclusions = {
                "paint_text_shaping_stack_present": "false",
                "paint_path_or_plan_construction_present": "false",
                "paint_normal_frame_allocation_stack_present": "false",
                "cursor_or_blink_reshaped_unchanged_rows": "false",
                "changed_row_proportionality_result": "PASS",
            }
            for key, value in conclusions.items():
                if audit.get(key) != value or str(record[key]).lower() != value.lower():
                    fail(f"render trace audit conclusion is missing or contradicts record: {key}")
            time_root = ET.fromstring(archive.read("time-profile.xml"))
            allocation_root = ET.fromstring(archive.read("allocations.xml"))
            time_stacks = [
                [frame.get("name") or xml_text(frame.find("name")) for frame in row.findall(".//frame")]
                for row in time_root.findall(".//row")
            ]
            allocation_stacks = [
                [frame.get("name") or xml_text(frame.find("name")) for frame in row.findall(".//frame")]
                for row in allocation_root.findall(".//row")
            ]
            paint_stacks = [stack for stack in time_stacks if any("TerminalGridElement::paint" in name for name in stack)]
            if not paint_stacks:
                fail("render proof contains no sampled TerminalGridElement::paint stack")
            forbidden = ("shape", "path", "symbol plan", "row plan", "image placement")
            if any(
                any(term in name.lower() for term in forbidden)
                for stack in paint_stacks for name in stack
            ):
                fail("render proof contains forbidden paint-descendant work")
            if any(
                any("TerminalGridElement::paint" in name for name in stack)
                for stack in allocation_stacks
            ):
                fail("render proof contains an allocation rooted in TerminalGridElement::paint")
        for xml_name in ("trace-toc.xml", "time-profile.xml", "allocations.xml", "hangs.xml"):
            try:
                xml_payload = archive.read(xml_name)
                without_declaration = re.sub(
                    rb"^\s*<\?xml\s+[^?]*\?>", b"", xml_payload, count=1
                )
                if any(token in without_declaration for token in (
                    b"<!--", b"<?", b"<!DOCTYPE", b"<!ENTITY", b"<![CDATA[",
                )):
                    fail(f"Instruments XML contains prohibited comments, directives, or entities: {xml_name}")
                xml_root = ET.fromstring(xml_payload)
            except ET.ParseError:
                fail(f"Instruments archive contains invalid XML: {xml_name}")
            try:
                xml_text = xml_payload.decode("utf-8")
            except UnicodeDecodeError:
                fail(f"Instruments XML is not UTF-8: {xml_name}")
            for line_number, line in enumerate(xml_text.splitlines(), start=1):
                privacy_scan(line, f"Instruments {xml_name} line {line_number}")
            for element in xml_root.iter():
                local_tag = element.tag.rsplit("}", 1)[-1]
                reject_forbidden_field_name(local_tag, f"Instruments {xml_name} XML tag")
                if local_tag.lower() in {"key", "name", "field", "attribute"} \
                        and element.text and element.text.strip():
                    reject_forbidden_field_name(
                        element.text.strip(),
                        f"Instruments {xml_name} semantic XML field name",
                    )
                for attribute, value in element.attrib.items():
                    reject_forbidden_field_name(
                        attribute.rsplit("}", 1)[-1],
                        f"Instruments {xml_name} XML attribute",
                    )
                    privacy_scan(value, f"Instruments {xml_name} XML attribute value")


def validate_payload_format(path: Path, artifact: dict[str, Any], record: dict[str, Any]) -> None:
    media_type = artifact["media_type"]
    kind = ARTIFACT_FILE_RE.fullmatch(Path(artifact["relative_path"]).name).group(4)  # type: ignore[union-attr]
    if media_type == "image/png":
        validate_png(path)
    elif media_type == "video/quicktime":
        validate_quicktime(path)
    elif media_type.startswith("text/"):
        validate_text_payload(path)
        if kind == "rss" and record["status"] == "PASS":
            validate_rss_payload(path, record)
        elif kind == "ax" and record["case_id"] == "capability-accessibility":
            validate_native_ax_observation(path, record)
    elif media_type == "application/zip" and kind in {"instruments", "time-profiler", "allocations"}:
        validate_trace_archive(path, record)
    elif media_type == "application/zip" and kind == "trace-archive":
        trace_tree_sha256_from_zip(path)
    else:
        fail(f"unsupported or mismatched issue #43 artifact type/kind: {media_type}/{kind}")


def validate_artifact_input(document: Any, records: dict[str, dict[str, Any]], root: Path,
                            run_id: str) -> dict[str, Any]:
    if not isinstance(document, dict):
        fail("artifact metadata must be a JSON object")
    require_keys(
        document,
        (
            "artifact_id", "record_id", "relative_path", "media_type", "created_utc",
            "producer", "producer_version", "redaction_notes", "public_url", "content_class",
        ),
        "artifact metadata",
    )
    forbidden = {"sha256", "bytes", "privacy_review", "subject", "case_id", "run_id"} & set(document)
    if forbidden:
        fail(f"artifact metadata attempts to supply derived fields: {', '.join(sorted(forbidden))}")
    record_id = document["record_id"]
    if record_id not in records:
        fail("artifact owner record does not exist")
    record = records[record_id]
    path, kind = validate_relative_payload(root, document["relative_path"], record)
    expected_id = f"{record_id}-{kind}"
    if document["artifact_id"] != expected_id:
        fail("artifact_id must be <record-id>-<artifact-kind>")
    if document["content_class"] not in {"content-free", "deterministic-fixture", "protocol-bytes"}:
        fail("artifact content_class must explicitly permit only safe acceptance content")
    reject_future_utc(document["created_utc"], "artifact created_utc")
    owner = records[document["record_id"]]
    if not owner["started_utc"] <= document["created_utc"]:
        fail("artifact created_utc predates its owning case attempt")
    for key in ("media_type", "producer", "producer_version", "redaction_notes", "public_url"):
        require_nonempty(document[key], f"artifact metadata {key}")
    if kind == "ax" and (
        record["case_id"] != "capability-accessibility"
        or document["producer_version"] != AX_PROBE_PRODUCER_COMMIT
    ):
        fail("native AX evidence must use the pinned 2099 probe producer")
    if not re.fullmatch(r"[a-z0-9][a-z0-9.+-]*/[a-z0-9][a-z0-9.+-]*", document["media_type"]):
        fail("artifact media_type is not concrete")
    if not str(document["public_url"]).startswith("https://"):
        fail("artifact public_url must be a direct HTTPS URL frozen before finalization")
    privacy_scan(document, "artifact metadata")
    result = dict(document)
    result.update(
        {
            "subject": record["subject"],
            "case_id": record["case_id"],
            "run_id": run_id,
            "sha256": sha256_path(path),
            "bytes": byte_count(path),
        }
    )
    validate_payload_format(path, result, record)
    return result


def command_add_artifact(args: argparse.Namespace) -> None:
    root = resolve_root(args.run_id)
    with campaign_lock(root):
        state = require_initialized(root, args.run_id)
        records = load_records(state, args.run_id)
        if list((state / "record-reviews").glob("*.json")) \
                or list((state / "artifact-reviews").glob("*.json")):
            fail("artifacts cannot be registered after manual review has begun")
        artifact = validate_artifact_input(read_json(Path(args.input)), records, root, args.run_id)
        write_exclusive(
            state / "artifact-metadata" / f"{artifact['artifact_id']}.json",
            canonical_json(artifact),
            0o400,
        )
        os.chmod(root / artifact["relative_path"], 0o444)


def load_artifacts(state: Path, records: dict[str, dict[str, Any]], root: Path,
                   run_id: str) -> dict[str, dict[str, Any]]:
    artifacts: dict[str, dict[str, Any]] = {}
    paths = sorted((state / "artifact-metadata").glob("*.json"))
    if len(paths) > 1024:
        fail("private artifact inventory exceeds the bounded 1024-payload limit")
    for path in paths:
        ensure_regular_file(path, "private artifact metadata")
        document = read_json(path)
        if not isinstance(document, dict):
            fail("private artifact metadata is invalid")
        required_derived = {"subject", "case_id", "run_id", "sha256", "bytes"}
        if not required_derived <= set(document):
            fail("private artifact metadata lacks derived binding")
        input_document = {key: value for key, value in document.items() if key not in required_derived}
        current = validate_artifact_input(input_document, records, root, run_id)
        if document != current:
            fail(f"artifact payload or metadata changed after registration: {document.get('artifact_id')}")
        artifact_id = document["artifact_id"]
        if path.name != f"{artifact_id}.json" or artifact_id in artifacts:
            fail(f"duplicate or misnamed artifact metadata: {path}")
        artifacts[artifact_id] = document
    return artifacts


def command_review_artifact(args: argparse.Namespace) -> None:
    if args.attestation != ARTIFACT_REVIEW_ATTESTATION:
        fail("artifact review attestation text is not exact")
    if args.decision not in {"PASS", "REJECTED"}:
        fail("artifact review decision must be PASS or REJECTED")
    if args.reviewer_role != ARTIFACT_REVIEWER_ROLE or not REVIEWER_RE.fullmatch(args.reviewer):
        fail("artifact review requires its exact role and an explicit github:<reviewer> identity")
    root = resolve_root(args.run_id)
    with campaign_lock(root):
        state = require_initialized(root, args.run_id)
        records = load_records(state, args.run_id)
        artifacts = load_artifacts(state, records, root, args.run_id)
        if args.artifact_id not in artifacts:
            fail("unknown artifact_id")
        artifact = artifacts[args.artifact_id]
        review = {
            "artifact_id": args.artifact_id,
            "reviewed_sha256": artifact["sha256"],
            "decision": args.decision,
            "reviewer_role": args.reviewer_role,
            "reviewer": args.reviewer,
            "review_url": args.review_url,
            "reviewed_utc": args.reviewed_utc or utc_now(),
            "attestation": args.attestation,
        }
        validate_utc(review["reviewed_utc"], "artifact review timestamp")
        require_nonempty(review["reviewer_role"], "artifact reviewer role")
        validate_github_review_url(review)
        privacy_scan(review, "artifact review")
        write_exclusive(
            state / "artifact-reviews" / f"{args.artifact_id}.json",
            canonical_json(review),
            0o400,
        )


def command_review_record(args: argparse.Namespace) -> None:
    if args.attestation != RECORD_REVIEW_ATTESTATION:
        fail("record review attestation text is not exact")
    if args.decision not in {"PASS", "REJECTED"}:
        fail("record review decision must be PASS or REJECTED")
    if args.reviewer_role != RECORD_REVIEWER_ROLE or not REVIEWER_RE.fullmatch(args.reviewer):
        fail("record review requires its exact role and an explicit github:<reviewer> identity")
    root = resolve_root(args.run_id)
    with campaign_lock(root):
        state = require_initialized(root, args.run_id)
        records = load_records(state, args.run_id)
        if args.record_id not in records:
            fail("unknown record_id")
        artifacts = load_artifacts(state, records, root, args.run_id)
        metadata_path = state / "campaign-metadata.json"
        ensure_regular_file(metadata_path, "campaign metadata")
        validate_metadata(read_json(metadata_path), args.run_id)
        record = records[args.record_id]
        inventory_digest = artifact_inventory_digest(record, artifacts)
        record_path = state / "records" / f"{args.record_id}.json"
        review = {
            "record_id": args.record_id,
            "reviewed_sha256": sha256_path(record_path),
            "artifact_inventory_sha256": inventory_digest,
            "campaign_metadata_sha256": sha256_path(metadata_path),
            "decision": args.decision,
            "reviewer_role": args.reviewer_role,
            "reviewer": args.reviewer,
            "review_url": args.review_url,
            "reviewed_utc": args.reviewed_utc or utc_now(),
            "attestation": args.attestation,
        }
        validate_utc(review["reviewed_utc"], "record review timestamp")
        require_nonempty(review["reviewer_role"], "record reviewer role")
        validate_github_review_url(review)
        privacy_scan(review, "record review")
        write_exclusive(
            state / "record-reviews" / f"{args.record_id}.json",
            canonical_json(review),
            0o400,
        )


def command_review_batch_proposal(args: argparse.Namespace) -> None:
    if not REVIEWER_RE.fullmatch(args.reviewer):
        fail("review batch proposal requires an explicit github:<reviewer> identity")
    root = resolve_root(args.run_id)
    with campaign_lock(root):
        state = require_initialized(root, args.run_id)
        records = load_records(state, args.run_id)
        artifacts = load_artifacts(state, records, root, args.run_id)
        metadata_path = state / "campaign-metadata.json"
        ensure_regular_file(metadata_path, "campaign metadata")
        validate_metadata(read_json(metadata_path), args.run_id)
        if args.kind == "records":
            metadata_digest = sha256_path(metadata_path)
            reviews = [
                {
                    "record_id": record_id,
                    "record_sha256": sha256_path(state / "records" / f"{record_id}.json"),
                    "artifact_inventory_sha256": artifact_inventory_digest(record, artifacts),
                    "campaign_metadata_sha256": metadata_digest,
                    "reviewer_role": RECORD_REVIEWER_ROLE,
                    "reviewer": args.reviewer,
                    "review_url": "proposal-only",
                    "reviewed_utc": "proposal-only",
                    "attestation": RECORD_REVIEW_ATTESTATION,
                }
                for record_id, record in sorted(records.items())
            ]
        else:
            reviews = [
                {
                    "artifact_id": artifact_id,
                    "artifact_sha256": artifact["sha256"],
                    "reviewer_role": ARTIFACT_REVIEWER_ROLE,
                    "reviewer": args.reviewer,
                    "review_url": "proposal-only",
                    "reviewed_utc": "proposal-only",
                    "attestation": ARTIFACT_REVIEW_ATTESTATION,
                }
                for artifact_id, artifact in sorted(artifacts.items())
            ]
        print(review_batch_body(reviews, args.kind))


def load_reviews(directory: Path, key: str) -> dict[str, dict[str, Any]]:
    reviews = {}
    for path in sorted(directory.glob("*.json")):
        ensure_regular_file(path, "private manual review")
        review = read_json(path)
        if not isinstance(review, dict) or key not in review:
            fail(f"invalid manual review: {path}")
        identity = review[key]
        if path.name != f"{identity}.json" or identity in reviews:
            fail(f"duplicate or misnamed manual review: {path}")
        reviews[identity] = review
    return reviews


def validate_graph(records: dict[str, dict[str, Any]]) -> tuple[dict[tuple[str, str], dict[str, Any]], str]:
    by_scope: dict[tuple[str, str], list[dict[str, Any]]] = {}
    for record in records.values():
        by_scope.setdefault((record["case_id"], record["subject"]), []).append(record)

    for (case_id, subject), chain in by_scope.items():
        chain.sort(key=lambda item: item["attempt"])
        attempts = [item["attempt"] for item in chain]
        if attempts != list(range(1, len(chain) + 1)):
            fail(f"attempt chain is not contiguous for {case_id}/{subject}")
        for index, record in enumerate(chain):
            expected = None if index == 0 else chain[index - 1]["record_id"]
            if record["supersedes_record_id"] != expected:
                fail(f"supersession chain is not exact and linear for {record['record_id']}")

    leaves = {scope: sorted(chain, key=lambda item: item["attempt"])[-1]
              for scope, chain in by_scope.items()}
    required_scopes = {(case, "spaceterm") for case in REQUIRED_SPACETERM}
    required_scopes |= {(case, "ghostty") for case in PERFORMANCE_CASES}
    missing = sorted(required_scopes - set(leaves))
    if missing:
        fail("required effective record inventory is incomplete: " + ", ".join(f"{c}/{s}" for c, s in missing))
    allowed_scopes = set(required_scopes)
    allowed_scopes |= {(case, "spaceterm") for case in SUPPLEMENTARY_CASES}
    for record in records.values():
        if record["case_id"] in NATIVE_CASES and record["subject"] == "spaceterm" and record["status"] == "FAIL":
            allowed_scopes.add((record["case_id"], "ghostty"))
    extras = sorted(set(leaves) - allowed_scopes)
    if extras:
        fail("record inventory contains unapproved subject/case scopes: " + ", ".join(f"{c}/{s}" for c, s in extras))

    for record in records.values():
        case_id = record["case_id"]
        comparison_id = record["comparison_record_id"]
        paired_required = case_id in PERFORMANCE_CASES or (
            case_id in NATIVE_CASES and record["status"] == "FAIL"
        ) or (record["subject"] == "ghostty" and case_id in NATIVE_CASES)
        if paired_required:
            if not isinstance(comparison_id, str) or comparison_id not in records:
                fail(f"required comparison record is missing for {record['record_id']}")
            opposite = records[comparison_id]
            if (
                opposite["comparison_record_id"] != record["record_id"]
                or opposite["case_id"] != case_id
                or opposite["subject"] == record["subject"]
            ):
                fail(f"comparison pair is non-reciprocal or stale for {record['record_id']}")
            if case_id in PERFORMANCE_CASES and (
                opposite.get("comparison_inputs") != record.get("comparison_inputs")
                or opposite.get("comparison_inputs_sha256") != record.get("comparison_inputs_sha256")
            ):
                fail(f"performance comparison inputs differ for {record['record_id']}")
            if case_id in NATIVE_CASES and (
                opposite.get("comparison_inputs_sha256") != record.get("comparison_inputs_sha256")
            ):
                fail(f"native failure comparison inputs differ for {record['record_id']}")
            if case_id in NATIVE_CASES and opposite["status"] != "PASS":
                fail(f"required native Ghostty reproduction was not run for {record['record_id']}")
            if case_id in PERFORMANCE_CASES and \
                    (record["supersedes_record_id"] is None) != \
                    (opposite["supersedes_record_id"] is None):
                fail(f"paired rerun history is stale for {record['record_id']}")
        elif comparison_id is not None:
            fail(f"unpaired record unexpectedly references a comparison: {record['record_id']}")

    spaceterm_statuses = [
        leaves[(case_id, "spaceterm")]["status"] for case_id in REQUIRED_SPACETERM
    ]
    # Ghostty comparisons must exist, be paired, and have completed evidence,
    # but their measured absolute results are not SpaceTerm correctness gates.
    # PASS here means the comparison observation was completed; threshold
    # differences remain in the record's measured fields and comment.
    ghostty_incomplete = any(
        leaves[(case_id, "ghostty")]["status"] != "PASS"
        for case_id in PERFORMANCE_CASES
    )
    native_reference_incomplete = any(
        record["case_id"] in NATIVE_CASES
        and record["subject"] == "ghostty"
        and record["status"] != "PASS"
        for record in records.values()
    )
    package_doctor = leaves[("package-doctor", "spaceterm")]
    if package_doctor["status"] == "NOT-APPLICABLE":
        allowed = any(
            item.get("name") == "just doctor when tool availability is known"
            and item.get("status") == "NOT-APPLICABLE"
            and item.get("availability_or_precondition_evidence")
            for item in package_doctor["conditional_subcases"]
        )
        if not allowed:
            fail("package-doctor NOT-APPLICABLE lacks the exact permitted precondition evidence")
        spaceterm_statuses.remove("NOT-APPLICABLE")
        spaceterm_statuses.append("PASS")
    campaign_status = (
        "PASS"
        if not ghostty_incomplete
        and not native_reference_incomplete
        and all(status == "PASS" for status in spaceterm_statuses)
        else "FAIL"
    )
    if campaign_status == "PASS" and (set(leaves) != required_scopes or len(leaves) != 75):
        fail("PASS issue #43 inventory must contain exactly the 75 required leaves")
    return leaves, campaign_status


def validate_campaign_conditionals(
    metadata: dict[str, Any], leaves: dict[tuple[str, str], dict[str, Any]]
) -> None:
    host = metadata["host"]

    def exact(record: dict[str, Any], name: str, expected_status: str) -> None:
        matches = [item for item in record["conditional_subcases"] if item["name"] == name]
        if len(matches) != 1 or matches[0]["status"] != expected_status:
            fail(
                f"{record['record_id']} must contain one {name!r} conditional with "
                f"status {expected_status}"
            )
        require_nonempty(matches[0]["availability_or_precondition_evidence"],
                         f"conditional evidence for {record['record_id']}")

    exact(
        leaves[("capability-keyboard", "spaceterm")],
        "numpad input where available",
        "PASS" if host["numpad_available"] else "SKIPPED-UNAVAILABLE",
    )
    exact(
        leaves[("focus-non-key-os-window", "spaceterm")],
        "non-key while SpaceTerm remains active where possible",
        "PASS" if host["non_key_window_possible"] else "SKIPPED-UNAVAILABLE",
    )
    second_display_status = "PASS" if host["second_display_available"] else "SKIPPED-UNAVAILABLE"
    exact(
        leaves[("capability-resize-scrollback", "spaceterm")],
        "backing-scale/display movement when a second display is available",
        second_display_status,
    )
    for subject in ("spaceterm", "ghostty"):
        resize_record = leaves[("perf-resize", subject)]
        if resize_record["second_display_available"] is not host["second_display_available"]:
            fail("perf-resize second-display fact disagrees with authenticated host metadata")
        exact(
            resize_record,
            "backing-scale/display movement when a second display is available",
            second_display_status,
        )
    for case_id in ("native-claude-code", "native-pi-coding-agent"):
        record = leaves[(case_id, "spaceterm")]
        if not isinstance(record.get("link_presented"), bool):
            fail(f"{case_id} must record link_presented as boolean")
        exact(
            record,
            "detected/OSC 8 link if presented",
            "PASS" if record["link_presented"] else "NOT-APPLICABLE",
        )


def percent_decode_manifest(value: str) -> str:
    # acceptance-identity.sh encodes percent first, then control characters.
    return (
        value.replace("%09", "\t")
        .replace("%0D", "\r")
        .replace("%0A", "\n")
        .replace("%25", "%")
    )


def read_identity_manifest(path: Path) -> dict[str, str]:
    ensure_regular_file(path, "collector public identity manifest")
    result: dict[str, str] = {}
    try:
        with path.open("r", encoding="utf-8", newline="") as handle:
            for line_number, line in enumerate(handle, start=1):
                line = line.removesuffix("\n")
                parts = line.split("\t")
                if len(parts) != 2 or not parts[0] or parts[0] in result:
                    fail(f"collector identity manifest is malformed at line {line_number}")
                result[parts[0]] = percent_decode_manifest(parts[1])
    except (OSError, UnicodeError) as error:
        fail(f"collector identity manifest cannot be read: {error}")
    return result


def verify_collector_identity(root: Path, metadata: dict[str, Any], identity_path: Path | None = None) -> None:
    identity = read_identity_manifest(identity_path or (root / "public-run-identity.tsv"))
    frozen = metadata["frozen_artifact"]
    require_keys(
        identity,
        (
            "run.id", "run.origin", "repository.commit", "repository.cargo_lock_sha256",
            "repository.clean", "package.app.marketing_version", "package.app.build_version",
            "package.app.executable.architectures", "package.app.sha256",
            "package.app.signature.verified", "package.dmg.sha256", "package.dmg.verified",
        ),
        "collector public identity",
    )
    exact = {
        "run.id": metadata["run_id"],
        "run.origin": "mounted-dmg",
        "repository.commit": frozen["commit_sha"],
        "repository.cargo_lock_sha256": frozen["cargo_lock_sha256"],
        "repository.clean": "true",
        "package.app.marketing_version": str(frozen["marketing_version"]),
        "package.app.build_version": str(frozen["build_version"]),
        "package.app.sha256": frozen["app_bundle_sha256"],
        "package.app.signature.verified": "true",
        "package.dmg.sha256": frozen["dmg_sha256"],
        "package.dmg.verified": "true",
    }
    for key, expected in exact.items():
        if identity[key] != expected:
            fail(f"collector public identity disagrees with frozen campaign field {key}")
    architectures = frozen["executable_architectures"]
    if isinstance(architectures, list):
        architecture_text = ",".join(str(item) for item in architectures)
    else:
        architecture_text = str(architectures)
    if identity["package.app.executable.architectures"] != architecture_text:
        fail("collector public identity disagrees with frozen executable architectures")


def validate_identity_closure(
    identity_path: Path,
    display_path: Path,
    ghostty_path: Path,
    preconditions_path: Path,
    metadata: dict[str, Any],
) -> None:
    identity = read_identity_manifest(identity_path)
    host = metadata["host"]
    exact_host = {
        "host.macos.product_version": str(host["macos_version"]),
        "host.macos.build_version": str(host["macos_build"]),
        "host.machine.model": str(host["model_identifier"]),
        "host.cpu": str(host["cpu"]),
        "host.memory_bytes": str(host["memory_bytes"]),
        "host.terminal_font_selected": str(host["terminal_font_selected"]),
        "host.initial_grid.rows": str(host["initial_grid"]["rows"]),
        "host.initial_grid.columns": str(host["initial_grid"]["columns"]),
        "host.initial_grid.logical_width": str(host["initial_grid"]["logical_width"]),
        "host.initial_grid.logical_height": str(host["initial_grid"]["logical_height"]),
        "host.initial_grid.backing_pixel_width": str(host["initial_grid"]["backing_pixel_width"]),
        "host.initial_grid.backing_pixel_height": str(host["initial_grid"]["backing_pixel_height"]),
    }
    if host["machine_model"] != host["model_identifier"]:
        fail("campaign machine_model must use the authenticated model identifier")
    for key, value in exact_host.items():
        if identity.get(key) != value:
            fail(f"campaign host facts disagree with authenticated collector identity: {key}")
    if identity.get("font.jetbrainsmono-nerd-font.available") != str(
        host["jetbrains_mono_nerd_font_available"]
    ).lower():
        fail("campaign font availability disagrees with authenticated collector identity")
    if identity.get("host.display.summary_sha256") != sha256_path(display_path):
        fail("published display summary disagrees with authenticated collector identity")
    if identity.get("host.display.count") != str(len(host["displays"])):
        fail("campaign display count disagrees with authenticated collector identity")
    if host["second_display_available"] != (len(host["displays"]) > 1):
        fail("campaign second-display availability contradicts its authenticated inventory")
    try:
        with display_path.open("r", encoding="utf-8", newline="") as handle:
            display_rows = list(csv.DictReader(handle, delimiter="\t"))
    except (OSError, UnicodeError, csv.Error) as error:
        fail(f"collector display summary is invalid: {error}")
    if len(display_rows) != len(host["displays"]):
        fail("campaign display inventory disagrees with collector display summary")
    for row, claimed in zip(display_rows, host["displays"]):
        physical = re.search(r"([0-9]+)\s*x\s*([0-9]+)", row.get("physical_pixels", ""))
        logical = re.search(r"([0-9]+)\s*x\s*([0-9]+)", row.get("logical_resolution_refresh", ""))
        refresh = re.search(r"@\s*([0-9]+(?:\.[0-9]+)?)", row.get("logical_resolution_refresh", ""))
        try:
            scale = float(row.get("backing_scale", ""))
        except ValueError:
            fail("collector display summary has an invalid backing scale")
        if (
            physical is None or logical is None or refresh is None or not math.isfinite(scale)
            or claimed["backing_resolution"] != f"{physical.group(1)}x{physical.group(2)}"
            or claimed["logical_resolution"] != f"{logical.group(1)}x{logical.group(2)}"
            or float(claimed["refresh_hz"]) != float(refresh.group(1))
            or float(claimed["backing_scale"]) != scale
        ):
            fail("campaign display facts disagree with collector display summary")

    program_ids = {
        "Bash": "bash", "Zsh": "zsh", "Vim": "vim", "Neovim": "neovim",
        "tmux": "tmux", "less": "less", "fzf": "fzf", "btop": "btop",
        "Yazi": "yazi", "Claude Code": "claude-code",
        "pi-coding-agent": "pi-coding-agent",
    }
    for program in metadata["programs"]:
        executable_id = program_ids[program["name"]]
        if (
            identity.get(f"executable.{executable_id}.status") != "available"
            or identity.get(f"executable.{executable_id}.path") != program["executable"]
            or identity.get(f"executable.{executable_id}.sha256") != program["executable_sha256"]
            or identity.get(f"executable.{executable_id}.version") != program["version_output"]
        ):
            fail(f"program facts disagree with authenticated collector identity: {program['name']}")

    ghostty = parse_unique_tsv_bytes(ghostty_path.read_bytes(), "Ghostty frozen identity")
    expected_ghostty = {"schema": "spaceterm.acceptance.ghostty-identity/v1"}
    for key, value in metadata["ghostty_reference"].items():
        expected_ghostty[f"ghostty.{key}"] = (
            value if isinstance(value, str)
            else json.dumps(value, ensure_ascii=True, sort_keys=True, separators=(",", ":"))
        )
    if ghostty != expected_ghostty:
        fail("Ghostty campaign facts disagree with the reviewed frozen identity artifact")
    preconditions = parse_unique_tsv_bytes(
        preconditions_path.read_bytes(), "host precondition receipt"
    )
    expected_preconditions = {
        "schema": "spaceterm.acceptance.host-preconditions/v1",
        "numpad_available": str(host["numpad_available"]).lower(),
        "non_key_window_possible": str(host["non_key_window_possible"]).lower(),
        "input_sources_sha256": hashlib.sha256(
            canonical_json(host["input_sources"])
        ).hexdigest(),
        "review": "PASS",
    }
    if preconditions != expected_preconditions:
        fail("host availability/input-source facts disagree with the reviewed precondition receipt")


def run_final_identity_replay(root: Path) -> dict[str, Any]:
    verifier = Path(__file__).resolve().parents[1] / "acceptance-identity.sh"
    ensure_regular_file(verifier, "acceptance identity verifier")
    try:
        result = subprocess.run(
            [str(verifier), "verify", "--run-dir", str(root), "--final"],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=180,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        fail(f"fresh authenticated final identity replay could not run: {error}")
    if result.returncode != 0:
        fail("fresh authenticated final identity replay failed")
    return {
        "command": "scripts/acceptance-identity.sh verify --run-dir $RUN_DIR --final",
        "status": "PASS",
        "completed_utc": utc_now(),
        "verifier_sha256": sha256_path(verifier),
        "public_identity_sha256": sha256_path(root / "public-run-identity.tsv"),
        "stdout_sha256": hashlib.sha256(result.stdout).hexdigest(),
        "stderr_sha256": hashlib.sha256(result.stderr).hexdigest(),
    }


def command_capture_identity_evidence(args: argparse.Namespace) -> None:
    root = resolve_root(args.run_id)
    if root.name.startswith(".acceptance-identity."):
        fail("identity evidence can be captured only after the collector's final run-directory rename")
    with campaign_lock(root):
        state = require_initialized(root, args.run_id)
        records = load_records(state, args.run_id)
        package_records = sorted(
            (
                record for record in records.values()
                if record["case_id"] == "package-identity" and record["subject"] == "spaceterm"
            ),
            key=lambda record: record["attempt"],
        )
        if not package_records:
            fail("capture identity evidence after recording package-identity")
        record = package_records[-1]
        attempt = f"{record['attempt']:02d}"
        evidence_directory = "identity/issue-43-package-identity-evidence"
        relatives = {
            "public": f"{evidence_directory}/package-identity--spaceterm--{attempt}--public-identity.tsv",
            "replay": f"{evidence_directory}/package-identity--spaceterm--{attempt}--final-identity-replay.tsv",
            "display": f"{evidence_directory}/package-identity--spaceterm--{attempt}--display-summary.tsv",
            "closure": f"{evidence_directory}/package-identity--spaceterm--{attempt}--native-closure-replay.tsv",
            "runtime_metadata": f"{evidence_directory}/package-identity--spaceterm--{attempt}--native-runtime-metadata.tsv",
            "runtime_samples": f"{evidence_directory}/package-identity--spaceterm--{attempt}--native-runtime-samples.tsv",
            "runtime_events": f"{evidence_directory}/package-identity--spaceterm--{attempt}--native-runtime-events.tsv",
            "failure_actions": f"{evidence_directory}/package-identity--spaceterm--{attempt}--native-failure-actions.tsv",
        }
        source = root / "public-run-identity.tsv"
        display_source = root / "identity" / "displays.tsv"
        ensure_regular_file(source, "collector public identity projection")
        ensure_regular_file(display_source, "collector display summary")
        replay = run_final_identity_replay(root)
        public_bytes = source.read_bytes()
        replay_fields = {
            "schema": "spaceterm.acceptance.final-identity-replay/v1",
            "run_id": args.run_id,
            **replay,
        }
        replay_bytes = b"".join(
            f"{key}\t{replay_fields[key]}\n".encode("utf-8") for key in sorted(replay_fields)
        )
        metadata = validate_metadata(read_json(state / "campaign-metadata.json"), args.run_id)
        closure_sources = [
            root / "identity" / name for name in (
                "native-observation.tsv", "runtime-metadata.tsv", "runtime-samples.tsv",
                "runtime-events.tsv", "failure-actions.tsv",
            )
        ]
        identity = read_identity_manifest(source)
        validate_native_failure_closure(
            closure_sources[0], closure_sources[1], closure_sources[2], closure_sources[3],
            closure_sources[4], identity, metadata,
        )
        final_directory = root / evidence_directory
        if final_directory.exists():
            fail("immutable issue #43 identity evidence directory already exists")
        staging = Path(tempfile.mkdtemp(prefix=".issue-43-identity.", dir=root / "identity"))
        try:
            payloads = {
                "public": public_bytes,
                "replay": replay_bytes,
                "display": display_source.read_bytes(),
                "closure": native_closure_receipt(*closure_sources, identity, args.run_id),
                "runtime_metadata": closure_sources[1].read_bytes(),
                "runtime_samples": closure_sources[2].read_bytes(),
                "runtime_events": closure_sources[3].read_bytes(),
                "failure_actions": closure_sources[4].read_bytes(),
            }
            for name, payload in payloads.items():
                write_exclusive(staging / Path(relatives[name]).name, payload, 0o400)
            os.rename(staging, final_directory)
        finally:
            if staging.exists():
                shutil.rmtree(staging)
        for name in (
            "public", "replay", "display", "closure", "runtime_metadata",
            "runtime_samples", "runtime_events", "failure_actions",
        ):
            print(relatives[name])


def validate_identity_evidence(
    root: Path,
    metadata: dict[str, Any],
    artifacts: dict[str, dict[str, Any]],
    leaves: dict[tuple[str, str], dict[str, Any]],
    fresh_replay: dict[str, Any],
) -> None:
    binding = read_json(private_dir(root) / "collector-binding.json")
    if binding.get("staged_dmg_sha256") != metadata["frozen_artifact"]["dmg_sha256"]:
        fail("frozen DMG hash disagrees with the authenticated collector binding")
    identity = metadata["identity_evidence"]
    package_record = leaves[("package-identity", "spaceterm")]
    public_id = identity["public_identity_artifact_id"]
    replay_id = identity["final_identity_replay_artifact_id"]
    display_id = identity["display_summary_artifact_id"]
    ghostty_id = identity["ghostty_identity_artifact_id"]
    preconditions_id = identity["host_preconditions_artifact_id"]
    public_artifact = artifacts.get(public_id)
    replay_artifact = artifacts.get(replay_id)
    display_artifact = artifacts.get(display_id)
    ghostty_artifact = artifacts.get(ghostty_id)
    preconditions_artifact = artifacts.get(preconditions_id)
    if (
        public_artifact is None
        or replay_artifact is None
        or display_artifact is None
        or ghostty_artifact is None
        or preconditions_artifact is None
        or any(
            artifact["record_id"] != package_record["record_id"]
            or artifact["relative_path"].split("/", 1)[0] != "identity"
            for artifact in (
                public_artifact, replay_artifact, display_artifact, ghostty_artifact,
                preconditions_artifact,
            )
        )
    ):
        fail("public identity evidence is missing, stale, or not owned by effective package-identity")
    public_path = root / public_artifact["relative_path"]
    replay_path = root / replay_artifact["relative_path"]
    if public_path.read_bytes() != (root / "public-run-identity.tsv").read_bytes():
        fail("published identity projection differs from the authenticated collector projection")
    if public_artifact["sha256"] != fresh_replay["public_identity_sha256"]:
        fail("published identity projection disagrees with the fresh final replay")
    authenticated_identity_hash = public_artifact["sha256"]
    ghostty_identity_hash = ghostty_artifact["sha256"]
    for (case_id, subject), record in leaves.items():
        if case_id not in PERFORMANCE_CASES:
            continue
        expected_subject_hash = (
            authenticated_identity_hash if subject == "spaceterm" else ghostty_identity_hash
        )
        inputs = record["comparison_inputs"]
        if inputs["host_identity_sha256"] != authenticated_identity_hash:
            fail("performance host identity hash is not the authenticated collector identity")
        if record.get("subject_identity_sha256") != expected_subject_hash:
            fail("performance subject identity hash is not its frozen app identity")
        if inputs["font_sha256"] != hashlib.sha256(
            metadata["host"]["terminal_font_selected"].encode("utf-8")
        ).hexdigest():
            fail("performance font hash is not derived from authenticated campaign metadata")
        if inputs["grid_sha256"] != hashlib.sha256(
            canonical_json(metadata["host"]["initial_grid"])
        ).hexdigest():
            fail("performance grid hash is not derived from authenticated campaign metadata")
    validate_identity_closure(
        public_path,
        root / display_artifact["relative_path"],
        root / ghostty_artifact["relative_path"],
        root / preconditions_artifact["relative_path"],
        metadata,
    )
    replay_fields = parse_unique_tsv_bytes(replay_path.read_bytes(), "final identity replay evidence")
    expected = {
        "schema": "spaceterm.acceptance.final-identity-replay/v1",
        "run_id": metadata["run_id"],
        "command": fresh_replay["command"],
        "status": "PASS",
        "verifier_sha256": fresh_replay["verifier_sha256"],
        "public_identity_sha256": fresh_replay["public_identity_sha256"],
        "stdout_sha256": fresh_replay["stdout_sha256"],
        "stderr_sha256": fresh_replay["stderr_sha256"],
    }
    for key, value in expected.items():
        if replay_fields.get(key) != value:
            fail(f"published final identity replay evidence is invalid or stale: {key}")
    validate_utc(replay_fields.get("completed_utc"), "published identity replay completion")
    if not {public_id, replay_id, display_id, ghostty_id, preconditions_id} <= set(package_record["artifacts"]):
        fail("package-identity record does not own its public identity evidence")
    if package_record["status"] == "PASS":
        require_keys(identity, NATIVE_PUBLIC_EVIDENCE_IDS, "PASS native identity evidence")
        native_artifacts = {key: artifacts.get(identity[key]) for key in NATIVE_PUBLIC_EVIDENCE_IDS}
        if any(
            artifact is None
            or artifact["record_id"] != package_record["record_id"]
            or identity[key] not in package_record["artifacts"]
            or not artifact["relative_path"].startswith(
                "identity/issue-43-package-identity-evidence/"
            )
            for key, artifact in native_artifacts.items()
        ):
            fail("PASS public native/runtime/failure replay evidence is missing or misowned")
        closure = [root / "identity" / name for name in (
            "native-observation.tsv", "runtime-metadata.tsv", "runtime-samples.tsv",
            "runtime-events.tsv", "failure-actions.tsv",
        )]
        identity_fields = read_identity_manifest(public_path)
        validate_native_failure_closure(
            closure[0], closure[1], closure[2], closure[3], closure[4],
            identity_fields, metadata,
        )
        expected_receipt = native_closure_receipt(*closure, identity_fields, metadata["run_id"])
        receipt_artifact = native_artifacts["native_closure_replay_artifact_id"]
        assert receipt_artifact is not None
        if (root / receipt_artifact["relative_path"]).read_bytes() != expected_receipt:
            fail("published native closure replay receipt is stale")
        source_indexes = {
            "native_runtime_metadata_artifact_id": 1,
            "native_runtime_samples_artifact_id": 2,
            "native_runtime_events_artifact_id": 3,
            "native_failure_actions_artifact_id": 4,
        }
        for key, source_index in source_indexes.items():
            artifact = native_artifacts[key]
            assert artifact is not None
            if (root / artifact["relative_path"]).read_bytes() != closure[source_index].read_bytes():
                fail("published native runtime/failure export differs from authenticated collector bytes")


def validate_public_identity_evidence(
    root: Path,
    metadata: dict[str, Any],
    rows: dict[str, dict[str, str]],
    leaves: dict[tuple[str, str], dict[str, Any]],
    identity_replay: dict[str, Any],
) -> None:
    identity = metadata["identity_evidence"]
    public_id = identity["public_identity_artifact_id"]
    replay_id = identity["final_identity_replay_artifact_id"]
    display_id = identity["display_summary_artifact_id"]
    ghostty_id = identity["ghostty_identity_artifact_id"]
    preconditions_id = identity["host_preconditions_artifact_id"]
    public_row = rows.get(public_id)
    replay_row = rows.get(replay_id)
    display_row = rows.get(display_id)
    ghostty_row = rows.get(ghostty_id)
    preconditions_row = rows.get(preconditions_id)
    package_record = leaves[("package-identity", "spaceterm")]
    if (
        public_row is None
        or replay_row is None
        or display_row is None
        or ghostty_row is None
        or preconditions_row is None
        or any(
            row["record_id"] != package_record["record_id"]
            or not row["relative_path"].startswith("identity/")
            for row in (public_row, replay_row, display_row, ghostty_row, preconditions_row)
        )
        or not {public_id, replay_id, display_id, ghostty_id, preconditions_id} <= set(package_record["artifacts"])
    ):
        fail("public identity evidence is missing, stale, or misowned")
    public_path = root / public_row["relative_path"]
    replay_path = root / replay_row["relative_path"]
    if public_row["sha256"] != identity_replay["public_identity_sha256"]:
        fail("public identity artifact disagrees with the final identity replay")
    verify_collector_identity(root, metadata, public_path)
    validate_identity_closure(
        public_path,
        root / display_row["relative_path"],
        root / ghostty_row["relative_path"],
        root / preconditions_row["relative_path"],
        metadata,
    )
    replay_fields = parse_unique_tsv_bytes(replay_path.read_bytes(), "public final identity replay")
    expected = {
        "schema": "spaceterm.acceptance.final-identity-replay/v1",
        "run_id": metadata["run_id"],
        "command": identity_replay["command"],
        "status": "PASS",
        "verifier_sha256": identity_replay["verifier_sha256"],
        "public_identity_sha256": identity_replay["public_identity_sha256"],
        "stdout_sha256": identity_replay["stdout_sha256"],
        "stderr_sha256": identity_replay["stderr_sha256"],
    }
    for key, value in expected.items():
        if replay_fields.get(key) != value:
            fail(f"public identity replay artifact is invalid or stale: {key}")
    validate_utc(replay_fields.get("completed_utc"), "public identity replay artifact completion")
    if package_record["status"] == "PASS":
        require_keys(identity, NATIVE_PUBLIC_EVIDENCE_IDS, "public PASS native identity evidence")
        native_rows = {key: rows.get(identity[key]) for key in NATIVE_PUBLIC_EVIDENCE_IDS}
        if any(
            row is None
            or row["record_id"] != package_record["record_id"]
            or identity[key] not in package_record["artifacts"]
            or not row["relative_path"].startswith(
                "identity/issue-43-package-identity-evidence/"
            )
            for key, row in native_rows.items()
        ):
            fail("public PASS native/runtime/failure replay evidence is missing or misowned")
        receipt_id = identity["native_closure_replay_artifact_id"]
        row = native_rows["native_closure_replay_artifact_id"]
        assert row is not None
        receipt = parse_unique_tsv_bytes(
            (root / row["relative_path"]).read_bytes(), "public native closure replay receipt"
        )
        identity_fields = read_identity_manifest(public_path)
        runtime_row = native_rows["native_runtime_metadata_artifact_id"]
        samples_row = native_rows["native_runtime_samples_artifact_id"]
        events_row = native_rows["native_runtime_events_artifact_id"]
        failure_row = native_rows["native_failure_actions_artifact_id"]
        assert runtime_row is not None and samples_row is not None \
            and events_row is not None and failure_row is not None
        validate_published_runtime_failure_closure(
            root / runtime_row["relative_path"], root / samples_row["relative_path"],
            root / events_row["relative_path"], root / failure_row["relative_path"],
            identity_fields, metadata["run_id"], metadata["frozen_artifact"]["app_bundle_sha256"],
        )
        expected = {
            "schema": "spaceterm.acceptance.native-closure-replay/v1",
            "run_id": metadata["run_id"], "status": "PASS",
            "producer_commit": NATIVE_FAILURE_PRODUCER_COMMIT,
            "native_observation_sha256": identity_fields["native.observation.sha256"],
            "provisional_observation_sha256":
                identity_fields["native.provisional.observation.sha256"],
            "runtime_metadata_sha256": identity_fields["native.runtime.metadata.sha256"],
            "failure_actions_sha256": identity_fields["native.failure.actions.sha256"],
            "failure_result_schema": "spaceterm.acceptance.failure-action-result/v2",
        }
        expected_keys = set(expected) | {
            "runtime_samples_sha256", "runtime_events_sha256",
        }
        if set(receipt) != expected_keys \
                or any(receipt.get(key) != value for key, value in expected.items()) \
                or not HASH_RE.fullmatch(receipt.get("runtime_samples_sha256", "")) \
                or not HASH_RE.fullmatch(receipt.get("runtime_events_sha256", "")):
            fail("public native closure replay receipt contradicts run identity v2")
        if receipt["runtime_samples_sha256"] != samples_row["sha256"] \
                or receipt["runtime_events_sha256"] != events_row["sha256"]:
            fail("public native closure receipt does not bind the replayed runtime streams")


def validate_public_url(url: str) -> urllib.parse.SplitResult:
    parsed = urllib.parse.urlsplit(url)
    if (
        parsed.scheme != "https" or not parsed.netloc or parsed.username or parsed.password
        or parsed.port not in {None, 443}
        or parsed.query or parsed.fragment
    ):
        fail("public evidence URL must be credential-free HTTPS")
    hostname = parsed.hostname or ""
    allowed_hosts = {
        "github.com", "api.github.com", "raw.githubusercontent.com",
        "objects.githubusercontent.com", "objects-origin.githubusercontent.com",
        "user-images.githubusercontent.com",
    }
    if hostname.lower() not in allowed_hosts:
        fail("public evidence URL must use an approved public GitHub host")
    if hostname.lower() == "localhost" or hostname.endswith(".localhost"):
        fail("public evidence URL must not target localhost")
    try:
        addresses = {
            item[4][0] for item in socket.getaddrinfo(hostname, parsed.port or 443, type=socket.SOCK_STREAM)
        }
    except socket.gaierror as error:
        fail(f"public evidence URL hostname cannot be resolved: {error}")
    if not addresses or any(not ipaddress.ip_address(address).is_global for address in addresses):
        fail("public evidence URL resolves to a private, loopback, or non-global address")
    return parsed


class ValidatingRedirectHandler(urllib.request.HTTPRedirectHandler):
    def redirect_request(
        self, request: urllib.request.Request, file_pointer: Any, code: int, message: str,
        headers: Any, new_url: str,
    ) -> urllib.request.Request | None:
        old = validate_public_url(request.full_url)
        new = validate_public_url(new_url)
        if request.has_header("Authorization") and (old.scheme, old.netloc) != (new.scheme, new.netloc):
            fail("authenticated public JSON requests cannot redirect across origins")
        return super().redirect_request(request, file_pointer, code, message, headers, new_url)


def open_public_request(request: urllib.request.Request, *, timeout: int) -> Any:
    return urllib.request.build_opener(ValidatingRedirectHandler()).open(request, timeout=timeout)


def remote_sha256(url: str, expected_bytes: int | None = None) -> tuple[str, int]:
    validate_public_url(url)
    request = urllib.request.Request(url, headers={"User-Agent": "SpaceTerm-issue-43-verifier/1"})
    digest = hashlib.sha256()
    total = 0
    try:
        with open_public_request(request, timeout=60) as response:
            validate_public_url(response.geturl())
            if getattr(response, "status", 200) != 200:
                fail("public evidence URL did not return HTTP 200")
            while True:
                block = response.read(1024 * 1024)
                if not block:
                    break
                digest.update(block)
                total += len(block)
                if expected_bytes is not None and total > expected_bytes:
                    fail("public evidence URL exceeds the frozen byte count")
                if expected_bytes is None and total > 16 * 1024 * 1024:
                    fail("public evidence URL exceeds the bounded metadata size")
    except (OSError, urllib.error.URLError) as error:
        fail(f"public evidence URL is inaccessible: {error}")
    return digest.hexdigest(), total


PUBLIC_JSON_CACHE: dict[str, dict[str, Any]] = {}


def fetch_public_json(url: str, maximum_bytes: int = 1024 * 1024) -> dict[str, Any]:
    if url in PUBLIC_JSON_CACHE:
        return PUBLIC_JSON_CACHE[url]
    validate_public_url(url)
    headers = {"User-Agent": "SpaceTerm-issue-43-verifier/1", "Accept": "application/json"}
    token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
    if token:
        headers["Authorization"] = f"Bearer {token}"
    request = urllib.request.Request(url, headers=headers)
    try:
        with open_public_request(request, timeout=60) as response:
            validate_public_url(response.geturl())
            if getattr(response, "status", 200) != 200:
                fail("public JSON evidence URL did not return HTTP 200")
            payload = response.read(maximum_bytes + 1)
    except (OSError, urllib.error.URLError) as error:
        fail(f"public JSON evidence URL is inaccessible: {error}")
    if len(payload) > maximum_bytes:
        fail("public JSON evidence exceeds its bounded size")
    try:
        value = json.loads(
            payload.decode("utf-8"),
            parse_constant=lambda constant: fail(f"non-finite JSON number: {constant}"),
        )
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"public JSON evidence is invalid: {error}")
    if not isinstance(value, dict):
        fail("public JSON evidence is not an object")
    PUBLIC_JSON_CACHE[url] = value
    return value


def fetch_issue_comment_body(url: str) -> str:
    parsed = validate_public_url(url)
    if (
        parsed.hostname != "api.github.com"
        or not re.fullmatch(r"/repos/sadiksaifi/SpaceTerm/issues/comments/[1-9][0-9]*", parsed.path)
    ):
        fail("issue comment anchor must be the public GitHub API URL for this repository")
    document = fetch_public_json(url)
    created = document.get("created_at")
    updated = document.get("updated_at")
    validate_utc(created, "GitHub issue anchor creation timestamp")
    validate_utc(updated, "GitHub issue anchor update timestamp")
    if (
        document.get("html_url", "").startswith(
            "https://github.com/sadiksaifi/SpaceTerm/issues/43#issuecomment-"
        ) is not True
        or not isinstance(document.get("body"), str)
        or not isinstance(document.get("user"), dict)
        or document.get("author_association") not in {"OWNER", "MEMBER", "COLLABORATOR"}
        or created != updated
    ):
        fail("GitHub comment anchor is not an immutable member-authored issue #43 comment")
    return document["body"]


def validate_github_review_url(review: dict[str, Any]) -> None:
    url = str(review.get("review_url", ""))
    parsed = validate_public_url(url)
    if (
        parsed.hostname != "api.github.com"
        or not re.fullmatch(r"/repos/sadiksaifi/SpaceTerm/issues/comments/[1-9][0-9]*", parsed.path)
    ):
        fail("manual PASS review must use a public GitHub issue-comment API URL")


def review_batch_entries(reviews: list[dict[str, Any]], kind: str) -> list[dict[str, str]]:
    if kind == "records":
        return [
            {
                "record_id": item["record_id"],
                "record_sha256": item["record_sha256"],
                "artifact_inventory_sha256": item["artifact_inventory_sha256"],
                "campaign_metadata_sha256": item["campaign_metadata_sha256"],
            }
            for item in sorted(reviews, key=lambda value: value["record_id"])
        ]
    if kind == "artifacts":
        return [
            {
                "artifact_id": item["artifact_id"],
                "artifact_sha256": item["artifact_sha256"],
            }
            for item in sorted(reviews, key=lambda value: value["artifact_id"])
        ]
    fail("unknown manual review batch kind")


def review_batch_body(reviews: list[dict[str, Any]], kind: str) -> str:
    if not reviews:
        fail("manual review batch cannot be empty")
    first = reviews[0]
    entries = review_batch_entries(reviews, kind)
    digest = hashlib.sha256(canonical_json(entries)).hexdigest()
    return (
        "SpaceTerm issue #43 manual review batch/v1\n"
        f"role: {first['reviewer_role']}\n"
        f"reviewer: {first['reviewer']}\n"
        f"kind: {kind}\n"
        f"entry_count: {len(entries)}\n"
        f"entries_sha256: {digest}\n"
        "decision: PASS\n"
        f"attestation: {first['attestation']}"
    )


def verify_github_review_batch(
    reviews: list[dict[str, Any]], kind: str, *, fetch: bool,
) -> None:
    if not reviews:
        fail("manual review batch cannot be empty")
    first = reviews[0]
    shared = (first.get("reviewer_role"), first.get("reviewer"), first.get("review_url"),
              first.get("reviewed_utc"), first.get("attestation"))
    if any(
        (item.get("reviewer_role"), item.get("reviewer"), item.get("review_url"),
         item.get("reviewed_utc"), item.get("attestation")) != shared
        for item in reviews
    ):
        fail(f"{kind} manual reviews must share one immutable authenticated batch comment")
    for item in reviews:
        validate_github_review_url(item)
    if not fetch:
        return
    url = str(first["review_url"])
    document = fetch_public_json(url)
    reviewer = str(first["reviewer"])
    login = reviewer.removeprefix("github:")
    if (
        document.get("html_url", "").startswith(
            "https://github.com/sadiksaifi/SpaceTerm/issues/43#issuecomment-"
        ) is not True
        or not isinstance(document.get("user"), dict)
        or document["user"].get("login", "").lower() != login.lower()
        or document.get("author_association") not in {"OWNER", "MEMBER", "COLLABORATOR"}
    ):
        fail("manual review author is not the named repository owner/member/collaborator")
    created = document.get("created_at")
    updated = document.get("updated_at")
    validate_utc(created, "GitHub manual review creation timestamp")
    validate_utc(updated, "GitHub manual review update timestamp")
    if created != updated or first.get("reviewed_utc") != created:
        fail("manual review timestamp is not the immutable GitHub comment creation time")
    if document.get("body") != review_batch_body(reviews, kind):
        fail("manual review batch does not attest the exact immutable evidence inventory")


def verify_public_payload_urls(rows: list[dict[str, str]]) -> None:
    for row in rows:
        digest, size = remote_sha256(row["public_url"], int(row["bytes"]))
        if digest != row["sha256"] or size != int(row["bytes"]):
            fail(f"downloaded public payload differs from frozen bytes: {row['artifact_id']}")


def verify_control_urls(root: Path, urls: dict[str, str]) -> None:
    for key, filename in (
        ("campaign", "campaign.yaml"),
        ("artifacts", "artifacts.tsv"),
        ("control", "control.sha256"),
    ):
        path = root / filename
        digest, size = remote_sha256(urls[key], byte_count(path))
        if digest != sha256_path(path) or size != byte_count(path):
            fail(f"downloaded public control file differs from local frozen {filename}")


def enumerate_public_payloads(root: Path, *, include_identity: bool = True) -> set[str]:
    discovered: set[str] = set()
    for directory_name in sorted(PAYLOAD_DIRS):
        if directory_name == "identity" and not include_identity:
            continue
        directory = root / directory_name
        if not directory.is_dir() or directory.is_symlink():
            fail(f"public payload directory is missing or unsafe: {directory_name}")
        for entry in directory.iterdir():
            if entry.is_symlink() or not entry.is_file():
                fail(f"undeclared directory, symlink, or non-file in payload directory: {entry}")
            if entry.stat().st_nlink != 1:
                fail(f"public payload is a hardlink: {entry}")
            discovered.add(f"{directory_name}/{entry.name}")
    return discovered


def validate_public_root_layout(root: Path, *, require_comment: bool) -> None:
    expected_directories = set(PAYLOAD_DIRS)
    expected_files = set(CONTROL_FILES)
    actual_directories: set[str] = set()
    actual_files: set[str] = set()
    for entry in root.iterdir():
        if entry.is_symlink():
            fail(f"public bundle root contains a symlink: {entry.name}")
        if entry.is_dir():
            actual_directories.add(entry.name)
        elif entry.is_file():
            actual_files.add(entry.name)
        else:
            fail(f"public bundle root contains an unsupported entry: {entry.name}")
    if actual_directories != expected_directories:
        fail(
            "public bundle directory inventory is not exact; "
            f"missing={sorted(expected_directories - actual_directories)}, "
            f"extra={sorted(actual_directories - expected_directories)}"
        )
    if actual_files != expected_files:
        fail(
            "public bundle file inventory is not exact; "
            f"missing={sorted(expected_files - actual_files)}, "
            f"extra={sorted(actual_files - expected_files)}"
        )
    if require_comment and actual_files != set(CONTROL_FILES):
        fail("the external issue comment must never be stored inside the evidence bundle")


def validate_matrix_evidence(
    leaves: dict[tuple[str, str], dict[str, Any]],
    artifacts: dict[str, dict[str, Any]],
    root: Path,
) -> None:
    def owned(record: dict[str, Any]) -> list[dict[str, Any]]:
        return [artifacts[item] for item in record["artifacts"]]

    for case_id in MATRIX_CASES["native"]:
        record = leaves[(case_id, "spaceterm")]
        if record["status"] != "NOT-RUN" and not any(
            artifact["media_type"].startswith("image/") for artifact in owned(record)
        ):
            fail(f"executed native row lacks its required screenshot: {case_id}")
    for case_id in MATRIX_CASES["focus"]:
        record = leaves[(case_id, "spaceterm")]
        if record["status"] == "PASS" and not any(
            artifact["media_type"].startswith("video/") for artifact in owned(record)
        ):
            fail(f"focus PASS lacks timed next-frame recording evidence: {case_id}")
        pty_id = record["dec_1004"]["pty_artifact_id"]
        if (
            pty_id not in record["artifacts"]
            or pty_id not in artifacts
            or artifacts[pty_id]["record_id"] != record["record_id"]
            or not artifacts[pty_id]["media_type"].startswith("text/")
        ):
            fail(f"focus record lacks a text PTY byte artifact: {case_id}")
        if record["status"] == "PASS":
            pty_path = root / artifacts[pty_id]["relative_path"]
            ensure_regular_file(pty_path, "focus DEC 1004 byte receipt")
            receipt = parse_unique_tsv_bytes(pty_path.read_bytes(), "focus DEC 1004 receipt")
            dec = record["dec_1004"]
            expected_receipt = {
                "schema": "spaceterm.acceptance.dec1004/v1",
                "record_id": record["record_id"],
                "enable_current_state_bytes_hex": dec["enable_current_state_bytes_hex"],
                "loss_bytes_hex": dec["loss_bytes_hex"],
                "gain_bytes_hex": dec["gain_bytes_hex"],
                "duplicate_reports": str(dec["duplicate_reports"]),
                "held_key_release_bytes_hex": dec["held_key_release_bytes_hex"],
            }
            if receipt != expected_receipt:
                fail(f"focus DEC 1004 record disagrees with exact PTY receipt: {case_id}")
    accessibility = leaves[("capability-accessibility", "spaceterm")]
    if accessibility["status"] == "PASS":
        ax_artifacts = [
            artifact for artifact in owned(accessibility)
            if ARTIFACT_FILE_RE.fullmatch(Path(artifact["relative_path"]).name).group(4) == "ax"  # type: ignore[union-attr]
        ]
        if len(ax_artifacts) != 1:
            fail("capability-accessibility PASS requires one pinned native AX observation")
        validate_native_ax_observation(
            root / ax_artifacts[0]["relative_path"], accessibility
        )
    for case_id in MATRIX_CASES["performance"]:
        spaceterm_record = leaves[(case_id, "spaceterm")]
        ghostty_record = leaves[(case_id, "ghostty")]
        if spaceterm_record["status"] == "PASS" and ghostty_record["status"] == "PASS":
            validate_performance_protocol_pair(
                case_id, spaceterm_record, ghostty_record, artifacts, root,
            )
        for subject in ("spaceterm", "ghostty"):
            record = leaves[(case_id, subject)]
            keys = ["time_profiler_artifact_id", "allocations_artifact_id"]
            if case_id in RENDER_CASES:
                keys.extend(record["screen_artifact_ids"])
            else:
                keys.append("rss_samples_artifact_id")
                keys.extend(record["screen_artifact_ids"])
            for key in keys:
                artifact_id = record[key] if key.endswith("_artifact_id") else key
                if (
                    artifact_id not in record["artifacts"]
                    or artifact_id not in artifacts
                    or artifacts[artifact_id]["record_id"] != record["record_id"]
                ):
                    fail(f"performance extension artifact is missing or misowned: {record['record_id']}")
            if record["status"] == "PASS":
                time_artifact = artifacts[record["time_profiler_artifact_id"]]
                allocation_artifact = artifacts[record["allocations_artifact_id"]]
                if (
                    time_artifact["media_type"] != "application/zip"
                    or allocation_artifact["media_type"] != "application/zip"
                ):
                    fail(f"performance traces must be reviewed ZIP exports: {record['record_id']}")
                if case_id not in RENDER_CASES and not artifacts[
                    record["rss_samples_artifact_id"]
                ]["media_type"].startswith("text/"):
                    fail(f"performance RSS evidence must be text: {record['record_id']}")
                if any(
                    not artifacts[artifact_id]["media_type"].startswith(("image/", "video/"))
                    for artifact_id in record["screen_artifact_ids"]
                ):
                    fail(f"performance representative evidence is not visual: {record['record_id']}")
    for case_id in MATRIX_CASES["package"]:
        record = leaves[(case_id, "spaceterm")]
        if case_id in {"package-window-shell", "package-command-output", "package-resize"}:
            if record["status"] == "PASS" and not any(
                artifact["media_type"].startswith("image/") for artifact in owned(record)
            ):
                fail(f"packaged smoke PASS lacks required visible evidence: {case_id}")
        if record["status"] == "PASS" and case_id in {
            "package-build", "package-final-validate",
        } and not any(artifact["media_type"].startswith("text/") for artifact in owned(record)):
            fail(f"packaged verification PASS lacks its complete text log: {case_id}")
        if record["status"] == "PASS" and case_id == "package-identity":
            media = [artifact["media_type"] for artifact in owned(record)]
            if not any(value.startswith("image/") for value in media) \
                    or not any(value.startswith("text/") for value in media):
                fail("package-identity PASS requires both screenshot and log/identity evidence")


def artifact_rows(artifacts: dict[str, dict[str, Any]], reviews: dict[str, dict[str, Any]],
                  records: dict[str, dict[str, Any]], record_reviews: dict[str, dict[str, Any]],
                  root: Path, state: Path) -> list[dict[str, str]]:
    metadata_path = state / "campaign-metadata.json"
    ensure_regular_file(metadata_path, "campaign metadata")
    metadata_digest = sha256_path(metadata_path)
    for record_id, record in records.items():
        review = record_reviews.get(record_id)
        path = state / "records" / f"{record_id}.json"
        if review is None:
            fail(f"record lacks a manual review: {record_id}")
        if (
            review.get("decision") != "PASS"
            or review.get("reviewed_sha256") != sha256_path(path)
            or review.get("attestation") != RECORD_REVIEW_ATTESTATION
            or review.get("artifact_inventory_sha256") != artifact_inventory_digest(record, artifacts)
            or review.get("campaign_metadata_sha256") != metadata_digest
            or review.get("reviewer_role") != RECORD_REVIEWER_ROLE
            or not REVIEWER_RE.fullmatch(str(review.get("reviewer", "")))
            or review.get("reviewed_utc", "") < max(
                [record["finished_utc"]]
                + [artifacts[artifact_id]["created_utc"] for artifact_id in record["artifacts"]]
            )
        ):
            fail(f"record manual review is rejected, stale, or forged: {record_id}")
        validate_github_review_url(review)

    rows = []
    for artifact_id, artifact in sorted(artifacts.items()):
        review = reviews.get(artifact_id)
        if review is None:
            fail(f"artifact lacks manual privacy review: {artifact_id}")
        path = root / artifact["relative_path"]
        if (
            review.get("decision") != "PASS"
            or review.get("reviewed_sha256") != sha256_path(path)
            or review.get("attestation") != ARTIFACT_REVIEW_ATTESTATION
            or review.get("reviewer_role") != ARTIFACT_REVIEWER_ROLE
            or not REVIEWER_RE.fullmatch(str(review.get("reviewer", "")))
            or review.get("reviewed_utc", "") < artifact["created_utc"]
        ):
            fail(f"artifact privacy review is rejected, stale, or forged: {artifact_id}")
        validate_github_review_url(review)
        row = {key: str(artifact[key]) for key in ARTIFACT_HEADER if key != "privacy_review"}
        row["privacy_review"] = "PASS"
        rows.append(row)
    referenced_artifacts: set[str] = set()
    for record in records.values():
        for artifact_id in record["artifacts"]:
            referenced_artifacts.add(artifact_id)
            if artifact_id not in artifacts:
                fail(f"record references a missing artifact: {record['record_id']} -> {artifact_id}")
            artifact = artifacts[artifact_id]
            if artifact["record_id"] != record["record_id"]:
                fail(f"record references artifact owned by another record: {artifact_id}")
        dec = record.get("dec_1004")
        if isinstance(dec, dict) and dec.get("pty_artifact_id") not in record["artifacts"]:
            fail(f"focus PTY artifact is not in the owning artifact list: {record['record_id']}")
        for key in ("rss_samples_artifact_id", "time_profiler_artifact_id", "allocations_artifact_id"):
            if key in record and record[key] not in record["artifacts"]:
                fail(f"performance {key} is not in the owning artifact list: {record['record_id']}")
        for artifact_id in record.get("screen_artifact_ids", []):
            if artifact_id not in record["artifacts"]:
                fail(f"performance screen artifact is not owned by record: {record['record_id']}")
    if referenced_artifacts != set(artifacts):
        orphaned = sorted(set(artifacts) - referenced_artifacts)
        fail("payload manifest would contain orphan artifacts: " + ", ".join(orphaned))
    expected_non_identity = {
        item["relative_path"] for item in artifacts.values()
        if not item["relative_path"].startswith("identity/")
    }
    if enumerate_public_payloads(root, include_identity=False) != expected_non_identity:
        fail("public payload directories contain missing or undeclared files")
    return rows


def render_artifacts_tsv(rows: list[dict[str, str]]) -> bytes:
    from io import StringIO

    output = StringIO(newline="")
    writer = csv.DictWriter(output, fieldnames=ARTIFACT_HEADER, delimiter="\t", lineterminator="\n")
    writer.writeheader()
    writer.writerows(rows)
    return output.getvalue().encode("utf-8")


def manual_review_public(records: dict[str, dict[str, Any]], record_reviews: dict[str, dict[str, Any]],
                         artifact_reviews: dict[str, dict[str, Any]]) -> dict[str, Any]:
    return {
        "record_reviews": [
            {
                "record_id": record_id,
                "record_sha256": record_reviews[record_id]["reviewed_sha256"],
                "artifact_inventory_sha256": record_reviews[record_id]["artifact_inventory_sha256"],
                "campaign_metadata_sha256": record_reviews[record_id]["campaign_metadata_sha256"],
                "reviewer_role": record_reviews[record_id]["reviewer_role"],
                "reviewer": record_reviews[record_id]["reviewer"],
                "review_url": record_reviews[record_id]["review_url"],
                "reviewed_utc": record_reviews[record_id]["reviewed_utc"],
                "decision": "PASS",
                "attestation": record_reviews[record_id]["attestation"],
            }
            for record_id in sorted(records)
        ],
        "artifact_reviews": [
            {
                "artifact_id": artifact_id,
                "artifact_sha256": artifact_reviews[artifact_id]["reviewed_sha256"],
                "reviewer_role": artifact_reviews[artifact_id]["reviewer_role"],
                "reviewer": artifact_reviews[artifact_id]["reviewer"],
                "review_url": artifact_reviews[artifact_id]["review_url"],
                "reviewed_utc": artifact_reviews[artifact_id]["reviewed_utc"],
                "decision": "PASS",
                "attestation": artifact_reviews[artifact_id]["attestation"],
            }
            for artifact_id in sorted(artifact_reviews)
        ],
    }


def apply_metadata_gates(
    metadata: dict[str, Any],
    campaign_status: str,
    artifacts: dict[str, dict[str, Any]],
    leaves: dict[tuple[str, str], dict[str, Any]],
    records: dict[str, dict[str, Any]],
) -> str:
    bindings = (
        (
            "package verification",
            metadata["frozen_artifact"]["package_verification_artifact"],
            "package-build",
        ),
        (
            "issue #42 conformance prerequisite",
            metadata["issue_42_conformance"]["artifact_id"],
            "package-final-validate",
        ),
        ("final validation", metadata["validation"]["artifact_id"], "package-final-validate"),
    )
    for label, artifact_id, expected_case in bindings:
        artifact = artifacts.get(artifact_id)
        if (
            artifact is None
            or artifact["case_id"] != expected_case
            or artifact["subject"] != "spaceterm"
            or artifact["record_id"] != leaves[(expected_case, "spaceterm")]["record_id"]
        ):
            fail(f"{label} artifact is missing, stale, or owned by the wrong effective record")
    if campaign_status == "PASS" and metadata["validation"]["status"] != "PASS":
        campaign_status = "FAIL"
    if campaign_status == "PASS" and metadata["issue_42_conformance"]["status"] != "PASS":
        campaign_status = "FAIL"
    expected_failure_records = {
        record["record_id"]: record
        for record in records.values()
        if record["subject"] == "spaceterm" and record["status"] == "FAIL"
    }
    supplied_failure_ids = [
        item.get("record_id") for item in metadata["known_deviations"]
        if isinstance(item, dict)
    ]
    if len(supplied_failure_ids) != len(set(supplied_failure_ids)):
        fail("known deviations contain a duplicate failing record ID")
    if set(supplied_failure_ids) != set(expected_failure_records):
        missing = sorted(set(expected_failure_records) - set(supplied_failure_ids))
        extra = sorted(set(supplied_failure_ids) - set(expected_failure_records))
        fail(f"known deviations do not exactly cover SpaceTerm FAIL history; missing={missing}, extra={extra}")
    for index, deviation in enumerate(metadata["known_deviations"]):
        if not isinstance(deviation, dict):
            fail(f"known_deviations[{index}] must be an object")
        require_keys(
            deviation,
            ("case_id", "record_id", "smallest_reproduction", "follow_up_issue", "status"),
            f"known_deviations[{index}]",
        )
        if deviation["status"] not in {"open", "fixed-and-rerun"}:
            fail("known deviation status is invalid")
        if deviation["case_id"] not in ALL_CASES:
            fail("known deviation uses an unknown case ID")
        referenced = records.get(deviation["record_id"])
        if (
            referenced is None
            or referenced["case_id"] != deviation["case_id"]
            or referenced["subject"] != "spaceterm"
            or referenced["status"] != "FAIL"
        ):
            fail("known deviation must reference an existing SpaceTerm FAIL record in its case")
        require_nonempty(deviation["smallest_reproduction"], "known deviation reproduction")
        if deviation["smallest_reproduction"] != referenced["smallest_reproduction"]:
            fail("known deviation reproduction disagrees with the immutable failing record")
        follow_up = str(deviation["follow_up_issue"])
        parsed_follow_up = validate_public_url(follow_up)
        if (
            parsed_follow_up.hostname != "github.com"
            or not re.fullmatch(
                r"/sadiksaifi/SpaceTerm/issues/[1-9][0-9]*", parsed_follow_up.path
            )
        ):
            fail("known deviation follow-up must be a public SpaceTerm GitHub issue")
        issue_number = parsed_follow_up.path.rsplit("/", 1)[-1]
        if issue_number == "43":
            fail("known deviation follow-up must be a distinct issue, not the acceptance issue")
        issue = fetch_public_json(
            f"https://api.github.com/repos/sadiksaifi/SpaceTerm/issues/{issue_number}"
        )
        if issue.get("html_url") != follow_up or issue.get("state") not in {"open", "closed"}:
            fail("known deviation follow-up issue identity/state cannot be authenticated")
        leaf = leaves.get((deviation["case_id"], "spaceterm"))
        fixed = (
            leaf is not None
            and leaf["status"] == "PASS"
            and leaf["attempt"] > referenced["attempt"]
        )
        expected_deviation_status = "fixed-and-rerun" if fixed else "open"
        if deviation["status"] != expected_deviation_status:
            fail("known deviation status is not derived from the effective rerun graph")
        if (fixed and issue["state"] != "closed") or (not fixed and issue["state"] != "open"):
            fail("known deviation follow-up GitHub state contradicts the rerun graph")
        if not fixed:
            campaign_status = "FAIL"
    return campaign_status


def validate_publication_capacity(rows: Iterable[dict[str, Any]], disk_probe: Path) -> int:
    try:
        total_payload_bytes = sum(int(row["bytes"]) for row in rows)
    except (KeyError, TypeError, ValueError):
        fail("campaign payload inventory contains a non-integer byte count")
    if total_payload_bytes > MAX_CAMPAIGN_PAYLOAD_BYTES:
        fail("campaign payloads exceed the bounded 4 GiB publication budget")
    required_free_bytes = total_payload_bytes + 512 * 1024 * 1024
    if shutil.disk_usage(disk_probe).free < required_free_bytes:
        fail("campaign publication lacks payload-size plus 512 MiB free-space headroom")
    return total_payload_bytes


def command_finalize(args: argparse.Namespace) -> None:
    root = resolve_root(args.run_id)
    if root.name.startswith(".acceptance-identity."):
        fail("quit the authenticated mounted app and let the collector perform the final RUN_DIR rename before finalization")
    with campaign_lock(root):
        state = require_initialized(root, args.run_id)
        metadata_path = state / "campaign-metadata.json"
        ensure_regular_file(metadata_path, "campaign metadata")
        metadata = validate_metadata(read_json(metadata_path), args.run_id)
        verify_frozen_repository_tools(metadata, require_clean_head=True)
        verify_collector_identity(root, metadata)
        identity_replay = run_final_identity_replay(root)
        records = load_records(state, args.run_id)
        leaves, campaign_status = validate_graph(records)
        validate_campaign_conditionals(metadata, leaves)
        artifacts = load_artifacts(state, records, root, args.run_id)
        validate_identity_evidence(root, metadata, artifacts, leaves, identity_replay)
        artifact_reviews = load_reviews(state / "artifact-reviews", "artifact_id")
        record_reviews = load_reviews(state / "record-reviews", "record_id")
        rows = artifact_rows(artifacts, artifact_reviews, records, record_reviews, root, state)
        disk_probe = publication_parent() if publication_parent().exists() else acceptance_parent()
        validate_publication_capacity(rows, disk_probe)
        validate_matrix_evidence(leaves, artifacts, root)
        if set(artifact_reviews) != set(artifacts):
            fail("artifact manual-review inventory disagrees with artifact inventory")
        if set(record_reviews) != set(records):
            fail("record manual-review inventory disagrees with record inventory")
        payload = render_artifacts_tsv(rows)
        manifest_digest = hashlib.sha256(payload).hexdigest()

        campaign_status = apply_metadata_gates(metadata, campaign_status, artifacts, leaves, records)
        verify_public_payload_urls(rows)

        campaign = dict(metadata)
        finished_utc = args.finished_utc or utc_now()
        reject_future_utc(finished_utc, "campaign finished_utc")
        campaign.update(
            {
                "campaign_status": campaign_status,
                "finished_utc": finished_utc,
                "identity_replay": identity_replay,
                "case_results": [records[key] for key in sorted(records)],
                "manual_review": manual_review_public(records, record_reviews, artifact_reviews),
                "payload_manifest": {
                    "path": "artifacts.tsv",
                    "sha256": manifest_digest,
                    "payload_rows": len(rows),
                    "excluded_control_files": ["campaign.yaml", "artifacts.tsv", "control.sha256"],
                    "privacy_review": "PASS",
                },
                "control_digest": {
                    "path": "control.sha256",
                    "algorithm": "sha256",
                    "entries_in_order": ["campaign.yaml", "artifacts.tsv"],
                    "digest_anchored_in": "final GitHub issue comment",
                },
            }
        )
        validate_utc(campaign["finished_utc"], "campaign finished_utc")
        if campaign["finished_utc"] < campaign["started_utc"]:
            fail("campaign finished_utc precedes started_utc")
        if campaign["identity_replay"]["completed_utc"] > campaign["finished_utc"]:
            fail("campaign finished_utc precedes the authenticated final identity replay")
        privacy_scan(campaign, "generated campaign")
        campaign_payload = canonical_json(campaign)
        campaign_digest = hashlib.sha256(campaign_payload).hexdigest()
        control = f"{campaign_digest}  campaign.yaml\n{manifest_digest}  artifacts.tsv\n".encode("ascii")
        staging, final_root, staging_parent = create_publication_staging(args.run_id)
        try:
            copy_public_payloads(root, staging, artifacts)
            write_exclusive(staging / "artifacts.tsv", payload, 0o444)
            write_exclusive(staging / "campaign.yaml", campaign_payload, 0o444)
            write_exclusive(staging / "control.sha256", control, 0o444)
            verify_bundle(staging, require_comment=False)
            public_root = publish_staging(staging, final_root, staging_parent)
        except BaseException:
            shutil.rmtree(staging_parent, ignore_errors=True)
            raise
        print(f"Finalized issue #43 campaign {args.run_id}: {campaign_status}")
        print(f"Public evidence bundle: {public_root}")
        print(f"control.sha256 SHA-256: {sha256_path(public_root / 'control.sha256')}")


def parse_manifest(path: Path) -> list[dict[str, str]]:
    ensure_regular_file(path, "payload manifest")
    if byte_count(path) > 8 * 1024 * 1024:
        fail("artifacts.tsv exceeds the bounded 8 MiB manifest size")
    try:
        with path.open("r", encoding="utf-8", newline="") as handle:
            reader = csv.DictReader(handle, delimiter="\t")
            if tuple(reader.fieldnames or ()) != ARTIFACT_HEADER:
                fail("artifacts.tsv header is not exact")
            rows = list(reader)
    except (OSError, UnicodeError, csv.Error) as error:
        fail(f"artifacts.tsv cannot be parsed: {error}")
    if not rows:
        fail("artifacts.tsv must contain at least one payload row")
    if len(rows) > 1024:
        fail("artifacts.tsv exceeds the bounded 1024-payload inventory")
    if any(set(row) != set(ARTIFACT_HEADER) or None in row for row in rows):
        fail("artifacts.tsv contains a row with the wrong field count")
    return rows


def validate_public_campaign(campaign: Any, rows: list[dict[str, str]], root: Path) -> None:
    if not isinstance(campaign, dict):
        fail("campaign.yaml must be a YAML-compatible JSON object")
    require_keys(
        campaign,
        (
            "schema_version", "issue", "run_id", "campaign_status", "started_utc",
            "finished_utc", "case_results", "manual_review", "payload_manifest",
            "control_digest", "identity_replay",
        ),
        "public campaign",
    )
    metadata = {key: value for key, value in campaign.items() if key not in {
        "campaign_status", "finished_utc", "case_results", "manual_review", "payload_manifest",
        "control_digest", "identity_replay",
    }}
    validate_metadata(metadata, campaign["run_id"])
    metadata_digest = hashlib.sha256(canonical_json(metadata)).hexdigest()
    if campaign.get("campaign_status") == "PASS":
        verify_frozen_repository_tools(metadata, require_clean_head=False)
    identity_replay = campaign["identity_replay"]
    if not isinstance(identity_replay, dict):
        fail("identity_replay must be an object")
    require_keys(
        identity_replay,
        (
            "command", "status", "completed_utc", "verifier_sha256",
            "public_identity_sha256", "stdout_sha256", "stderr_sha256",
        ),
        "identity_replay",
    )
    if (
        identity_replay["command"]
        != "scripts/acceptance-identity.sh verify --run-dir $RUN_DIR --final"
        or identity_replay["status"] != "PASS"
    ):
        fail("campaign lacks the exact authenticated final identity replay")
    validate_utc(identity_replay["completed_utc"], "identity replay completion")
    if identity_replay["completed_utc"] > campaign["finished_utc"]:
        fail("identity replay completion falls after campaign completion")
    for key in ("verifier_sha256", "public_identity_sha256", "stdout_sha256", "stderr_sha256"):
        if not HASH_RE.fullmatch(str(identity_replay[key])):
            fail(f"identity_replay.{key} is invalid")
    identity_path = root / "public-run-identity.tsv"
    if identity_path.is_file() and identity_replay["public_identity_sha256"] != sha256_path(identity_path):
        fail("public collector identity changed after authenticated final replay")
    records = {}
    for item in campaign["case_results"]:
        record = validate_record(item, campaign["run_id"])
        if record["record_id"] in records:
            fail("public campaign contains duplicate record IDs")
        records[record["record_id"]] = record
    leaves, computed_status = validate_graph(records)
    validate_campaign_conditionals(metadata, leaves)
    for record in records.values():
        if record["started_utc"] < campaign["started_utc"] or record["finished_utc"] > campaign["finished_utc"]:
            fail("record timestamp falls outside the campaign interval")
    review = campaign["manual_review"]
    if not isinstance(review, dict):
        fail("public manual_review is missing")
    require_keys(review, ("record_reviews", "artifact_reviews"), "public manual_review")
    if not isinstance(review["record_reviews"], list) or any(
        not isinstance(item, dict) for item in review["record_reviews"]
    ):
        fail("public record_reviews must be a list of objects")
    if not isinstance(review["artifact_reviews"], list) or any(
        not isinstance(item, dict) for item in review["artifact_reviews"]
    ):
        fail("public artifact_reviews must be a list of objects")
    record_review_ids = [item.get("record_id") for item in review["record_reviews"]]
    if set(record_review_ids) != set(records) or len(record_review_ids) != len(records):
        fail("public record review inventory is incomplete or duplicated")
    row_ids = [row["artifact_id"] for row in rows]
    if len(row_ids) != len(set(row_ids)):
        fail("artifacts.tsv contains duplicate artifact IDs")
    row_by_id = {row["artifact_id"]: row for row in rows}
    try:
        total_payload_bytes = sum(int(row["bytes"]) for row in rows)
    except ValueError:
        fail("public evidence payload inventory contains a non-integer byte count")
    if total_payload_bytes > MAX_CAMPAIGN_PAYLOAD_BYTES:
        fail("public evidence payload inventory exceeds the bounded 4 GiB campaign size")
    for item in review["record_reviews"]:
        if item.get("decision") != "PASS" or not HASH_RE.fullmatch(str(item.get("record_sha256", ""))):
            fail("public record review is invalid")
        if not HASH_RE.fullmatch(str(item.get("artifact_inventory_sha256", ""))):
            fail("public record review lacks its artifact-inventory digest")
        if item.get("campaign_metadata_sha256") != metadata_digest:
            fail("public record review does not bind the exact campaign metadata")
        if item.get("reviewer_role") != RECORD_REVIEWER_ROLE \
                or not REVIEWER_RE.fullmatch(str(item.get("reviewer", ""))):
            fail("public record review lacks its exact role or explicit reviewer identity")
        if item.get("attestation") != RECORD_REVIEW_ATTESTATION:
            fail("public record review attestation is not exact")
        validate_utc(item.get("reviewed_utc"), "public record review timestamp")
        record = records[item["record_id"]]
        latest_artifact_utc = max(
            row_by_id[artifact_id]["created_utc"] for artifact_id in record["artifacts"]
        )
        if item["reviewed_utc"] < max(record["finished_utc"], latest_artifact_utc) \
                or item["reviewed_utc"] > campaign["finished_utc"]:
            fail("public record review falls outside its valid post-evidence interval")
        expected = hashlib.sha256(canonical_json(records[item["record_id"]])).hexdigest()
        if item["record_sha256"] != expected:
            fail("public record review hash is stale")
        expected_inventory = artifact_inventory_digest(records[item["record_id"]], row_by_id)
        if item["artifact_inventory_sha256"] != expected_inventory:
            fail("public record review artifact inventory is stale")
        validate_github_review_url(item)

    artifact_review_by_id = {item.get("artifact_id"): item for item in review["artifact_reviews"]}
    if set(artifact_review_by_id) != set(row_ids) or len(artifact_review_by_id) != len(row_ids):
        fail("public artifact review inventory is incomplete or duplicated")
    record_reviewers = {item.get("reviewer") for item in review["record_reviews"]}
    artifact_reviewers = {item.get("reviewer") for item in review["artifact_reviews"]}
    if record_reviewers & artifact_reviewers:
        fail("case-observation and artifact-privacy review roles require distinct reviewers")
    manifest_paths: set[str] = set()
    for row in rows:
        privacy_scan(row, f"payload manifest row {row.get('artifact_id', 'unknown')}")
        if row["relative_path"] in manifest_paths:
            fail("artifacts.tsv contains duplicate payload paths")
        manifest_paths.add(row["relative_path"])
        if row["run_id"] != campaign["run_id"] or row["privacy_review"] != "PASS":
            fail("payload manifest run identity or privacy review is invalid")
        if not HASH_RE.fullmatch(row["sha256"]) or not re.fullmatch(r"0|[1-9][0-9]*", row["bytes"]):
            fail("payload manifest hash or byte count is invalid")
        validate_utc(row["created_utc"], "payload created_utc")
        if row["created_utc"] < campaign["started_utc"] or row["created_utc"] > campaign["finished_utc"]:
            fail("payload timestamp falls outside the campaign interval")
        if not re.fullmatch(r"[a-z0-9][a-z0-9.+-]*/[a-z0-9][a-z0-9.+-]*", row["media_type"]):
            fail("payload manifest media type is invalid")
        if row["content_class"] not in {
            "content-free", "deterministic-fixture", "protocol-bytes",
        }:
            fail("payload manifest content class is not explicitly safe")
        for key in ("producer", "producer_version", "redaction_notes"):
            require_nonempty(row[key], f"payload manifest {key}")
        if row["record_id"] not in records:
            fail("payload manifest references an unknown record")
        record = records[row["record_id"]]
        if row["subject"] != record["subject"] or row["case_id"] != record["case_id"]:
            fail("payload manifest ownership disagrees with its record")
        if not row["public_url"].startswith("https://"):
            fail("payload manifest contains a non-HTTPS public URL")
        path, kind = validate_relative_payload(root, row["relative_path"], record)
        if row["artifact_id"] != f"{record['record_id']}-{kind}":
            fail("payload artifact ID disagrees with its deterministic path")
        if row["sha256"] != sha256_path(path) or row["bytes"] != str(byte_count(path)):
            fail("payload artifact bytes changed after finalization")
        validate_payload_format(path, row, record)
        artifact_review = artifact_review_by_id[row["artifact_id"]]
        if artifact_review.get("decision") != "PASS" or artifact_review.get("artifact_sha256") != row["sha256"]:
            fail("public artifact review is invalid or stale")
        if artifact_review.get("reviewer_role") != ARTIFACT_REVIEWER_ROLE \
                or not REVIEWER_RE.fullmatch(str(artifact_review.get("reviewer", ""))):
            fail("public artifact review lacks its exact role or explicit reviewer identity")
        if artifact_review.get("attestation") != ARTIFACT_REVIEW_ATTESTATION:
            fail("public artifact review attestation is not exact")
        validate_utc(artifact_review.get("reviewed_utc"), "public artifact review timestamp")
        if artifact_review["reviewed_utc"] < row["created_utc"] \
                or artifact_review["reviewed_utc"] > campaign["finished_utc"]:
            fail("public artifact review falls outside its valid post-creation interval")
        validate_github_review_url(artifact_review)
    verify_github_review_batch(review["record_reviews"], "records", fetch=True)
    verify_github_review_batch(review["artifact_reviews"], "artifacts", fetch=True)
    for record in records.values():
        for artifact_id in record["artifacts"]:
            if artifact_id not in row_by_id or row_by_id[artifact_id]["record_id"] != record["record_id"]:
                fail("public record artifact inventory is incomplete or misowned")
    referenced = {artifact_id for record in records.values() for artifact_id in record["artifacts"]}
    if referenced != set(row_by_id):
        fail("payload manifest contains orphan rows")
    validate_public_identity_evidence(root, metadata, row_by_id, leaves, identity_replay)
    if enumerate_public_payloads(root) != manifest_paths:
        fail("public payload directories contain missing or undeclared files")
    validate_matrix_evidence(leaves, row_by_id, root)
    computed_status = apply_metadata_gates(metadata, computed_status, row_by_id, leaves, records)
    if campaign["campaign_status"] != computed_status:
        fail("campaign_status was not derived from the effective inventory and metadata gates")

    manifest = campaign["payload_manifest"]
    expected_manifest_hash = sha256_path(root / "artifacts.tsv")
    if (
        manifest.get("path") != "artifacts.tsv"
        or manifest.get("sha256") != expected_manifest_hash
        or manifest.get("payload_rows") != len(rows)
        or manifest.get("excluded_control_files") != ["campaign.yaml", "artifacts.tsv", "control.sha256"]
        or manifest.get("privacy_review") != "PASS"
    ):
        fail("campaign payload_manifest binding is invalid")
    control = campaign["control_digest"]
    if control != {
        "path": "control.sha256",
        "algorithm": "sha256",
        "entries_in_order": ["campaign.yaml", "artifacts.tsv"],
        "digest_anchored_in": "final GitHub issue comment",
    }:
        fail("campaign control_digest contract is invalid")
    privacy_scan(campaign, "public campaign")


def verify_bundle(
    root: Path,
    *,
    require_comment: bool,
    expected_control_sha256: str | None = None,
    fetch_public: bool = False,
    issue_comment_url: str | None = None,
) -> dict[str, Any]:
    validate_public_root_layout(root, require_comment=require_comment)
    for name in CONTROL_FILES:
        ensure_regular_file(root / name, name)
        if (root / name).stat().st_nlink != 1:
            fail(f"control file must not be a hardlink: {name}")
    control_bytes = (root / "control.sha256").read_bytes()
    expected_control = (
        f"{sha256_path(root / 'campaign.yaml')}  campaign.yaml\n"
        f"{sha256_path(root / 'artifacts.tsv')}  artifacts.tsv\n"
    ).encode("ascii")
    if control_bytes != expected_control:
        fail("control.sha256 is not the exact acyclic two-entry digest file")
    actual_control_hash = sha256_path(root / "control.sha256")
    if expected_control_sha256 is not None:
        if not HASH_RE.fullmatch(expected_control_sha256):
            fail("expected external control SHA-256 is invalid")
        if actual_control_hash != expected_control_sha256:
            fail("local control.sha256 disagrees with the externally anchored digest")
    if actual_control_hash.encode("ascii") in (root / "campaign.yaml").read_bytes():
        fail("campaign.yaml creates a digest cycle by containing control.sha256's digest")
    rows = parse_manifest(root / "artifacts.tsv")
    campaign = read_json(root / "campaign.yaml")
    if not isinstance(campaign, dict) or root.name != campaign.get("run_id"):
        fail("bundle directory basename must equal campaign run_id")
    validate_public_campaign(campaign, rows, root)
    if fetch_public:
        verify_public_payload_urls(rows)
    control_hash_bytes = actual_control_hash.encode("ascii")
    for row in rows:
        with (root / row["relative_path"]).open("rb") as handle:
            tail = b""
            while True:
                block = handle.read(1024 * 1024)
                if not block:
                    break
                combined = tail + block
                if control_hash_bytes in combined:
                    fail("payload artifact creates a control digest cycle")
                tail = combined[-63:]
    if require_comment:
        if expected_control_sha256 is None:
            fail("comment replay requires the expected digest copied from the external issue anchor")
        if issue_comment_url is None:
            fail("comment replay requires the public GitHub issue-comment API URL")
        text = fetch_issue_comment_body(issue_comment_url)
        if actual_control_hash not in text or campaign["run_id"] not in text:
            fail("issue comment does not externally anchor the run and detached control digest")
        url_patterns = {
            "campaign": r"\[campaign\.yaml\]\((https://[^)]+)\)",
            "artifacts": r"\[artifacts\.tsv\]\((https://[^)]+)\)",
            "control": r"\[control\.sha256\]\((https://[^)]+)\)",
        }
        urls = {}
        for key, pattern in url_patterns.items():
            matches = re.findall(pattern, text)
            if len(matches) != 1:
                fail(f"issue comment does not contain one exact {key} control URL")
            urls[key] = matches[0]
        expected_comment = render_issue_comment(campaign, rows, urls, actual_control_hash)
        if text != expected_comment:
            fail("issue comment is not the deterministic complete rendering of the campaign")
        verify_control_urls(root, urls)
        placeholder_tokens = (
            "<run-id>", "<timestamps>", "<40-character sha>", "<hash>", "<value>",
            "<url>", "<status>", "<facts>", "<links>",
        )
        if any(token in text.lower() for token in placeholder_tokens):
            fail("issue comment contains unresolved placeholders")
        for line_number, line in enumerate(text.splitlines(), start=1):
            privacy_scan(line, f"issue comment line {line_number}")
    return campaign


def artifact_links(record: dict[str, Any], rows: dict[str, dict[str, str]]) -> str:
    links = []
    for artifact_id in record["artifacts"]:
        row = rows[artifact_id]
        links.append(
            f"[{artifact_id}]({row['public_url']}) "
            f"(`{row['sha256']}`; {row['bytes']} bytes; {row['media_type']})"
        )
    return "<br>".join(links)


def raw_render_value(value: Any) -> str:
    if isinstance(value, str):
        return value
    return json.dumps(value, ensure_ascii=True, sort_keys=True)


def render_value(value: Any) -> str:
    """Render one value with a one-pass Markdown/HTML character map."""
    replacements = {
        "&": "&amp;", "<": "&lt;", ">": "&gt;", "|": "&#124;",
        "\\": "&#92;", "`": "&#96;", "[": "&#91;", "]": "&#93;",
        "(": "&#40;", ")": "&#41;", "!": "&#33;", "#": "&#35;",
        "\r": "<br>", "\n": "<br>",
    }
    return "".join(replacements.get(character, character) for character in raw_render_value(value))


def render_interactions(record: dict[str, Any]) -> str:
    items = []
    for interaction in record["interactions"]:
        clauses = ", ".join(interaction["clause_ids"])
        items.append(
            f"{interaction['order']}. {interaction['action']} "
            f"[timing: {interaction['timing']}; clauses: {clauses}]"
        )
    return render_value(items)


def render_clause_results(record: dict[str, Any]) -> str:
    results = [
        {
            "clause_id": check["clause_id"],
            "requirement": check["requirement"],
            "status": check["status"],
            "evidence_artifact_ids": check["evidence_artifact_ids"],
        }
        for check in record["requirement_checks"]
    ]
    return render_value(results)


def render_focus_details(record: dict[str, Any]) -> str:
    return render_value({
        "focused_pane_identity_before": record["focused_pane_identity_before"],
        "focused_pane_identity_blocked": record["focused_pane_identity_blocked"],
        "terminal_input_focus_before": record["terminal_input_focus_before"],
        "terminal_input_focus_blocked": record["terminal_input_focus_blocked"],
        "terminal_input_focus_restored": record["terminal_input_focus_restored"],
        "cursor_negotiated_before": record["cursor_negotiated_before"],
        "cursor_blocked": record["cursor_blocked"],
        "cursor_restored": record["cursor_restored"],
        "hollow_visible_on_next_presented_frame": record["hollow_visible_on_next_presented_frame"],
        "dec_1004": record["dec_1004"],
        "conditional_subcases": record["conditional_subcases"],
    })


def render_failure_details(record: dict[str, Any]) -> str:
    return render_value({
        "injection_or_trigger": record["injection_or_trigger"],
        "presentation_generation_before": record["presentation_generation_before"],
        "presentation_generation_visible_during_failure": record[
            "presentation_generation_visible_during_failure"
        ],
        "visible_state": record["visible_state"],
        "terminal_input_usable_during_failure": record[
            "terminal_input_usable_during_failure"
        ],
        "recovery_action": record["recovery_action"],
        "post_recovery_result": record["post_recovery_result"],
        "owned_processes_remaining": record["owned_processes_remaining"],
        "diagnostics_bytes": record["diagnostics_bytes"],
        "diagnostics_content_audit": record["diagnostics_content_audit"],
    })


def render_performance_protocol(record: dict[str, Any]) -> str:
    comparison = record["comparison_inputs"]
    common: dict[str, Any] = {
        "scenario_command": record["command"],
        "comparison_inputs": comparison,
        "comparison_inputs_sha256": record["comparison_inputs_sha256"],
    }
    if record["case_id"] in RENDER_CASES:
        common.update({
            "protocol": "render-path audit",
            "trace_duration_seconds": record["trace_duration_seconds"],
            "warmup_seconds": comparison["warmup_seconds"],
            "bytes_processed": "not part of the render-path protocol",
            "rss_windows_and_threshold": "not part of the render-path protocol",
            "sampling_settings": record["sampling_settings"],
            "process_identity": record["process_identity"],
            "inspected_call_tree_filters": record["inspected_call_tree_filters"],
            "time_profiler_artifact_id": record["time_profiler_artifact_id"],
            "allocations_artifact_id": record["allocations_artifact_id"],
            "screen_artifact_ids": record["screen_artifact_ids"],
        })
        if record["subject"] == "spaceterm":
            common.update({
                "paint_text_shaping_stack_present": record[
                    "paint_text_shaping_stack_present"
                ],
                "paint_path_or_plan_construction_present": record[
                    "paint_path_or_plan_construction_present"
                ],
                "paint_normal_frame_allocation_stack_present": record[
                    "paint_normal_frame_allocation_stack_present"
                ],
                "cursor_or_blink_reshaped_unchanged_rows": record[
                    "cursor_or_blink_reshaped_unchanged_rows"
                ],
                "changed_row_proportionality_result": record[
                    "changed_row_proportionality_result"
                ],
                "exceptional_error_allocations_excluded": record[
                    "exceptional_error_allocations_excluded"
                ],
            })
        else:
            common["reference_render_observation"] = record[
                "reference_render_observation"
            ]
    else:
        common.update({
            "protocol": "workload",
            "optimization_profile": record["optimization_profile"],
            "workload_command": record["workload_command"],
            "workload_input_sha256": record["workload_input_sha256"],
            "duration_seconds": record["duration_seconds"],
            "warmup_seconds": record["warmup_seconds"],
            "bytes_processed": record["bytes_processed"],
            "initial_grid": record["initial_grid"],
            "rss_sample_interval_seconds": record["rss_sample_interval_seconds"],
            "first_post_warmup_five_minutes": record[
                "first_post_warmup_five_minutes"
            ],
            "final_five_minutes": record["final_five_minutes"],
            "allowed_range_delta_bytes": record["allowed_range_delta_bytes"],
            "memory_plateau_result": record["memory_plateau_result"],
            "maximum_main_thread_stall_ms": record["maximum_main_thread_stall_ms"],
            "input_responsiveness_observation": record[
                "input_responsiveness_observation"
            ],
            "ui_backlog_observation": record["ui_backlog_observation"],
            "final_presentation_observation": record[
                "final_presentation_observation"
            ],
            "shell_process_exit_observation": record[
                "shell_process_exit_observation"
            ],
            "rss_samples_artifact_id": record["rss_samples_artifact_id"],
            "time_profiler_artifact_id": record["time_profiler_artifact_id"],
            "allocations_artifact_id": record["allocations_artifact_id"],
            "screen_artifact_ids": record["screen_artifact_ids"],
        })
        if record["case_id"] == "perf-resize":
            common.update({
                "resize_count": record["resize_count"],
                "reflow_timings": record["reflow_timings"],
                "pty_geometry_samples": record["pty_geometry_samples"],
                "final_grid": record["final_grid"],
                "selection_anchoring": record["selection_anchoring"],
                "viewport_anchoring": record["viewport_anchoring"],
                "backing_scale_transition": record["backing_scale_transition"],
                "second_display_available": record["second_display_available"],
            })
    return render_value(common)


def matrix_rows(campaign: dict[str, Any]) -> dict[tuple[str, str], dict[str, Any]]:
    records = {item["record_id"]: item for item in campaign["case_results"]}
    leaves, _status = validate_graph(records)
    return leaves


def render_issue_comment_unbounded_reference(
    campaign: dict[str, Any], rows: list[dict[str, str]], urls: dict[str, str],
    control_hash: str,
) -> str:
    by_artifact = {row["artifact_id"]: row for row in rows}
    leaves = matrix_rows(campaign)
    records_by_id = {record["record_id"]: record for record in campaign["case_results"]}
    frozen = campaign["frozen_artifact"]
    host = campaign["host"]
    ghostty = campaign["ghostty_reference"]
    lines = [
        "## Acceptance run", "",
        f"- **Run ID:** `{campaign['run_id']}`",
        f"- **Started/finished (UTC):** {campaign['started_utc']} / {campaign['finished_utc']}",
        f"- **Commit:** `{frozen['commit_sha']}`",
        f"- **Cargo.lock SHA-256:** `{frozen['cargo_lock_sha256']}`",
        f"- **App/DMG SHA-256:** `{frozen['app_bundle_sha256']}` / `{frozen['dmg_sha256']}`",
        "- **Package version/build/architecture/signing:** "
        f"{render_value(frozen['marketing_version'])} / {render_value(frozen['build_version'])} / "
        f"{render_value(frozen['executable_architectures'])} / {render_value(frozen['code_signing_result'])}",
        "- **Launch source:** mounted verified DMG",
        f"- **Campaign record:** [campaign.yaml]({urls['campaign']})",
        f"- **Payload manifest:** [artifacts.tsv]({urls['artifacts']}) "
        f"(`{campaign['payload_manifest']['sha256']}`; {campaign['payload_manifest']['payload_rows']} payload rows)",
        f"- **Detached control digests:** [control.sha256]({urls['control']}) (`{control_hash}`)",
        "- **Privacy review:** PASS", "",
        "### macOS, hardware, display, and terminal identity", "",
        "| Field | Recorded value |", "| --- | --- |",
        f"| macOS version/build | {render_value(host['macos_version'])} / {render_value(host['macos_build'])} |",
        f"| Machine model / model identifier | {render_value(host['machine_model'])} / {render_value(host['model_identifier'])} |",
        f"| CPU / memory | {render_value(host['cpu'])} / {host['memory_bytes']} bytes |",
        f"| Displays | {render_value(host['displays'])} |",
        f"| Selected terminal font | {render_value(host['terminal_font_selected'])} |",
        f"| JetBrainsMono Nerd Font available | {str(host['jetbrains_mono_nerd_font_available']).lower()} |",
        f"| Initial grid/dimensions | {render_value(host['initial_grid'])} |",
        f"| Clean Workspace root / temporary config | {render_value(campaign['clean_environment'])} |",
        "", "### Shell and TUI versions", "",
        "| Program | Executable | Version | Executable SHA-256 |", "| --- | --- | --- | --- |",
    ]
    for program in campaign["programs"]:
        lines.append(
            f"| {render_value(program['name'])} | {render_value(program['executable'])} | "
            f"{render_value(program['version_output'])} | `{program['executable_sha256']}` |"
        )

    records_by_id = {record["record_id"]: record for record in campaign["case_results"]}
    lines.extend([
        "", "### Native matrix", "",
        "| Case / SpaceTerm record | Status | Command | Ordered interactions | Expected | Observed | Clause results | Evidence | Paired Ghostty reproduction / difference |",
        "| --- | --- | --- | --- | --- | --- | --- | --- | --- |",
    ])
    for case_id in MATRIX_CASES["native"]:
        record = leaves[(case_id, "spaceterm")]
        comparison_id = record["comparison_record_id"]
        if comparison_id is None:
            comparison = render_value(record["comparison_observation"])
        else:
            paired = records_by_id[comparison_id]
            comparison = (
                f"`{paired['record_id']}`; status {paired['status']}; "
                f"command: {render_value(paired['command'])}; "
                f"interactions: {render_interactions(paired)}; "
                f"expected: {render_value(paired['expected'])}; "
                f"observed: {render_value(paired['observed'])}; "
                f"difference: {render_value(record['comparison_observation'])}; "
                f"evidence: {artifact_links(paired, by_artifact)}"
            )
        lines.append(
            f"| `{case_id}`<br>`{record['record_id']}` | {record['status']} | "
            f"{render_value(record['command'])} | {render_interactions(record)} | "
            f"{render_value(record['expected'])} | {render_value(record['observed'])} | "
            f"{render_clause_results(record)} | {artifact_links(record, by_artifact)} | "
            f"{comparison} |"
        )

    lines.extend([
        "", "### Terminal Input Focus matrix", "",
        "| Case / SpaceTerm record | Status | Command and ordered interactions | Expected / observed | Focus, cursor, and DEC 1004 details | Clause results | Evidence |",
        "| --- | --- | --- | --- | --- | --- | --- |",
    ])
    for case_id in MATRIX_CASES["focus"]:
        record = leaves[(case_id, "spaceterm")]
        lines.append(
            f"| `{case_id}`<br>`{record['record_id']}` | {record['status']} | "
            f"command: {render_value(record['command'])}<br>interactions: "
            f"{render_interactions(record)} | expected: {render_value(record['expected'])}"
            f"<br>observed: {render_value(record['observed'])} | "
            f"{render_focus_details(record)} | {render_clause_results(record)} | "
            f"{artifact_links(record, by_artifact)} |"
        )

    lines.extend([
        "", "### Capability and native-service matrix", "",
        "| Case / SpaceTerm record | Status | Command | Ordered interactions | Expected | Observed | Conditional subcases | Clause results | Evidence |",
        "| --- | --- | --- | --- | --- | --- | --- | --- | --- |",
    ])
    for case_id in MATRIX_CASES["capability"]:
        record = leaves[(case_id, "spaceterm")]
        lines.append(
            f"| `{case_id}`<br>`{record['record_id']}` | {record['status']} | "
            f"{render_value(record['command'])} | {render_interactions(record)} | "
            f"{render_value(record['expected'])} | {render_value(record['observed'])} | "
            f"{render_value(record['conditional_subcases'])} | "
            f"{render_clause_results(record)} | {artifact_links(record, by_artifact)} |"
        )

    lines.extend([
        "", "### Failure recovery", "",
        "| Case / SpaceTerm record | Status | Command and ordered interactions | Expected / observed | Failure and recovery details | Clause results | Evidence |",
        "| --- | --- | --- | --- | --- | --- | --- |",
    ])
    for case_id in MATRIX_CASES["failure"]:
        record = leaves[(case_id, "spaceterm")]
        lines.append(
            f"| `{case_id}`<br>`{record['record_id']}` | {record['status']} | "
            f"command: {render_value(record['command'])}<br>interactions: "
            f"{render_interactions(record)} | expected: {render_value(record['expected'])}"
            f"<br>observed: {render_value(record['observed'])} | "
            f"{render_failure_details(record)} | {render_clause_results(record)} | "
            f"{artifact_links(record, by_artifact)} |"
        )

    lines.extend([
        "", "### Performance", "",
        f"- **Frozen Ghostty reference:** {render_value(ghostty)}",
        "- **SpaceTerm/Ghostty interpretation:** Ghostty is a comparison reference; published protocols and Apple behavior remain authoritative.",
        "- **Pairing:** Each row below names the exact reciprocal comparison record and independently links that subject's evidence.",
    ])
    for subject, title in (("spaceterm", "SpaceTerm performance detail"),
                           ("ghostty", "Ghostty performance detail")):
        lines.extend([
            "", f"#### {title}", "",
            "| Scenario / record | Status | Paired record | Workload, duration, bytes, RSS, threshold, stalls, plateau, and observations | Expected / observed / comparison | Clause results | Exact evidence |",
            "| --- | --- | --- | --- | --- | --- | --- |",
        ])
        for case_id in MATRIX_CASES["performance"]:
            record = leaves[(case_id, subject)]
            lines.append(
                f"| `{case_id}`<br>`{record['record_id']}` | {record['status']} | "
                f"`{record['comparison_record_id']}` | {render_performance_protocol(record)} | "
                f"expected: {render_value(record['expected'])}<br>"
                f"observed: {render_value(record['observed'])}<br>"
                f"comparison: {render_value(record['comparison_observation'])} | "
                f"{render_clause_results(record)} | {artifact_links(record, by_artifact)} |"
            )
    lines.extend([
        "", "### Packaged smoke", "",
        "| Case / SpaceTerm record | Status | Observation | Evidence |",
        "| --- | --- | --- | --- |",
    ])
    for case_id in MATRIX_CASES["package"]:
        record = leaves[(case_id, "spaceterm")]
        lines.append(
            f"| `{case_id}`<br>`{record['record_id']}` | {record['status']} | "
            f"expected: {render_value(record['expected'])}<br>"
            f"observed: {render_value(record['observed'])}<br>"
            f"command: {render_value(record['command'])}<br>"
            f"interactions: {render_interactions(record)}<br>"
            f"clauses: {render_clause_results(record)} | "
            f"{artifact_links(record, by_artifact)} |"
        )
    validation_row = by_artifact[campaign["validation"]["artifact_id"]]
    lines.extend([
        "", "### just validate", "",
        f"- **Result:** {campaign['validation']['status']}",
        f"- **Complete log:** [validation log]({validation_row['public_url']}) (`{validation_row['sha256']}`)",
        "", "### Known deviations", "",
        "| Case / record IDs | Smallest reproduction | Ghostty reproduction observation / evidence | Follow-up issue | Current status |",
        "| --- | --- | --- | --- | --- |",
    ])
    if campaign["known_deviations"]:
        for item in campaign["known_deviations"]:
            failed_record = next(
                record for record in campaign["case_results"]
                if record["record_id"] == item["record_id"]
            )
            comparison_id = failed_record.get("comparison_record_id")
            if failed_record["case_id"] in NATIVE_CASES and comparison_id:
                comparison = next(
                    record for record in campaign["case_results"]
                    if record["record_id"] == comparison_id
                )
                ghostty = (
                    f"{render_value(comparison['comparison_observation'])}<br>"
                    f"{artifact_links(comparison, by_artifact)}"
                )
            else:
                ghostty = "not a named native-program deviation"
            lines.append(
                f"| {render_value([item.get('case_id'), item.get('record_id')])} | "
                f"{render_value(item.get('smallest_reproduction'))} | "
                f"{ghostty} | "
                f"{render_value(item.get('follow_up_issue'))} | "
                f"{render_value(item.get('status'))} |"
            )
    else:
        lines.append("| none | none | none | none | none |")
    lines.extend(["", "### Supplementary Kitty static graphics", ""])
    kitty = leaves.get(("perf-render-kitty-static", "spaceterm"))
    if kitty is None:
        lines.append(
            "No supplementary `perf-render-kitty-static` record is present; this does not "
            "affect conventional acceptance."
        )
    else:
        lines.extend([
            "This supplementary row does not affect conventional acceptance.", "",
            "| Record | Status | Command | Ordered interactions | Expected | Observed | Evidence |",
            "| --- | --- | --- | --- | --- | --- | --- |",
            f"| `{kitty['record_id']}` | {kitty['status']} | "
            f"{render_value(kitty['command'])} | {render_interactions(kitty)} | "
            f"{render_value(kitty['expected'])} | {render_value(kitty['observed'])} | "
            f"{artifact_links(kitty, by_artifact)} |",
        ])
    lines.extend([
        "", f"## Final result: {campaign['campaign_status']}", "",
        "All required effective records, payload hashes, manual reviews, pair links, and detached control digests were replayed by the repository finalizer."
        if campaign["campaign_status"] == "PASS"
        else "One or more required effective records failed or were not run; this campaign does not satisfy issue #43.",
        "",
    ])
    result = "\n".join(lines)
    for line_number, line in enumerate(result.splitlines(), start=1):
        privacy_scan(line, f"generated issue comment line {line_number}")
    return result


def render_issue_comment(
    campaign: dict[str, Any], rows: list[dict[str, str]], urls: dict[str, str],
    control_hash: str,
) -> str:
    """Render the bounded GitHub anchor; campaign.yaml retains complete facts."""
    by_artifact = {row["artifact_id"]: row for row in rows}
    leaves = matrix_rows(campaign)

    def brief(value: Any, limit: int = 140) -> str:
        raw = raw_render_value(value)
        if len(raw) > limit:
            raw = raw[: limit - 1] + "…"
        return render_value(raw)

    def evidence(record: dict[str, Any]) -> str:
        return ", ".join(
            f"[{artifact_id}]({by_artifact[artifact_id]['public_url']})"
            for artifact_id in record["artifacts"]
        )

    frozen = campaign["frozen_artifact"]
    host = campaign["host"]
    lines = [
        "## Acceptance run", "",
        f"- **Run ID:** `{campaign['run_id']}`",
        f"- **Started/finished (UTC):** {campaign['started_utc']} / {campaign['finished_utc']}",
        f"- **Commit:** `{frozen['commit_sha']}`",
        f"- **Cargo.lock SHA-256:** `{frozen['cargo_lock_sha256']}`",
        f"- **App/DMG SHA-256:** `{frozen['app_bundle_sha256']}` / `{frozen['dmg_sha256']}`",
        f"- **Package:** {brief([frozen['marketing_version'], frozen['build_version'], frozen['executable_architectures'], frozen['code_signing_result']], 240)}",
        f"- **Package/signing commands:** {render_value([frozen['package_command'], frozen['code_signing_command']])}",
        "- **Launch source:** mounted verified DMG",
        f"- **Campaign record (complete facts and reviews):** [campaign.yaml]({urls['campaign']})",
        f"- **Payload manifest (all evidence hashes):** [artifacts.tsv]({urls['artifacts']}) (`{campaign['payload_manifest']['sha256']}`; {len(rows)} rows)",
        f"- **Detached control:** [control.sha256]({urls['control']}) (`{control_hash}`)",
        "- **Privacy review:** PASS", "",
        "### Host and tool identity", "",
        f"- macOS: {brief(host['macos_version'])} ({brief(host['macos_build'])}); machine/model/CPU/memory: {brief(host['machine_model'])} / {brief(host['model_identifier'])} / {brief(host['cpu'])} / {host['memory_bytes']} bytes",
        f"- Display/grid/font: {brief(host['displays'], 320)} / {brief(host['initial_grid'], 240)} / {brief(host['terminal_font_selected'])}",
        f"- JetBrains Mono Nerd Font available: {str(host['jetbrains_mono_nerd_font_available']).lower()}",
        f"- Clean Workspace/temp configurations: {render_value(campaign['clean_environment'])}",
        f"- Ghostty reference (complete): {render_value(campaign['ghostty_reference'])}", "",
        "| Program | Executable / version command | Version | SHA-256 |", "| --- | --- | --- | --- |",
    ]
    for program in campaign["programs"]:
        lines.append(
            f"| {brief(program['name'])} | {render_value(program['executable'])}<br>{render_value(program['version_command'])} | {brief(program['version_output'])} | `{program['executable_sha256']}` |"
        )

    for title, matrix in (
        ("Native matrix", "native"),
        ("Terminal Input Focus matrix", "focus"),
        ("Capability and native-service matrix", "capability"),
        ("Failure recovery", "failure"),
        ("Packaged smoke", "package"),
    ):
        lines.extend([
            "", f"### {title}", "",
            "| Case / record | Status | Command/interactions | Expected / observed | Evidence | Ghostty difference |",
            "| --- | --- | --- | --- | --- | --- |",
        ])
        for case_id in MATRIX_CASES[matrix]:
            record = leaves[(case_id, "spaceterm")]
            interaction = "; ".join(
                f"{item['order']}:{item['action']}@{item['timing']}" for item in record["interactions"]
            )
            details: Any = record["observed"]
            if matrix == "focus":
                details = {
                    "pane": [record["focused_pane_identity_before"], record["focused_pane_identity_blocked"]],
                    "focus": [record["terminal_input_focus_before"], record["terminal_input_focus_blocked"], record["terminal_input_focus_restored"]],
                    "cursor": [record["cursor_negotiated_before"], record["cursor_blocked"], record["cursor_restored"]],
                    "next_frame": record["hollow_visible_on_next_presented_frame"],
                    "dec1004": record["dec_1004"],
                }
            elif matrix == "failure":
                details = {
                    "trigger": record["injection_or_trigger"],
                    "visible_generation": record["presentation_generation_visible_during_failure"],
                    "recovery": record["recovery_action"],
                    "result": record["post_recovery_result"],
                }
            ghostty_difference = "not applicable"
            if matrix == "native" and record.get("comparison_record_id"):
                comparison = records_by_id[record["comparison_record_id"]]
                ghostty_difference = (
                    f"`{comparison['record_id']}`: "
                    f"{brief(comparison['comparison_observation'], 160)}; {evidence(comparison)}"
                )
            lines.append(
                f"| `{case_id}`<br>`{record['record_id']}` | {record['status']} | "
                f"{brief(record['command'], 100)}<br>{brief(interaction, 180)} | "
                f"{brief(record['expected'], 120)} / {brief(details, 260)} | {evidence(record)} | "
                f"{ghostty_difference} |"
            )

    lines.extend([
        "", "### Performance", "",
        "Ghostty rows are required completed comparisons; their absolute results are not SpaceTerm correctness gates.", "",
        "| Scenario | SpaceTerm / Ghostty records | Workload, RSS/trace facts, observations | Status | Evidence |",
        "| --- | --- | --- | --- | --- |",
    ])
    for case_id in MATRIX_CASES["performance"]:
        space = leaves[(case_id, "spaceterm")]
        ghost = leaves[(case_id, "ghostty")]
        if case_id in RENDER_CASES:
            facts = {
                "duration": space["trace_duration_seconds"],
                "sampling": space["sampling_settings"],
                "filters": space["inspected_call_tree_filters"],
                "SpaceTerm": space["observed"],
                "Ghostty": ghost["reference_render_observation"],
            }
        else:
            facts = {
                "command": space["workload_command"],
                "duration/warmup/bytes": [space["duration_seconds"], space["warmup_seconds"], space["bytes_processed"]],
                "RSS first/final/threshold/result": [space["first_post_warmup_five_minutes"], space["final_five_minutes"], space["allowed_range_delta_bytes"], space["memory_plateau_result"]],
                "max_stall_ms": space["maximum_main_thread_stall_ms"],
                "SpaceTerm": space["observed"],
                "Ghostty": ghost["observed"],
            }
        lines.append(
            f"| `{case_id}` | `{space['record_id']}`<br>`{ghost['record_id']}` | "
            f"{brief(facts, 500)} | {space['status']} / {ghost['status']} | "
            f"{evidence(space)}<br>{evidence(ghost)} |"
        )

    lines.extend([
        "", "### just validate", "",
        f"- **Result:** {campaign['validation']['status']}",
        f"- **Command:** `{campaign['validation']['command']}`",
        f"- **Issue #42 conformance at candidate SHA:** {campaign['issue_42_conformance']['status']} (`{campaign['issue_42_conformance']['candidate_commit_sha']}`; [prerequisite]({campaign['issue_42_conformance']['issue_url']}))",
        f"- **Complete log:** [{campaign['validation']['artifact_id']}]({by_artifact[campaign['validation']['artifact_id']]['public_url']}) (`{by_artifact[campaign['validation']['artifact_id']]['sha256']}`)", "",
        "### Known deviations", "",
        "| Case / failed record | Reproduction | Ghostty observation / evidence | Follow-up | Status |",
        "| --- | --- | --- | --- | --- |",
    ])
    if not campaign["known_deviations"]:
        lines.append("| none | none | none | none | none |")
    for deviation in campaign["known_deviations"]:
        failed = records_by_id[deviation["record_id"]]
        comparison = records_by_id.get(failed.get("comparison_record_id"))
        ghost = "not applicable"
        if comparison is not None:
            ghost = f"{brief(comparison['comparison_observation'], 160)}; {evidence(comparison)}"
        lines.append(
            f"| `{deviation['case_id']}`<br>`{deviation['record_id']}` | "
            f"{brief(deviation['smallest_reproduction'], 220)} | {ghost} | "
            f"[{brief(deviation['follow_up_issue'], 80)}]({deviation['follow_up_issue']}) | {deviation['status']} |"
        )

    kitty = leaves.get(("perf-render-kitty-static", "spaceterm"))
    lines.extend(["", "### Supplementary Kitty static graphics", ""])
    if kitty is None:
        lines.append("Not run; this supplementary row does not gate acceptance.")
    else:
        lines.append(
            f"`{kitty['record_id']}` — {kitty['status']} — {brief(kitty['observed'], 240)} — {evidence(kitty)}"
        )
    lines.extend([
        "", f"## Final result: {campaign['campaign_status']}", "",
        "The repository finalizer replayed the exact inventory, reviewed payload hashes, authenticated identities, pair graph, and detached controls. Complete unabridged observations remain in campaign.yaml.", "",
    ])
    result = "\n".join(lines)
    if len(result) > 65536 or len(result.encode("utf-8")) > 65536:
        fail("deterministic issue comment exceeds GitHub's 65,536-character/byte bound")
    for line_number, line in enumerate(result.splitlines(), start=1):
        privacy_scan(line, f"generated issue comment line {line_number}")
    return result


def command_comment(args: argparse.Namespace) -> None:
    urls = {"campaign": args.campaign_url, "artifacts": args.artifacts_url, "control": args.control_url}
    for label, value in urls.items():
        if not value.startswith("https://"):
            fail(f"{label} control URL must be direct HTTPS")
    root = resolve_publication(args.run_id)
    campaign = verify_bundle(root, require_comment=False)
    rows = parse_manifest(root / "artifacts.tsv")
    control_hash = sha256_path(root / "control.sha256")
    verify_control_urls(root, urls)
    verify_public_payload_urls(rows)
    comment = render_issue_comment(campaign, rows, urls, control_hash)
    comment_parent = publication_parent() / "comments"
    comment_parent.mkdir(mode=0o700, exist_ok=True)
    if comment_parent.is_symlink():
        fail("issue-comment proposal directory must not be a symlink")
    comment_path = comment_parent / f"{args.run_id}.md"
    write_exclusive(comment_path, comment.encode("utf-8"), 0o444)
    print(comment_path)


def command_verify(args: argparse.Namespace) -> None:
    root = Path(args.run_dir).expanduser().resolve()
    campaign = verify_bundle(
        root,
        require_comment=args.require_comment,
        expected_control_sha256=args.expected_control_sha256,
        fetch_public=args.fetch_public,
        issue_comment_url=args.issue_comment_url,
    )
    print(f"Verified issue #43 campaign {campaign['run_id']}: {campaign['campaign_status']}")


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Record and finalize privacy-safe issue #43 acceptance evidence."
    )
    subparsers = parser.add_subparsers(dest="command", required=True)

    init = subparsers.add_parser("init", help="bind the unique hidden mounted collector root")
    init.add_argument("--run-id", required=True)
    init.set_defaults(func=command_init)

    metadata = subparsers.add_parser("set-metadata", help="freeze sanitized campaign metadata")
    metadata.add_argument("--run-id", required=True)
    metadata.add_argument("--input", required=True)
    metadata.set_defaults(func=command_set_metadata)

    record = subparsers.add_parser("record", help="append one immutable subject-scoped case record")
    record.add_argument("--run-id", required=True)
    record.add_argument("--input", required=True)
    record.set_defaults(func=command_record)

    artifact = subparsers.add_parser("add-artifact", help="bind one existing public payload artifact")
    artifact.add_argument("--run-id", required=True)
    artifact.add_argument("--input", required=True)
    artifact.set_defaults(func=command_add_artifact)

    capture_identity = subparsers.add_parser(
        "capture-identity-evidence",
        help="capture the public projection and a fresh final identity replay after collector rename",
    )
    capture_identity.add_argument("--run-id", required=True)
    capture_identity.set_defaults(func=command_capture_identity_evidence)

    review_artifact = subparsers.add_parser("review-artifact", help="manually privacy-review exact payload bytes")
    review_artifact.add_argument("--run-id", required=True)
    review_artifact.add_argument("--artifact-id", required=True)
    review_artifact.add_argument("--decision", required=True)
    review_artifact.add_argument("--reviewer-role", required=True)
    review_artifact.add_argument("--reviewer", required=True)
    review_artifact.add_argument("--review-url", required=True)
    review_artifact.add_argument("--reviewed-utc")
    review_artifact.add_argument("--attestation", required=True)
    review_artifact.set_defaults(func=command_review_artifact)

    review_record = subparsers.add_parser("review-record", help="manually verify one immutable observation")
    review_record.add_argument("--run-id", required=True)
    review_record.add_argument("--record-id", required=True)
    review_record.add_argument("--decision", required=True)
    review_record.add_argument("--reviewer-role", required=True)
    review_record.add_argument("--reviewer", required=True)
    review_record.add_argument("--review-url", required=True)
    review_record.add_argument("--reviewed-utc")
    review_record.add_argument("--attestation", required=True)
    review_record.set_defaults(func=command_review_record)

    batch_proposal = subparsers.add_parser(
        "review-batch-proposal",
        help="render the exact immutable GitHub comment body for one complete review batch",
    )
    batch_proposal.add_argument("--run-id", required=True)
    batch_proposal.add_argument("--kind", choices=("records", "artifacts"), required=True)
    batch_proposal.add_argument("--reviewer", required=True)
    batch_proposal.set_defaults(func=command_review_batch_proposal)

    finalize = subparsers.add_parser("finalize", help="freeze artifacts.tsv, campaign.yaml, and control.sha256")
    finalize.add_argument("--run-id", required=True)
    finalize.add_argument("--finished-utc")
    finalize.set_defaults(func=command_finalize)

    comment = subparsers.add_parser("comment", help="anchor uploaded control URLs in issue-comment.md")
    comment.add_argument("--run-id", required=True)
    comment.add_argument("--campaign-url", required=True)
    comment.add_argument("--artifacts-url", required=True)
    comment.add_argument("--control-url", required=True)
    comment.set_defaults(func=command_comment)

    verify = subparsers.add_parser("verify", help="replay a finalized public bundle")
    verify.add_argument("--run-dir", required=True)
    verify.add_argument("--require-comment", action="store_true")
    verify.add_argument("--expected-control-sha256")
    verify.add_argument("--fetch-public", action="store_true")
    verify.add_argument("--issue-comment-url")
    verify.set_defaults(func=command_verify)
    return parser


def main() -> int:
    try:
        args = build_parser().parse_args()
        args.func(args)
        return 0
    except EvidenceError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
