"""The judged E4 candidate format. These are authenticated inputs, not custody."""

import hashlib
import importlib.util
import os
import plistlib
import secrets
import stat
import sys
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError, main, package_server, package_server_macos
from spaceterm_tasks import server_artifacts as artifacts
from spaceterm_tasks.cargo import build_executable
from spaceterm_tasks.macos_signing import release_keychain
from spaceterm_tasks.packaging import NOTICES, SHELL_INTEGRATION, TERMINFO
from spaceterm_tasks.server_native_macos import (
    CERTIFICATE,
    IDENTIFIER,
    NativeCodeVerifier,
    RetainedArtifacts,
    _source_names,
    _tool,
    canonical_json,
    native_environment,
    read_regular,
    requirement,
)

MANIFEST = "candidate.json"
DEFAULT_UNSIGNED = ROOT / "dist/native-evidence-unsigned"
DEFAULT_SIGNED = ROOT / "dist/native-evidence-signed"
DEFAULT_PREPARED = ROOT / "dist/native-evidence-prepared"
MAX_FILES = 256


def build_environment(environment):
    # Allow build selectors, never account credentials, agents, loader injection,
    # arbitrary compiler wrappers or inherited flags. Signing sees its inputs only
    # inside the existing serialized helper, not in any build or inspection child.
    clean = {
        key: value
        for key, value in environment.items()
        if key
        in {
            "PATH",
            "HOME",
            "CARGO_HOME",
            "RUSTUP_HOME",
            "CARGO_TARGET_DIR",
            "SDKROOT",
            "DEVELOPER_DIR",
            "MACOSX_DEPLOYMENT_TARGET",
            "TMPDIR",
        }
    }
    clean.update(CARGO_BUILD_JOBS="3", SPACETERM_PACKAGED="1", MACOSX_DEPLOYMENT_TARGET="26.0")
    return clean


def task_module(name):
    path = ROOT / f"mise-tasks/server/native/macos/{name}.py"
    specification = importlib.util.spec_from_file_location(f"e4_{name}", path)
    if specification is None or specification.loader is None:
        raise TaskError("native-unavailable")
    module = importlib.util.module_from_spec(specification)
    specification.loader.exec_module(module)
    return module


def build_probe(destination, *, environment):
    task_module("capability_tests").build_fixture(destination, environment=environment)


def build_targets(destination, *, environment):
    module = task_module("companion_tests")
    executable = destination / "target-executable"
    module.build_app(executable, environment)
    app = module.assemble_bundle(destination / "bundle", executable)
    target = destination / app.name
    app.rename(target)
    executable.unlink()
    (destination / "bundle").rmdir()
    return target


def new_directory(path):
    if not path.is_absolute():
        raise TaskError("invalid-record")
    if path.exists() or path.is_symlink():
        raise TaskError("unsafe-native-state")
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    path.mkdir(mode=0o700)
    return path


def code_targets(metadata):
    identifier = metadata["native_identifier"]
    app = f"server/{artifacts.bundle_name(metadata['build_identity'])}"
    return [
        {
            "name": app,
            "executable": f"{app}/Contents/MacOS/spaceterm-server",
            "identifier": identifier,
        },
        *(
            {
                "name": f"server/libexec/{name}",
                "executable": f"server/libexec/{name}",
                "identifier": f"{identifier}.{suffix}",
            }
            for name, suffix in (
                ("spaceterm-server-launcher", "launcher"),
                ("spaceterm-server-manager", "manager"),
                ("spaceterm-server-spawner", "spawner"),
            )
        ),
        {
            "name": "evidence-client",
            "executable": "evidence-client",
            "identifier": f"{IDENTIFIER}.client",
        },
        {
            "name": "capability-probe",
            "executable": "capability-probe",
            "identifier": f"{IDENTIFIER}.probe",
        },
        {
            "name": "targets/SpaceTerm Evidence Companion.app",
            "executable": "targets/SpaceTerm Evidence Companion.app/Contents/MacOS/spaceterm-evidence-companion",
            "identifier": f"{IDENTIFIER}.companion",
        },
    ]


