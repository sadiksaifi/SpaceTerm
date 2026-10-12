#!/usr/bin/env -S python -I
# MISE description="Test the temporary macOS GUI evidence companion without permission changes"
"""Build and test the temporary GUI evidence companion.

The companion is a test fixture outside the server. This task builds its model tests, its
interaction tests, and its app executable with warnings as errors. It never launches the app,
registers it with LaunchServices, reads or writes the clipboard, or changes any permission.

    companion_tests.py                 policy tests, model and interaction tests, app build
    companion_tests.py --sanitizers    the same native tests with ASan and UBSan
    companion_tests.py --portable      policy tests only; runs on any platform
    companion_tests.py --bundle DIR    build an unsigned app bundle into the new directory DIR
    companion_tests.py --preview DIR   render light and dark window previews into the new DIR
"""

import json
import os
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[4]
sys.path.insert(0, str(ROOT / "mise-tasks" / "lib"))

from spaceterm_tasks import TaskError, main  # noqa: E402
from spaceterm_tasks.macos_signing import signing_environment  # noqa: E402

GUI = ROOT / "tests" / "server_native" / "macos" / "gui"
INFO_PLIST = GUI / "Info.plist"
BUNDLE_NAME = "SpaceTerm Evidence Companion.app"
EXECUTABLE = "spaceterm-evidence-companion"

C_SOURCES = (
    "companion_model.c",
    "companion_receipts.c",
    "companion_protocol.c",
    "companion_authorization.c",
)
HEADERS = (
    "companion_model.h",
    "companion_receipts.h",
    "companion_protocol.h",
    "companion_authorization.h",
    "companion_app.h",
)
APP_SOURCES = (
    "companion_windows.m",
    "companion_controller.m",
    "companion_ports.m",
    "companion_pasteboard.m",
    "companion_connection.m",
    "companion_main.m",
)
# Interaction tests link the interface and controller with fake ports only. The production
# ports, pasteboard, socket, and entry point stay out, so tests cannot touch the clipboard,
# System Settings, or a controller socket.
INTERFACE_SOURCES = ("companion_windows.m", "companion_controller.m", "companion_ui_tests.m")
MODEL_TESTS = "companion_model_tests.c"
PRODUCTION = C_SOURCES + HEADERS + APP_SOURCES

REQUIRED_INTERFACE_CASES = (
    "permission_explanation_names_requesting_process",
    "authorization_is_separate_from_destructive_confirmation",
    "denied_cancelled_and_unavailable_are_distinct_visible_states",
    "keyboard_navigation_reaches_every_probe_and_cleanup_control",
    "synthetic_targets_do_not_access_other_application_content",
    "url_and_apple_event_receipts_bind_exact_run",
    "companion_quit_does_not_claim_daemon_cleanup",
    "no_automatic_grant_or_global_reset_operation_exists",
    # Regressions from the first interaction review.
    "linked_adapter_native_unavailable_is_not_a_missing_adapter",
    "rejected_expectations_keep_the_channel_open",
    "targets_window_opens_on_screen_beside_the_companion",
    "armed_targets_come_forward_without_activation",
    "accessibility_tree_exposes_targets_and_controls",
)

CLANG = (
    "/usr/bin/xcrun",
    "--sdk",
    "macosx",
    "clang",
    "-arch",
    "arm64",
    "-mmacosx-version-min=26.0",
)
WARNINGS = ("-Wall", "-Wextra", "-Wpedantic", "-Wconversion", "-Wshadow", "-Werror")
SANITIZERS = (
    "-g",
    "-O1",
    "-fsanitize=address,undefined",
    "-fno-sanitize-recover=undefined",
    "-fno-omit-frame-pointer",
)
COMPILE_TIMEOUT = 180
RUN_TIMEOUT = 120

