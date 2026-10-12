"""Build the frozen credential target inside the digest-selected Jammy container.

Run with python3 -I from /driver. Downloads and build output live elsewhere.
No host key, TPM, native registration, or service-manager operation runs here.
"""

import argparse
import hashlib
import json
import re
import shutil
import subprocess
import sys
import tarfile
from pathlib import Path

SOURCE_SHA256 = "9c6f6666e48f4c4f230bbb08bfa293f323d28a568f798fafba0eeca1a5936f9b"
SOURCE_COMMIT = "84ff4f0dff41c26472cd48c4c4955c2d77f5ac08"
SYSTEM_LIBRARIES = {
    "libc.so.6",
    "libm.so.6",
    "libpthread.so.0",
    "libdl.so.2",
    "librt.so.1",
    "libgcc_s.so.1",
    "ld-linux-x86-64.so.2",
}
DEPENDENCIES = {
    "openssl": ("3.0.2-0ubuntu1.30", "Apache-2.0"),
    "util-linux": ("2.37.2-4ubuntu3.6", "LGPL-2.1-or-later"),
    "libcap2": ("1:2.44-1ubuntu0.22.04.3", "BSD-3-Clause OR GPL-2.0-only"),
    "tpm2-tss": ("3.2.0-1ubuntu1.1", "BSD-2-Clause"),
    "libselinux": ("3.3-1build2", "public-domain"),
    "pcre2": ("10.39-3ubuntu0.1", "BSD-3-Clause"),
}


PHASE = "initialization"


class BuildFailure(Exception):
    pass


def run(arguments, environment, cwd=Path("/driver"), failure="credential-tool-build-failed"):
    result = subprocess.run(
        arguments,
        env=environment,
        cwd=cwd,
        stdin=subprocess.DEVNULL,
        capture_output=True,
        timeout=900,
    )
    if result.returncode:
        line = re.search(rb":([0-9]+):[0-9]+: ERROR:", result.stdout)
        if line:
            print("build-source-line=" + line.group(1).decode(), file=sys.stderr)
        raise BuildFailure(failure)
    return result.stdout.decode("utf-8", errors="strict").strip()


def digest(path):
    with path.open("rb") as stream:
        value = hashlib.sha256()
        while data := stream.read(65536):
            value.update(data)
        return value.hexdigest()


def copy(source, destination):
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, destination)
    destination.chmod(0o644)


def source_material(source_root, package, destination):
    source = source_root / package
    paths = sorted(source.iterdir())
    if not paths or any(not path.is_file() or path.is_symlink() for path in paths):
        raise BuildFailure("credential-tool-source-unavailable")
    components = [{"name": path.name, "sha256": digest(path)} for path in paths]
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tarfile.open(destination, "w", format=tarfile.GNU_FORMAT) as archive:
        for path in paths:
            info = tarfile.TarInfo(path.name)
            info.size = path.stat().st_size
            info.mode = 0o644
            info.uid = info.gid = info.mtime = 0
            with path.open("rb") as stream:
                archive.addfile(info, stream)
    return components


def library_package(name):
    for prefix, package in (
        ("libssl", "openssl"),
        ("libcrypto", "openssl"),
        ("libmount", "util-linux"),
        ("libblkid", "util-linux"),
        ("libcap", "libcap2"),
        ("libtss2-", "tpm2-tss"),
        ("libselinux", "libselinux"),
        ("libpcre2-", "pcre2"),
    ):
        if name.startswith(prefix):
            return package
    raise BuildFailure("credential-tool-dependency-unsupported")


def elf_dependencies(path, environment):
    data = run(["readelf", "-dW", path], environment)
    return re.findall(r"\(NEEDED\).*\[(.*?)\]", data)