def finalize_modes(root, metadata):
    executables = {entry["executable"] for entry in code_targets(metadata)}
    for path in sorted(root.rglob("*")):
        info = path.lstat()
        if stat.S_ISDIR(info.st_mode):
            path.chmod(0o755)
        elif stat.S_ISREG(info.st_mode) and info.st_nlink == 1:
            path.chmod(0o555 if path.relative_to(root).as_posix() in executables else 0o444)
        else:
            raise TaskError("artifact-rejected")


def write_candidate(root, metadata, run_id, dirty):
    code = code_targets(metadata)
    executables = {entry["executable"] for entry in code}
    files = []
    total = 0
    for path in sorted(root.rglob("*")):
        info = path.lstat()
        if stat.S_ISDIR(info.st_mode):
            continue
        name = path.relative_to(root).as_posix()
        if name == MANIFEST:
            continue
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1 or info.st_size > 64 * 1024 * 1024:
            raise TaskError("artifact-rejected")
        mode = stat.S_IMODE(info.st_mode)
        if mode != (0o555 if name in executables else 0o444):
            raise TaskError("artifact-rejected")
        total += info.st_size
        files.append(
            {
                "name": name,
                "size": info.st_size,
                "mode": mode,
                "sha256": hashlib.sha256(read_regular(root, name, 64 * 1024 * 1024)).hexdigest(),
            }
        )
    if len(files) > MAX_FILES or total > artifacts.MAX_ARCHIVE_BYTES:
        raise TaskError("limit-exceeded")
    document = {
        "schema": 2,
        "scope": "native-evidence-520",
        "run_id": run_id,
        "revision": metadata["build_commit"],
        "dirty": dirty,
        "metadata": metadata,
        "files": files,
        "code": code,
    }
    # Signing changes the copied bytes. Replace only this account-owned staging
    # manifest, never an approved installation record or source candidate.
    (root / MANIFEST).unlink(missing_ok=True)
    with (root / MANIFEST).open("xb") as output:
        output.write(canonical_json(document))
    (root / MANIFEST).chmod(0o444)
    return document


def read_candidate(root):
    data = read_regular(root, MANIFEST, artifacts.MAX_MANIFEST_BYTES)
    document = artifacts.strict_json(data, artifacts.MAX_MANIFEST_BYTES)
    if (
        not isinstance(document, dict)
        or set(document)
        != {
            "schema",
            "scope",
            "run_id",
            "revision",
            "dirty",
            "metadata",
            "files",
            "code",
        }
        or document["schema"] != 2
        or type(document["schema"]) is not int
        or document["scope"] != "native-evidence-520"
    ):
        raise TaskError("invalid-record")
    from spaceterm_tasks.native_evidence_schema import boolean, hexadecimal

    hexadecimal(document["run_id"], 32)
    hexadecimal(document["revision"], 40)
    boolean(document["dirty"])
    metadata = artifacts.validate_metadata(document["metadata"], "darwin-arm64", "preflight")
    if (
        document["revision"] != metadata["build_commit"]
        or document["code"] != code_targets(metadata)
        or canonical_json(document) != data
        or type(document["files"]) is not list
        or not 7 <= len(document["files"]) <= MAX_FILES
    ):
        raise TaskError("artifact-rejected")
    names = []
    total = 0
    executables = {entry["executable"] for entry in document["code"]}
    for entry in document["files"]:
        if not isinstance(entry, dict) or set(entry) != {"name", "size", "mode", "sha256"}:
            raise TaskError("artifact-rejected")
        name = entry["name"]
        if (
            not artifacts.relative_name(name)
            or name == MANIFEST
            or type(entry["size"]) is not int
            or not 0 <= entry["size"] <= 64 * 1024 * 1024
            or type(entry["mode"]) is not int
            or entry["mode"] != (0o555 if name in executables else 0o444)
            or not isinstance(entry["sha256"], str)
            or not artifacts.valid_sha256(entry["sha256"])
        ):
            raise TaskError("artifact-rejected")
        content = read_regular(root, name, 64 * 1024 * 1024)
        if (
            len(content) != entry["size"]
            or hashlib.sha256(content).hexdigest() != entry["sha256"]
            or stat.S_IMODE((root / name).lstat().st_mode) != entry["mode"]
        ):
            raise TaskError("artifact-rejected")
        names.append(name)
        total += entry["size"]
    if (
        names != sorted(set(names))
        or not executables <= set(names)
        or total > artifacts.MAX_ARCHIVE_BYTES
        or _source_names(root, set(names) | {MANIFEST}) != set(names) | {MANIFEST}
    ):
        raise TaskError("artifact-rejected")
    return document


