"""Build, sign and publish one SpaceTerm release from an annotated tag at HEAD.

The release workflow runs these steps, each on its own runner:

1. `core`: build SpaceTerm Core once for the host target and sign it once: on macOS Developer
   ID, the hardened runtime, notarization and stapling, then Ed25519 everywhere.
2. `notes`: render the release notes with git-cliff before any desktop build.
3. `desktop_macos` and `desktop_linux`: build each desktop app with both Core builds and the
   notes embedded. macOS signs, notarizes and staples the app and its disk image and writes the
   Sparkle appcast.
4. `assemble`: sign the Linux desktop archive and every feed, add install.sh and SHA256SUMS.
5. `publish`: upload the assembled assets as one GitHub release.

A dry run runs every step with a generated update key and ad hoc code signatures, skips
notarization, and never publishes.
"""

import base64
import json
import os
import plistlib
import re
import shutil
import subprocess
import tempfile
import time
import tomllib
import xml.etree.ElementTree as ET
from pathlib import Path

from spaceterm_tasks import (
    ROOT,
    TaskError,
    macos_codesign,
    release_feed,
    release_floor,
    sparkle,
    update_signing,
)
from spaceterm_tasks.packaging import DIST, STABLE_TAG, checked, release_version
from spaceterm_tasks.release_feed import (
    APPCAST,
    CHECKSUMS,
    INSTALLER,
    archive_name,
    asset_names,
    dmg_name,
    download_url,
    notes_url,
)

SPARKLE_NS = "http://www.andymatuschak.org/xml-namespaces/sparkle"
RELEASES = release_feed.RELEASES
INSTALLER_SOURCE = ROOT / "packaging" / "install.sh"
# The directory each step writes, which the next steps read.
CORE = DIST / "release-core"
NOTES = DIST / "release-notes.md"
DESKTOP_MACOS = DIST / "release-desktop-macos"
DESKTOP_LINUX = DIST / "release-desktop-linux"
RELEASE = DIST / "release"
CORE_FILES = ("artifact.tar.gz", "artifact.sig", "artifact.sha256")


def sha256(path):
    return update_signing.sha256(path)


def checksums(paths):
    return "".join(f"{sha256(path)}  {Path(path).name}\n" for path in paths)


def replace_directory(staged, destination):
    shutil.rmtree(destination, ignore_errors=True)
    destination.parent.mkdir(parents=True, exist_ok=True)
    staged.rename(destination)


def notes(root=ROOT):
    """Markdown notes for the release tagged at HEAD, from its Conventional Commits.

    cliff.toml's tag_pattern decides which tags are releases.
    """
    with open(root / "cliff.toml", "rb") as config:
        release_tag = re.compile(tomllib.load(config)["git"]["tag_pattern"])
    tags = subprocess.run(
        ["git", "tag", "--points-at", "HEAD"], cwd=root, capture_output=True, text=True
    ).stdout.split()
    if not any(release_tag.match(tag) for tag in tags):
        raise TaskError("release notes require a stable release tag from 1.0.0 at HEAD")
    result = subprocess.run(
        ["git-cliff", "--config", "cliff.toml", "--current", "--offline", "--strip", "all"],
        cwd=root,
        capture_output=True,
        text=True,
    )
    if result.returncode or not result.stdout.strip():
        raise TaskError("release notes require a stable release tag at HEAD")
    return result.stdout


def write_notes(output=NOTES, root=ROOT):
    """Write the notes file that the desktop builds embed and the GitHub release shows."""
    text = notes(root)
    if len(text.encode()) > 64 * 1024:
        raise TaskError("the release notes are larger than the 64 KiB a desktop embeds")
    output = Path(output)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(text)
    print(f"Wrote the release notes to {output.name}")
    return output


def dry_run_version(tags):
    """A version newer than every stable tag, from 1.0.0, so a dry run never reuses a tag."""
    versions = [
        tuple(int(part) for part in match.groups())
        for match in (STABLE_TAG.fullmatch(tag) for tag in tags)
        if match
    ]
    newest = max(versions, default=(0, 0, 0))
    if newest < (1, 0, 0):
        return "1.0.0"
    return f"{newest[0]}.{newest[1]}.{newest[2] + 1}"


def dry_run_tag(root=ROOT):
    tags = checked(["git", "tag", "--list"], "Git could not list tags", cwd=root, text=True)
    return f"v{dry_run_version(tags.split())}"


def create_dry_run_tag(root=ROOT):
    """Tag HEAD for a dry run in a disposable checkout, and return the tag."""
    tag = dry_run_tag(root)
    checked(
        [
            "git",
            "-c",
            "user.name=SpaceTerm dry run",
            "-c",
            "user.email=dry-run@spaceterm.invalid",
            "tag",
            "--annotate",
            "--no-sign",
            tag,
            "--message",
            f"Dry run of SpaceTerm {tag[1:]}",
        ],
        "Git could not tag the dry run",
        cwd=root,
    )
    return tag


