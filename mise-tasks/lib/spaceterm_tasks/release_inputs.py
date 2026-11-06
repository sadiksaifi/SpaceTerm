"""Transfer only compiled products between credential-free builds and protected signing."""

import hashlib
import json
import os
import re
import shutil
import tarfile
import tempfile
from pathlib import Path

from spaceterm_tasks import TaskError, macos_signing, release, release_plan, sparkle
from spaceterm_tasks.cargo import build_executable
from spaceterm_tasks.core_package import (
    compile_unsigned,
    executable_path,
    host_target,
    sign_unsigned,
)
from spaceterm_tasks.packaging import (
    DESKTOP_FLOOR,
    DIST,
    RELEASE_FLOOR,
    RELEASE_NOTES,
    checked,
    packaged_environment,
)
from spaceterm_tasks.update_signing import sha256


def verify_compiled(executable, target):
    if target == "aarch64-apple-darwin":
        if checked(
            ["lipo", "-archs", str(executable)], "compiled architecture unavailable", text=True
        ).split() != ["arm64"]:
            raise TaskError("compiled macOS products require Apple Silicon code")
        build = checked(
            ["xcrun", "vtool", "-show-build", str(executable)],
            "compiled deployment target unavailable",
            text=True,
        )
        if "minos 26.0" not in " ".join(build.split()):
            raise TaskError("compiled macOS products require deployment target 26.0")
    else:
        from spaceterm_tasks.linux_toolchain import MANIFEST, sysroot_path
        from spaceterm_tasks.package_linux import verify_glibc_baseline

        if b"elf64-x86-64" not in checked(
            ["objdump", "-f", str(executable)], "compiled architecture unavailable"
        ):
            raise TaskError("compiled Linux products require x86_64 code")
        verify_glibc_baseline(executable, "compiled Linux product")
        program = checked(["readelf", "-l", str(executable)], "compiled interpreter unavailable")
        if b"[Requesting program interpreter: /lib64/ld-linux-x86-64.so.2]" not in program:
            raise TaskError("compiled Linux products require the glibc dynamic loader")
        dependencies = checked(
            ["objdump", "-p", str(executable)], "compiled dependencies unavailable"
        )
        root = sysroot_path(MANIFEST, Path(os.environ["SPACETERM_LINUX_PACKAGE_SYSROOT"]))
        allowed = {path.name for path in root.rglob("*.so*") if path.is_file()}
        if (
            not set(name.decode() for name in re.findall(rb"\bNEEDED\s+(\S+)", dependencies))
            <= allowed
        ):
            raise TaskError("compiled Linux products require a library outside the pinned sysroot")


def compile_products():
    plan = release_plan.load()
    target = host_target()
    with tempfile.TemporaryDirectory(dir=DIST, prefix=".compile-") as temporary:
        tree = Path(temporary) / "inputs"
        tree.mkdir()
        core = compile_unsigned("spaceterm", plan["tag"], destination=tree / "core")
        info = json.loads((core / "build.json").read_bytes())
        verify_compiled(core / executable_path(target, "spaceterm"), target)
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
        verify_compiled(executable, target)
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
                retained,
                target,
                plan["version"],
                plan["floors"]["core"],
                Path(temporary),
                execute=False,
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
    require_core_evidence(plan)
    os.environ["SPACETERM_CLI_ARTIFACTS"] = str(release.core_catalog(release.CORE).resolve())
    if host_target() == "aarch64-apple-darwin":
        from spaceterm_tasks import package_macos

        package_macos.package(plan["tag"], release.NOTES, compiled=tree, execute=False)
        release.appcast(plan["tag"])
    else:
        from spaceterm_tasks import package_linux

        package_linux.package(plan["tag"], release.NOTES, compiled=tree, execute=False)
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
                digest = hashlib.sha256()
                while chunk := stream.read(1024 * 1024):
                    digest.update(chunk)
                actual[core] = digest.hexdigest()
    if actual != expected:
        raise TaskError("the desktop changed its finalized Core archive bytes")
    (destination / "evidence.json").write_text(
        json.dumps(
            {
                "source": plan["source"],
                "plan_sha256": sha256(release_plan.PLAN),
                "core": expected,
                "files": {
                    path.name: sha256(path)
                    for path in destination.iterdir()
                    if path.name != "evidence.json"
                },
            }
        )
        + "\n"
    )