# APIs that grant, reset, or bypass permissions, run privileged or external code, read other
# apps' content, or change the clipboard. Each pattern matches at the start of an identifier.
FORBIDDEN_APIS = (
    r"tccutil",
    r"TCCAccess",
    r"SMJobBless",
    r"SMAppService",
    r"Authorization(Create|CopyRights|ExecuteWithPrivileges|MakeExternalForm)",
    r"launchctl",
    r"sfltool",
    r"pam_",
    r"/etc/pam\.d",
    r"/usr/bin/sudo",
    r"AXUIElementCreate",
    r"AXIsProcessTrusted",
    r"CGRequestScreenCaptureAccess",
    r"CGPreflightScreenCaptureAccess",
    r"CGWindowListCreateImage",
    r"CGDisplayCreateImage",
    r"SCScreenshotManager",
    r"SCShareableContent",
    r"SCStream",
    r"AESend",
    r"NSAppleScript",
    r"NSUserAppleScriptTask",
    r"OSAScript",
    r"NSTask",
    r"posix_spawn",
    r"exec[lv]p?e?\s*\(",
    r"v?fork\s*\(",
    r"system\s*\(",
    r"popen\s*\(",
    r"SecItem",
    r"SecKeychain",
    r"SecAccess",
    r"SecTrustSettings",
    r"AVCaptureDevice",
    r"AVAudio",
    r"requestAccess",
    r"LSRegister",
    r"LSSetDefault",
    r"setDefaultApplication",
    r"clearContents",
    r"setString:",
    r"setData:",
    r"writeObjects",
    r"declareTypes",
)
SYSTEM_HEADERS = frozenset(
    {
        "AppKit/AppKit.h",
        "errno.h",
        "fcntl.h",
        "poll.h",
        "stdbool.h",
        "stddef.h",
        "stdint.h",
        "stdio.h",
        "stdlib.h",
        "string.h",
        "sys/socket.h",
        "sys/stat.h",
        "sys/un.h",
        "time.h",
        "unistd.h",
    }
)
TEST_SYSTEM_HEADERS = frozenset({"objc/runtime.h", "math.h"})
CASE_NAME = re.compile(r"[a-z0-9][a-z0-9_-]{0,95}")


def identifier_pattern(api: str) -> re.Pattern[str]:
    return re.compile(r"(?<![A-Za-z0-9_])" + api)


def source_text(name: str) -> str:
    return (GUI / name).read_text(encoding="utf-8")


def forbidden_uses(names) -> list[tuple[str, str]]:
    """Return (file, pattern) pairs; the matched text itself is never reported."""
    patterns = [(api, identifier_pattern(api)) for api in FORBIDDEN_APIS]
    found = []
    for name in names:
        text = source_text(name)
        for api, pattern in patterns:
            if pattern.search(text):
                found.append((name, api))
    return found


def includes(name: str) -> list[str]:
    return re.findall(r'^\s*#\s*(?:include|import)\s*[<"]([^>"]+)[>"]', source_text(name), re.M)


def header_constant(name: str) -> str:
    match = re.search(
        rf'#define {name}\s+\\?\s*"([^"]+)"', source_text("companion_receipts.h"), re.M
    )
    if match is None:
        raise TaskError("companion-header-constant-missing")
    return match.group(1)


def verify_report(output: str, scope: str, required=()) -> int:
    """Check one test binary's JSON lines and return its case count."""
    cases: dict[str, bool] = {}
    summary = None
    for line in output.splitlines():
        if not line.strip():
            continue
        try:
            record = json.loads(line)
        except ValueError:
            raise TaskError(f"{scope}-report-malformed") from None
        if not isinstance(record, dict) or record.get("schema") != 1:
            raise TaskError(f"{scope}-report-malformed")
        if record.get("scope") != scope:
            raise TaskError(f"{scope}-report-scope-mismatch")
        if summary is not None:
            raise TaskError(f"{scope}-report-after-summary")
        if record.get("summary") is True:
            summary = record
            continue
        name = record.get("case")
        passed = record.get("passed")
        if not isinstance(name, str) or not CASE_NAME.fullmatch(name):
            raise TaskError(f"{scope}-report-malformed")
        if not isinstance(passed, bool) or name in cases:
            raise TaskError(f"{scope}-report-malformed")
        cases[name] = passed
    if summary is None:
        raise TaskError(f"{scope}-report-incomplete")
    failed = sorted(name for name, passed in cases.items() if not passed)
    if summary.get("cases") != len(cases) or summary.get("failed") != len(failed):
        raise TaskError(f"{scope}-report-count-mismatch")
    if not cases:
        raise TaskError(f"{scope}-report-empty")
    missing = [name for name in required if name not in cases]
    if missing:
        raise TaskError(f"{scope} did not run: {', '.join(missing)}")
    if failed:
        raise TaskError(f"{scope} failed: {', '.join(failed)}")
    return len(cases)


