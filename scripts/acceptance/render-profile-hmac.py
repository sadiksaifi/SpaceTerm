#!/usr/bin/python3

"""Authenticate render-profile evidence without exposing the HMAC key."""

from __future__ import annotations

import argparse
import hashlib
import hmac
import json
import os
import re
import stat
import sys
from pathlib import Path


SECRET_PATTERN = re.compile(rb"[0-9a-f]{64}\n\Z")


def fail(message: str) -> None:
    print(f"error: {message}", file=sys.stderr)
    raise SystemExit(1)


def stable_signature(value: os.stat_result) -> tuple[int, ...]:
    return (
        value.st_dev,
        value.st_ino,
        value.st_nlink,
        value.st_uid,
        stat.S_IMODE(value.st_mode),
        value.st_size,
        value.st_mtime_ns,
        value.st_ctime_ns,
    )


def read_secret(path: Path) -> tuple[bytes, str]:
    try:
        before = path.lstat()
    except OSError as error:
        fail(f"HMAC secret is unavailable: {error}")
    if not stat.S_ISREG(before.st_mode):
        fail("HMAC secret must be a regular file, not a symbolic link")
    if before.st_uid != os.getuid():
        fail("HMAC secret must be owned by the current user")
    if before.st_nlink != 1:
        fail("HMAC secret must have exactly one link")
    mode = stat.S_IMODE(before.st_mode)
    if mode & 0o077 or mode & 0o200 or not mode & 0o400:
        fail("HMAC secret must be owner-readable, owner-nonwritable, and private")

    flags = os.O_RDONLY
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        fail(f"HMAC secret could not be opened safely: {error}")
    try:
        opened = os.fstat(descriptor)
        if stable_signature(opened) != stable_signature(before):
            fail("HMAC secret identity changed while it was opened")
        chunks: list[bytes] = []
        while True:
            chunk = os.read(descriptor, 4096)
            if not chunk:
                break
            chunks.append(chunk)
        after_read = os.fstat(descriptor)
    finally:
        os.close(descriptor)
    try:
        after_path = path.lstat()
    except OSError:
        fail("HMAC secret disappeared while it was read")
    if (
        stable_signature(after_read) != stable_signature(opened)
        or stable_signature(after_path) != stable_signature(opened)
    ):
        fail("HMAC secret changed while it was read")

    encoded = b"".join(chunks)
    if SECRET_PATTERN.fullmatch(encoded) is None:
        fail("HMAC secret must be exactly 64 lowercase hex characters plus LF")
    key_hex = encoded[:-1]
    fingerprint = ":".join(str(item) for item in stable_signature(opened))
    return key_hex, fingerprint


def read_stable_body(path: Path) -> bytes:
    try:
        before = path.lstat()
        if not stat.S_ISREG(before.st_mode):
            fail("HMAC body must be a regular file, not a symbolic link")
        body = path.read_bytes()
        after = path.lstat()
    except OSError as error:
        fail(f"HMAC body is unavailable: {error}")
    if stable_signature(before) != stable_signature(after):
        fail("HMAC body changed while it was read")
    return body


def write_process_audit(key_hex: bytes) -> None:
    audit_path = os.environ.get("SPACETERM_RENDER_PROFILE_PROCESS_AUDIT", "")
    if not audit_path:
        return
    key_text = key_hex.decode("ascii")
    argv_values = list(sys.argv)
    environment_values = [item for pair in os.environ.items() for item in pair]
    report = {
        "argv_contains_secret": any(key_text in value for value in argv_values),
        "environment_contains_secret": any(
            key_text in value for value in environment_values
        ),
        "executable": sys.executable,
        "pid": os.getpid(),
    }
    try:
        with Path(audit_path).open("a", encoding="utf-8") as output:
            output.write(json.dumps(report, sort_keys=True) + "\n")
    except OSError as error:
        fail(f"process audit could not be written: {error}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--secret", required=True, type=Path)
    parser.add_argument("--domain", required=True)
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--body", type=Path)
    source.add_argument("--artifact", type=Path)
    parser.add_argument("--last-key")
    arguments = parser.parse_args()
    if not arguments.domain or "\x00" in arguments.domain:
        fail("HMAC domain is invalid")

    key_hex, fingerprint = read_secret(arguments.secret)
    write_process_audit(key_hex)
    if any(key_hex.decode("ascii") in value for value in sys.argv):
        fail("HMAC key material must not appear in process arguments")
    if any(
        key_hex.decode("ascii") in item
        for pair in os.environ.items()
        for item in pair
    ):
        fail("HMAC key material must not appear in the process environment")
    source_path = arguments.body if arguments.body is not None else arguments.artifact
    assert source_path is not None
    body = read_stable_body(source_path)
    if arguments.artifact is not None:
        if not arguments.last_key:
            fail("artifact authentication requires --last-key")
        lines = body.splitlines(keepends=True)
        expected_prefix = (arguments.last_key + "\t").encode("ascii")
        if (
            not body.endswith(b"\n")
            or not lines
            or not lines[-1].startswith(expected_prefix)
        ):
            fail("authenticated artifact trailer is invalid")
        body = b"".join(lines[:-1])
    elif arguments.last_key:
        fail("--last-key is valid only with --artifact")
    key_identifier = hashlib.sha256(key_hex).hexdigest()
    digest = hmac.new(
        bytes.fromhex(key_hex.decode("ascii")),
        arguments.domain.encode("ascii") + b"\0" + body,
        hashlib.sha256,
    ).hexdigest()
    print(f"secret_fingerprint\t{fingerprint}")
    print(f"key_identifier_sha256\t{key_identifier}")
    print(f"hmac_sha256\t{digest}")


if __name__ == "__main__":
    main()
