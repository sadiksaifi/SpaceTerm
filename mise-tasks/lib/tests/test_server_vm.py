"""Admission cannot turn VM inputs into host mutation or cleanup authority."""

import hashlib
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from spaceterm_tasks import TaskError
from spaceterm_tasks import server_vm as vm


class VmAdmissionTests(unittest.TestCase):
    def test_resource_bounds_reject_booleans_and_excess_before_mutation(self):
        for field, value in (
            ("cpus", True),
            ("cpus", 3),
            ("memory_mib", 4097),
            ("disk_gib", 25),
            ("boot_timeout_seconds", 181),
        ):
            with self.subTest(field=field), self.assertRaises(TaskError):
                vm.Resources(**{field: value}).validate()
        vm.Resources().validate()

    def test_mutation_requires_explicit_approval_and_a_nonroot_runner(self):
        with patch.object(vm.os, "getuid", return_value=501):
            for allowed in (False, None, "true", 1):
                with self.assertRaises(TaskError):
                    vm.require_mutation(allowed)
            vm.require_mutation(True)
        with patch.object(vm.os, "getuid", return_value=0), self.assertRaises(TaskError):
            vm.require_mutation(True)

    def test_bad_image_bytes_are_never_supplied_to_the_vm_provider(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            paths = []
            for name in ("image", "checksums", "signature", "keyring", "output"):
                directory = root / name
                directory.mkdir(mode=0o700)
                paths.append(directory)
            image = paths[0] / vm.IMAGE_NAME
            image.write_bytes(b"different bytes")
            image.chmod(0o600)
            pin = hashlib.sha256(b"expected bytes!").hexdigest()
            spec = vm.GuestImage(
                image,
                paths[1] / "checksums",
                paths[2] / "signature",
                paths[3] / "keyring",
                pin,
                len(b"expected bytes!"),
                "A" * 40,
                "a" * 64,
            )
            destination = paths[4] / "copy.img"
            with patch.object(vm.native, "verify_checksums", return_value={vm.IMAGE_NAME: pin}):
                with self.assertRaises(TaskError):
                    vm.copy_verified_image(spec, destination)
            self.assertFalse(destination.exists())

    def test_symlinked_and_checkout_directories_do_not_grant_cleanup_authority(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            target = root / "private"
            target.mkdir(mode=0o700)
            link = root / "link"
            link.symlink_to(target)
            with self.assertRaises(TaskError):
                vm.private_directory(link)
            (root / ".git").mkdir()
            with self.assertRaises(TaskError):
                vm.private_directory(target)