def parse_arguments(arguments: list[str]) -> tuple[str, Path | None]:
    if arguments == []:
        return "default", None
    if arguments == ["--sanitizers"]:
        return "sanitizers", None
    if arguments == ["--portable"]:
        return "portable", None
    if len(arguments) == 2 and arguments[0] in ("--bundle", "--preview") and arguments[1]:
        return arguments[0][2:], Path(arguments[1])
    raise TaskError("companion-tests-invalid-arguments")


def check_destination(path: Path) -> Path:
    """Reject a destination that exists, so no output replaces or joins earlier files."""
    path = path.absolute()
    if path.exists() or path.is_symlink():
        raise TaskError("companion-destination-exists")
    if not path.parent.is_dir():
        raise TaskError("companion-destination-parent-missing")
    return path


def new_directory(path: Path) -> Path:
    """Create a fresh private directory. mkdir fails if the path appeared meanwhile."""
    path = check_destination(path)
    path.mkdir(mode=0o700)
    return path


def assemble_bundle(destination: Path, executable: Path) -> Path:
    """Lay out an unsigned app bundle. Signing and launching belong to the evidence runbook."""
    root = new_directory(destination)
    contents = root / BUNDLE_NAME / "Contents"
    (contents / "MacOS").mkdir(parents=True, mode=0o755)
    shutil.copyfile(INFO_PLIST, contents / "Info.plist")
    target = contents / "MacOS" / EXECUTABLE
    shutil.copyfile(executable, target)
    target.chmod(0o755)
    (contents / "PkgInfo").write_bytes(b"APPL????")
    return root / BUNDLE_NAME