def core(tag, target=None):
    """Build and sign SpaceTerm Core for `target`, the host by default, into dist/release-core."""
    from spaceterm_tasks.core_package import host_target, package

    target = target or host_target()
    archive, signature = package("spaceterm", tag, target=target)
    with tempfile.TemporaryDirectory(dir=DIST, prefix=".release-core-") as temporary:
        staged = Path(temporary) / target
        staged.mkdir()
        shutil.copyfile(archive, staged / "artifact.tar.gz")
        shutil.copyfile(signature, staged / "artifact.sig")
        (staged / "artifact.sha256").write_text(sha256(archive) + "\n")
        replace_directory(staged, CORE / target)
    print(f"SpaceTerm Core for {target} is signed and ready to embed")


def core_catalog(directory):
    """Require dist/release-core to hold both targets' signed Core builds."""
    from spaceterm_tasks.core_package import TARGETS

    directory = Path(directory)
    for target in TARGETS.values():
        if not all((directory / target / name).is_file() for name in CORE_FILES):
            raise TaskError(f"the release Core builds lack {target}; run release:core on it")
    return directory


def desktop_macos(tag, notes_file=NOTES, core_directory=CORE):
    """Package the macOS desktop with both Core builds, then write its signed appcast."""
    from spaceterm_tasks import package_macos

    os.environ["SPACETERM_CLI_ARTIFACTS"] = str(core_catalog(core_directory).resolve())
    os.environ.pop("SPACETERM_CLI_ARCHIVE", None)
    os.environ.pop("SPACETERM_CLI_SIGNATURE", None)
    package_macos.package(tag, notes_file)
    appcast(tag)


def desktop_linux(tag, notes_file=NOTES, core_directory=CORE):
    """Package the Linux desktop with both Core builds."""
    from spaceterm_tasks import package_linux

    os.environ["SPACETERM_CLI_ARTIFACTS"] = str(core_catalog(core_directory).resolve())
    os.environ.pop("SPACETERM_CLI_ARCHIVE", None)
    os.environ.pop("SPACETERM_CLI_SIGNATURE", None)
    package_linux.package(tag, notes_file)
    version = release_version(tag)
    with tempfile.TemporaryDirectory(dir=DIST, prefix=".release-desktop-") as temporary:
        staged = Path(temporary) / "desktop"
        staged.mkdir()
        shutil.copyfile(
            package_linux.archive_path("spaceterm"),
            staged / archive_name("desktop", version, "linux-x86_64"),
        )
        replace_directory(staged, DESKTOP_LINUX)
    print("The Linux desktop archive is ready to sign")


def sparkle_signing(environment, plist):
    """Sparkle's options for the update key, which must be the key the packaged app trusts.

    A release app pins the current key of update-trust.json, a dry-run app its generated key.
    """
    seed = update_signing.release_seed(environment)
    if plist.get("SUPublicEDKey") != update_signing.trust(environment)[0]:
        raise TaskError("the app's SUPublicEDKey must be the current update key")
    return ["--ed-key-file", "-"], update_signing.encoded_seed(seed).encode()


def verify_appcast(path, archive, version):
    root = ET.parse(path).getroot()
    items = root.findall("./channel/item")
    if len(items) != 1:
        raise ValueError("the appcast must contain exactly one complete update")
    item = items[0]
    if (
        item.findtext(f"{{{SPARKLE_NS}}}version") != version
        or item.findtext(f"{{{SPARKLE_NS}}}shortVersionString") != version
    ):
        raise ValueError("the appcast version must match the Git tag")
    if item.findtext(f"{{{SPARKLE_NS}}}minimumSystemVersion") != "26.0":
        raise ValueError("the appcast minimum system version must be 26.0")
    if item.findtext(f"{{{SPARKLE_NS}}}fullReleaseNotesLink") != notes_url(version):
        raise ValueError("the appcast must link the release notes")
    if item.find(f"{{{SPARKLE_NS}}}channel") is not None:
        raise ValueError("release channels are not supported")
    enclosure = item.find("enclosure")
    if enclosure is None:
        raise ValueError("the release disk image is missing")
    if enclosure.get("url") != download_url(version, archive.name) or enclosure.get(
        "length"
    ) != str(archive.stat().st_size):
        raise ValueError("the release disk image URL or size does not match")
    signature = enclosure.get(f"{{{SPARKLE_NS}}}edSignature", "")
    if len(base64.b64decode(signature, validate=True)) != 64:
        raise ValueError("the release disk image signature is missing")
    return signature


