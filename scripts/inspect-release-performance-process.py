#!/usr/bin/env python3
"""Verify one frozen executable is the code running in a live macOS process."""

from __future__ import annotations

import argparse
import ctypes
import hashlib
import os
import re
import subprocess
import sys
from typing import NoReturn


PROC_PIDTBSDINFO = 3
PROC_PIDPATHINFO_MAXSIZE = 4096
SZOMB = 5
K_CF_NUMBER_SINT32_TYPE = 3
K_CF_STRING_ENCODING_UTF8 = 0x08000100
K_CF_URL_POSIX_PATH_STYLE = 0
K_SEC_CS_SIGNING_INFORMATION = 1 << 1
K_SEC_CS_DYNAMIC_INFORMATION = 1 << 3
K_SEC_CS_STRICT_VALIDATE = 1 << 4


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


def fail(message: str) -> NoReturn:
    print(f"error: {message}", file=sys.stderr)
    raise SystemExit(1)


def sha256(path: str) -> str:
    digest = hashlib.sha256()
    try:
        with open(path, "rb") as source:
            for chunk in iter(lambda: source.read(1024 * 1024), b""):
                digest.update(chunk)
    except OSError:
        fail("frozen executable is unavailable")
    return digest.hexdigest()


def bounded_unsigned(value: str, *, maximum: int) -> int:
    if not value.isascii() or not value.isdecimal() or (
        len(value) > 1 and value.startswith("0")
    ):
        fail("kernel process start identity is malformed")
    parsed = int(value)
    if parsed > maximum:
        fail("kernel process start identity is out of range")
    return parsed


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
    expected_stat = os.stat(expected_path, follow_symlinks=False)
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


class SecurityBindings:
    """Small typed CoreFoundation/Security bridge for dynamic guest identity."""

    def __init__(self) -> None:
        self.cf = ctypes.CDLL(
            "/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation"
        )
        self.security = ctypes.CDLL(
            "/System/Library/Frameworks/Security.framework/Security"
        )
        self.cf.CFRelease.argtypes = [ctypes.c_void_p]
        self.cf.CFNumberCreate.argtypes = [
            ctypes.c_void_p,
            ctypes.c_long,
            ctypes.c_void_p,
        ]
        self.cf.CFNumberCreate.restype = ctypes.c_void_p
        self.cf.CFDictionaryCreateMutable.argtypes = [
            ctypes.c_void_p,
            ctypes.c_long,
            ctypes.c_void_p,
            ctypes.c_void_p,
        ]
        self.cf.CFDictionaryCreateMutable.restype = ctypes.c_void_p
        self.cf.CFDictionarySetValue.argtypes = [
            ctypes.c_void_p,
            ctypes.c_void_p,
            ctypes.c_void_p,
        ]
        self.cf.CFDictionaryGetValue.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
        self.cf.CFDictionaryGetValue.restype = ctypes.c_void_p
        self.cf.CFStringCreateWithCString.argtypes = [
            ctypes.c_void_p,
            ctypes.c_char_p,
            ctypes.c_uint32,
        ]
        self.cf.CFStringCreateWithCString.restype = ctypes.c_void_p
        self.cf.CFStringGetCString.argtypes = [
            ctypes.c_void_p,
            ctypes.c_char_p,
            ctypes.c_long,
            ctypes.c_uint32,
        ]
        self.cf.CFStringGetCString.restype = ctypes.c_bool
        self.cf.CFDataGetLength.argtypes = [ctypes.c_void_p]
        self.cf.CFDataGetLength.restype = ctypes.c_long
        self.cf.CFDataGetBytePtr.argtypes = [ctypes.c_void_p]
        self.cf.CFDataGetBytePtr.restype = ctypes.POINTER(ctypes.c_ubyte)
        self.cf.CFURLCopyFileSystemPath.argtypes = [ctypes.c_void_p, ctypes.c_long]
        self.cf.CFURLCopyFileSystemPath.restype = ctypes.c_void_p
        self.security.SecCodeCopyGuestWithAttributes.argtypes = [
            ctypes.c_void_p,
            ctypes.c_void_p,
            ctypes.c_uint32,
            ctypes.POINTER(ctypes.c_void_p),
        ]
        self.security.SecCodeCopyGuestWithAttributes.restype = ctypes.c_int32
        self.security.SecCodeCheckValidity.argtypes = [
            ctypes.c_void_p,
            ctypes.c_uint32,
            ctypes.c_void_p,
        ]
        self.security.SecCodeCheckValidity.restype = ctypes.c_int32
        self.security.SecCodeCopySigningInformation.argtypes = [
            ctypes.c_void_p,
            ctypes.c_uint32,
            ctypes.POINTER(ctypes.c_void_p),
        ]
        self.security.SecCodeCopySigningInformation.restype = ctypes.c_int32
        self.security.SecRequirementCreateWithString.argtypes = [
            ctypes.c_void_p,
            ctypes.c_uint32,
            ctypes.POINTER(ctypes.c_void_p),
        ]
        self.security.SecRequirementCreateWithString.restype = ctypes.c_int32

    def constant(self, name: str) -> int:
        return ctypes.c_void_p.in_dll(self.security, name).value or 0

    def cf_string(self, value: str) -> int:
        result = self.cf.CFStringCreateWithCString(
            None, value.encode("utf-8"), K_CF_STRING_ENCODING_UTF8
        )
        if not result:
            fail("CoreFoundation string allocation failed")
        return result

    def string_value(self, value: int) -> str:
        buffer = ctypes.create_string_buffer(8192)
        if not value or not self.cf.CFStringGetCString(
            value, buffer, len(buffer), K_CF_STRING_ENCODING_UTF8
        ):
            fail("live signing string is unavailable")
        return os.fsdecode(buffer.value)

    def data_hex(self, value: int) -> str:
        if not value:
            fail("live code hash is unavailable")
        length = self.cf.CFDataGetLength(value)
        pointer = self.cf.CFDataGetBytePtr(value)
        if length <= 0 or not pointer:
            fail("live code hash is unavailable")
        return bytes(pointer[:length]).hex()


