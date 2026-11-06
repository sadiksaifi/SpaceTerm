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
    "UPDATE_SIGNING_KEY": "fixture-update-key",
    "MACOS_SIGNING_CERTIFICATE_P12": "fixture-certificate",
    "MACOS_SIGNING_CERTIFICATE_PASSWORD": "fixture-password",
    "APP_STORE_CONNECT_API_KEY": "fixture-notary-key",
    "APP_STORE_CONNECT_KEY_ID": "FIXTURE123",
    "APP_STORE_CONNECT_ISSUER_ID": "fixture-issuer",
}


class PackageCredentialsTests(unittest.TestCase):
    def test_linux_package_does_not_pass_signing_inputs_to_cargo(self):
        environment = {
            "PATH": os.defpath,
            **SIGNING_INPUTS,
            "SPACETERM_LINUX_PACKAGE_TARGET": "x86_64-unknown-linux-gnu.2.35",
            "SPACETERM_LINUX_PACKAGE_SYSROOT": "/fixture-sysroot",
        }

        def compiler(arguments, **kwargs):
            self.assertTrue(arguments[:2] == ["cargo", "zigbuild"])
            self.assertEqual(
                set(kwargs["env"]) & set(environment),
                {"PATH", "SPACETERM_LINUX_PACKAGE_TARGET", "SPACETERM_LINUX_PACKAGE_SYSROOT"},
                "signing inputs must stay in the packaging process",
            )
            raise TaskError("compiler boundary reached")

        with (
            tempfile.TemporaryDirectory() as temporary,
            patch.dict(os.environ, environment, clear=True),
            patch.object(package_linux, "STAGE", Path(temporary) / "stage"),
            patch(
                "spaceterm_tasks.core_package.prepare_desktop_artifact",
                return_value={
                    "SPACETERM_CLI_ARCHIVE": "/verified/artifact.tar.gz",
                    "SPACETERM_CLI_SIGNATURE": "/verified/artifact.sig",
                },
            ),
            patch.object(package_linux.sys, "platform", "linux"),
            patch.object(package_linux.platform, "machine", return_value="x86_64"),
            patch("spaceterm_tasks.cargo.subprocess.Popen", side_effect=compiler),
            self.assertRaisesRegex(TaskError, "compiler boundary reached"),
        ):
            package_linux.package()

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