def appcast(tag):
    """Sign the packaged disk image into dist/release-desktop-macos with its Sparkle appcast."""
    version = release_version(tag)
    plist = plistlib.loads((DIST / "SpaceTerm.app/Contents/Info.plist").read_bytes())
    if plist["CFBundleShortVersionString"] != version or plist["CFBundleVersion"] != version:
        raise TaskError("package the selected Git tag before writing its appcast")
    options, stdin = sparkle_signing(os.environ, plist)
    environment = {
        key: value for key, value in os.environ.items() if key != update_signing.SIGNING_SECRET
    }

    def run(*arguments):
        if subprocess.run(arguments, input=stdin, env=environment, capture_output=True).returncode:
            raise TaskError("Sparkle could not sign or verify the appcast")

    with tempfile.TemporaryDirectory(dir=DIST, prefix=".release-appcast-") as temporary:
        directory = Path(temporary) / "desktop"
        directory.mkdir()
        archive = directory / dmg_name(version)
        shutil.copyfile(DIST / "SpaceTerm.dmg", archive)
        run(
            sparkle.tool("generate_appcast"),
            *options,
            "--maximum-deltas",
            "0",
            "--maximum-versions",
            "1",
            "--download-url-prefix",
            f"{RELEASES}/download/v{version}/",
            "--full-release-notes-url",
            notes_url(version),
            "--link",
            notes_url(version),
            directory,
        )
        feed = directory / APPCAST
        try:
            signature = verify_appcast(feed, archive, version)
        except (OSError, ValueError, ET.ParseError) as error:
            raise TaskError(str(error)) from None
        run(sparkle.tool("sign_update"), *options, "--verify", archive, signature)
        run(sparkle.tool("sign_update"), *options, "--verify", feed)
        replace_directory(directory, DESKTOP_MACOS)
    print("The disk image and its signed appcast are verified")


def verified_core(entry, target, version, floor, directory):
    """Authenticate one Core build of `version` with Update Floor `floor` and return its archive
    and signature."""
    from spaceterm_tasks.core_package import document, host_target, verified_copy

    if (entry / "artifact.sha256").read_text() != sha256(entry / "artifact.tar.gz") + "\n":
        raise TaskError(f"the {target} Core build differs from its digest")
    tree, archive, signature = verified_copy(
        entry / "artifact.tar.gz",
        entry / "artifact.sig",
        directory,
        None,
        identity="spaceterm",
        target=target,
        native=target == host_target(),
    )
    info = document((tree / "build.json").read_bytes())
    if info["version"] != version:
        raise TaskError("the Core version must match the release tag")
    if info["floor"] != floor:
        raise TaskError("the Core build's Update Floor differs from the release's")
    return archive, signature


def assemble(
    tag,
    core_directory=CORE,
    macos_directory=DESKTOP_MACOS,
    linux_directory=DESKTOP_LINUX,
    *,
    complete=True,
    environment=os.environ,
):
    """Sign every feed and the Linux desktop archive and write dist/release with SHA256SUMS.

    A release requires every asset. A local dry run, which cannot build both desktops on one
    host, omits the products its host does not build.
    """
    from spaceterm_tasks.core_package import PLATFORMS

    version = release_version(tag)
    if not complete and not update_signing.dry_run(environment):
        raise TaskError("only a dry run may assemble a release without every asset")
    seed = update_signing.release_seed(environment)
    keys = update_signing.trust(environment)
    protocols = release_feed.protocols()
    floors = release_floor.floors(version)
    published_at = int(time.time())
    with tempfile.TemporaryDirectory(dir=DIST, prefix=".release-") as temporary:
        directory = Path(temporary) / "release"
        directory.mkdir()
        scratch = Path(temporary) / "scratch"
        scratch.mkdir()
        archives = []
        for target, platform in PLATFORMS.items():
            entry = Path(core_directory) / target
            if not entry.is_dir() and not complete:
                print(f"Dry run: no {platform} Core build on this host; its assets are skipped.")
                continue
            verified = scratch / target
            verified.mkdir()
            archive, signature = verified_core(entry, target, version, floors["core"], verified)
            name = archive_name("core", version, platform)
            shutil.copyfile(archive, directory / name)
            shutil.copyfile(signature, directory / f"{name}.sig")
            archives.append(("core", platform, directory / name))
        linux = Path(linux_directory) / archive_name("desktop", version, "linux-x86_64")
        if linux.is_file() or complete:
            from spaceterm_tasks import package_linux

            package_linux.verify(linux, tag)
            shutil.copyfile(linux, directory / linux.name)
            update_signing.sign(seed, directory / linux.name, directory / f"{linux.name}.sig")
            archives.append(("desktop", "linux-x86_64", directory / linux.name))
        else:
            print("Dry run: no Linux desktop build on this host; its assets are skipped.")
        for product, platform, archive in archives:
            release_feed.write(
                seed,
                keys,
                directory,
                product=product,
                platform=platform,
                version=version,
                floor=floors[product],
                published_at=published_at,
                protocols=protocols,
                archive=archive,
            )
        dmg = Path(macos_directory) / dmg_name(version)
        if dmg.is_file() or complete:
            for name in (dmg.name, APPCAST):
                shutil.copyfile(Path(macos_directory) / name, directory / name)
            try:
                verify_appcast(directory / APPCAST, directory / dmg.name, version)
            except (OSError, ValueError, ET.ParseError) as error:
                raise TaskError(str(error)) from None
        else:
            print("Dry run: no macOS desktop build on this host; its assets are skipped.")
        installer(directory / INSTALLER, environment)
        listed = sorted(path for path in directory.iterdir())
        (directory / CHECKSUMS).write_text(checksums(listed))
        if complete:
            require_assets(directory, version)
        replace_directory(directory, RELEASE)
    print(f"Assembled {len(listed) + 1} release assets with checksums")


