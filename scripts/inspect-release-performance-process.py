#!/usr/bin/env python3
"""Return a privacy-safe, kernel-backed identity for one live macOS process."""

from __future__ import annotations

import argparse
import ctypes
import hashlib
import os
import subprocess
import sys


PROC_PIDTBSDINFO = 3
PROC_PIDPATHINFO_MAXSIZE = 4096
SZOMB = 5


class ProcBSDInfo(ctypes.Structure):
    _fields_ = [
        ("pbi_flags", ctypes.c_uint32),
        ("pbi_status", ctypes.c_uint32),
        ("pbi_xstatus", ctypes.c_uint32),
        ("pbi_pid", ctypes.c_uint32),
        ("pbi_ppid", ctypes.c_uint32),
        ("pbi_uid", ctypes.c_uint32),
        ("pbi_gid", ctypes.c_uint32),
        ("pbi_ruid", ctypes.c_uint32),
        ("pbi_rgid", ctypes.c_uint32),
        ("pbi_svuid", ctypes.c_uint32),
        ("pbi_svgid", ctypes.c_uint32),
        ("rfu_1", ctypes.c_uint32),
        ("pbi_comm", ctypes.c_char * 16),
        ("pbi_name", ctypes.c_char * 32),
        ("pbi_nfiles", ctypes.c_uint32),
        ("pbi_pgid", ctypes.c_uint32),
        ("pbi_pjobc", ctypes.c_uint32),
        ("e_tdev", ctypes.c_uint32),
        ("e_tpgid", ctypes.c_uint32),
        ("pbi_nice", ctypes.c_int32),
        ("pbi_start_tvsec", ctypes.c_uint64),
        ("pbi_start_tvusec", ctypes.c_uint64),
    ]


def fail(message: str) -> "NoReturn":
    print(f"error: {message}", file=sys.stderr)
    raise SystemExit(1)


def bsd_info(libproc: ctypes.CDLL, pid: int) -> ProcBSDInfo:
    info = ProcBSDInfo()
    result = libproc.proc_pidinfo(
        pid, PROC_PIDTBSDINFO, 0, ctypes.byref(info), ctypes.sizeof(info)
    )
    if result != ctypes.sizeof(info):
        fail("kernel process identity is unavailable")
    if info.pbi_pid != pid or info.pbi_status == SZOMB:
        fail("process is unavailable or a zombie")
    return info


def process_path(libproc: ctypes.CDLL, pid: int) -> str:
    path = ctypes.create_string_buffer(PROC_PIDPATHINFO_MAXSIZE)
    result = libproc.proc_pidpath(pid, path, len(path))
    if result <= 0:
        fail("kernel executable path is unavailable")
    return os.path.realpath(os.fsdecode(path.value))


def mapped_text_identity(pid: int, expected_path: str) -> tuple[int, int]:
    result = subprocess.run(
        ["/usr/sbin/lsof", "-a", "-p", str(pid), "-d", "txt", "-FnDi"],
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        fail("live executable vnode is unavailable")
    records: list[dict[str, str]] = []
    current: dict[str, str] = {}
    for line in result.stdout.splitlines():
        if line.startswith("f"):
            if current:
                records.append(current)
            current = {"f": line[1:]}
        elif line:
            current[line[0]] = line[1:]
    if current:
        records.append(current)
    expected_stat = os.stat(expected_path)
    matching = [
        record
        for record in records
        if record.get("n")
        and os.path.realpath(record["n"]) == expected_path
        and record.get("i") == str(expected_stat.st_ino)
    ]
    if len(matching) != 1:
        fail("live executable vnode does not match the frozen package")
    return expected_stat.st_dev, expected_stat.st_ino


def code_identity(path: str) -> str:
    verification = subprocess.run(
        ["/usr/bin/codesign", "--verify", "--strict", path],
        check=False,
        capture_output=True,
        text=True,
    )
    if verification.returncode != 0:
        fail("live executable code signature is invalid")
    details = subprocess.run(
        ["/usr/bin/codesign", "-d", "--verbose=4", path],
        check=False,
        capture_output=True,
        text=True,
    )
    if details.returncode != 0:
        fail("live executable code identity is unavailable")
    for line in details.stderr.splitlines():
        if line.startswith("CDHash="):
            value = line.removeprefix("CDHash=").strip().lower()
            if value and all(character in "0123456789abcdef" for character in value):
                return value
    fail("live executable CDHash is unavailable")


def sha256(path: str) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--pid", type=int, required=True)
    parser.add_argument("--expected-executable", required=True)
    parser.add_argument("--expected-sha256", required=True)
    arguments = parser.parse_args()
    if arguments.pid <= 0:
        fail("PID must be positive")
    expected_path = os.path.realpath(arguments.expected_executable)
    if sha256(expected_path) != arguments.expected_sha256:
        fail("frozen executable hash mismatch")

    libproc = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
    first = bsd_info(libproc, arguments.pid)
    observed_path = process_path(libproc, arguments.pid)
    device, inode = mapped_text_identity(arguments.pid, expected_path)
    cdhash = code_identity(expected_path)
    final_path = process_path(libproc, arguments.pid)
    final_device, final_inode = mapped_text_identity(arguments.pid, expected_path)
    second = bsd_info(libproc, arguments.pid)
    first_token = (first.pbi_pid, first.pbi_start_tvsec, first.pbi_start_tvusec)
    second_token = (second.pbi_pid, second.pbi_start_tvsec, second.pbi_start_tvusec)
    if (
        first_token != second_token
        or observed_path != expected_path
        or final_path != expected_path
        or (device, inode) != (final_device, final_inode)
        or sha256(expected_path) != arguments.expected_sha256
    ):
        fail("process identity changed during inspection")

    token = (
        f"{arguments.pid}:{first.pbi_start_tvsec}:{first.pbi_start_tvusec}:"
        f"{device}:{inode}:{cdhash}"
    )
    print(f"identity_token\t{token}")


if __name__ == "__main__":
    main()
