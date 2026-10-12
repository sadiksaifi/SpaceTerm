#!/usr/bin/env python
# MISE description="Run the pinned credential closure without compiler-image libraries"
"""Check artifacts in the minimal Jammy image, not kernel, Host-key, TPM, or custody evidence."""

import argparse
import os
import re
import sys

from spaceterm_tasks import ROOT, TaskError, main
from spaceterm_tasks.macos_signing import signing_environment
from spaceterm_tasks.package_server_linux import (
    CLOSURE,
    credential_tool_pin,
    verify_credential_build,
    verify_elf_closure,
)
from spaceterm_tasks.packaging import checked

BASE = "ubuntu@sha256:5ec03bb3441e8b0bf3b4f9cd4629a1ae763010dc3035bb8da3ae6cf026486401"
SYSTEM = {
    "libc.so.6",
    "libm.so.6",
    "libpthread.so.0",
    "libdl.so.2",
    "librt.so.1",
    "libgcc_s.so.1",
    "ld-linux-x86-64.so.2",
}


def test():
    if sys.platform != "linux":
        raise TaskError("credential-tool compiler-container checks require Linux")
    pin = credential_tool_pin()
    if pin["build"]["complete"] is not True:
        raise TaskError("credential-tool compiler-container checks require a completed build")
    verify_credential_build(CLOSURE, pin["build"])
    verify_elf_closure(CLOSURE)
    command = [
        "docker",
        "run",
        "--rm",
        "--network=none",
        "--read-only",
        "--cap-drop=ALL",
        "--security-opt=no-new-privileges",
        "--user",
        f"{os.getuid()}:{os.getgid()}",
        "--mount",
        f"type=bind,src={CLOSURE},dst=/candidate,readonly",
        "--env",
        "OPENSSL_CONF=/candidate/share/spaceterm-server/credential-tools/openssl.cnf",
        "--env",
        "OPENSSL_MODULES=/candidate/lib/credential-tools",
        BASE,
    ]
    clean = signing_environment(os.environ)
    targets = [
        "libexec/credential-tools/systemd-creds",
        *(
            f"lib/credential-tools/{name}"
            for name in (
                "libtss2-esys.so.0",
                "libtss2-rc.so.0",
                "libtss2-mu.so.0",
                "libtss2-tcti-device.so.0",
                "libmount.so.1",
                "libcap.so.2",
            )
        ),
    ]
    checks = 0
    for target in targets:
        resolved = checked(
            [*command, "/lib64/ld-linux-x86-64.so.2", "--list", "/candidate/" + target],
            "credential-tool dependency cannot run without compiler-image libraries",
            env=clean,
            timeout=30,
        )
        if len(resolved) > 65536:
            raise TaskError("credential-tool loader observation exceeds its bound")
        selections = re.findall(rb"(?m)^\s*(\S+) => (/\S+) ", resolved)
        if not selections:
            raise TaskError("credential-tool loader observation is incomplete")
        for name, selected in selections:
            if (
                name.decode() not in SYSTEM
                and os.path.normpath(selected) != b"/candidate/lib/credential-tools/" + name
            ):
                raise TaskError("credential-tool loader selected an external private dependency")
        checks += 1
    version = checked(
        [*command, "/candidate/libexec/credential-tools/systemd-creds", "--version"],
        "credential-tool version query cannot run without compiler-image libraries",
        env=clean,
        timeout=30,
    )
    if (
        len(version) > 4096
        or not version.startswith(b"systemd 260")
        or b"+OPENSSL" not in version
        or b"+TPM2" not in version
    ):
        raise TaskError("credential-tool version or required support differs")
    checks += 1
    print(
        f"Executed {checks} loader/version checks with zero failures. Source and LGPL receipts verified."
    )
    observations = checked(
        [
            *command[:-1],
            "--tmpfs",
            "/tmp:rw,exec,nosuid,nodev,size=64m",
            "--mount",
            f"type=bind,src={ROOT / 'tests/server_native/linux/custody'},dst=/fixtures,readonly",
            pin["build"]["build_image"],
            "python3",
            "-I",
            "/fixtures/credential_tool_tests.py",
            "--root",
            "/candidate",
            "--peer-source",
            "/fixtures/credential_tool_peers.c",
        ],
        "credential-tool injected native-peer checks failed",
        env=clean,
        timeout=120,
    )
    if not re.fullmatch(
        rb"native-peer-cases=[1-9][0-9]{0,2}; privileged-evidence=unrun\n", observations
    ):
        raise TaskError("credential-tool native-peer observation is incomplete")
    print(observations.decode().strip())
    print("No native Jammy-kernel, systemd 249, Host-key, TPM, or custody case ran.")


if __name__ == "__main__":
    argparse.ArgumentParser(description=__doc__).parse_args()
    main(test)