def build_candidate(
    destination,
    *,
    build_binary=build_executable,
    metadata=package_server.executable_metadata,
    build_probe=build_probe,
    build_targets=build_targets,
):
    root = new_directory(destination)
    environment = build_environment(os.environ)
    binaries = {}
    for binary, package in (
        ("spaceterm-server", "spaceterm-server"),
        ("spaceterm-server-launcher", "spaceterm-host"),
        ("spaceterm-server-manager", "spaceterm-host"),
        ("spaceterm-server-spawner", "spaceterm-host"),
        ("spaceterm-native-evidence-client", "spaceterm-host"),
    ):
        extra = (
            ("--features", "native-tests") if binary == "spaceterm-native-evidence-client" else ()
        )
        binaries[binary] = build_binary(
            "--release", "--locked", "-p", package, *extra, binary=binary, env=environment
        )
    observed = metadata(binaries["spaceterm-server"], "darwin-arm64", "preflight")
    artifacts.validate_metadata(observed, "darwin-arm64", "preflight")
    app = root / "server" / artifacts.bundle_name("preflight")
    (app / "Contents").mkdir(parents=True)
    info = plistlib.loads(package_server_macos.TEMPLATE.read_bytes())
    replacements = {
        "@SERVER_IDENTIFIER@": observed["native_identifier"],
        "@SERVER_NAME@": app.stem,
        "@BUNDLE_VERSION@": "0.0.0",
    }
    for key, value in info.items():
        if isinstance(value, str):
            for source, target in replacements.items():
                value = value.replace(source, target)
            info[key] = value
    (app / "Contents/Info.plist").write_bytes(plistlib.dumps(info))
    package_server.copy_file(
        binaries["spaceterm-server"], app / "Contents/MacOS/spaceterm-server", 0o555
    )
    resources = app / "Contents/Resources"
    package_server.copy_file(NOTICES, resources / NOTICES.name)
    for source in SHELL_INTEGRATION.rglob("*"):
        if source.is_symlink():
            raise TaskError("artifact-rejected")
        if source.is_file():
            package_server.copy_file(
                source, resources / "shell-integration" / source.relative_to(SHELL_INTEGRATION)
            )
    (resources / "terminfo").mkdir()
    # Only the resource compiler is external. It receives no build/account inputs.
    _tool(["/usr/bin/tic", "-x", "-o", str(resources / "terminfo"), str(TERMINFO)])
    for name in (
        "spaceterm-server-launcher",
        "spaceterm-server-manager",
        "spaceterm-server-spawner",
    ):
        package_server.copy_file(binaries[name], root / "server/libexec" / name, 0o555)
    package_server.copy_file(
        binaries["spaceterm-native-evidence-client"], root / "evidence-client", 0o555
    )
    build_probe(root / "capability-probe", environment=environment)
    (root / "targets").mkdir()
    build_targets(root / "targets", environment=environment)
    finalize_modes(root, observed)
    write_candidate(root, observed, secrets.token_hex(16), observed["version"].endswith(".dirty"))
    read_candidate(root)
    return root


def copy_candidate(source, destination):
    document = read_candidate(source)
    root = new_directory(destination)
    expected = {entry["name"] for entry in document["files"]} | {MANIFEST}
    with RetainedArtifacts(source, expected) as retained:
        for name in sorted(expected):
            data = read_regular(
                source, name, artifacts.MAX_MANIFEST_BYTES if name == MANIFEST else 64 * 1024 * 1024
            )
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            with path.open("xb") as output:
                output.write(data)
            path.chmod(
                0o444
                if name == MANIFEST
                else next(entry["mode"] for entry in document["files"] if entry["name"] == name)
            )
            retained.check()
    read_candidate(root)
    return root


