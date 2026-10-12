#!/usr/bin/env -S python3 -I
# MISE description="Test fixed management policy without authorization prompts or host changes"
"""Synthetic management gates. These do not prove native deployment feasibility."""

import os
import plistlib
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path

ROOT = Path(__file__).resolve().parents[4]
sys.path.insert(0, str(ROOT / "mise-tasks" / "lib"))

from spaceterm_tasks import TaskError, main  # noqa: E402
from spaceterm_tasks.macos_signing import signing_environment  # noqa: E402

OPERATIONS = (
    "install",
    "stage",
    "activate",
    "start",
    "stop",
    "restart",
    "uninstall",
    "repair",
    "collect",
    "remote-access",
    "owner-enrollment",
)
PACKAGING = ROOT / "packaging" / "server"


def template(relative: str) -> str:
    text = (PACKAGING / relative).read_text()
    for key, value in {
        "SERVER_IDENTIFIER": "io.github.sadiksaifi.spaceterm-development.server",
        "MANAGER_IDENTIFIER": "io.github.sadiksaifi.spaceterm-development.server.manager",
        "ACCOUNT_UID": "501",
        "BUILD": "development",
        "REGISTRATION_GENERATION": "7",
        "LAUNCHER_PATH": "/protected/fixture/launcher",
        "MANAGER_PATH": "/protected/fixture/manager",
    }.items():
        text = text.replace(f"@{key}@", value)
    if "@" in text:
        raise TaskError("management-template-unresolved")
    return text


class ManagementTemplates(unittest.TestCase):
    def test_system_launchdaemon_tracks_launcher_and_respects_stopped_exit(self):
        daemon = plistlib.loads(template("macos/LaunchDaemon.plist.in").encode())
        self.assertEqual(daemon["Label"], "io.github.sadiksaifi.spaceterm-development.server.501")
        self.assertEqual(daemon["UserName"], "root")
        self.assertEqual(
            daemon["ProgramArguments"],
            [
                "/protected/fixture/launcher",
                "--build",
                "development",
                "--account-uid",
                "501",
                "--registration-generation",
                "7",
            ],
        )
        self.assertEqual(daemon["KeepAlive"], {"SuccessfulExit": False})
        self.assertNotIn("EnvironmentVariables", daemon)

    def test_no_finite_launchd_timeout_interrupts_started_git_write(self):
        daemon = plistlib.loads(template("macos/LaunchDaemon.plist.in").encode())
        self.assertEqual(daemon["ExitTimeOut"], 0)
        self.assertFalse(daemon["AbandonProcessGroup"])

    def test_demand_helper_has_no_resident_keepalive_or_run_at_load(self):
        helper = plistlib.loads(template("macos/ManagementHelper.plist.in").encode())
        self.assertFalse(helper["KeepAlive"])
        self.assertFalse(helper["RunAtLoad"])
        self.assertEqual(helper["ProgramArguments"], ["/protected/fixture/manager", "--gui-demand"])
        self.assertEqual(
            helper["MachServices"],
            {
                "io.github.sadiksaifi.spaceterm-development.server.manager.demand": True,
            },
        )

    def test_rights_require_fresh_operation_specific_admin_authentication(self):
        rights = plistlib.loads(template("macos/AuthorizationRights.plist.in").encode())
        self.assertEqual(
            set(rights),
            {
                f"io.github.sadiksaifi.spaceterm-development.server.manage.{name}"
                for name in OPERATIONS
            },
        )
        for value in rights.values():
            self.assertEqual(
                value,
                {
                    "class": "user",
                    "group": "admin",
                    "authenticate-user": True,
                    "shared": False,
                    "allow-root": False,
                    "timeout": 0,
                    "tries": 3,
                    "session-owner": False,
                },
            )

    def test_polkit_rights_have_no_retained_auth_or_gui_environment(self):
        policy = ET.fromstring(template("linux/polkit/spaceterm-server.policy.in"))
        actions = policy.findall("action")
        self.assertEqual(len(actions), len(OPERATIONS))
        self.assertEqual(
            {action.attrib["id"] for action in actions},
            {
                f"io.github.sadiksaifi.spaceterm-development.server.manage.{name}"
                for name in OPERATIONS
            },
        )
        for action in actions:
            defaults = action.find("defaults")
            self.assertIsNotNone(defaults)
            assert defaults is not None
            self.assertEqual(
                {element.tag: element.text for element in defaults},
                {
                    "allow_any": "auth_admin",
                    "allow_inactive": "auth_admin",
                    "allow_active": "auth_admin",
                },
            )
            annotations = {item.attrib["key"]: item.text for item in action.findall("annotate")}
            self.assertEqual(
                annotations,
                {
                    "org.freedesktop.policykit.exec.path": "/protected/fixture/manager",
                    "org.freedesktop.policykit.exec.argv1": action.attrib["id"].split(".")[-1],
                },
            )

    def test_systemd_249_template_requires_no_encrypted_credential_directive(self):
        service = template("linux/spaceterm-server.service.in")
        self.assertNotIn("LoadCredentialEncrypted", service)
        self.assertNotIn("LoadCredential=", service)
        self.assertIn("Type=exec\n", service)
        self.assertIn("User=root\n", service)
        self.assertIn("WantedBy=multi-user.target\n", service)
        self.assertIn('ExecStart="/protected/fixture/launcher" --build development', service)
        self.assertNotIn("XDG_RUNTIME_DIR", service)

    def test_locked_storage_wait_does_not_exhaust_configured_restart_limits(self):
        service = template("linux/spaceterm-server.service.in")
        self.assertIn("StartLimitIntervalSec=0\n", service)
        self.assertIn("Restart=on-failure\n", service)
        self.assertIn("RestartSec=20s\n", service)
        self.assertIn("SuccessExitStatus=0\n", service)
        daemon = plistlib.loads(template("macos/LaunchDaemon.plist.in").encode())
        self.assertEqual(daemon["ThrottleInterval"], 20)
        self.assertEqual(daemon["KeepAlive"], {"SuccessfulExit": False})

    def test_systemd_drain_does_not_signal_or_force_kill_git_children(self):
        service = template("linux/spaceterm-server.service.in")
        self.assertIn("KillMode=mixed\n", service)
        self.assertIn("TimeoutStopSec=infinity\n", service)
        self.assertIn("SendSIGKILL=no\n", service)

    def test_templates_do_not_disable_sudo_or_supplementary_group_workflows(self):
        service = template("linux/spaceterm-server.service.in")
        for restriction in (
            "NoNewPrivileges",
            "CapabilityBoundingSet",
            "SupplementaryGroups",
            "PrivateUsers",
        ):
            self.assertNotIn(restriction, service)