class PolicyTests(unittest.TestCase):
    """Portable checks of the companion's sources, bundle metadata, and report handling."""

    def test_production_sources_avoid_privileged_and_permission_changing_apis(self):
        self.assertEqual(forbidden_uses(PRODUCTION), [])

    def test_forbidden_api_scan_detects_identifier_uses(self):
        uses = {
            "AuthorizationExecuteWithPrivileges(x)",
            "[NSPasteboard.generalPasteboard clearContents]",
            '  system("x")',
            "SMAppService *s",
            "posix_spawn(&pid",
        }
        for use in uses:
            hits = [api for api in FORBIDDEN_APIS if identifier_pattern(api).search(use)]
            self.assertTrue(hits, use)
        for prose in ("operating system", "requestAuthorization:", "st_companion_authorization"):
            hits = [api for api in FORBIDDEN_APIS if identifier_pattern(api).search(prose)]
            self.assertEqual(hits, [], prose)

    def test_pasteboard_and_workspace_access_stay_in_their_ports(self):
        for name in PRODUCTION:
            text = source_text(name)
            if name != "companion_pasteboard.m":
                self.assertNotIn("NSPasteboard", text, name)
            if name != "companion_ports.m":
                self.assertNotIn("NSWorkspace", text, name)
        self.assertEqual(source_text("companion_ports.m").count("openURL:"), 1)

    def test_interaction_tests_link_no_production_ports(self):
        for name in ("companion_ports.m", "companion_pasteboard.m", "companion_connection.m"):
            self.assertNotIn(name, INTERFACE_SOURCES)

    def test_settings_url_appears_only_in_the_allowlist(self):
        for name in PRODUCTION:
            count = source_text(name).count("x-apple.systempreferences")
            self.assertEqual(count, 1 if name == "companion_windows.m" else 0, name)
        text = source_text("companion_windows.m")
        match = re.search(r"allowed\[\] = \{([^}]*)\}", text)
        self.assertIsNotNone(match)
        assert match is not None
        anchors = re.findall(r'"([^"]+)"', match.group(1))
        self.assertEqual(
            anchors,
            [
                "Privacy_FilesAndFolders",
                "Privacy_Microphone",
                "Privacy_ScreenCapture",
                "Privacy_Accessibility",
                "Privacy_Automation",
            ],
        )

    def test_sources_include_only_fixture_and_system_headers(self):
        local = set(HEADERS)
        for name in PRODUCTION + (MODEL_TESTS, "companion_ui_tests.m"):
            allowed = SYSTEM_HEADERS | local
            if name in (MODEL_TESTS, "companion_ui_tests.m"):
                allowed = allowed | TEST_SYSTEM_HEADERS
            for header in includes(name):
                self.assertIn(header, allowed, name)

    def test_fixture_has_no_crate_or_server_dependency(self):
        self.assertFalse((GUI / "Cargo.toml").exists())
        for name in PRODUCTION:
            text = source_text(name)
            self.assertNotIn("crates/", text, name)
            self.assertNotIn("spaceterm_native.h", text, name)

    def test_info_plist_declares_identity_without_usage_descriptions(self):
        with INFO_PLIST.open("rb") as handle:
            info = plistlib.load(handle)
        self.assertEqual(
            info["CFBundleIdentifier"], header_constant("ST_COMPANION_BUNDLE_IDENTIFIER")
        )
        self.assertEqual(info["CFBundleExecutable"], EXECUTABLE)
        self.assertEqual(info["CFBundlePackageType"], "APPL")
        self.assertEqual(info["LSMinimumSystemVersion"], "26.0")
        self.assertEqual(info["NSPrincipalClass"], "NSApplication")
        self.assertIs(info["NSSupportsSuddenTermination"], False)
        schemes = [
            scheme for entry in info["CFBundleURLTypes"] for scheme in entry["CFBundleURLSchemes"]
        ]
        self.assertEqual(schemes, [header_constant("ST_COMPANION_URL_SCHEME")])
        self.assertEqual([key for key in info if key.endswith("UsageDescription")], [])
        for key in (
            "LSUIElement",
            "LSBackgroundOnly",
            "LSEnvironment",
            "SMPrivilegedExecutables",
            "SMAuthorizedClients",
            "NSAppleScriptEnabled",
        ):
            self.assertNotIn(key, info)

    def test_required_interaction_cases_are_registered(self):
        text = source_text("companion_ui_tests.m")
        for name in REQUIRED_INTERFACE_CASES:
            self.assertRegex(text, rf'\{{"{name}",\s*{name}\}}')
            self.assertRegex(text, rf"static void {name}\(void\)")

    def test_report_accepts_a_complete_passing_run(self):
        output = "\n".join(
            [
                '{"schema":1,"scope":"s","case":"a","passed":true}',
                '{"schema":1,"scope":"s","case":"b-c","passed":true}',
                '{"schema":1,"scope":"s","summary":true,"cases":2,"failed":0}',
            ]
        )
        self.assertEqual(verify_report(output, "s", ("a",)), 2)

    def test_report_rejects_incomplete_failed_or_inconsistent_runs(self):
        case_a = '{"schema":1,"scope":"s","case":"a","passed":true}'
        failed_b = '{"schema":1,"scope":"s","case":"b","passed":false}'
        rejected = {
            "empty": "",
            "no summary": case_a,
            "no cases": '{"schema":1,"scope":"s","summary":true,"cases":0,"failed":0}',
            "failed case": "\n".join(
                [case_a, failed_b, '{"schema":1,"scope":"s","summary":true,"cases":2,"failed":1}']
            ),
            "count mismatch": "\n".join(
                [case_a, '{"schema":1,"scope":"s","summary":true,"cases":3,"failed":0}']
            ),
            "hidden failure": "\n".join(
                [failed_b, '{"schema":1,"scope":"s","summary":true,"cases":1,"failed":0}']
            ),
            "duplicate": "\n".join(
                [case_a, case_a, '{"schema":1,"scope":"s","summary":true,"cases":2,"failed":0}']
            ),
            "other scope": "\n".join(
                [
                    '{"schema":1,"scope":"t","case":"a","passed":true}',
                    '{"schema":1,"scope":"s","summary":true,"cases":1,"failed":0}',
                ]
            ),
            "after summary": "\n".join(
                [
                    case_a,
                    '{"schema":1,"scope":"s","summary":true,"cases":1,"failed":0}',
                    case_a,
                ]
            ),
            "schema": '{"schema":2,"scope":"s","summary":true,"cases":0,"failed":0}',
            "not json": "check failed at line 1",
            "unsafe name": "\n".join(
                [
                    '{"schema":1,"scope":"s","case":"a b\\u001b","passed":true}',
                    '{"schema":1,"scope":"s","summary":true,"cases":1,"failed":0}',
                ]
            ),
        }
        for label, output in rejected.items():
            with self.assertRaises(TaskError, msg=label):
                verify_report(output, "s")
        missing = "\n".join(
            [case_a, '{"schema":1,"scope":"s","summary":true,"cases":1,"failed":0}']
        )
        with self.assertRaises(TaskError):
            verify_report(missing, "s", ("a", "z"))

    def test_arguments_accept_only_documented_forms(self):
        self.assertEqual(parse_arguments([]), ("default", None))
        self.assertEqual(parse_arguments(["--sanitizers"]), ("sanitizers", None))
        self.assertEqual(parse_arguments(["--portable"]), ("portable", None))
        self.assertEqual(parse_arguments(["--bundle", "out"]), ("bundle", Path("out")))
        self.assertEqual(parse_arguments(["--preview", "out"]), ("preview", Path("out")))
        for arguments in (
            ["--bundle"],
            ["--bundle", ""],
            ["--preview", "a", "b"],
            ["--sanitizers", "--portable"],
            ["--launch"],
            ["out"],
        ):
            with self.assertRaises(TaskError, msg=str(arguments)):
                parse_arguments(arguments)

    def test_bundle_layout_uses_a_new_directory(self):
        with tempfile.TemporaryDirectory(prefix="spaceterm-companion-policy-") as directory:
            executable = Path(directory) / "binary"
            executable.write_bytes(b"\0")
            bundle = assemble_bundle(Path(directory) / "bundle", executable)
            self.assertEqual(bundle.name, BUNDLE_NAME)
            self.assertEqual(
                (bundle / "Contents" / "Info.plist").read_bytes(), INFO_PLIST.read_bytes()
            )
            target = bundle / "Contents" / "MacOS" / EXECUTABLE
            self.assertEqual(target.stat().st_mode & 0o777, 0o755)
            with self.assertRaises(TaskError):
                assemble_bundle(Path(directory) / "bundle", executable)
            with self.assertRaises(TaskError):
                assemble_bundle(Path(directory) / "missing" / "bundle", executable)


