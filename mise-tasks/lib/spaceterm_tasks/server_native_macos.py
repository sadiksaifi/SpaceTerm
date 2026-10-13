"""Unprivileged ND520/1 assembly and receipt orchestration.

This module never imports signing material or invokes sudo, launchctl, security,
privacy tools, or production executables. Native mutation is an injected port with
no default implementation. Receipts are recovery facts, not execution authority.
"""

import argparse
import base64
import binascii
import copy
import fcntl
import hashlib
import json
import os
import platform
import selectors
import shlex
import signal
import stat
import subprocess
import time
from collections.abc import Callable
from contextlib import AbstractContextManager, contextmanager
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Protocol

from spaceterm_tasks import ROOT, TaskError
from spaceterm_tasks.native_evidence_schema import (
    CAPABILITIES,
    FAILURES,
    RequiredCase,
    boolean,
    collect,
    decode_json,
    exact_fields,
    hexadecimal,
    integer,
    reject,
)

STAGE = Path("/private/tmp/spaceterm-native-520-candidate")
PROTECTED_ROOT = Path("/Library/Application Support/SpaceTerm Native Evidence 520")
CERTIFICATE = "9e07cd5c15c5c862a21b93ea55d4303bc19b7366"
IDENTIFIER = "io.github.sadiksaifi.spaceterm-development.server.native-evidence"
CODE_IDENTIFIERS = {
    "evidence-admin": f"{IDENTIFIER}.admin",
    "evidence-client": f"{IDENTIFIER}.client",
    "fixture-launcher": f"{IDENTIFIER}.launcher",
    "fixture-server": f"{IDENTIFIER}.server",
    "fixture-server-update": f"{IDENTIFIER}.server",
    "capability-probe": f"{IDENTIFIER}.probe",
    "Evidence Companion.app": f"{IDENTIFIER}.companion",
}
MANIFEST_BYTES = 1024 * 1024
FILE_BYTES = 64 * 1024 * 1024
PACKAGE_BYTES = 256 * 1024 * 1024
RESOURCES = ("fixtures", "registration", "credential", "service")
INTERACTIVE_CASES = frozenset(CAPABILITIES) | frozenset(
    (
        "authorization-denied",
        "authorization-cancelled",
        "authorization-granted",
        "post-filevault-unlock-before-gui-login",
    )
)


def native_environment() -> dict[str, str]:
    """No inherited values, including signing, loader, Python, or account inputs."""
    return {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "LC_ALL": "C"}


def canonical_json(value) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True).encode()


def code_file(name: str) -> str:
    return f"{name}/Contents/MacOS/evidence-companion" if name.endswith(".app") else name


def requirement(identifier: str) -> str:
    return f'=identifier "{identifier}" and certificate leaf = H"{CERTIFICATE}"'


def relative_name(name: Any):
    if type(name) is not str:
        reject("artifact-rejected")
    try:
        length = len(name.encode("utf-8"))
    except UnicodeError:
        reject("artifact-rejected")
    if (
        not name
        or length > 1024
        or name.startswith("/")
        or "\\" in name
        or any(ord(char) < 32 or ord(char) == 127 for char in name)
        or any(part in ("", ".", "..") for part in name.split("/"))
    ):
        reject("artifact-rejected")


def file_snapshot(info: os.stat_result) -> tuple[int, ...]:
    """Access-time changes caused by reading are not content substitutions."""
    return (
        info.st_dev,
        info.st_ino,
        info.st_mode,
        info.st_uid,
        info.st_gid,
        info.st_nlink,
        info.st_size,
        info.st_mtime_ns,
        info.st_ctime_ns,
    )


def read_regular(root: Path, name: str, limit: int) -> bytes:
    """Retain each source directory. Never follow a source link or read a device."""
    relative_name(name)
    descriptors = []
    try:
        descriptor = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        descriptors.append(descriptor)
        parts = name.split("/")
        for part in parts[:-1]:
            descriptor = os.open(
                part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=descriptor
            )
            descriptors.append(descriptor)
        descriptor = os.open(
            parts[-1], os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=descriptor
        )
        descriptors.append(descriptor)
        info = os.fstat(descriptor)
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or info.st_size > limit:
            reject("artifact-rejected")
        data = bytearray()
        while len(data) <= limit:
            chunk = os.read(descriptor, min(65536, limit + 1 - len(data)))
            if not chunk:
                break
            data.extend(chunk)
        if len(data) > limit or file_snapshot(os.fstat(descriptor)) != file_snapshot(info):
            reject("artifact-rejected")
        return bytes(data)
    except OSError:
        reject("artifact-rejected")
    finally:
        for descriptor in reversed(descriptors):
            os.close(descriptor)