def run_program(command: list[str], environment: dict[str, str], timeout: int) -> int:
    try:
        return subprocess.run(
            command, cwd=ROOT, env=environment, timeout=timeout, check=False
        ).returncode
    except (OSError, subprocess.TimeoutExpired):
        raise TaskError("management-tests-execution-unavailable") from None


def compiler(*, sanitizers: bool) -> list[str]:
    if sys.platform == "darwin":
        command = [
            "/usr/bin/xcrun",
            "--sdk",
            "macosx",
            "clang",
            "-arch",
            "arm64",
            "-mmacosx-version-min=26.0",
        ]
    else:
        command = ["cc"]
    command += ["-std=c11", "-Wall", "-Wextra", "-Werror"]
    if sanitizers:
        command += ["-O1", "-g", "-fsanitize=address,undefined", "-fno-omit-frame-pointer"]
    return command


def admin_admission(binary: Path, environment: dict[str, str]) -> int:
    cases = [
        (["uninstall"], 2),
        (
            [
                "install",
                "--account-uid",
                "501",
                "--candidate",
                "/private/tmp/spaceterm-native-520-candidate",
            ],
            2,
        ),
        (
            [
                "install",
                "--account-uid",
                "0",
                "--candidate",
                "/private/tmp/spaceterm-native-520-candidate",
            ],
            9,
        ),
        (
            [
                "install",
                "--account-uid",
                "0501",
                "--candidate",
                "/private/tmp/spaceterm-native-520-candidate",
            ],
            9,
        ),
        (
            [
                "install",
                "--account-uid",
                "4294967295",
                "--candidate",
                "/private/tmp/spaceterm-native-520-candidate",
            ],
            9,
        ),
        (["install", "--account-uid", "501", "--candidate", "/untrusted/replacement"], 9),
        (["uninstall", "--force"], 9),
    ]
    for arguments, expected in cases:
        try:
            result = subprocess.run(
                [str(binary), *arguments],
                cwd=ROOT,
                env=environment,
                capture_output=True,
                timeout=2,
                check=False,
            )
        except (OSError, subprocess.TimeoutExpired):
            raise TaskError("management-admin-retirement-unavailable") from None
        output = (
            b"classification=native-unavailable\n"
            if expected == 2
            else b"classification=invalid-record\n"
        )
        if result.returncode != expected or result.stdout != output or result.stderr:
            raise TaskError("management-admin-admission-failed")
    print(f"tests={len(cases)} scope=evidence-admin-admission proof=synthetic")
    return 0


def entry() -> int:
    arguments = sys.argv[1:]
    if arguments not in ([], ["--tools"], ["--portable"], ["--sanitizers"]):
        raise TaskError("management-tests-invalid-arguments")
    if not arguments and sys.platform != "darwin":
        raise TaskError("macos-management-tests-unavailable")
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(ManagementTemplates)
    if suite.countTestCases() != 9:
        raise TaskError("management-tests-incomplete")
    if not unittest.TextTestRunner(verbosity=2).run(suite).wasSuccessful():
        return 1
    environment = signing_environment(os.environ)
    sources = ROOT / "tests" / "server_native" / "macos"
    with tempfile.TemporaryDirectory(prefix="spaceterm-management-") as directory:
        binary = Path(directory) / "management-tests"
        command = compiler(sanitizers=arguments == ["--sanitizers"])
        command += [
            "-DST_MANAGEMENT_TESTING",
            str(ROOT / "crates" / "spaceterm-host" / "native" / "macos" / "management.c"),
            str(sources / "management_tests.c"),
        ]
        if sys.platform == "darwin":
            command += ["-framework", "Security", "-framework", "CoreFoundation"]
        command += ["-o", str(binary)]
        result = run_program(command, environment, 60)
        if result:
            return result
        result = run_program([str(binary)], environment, 10)
        if result:
            return result
        admin = Path(directory) / "evidence-admin"
        result = run_program(
            compiler(sanitizers=False) + [str(sources / "evidence_admin.c"), "-o", str(admin)],
            environment,
            60,
        )
        if result:
            return result
        return admin_admission(admin, environment)


if __name__ == "__main__":
    main(entry)
