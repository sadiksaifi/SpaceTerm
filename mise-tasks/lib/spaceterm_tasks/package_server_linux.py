"""Linux server packaging. An unbuilt or unproved credential closure blocks releases."""

import hashlib
import json
import os
import re
import shutil
import sys
import tempfile
import tomllib
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError, package_server
from spaceterm_tasks import server_artifacts as artifacts
from spaceterm_tasks.macos_signing import signing_environment
from spaceterm_tasks.packaging import DIST, checked, release_version

PIN = ROOT / "packaging/server/linux/systemd-creds.toml"
CLOSURE = DIST / "server-credential-tools-x86_64"
SOURCE_COMMIT = "84ff4f0dff41c26472cd48c4c4955c2d77f5ac08"
SOURCE_TAG = "cc38479b7ff6e20ab265dda8f01f85e1e83f0058"
SOURCE_SHA256 = "9c6f6666e48f4c4f230bbb08bfa293f323d28a568f798fafba0eeca1a5936f9b"
PATCHES = [
    {
        "name": "0001-spaceterm-private-credential-policy.patch",
        "sha256": "ea0f2897290b784a85dbadf53789bd17e5272704a71bb3b3b7aa13fcffd3af10",
    }
]


RUNTIME_RESULTS = ("jammy_result", "host_key_result", "tpm_result")
RUNTIME_POLICY = {
    "elf_interpreter": "/lib64/ld-linux-x86-64.so.2",
    "credential_tool_runpath": "$ORIGIN/../../lib/credential-tools",
    "library_runpath": "$ORIGIN",
    "openssl_providers": "private-authenticated-closure",
    "openssl_configuration": "private-authenticated-configuration",
    "minimum_glibc": [2, 35],
    "minimum_kernel": [5, 15],
    "minimum_supervisor": [249],
}