def designated_requirement(path: str) -> str:
    result = subprocess.run(
        ["/usr/bin/codesign", "--display", "-r-", path],
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        fail("frozen executable designated requirement is unavailable")
    for line in (result.stdout + "\n" + result.stderr).splitlines():
        if line.startswith("designated => "):
            requirement = line.removeprefix("designated => ").strip()
            if requirement:
                return requirement
    # Ad-hoc signatures may not encode an explicit designated requirement.
    # Build the narrow identifier requirement from static signing metadata;
    # the live guest CDHash/identifier/team are still read dynamically and
    # compared with the independently frozen subject below.
    details = subprocess.run(
        ["/usr/bin/codesign", "--display", "--verbose=4", path],
        check=False,
        capture_output=True,
        text=True,
    )
    identifier = ""
    for line in (details.stdout + "\n" + details.stderr).splitlines():
        if line.startswith("Identifier="):
            identifier = line.removeprefix("Identifier=").strip()
    if details.returncode != 0 or re.fullmatch(r"[A-Za-z0-9._-]+", identifier) is None:
        fail("frozen executable designated requirement is unavailable")
    return f'identifier "{identifier}"'


def live_code_identity(pid: int, expected_path: str) -> tuple[str, str, str, str]:
    bindings = SecurityBindings()
    pid_value = ctypes.c_int32(pid)
    number = bindings.cf.CFNumberCreate(
        None, K_CF_NUMBER_SINT32_TYPE, ctypes.byref(pid_value)
    )
    dictionary = bindings.cf.CFDictionaryCreateMutable(None, 0, None, None)
    if not number or not dictionary:
        fail("dynamic guest attributes are unavailable")
    code = ctypes.c_void_p()
    requirement = ctypes.c_void_p()
    information = ctypes.c_void_p()
    requirement_string = bindings.cf_string(designated_requirement(expected_path))
    try:
        bindings.cf.CFDictionarySetValue(
            dictionary, bindings.constant("kSecGuestAttributePid"), number
        )
        status = bindings.security.SecCodeCopyGuestWithAttributes(
            None, dictionary, 0, ctypes.byref(code)
        )
        if status != 0 or not code.value:
            fail("dynamic guest code is unavailable")
        status = bindings.security.SecRequirementCreateWithString(
            requirement_string, 0, ctypes.byref(requirement)
        )
        if status != 0 or not requirement.value:
            fail("designated requirement could not be compiled")
        status = bindings.security.SecCodeCheckValidity(
            code, K_SEC_CS_STRICT_VALIDATE, requirement
        )
        if status != 0:
            fail("dynamic guest code is invalid or violates its designated requirement")
        status = bindings.security.SecCodeCopySigningInformation(
            code,
            K_SEC_CS_SIGNING_INFORMATION | K_SEC_CS_DYNAMIC_INFORMATION,
            ctypes.byref(information),
        )
        if status != 0 or not information.value:
            fail("dynamic guest signing information is unavailable")
        identifier = bindings.string_value(
            bindings.cf.CFDictionaryGetValue(
                information, bindings.constant("kSecCodeInfoIdentifier")
            )
        )
        team_pointer = bindings.cf.CFDictionaryGetValue(
            information, bindings.constant("kSecCodeInfoTeamIdentifier")
        )
        team = bindings.string_value(team_pointer) if team_pointer else "none"
        cdhash = bindings.data_hex(
            bindings.cf.CFDictionaryGetValue(
                information, bindings.constant("kSecCodeInfoUnique")
            )
        )
        executable_url = bindings.cf.CFDictionaryGetValue(
            information, bindings.constant("kSecCodeInfoMainExecutable")
        )
        path_string = bindings.cf.CFURLCopyFileSystemPath(
            executable_url, K_CF_URL_POSIX_PATH_STYLE
        )
        if not path_string:
            fail("dynamic guest executable path is unavailable")
        try:
            live_path = os.path.realpath(bindings.string_value(path_string))
        finally:
            bindings.cf.CFRelease(path_string)
        return identifier, team, cdhash, live_path
    finally:
        for reference in (
            information.value,
            requirement.value,
            code.value,
            requirement_string,
            dictionary,
            number,
        ):
            if reference:
                bindings.cf.CFRelease(reference)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--pid", type=int, required=True)
    parser.add_argument("--print-start-identity", action="store_true")
    parser.add_argument("--expected-executable")
    parser.add_argument("--expected-sha256")
    parser.add_argument("--expected-device", type=int)
    parser.add_argument("--expected-inode", type=int)
    parser.add_argument("--expected-start-identity")
    parser.add_argument("--expected-signing-identifier")
    parser.add_argument("--expected-team-identifier")
    parser.add_argument("--expected-cdhash")
    arguments = parser.parse_args()
    if arguments.pid <= 0:
        fail("PID must be positive")
    libproc = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
    first = bsd_info(libproc, arguments.pid)
    if arguments.print_start_identity:
        print(
            "process_start_identity\t"
            f"{first.pbi_start_tvsec}:{first.pbi_start_tvusec}"
        )
        return
    if not arguments.expected_executable or not arguments.expected_sha256:
        fail("expected executable and SHA-256 are required")
    supplied_trace_identity = all(
        value is not None
        for value in (
            arguments.expected_device,
            arguments.expected_inode,
            arguments.expected_start_identity,
            arguments.expected_signing_identifier,
            arguments.expected_team_identifier,
            arguments.expected_cdhash,
        )
    )
    if not supplied_trace_identity and any(
        value is not None
        for value in (
            arguments.expected_device,
            arguments.expected_inode,
            arguments.expected_start_identity,
            arguments.expected_signing_identifier,
            arguments.expected_team_identifier,
            arguments.expected_cdhash,
        )
    ):
        fail("complete frozen process and code identity is required")
    expected_path = os.path.realpath(arguments.expected_executable)
    expected_hash = arguments.expected_sha256.lower()
    if sha256(expected_path) != expected_hash:
        fail("frozen executable hash mismatch")
    first_path = process_path(libproc, arguments.pid)
    device, inode = mapped_text_identity(arguments.pid, expected_path)
    identifier, team, cdhash, live_path = live_code_identity(
        arguments.pid, expected_path
    )
    final_path = process_path(libproc, arguments.pid)
    final_device, final_inode = mapped_text_identity(arguments.pid, expected_path)
    second = bsd_info(libproc, arguments.pid)
    first_token = (first.pbi_pid, first.pbi_start_tvsec, first.pbi_start_tvusec)
    second_token = (second.pbi_pid, second.pbi_start_tvsec, second.pbi_start_tvusec)
    if (
        first_token != second_token
        or first_path != expected_path
        or final_path != expected_path
        or live_path != expected_path
        or (device, inode) != (final_device, final_inode)
        or sha256(expected_path) != expected_hash
    ):
        fail("live process or dynamic code identity does not match the frozen subject")
    if supplied_trace_identity:
        expected_start = (
            f"{first.pbi_start_tvsec}:{first.pbi_start_tvusec}"
        )
        start_fields = arguments.expected_start_identity.split(":", 1)
        if len(start_fields) != 2:
            fail("kernel process start identity is malformed")
        bounded_unsigned(start_fields[0], maximum=(1 << 64) - 1)
        bounded_unsigned(start_fields[1], maximum=999_999)
        if (
            arguments.expected_start_identity != expected_start
            or device != arguments.expected_device
            or inode != arguments.expected_inode
            or identifier != arguments.expected_signing_identifier
            or team != arguments.expected_team_identifier
            or cdhash != arguments.expected_cdhash.lower()
        ):
            fail("live process or dynamic code identity does not match the frozen subject")

    token = (
        f"{arguments.pid}:{first.pbi_start_tvsec}:{first.pbi_start_tvusec}:"
        f"{device}:{inode}:{identifier}:{team}:{cdhash}"
    )
    print(f"identity_token\t{token}")
    print("live_code_identity_verified\ttrue")


if __name__ == "__main__":
    main()
