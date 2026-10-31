"""Disposable source snapshots and signed Preflight candidates for stream F's native runner."""

import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError, release, server_package
from spaceterm_tasks.server_builds import _source


def _git(root, *arguments):
    subprocess.run(
        [
            "git",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.name=SpaceTerm fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-C",
            str(root),
            *arguments,
        ],
        check=True,
        stdout=subprocess.DEVNULL,
    )


def _commit(root, description):
    _git(root, "add", "-u")
    _git(root, "commit", "-qm", description)


def _record(archive, signature, role, target):
    release.verify_ed25519(
        server_package.artifact_public_key("spaceterm-preflight"), archive, signature
    )
    with tarfile.open(archive, "r:gz") as source:
        stream = source.extractfile("server/build.json")
        if stream is None:
            raise TaskError("the fixture build document is missing")
        info = server_package.document(stream.read(4097))
    if (
        info["identity"] != "spaceterm-preflight"
        or info["target"] != target
        or info["protocol_major"]
        != server_package.PROTOCOL_MAJOR + (1 if role == "next-major" else 0)
    ):
        raise TaskError("the fixture has a different identity, target or protocol major")
    with tempfile.TemporaryDirectory(prefix="spaceterm-resume-fixture-") as temporary:
        server_package.verified_copy(archive, signature, Path(temporary), info, native=False)
    return {
        "archive": str(archive),
        "signature": str(signature),
        "sha256": release.sha256(archive),
        "protocol_major": info["protocol_major"],
        "version": info["version"],
    }


def _build(root, destination, role, targets, result, resume):
    cache = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
    environment = {
        **os.environ,
        "PYTHONPATH": str(root / "mise-tasks/lib"),
        "CARGO_TARGET_DIR": str(cache / "admin-fixtures" / root.parent.name),
    }
    for target in targets:
        output = destination / target
        output.mkdir(exist_ok=True)
        saved = output / f"{role}.tar.gz"
        detached = saved.with_suffix(saved.suffix + ".sig")
        if resume and saved.exists() and detached.exists():
            result.setdefault(target, {})[role] = _record(saved, detached, role, target)
            print(f"Reused verified {role} for {target}", flush=True)
            continue
        print(f"Building {role} for {target}", flush=True)
        recorded = root.parent / "built.json"
        subprocess.run(
            [
                sys.executable,
                "-c",
                "import json,sys; from pathlib import Path; "
                "from spaceterm_tasks.server_package import package; "
                "a,s=package('spaceterm-preflight',target=sys.argv[1]); "
                "Path(sys.argv[2]).write_text(json.dumps([str(a),str(s)]))",
                target,
                str(recorded),
            ],
            env=environment,
            cwd=root,
            check=True,
        )
        archive, signature = map(Path, json.loads(recorded.read_text()))
        shutil.copyfile(archive, saved)
        shutil.copyfile(signature, detached)
        result.setdefault(target, {})[role] = _record(saved, detached, role, target)


def _broken_verification(destination, target, result):
    normal = Path(result[target]["normal"]["archive"])
    broken = destination / target / "broken.tar.gz"
    signature = broken.with_suffix(broken.suffix + ".sig")
    resources = server_package.resource_directory(target, "spaceterm-preflight")
    omitted = f"server/{resources}/shell-integration/VERSION"
    with tarfile.open(normal, "r:gz") as source, tarfile.open(broken, "w:gz") as output:
        for member in source:
            if member.name != omitted:
                output.addfile(member, source.extractfile(member) if member.isfile() else None)
    release.sign_ed25519(
        server_package.artifact_signing_seed("spaceterm-preflight"), broken, signature
    )
    release.verify_ed25519(
        server_package.artifact_public_key("spaceterm-preflight"), broken, signature
    )
    # Its signature is genuine; the missing resource must still refuse verification.
    with tempfile.TemporaryDirectory(prefix="spaceterm-broken-fixture-") as temporary:
        try:
            server_package.verified_copy(
                broken,
                signature,
                Path(temporary),
                None,
                identity="spaceterm-preflight",
                target=target,
                native=False,
            )
        except TaskError:
            pass
        else:
            raise TaskError("the broken fixture unexpectedly passed verification")
    result[target]["broken"] = {
        "archive": str(broken),
        "signature": str(signature),
        "sha256": release.sha256(broken),
    }


def prepare(destination, selected, *, resume=False):
    destination = Path(destination).absolute()
    if destination.exists() and not resume:
        raise TaskError("the fixture destination already exists")
    targets = list(server_package.TARGETS.values()) if selected == "all" else [selected]
    if server_package.TARGETS["darwin"] in targets and sys.platform != "darwin":
        raise TaskError("build macOS fixtures natively on the Mac")
    destination.mkdir(parents=True, exist_ok=resume)
    result = {}
    files, repositories, _ = _source(ROOT)
    with tempfile.TemporaryDirectory(prefix="spaceterm-admin-source-") as temporary:
        root = Path(temporary) / "source"
        root.mkdir()
        for relative, original in files.items():
            copied = root / relative
            copied.parent.mkdir(parents=True, exist_ok=True)
            if original.is_symlink():
                copied.symlink_to(original.readlink())
            else:
                shutil.copy2(original, copied)
        # The Ghostty build clones its pinned submodule commit into the Cargo output.
        for relative, commit in repositories.items():
            if relative == Path("."):
                continue
            nested = root / relative
            _git(nested, "init", "-q")
            _git(nested, "fetch", "-q", "--depth=1", "--no-tags", str(ROOT / relative), commit)
            _git(nested, "update-ref", "--no-deref", "HEAD", commit)
            _git(nested, "reset", "--mixed", "-q", commit)
        _git(root, "init", "-q")
        (root / "NATIVE_FIXTURE").write_text("restore candidate\n")
        _git(root, "add", "-f", ".")
        _commit(root, "fixture: restore candidate")
        _build(root, destination, "restore", targets, result, resume)
        (root / "NATIVE_FIXTURE").write_text("normal update candidate\n")
        _commit(root, "fixture: normal candidate")
        _build(root, destination, "normal", targets, result, resume)
        for target in targets:
            _broken_verification(destination, target, result)
        major = root / server_package.PROTOCOL_MAJOR_SOURCE
        current = major.read_text()
        major.write_text(
            current.replace(
                f"pub const MAJOR: u16 = {server_package.PROTOCOL_MAJOR};",
                f"pub const MAJOR: u16 = {server_package.PROTOCOL_MAJOR + 1};",
            )
        )
        if server_package.protocol_major(root) != server_package.PROTOCOL_MAJOR + 1:
            raise TaskError("the next-major fixture did not change the protocol major")
        _commit(root, "fixture: next protocol major candidate")
        _build(root, destination, "next-major", targets, result, resume)
        major.write_text(current)
        entry = root / "crates/spaceterm-cli/src/main.rs"
        text = entry.read_text()
        start, end = text.index("fn run_server("), text.index("\nfn complete")
        entry.write_text(
            text[:start]
            + "fn run_server(_service: bool, _owned: bool) -> Result<(), ServerError> {\n    Err(ServerError::StartupFailed)\n}\n"
            + text[end:]
        )
        _commit(root, "fixture: startup failure for return acceptance")
        _build(root, destination, "broken-startup", targets, result, resume)
    manifest = destination / "fixtures.json"
    manifest.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result, indent=2), flush=True)
    print(f"Fixture manifest: {manifest}")
