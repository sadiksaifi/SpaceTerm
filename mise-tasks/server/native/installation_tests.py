#!/usr/bin/env -S python -I
# MISE description="Run bounded native registration, journal and installation regressions"
"""Run synthetic Rust policy and C ABI tests. This task never grants native authority."""

import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "mise-tasks" / "lib"))

from spaceterm_tasks import TaskError, main  # noqa: E402
from spaceterm_tasks.macos_signing import signing_environment  # noqa: E402

C_HARNESS = """
#include <spaceterm_native.h>
int main(void) {
    st_native_pending_facts facts = {0};
    facts.abi_version = ST_NATIVE_ABI_VERSION;
    facts.struct_size = (uint32_t) sizeof(facts);
    return facts.abi_version == 2 && facts.struct_size == 312
        && ST_NATIVE_SECRET_BYTES == 32 && ST_NATIVE_BINDING_BYTES == 32 ? 0 : 1;
}
"""


def run_rust(environment: dict[str, str], *, linux_credentials: bool) -> int:
    command = ["cargo", "test", "--package", "spaceterm-host", "--locked"]
    tests = (
        ["linux_boot_credentials"]
        if linux_credentials
        else [
            "approved_installation",
            "activation_journal",
            "native_deployment",
            "server_installation",
            "startup",
            "native_registration",
        ]
    )
    for test in tests:
        command.extend(["--test", test])
    command.extend(["--", "--skip", "authorized_fixed_root_record_fixture"])
    try:
        result = subprocess.run(
            command, cwd=ROOT, env=environment, capture_output=True, text=True, timeout=240
        )
    except (OSError, subprocess.TimeoutExpired):
        raise TaskError("native-installation-rust-unavailable") from None
    if result.returncode:
        raise TaskError("native-installation-rust-failed")
    counts = re.findall(
        r"test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored;", result.stdout
    )
    if len(counts) != len(tests) or any(
        int(failed) or int(ignored) for _, failed, ignored in counts
    ):
        raise TaskError("native-installation-rust-incomplete")
    count = sum(int(passed) for passed, _, _ in counts)
    if not count:
        raise TaskError("native-installation-zero-tests")
    print(f"{count} Rust regressions passed; synthetic policy, not native deployment evidence")
    return count


def run_c(environment: dict[str, str]) -> None:
    with tempfile.TemporaryDirectory(prefix="spaceterm-installation-abi-") as directory:
        root = Path(directory)
        source = root / "abi.c"
        binary = root / "abi-tests"
        source.write_text(C_HARNESS, encoding="utf-8")
        compiler = (
            ["/usr/bin/xcrun", "--sdk", "macosx", "clang"]
            if sys.platform == "darwin"
            else ["/usr/bin/cc"]
        )
        command = [
            *compiler,
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-I",
            str(ROOT / "crates/spaceterm-host/native/include"),
            str(source),
            "-o",
            str(binary),
        ]
        try:
            compiled = subprocess.run(
                command, cwd=ROOT, env=environment, capture_output=True, timeout=60
            )
            if compiled.returncode:
                raise TaskError("native-installation-c-abi-failed")
            executed = subprocess.run(
                [str(binary)], cwd=ROOT, env=environment, capture_output=True, timeout=10
            )
            if executed.returncode:
                raise TaskError("native-installation-c-abi-failed")
        except (OSError, subprocess.TimeoutExpired):
            raise TaskError("native-installation-c-abi-unavailable") from None
    print("1 C ABI regression passed; layout only, not native authority evidence")


def entry() -> int:
    if sys.argv[1:] not in ([], ["--linux-credentials"]):
        raise TaskError("native-installation-invalid-arguments")
    linux_credentials = bool(sys.argv[1:])
    if linux_credentials and sys.platform != "linux":
        raise TaskError("linux-credentials-tests-unavailable")
    environment = signing_environment(os.environ)
    if sys.platform == "darwin":
        environment["CARGO_BUILD_JOBS"] = "3"
    run_rust(environment, linux_credentials=linux_credentials)
    run_c(environment)
    return 0


if __name__ == "__main__":
    main(entry)
