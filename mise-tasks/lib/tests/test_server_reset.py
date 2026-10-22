"""Explicit resets preserve unrelated state and refuse unresolved originals and cleanup."""

import contextlib
import fcntl
import io
import json
import re
import tempfile
import unittest
from pathlib import Path

from spaceterm_tasks import TaskError
from spaceterm_tasks.server_reset import reset_at


class ResetTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.data = self.root / "server"
        self.runtime = self.root / "runtime"
        self.data.mkdir(mode=0o700)
        self.runtime.mkdir(mode=0o700)
        self.write(self.runtime / "server.lock", {})
        self.identity = self.data / "identity.json"
        self.write(self.identity, {"organization": None})
        self.client = self.data / "clients" / "desktop"
        self.client.mkdir(mode=0o700, parents=True)
        self.client.parent.chmod(0o700)
        self.write(self.client / "operations.json", {"pending": None})
        self.write(self.data / "service.json", {"stopped": True, "installed": True, "run": None})

    def write(self, path, value):
        path.write_text(json.dumps(value))
        path.chmod(0o600)

    def preview(self):
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            reset_at(self.data, self.runtime)
        match = re.search(r"Review digest: ([a-f0-9]+)", output.getvalue())
        assert match is not None
        return match[1]

    def test_preview_and_exact_confirmation_archive_only_the_selected_state(self):
        direct = self.data / "direct.json"
        self.write(direct, {"policy": None, "devices": [{"pairing": "retired fixture"}]})
        direct_bytes = direct.read_bytes()
        native_references = self.root / "direct-device-connections.json"
        self.write(native_references, {"pending": [{"credential": "preserved fixture reference"}]})
        native_bytes = native_references.read_bytes()
        service = (self.data / "service.json").read_bytes()
        identity = self.identity.read_bytes()
        token = self.preview()
        self.assertTrue(self.identity.exists())
        with contextlib.redirect_stdout(io.StringIO()):
            reset_at(self.data, self.runtime, token)
        archive = next(self.data.glob("reset-*"))
        self.assertEqual((archive / "identity.json").read_bytes(), identity)
        self.assertTrue((archive / "clients/desktop/operations.json").is_file())
        self.assertEqual((archive / "direct.json").read_bytes(), direct_bytes)
        self.assertFalse(direct.exists())
        self.assertEqual(native_references.read_bytes(), native_bytes)
        self.assertEqual((self.data / "service.json").read_bytes(), service)
        self.assertFalse(self.identity.exists())

    def test_changed_scope_cannot_use_an_earlier_confirmation(self):
        direct = self.data / "direct.json"
        self.write(direct, {"devices": []})
        for path in (self.identity, direct):
            with self.subTest(path=path.name):
                token = self.preview()
                self.write(path, {"changed": path.name})
                with self.assertRaises(TaskError), contextlib.redirect_stdout(io.StringIO()):
                    reset_at(self.data, self.runtime, token)
                self.assertTrue(path.exists())
                self.assertFalse(list(self.data.glob("reset-*")))

    def test_pending_client_or_started_server_write_refuses_reset(self):
        self.write(self.client / "operations.json", {"pending": {"sequence": 1}})
        with self.assertRaisesRegex(TaskError, "original client operation"):
            self.preview()
        self.write(self.client / "operations.json", {"pending": None})
        self.write(self.identity, {"organization": {"devices": [{"outcome": "Pending"}]}})
        with self.assertRaisesRegex(TaskError, "started Git write"):
            self.preview()
        self.assertTrue(self.identity.exists())

    def test_unresolved_process_cleanup_and_busy_instance_are_preserved(self):
        self.write(self.data / "service.json", {"run": {"boot": "unproved"}})
        with self.assertRaisesRegex(TaskError, "process cleanup"):
            self.preview()
        self.write(self.data / "service.json", {"stopped": True})
        with (self.runtime / "server.lock").open() as instance:
            fcntl.flock(instance, fcntl.LOCK_EX | fcntl.LOCK_NB)
            with self.assertRaises(TaskError):
                self.preview()
        self.assertTrue(self.identity.exists())

    def test_symlinked_scope_is_preserved_without_following_it(self):
        self.identity.unlink()
        unrelated = self.root / "unrelated.json"
        self.write(unrelated, {"keep": True})
        self.identity.symlink_to(unrelated)
        with self.assertRaises(TaskError):
            self.preview()
        self.assertTrue(self.identity.is_symlink())
        self.assertEqual(unrelated.read_text(), '{"keep": true}')