def verify_candidate(root, *, verifier=None):
    document = read_candidate(root)
    names = {entry["name"] for entry in document["files"]} | {MANIFEST}
    verifier = verifier or NativeCodeVerifier()
    with RetainedArtifacts(root, names) as retained:
        for entry in document["code"]:
            retained.check()
            verifier.verify(root / entry["executable"], entry["identifier"])
            retained.check()
            if entry["name"] != entry["executable"]:
                _tool(
                    [
                        "/usr/bin/codesign",
                        "--verify",
                        "--strict",
                        "--all-architectures",
                        "-R",
                        requirement(entry["identifier"]),
                        str(root / entry["name"]),
                    ]
                )
            retained.check()
        # The shared closure/metadata inspectors read os.environ internally.
        # Isolate them rather than temporarily mutating a signing parent's state.
        _tool(
            [
                sys.executable,
                "-I",
                str(Path(__file__).with_name("inspect_candidate.py")),
                str(root),
            ],
            timeout=180,
        )
        package_server_macos.verify_info_plist(
            root / "server" / artifacts.bundle_name("preflight"), document["metadata"]
        )
        # Exclude unlisted Mach-O resources throughout the target and package trees.
        from spaceterm_tasks.macos_signing import MACH_O_MAGIC

        executables = {entry["executable"] for entry in document["code"]}
        for entry in document["files"]:
            if (
                read_regular(root, entry["name"], 64 * 1024 * 1024)[:4] in MACH_O_MAGIC
                and entry["name"] not in executables
            ):
                raise TaskError("artifact-rejected")
        retained.check()
    return document


def require_update_pair(candidate, update):
    before = read_candidate(candidate)
    after = read_candidate(update)
    server = code_targets(before["metadata"])[0]["executable"]
    original = next(entry["sha256"] for entry in before["files"] if entry["name"] == server)
    replacement = next(entry["sha256"] for entry in after["files"] if entry["name"] == server)
    if (
        before["metadata"]["native_identifier"] != after["metadata"]["native_identifier"]
        or before["metadata"]["version"] == after["metadata"]["version"]
        or original == replacement
    ):
        raise TaskError("artifact-rejected")
    return before, after


def sign_pair(candidate, update, destination):
    require_update_pair(candidate, update)
    root = new_directory(destination)
    # Never merge into an existing signed candidate. The helper owns signing
    # imports and search-list restoration; compiler children have already exited.
    for name, source in (("baseline", candidate), ("update", update)):
        copy_candidate(source, root / name)
    signing_inputs = {
        **native_environment(),
        **{
            key: os.environ[key]
            for key in ("MACOS_SIGNING_CERTIFICATE_P12", "MACOS_SIGNING_CERTIFICATE_PASSWORD")
            if key in os.environ
        },
    }
    with release_keychain(CERTIFICATE, signing_inputs):
        for name in ("baseline", "update"):
            document = read_candidate(root / name)
            for entry in document["code"]:
                arguments = [
                    "/usr/bin/codesign",
                    "--force",
                    "--sign",
                    CERTIFICATE,
                    "--identifier",
                    entry["identifier"],
                    "--requirements",
                    f"=designated => {requirement(entry['identifier'])[1:]}",
                    "--options",
                    "runtime",
                    "--timestamp=none",
                ]
                if entry["name"] != entry["executable"]:
                    _tool([*arguments, str(root / name / entry["executable"])])
                _tool([*arguments, str(root / name / entry["name"])])
            # Signing adds resources with writable modes. Freeze their modes
            # before the installation manifest records them; candidate writing
            # only inventories these final files and never chmods them.
            finalize_modes(root / name, document["metadata"])
            package_server.write_manifest(root / name / "server", document["metadata"])
            (root / name / "server/installation.json").chmod(0o444)
            write_candidate(
                root / name, document["metadata"], document["run_id"], document["dirty"]
            )
            verify_candidate(root / name)
            archive = root / f"{name}.tar.gz"
            package_server.write_archive(root / name / "server", archive)
            package_server.verify_archive(archive)
    require_update_pair(root / "baseline", root / "update")
    return root


def task_main(entry):
    """Keep task failures in the shared closed native classification vocabulary."""
    from spaceterm_tasks.native_evidence_schema import FAILURES

    def guarded():
        try:
            return entry()
        except TaskError as error:
            classification = str(error) if str(error) in FAILURES else "native-unavailable"
            raise TaskError(classification) from None
        except OSError:
            raise TaskError("io-unavailable") from None
        except (ValueError, TypeError, KeyError):
            raise TaskError("invalid-record") from None

    main(guarded)
