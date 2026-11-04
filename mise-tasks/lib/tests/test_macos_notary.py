"""Notarization policy, the API key's lifetime and notarytool's outcomes, without Apple."""

import contextlib
import io
import json
import os
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from spaceterm_tasks import TaskError, macos_codesign, macos_notary, package_macos
from spaceterm_tasks.update_signing import DRY_RUN

TEAM = "ABCDE12345"
NAME = f"Developer ID Application: Example ({TEAM})"
SUBMISSION = "2efe2717-52ef-43a5-96dc-0797e4ca1041"
CREDENTIALS = {
    macos_notary.API_KEY: "-----BEGIN PRIVATE KEY-----\nfixture\n-----END PRIVATE KEY-----\n",
    macos_notary.KEY_ID: "ABCDEF1234",
    macos_notary.ISSUER: "c055ca8c-e5a8-4836-b61d-aa5794eeb3f4",
}


def signing(identity, *, dry_run=False):
    environment = {macos_codesign.TEAM_ID: TEAM, macos_codesign.SIGNING_IDENTITY: identity}
    if dry_run:
        environment[DRY_RUN] = "1"
    return environment


class PolicyTests(unittest.TestCase):
    def test_a_dry_run_signs_ad_hoc_and_a_release_signs_with_developer_id(self):
        self.assertFalse(macos_notary.required(signing("-", dry_run=True)))
        self.assertTrue(macos_notary.required(signing(NAME)))
        with self.assertRaisesRegex(TaskError, "a dry run signs ad hoc"):
            macos_notary.required(signing(NAME, dry_run=True))
        with self.assertRaisesRegex(TaskError, "a release signs with Developer ID"):
            macos_notary.required(signing("-"))

    def test_credentials_must_be_an_api_key_its_id_and_the_issuer(self):
        self.assertEqual(macos_notary.credentials(CREDENTIALS)[1], "ABCDEF1234")
        for name, value in (
            (macos_notary.API_KEY, ""),
            (macos_notary.API_KEY, "not a key"),
            (macos_notary.KEY_ID, "abcdef1234"),
            (macos_notary.ISSUER, "issuer"),
        ):
            with self.subTest(name=name, value=value):
                with self.assertRaises(TaskError):
                    macos_notary.credentials({**CREDENTIALS, name: value})

    def test_the_key_file_is_private_and_removed_after_use(self):
        with macos_notary.notary({**signing(NAME), **CREDENTIALS}) as notary:
            key = Path(notary.key_file)
            self.assertEqual(stat.S_IMODE(key.stat().st_mode), 0o600)
            self.assertEqual(key.read_text(), CREDENTIALS[macos_notary.API_KEY])
            self.assertEqual(macos_notary.from_environment(notary.environment()), notary)
            self.assertNotIn(macos_notary.API_KEY, notary.environment())
        self.assertFalse(key.exists())
        with macos_notary.notary(signing("-", dry_run=True)) as notary:
            self.assertIsNone(notary)


class NotarizeTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.app = Path(temporary.name) / "SpaceTerm.app"
        self.app.mkdir()
        self.notary = macos_notary.Notary("/private/key.p8", "ABCDEF1234", "issuer")
        self.calls = []

    def run_notarize(self, status):
        def submit(arguments, **kwargs):
            self.calls.append(arguments)
            output = json.dumps({"id": SUBMISSION, "status": status}).encode()
            return subprocess.CompletedProcess(arguments, 0, output, b"")

        def tool(arguments, message, **kwargs):
            self.calls.append(arguments)
            return b""

        with (
            patch.object(macos_notary.subprocess, "run", submit),
            patch.object(macos_notary, "checked", tool),
            contextlib.redirect_stdout(io.StringIO()),
        ):
            macos_notary.notarize(self.app, self.notary, {"PATH": os.defpath})

    def test_an_accepted_app_is_zipped_submitted_stapled_and_validated(self):
        self.run_notarize("Accepted")
        self.assertEqual([call[0] for call in self.calls], ["ditto", "xcrun", "xcrun", "xcrun"])
        submitted = self.calls[1]
        self.assertEqual(submitted[1:3], ["notarytool", "submit"])
        self.assertTrue(submitted[3].endswith("SpaceTerm.zip"))
        self.assertIn("--wait", submitted)
        self.assertEqual(submitted[submitted.index("--key") + 1], "/private/key.p8")
        self.assertEqual(self.calls[2][1:3], ["stapler", "staple"])
        self.assertEqual(self.calls[3][1:3], ["stapler", "validate"])

    def test_a_rejected_submission_names_its_log_and_staples_nothing(self):
        with self.assertRaisesRegex(TaskError, f"notarytool log {SUBMISSION}"):
            self.run_notarize("Invalid")
        self.assertFalse(any(call[1:2] == ["stapler"] for call in self.calls))

    def test_a_dry_run_logs_the_skipped_step(self):
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            macos_notary.notarize(self.app, None)
        self.assertEqual(
            output.getvalue(),
            "Dry run: notarization and stapling skipped for SpaceTerm.app; "
            "no App Store Connect key is used.\n",
        )


class SignAppHookTests(unittest.TestCase):
    def hook(self, **extra):
        calls = []
        environment = {
            "CARGO_PACKAGER_FORMAT": "dmg",
            "SPACETERM_PACKAGER_APP": "/stage/SpaceTerm.app",
            "SPACETERM_PACKAGER_IDENTITY": NAME,
            "SPACETERM_PACKAGER_ENTITLEMENTS": "/stage/Entitlements.plist",
            **extra,
        }
        with (
            patch.object(package_macos, "checked"),
            patch.object(
                macos_codesign, "sign_desktop", lambda *args, **kwargs: calls.append("sign")
            ),
            patch.object(
                macos_notary,
                "notarize",
                lambda app, notary, env: calls.append(("notarize", notary)),
            ),
        ):
            package_macos.sign_app(environment)
        return calls

    def test_a_release_notarizes_the_app_right_after_signing_it(self):
        notary = macos_notary.Notary("/private/key.p8", "ABCDEF1234", "issuer")
        calls = self.hook(SPACETERM_PACKAGER_RELEASE="1", **notary.environment())
        self.assertEqual(calls, ["sign", ("notarize", notary)])

    def test_preflight_is_signed_and_never_notarized(self):
        self.assertEqual(self.hook(), ["sign"])


if __name__ == "__main__":
    unittest.main()