def build(arguments):
    global PHASE
    scratch = Path(arguments.scratch)
    scratch.mkdir()
    environment = {
        "PATH": "/usr/local/bin:/usr/bin:/bin",
        "LC_ALL": "C",
        "HOME": str(scratch / "home"),
        "SOURCE_DATE_EPOCH": "1789082547",
    }
    if run(["getconf", "GNU_LIBC_VERSION"], environment) != "glibc 2.35":
        raise BuildFailure("credential-tool-build-baseline-unsupported")
    PHASE = "source-read"
    archive = Path(arguments.source)
    if digest(archive) != SOURCE_SHA256:
        raise BuildFailure("credential-tool-source-rejected")
    recipe = json.loads(Path(arguments.recipe).read_text())
    source = scratch / "source"
    source.mkdir()
    PHASE = "source-extraction"
    with tarfile.open(archive) as downloaded:
        # Preserve source-internal links, but omit absolute/escaping test-fixture
        # links. Tests are disabled. The shipped source archive stays unchanged.
        def source_filter(member, destination):
            try:
                return tarfile.data_filter(member, destination)
            except tarfile.FilterError:
                if member.issym() or member.islnk():
                    return None
                raise

        downloaded.extractall(source, filter=source_filter)
    source = source / f"systemd-{SOURCE_COMMIT}"
    build_root = scratch / "build"
    PHASE = "meson-setup"
    run(
        [
            "python3",
            "-I",
            "/usr/local/bin/meson",
            "setup",
            build_root,
            source,
            *recipe["build"]["options"],
        ],
        environment,
        failure="credential-tool-meson-setup-failed",
    )
    PHASE = "ninja"
    run(
        ["ninja", "-C", build_root, "-j3", "systemd-creds"],
        environment,
        failure="credential-tool-ninja-failed",
    )
    config = (build_root / "config.h").read_text()
    if any(
        f"#define {feature} 1" not in config
        for feature in ("HAVE_OPENSSL", "HAVE_TPM2", "HAVE_LIBMOUNT")
    ):
        raise BuildFailure("credential-tool-required-feature-unavailable")
    PHASE = "closure-copy"
    root = Path(arguments.output)
    root.mkdir()
    tool = root / "libexec/credential-tools/systemd-creds"
    copy(build_root / "systemd-creds", tool)
    libraries = root / "lib/credential-tools"
    shared = build_root / "src/shared/libsystemd-shared-spaceterm-260.5.so"
    copy(shared, libraries / shared.name)
    pending = [tool, libraries / shared.name]
    # These libraries are dlopen targets, not necessarily DT_NEEDED entries.
    names = {
        "libtss2-esys.so.0",
        "libtss2-rc.so.0",
        "libtss2-mu.so.0",
        "libtss2-tcti-device.so.0",
        "libmount.so.1",
        "libcap.so.2",
    }
    packages = {}
    PHASE = "dependency-metadata"
    while pending or names:
        while pending:
            names.update(elf_dependencies(pending.pop(), environment))
        for name in sorted(names):
            if name in SYSTEM_LIBRARIES or (libraries / name).exists():
                continue
            package = library_package(name)
            candidate = Path("/usr/lib/x86_64-linux-gnu") / name
            if not candidate.is_file():
                raise BuildFailure("credential-tool-dependency-unavailable")
            owners = run(["dpkg-query", "-S", "*/" + name], environment).splitlines()
            package_names = {owner.split(": ", 1)[0] for owner in owners}
            if len(package_names) != 1:
                raise BuildFailure("credential-tool-dependency-owner-ambiguous")
            package_name = package_names.pop()
            installed_source = run(
                ["dpkg-query", "-W", "-f=${source:Package}\t${source:Version}", package_name],
                environment,
            ).split("\t")
            if installed_source != [package, DEPENDENCIES[package][0]]:
                raise BuildFailure("credential-tool-dependency-pin-mismatch")
            packages.setdefault(package, set()).add(package_name.split(":")[0])
            destination = libraries / name
            copy(candidate, destination)
            pending.append(destination)
        names.clear()
    PHASE = "loader-normalization"
    for path in [tool, *sorted(libraries.iterdir())]:
        run(
            [
                "patchelf",
                "--set-rpath",
                "$ORIGIN/../../lib/credential-tools" if path == tool else "$ORIGIN",
                path,
            ],
            environment,
        )
        path.chmod(0o555)
        versions = run(["readelf", "--version-info", "-W", path], environment)
        if "GLIBC_PRIVATE" in versions or any(
            tuple(map(int, value.split("."))) > (2, 35)
            for value in re.findall(r"\bGLIBC_([0-9.]+)\b", versions)
        ):
            raise BuildFailure("credential-tool-abi-rejected")
    # The default provider is built into the authenticated libcrypto.so.3. No
    # host provider module or host openssl.cnf belongs to the admitted closure.
    configuration = root / "share/spaceterm-server/credential-tools/openssl.cnf"
    configuration.parent.mkdir(parents=True)
    configuration.write_text(
        "openssl_conf = spaceterm_init\n[spaceterm_init]\nproviders = spaceterm_providers\n[spaceterm_providers]\ndefault = spaceterm_default\n[spaceterm_default]\nactivate = 1\n"
    )
    PHASE = "source-and-license-material"
    dependencies = []
    notices = []
    components = {}
    systemd_source = root / "share/sources/systemd.tar.gz"
    copy(archive, systemd_source)
    for package, (version, license_name) in [
        ("systemd", ("260.5", "LGPL-2.1-or-later")),
        *[(package, DEPENDENCIES[package]) for package in sorted(packages)],
    ]:
        license_path = root / f"share/licenses/{package}.txt"
        if package == "systemd":
            source_path = systemd_source
            copy(Path("/driver/LGPL-2.1-or-later.txt"), license_path)
            notice = Path("/driver/THIRD-PARTY-NOTICES.txt").read_text()
            source_identity = SOURCE_COMMIT
            options = recipe["build"]["options"]
        else:
            source_path = root / f"share/sources/{package}.tar"
            components[package] = source_material(
                Path("/opt/credential-sources"), package, source_path
            )
            copyright_files = [
                Path("/usr/share/doc") / binary / "copyright"
                for binary in sorted(packages[package])
            ]
            notice = "\n\n".join(path.read_text() for path in copyright_files)
            license_path.parent.mkdir(parents=True, exist_ok=True)
            license_path.write_text(notice)
            source_identity = f"ubuntu-jammy:{package}={version}"
            options = [
                "Ubuntu signed archive binary; exact source package and debian/rules included",
                "patchelf --set-rpath $ORIGIN",
            ]
        notices.append(f"{package} {version}\n{notice}\n")
        dependencies.append(
            {
                "name": package,
                "version": version,
                "source_identity": source_identity,
                "source_sha256": digest(source_path),
                "build_options": options,
                "license": license_name,
                "notice_file": f"share/licenses/{package}.txt",
                "corresponding_source": source_path.relative_to(root).as_posix(),
            }
        )
    notice_path = root / "share/spaceterm-server/THIRD-PARTY-NOTICES.txt"
    notice_path.write_text("\n\n".join(notices))
    materials = root / "share/sources/build-material"
    for name in ("LGPL-2.0-or-later.txt", "UPSTREAM-LICENSE-NOTES.txt"):
        copy(Path("/driver") / name, root / "share/licenses" / name)
    for name in ("build.py", "Jammy.Dockerfile", "RELINKING.txt"):
        copy(Path("/driver") / name, materials / name)
    copy(Path(arguments.recipe), materials / "recipe.json")
    copy(Path("/opt/credential-sources/packages.tsv"), materials / "packages.tsv")
    (materials / "source-components.json").write_text(
        json.dumps(components, sort_keys=True, separators=(",", ":"))
    )
    effective_options = run(
        ["python3", "-I", "/usr/local/bin/meson", "introspect", "--buildoptions", build_root],
        environment,
    )
    (materials / "meson-options.json").write_text(effective_options)
    PHASE = "metadata-execution"
    reported_version = run(
        [tool, "--version"],
        {**environment, "OPENSSL_CONF": str(configuration), "OPENSSL_MODULES": str(libraries)},
    )
    if (
        not reported_version.startswith("systemd 260")
        or "+OPENSSL" not in reported_version
        or "+TPM2" not in reported_version
    ):
        raise BuildFailure("credential-tool-version-rejected")
    build_info = {
        **recipe["build"],
        "complete": True,
        "compiler": "gcc",
        "compiler_version": run(["gcc", "-dumpfullversion"], environment),
        "build_image": arguments.image,
        "build_image_sha256": arguments.image.removeprefix("sha256:"),
        "meson_version": run(["python3", "-I", "/usr/local/bin/meson", "--version"], environment),
        "ninja_version": run(["ninja", "--version"], environment),
        "dependencies": dependencies,
    }
    (materials / "build-info.json").write_text(
        json.dumps(
            {**build_info, "source_date_epoch": 1789082547, "patchelf_version": "0.14.3"},
            sort_keys=True,
            separators=(",", ":"),
        )
    )
    files = [
        {"name": path.relative_to(root).as_posix(), "sha256": digest(path)}
        for path in sorted(root.rglob("*"))
        if path.is_file()
    ]
    receipt = {
        "schema_version": 1,
        "source": recipe["source"],
        "build": {**build_info, "files": files},
        # Containers do not establish the kernel/supervisor, Host key, or TPM gates.
        "runtime": recipe["runtime"],
        "licensing": recipe["licensing"],
    }
    Path(arguments.receipt).write_text(json.dumps(receipt, sort_keys=True, separators=(",", ":")))
    print("credential-tool-built; native-baseline-and-credential-evidence-unrun")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for operand in ("source", "recipe", "scratch", "output", "image", "receipt"):
        parser.add_argument("--" + operand, required=True)
    try:
        build(parser.parse_args())
    except BuildFailure as failure:
        print(str(failure), file=sys.stderr)
        sys.exit(1)
    except (OSError, ValueError, subprocess.SubprocessError, tarfile.TarError):
        print("credential-tool-" + PHASE + "-unavailable", file=sys.stderr)
        sys.exit(1)
