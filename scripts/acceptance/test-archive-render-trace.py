#!/usr/bin/python3
import pathlib
import subprocess
import tempfile
import zipfile
import importlib.util
import sys
from unittest import mock


TOOL = pathlib.Path(__file__).resolve().parent / "archive-render-trace.py"


def run(*args: str, expected: int = 0) -> None:
    result = subprocess.run([str(TOOL), *args], capture_output=True, text=True)
    if result.returncode != expected:
        raise AssertionError((result.returncode, result.stdout, result.stderr))


with tempfile.TemporaryDirectory(prefix="spaceterm-trace-archive-") as raw:
    root = pathlib.Path(raw).resolve()
    trace = root / "case.trace"
    trace.mkdir()
    (trace / "one").write_text("one")
    nested = trace / "nested"
    nested.mkdir()
    (nested / "two").write_text("two")
    output = root / "case.trace.zip"
    run("--trace", str(trace), "--output", str(output))
    with zipfile.ZipFile(output) as archive:
        assert archive.namelist() == ["case.trace/nested/", "case.trace/one", "case.trace/nested/two"]

    wrong = root / "not-trace"
    wrong.mkdir()
    run("--trace", str(wrong), "--output", str(root / "wrong.zip"), expected=1)

    bad = root / "bad.trace"
    bad.mkdir()
    (bad / "link").symlink_to(trace / "one")
    run("--trace", str(bad), "--output", str(root / "bad.zip"), expected=1)

    spec = importlib.util.spec_from_file_location("archive_render_trace", TOOL)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    low_output = root / "low-space.zip"
    with mock.patch.object(module.os, "statvfs") as statvfs, \
            mock.patch.object(sys, "argv", [str(TOOL), "--trace", str(trace),
                                             "--output", str(low_output)]):
        statvfs.return_value.f_bavail = 0
        statvfs.return_value.f_frsize = 4096
        try:
            module.main()
        except SystemExit:
            pass
        else:
            raise AssertionError("low-space archive unexpectedly succeeded")

    streaming_output = root / "streaming.zip"
    with mock.patch.object(
            module.pathlib.Path, "read_bytes",
            side_effect=AssertionError("archive must stream trace members")), \
            mock.patch.object(sys, "argv", [str(TOOL), "--trace", str(trace),
                                             "--output", str(streaming_output)]):
        module.main()
    with zipfile.ZipFile(streaming_output) as archive:
        assert archive.read("case.trace/one") == b"one"
        assert archive.read("case.trace/nested/two") == b"two"

    mutation_output = root / "mutated.zip"
    original_stream = module.stream_stable_file
    mutated = [False]

    def mutate_before_stream(archive, path, archive_name, fingerprint):
        if not mutated[0] and path.is_file():
            mutated[0] = True
            path.write_text("changed during archive")
        return original_stream(archive, path, archive_name, fingerprint)

    with mock.patch.object(module, "stream_stable_file", mutate_before_stream), \
            mock.patch.object(sys, "argv", [str(TOOL), "--trace", str(trace),
                                             "--output", str(mutation_output)]):
        try:
            module.main()
        except SystemExit:
            pass
        else:
            raise AssertionError("mutating trace unexpectedly archived")

    nested_mutation_output = root / "nested-mutated.zip"
    nested_mutated = [False]

    def add_nested_entry_during_stream(archive, path, archive_name, fingerprint):
        if not nested_mutated[0]:
            nested_mutated[0] = True
            (nested / "late-entry").write_text("late")
        return original_stream(archive, path, archive_name, fingerprint)

    with mock.patch.object(module, "stream_stable_file", add_nested_entry_during_stream), \
            mock.patch.object(sys, "argv", [str(TOOL), "--trace", str(trace),
                                             "--output", str(nested_mutation_output)]):
        try:
            module.main()
        except SystemExit:
            pass
        else:
            raise AssertionError("nested trace mutation unexpectedly archived")
    (nested / "late-entry").unlink()

print("render trace archive fixtures passed")