def verify_evidence(plan):
    require_core_evidence(plan)
    expected = {
        "source": plan["source"],
        "plan_sha256": sha256(release_plan.PLAN),
        "core": {
            target: sha256(release.CORE / target / "artifact.tar.gz")
            for target in ("aarch64-apple-darwin", "x86_64-unknown-linux-gnu")
        },
    }
    for directory in (release.DESKTOP_MACOS, release.DESKTOP_LINUX):
        desktop = {
            **expected,
            "files": {
                path.name: sha256(path)
                for path in directory.iterdir()
                if path.name != "evidence.json"
            },
        }
        if json.loads((directory / "evidence.json").read_bytes()) != desktop:
            raise TaskError("the desktop evidence differs from the finalized Core archive bytes")
        target = (
            "aarch64-apple-darwin"
            if directory == release.DESKTOP_MACOS
            else "x86_64-unknown-linux-gnu"
        )
        proof = json.loads((DIST / "desktop-verification" / f"{target}.json").read_bytes())
        name = (
            release.dmg_name(plan["version"])
            if directory == release.DESKTOP_MACOS
            else release.archive_name("desktop", plan["version"], "linux-x86_64")
        )
        native = {
            "schema": 1,
            "kind": "desktop-verification",
            "source_sha": plan["source"],
            "plan_sha256": sha256(release_plan.PLAN),
            "asset_sha256": sha256(directory / name),
            "core_sha256": expected["core"],
        }
        if proof != native:
            raise TaskError(
                "native desktop verification differs from the final signed bytes and plan"
            )
    smoke = json.loads((DIST / "floor-smoke.json").read_bytes())
    if (
        smoke["source"] != plan["source"]
        or smoke["tag"] != plan["tag"]
        or smoke["sha256"] != expected["core"]["x86_64-unknown-linux-gnu"]
    ):
        raise TaskError("the floor smoke did not exercise these exact final Core bytes")


def core_evidence(plan, target):
    return {
        "source": plan["source"],
        "plan_sha256": sha256(release_plan.PLAN),
        "target": target,
        "verified": True,
        "files": {name: sha256(release.CORE / target / name) for name in release.CORE_FILES},
    }


def verify_core():
    """Execute final Core bytes only on a runner that never receives release secrets."""
    require_no_credentials()
    plan = release_plan.load()
    target = host_target()
    with tempfile.TemporaryDirectory(dir=DIST, prefix=".verify-core-") as temporary:
        release.verified_core(
            release.CORE / target, target, plan["version"], plan["floors"]["core"], Path(temporary)
        )
    output = DIST / "core-verification"
    output.mkdir(exist_ok=True)
    (output / f"{target}.json").write_text(json.dumps(core_evidence(plan, target)) + "\n")


def require_no_credentials():
    private = (
        "SPACETERM_RELEASE_UPDATE_ED25519_SEED",
        "SPACETERM_RELEASE_DEVID_P12_BASE64",
        "SPACETERM_RELEASE_DEVID_P12_PASSWORD",
        "SPACETERM_RELEASE_NOTARY_P8",
        "HOMEBREW_TAP_PAT",
    )
    if any(os.environ.get(name) for name in private):
        raise TaskError("executable verification requires a credential-free job")


def verify_desktop():
    """Call the native desktop verifier only after signing, in a separate no-secret job."""
    require_no_credentials()
    from spaceterm_tasks.desktop_manifest import verify_desktop_release

    plan = release_plan.load()
    macos = host_target() == "aarch64-apple-darwin"
    directory = release.DESKTOP_MACOS if macos else release.DESKTOP_LINUX
    name = (
        release.dmg_name(plan["version"])
        if macos
        else release.archive_name("desktop", plan["version"], "linux-x86_64")
    )
    core = {
        target: sha256(release.CORE / target / "artifact.tar.gz")
        for target in ("aarch64-apple-darwin", "x86_64-unknown-linux-gnu")
    }
    proof = verify_desktop_release(
        directory / name,
        plan["tag"],
        macos=macos,
        source_sha=plan["source"],
        plan_sha256=sha256(release_plan.PLAN),
        core_digests=core,
    )
    output = DIST / "desktop-verification"
    output.mkdir(exist_ok=True)
    (output / f"{host_target()}.json").write_text(json.dumps(proof) + "\n")


def require_core_evidence(plan):
    for target in ("aarch64-apple-darwin", "x86_64-unknown-linux-gnu"):
        path = DIST / "core-verification" / f"{target}.json"
        if json.loads(path.read_bytes()) != core_evidence(plan, target):
            raise TaskError("native Core verification differs from the final signed bytes and plan")