def installer(destination, environment=os.environ, source=INSTALLER_SOURCE, root=ROOT):
    """Write the published install.sh: the release kind and Team ID filled in once each, and
    the update keys it pins equal to packaging/update-trust.json.

    A release fills the Developer ID Team ID and needs real keys. A dry run's installer accepts
    ad hoc code signatures and placeholder keys, so it receives no Team ID.
    """
    text = Path(source).read_text()
    for token in ("@RELEASE_KIND@", "@TEAM_ID@"):
        if text.count(token) != 1:
            raise TaskError(f"packaging/install.sh must hold {token} exactly once")
    pinned = dict(re.findall(r"^readonly UPDATE_KEY_(CURRENT|BACKUP)='([^'\n]*)'$", text, re.M))
    try:
        trust = json.loads((root / update_signing.TRUST).read_text())
    except (OSError, ValueError):
        raise TaskError("packaging/update-trust.json is missing or malformed") from None
    if pinned != {"CURRENT": trust.get("current"), "BACKUP": trust.get("backup")}:
        raise TaskError("packaging/install.sh must pin the keys in packaging/update-trust.json")
    if update_signing.dry_run(environment):
        kind, team = "dry-run", ""
    else:
        update_signing.release_trust(root)
        kind, team = "release", macos_codesign.team_id(environment)
    Path(destination).write_text(text.replace("@RELEASE_KIND@", kind).replace("@TEAM_ID@", team))
    Path(destination).chmod(0o755)


def require_assets(directory, version):
    """Require exactly the release assets, each listed once with its digest in SHA256SUMS."""
    names = asset_names(version)
    if sorted(path.name for path in Path(directory).iterdir()) != sorted(names):
        raise TaskError("the release assets differ from the release asset list")
    listed = [path for path in (Path(directory) / name for name in names) if path.name != CHECKSUMS]
    if (Path(directory) / CHECKSUMS).read_text() != checksums(sorted(listed)):
        raise TaskError("the release assets differ from their checksums")


def publish(tag, notes_file=NOTES, directory=RELEASE, environment=os.environ):
    """Publish the assembled release with the notes the desktops embed; a published release is
    never replaced."""
    version = release_version(tag)
    require_assets(directory, version)
    head = checked(["git", "rev-parse", "HEAD"], "Git could not read HEAD", cwd=ROOT).strip()
    tagged = subprocess.run(
        ["git", "rev-parse", "--verify", "--quiet", f"refs/tags/{tag}^{{commit}}"],
        cwd=ROOT,
        capture_output=True,
    )
    if tagged.returncode or tagged.stdout.strip() != head:
        raise TaskError("check out the release tag before publishing")
    if not Path(notes_file).is_file():
        raise TaskError("publishing needs the release notes from release:notes")
    assets = [Path(directory) / name for name in asset_names(version)]
    if update_signing.dry_run(environment):
        print(f"Dry run: verified {len(assets)} release assets for {tag}; nothing is published.")
        return
    # gh uploads the assets to a draft and publishes it only after every upload succeeds, so the
    # latest/download URLs never point at a partial release.
    if subprocess.run(
        [
            "gh",
            "release",
            "create",
            tag,
            *assets,
            "--verify-tag",
            "--latest",
            "--title",
            f"SpaceTerm {tag}",
            "--notes-file",
            notes_file,
        ],
        cwd=ROOT,
    ).returncode:
        raise TaskError("GitHub could not publish the release")
    print(f"Published SpaceTerm {tag} with {len(assets)} assets.")