def validate_manifest(data: bytes) -> dict[str, Any]:
    manifest = decode_json(data, MANIFEST_BYTES)
    exact_fields(manifest, "schema scope run_id revision dirty files code".split())
    if (
        type(manifest["schema"]) is not int
        or manifest["schema"] != 1
        or manifest["scope"] != "native-evidence-520"
    ):
        reject("unsupported-schema")
    hexadecimal(manifest["run_id"], 32)
    hexadecimal(manifest["revision"], 40)
    boolean(manifest["dirty"])
    if canonical_json(manifest) != data:
        reject("artifact-rejected")
    files = manifest["files"]
    code = manifest["code"]
    if (
        type(files) is not list
        or not 7 <= len(files) <= 256
        or type(code) is not list
        or len(code) != len(CODE_IDENTIFIERS)
    ):
        reject("artifact-rejected")
    names = []
    total = 0
    for entry in files:
        exact_fields(entry, ("name", "size", "sha256", "mode"))
        relative_name(entry["name"])
        hexadecimal(entry["sha256"], 64)
        integer(entry["size"], 0, FILE_BYTES)
        integer(entry["mode"], 0, 0o777)
        if (
            entry["mode"] not in (0o444, 0o555)
            or entry["name"]
            in (
                "fixture-manifest.json",
                "preflight.json",
                "runbook-local.sh",
                "runbook-disposable.sh",
                "bootstrap-receipt.json",
            )
            or (
                entry["mode"] == 0o555
                and entry["name"] not in {code_file(name) for name in CODE_IDENTIFIERS}
            )
        ):
            reject("artifact-rejected")
        names.append(entry["name"])
        total += entry["size"]
    if total > PACKAGE_BYTES or names != sorted(set(names)):
        reject("artifact-rejected")
    expected = [
        {"name": name, "identifier": identifier}
        for name, identifier in sorted(CODE_IDENTIFIERS.items())
    ]
    if code != expected:
        reject("artifact-rejected")
    for name in CODE_IDENTIFIERS:
        path = code_file(name)
        if path not in names or files[names.index(path)]["mode"] != 0o555:
            reject("artifact-rejected")
    return manifest


def _source_names(root: Path, expected: set[str]) -> set[str]:
    names = set()
    directories = {
        parent.as_posix()
        for name in expected
        for parent in Path(name).parents
        if parent != Path(".")
    }
    entries = 0
    for directory, dirs, files in os.walk(root, followlinks=False):
        for name in dirs + files:
            entries += 1
            if entries > 1024:
                reject("limit-exceeded")
            path = Path(directory) / name
            info = path.lstat()
            if stat.S_ISLNK(info.st_mode):
                reject("artifact-rejected")
            relative = path.relative_to(root).as_posix()
            if stat.S_ISDIR(info.st_mode) and relative not in directories:
                reject("artifact-rejected")
            if not stat.S_ISDIR(info.st_mode):
                if relative not in expected:
                    reject("artifact-rejected")
                if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
                    reject("artifact-rejected")
                names.add(path.relative_to(root).as_posix())
            if len(names) > 257:
                reject("limit-exceeded")
    return names


def _tool(arguments: list[str], timeout=10.0) -> tuple[bytes, bytes]:
    """Bound both channels while reading, with one absolute monotonic deadline."""
    try:
        process = subprocess.Popen(
            arguments,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=native_environment(),
            cwd="/",
            start_new_session=True,
        )
    except OSError:
        reject("native-unavailable")
    channels = [bytearray(), bytearray()]
    deadline = time.monotonic() + timeout
    try:
        with selectors.DefaultSelector() as selector:
            for index, pipe in enumerate((process.stdout, process.stderr)):
                if pipe is None:
                    reject("native-unavailable")
                os.set_blocking(pipe.fileno(), False)
                selector.register(pipe, selectors.EVENT_READ, index)
            while selector.get_map():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    reject("timed-out")
                for key, _ in selector.select(remaining):
                    data = os.read(key.fd, 65536 + 1 - len(channels[key.data]))
                    if not data:
                        selector.unregister(key.fileobj)
                    else:
                        channels[key.data].extend(data)
                        if len(channels[key.data]) > 65536:
                            reject("limit-exceeded")
            try:
                status = process.wait(timeout=max(0.001, deadline - time.monotonic()))
            except subprocess.TimeoutExpired:
                reject("timed-out")
            if status:
                reject("artifact-rejected")
        return bytes(channels[0]), bytes(channels[1])
    finally:
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()
        if process.stdout is not None:
            process.stdout.close()
        if process.stderr is not None:
            process.stderr.close()