def run_policy_tests() -> int:
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(PolicyTests)
    result = unittest.TextTestRunner(stream=sys.stderr, verbosity=1).run(suite)
    if not result.wasSuccessful():
        raise TaskError("companion-policy-tests-failed")
    print(f"companion-policy: {result.testsRun} tests passed")
    return result.testsRun


def compile_native(command: list[str], environment: dict[str, str]) -> None:
    try:
        result = subprocess.run(command, cwd=GUI, env=environment, timeout=COMPILE_TIMEOUT)
    except subprocess.TimeoutExpired:
        raise TaskError("companion-build-timeout") from None
    if result.returncode:
        raise TaskError("companion-build-failed")


def run_report(command: list[str], scope: str, environment: dict[str, str], required=()) -> int:
    try:
        result = subprocess.run(
            command,
            cwd=GUI,
            env=environment,
            stdout=subprocess.PIPE,
            text=True,
            timeout=RUN_TIMEOUT,
        )
    except subprocess.TimeoutExpired:
        raise TaskError(f"{scope}-timeout") from None
    count = verify_report(result.stdout, scope, required)
    if result.returncode:
        raise TaskError(f"{scope}-exit-status")
    print(f"{scope}: {count} cases passed")
    return count


def objective_c_command(sources, output: Path, extra=()) -> list[str]:
    return [
        *CLANG,
        *WARNINGS,
        *extra,
        "-fobjc-arc",
        "-x",
        "c",
        "-std=c11",
        *C_SOURCES,
        "-x",
        "objective-c",
        *sources,
        "-framework",
        "AppKit",
        "-o",
        str(output),
    ]


