"""VM ownership and original creation outcomes through a real private record."""

import json
import tempfile
import unittest
from dataclasses import asdict
from pathlib import Path
from unittest.mock import patch

from spaceterm_tasks import TaskError
from spaceterm_tasks import server_vm as vm
from spaceterm_tasks.server_vm_executor import FILES, STATE, VmExecutor, _file_identity


class VmProvider:
    def __init__(self):
        self.running = False
        self.present = False
        self.calls = []
        self.token = None
        self.changed = False

    def create(self, directory, image, resources, port, token):
        self.calls.append("create")
        self.token = token
        self.present = True
        return "c" * 64

    def inspect(self, identifier):
        return {"Id": identifier, "State": {"Running": self.running}}

    def validate(self, *arguments):
        if self.changed:
            raise TaskError("vm-ownership-mismatch")

    def start(self, identifier):
        self.calls.append(("start", identifier))
        self.running = True

    def stop(self, identifier):
        self.calls.append(("stop", identifier))
        self.running = False

    def remove(self, identifier):
        self.calls.append(("remove", identifier))
        self.present = False

    def exists(self, identifier):
        return self.present


class OwnedVmTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name).resolve() / "vm"
        self.directory.mkdir(mode=0o700)
        for name in FILES:
            path = self.directory / name
            path.write_bytes(b"owned fixture")
            path.chmod(0o600)
        document = {
            "schema": 1,
            "directory": list(vm.private_directory(self.directory)),
            "token": "a" * 64,
            "runner_image": "registry.example/qemu@sha256:" + "b" * 64,
            "resources": asdict(vm.Resources()),
            "ssh_port": 25022,
            "phase": "prepared",
            "container": None,
            "files": {name: _file_identity(self.directory, name) for name in FILES},
        }
        (self.directory / STATE).write_text(json.dumps(document))
        (self.directory / STATE).chmod(0o600)
        self.provider = VmProvider()
        self.executor = VmExecutor(self.directory, self.provider)

    def test_default_start_stop_remove_perform_no_native_or_file_effects(self):
        before = set(self.directory.iterdir())
        for action in (self.executor.start, self.executor.stop, self.executor.remove):
            with self.assertRaisesRegex(TaskError, "explicit-vm-mutation-required"):
                action()
        self.assertEqual(self.provider.calls, [])
        self.assertEqual(set(self.directory.iterdir()), before)

    def test_reconstructed_executor_stops_and_removes_only_its_exact_vm(self):
        self.executor.start(allow_mutation=True)
        VmExecutor(self.directory, self.provider).stop(allow_mutation=True)
        VmExecutor(self.directory, self.provider).remove(allow_mutation=True)
        self.assertFalse(self.directory.exists())
        self.assertEqual(
            self.provider.calls,
            ["create", ("start", "c" * 64), ("stop", "c" * 64), ("remove", "c" * 64)],
        )

    def test_a_lost_creation_response_never_creates_or_starts_a_replacement(self):
        def lost(*arguments):
            self.provider.calls.append("create")
            raise TaskError("vm-creation-unresolved")

        with patch.object(self.provider, "create", side_effect=lost):
            with self.assertRaises(TaskError):
                self.executor.start(allow_mutation=True)
            with self.assertRaises(TaskError):
                VmExecutor(self.directory, self.provider).start(allow_mutation=True)
        self.assertEqual(self.provider.calls, ["create"])
        self.assertEqual(json.loads((self.directory / STATE).read_text())["phase"], "creating")

    def test_native_policy_change_is_rejected_before_start(self):
        self.provider.changed = True
        with self.assertRaisesRegex(TaskError, "vm-ownership-mismatch"):
            self.executor.start(allow_mutation=True)
        self.assertEqual(self.provider.calls, ["create"])

    def test_foreign_or_replaced_files_block_removal_without_deletion(self):
        self.executor.start(allow_mutation=True)
        self.executor.stop(allow_mutation=True)
        foreign = self.directory / "foreign"
        foreign.write_bytes(b"not owned")
        with self.assertRaises(TaskError):
            self.executor.remove(allow_mutation=True)
        self.assertTrue(foreign.exists())
        foreign.unlink()
        owned = self.directory / "known_hosts"
        owned.unlink()
        owned.write_bytes(b"replacement")
        owned.chmod(0o600)
        with self.assertRaises(TaskError):
            self.executor.remove(allow_mutation=True)
        self.assertEqual(owned.read_bytes(), b"replacement")
        self.assertTrue(self.provider.present)

    def test_failed_state_sync_cannot_admit_an_external_effect(self):
        with patch("spaceterm_tasks.server_vm_executor.os.fsync", side_effect=OSError()):
            with self.assertRaises(TaskError):
                self.executor.start(allow_mutation=True)
        with self.assertRaises(TaskError):
            VmExecutor(self.directory, self.provider).start(allow_mutation=True)
        self.assertEqual(self.provider.calls, [])
