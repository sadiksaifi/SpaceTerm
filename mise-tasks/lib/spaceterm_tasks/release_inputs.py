"""Transfer only compiled products between credential-free builds and protected signing."""

import hashlib
import json
import os
import shutil
import tarfile
import tempfile
from pathlib import Path

from spaceterm_tasks import TaskError, macos_signing, release, release_plan, sparkle
from spaceterm_tasks.cargo import build_executable
from spaceterm_tasks.core_package import compile_unsigned, host_target, sign_unsigned
from spaceterm_tasks.packaging import (
    DESKTOP_FLOOR,
    DIST,
    RELEASE_FLOOR,
    RELEASE_NOTES,
    packaged_environment,
)
from spaceterm_tasks.update_signing import sha256


def compile_products():
    plan = release_plan.load()
    target = host_target()
    with tempfile.TemporaryDirectory(dir=DIST, prefix=".compile-") as temporary:
        tree = Path(temporary) / "inputs"
        tree.mkdir()
        core = compile_unsigned("spaceterm", plan["tag"], destination=tree / "core")
        info = json.loads((core / "build.json").read_bytes())
        environment = macos_signing.signing_environment(packaged_environment(plan["tag"]))
        environment.update(
            {
                RELEASE_FLOOR: plan["floors"]["core"],
                DESKTOP_FLOOR: plan["floors"]["desktop"],
                RELEASE_NOTES: str(release.NOTES),
                "SPACETERM_CLI_SOURCE_STATE": info["source_sha256"],
            }
        )
        if target == "aarch64-apple-darwin":
            environment["SPACETERM_SPARKLE_DIR"] = str(sparkle.directory())
        executable, metadata = build_executable(
            "--release",
            "--locked",
            "--no-default-features",
            "--features",
            "desktop",
            env=environment,
            portable_linux=target == "x86_64-unknown-linux-gnu",
            metadata="desktop-build-info.json",
        )
        desktop = tree / "desktop"
        desktop.mkdir()
        shutil.copyfile(executable, desktop / "spaceterm-desktop")
        (desktop / "spaceterm-desktop").chmod(0o755)
        (desktop / "desktop-build-info.json").write_bytes(metadata)
        (tree / "manifest.json").write_text(
            json.dumps(
                {
                    "source": plan["source"],
                    "plan_sha256": sha256(release_plan.PLAN),
                    "target": target,
                    "files": {
                        str(path.relative_to(tree)): sha256(path)
                        for path in tree.rglob("*")
                        if path.is_file()
                    },
                }
            )
            + "\n"
        )
        output = DIST / "compiled" / target
        output.mkdir(parents=True, exist_ok=True)
        with tarfile.open(output / "inputs.tar.gz", "w:gz") as archive:
            archive.add(tree, arcname="inputs")


def unpack_products(path):
    plan = release_plan.load()
    destination = DIST / "compiled-inputs"
    shutil.rmtree(destination, ignore_errors=True)
    destination.mkdir()
    with tarfile.open(path, "r:gz") as archive:
        if any(
            not (entry.isfile() or entry.isdir())
            or entry.name.startswith("/")
            or ".." in Path(entry.name).parts
            for entry in archive
        ):
            raise TaskError("compiled inputs contain unsupported archive entries")
        archive.extractall(destination, filter="data")
    tree = destination / "inputs"
    manifest = json.loads((tree / "manifest.json").read_bytes())
    files = {
        str(path.relative_to(tree)): sha256(path)
        for path in tree.rglob("*")
        if path.is_file() and path != tree / "manifest.json"
    }
    if (
        manifest["source"] != plan["source"]
        or manifest["plan_sha256"] != sha256(release_plan.PLAN)
        or manifest["target"] != host_target()
        or manifest["files"] != files
    ):
        raise TaskError("compiled inputs differ from the exact release plan")
    (tree / "desktop/spaceterm-desktop").chmod(0o755)
    return tree