def credential_closure_binding(receipt):
    document = {**receipt, "runtime": RUNTIME_POLICY}
    return hashlib.sha256(
        json.dumps(document, sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()


def validate_runtime(receipt, *, fresh=False):
    runtime = receipt["runtime"]
    if (
        not isinstance(runtime, dict)
        or set(runtime)
        != set(RUNTIME_POLICY) | set(RUNTIME_RESULTS) | {"closure_sha256", "evidence"}
        or any(runtime[key] != value for key, value in RUNTIME_POLICY.items())
        or runtime["closure_sha256"] != credential_closure_binding(receipt)
        or not isinstance(runtime["evidence"], dict)
        or not set(runtime["evidence"]) <= {"jammy", "host-key", "tpm"}
    ):
        raise TaskError("credential-tool runtime evidence differs from its exact closure")
    for field, gate in zip(RUNTIME_RESULTS, ("jammy", "host-key", "tpm"), strict=True):
        result = runtime[field]
        record = runtime["evidence"].get(gate)
        if result == "unrun" and record is None:
            continue
        if (
            fresh
            or result != "passed"
            or not isinstance(record, dict)
            or set(record) != {"closure_sha256", "tests_run", "tests_passed", "tests_skipped"}
            or record["closure_sha256"] != runtime["closure_sha256"]
            or any(
                type(record[key]) is not int
                for key in ("tests_run", "tests_passed", "tests_skipped")
            )
            or not 0 < record["tests_run"] <= 4096
            or record["tests_passed"] != record["tests_run"]
            or record["tests_skipped"] != 0
        ):
            raise TaskError("credential-tool runtime evidence differs from its exact closure")


def validate_build_receipt(receipt, pin, image):
    try:
        if (
            not isinstance(receipt, dict)
            or set(receipt) != set(pin)
            or any(
                receipt[field] != pin[field] for field in ("source", "licensing", "schema_version")
            )
            or not isinstance(receipt["build"], dict)
            or receipt["build"].get("complete") is not True
            or receipt["build"].get("options") != pin["build"]["options"]
            or receipt["build"].get("targets") != ["systemd-creds"]
            or receipt["build"].get("build_image") != image
            or receipt["build"].get("build_image_sha256") != image.removeprefix("sha256:")
            or not artifacts.valid_sha256(receipt["build"].get("build_id"))
            or receipt["build"].get("build_id") == pin["build"].get("build_id")
            or not receipt["build"].get("dependencies")
        ):
            raise TaskError("credential-tool build receipt differs from its frozen recipe")
        validate_runtime(receipt, fresh=True)
    except (KeyError, TypeError, ValueError):
        raise TaskError("credential-tool build receipt differs from its frozen recipe") from None


def credential_tool_pin():
    try:
        return _credential_tool_pin()
    except (tomllib.TOMLDecodeError, KeyError, TypeError, ValueError):
        raise TaskError("credential-tool source or build pin is invalid") from None


def _credential_tool_pin():
    pin = tomllib.loads(PIN.read_text())
    source = pin["source"]
    if (
        type(pin["schema_version"]) is not int
        or pin["schema_version"] != 1
        or source["repository"] != "https://github.com/systemd/systemd"
        or source["tag"] != "v260.5"
        or source["annotated_tag"] != SOURCE_TAG
        or source["commit"] != SOURCE_COMMIT
        or source["archive_sha256"] != SOURCE_SHA256
        or source["tagger_date"] != "2026-09-10T23:22:27Z"
        or source["authentication_result"] != "valid"
        or source["authentication"] != "github-verified-exact-annotated-tag"
        or source["archive_url"]
        != f"https://codeload.github.com/systemd/systemd/tar.gz/{SOURCE_COMMIT}"
        or source["license"] != "LGPL-2.1-or-later"
        or source["patches"] != PATCHES
    ):
        raise TaskError("credential tool requires the exact authenticated upstream source pin")
    options = pin["build"]["options"]
    if (
        not isinstance(options, list)
        or any(not isinstance(option, str) for option in options)
        or len({option.partition("=")[0] for option in options}) != len(options)
    ):
        raise TaskError("credential tool build options contain an override or duplicate")
    for option in (
        "-Dauto_features=disabled",
        "-Dopenssl=enabled",
        "-Dtpm2=enabled",
        "-Dlibmount=enabled",
        "-Ddefault_library=shared",
    ):
        if option not in options:
            raise TaskError("credential tool build options omit required support")
    if pin["build"]["targets"] != ["systemd-creds"]:
        raise TaskError("credential tool build must not replace the host service manager")
    return pin


def verify_credential_files(root, pinned_files):
    """Authenticate the copied destination against the retained build receipt."""
    files = set()
    for file in pinned_files:
        if (
            set(file) != {"name", "sha256"}
            or not artifacts.relative_name(file["name"])
            or file["name"] in files
            or not artifacts.valid_sha256(file["sha256"])
        ):
            raise TaskError("invalid credential-tool closure pin")
        files.add(file["name"])
        path = root / file["name"]
        if path.is_symlink() or not path.is_file() or artifacts.digest(path) != file["sha256"]:
            raise TaskError("credential-tool closure differs from its binary pin")
    hashes = {file["name"]: file["sha256"] for file in pinned_files}
    if any(
        hashes.get("share/sources/build-material/" + patch["name"]) != patch["sha256"]
        for patch in PATCHES
    ):
        raise TaskError("credential-tool corresponding hardening patch is missing or unrecorded")
    actual = {p.relative_to(root).as_posix() for p in root.rglob("*") if not p.is_dir()}
    if actual != files or "libexec/credential-tools/systemd-creds" not in files:
        raise TaskError("credential-tool closure is missing or has unexpected files")
    return files


def verify_credential_build(root, build):
    files = verify_credential_files(root, build["files"])
    metadata_name = "share/sources/build-material/build-info.json"
    if metadata_name not in files or not artifacts.valid_sha256(build.get("build_id")):
        raise TaskError("credential-tool authenticated build identity is unavailable")
    try:
        with (root / metadata_name).open("rb") as stream:
            metadata = artifacts.strict_json(
                stream.read(artifacts.MAX_MANIFEST_BYTES + 1), artifacts.MAX_MANIFEST_BYTES
            )
    except OSError:
        raise TaskError("credential-tool authenticated build identity is unavailable") from None
    if not isinstance(metadata, dict) or metadata.get("build_id") != build["build_id"]:
        raise TaskError("credential-tool build identity differs from its authenticated metadata")
    return files


def verify_selected_credential_receipt(root, pin):
    try:
        with root.with_name(root.name + ".json").open("rb") as stream:
            receipt = artifacts.strict_json(
                stream.read(artifacts.MAX_MANIFEST_BYTES + 1), artifacts.MAX_MANIFEST_BYTES
            )
        if not isinstance(receipt, dict) or set(receipt) != set(pin):
            raise TaskError("selected credential-tool build receipt is invalid")
        validate_runtime(receipt)
        # Evidence is recorded in the pin after native execution. The original
        # build receipt may remain unrun, but it must select that same closure.
        if receipt["runtime"]["closure_sha256"] != pin["runtime"]["closure_sha256"]:
            raise TaskError(
                "selected credential-tool build differs from its authenticated evidence"
            )
    except (OSError, KeyError, TypeError, ValueError):
        raise TaskError(
            "selected credential-tool build receipt is unavailable or invalid"
        ) from None


def verify_copied_credential_files(root, pinned_files, notices_sha256):
    """Recheck every admitted closure file immediately before manifest creation."""
    for file in pinned_files:
        expected = (
            notices_sha256
            if file["name"] == "share/spaceterm-server/THIRD-PARTY-NOTICES.txt"
            else file["sha256"]
        )
        path = root / file["name"]
        if path.is_symlink() or not path.is_file() or artifacts.digest(path) != expected:
            raise TaskError("copied credential-tool closure differs from its retained binary pin")


def validate_credential_closure(root=None):
    """Production packaging requires every runtime gate on this exact construction."""
    return _validate_credential_closure(CLOSURE if root is None else root, evidence_candidate=False)


def validate_evidence_credential_closure(root=None):
    """Authenticate a fresh unrun construction, not production admission."""
    return _validate_credential_closure(CLOSURE if root is None else root, evidence_candidate=True)


def _validate_credential_closure(root, *, evidence_candidate):
    pin = credential_tool_pin()
    build = pin["build"]
    runtime = pin["runtime"]
    validate_runtime(pin, fresh=evidence_candidate)
    if (
        build["complete"] is not True
        or any(
            not build[field]
            for field in (
                "compiler",
                "compiler_version",
                "build_image",
                "build_image_sha256",
                "meson_version",
                "ninja_version",
            )
        )
        or (not evidence_candidate and any(runtime[field] != "passed" for field in RUNTIME_RESULTS))
    ):
        raise TaskError("pinned credential-tool closure and Jammy evidence are unavailable")
    if (
        not artifacts.valid_sha256(build["build_image_sha256"])
        or not build["dependencies"]
        or not build["files"]
    ):
        raise TaskError("credential-tool dependency and source material are incomplete")
    verify_selected_credential_receipt(root, pin)
    files = verify_credential_build(root, build)
    dependencies = []
    for dependency in build["dependencies"]:
        required = {
            "name",
            "version",
            "source_identity",
            "source_sha256",
            "build_options",
            "license",
            "notice_file",
            "corresponding_source",
        }
        if (
            set(dependency) != required
            or not artifacts.valid_sha256(dependency["source_sha256"])
            or any(
                dependency[field] not in files for field in ("notice_file", "corresponding_source")
            )
        ):
            raise TaskError("credential-tool dependency license or source material is missing")
        if (
            artifacts.digest(root / dependency["corresponding_source"])
            != dependency["source_sha256"]
        ):
            raise TaskError("credential-tool corresponding source differs from its pin")
        dependencies.append(
            {key: dependency[key] for key in required - {"source_sha256", "build_options"}}
        )
    if not any(
        d["name"] == "systemd"
        and d["source_identity"] == SOURCE_COMMIT
        and d["license"] == "LGPL-2.1-or-later"
        for d in dependencies
    ):
        raise TaskError("credential-tool LGPL corresponding source is missing")
    return pin


def verify_elf(path, private=False):
    header = checked(
        ["readelf", "-hW", path],
        "server ELF header is unavailable",
        text=True,
        env=signing_environment(os.environ),
        timeout=30,
    )
    if "Advanced Micro Devices X86-64" not in header or "ELF64" not in header:
        raise TaskError("server ELF target is unsupported")
    versions = checked(
        ["readelf", "--version-info", "-W", path],
        "server ELF versions are unavailable",
        text=True,
        env=signing_environment(os.environ),
        timeout=30,
    )
    if (
        any(
            tuple(map(int, match.split("."))) > (2, 35)
            for match in re.findall(r"\bGLIBC_([0-9.]+)\b", versions)
        )
        or "GLIBC_PRIVATE" in versions
    ):
        raise TaskError("server ELF requires a newer or private glibc ABI")
    dynamic = checked(
        ["readelf", "-dW", path],
        "server ELF dependency metadata is unavailable",
        text=True,
        env=signing_environment(os.environ),
        timeout=30,
    )
    if "(RPATH)" in dynamic:
        raise TaskError("server ELF contains a legacy loader search path")
    runpaths = re.findall(r"\(RUNPATH\).*\[(.*?)\]", dynamic)
    allowed = "$ORIGIN/../../lib/credential-tools" if path.name == "systemd-creds" else "$ORIGIN"
    if any(value != allowed for value in runpaths) or (private and runpaths != [allowed]):
        raise TaskError("server ELF resolves through an unapproved library search path")
    needed = re.findall(r"\(NEEDED\).*\[(.*?)\]", dynamic)
    if any("/" in name or not name for name in needed):
        raise TaskError("server ELF dependency name is unsafe")
    program = checked(
        ["readelf", "-lW", path],
        "server ELF interpreter metadata is unavailable",
        text=True,
        env=signing_environment(os.environ),
        timeout=30,
    )
    interpreters = re.findall(r"Requesting program interpreter: (.*?)\]", program)
    if interpreters and interpreters != ["/lib64/ld-linux-x86-64.so.2"]:
        raise TaskError("server ELF interpreter is unsupported")
    return needed


def verify_elf_closure(root):
    system = {
        "libc.so.6",
        "libm.so.6",
        "libpthread.so.0",
        "libdl.so.2",
        "librt.so.1",
        "libgcc_s.so.1",
        "ld-linux-x86-64.so.2",
    }
    libraries = root / "lib/credential-tools"
    private_names = set()
    elves = {}
    entry_points = {root / "libexec/credential-tools/systemd-creds"}
    server = root / "bin/spaceterm-server"
    if server.exists():
        entry_points.update(
            {
                server,
                *(
                    root / "libexec" / name
                    for name in (
                        "spaceterm-server-launcher",
                        "spaceterm-server-manager",
                        "spaceterm-server-spawner",
                    )
                ),
            }
        )
    for path in root.rglob("*"):
        if path.is_symlink():
            raise TaskError("server ELF closure contains a linked file")
        if not path.is_file():
            continue
        relative = path.relative_to(root)
        credential = "credential-tools" in relative.parts
        if path.is_relative_to(libraries):
            if path.name in private_names or path.name in system:
                raise TaskError("server ELF closure has ambiguous or overridden dependencies")
            private_names.add(path.name)
        with path.open("rb") as source:
            if source.read(4) == b"\x7fELF":
                if path not in entry_points and path.parent != libraries:
                    raise TaskError("server ELF closure contains unrecorded native code")
                elves[path] = verify_elf(path, private=credential)
    if not entry_points <= elves.keys():
        raise TaskError("server ELF closure is missing a native executable")
    for path, needed in elves.items():
        credential = "credential-tools" in path.relative_to(root).parts
        search = (
            Path(os.path.normpath(path.parent / "../../lib/credential-tools"))
            if path.name == "systemd-creds"
            else path.parent
        )
        for name in needed:
            if re.match(r"lib(?:X11|xcb|wayland|fontconfig|freetype|GL|EGL|vulkan)[.-]", name):
                raise TaskError("server ELF links a graphical dependency")
            if name in system:
                continue
            selected = search / name
            if not credential or not selected.is_relative_to(libraries) or selected not in elves:
                raise TaskError("server ELF dependency does not resolve to an admitted file")


def build_credential_tool():
    """Build comparison facts, never native admission or credential evidence."""
    if sys.platform != "linux":
        raise TaskError("credential-tool construction requires Linux")
    pin = credential_tool_pin()
    driver = ROOT / "packaging/server/linux/credential-tools"
    destination = DIST / "server-credential-tools-x86_64"
    receipt_path = DIST / "server-credential-tools-x86_64.json"
    if destination.exists() or receipt_path.exists():
        raise TaskError("credential-tool build output already exists")
    clean = signing_environment(os.environ)
    DIST.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="credential-tool-build-", dir=DIST) as temporary:
        stage = Path(temporary)
        image_file = stage / "image-id"
        checked(
            [
                "docker",
                "build",
                "--platform",
                "linux/amd64",
                "--file",
                driver / "Jammy.Dockerfile",
                "--iidfile",
                image_file,
                driver,
            ],
            "credential-tool compiler image could not be built",
            cwd=ROOT,
            env=clean,
        )
        image = image_file.read_text().strip()
        if not re.fullmatch(r"sha256:[0-9a-f]{64}", image):
            raise TaskError("credential-tool compiler image identity is invalid")
        # BuildKit's manifest can include a fresh attestation on each export.
        # Record the content-addressed image configuration actually executed.
        image = checked(
            ["docker", "image", "inspect", image, "--format", "{{.Id}}"],
            "credential-tool compiler image could not be retained",
            env=clean,
            text=True,
        ).strip()
        if not re.fullmatch(r"sha256:[0-9a-f]{64}", image):
            raise TaskError("credential-tool compiler image identity is invalid")
        download = stage / "download"
        download.mkdir()
        archive = download / "systemd.tar.gz"
        checked(
            [
                "curl",
                "--fail",
                "--silent",
                "--show-error",
                "--location",
                "--max-time",
                "120",
                pin["source"]["archive_url"],
                "--output",
                archive,
            ],
            "credential-tool frozen source could not be downloaded",
            env=clean,
        )
        if artifacts.digest(archive) != SOURCE_SHA256:
            raise TaskError("credential-tool frozen source digest differs")
        recipe = stage / "recipe.json"
        # The recipe cannot include its own resulting file hashes. Keep the
        # frozen inputs separate from the completed build receipt.
        recipe_document = {
            **pin,
            "build": {
                "options": pin["build"]["options"],
                "targets": pin["build"]["targets"],
            },
        }
        recipe.write_text(json.dumps(recipe_document, sort_keys=True, separators=(",", ":")))
        checked(
            [
                "docker",
                "run",
                "--rm",
                "--platform",
                "linux/amd64",
                "--network=none",
                "--read-only",
                "--cap-drop=ALL",
                "--security-opt=no-new-privileges",
                "--user",
                f"{os.getuid()}:{os.getgid()}",
                "--tmpfs",
                "/tmp:rw,nosuid,nodev,size=256m",
                "--mount",
                f"type=bind,src={driver},dst=/driver,readonly",
                "--mount",
                f"type=bind,src={download},dst=/source,readonly",
                "--mount",
                f"type=bind,src={recipe},dst=/recipe.json,readonly",
                "--mount",
                f"type=bind,src={stage},dst=/output",
                image,
                "python3",
                "-I",
                "/driver/build.py",
                "--source",
                "/source/systemd.tar.gz",
                "--recipe",
                "/recipe.json",
                "--scratch",
                "/output/scratch",
                "--output",
                "/output/closure",
                "--image",
                image,
                "--receipt",
                "/output/receipt.json",
            ],
            "pinned credential-tool construction failed",
            env=clean,
            timeout=1800,
        )
        data = (stage / "receipt.json").read_bytes()
        receipt = artifacts.strict_json(data, artifacts.MAX_MANIFEST_BYTES)
        validate_build_receipt(receipt, pin, image)
        verify_credential_build(stage / "closure", receipt["build"])
        verify_elf_closure(stage / "closure")
        shutil.move(stage / "closure", destination)
        receipt_path.write_bytes(data)
    print(
        "Pinned credential-tool closure built; native Jammy, Host-key, and TPM evidence remain unrun."
    )


def package(release_tag=None):
    if sys.platform != "linux":
        raise TaskError("Linux server packaging requires Linux")
    destination = _package(validate_credential_closure(), release_tag)
    print("Linux server archive and complete licensed closure verified.")
    return destination


def package_evidence_candidate():
    """Build separately named Preflight input. No release feed or authority gate changes."""
    if sys.platform != "linux":
        raise TaskError("Linux evidence-candidate construction requires Linux")
    destination = _package(validate_evidence_credential_closure(), None, evidence_candidate=True)
    print(
        "Authenticated Linux evidence candidate built; production and runtime evidence remain unavailable."
    )
    return destination


def package_publisher(release_tag=None):
    """Build the production APT prerequisite only after closure-bound runtime evidence."""
    if sys.platform != "linux":
        raise TaskError("native publisher construction requires Linux")
    return _package(validate_credential_closure(), release_tag, publisher=True)


def _package(pin, release_tag, *, evidence_candidate=False, publisher=False):
    candidate = DIST / "spaceterm-server-evidence-linux-x86_64.tar.gz"
    publisher_output = DIST / (
        "spaceterm-native-publisher-evidence-linux-x86_64.deb"
        if evidence_candidate
        else f"spaceterm-native-publisher-{'release' if release_tag else 'preflight'}-linux-x86_64.deb"
    )
    if evidence_candidate and any(
        path.exists() or path.is_symlink()
        for path in (candidate, candidate.with_name(candidate.name + ".json"), publisher_output)
    ):
        raise TaskError("native evidence-candidate output already exists")
    dependencies = [
        {
            key: value
            for key, value in dependency.items()
            if key not in ("source_sha256", "build_options")
        }
        for dependency in pin["build"]["dependencies"]
    ]
    binaries = package_server.build(release_tag)
    build = "release" if release_tag else "preflight"
    metadata = package_server.executable_metadata(
        binaries["spaceterm-server"],
        "linux-x86_64",
        build,
        release_version(release_tag) if release_tag else None,
    )
    DIST.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="spaceterm-server-linux-", dir=DIST) as temporary:
        root = Path(temporary) / "spaceterm-server"
        root.mkdir()
        for source in CLOSURE.rglob("*"):
            if source.is_file():
                package_server.copy_file(
                    source, root / source.relative_to(CLOSURE), source.stat().st_mode & 0o777
                )
        verify_credential_files(root, pin["build"]["files"])
        package_server.copy_built_executables(binaries, root, metadata)
        notices = root / "share/spaceterm-server/THIRD-PARTY-NOTICES.txt"
        if notices.stat().st_size > 1024 * 1024:
            raise TaskError("credential-tool notices exceed their bound")
        notices_sha256 = package_server.copy_resources(
            root / "share/spaceterm-server", additional_notices=notices.read_bytes()
        )
        verify_elf_closure(root)
        package_server.verify_executable_metadata(root, metadata)
        verify_copied_credential_files(root, pin["build"]["files"], notices_sha256)
        package_server.write_manifest(root, metadata, dependencies)
        output = Path(temporary) / "server.tar.gz"
        package_server.write_archive(root, output)
        package_server.verify_archive(output)
        if publisher or evidence_candidate:
            from spaceterm_tasks.linux_custody_packaging import build_publisher_package

            build_publisher_package(root, publisher_output)
        destination = (
            candidate if evidence_candidate else package_server.archive_path("linux-x86_64", build)
        )
        if evidence_candidate:
            with output.open("rb") as source, destination.open("xb") as target:
                shutil.copyfileobj(source, target)
        else:
            shutil.copyfile(output, destination)
        if evidence_candidate:
            receipt = {
                "schema_version": 1,
                "purpose": "native-evidence-candidate",
                "build_identity": "preflight",
                "runtime_evidence": "unrun",
                "credential_closure_sha256": pin["runtime"]["closure_sha256"],
                "archive_sha256": artifacts.digest(destination),
                "manifest_sha256": artifacts.digest(root / "installation.json"),
                "publisher_package_sha256": artifacts.digest(publisher_output),
            }
            with destination.with_name(destination.name + ".json").open("x") as stream:
                stream.write(json.dumps(receipt, sort_keys=True, separators=(",", ":")))
    return publisher_output if publisher else destination
