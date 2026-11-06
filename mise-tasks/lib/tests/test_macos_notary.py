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
        self.assertFalse(key.exists())
        with macos_notary.notary(signing("-", dry_run=True)) as notary:
            self.assertIsNone(notary)


class NotarizeTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.app = Path(temporary.name) / "SpaceTerm.app"
        self.app.mkdir()
        (self.app / "executable").write_bytes(b"signed executable")
        self.artifacts = Path(temporary.name) / "artifacts"
        self.notary = macos_notary.Notary("/private/key.p8", "ABCDEF1234", "issuer")
        self.calls = []

    def run_notarize(
        self,
        status="Accepted",
        *,
        broken=None,
        timeout=False,
        submit_output=None,
        log_output=None,
        info_status="In Progress",
        returncode=0,
        source_sha="a" * 40,
        plan_sha256="b" * 64,
    ):
        def submit(arguments, **kwargs):
            self.calls.append(arguments)
            command = arguments[2]
            self.assertFalse(set(CREDENTIALS) & kwargs["env"].keys())
            self.assertEqual(kwargs["stderr"], subprocess.DEVNULL)
            if command == "submit":
                self.assertIsNone(json.loads((self.artifacts / "receipt.json").read_text())["id"])
            if command in ("info", "wait"):
                self.assertEqual(
                    json.loads((self.artifacts / "receipt.json").read_text())["id"], SUBMISSION
                )
                self.assertTrue((self.artifacts / "submission.zip").exists())
            if command == "wait" and timeout:
                self.assertEqual(kwargs["timeout"], macos_notary.WAIT_SECONDS + 30)
                raise subprocess.TimeoutExpired(arguments, kwargs["timeout"])
            if command == "log":
                output = {
                    "jobId": SUBMISSION,
                    "statusCode": 4000,
                    "archiveFilename": "/private/key.p8",
                    "statusSummary": "secret",
                    "issues": [
                        {
                            "severity": "error",
                            "code": 1,
                            "architecture": "arm64",
                            "message": CREDENTIALS[macos_notary.API_KEY],
                            "path": "/private/key.p8",
                        }
                    ]
                    * 200,
                }
            else:
                output = {
                    "id": SUBMISSION,
                    "status": "In Progress"
                    if command == "submit"
                    else info_status
                    if command == "info"
                    else status,
                }
            raw = broken if command == "wait" and broken else json.dumps(output).encode()
            if command == "submit" and submit_output is not None:
                raw = submit_output
            if command == "log" and log_output is not None:
                raw = log_output
            kwargs["stdout"].write(raw)
            return subprocess.CompletedProcess(arguments, returncode if command == "wait" else 0)

        def tool(arguments, message, **kwargs):
            self.calls.append(arguments)
            if arguments[0] == "ditto":
                Path(arguments[-1]).write_bytes(b"exact submitted archive")
            return b""

        with (
            patch.object(macos_notary.subprocess, "run", submit),
            patch.object(macos_notary, "checked", tool),
            contextlib.redirect_stdout(io.StringIO()),
        ):
            macos_notary.notarize(
                self.app,
                self.notary,
                {"PATH": os.defpath, **CREDENTIALS, "GITHUB_SHA": source_sha},
                artifact_directory=self.artifacts,
                plan_sha256=plan_sha256,
            )

    def test_an_accepted_app_is_zipped_submitted_stapled_and_validated(self):
        self.run_notarize("Accepted")
        self.assertEqual([call[0] for call in self.calls], ["ditto"] + ["xcrun"] * 5)
        submitted = self.calls[1]
        self.assertEqual(submitted[1:3], ["notarytool", "submit"])
        self.assertTrue(submitted[3].endswith("submission.zip"))
        self.assertNotIn("--wait", submitted)
        self.assertEqual(submitted[-2:], ["--output-format", "json"])
        self.assertEqual(submitted[submitted.index("--key") + 1], "/private/key.p8")
        self.assertEqual(self.calls[2][1:4], ["notarytool", "info", SUBMISSION])
        self.assertEqual(self.calls[3][1:4], ["notarytool", "wait", SUBMISSION])
        self.assertEqual(self.calls[4][1:3], ["stapler", "staple"])
        self.assertEqual(self.calls[5][1:3], ["stapler", "validate"])
        self.assertEqual(
            json.loads((self.artifacts / "receipt.json").read_text())["status"], "Accepted"
        )

    def test_a_rejected_submission_fetches_a_bounded_redacted_log_and_staples_nothing(self):
        with self.assertRaisesRegex(TaskError, "not_accepted"):
            self.run_notarize("Invalid")
        self.assertEqual(self.calls[-1][1:4], ["notarytool", "log", SUBMISSION])
        self.assertFalse(any(call[1:2] == ["stapler"] for call in self.calls))
        raw = (self.artifacts / "diagnostic.json").read_text()
        evidence = json.loads(raw)
        self.assertEqual(evidence["status_code"], 4000)
        self.assertEqual(len(evidence["issues"]), 100)
        self.assertTrue(evidence["truncated"])
        self.assertLess(len(raw), 12000)
        for private in ("secret", "/private/key.p8", "PRIVATE KEY", "fixture"):
            self.assertNotIn(private, raw)

    def test_timeout_keeps_the_receipt_and_exact_payload_then_resumes_without_submit(self):
        with self.assertRaisesRegex(TaskError, "wait_timeout"):
            self.run_notarize(timeout=True)
        self.assertFalse(any(call[1:2] == ["stapler"] for call in self.calls))
        self.calls.clear()
        self.run_notarize()
        self.assertEqual([call[2] for call in self.calls], ["info", "wait", "staple", "validate"])

    def test_malformed_wait_output_fetches_log_and_never_staples(self):
        for raw in (b"{", b"[]", b'{"id":"wrong","status":"Accepted"}', b'{"status":"Accepted"}'):
            with self.subTest(raw=raw), self.assertRaisesRegex(TaskError, "malformed_output"):
                self.run_notarize(broken=raw)
        self.assertEqual(self.calls[-1][2], "log")
        self.assertFalse(any(call[1:2] == ["stapler"] for call in self.calls))

    def test_malformed_submit_saves_uuid_and_fetches_log_before_failing(self):
        with self.assertRaisesRegex(TaskError, "malformed_output"):
            self.run_notarize(submit_output=f'{{"id":"{SUBMISSION}",'.encode())
        self.assertEqual(
            json.loads((self.artifacts / "receipt.json").read_text())["id"], SUBMISSION
        )
        self.assertEqual(self.calls[-1][2], "log")
        self.calls.clear()
        self.run_notarize(info_status="Accepted")
        self.assertEqual([call[2] for call in self.calls], ["info", "staple", "validate"])

    def test_submit_without_uuid_fails_closed_on_retry(self):
        with self.assertRaisesRegex(TaskError, "malformed_output"):
            self.run_notarize(submit_output=b"not JSON")
        self.calls.clear()
        with self.assertRaisesRegex(TaskError, "submission_unconfirmed"):
            self.run_notarize()
        self.assertEqual(self.calls, [])

    def test_unavailable_log_and_nonzero_accepted_output_still_fail(self):
        with self.assertRaisesRegex(TaskError, "tool_failed"):
            self.run_notarize(returncode=1, log_output=b"credentials and native error")
        evidence = json.loads((self.artifacts / "diagnostic.json").read_text())
        self.assertEqual(evidence["log"], "unavailable")
        self.assertEqual(evidence["log_failure"], "malformed_output")
        self.assertFalse(any(call[1:2] == ["stapler"] for call in self.calls))

    def test_resume_rejects_changed_signed_input_and_tampered_payload(self):
        with self.assertRaises(TaskError):
            self.run_notarize(timeout=True)
        (self.app / "executable").write_bytes(b"different signature")
        self.calls.clear()
        with self.assertRaisesRegex(TaskError, "receipt_mismatch"):
            self.run_notarize()
        self.assertEqual(self.calls, [])

        (self.app / "executable").write_bytes(b"signed executable")
        (self.artifacts / "submission.zip").write_bytes(b"different payload")
        with self.assertRaisesRegex(TaskError, "receipt_mismatch"):
            self.run_notarize()
        self.assertEqual(self.calls, [])

    def test_resume_requires_the_same_source_and_release_plan(self):
        with self.assertRaises(TaskError):
            self.run_notarize(timeout=True)
        self.calls.clear()
        for changed in ({"source_sha": "c" * 40}, {"plan_sha256": "c" * 64}):
            with (
                self.subTest(changed=changed),
                self.assertRaisesRegex(TaskError, "receipt_mismatch"),
            ):
                self.run_notarize(**changed)
        self.assertEqual(self.calls, [])

    def test_a_dry_run_logs_the_skipped_step(self):
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            macos_notary.notarize(self.app, None)
        self.assertEqual(
            output.getvalue(),
            "Dry run: notarization and stapling skipped for SpaceTerm.app; "
            "no App Store Connect key is used.\n",
        )


class SignAppTests(unittest.TestCase):
    def sign(self, release, notary=None):
        calls = []
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
            package_macos.sign_app(
                Path("/stage/SpaceTerm.app"),
                NAME,
                Path("/stage/Entitlements.plist"),
                team=None,
                release=release,
                notary=notary,
                environment={},
            )
        return calls

    def test_a_release_notarizes_the_app_right_after_signing_it(self):
        notary = macos_notary.Notary("/private/key.p8", "ABCDEF1234", "issuer")
        self.assertEqual(self.sign(True, notary), ["sign", ("notarize", notary)])

    def test_preflight_is_signed_and_never_notarized(self):
        self.assertEqual(self.sign(False), ["sign"])


if __name__ == "__main__":
    unittest.main()