def sign_core(path):
    tree = unpack_products(path)
    plan = release_plan.load()
    target = host_target()
    retained = DIST / "notarization/final-core" / target
    destination = release.CORE / target
    if all((retained / name).is_file() for name in release.CORE_FILES):
        with tempfile.TemporaryDirectory(dir=DIST, prefix=".restore-core-") as temporary:
            release.verified_core(
                retained, target, plan["version"], plan["floors"]["core"], Path(temporary)
            )
        shutil.copytree(retained, destination, dirs_exist_ok=True)
        return
    archive, signature = sign_unsigned(tree / "core", identity="spaceterm", release_tag=plan["tag"])
    destination.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(archive, destination / "artifact.tar.gz")
    shutil.copyfile(signature, destination / "artifact.sig")
    (destination / "artifact.sha256").write_text(sha256(archive) + "\n")
    shutil.copytree(destination, retained, dirs_exist_ok=True)


def package_desktop(path):
    """T5b's packaging entry point consumes executable + Cargo metadata, without Cargo."""
    tree = unpack_products(path)
    plan = release_plan.load()
    os.environ["SPACETERM_CLI_ARTIFACTS"] = str(release.core_catalog(release.CORE).resolve())
    if host_target() == "aarch64-apple-darwin":
        from spaceterm_tasks import package_macos

        package_macos.package(plan["tag"], release.NOTES, compiled=tree)
        release.appcast(plan["tag"])
    else:
        from spaceterm_tasks import package_linux

        package_linux.package(plan["tag"], release.NOTES, compiled=tree)
        release.DESKTOP_LINUX.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(
            package_linux.archive_path("spaceterm"),
            release.DESKTOP_LINUX
            / release.archive_name("desktop", plan["version"], "linux-x86_64"),
        )
    write_evidence(plan)


def write_evidence(plan):
    """Record equality of both finalized Core archives with each desktop's resources."""
    if host_target() == "aarch64-apple-darwin":
        resources = DIST / "SpaceTerm.app/Contents/Resources/core-artifacts"
        destination = release.DESKTOP_MACOS
    else:
        resources = None
        destination = release.DESKTOP_LINUX
    expected = {
        core: sha256(release.CORE / core / "artifact.tar.gz")
        for core in ("aarch64-apple-darwin", "x86_64-unknown-linux-gnu")
    }
    if resources is not None:
        actual = {core: sha256(resources / core / "artifact.tar.gz") for core in expected}
    else:
        archive = destination / release.archive_name("desktop", plan["version"], "linux-x86_64")
        actual = {}
        with tarfile.open(archive, "r:gz") as packed:
            for core in expected:
                stream = packed.extractfile(
                    f"spaceterm/share/spaceterm/core-artifacts/{core}/artifact.tar.gz"
                )
                if stream is None:
                    raise TaskError("the Linux desktop lacks a finalized Core archive")
                actual[core] = hashlib.file_digest(stream, "sha256").hexdigest()
    if actual != expected:
        raise TaskError("the desktop changed its finalized Core archive bytes")
    (destination / "evidence.json").write_text(
        json.dumps(
            {"source": plan["source"], "plan_sha256": sha256(release_plan.PLAN), "core": expected}
        )
        + "\n"
    )


def verify_evidence():
    plan = release_plan.load()
    expected = {
        "source": plan["source"],
        "plan_sha256": sha256(release_plan.PLAN),
        "core": {
            target: sha256(release.CORE / target / "artifact.tar.gz")
            for target in ("aarch64-apple-darwin", "x86_64-unknown-linux-gnu")
        },
    }
    for directory in (release.DESKTOP_MACOS, release.DESKTOP_LINUX):
        if json.loads((directory / "evidence.json").read_bytes()) != expected:
            raise TaskError("the desktop evidence differs from the finalized Core archive bytes")
    smoke = json.loads((DIST / "floor-smoke.json").read_bytes())
    if (
        smoke["source"] != plan["source"]
        or smoke["tag"] != plan["tag"]
        or smoke["sha256"] != expected["core"]["x86_64-unknown-linux-gnu"]
    ):
        raise TaskError("the floor smoke did not exercise these exact final Core bytes")
