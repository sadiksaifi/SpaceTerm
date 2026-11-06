"""Desktop packaging keeps signing material out of compiler and disk image subprocesses."""

import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from spaceterm_tasks import TaskError, disk_image, package_linux, venv

SIGNING_INPUTS = {
    "SPACETERM_RELEASE_UPDATE_ED25519_SEED": "manifest-seed",
    "UPDATE_SIGNING_KEY": "fixture-update-key",
    "MACOS_SIGNING_CERTIFICATE_P12": "fixture-certificate",
    "MACOS_SIGNING_CERTIFICATE_PASSWORD": "fixture-password",
    "APP_STORE_CONNECT_API_KEY": "fixture-notary-key",
    "APP_STORE_CONNECT_KEY_ID": "FIXTURE123",
    "APP_STORE_CONNECT_ISSUER_ID": "fixture-issuer",
}


class PackageCredentialsTests(unittest.TestCase):
    def test_linux_package_refuses_product_execution_beside_signing_credentials(self):
        environment = {"PATH": os.defpath, **SIGNING_INPUTS}
        with (
            patch.dict(os.environ, environment, clear=True),
            patch(
                "spaceterm_tasks.cargo.subprocess.Popen",
                side_effect=AssertionError("must not compile or execute products"),
            ),
            self.assertRaisesRegex(TaskError, "credential-free"),
        ):
            package_linux.package()

    def test_dry_run_core_verification_needs_only_tracked_public_keys(self):
        import json

        from spaceterm_tasks import ROOT, core_package, update_signing

        tracked = json.loads((ROOT / update_signing.TRUST).read_text())
        with patch.dict(os.environ, {update_signing.DRY_RUN: "1"}, clear=True):
            self.assertEqual(
                core_package.artifact_trust("spaceterm"), (tracked["current"], tracked["backup"])
            )

    def test_the_disk_image_builder_and_its_installer_run_with_the_given_environment(self):
        given = {"PATH": os.defpath, "SPACETERM_PACKAGED": "1"}
        environments = []

        def run(arguments, **kwargs):
            environments.append(kwargs.get("env"))
            return subprocess.CompletedProcess(arguments, 0, b"", b"")

        with (
            tempfile.TemporaryDirectory() as temporary,
            patch.dict(os.environ, {**given, **SIGNING_INPUTS}, clear=True),
            # A fresh root makes the private environment install its requirements.
            patch.object(venv, "ROOT", Path(temporary)),
            patch.object(
                venv.venv,
                "EnvBuilder",
                return_value=SimpleNamespace(create=lambda path: path.mkdir(parents=True)),
            ),
            patch.object(subprocess, "run", side_effect=run),
            self.assertRaisesRegex(TaskError, "dmgbuild did not produce the disk image"),
        ):
            disk_image.build(
                Path(temporary) / "SpaceTerm.app",
                Path(temporary) / "SpaceTerm.dmg",
                Path(temporary) / "SpaceTerm.icns",
                given,
            )
        installer, builder = environments
        self.assertEqual(installer, given)
        self.assertEqual(set(builder), {*given, "PYTHONPATH"})