class RetainedArtifacts:
    """Bind unprivileged observations to retained files and their pathname edges.

    These handles are not custody. Kernel change stamps make a replace-and-restore
    fail preflight, including swaps of parent directories. Protected native copies
    still require their own custody and a full policy check before root execution.
    """

    def __init__(self, root: Path, names: set[str]):
        self.original_root = root
        self.names = names
        self.root = root
        self.entries: list[tuple[Path, int, tuple[int, ...], str | None]] = []

    @staticmethod
    def digest(descriptor):
        os.lseek(descriptor, 0, os.SEEK_SET)
        digest = hashlib.sha256()
        total = 0
        while chunk := os.read(descriptor, 65536):
            total += len(chunk)
            if total > FILE_BYTES:
                reject("artifact-rejected")
            digest.update(chunk)
        return digest.hexdigest()

    def __enter__(self):
        try:
            # Resolve aliases only for unprivileged tool input, never to create
            # publication authority or adopt a caller-selected protected root.
            self.root = self.original_root.resolve(strict=True)
            directories = {self.root, *self.root.parents}
            for name in self.names:
                relative_name(name)
                directories.update((self.root / name).parent.parents)
                directories.add((self.root / name).parent)
            for path in sorted(directories, key=lambda path: len(path.parts)):
                descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
                self.entries.append((path, descriptor, file_snapshot(os.fstat(descriptor)), None))
            for name in sorted(self.names):
                path = self.root / name
                descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
                info = os.fstat(descriptor)
                self.entries.append((path, descriptor, file_snapshot(info), ""))
                if (
                    not stat.S_ISREG(info.st_mode)
                    or info.st_nlink != 1
                    or info.st_size > FILE_BYTES
                ):
                    reject("artifact-rejected")
                self.entries[-1] = (path, descriptor, file_snapshot(info), self.digest(descriptor))
            self.check()
            return self
        except BaseException as error:
            self.__exit__(None, None, None)
            if isinstance(error, OSError):
                reject("artifact-rejected")
            raise

    def check(self):
        try:
            if self.original_root.resolve(strict=True) != self.root:
                reject("artifact-rejected")
            for path, descriptor, snapshot, digest in self.entries:
                if (
                    file_snapshot(os.fstat(descriptor)) != snapshot
                    or file_snapshot(path.lstat()) != snapshot
                ):
                    reject("artifact-rejected")
                if digest is not None and self.digest(descriptor) != digest:
                    reject("artifact-rejected")
            # Recheck after reading so a concurrent write cannot hide behind its hash.
            for path, descriptor, snapshot, _ in self.entries:
                if (
                    file_snapshot(os.fstat(descriptor)) != snapshot
                    or file_snapshot(path.lstat()) != snapshot
                ):
                    reject("artifact-rejected")
        except OSError:
            reject("artifact-rejected")

    def __exit__(self, _type, _value, _traceback):
        for _, descriptor, _, _ in reversed(self.entries):
            os.close(descriptor)
        self.entries.clear()


class CodeVerifier(Protocol):
    def verify(self, path: Path, identifier: str) -> None: ...


class NativeCodeVerifier:
    """Inspect existing signatures. This adapter cannot sign or open a Keychain."""

    def verify(self, path: Path, identifier: str):
        if platform.system() != "Darwin" or platform.machine() != "arm64":
            reject("unsupported-host")
        major = platform.mac_ver()[0].split(".", 1)[0]
        if (
            not 1 <= len(major) <= 5
            or not major.isascii()
            or not major.isdecimal()
            or not 26 <= int(major) <= 65535
        ):
            reject("unsupported-host")
        if path.suffix == ".app":
            names = set()
            for directory, dirs, files in os.walk(path, followlinks=False):
                for name in dirs + files:
                    candidate = Path(directory) / name
                    if candidate.is_symlink():
                        reject("artifact-rejected")
                names.update(
                    (Path(directory) / name).relative_to(path.parent).as_posix() for name in files
                )
                if len(names) > 256:
                    reject("limit-exceeded")
            if not names:
                reject("artifact-rejected")
        else:
            names = {path.name}
        with RetainedArtifacts(path.parent, names) as retained:

            def checked_tool(arguments):
                retained.check()
                result = _tool(arguments)
                retained.check()
                return result

            self._verify_code(retained.root / path.name, identifier, checked_tool)
            retained.check()

    def _verify_code(self, path, identifier, tool):
        tool(
            [
                "/usr/bin/codesign",
                "--verify",
                "--strict",
                "--all-architectures",
                "-R",
                requirement(identifier),
                str(path),
            ]
        )
        _, details = tool(["/usr/bin/codesign", "--display", "--verbose=4", str(path)])
        if not any(
            line.startswith(b"CodeDirectory ") and b"runtime" in line.split(b"flags=", 1)[-1]
            for line in details.splitlines()
        ):
            reject("artifact-rejected")
        stdout, _ = tool(["/usr/bin/codesign", "--display", "--entitlements", ":-", str(path)])
        if stdout.strip():
            import plistlib

            try:
                entitlements = plistlib.loads(stdout)
            except (ValueError, plistlib.InvalidFileException):
                reject("artifact-rejected")
            if entitlements != {}:
                reject("artifact-rejected")
        executable = path / "Contents/MacOS/evidence-companion" if path.suffix == ".app" else path
        libraries, _ = tool(["/usr/bin/otool", "-L", str(executable)])
        lines = libraries.splitlines()
        if len(lines) < 2:
            reject("artifact-rejected")
        for line in lines[1:]:
            dependency = line.strip().split(b" (", 1)[0]
            if (
                not (
                    dependency.startswith(b"/usr/lib/")
                    or dependency.startswith(b"/System/Library/Frameworks/")
                )
                or b"/../" in dependency
            ):
                reject("artifact-rejected")
        commands, _ = tool(["/usr/bin/otool", "-l", str(executable)])
        if b"LC_RPATH" in commands or b"LC_DYLD_ENVIRONMENT" in commands:
            reject("artifact-rejected")
        architecture, _ = tool(["/usr/bin/lipo", "-archs", str(executable)])
        if architecture.strip() != b"arm64":
            reject("artifact-rejected")


@dataclass(frozen=True, slots=True)
class Preparation:
    artifact_sha256: str
    runnable: bool
    failure: str | None
    files: int