def build_app(output: Path, environment: dict[str, str]) -> None:
    compile_native(objective_c_command(APP_SOURCES, output), environment)
    check_usage(output, environment)
    print("companion-app: built with warnings as errors; invalid arguments exit 64")


def check_usage(executable: Path, environment: dict[str, str]) -> None:
    """Run the app only with invalid arguments, which exit before AppKit starts.

    The task never passes a valid run ID and socket, so no window, event handler, or
    connection is ever created here.
    """
    run = "0" * 31 + "1"
    for arguments in (
        [],
        ["--run-id", run],
        ["--run-id", "not-hex", "--controller-socket", "/nonexistent/socket"],
        ["--run-id", run, "--controller-socket", "relative/socket"],
        ["--run-id", run, "--controller-socket", "/nonexistent/socket", "--extra"],
    ):
        try:
            result = subprocess.run(
                [str(executable), *arguments],
                cwd=GUI,
                env=environment,
                capture_output=True,
                timeout=30,
            )
        except subprocess.TimeoutExpired:
            raise TaskError("companion-usage-timeout") from None
        if result.returncode != 64 or result.stdout:
            raise TaskError("companion-usage-check-failed")


def run_native(mode: str, destination: Path | None) -> int:
    if sys.platform != "darwin":
        raise TaskError("companion-tests-unavailable")
    environment = signing_environment(os.environ)
    extra = SANITIZERS if mode == "sanitizers" else ()
    if destination is not None:
        check_destination(destination)
    with tempfile.TemporaryDirectory(prefix="spaceterm-evidence-companion-") as directory:
        build = Path(directory)
        if mode == "bundle":
            assert destination is not None
            executable = build / EXECUTABLE
            build_app(executable, environment)
            bundle = assemble_bundle(destination, executable)
            print(
                f"companion-bundle: {bundle.name} has only the linker's ad hoc signature "
                "and was not launched"
            )
            return 0
        interface = build / "interface-tests"
        compile_native(objective_c_command(INTERFACE_SOURCES, interface, extra), environment)
        if mode == "preview":
            assert destination is not None
            target = new_directory(destination)
            try:
                rendered = subprocess.run(
                    [str(interface), "--preview", str(target)],
                    cwd=GUI,
                    env=environment,
                    timeout=RUN_TIMEOUT,
                )
            except subprocess.TimeoutExpired:
                raise TaskError("companion-preview-timeout") from None
            if rendered.returncode:
                raise TaskError("companion-preview-failed")
            print("companion-preview: rendered light and dark previews")
            return 0

        model = build / "model-tests"
        compile_native(
            [*CLANG, *WARNINGS, *extra, "-std=c11", *C_SOURCES, MODEL_TESTS, "-o", str(model)],
            environment,
        )
        run_report([str(model)], "companion-model", environment)
        run_report([str(interface)], "companion-interface", environment, REQUIRED_INTERFACE_CASES)
        build_app(build / EXECUTABLE, environment)
    return 0


def entry() -> int:
    mode, destination = parse_arguments(sys.argv[1:])
    if mode == "portable":
        run_policy_tests()
        return 0
    if mode in ("default", "sanitizers"):
        run_policy_tests()
    return run_native(mode, destination)


if __name__ == "__main__":
    main(entry)