def prepare(source: Path, destination: Path, verifier: CodeVerifier) -> Preparation:
    """Copy pre-signed fixtures, then authenticate the exact destination copies.

    A rejected signature leaves a nonrunnable package for inspection. A manifest
    failure produces no runbook. Existing candidate storage is never overwritten.
    """
    data = read_regular(source, "fixture-manifest.json", MANIFEST_BYTES)
    manifest = validate_manifest(data)
    expected = {entry["name"] for entry in manifest["files"]} | {"fixture-manifest.json"}
    if _source_names(source, expected) != expected:
        reject("artifact-rejected")
    if destination.exists() or destination.is_symlink():
        reject("unsafe-native-state")
    destination.mkdir(mode=0o700)
    for entry in manifest["files"]:
        content = read_regular(source, entry["name"], FILE_BYTES)
        if len(content) != entry["size"] or hashlib.sha256(content).hexdigest() != entry["sha256"]:
            reject("artifact-rejected")
        path = destination / entry["name"]
        path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        with path.open("xb") as output:
            output.write(content)
        path.chmod(entry["mode"])
    (destination / "fixture-manifest.json").write_bytes(data)
    digest = hashlib.sha256(data).hexdigest()
    failure = None
    try:
        with RetainedArtifacts(destination, expected) as retained:
            if read_regular(destination, "fixture-manifest.json", MANIFEST_BYTES) != data:
                reject("artifact-rejected")
            for entry in manifest["code"]:
                retained.check()
                verifier.verify(destination / entry["name"], entry["identifier"])
                retained.check()
            # Reject unlisted native code, including nested libraries and executable resources.
            from spaceterm_tasks.macos_signing import MACH_O_MAGIC

            executable_names = {code_file(name) for name in CODE_IDENTIFIERS}
            for entry in manifest["files"]:
                content = read_regular(destination, entry["name"], FILE_BYTES)
                if content[:4] in MACH_O_MAGIC and entry["name"] not in executable_names:
                    reject("artifact-rejected")
                if hashlib.sha256(content).hexdigest() != entry["sha256"]:
                    reject("artifact-rejected")
            retained.check()
    except TaskError as error:
        failure = str(error) if str(error) in FAILURES else "native-unavailable"
    preflight = {
        "schema": 1,
        "scope": "native-evidence-520",
        "run_id": manifest["run_id"],
        "revision": manifest["revision"],
        "dirty": manifest["dirty"],
        "artifact_sha256": digest,
        "files": len(manifest["files"]),
        "signatures_verified": failure is None,
        "failure": failure,
        "native_installation_observed": False,
        "privileged_collision_check": "required",
        "independent_review": "required",
        "receipt_durability": "required-before-effects",
        "disposable_cases": disposable_cases(),
    }
    (destination / "preflight.json").write_bytes(canonical_json(preflight))
    if failure is None:
        for disposable, name in ((False, "runbook-local.sh"), (True, "runbook-disposable.sh")):
            (destination / name).write_text(render_runbook(manifest, digest, disposable))
            (destination / name).chmod(0o400)
    return Preparation(digest, failure is None, failure, len(manifest["files"]))


def disposable_cases() -> dict[str, str]:
    return {
        **dict.fromkeys(sorted(INTERACTIVE_CASES), "unrun"),
        **dict.fromkeys(
            ("crash-restart", "explicit-start", "approved-restart", "receipt-cleanup"), "required"
        ),
    }


def render_runbook(manifest: dict[str, Any], digest: str, disposable=False) -> str:
    """The shell contains digest literals, never a privileged parser of a manifest."""
    validate_manifest(canonical_json(manifest))
    hexadecimal(digest, 64)
    entries = {entry["name"]: entry for entry in manifest["files"]}
    hashes = [f'  hash_matches {shlex.quote(digest)} "$STAGE/fixture-manifest.json"']
    for entry in manifest["files"]:
        hashes.append(
            f'  hash_matches {shlex.quote(entry["sha256"])} "$STAGE/"{shlex.quote(entry["name"])}'
        )
    signatures = [
        f'  /usr/bin/codesign --verify --strict --all-architectures -R {shlex.quote(requirement(identifier))} "$STAGE/"{shlex.quote(name)} >/dev/null 2>&1'
        for name, identifier in CODE_IDENTIFIERS.items()
    ]
    bootstrap = canonical_json(
        {
            "schema": 1,
            "scope": "native-evidence-520-bootstrap",
            "run_id": manifest["run_id"],
            "artifact_sha256": digest,
            "admin_sha256": entries["evidence-admin"]["sha256"],
        }
    ).decode()
    tokens = {
        "@STAGE@": shlex.quote(str(STAGE)),
        "@ROOT@": shlex.quote(str(PROTECTED_ROOT)),
        "@RUN_ID@": manifest["run_id"],
        "@DIGEST@": digest,
        "@ADMIN_DIGEST@": entries["evidence-admin"]["sha256"],
        "@ADMIN_REQUIREMENT@": shlex.quote(requirement(CODE_IDENTIFIERS["evidence-admin"])),
        "@VERIFY_CANDIDATE@": "\n".join(hashes + signatures),
        "@BOOTSTRAP_RECEIPT@": bootstrap,
        "@COPY_BLOCKS@": str((entries["evidence-admin"]["size"] + 65536) // 65536),
        "@SUDO_MODE@": "-n" if disposable else "",
        "@SUDO_VALIDATE@": "/usr/bin/sudo -n /usr/bin/true >/dev/null 2>&1"
        if disposable
        else "/usr/bin/sudo -v",
        "@CLIENT_MODE@": "automated" if disposable else "live-session",
    }
    template = (ROOT / "packaging/macos/native-evidence/runbook.sh.in").read_text()
    for key, value in tokens.items():
        template = template.replace(key, value)
    if disposable:
        wrapper = (ROOT / "packaging/macos/native-evidence/disposable.sh.in").read_text()
        # The remote command runs the very same sequence under sudo -n.
        return wrapper.replace("@STAGE@", shlex.quote(str(STAGE))).replace(
            "@REMOTE_SEQUENCE@", template
        )
    return template


def new_receipt(
    *,
    run_id: str,
    account_uid: int,
    primary_gid: int,
    account_uuid: str,
    server_id: str,
    credential_slot: str,
    credential_binding_sha256: str,
    registration_sha256: str,
    artifact_sha256: str,
    artifacts: dict[str, str],
) -> dict[str, Any]:
    value = {
        "schema": 2,
        "scope": "native-evidence-520",
        "run_id": run_id,
        "account_uid": account_uid,
        "primary_gid": primary_gid,
        "account_uuid": account_uuid,
        "build_identity": "development",
        "artifact_sha256": artifact_sha256,
        "server_id": server_id,
        "credential_slot": credential_slot,
        "credential_binding_sha256": credential_binding_sha256,
        "registration_sha256": registration_sha256,
        "registration_name": f"development-{account_uid}.registration",
        "artifacts": dict(artifacts),
        "service_label": f"{IDENTIFIER}.{account_uid}",
        "objects": {resource: {"state": "not-started", "bindings": []} for resource in RESOURCES},
        "state": "prepared",
    }
    validate_receipt(value)
    return value


def validate_receipt(value):
    exact_fields(
        value,
        "schema scope run_id account_uid primary_gid account_uuid build_identity server_id credential_slot credential_binding_sha256 registration_sha256 registration_name artifact_sha256 artifacts service_label objects state".split(),
    )
    if (
        type(value["schema"]) is not int
        or value["schema"] != 2
        or value["scope"] != "native-evidence-520"
        or value["build_identity"] != "development"
    ):
        reject("invalid-record")
    for field, length in (
        ("run_id", 32),
        ("account_uuid", 32),
        ("server_id", 32),
        ("credential_slot", 32),
        ("credential_binding_sha256", 64),
        ("registration_sha256", 64),
        ("artifact_sha256", 64),
    ):
        hexadecimal(value[field], length)
    integer(value["account_uid"], 1, 2**32 - 2)
    integer(value["primary_gid"], 0, 2**32 - 2)
    if (
        value["service_label"] != f"{IDENTIFIER}.{value['account_uid']}"
        or value["registration_name"] != f"development-{value['account_uid']}.registration"
    ):
        reject("identity-mismatch")
    exact_fields(value["artifacts"], CODE_IDENTIFIERS)
    for digest in value["artifacts"].values():
        hexadecimal(digest, 64)
    exact_fields(value["objects"], RESOURCES)
    identities = set()
    for resource, effect in value["objects"].items():
        exact_fields(effect, ("state", "bindings"))
        if effect["state"] not in ("not-started", "pending", "installed", "removed"):
            reject()
        if type(effect["bindings"]) is not list or len(effect["bindings"]) > 256:
            reject("limit-exceeded")
        if effect["state"] == "not-started" and effect["bindings"]:
            reject()
        if effect["state"] == "installed" and not effect["bindings"]:
            reject()
        for binding in effect["bindings"]:
            exact_fields(binding, ("state", "identity"))
            if binding["state"] not in ("pending", "completed", "removed"):
                reject()
            if effect["state"] == "installed" and binding["state"] != "completed":
                reject()
            if effect["state"] == "removed" and binding["state"] != "removed":
                reject()
            validate_effect_identity(binding["identity"], resource, value)
            encoded = canonical_json(binding["identity"])
            if encoded in identities:
                reject()
            identities.add(encoded)
    if len(identities) > 256 or len(canonical_json(value)) > 65536:
        reject("limit-exceeded")
    if value["state"] not in (
        "prepared",
        "installing",
        "installed",
        "cleaning",
        "cleaned",
        "cleanup-unresolved",
    ):
        reject()
    if value["state"] == "installed" and any(
        effect["state"] != "installed" for effect in value["objects"].values()
    ):
        reject()
    if value["state"] == "cleaned" and any(
        effect["state"] not in ("not-started", "removed") for effect in value["objects"].values()
    ):
        reject()


def validate_effect_identity(identity, resource, receipt):
    if type(identity) is not dict:
        reject()
    kind = identity.get("kind")
    allowed = {
        "fixtures": ("filesystem",),
        "registration": ("filesystem",),
        "credential": ("credential-item",),
        "service": ("service", "process"),
    }
    if kind not in allowed[resource]:
        reject()
    match kind:
        case "filesystem":
            exact_fields(
                identity,
                "kind name file_type volume_uuid device inode generation birth_seconds birth_nanoseconds artifact_sha256".split(),
            )
            relative_name(identity["name"])
            if identity["file_type"] not in ("file", "directory"):
                reject()
            hexadecimal(identity["volume_uuid"], 32)
            for field in ("device", "generation", "birth_seconds"):
                integer(identity[field], 0, 2**64 - 1)
            integer(identity["inode"], 1, 2**64 - 1)
            integer(identity["birth_nanoseconds"], 0, 999999999)
            if identity["file_type"] == "file":
                hexadecimal(identity["artifact_sha256"], 64)
            elif identity["artifact_sha256"] is not None:
                reject()
            if resource == "registration" and (
                identity["name"] != receipt["registration_name"]
                or identity["artifact_sha256"] != receipt["registration_sha256"]
                or identity["file_type"] != "file"
            ):
                reject("identity-mismatch")
            code_names = {code_file(name): digest for name, digest in receipt["artifacts"].items()}
            if (
                identity["name"] in code_names
                and identity["artifact_sha256"] != code_names[identity["name"]]
            ):
                reject("identity-mismatch")
        case "credential-item":
            exact_fields(
                identity, ("kind", "keychain", "persistent_reference", "credential_binding_sha256")
            )
            if (
                identity["keychain"] != "system"
                or identity["credential_binding_sha256"] != receipt["credential_binding_sha256"]
            ):
                reject("identity-mismatch")
            reference = identity["persistent_reference"]
            if type(reference) is not str or not 1 <= len(reference) <= 4096:
                reject()
            try:
                decoded = base64.b64decode(reference, validate=True)
            except (ValueError, binascii.Error):
                reject()
            if not decoded or base64.b64encode(decoded).decode() != reference:
                reject()
        case "service":
            exact_fields(identity, ("kind", "label", "native_instance_id"))
            if identity["label"] != receipt["service_label"]:
                reject("identity-mismatch")
            hexadecimal(identity["native_instance_id"], 64)
        case "process":
            exact_fields(
                identity,
                "kind role pid start_seconds start_microseconds boot_id executable_sha256".split(),
            )
            if identity["role"] not in ("server", "retriever", "session"):
                reject()
            integer(identity["pid"], 1, 2**31 - 1)
            integer(identity["start_seconds"], 0, 2**64 - 1)
            integer(identity["start_microseconds"], 0, 999999)
            hexadecimal(identity["boot_id"], 32)
            hexadecimal(identity["executable_sha256"], 64)


class ReceiptStorage(Protocol):
    def serialized(self) -> AbstractContextManager[None]: ...
    def exists(self) -> bool: ...
    def read(self) -> dict[str, Any]: ...
    def publish(self, receipt: dict[str, Any]) -> None: ...


class ReceiptStore:
    """Durable directory-relative receipt IO. The caller owns native protection.

    This is not a native protected-directory provider. Production admin code must
    retain its own authenticated storage and serialization authority. Uncertain IO
    latches this instance; later synchronization cannot clear that original outcome.
    """

    def __init__(self, directory: Path):
        self.directory = directory
        self._uncertain = False

    @contextmanager
    def serialized(self):
        directory = os.open(self.directory, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        descriptor = None
        try:
            descriptor = os.open(
                ".evidence.lock",
                os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW | os.O_NONBLOCK,
                0o600,
                dir_fd=directory,
            )
            info = os.fstat(descriptor)
            if (
                not stat.S_ISREG(info.st_mode)
                or info.st_nlink != 1
                or info.st_uid != os.geteuid()
                or info.st_mode & 0o077
            ):
                reject("unsafe-native-state")
            try:
                fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                reject("outcome-pending")
            yield
        finally:
            if descriptor is not None:
                os.close(descriptor)
            os.close(directory)

    def exists(self) -> bool:
        return os.path.lexists(self.directory / "receipt.json")

    def read(self) -> dict[str, Any]:
        if self._uncertain or os.path.lexists(self.directory / "receipt.pending"):
            reject("indeterminate")
        value = decode_json(read_regular(self.directory, "receipt.json", 65536), 65536)
        validate_receipt(value)
        return value

    def publish(self, receipt: dict[str, Any]):
        if self._uncertain:
            reject("indeterminate")
        validate_receipt(receipt)
        directory = os.open(self.directory, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        descriptor = None
        temporary = "receipt.pending"
        try:
            descriptor = os.open(
                temporary,
                os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                0o600,
                dir_fd=directory,
            )
            data = canonical_json(receipt)
            offset = 0
            while offset < len(data):
                written = os.write(descriptor, data[offset:])
                if not written:
                    raise OSError()
                offset += written
            os.fsync(descriptor)
            if platform.system() == "Darwin":
                fcntl.fcntl(descriptor, 51)  # F_FULLFSYNC: no durability fallback.
            # Keep the exact pending inode until the new receipt and its name are
            # durable. A recreated consumer refuses it instead of guessing which
            # identity publications survived. Operator reconciliation is required.
            os.link(temporary, "receipt.next", src_dir_fd=directory, dst_dir_fd=directory)
            os.rename("receipt.next", "receipt.json", src_dir_fd=directory, dst_dir_fd=directory)
            os.fsync(directory)
            os.unlink(temporary, dir_fd=directory)
            # The receipt is already durable. A stale pending name after a crash
            # is a conservative refusal, not authority to repeat a cleanup effect.
        except OSError:
            self._uncertain = True
            reject("indeterminate")
        finally:
            if descriptor is not None:
                os.close(descriptor)
            os.close(directory)


@dataclass(frozen=True, slots=True)
class CleanupObservation:
    scope_verified: bool
    process_absent: bool
    retriever_absent: bool
    sessions_absent: bool
    endpoint_absent: bool

    @property
    def complete(self) -> bool:
        return all(
            type(value) is bool and value
            for value in (
                self.scope_verified,
                self.process_absent,
                self.retriever_absent,
                self.sessions_absent,
                self.endpoint_absent,
            )
        )


class FixtureNativeOperations(Protocol):
    """Native admin implements this port under protected serialization.

    install records each native identity through the durable callback before
    exposing it or making another effect. A crash before identity publication is
    unresolved, never permission to rediscover an object by name or content hash.
    The returned list must match every recorded identity. References come from
    actual native observations, not fixture-generated markers.

    retire drains work and proves absence of the recorded previous process scope.
    retain_cleanup_bindings authenticates and retains exact objects through remove.
    remove independently rechecks those retained identities before deletion. An
    unavailable native incarnation or replacement race must leave cleanup unresolved.
    """

    def inspect_collisions(self, receipt: dict[str, Any]) -> None: ...
    def install(
        self,
        resource: str,
        receipt: dict[str, Any],
        record_identity: Callable[[dict[str, Any]], None],
    ) -> list[dict[str, Any]]: ...
    def retire(self, receipt: dict[str, Any]) -> CleanupObservation: ...
    def retain_cleanup_bindings(
        self, resource: str, receipt: dict[str, Any]
    ) -> list[dict[str, Any]]: ...
    def remove(self, resource: str, receipt: dict[str, Any]) -> None: ...


@dataclass(frozen=True, slots=True)
class RunOutcome:
    failure: str | None
    signal: int | None = None

    @property
    def exit_code(self) -> int:
        if self.failure in ("indeterminate", "cleanup-unresolved"):
            return 75
        if self.signal is not None:
            return 128 + self.signal
        return 0 if self.failure is None else 1


def _failure(error: BaseException) -> str:
    if isinstance(error, TaskError) and str(error) in FAILURES:
        return str(error)
    if isinstance(error, (KeyboardInterrupt, SystemExit)):
        return "authorization-cancelled"
    return "io-unavailable"


def cleanup_receipt(store: ReceiptStorage, native: FixtureNativeOperations) -> None:
    """Idempotent recovery after partial installation. Preserve unresolved scope."""
    with store.serialized(), preserve_cancellation():
        _cleanup_receipt(store, native)


def _cleanup_receipt(store: ReceiptStorage, native: FixtureNativeOperations) -> None:
    receipt = store.read()
    if receipt["state"] == "cleaned":
        return
    receipt["state"] = "cleaning"
    store.publish(receipt)
    try:
        service = receipt["objects"]["service"]
        if service["state"] in ("pending", "installed") and not any(
            binding["identity"]["kind"] == "process" for binding in service["bindings"]
        ):
            # A service label or configuration is not prior process scope. Even
            # a legitimately stopped service needs operator reconciliation here.
            reject("cleanup-unresolved")
        observation = native.retire(copy.deepcopy(receipt))
        if type(observation) is not CleanupObservation or not observation.complete:
            reject("cleanup-unresolved")
        for resource in reversed(RESOURCES):
            effect = receipt["objects"][resource]
            if effect["state"] not in ("pending", "installed"):
                continue
            identities = [binding["identity"] for binding in effect["bindings"]]
            if (
                not identities
                or native.retain_cleanup_bindings(resource, copy.deepcopy(receipt)) != identities
            ):
                reject("cleanup-unresolved")
            native.remove(resource, copy.deepcopy(receipt))
            effect["state"] = "removed"
            for binding in effect["bindings"]:
                binding["state"] = "removed"
            store.publish(receipt)
    except BaseException:
        receipt["state"] = "cleanup-unresolved"
        store.publish(receipt)
        reject("cleanup-unresolved")
    receipt["state"] = "cleaned"
    store.publish(receipt)


@dataclass(slots=True)
class Cancellation:
    signum: int | None = None


@contextmanager
def preserve_cancellation():
    """First signal unwinds into cleanup; subsequent signals cannot interrupt it."""
    cancellation = Cancellation()

    def cancel(signum, _frame):
        if cancellation.signum is None:
            cancellation.signum = signum
            raise SystemExit(128 + signum)

    previous = {
        number: signal.getsignal(number)
        for number in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP)
    }
    try:
        for number in previous:
            signal.signal(number, cancel)
        yield cancellation
    finally:
        for number, handler in previous.items():
            signal.signal(number, handler)


def orchestrate(
    receipt: dict[str, Any],
    store: ReceiptStorage,
    native: FixtureNativeOperations,
    exercise: Callable[[], None],
) -> RunOutcome:
    with preserve_cancellation() as cancellation:
        try:
            with store.serialized():
                outcome = _orchestrate(receipt, store, native, exercise)
        except BaseException as error:
            outcome = RunOutcome(_failure(error), cancellation.signum)
        return RunOutcome(outcome.failure, cancellation.signum or outcome.signal)


def _orchestrate(
    receipt: dict[str, Any],
    store: ReceiptStorage,
    native: FixtureNativeOperations,
    exercise: Callable[[], None],
) -> RunOutcome:
    """Install and always clean up independently of the output channel.

    Mutation has no default composition. A caller must hold native serialization
    and verify the receipt/account/artifacts before implementing the native port.
    """
    validate_receipt(receipt)
    receipt = copy.deepcopy(receipt)
    admitted = False
    failure = None
    first_signal = None
    try:
        if store.exists():
            reject("unsafe-native-state")
        native.inspect_collisions(copy.deepcopy(receipt))
        store.publish(receipt)
        admitted = True
        receipt["state"] = "installing"
        for resource in RESOURCES:
            effect = receipt["objects"][resource]
            effect["state"] = "pending"
            store.publish(receipt)

            def record_identity(identity, effect=effect):
                validate_effect_identity(identity, resource, receipt)
                if any(binding["identity"] == identity for binding in effect["bindings"]):
                    reject("invalid-record")
                effect["bindings"].append({"state": "pending", "identity": copy.deepcopy(identity)})
                store.publish(receipt)

            identities = native.install(resource, copy.deepcopy(receipt), record_identity)
            if not identities or identities != [
                binding["identity"] for binding in effect["bindings"]
            ]:
                reject("unsafe-native-state")
            effect["state"] = "installed"
            for binding in effect["bindings"]:
                binding["state"] = "completed"
            store.publish(receipt)
        receipt["state"] = "installed"
        store.publish(receipt)
        exercise()
    except BaseException as error:
        failure = _failure(error)
        if isinstance(error, KeyboardInterrupt):
            first_signal = signal.SIGINT
        elif (
            isinstance(error, SystemExit)
            and type(error.code) is int
            and error.code in (128 + signal.SIGINT, 128 + signal.SIGTERM, 128 + signal.SIGHUP)
        ):
            first_signal = error.code - 128
    finally:
        if admitted and failure != "indeterminate":
            try:
                _cleanup_receipt(store, native)
            except BaseException as error:
                failure = (
                    "indeterminate" if _failure(error) == "indeterminate" else "cleanup-unresolved"
                )
    return RunOutcome(failure, first_signal)


class EvidenceArgumentParser(argparse.ArgumentParser):
    def error(self, message):
        # argparse's default error includes supplied paths and arbitrary arguments.
        reject("invalid-record")


def required_cases(profile: str) -> list[RequiredCase]:
    if profile not in ("local", "disposable"):
        reject("invalid-record")
    context = "gui-parented" if profile == "local" else "disposable-macos"
    daemon = "daemon-parented" if profile == "local" else "disposable-macos"
    cases = [
        RequiredCase("native-installation", "stopped-before-credential-and-retention", daemon),
        RequiredCase("native-installation", "startup-never-selects-staged-installation", daemon),
        RequiredCase("macos-keychain", "exact-startup-retrieval", daemon),
        RequiredCase("macos-launcher", "privilege-and-inheritance-proof", daemon),
        RequiredCase("macos-management", "stop-drains-started-git-write-before-bootout", daemon),
        RequiredCase(
            "macos-evidence", "bootstrap-authenticates-protected-admin-before-root-exec", context
        ),
        RequiredCase("macos-evidence", "receipt-cleanup", context),
    ]
    cases.extend(
        RequiredCase("macos-keychain", name, daemon, "denied")
        for name in (
            "unprivileged-copy-denied",
            "unsigned-copy-denied",
            "wrong-identifier-denied",
            "other-account-denied",
            "stopped-retrieval-denied",
            "stale-generation-denied",
        )
    )
    cases.extend(
        RequiredCase("macos-management", name, daemon)
        for name in ("crash-restart", "explicit-start", "approved-restart", "fresh-signed-update")
    )
    if profile == "local":
        for probe_context in ("gui-parented", "daemon-parented"):
            cases.extend(
                RequiredCase("macos-capabilities", name, probe_context, None)
                for name in CAPABILITIES
            )
        for name, result in (
            ("authorization-denied", "denied"),
            ("authorization-cancelled", "cancelled"),
            ("authorization-unavailable", "unavailable"),
            ("authorization-granted", "passed"),
        ):
            cases.append(RequiredCase("macos-management", name, context, result))
    else:
        cases.extend(
            RequiredCase("macos-capabilities", name, context, "unrun") for name in CAPABILITIES
        )
    return cases


def collect_task() -> int:
    parser = EvidenceArgumentParser(
        description="Validate exact native evidence without inventing observations"
    )
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--profile", choices=("local", "disposable"), default="local")
    arguments = parser.parse_args()
    descriptor = os.open(arguments.input, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as source:
        info = os.fstat(descriptor)
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or info.st_size > 32 * 1024 * 1024:
            reject("invalid-record")
        summary = collect(source, required_cases(arguments.profile))
        if file_snapshot(os.fstat(descriptor)) != file_snapshot(info):
            reject("invalid-record")
    print(
        f"classification=records-valid records={summary.records} native_passed={summary.passed} unrun={summary.unrun} unresolved={summary.unresolved}"
    )
    # Valid Unrun evidence is retained, but it cannot pass a required native gate.
    return 2 if summary.unrun or summary.unresolved else 0


def prepare_task() -> int:
    parser = EvidenceArgumentParser(
        description="Assemble existing signed fixtures without signing or host changes"
    )
    parser.add_argument("--fixtures", type=Path)
    arguments = parser.parse_args()
    if arguments.fixtures is None:
        print("phase=prepare classification=native-unavailable")
        return 2
    preparation = prepare(arguments.fixtures, STAGE, NativeCodeVerifier())
    print(
        f"phase=prepare files={preparation.files} classification={'preflight' if preparation.runnable else preparation.failure}"
    )
    return 0 if preparation.runnable else 2
