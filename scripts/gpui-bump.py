#!/usr/bin/env python3
"""Bump SpaceTerm's pinned Zed fork tag and matching Rust toolchain."""

import argparse
from contextlib import contextmanager
from dataclasses import dataclass
from enum import Enum
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import tomllib
from urllib.error import URLError
from urllib.parse import quote
from urllib.request import urlopen


FORK_URL = "https://github.com/sadiksaifi/zed"
ROOT = Path(__file__).resolve().parent.parent
EXPECTED = {
    ("dependencies", "gpui"),
    ("dependencies", "gpui_platform"),
    ("dev-dependencies", "gpui"),
    ("dev-dependencies", "gpui_macos"),
}
TAG_PATTERN = re.compile(r"spaceterm-[0-9]{4}-[0-9]{2}-[0-9]{2}(?:\.[1-9][0-9]*)?\Z")
FIELD_PATTERN = re.compile(r'(\btag\s*=\s*")([^"]*)(")')
CHANNEL_PATTERN = re.compile(r'(?m)^([ \t]*channel[ \t]*=[ \t]*")([^"]*)("[^\n]*)$')
BUMP_FILES = ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml")


class Failure(Enum):
    DIRTY_INPUTS = "commit or stash changes to Cargo.toml, Cargo.lock, and rust-toolchain.toml before bumping"
    GIT_CHECK_FAILED = "could not check GPUI bump files in Git"
    INVALID_TAG = "expected spaceterm-YYYY-MM-DD or spaceterm-YYYY-MM-DD.N (N is positive without leading zeros)"
    OVERRIDE_ACTIVE = "local GPUI override is active; run mise run gpui:local:off"
    CONFIG_INVALID = "local Cargo configuration is invalid"
    DEPENDENCIES_UNEXPECTED = "fork dependency set is unexpected"
    TAGS_INCONSISTENT = "fork tags in Cargo.toml are inconsistent"
    LOCK_INVALID = "fork entries in Cargo.lock are missing or inconsistent"
    TOOLCHAIN_INVALID = "Rust toolchain file has no channel"
    REMOTE_FAILED = "could not query the Zed fork remote"
    TAG_MISSING = "tag does not exist on the Zed fork remote"
    TOOLCHAIN_FETCH_FAILED = "could not fetch the fork Rust toolchain file"
    UPDATE_FAILED = "cargo update failed for the fork packages"
    FILE_READ_FAILED = "could not read a GPUI bump input file"
    FILE_WRITE_FAILED = "could not write a GPUI bump file"
    RESTORE_FAILED = "rollback failed; run git restore Cargo.toml Cargo.lock rust-toolchain.toml"


class BumpError(Exception):
    def __init__(self, kind: Failure):
        self.kind = kind
        super().__init__(kind.value)


class BumpCancelled(BaseException):
    """A termination request received during a bump."""


@dataclass(frozen=True)
class BumpResult:
    old_tag: str
    new_tag: str
    old_channel: str
    new_channel: str


def cancel_on_sigterm(signum, frame) -> None:
    raise BumpCancelled


@contextmanager
def sigterm_cancellation():
    previous = signal.getsignal(signal.SIGTERM)
    signal.signal(signal.SIGTERM, cancel_on_sigterm)
    try:
        yield
    finally:
        signal.signal(signal.SIGTERM, previous)


class ForkRemote:
    def has_tag(self, tag: str) -> bool:
        try:
            result = subprocess.run(
                ["git", "ls-remote", "--tags", FORK_URL, tag],
                check=True,
                capture_output=True,
                text=True,
            )
        except (OSError, subprocess.CalledProcessError) as error:
            raise BumpError(Failure.REMOTE_FAILED) from error
        return any(line.endswith(f"\trefs/tags/{tag}") for line in result.stdout.splitlines())

    def toolchain(self, tag: str) -> str:
        url = f"https://raw.githubusercontent.com/sadiksaifi/zed/{quote(tag, safe='')}/rust-toolchain.toml"
        try:
            with urlopen(url, timeout=30) as response:
                return response.read().decode("utf-8")
        except (OSError, URLError, UnicodeError) as error:
            raise BumpError(Failure.TOOLCHAIN_FETCH_FAILED) from error


def read_file(path: Path) -> str:
    try:
        return path.read_text()
    except (OSError, UnicodeError) as error:
        raise BumpError(Failure.FILE_READ_FAILED) from error


def require_clean_inputs(root: Path) -> None:
    try:
        result = subprocess.run(
            ["git", "diff", "--quiet", "HEAD", "--", *BUMP_FILES],
            cwd=root,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
        )
    except OSError as error:
        raise BumpError(Failure.GIT_CHECK_FAILED) from error
    if result.returncode == 1:
        raise BumpError(Failure.DIRTY_INPUTS)
    if result.returncode != 0:
        raise BumpError(Failure.GIT_CHECK_FAILED)


def restore_from_head(root: Path) -> None:
    try:
        result = subprocess.run(
            ["git", "restore", "--source=HEAD", "--", *BUMP_FILES],
            cwd=root,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
        )
    except OSError as error:
        raise BumpError(Failure.RESTORE_FAILED) from error
    if result.returncode != 0:
        raise BumpError(Failure.RESTORE_FAILED)


def fork_dependencies(contents: str, tag: str) -> tuple[str, str]:
    try:
        manifest = tomllib.loads(contents)
    except tomllib.TOMLDecodeError as error:
        raise BumpError(Failure.DEPENDENCIES_UNEXPECTED) from error
    sections = {name: manifest.get(name, {}) for name in ("dependencies", "dev-dependencies")}
    if any(not isinstance(dependencies, dict) for dependencies in sections.values()):
        raise BumpError(Failure.DEPENDENCIES_UNEXPECTED)
    found = {
        (section, name)
        for section, dependencies in sections.items()
        for name, dependency in dependencies.items()
        if isinstance(dependency, dict) and dependency.get("git") == FORK_URL
    }
    if found != EXPECTED:
        raise BumpError(Failure.DEPENDENCIES_UNEXPECTED)
    old_tags = [sections[section][name].get("tag") for section, name in EXPECTED]
    if any(not isinstance(value, str) for value in old_tags) or len(set(old_tags)) != 1:
        raise BumpError(Failure.TAGS_INCONSISTENT)
    old_tag = old_tags[0]

    section = ""
    replaced = set()
    lines = []
    for line in contents.splitlines(keepends=True):
        if line.startswith("["):
            section = line.strip().strip("[]")
        match = re.match(r"^([ \t]*)([\w-]+)([ \t]*=[ \t]*\{)([^}\n]*)(\}[^\n]*)", line)
        if match and (section, match[2]) in EXPECTED:
            body, count = FIELD_PATTERN.subn(lambda field: field[1] + tag + field[3], match[4])
            if count != 1 or f'git = "{FORK_URL}"' not in body:
                raise BumpError(Failure.DEPENDENCIES_UNEXPECTED)
            line = line[: match.start(4)] + body + line[match.end(4) :]
            replaced.add((section, match[2]))
        lines.append(line)
    if replaced != EXPECTED:
        raise BumpError(Failure.DEPENDENCIES_UNEXPECTED)
    return old_tag, "".join(lines)


def channel(contents: str) -> str:
    try:
        value = tomllib.loads(contents)["toolchain"]["channel"]
    except (tomllib.TOMLDecodeError, KeyError, TypeError) as error:
        raise BumpError(Failure.TOOLCHAIN_INVALID) from error
    if not isinstance(value, str) or not value:
        raise BumpError(Failure.TOOLCHAIN_INVALID)
    return value


def set_channel(contents: str, new_channel: str) -> tuple[str, str]:
    old_channel = channel(contents)
    updated, count = CHANNEL_PATTERN.subn(
        lambda match: match[1] + new_channel + match[3], contents
    )
    if count != 1:
        raise BumpError(Failure.TOOLCHAIN_INVALID)
    return old_channel, updated


def fork_packages(contents: str, old_tag: str, new_tag: str) -> tuple[list[str], set[str]]:
    try:
        packages = tomllib.loads(contents)["package"]
    except (tomllib.TOMLDecodeError, KeyError, TypeError) as error:
        raise BumpError(Failure.LOCK_INVALID) from error
    if not isinstance(packages, list) or any(not isinstance(package, dict) for package in packages):
        raise BumpError(Failure.LOCK_INVALID)
    entries = [
        package for package in packages
        if package.get("source", "").startswith(f"git+{FORK_URL}?")
    ]
    if not entries or any(
        not package["source"].startswith(f"git+{FORK_URL}?tag=")
        or not isinstance(package.get("name"), str)
        for package in entries
    ):
        raise BumpError(Failure.LOCK_INVALID)
    tags = {package["source"].split("?tag=", 1)[1].split("#", 1)[0] for package in entries}
    if not tags <= {old_tag, new_tag}:
        raise BumpError(Failure.LOCK_INVALID)
    return sorted({package["name"] for package in entries}), tags


def cargo_update(root: Path, packages: list[str]) -> None:
    command = ["cargo", "update"]
    for package in packages:
        command.extend(("--package", package))
    try:
        process = subprocess.Popen(
            command,
            cwd=root,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=os.name == "posix",
        )
    except OSError as error:
        raise BumpError(Failure.UPDATE_FAILED) from error
    try:
        returncode = process.wait()
    except BaseException:
        stop_cargo_process(process)
        raise
    if returncode != 0:
        raise BumpError(Failure.UPDATE_FAILED)


def stop_cargo_process(process: subprocess.Popen) -> None:
    if process.poll() is not None:
        return
    try:
        if os.name == "posix":
            os.killpg(process.pid, signal.SIGTERM)
        else:
            process.terminate()
    except ProcessLookupError:
        process.wait()
        return
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        try:
            if os.name == "posix":
                os.killpg(process.pid, signal.SIGKILL)
            else:
                process.kill()
        except ProcessLookupError:
            pass
        process.wait()


def bump(root: Path, tag: str, remote: ForkRemote, update=cargo_update) -> BumpResult:
    require_clean_inputs(root)
    if not TAG_PATTERN.fullmatch(tag):
        raise BumpError(Failure.INVALID_TAG)
    config = root / ".cargo" / "config.toml"
    if config.exists():
        try:
            contents = read_file(config)
            cargo_config = tomllib.loads(contents)
        except tomllib.TOMLDecodeError as error:
            raise BumpError(Failure.CONFIG_INVALID) from error
        patches = cargo_config.get("patch", {})
        if not isinstance(patches, dict):
            raise BumpError(Failure.CONFIG_INVALID)
        if "# BEGIN SpaceTerm local GPUI patches" in contents or FORK_URL in patches:
            raise BumpError(Failure.OVERRIDE_ACTIVE)

    manifest_path = root / "Cargo.toml"
    toolchain_path = root / "rust-toolchain.toml"
    lock_path = root / "Cargo.lock"
    manifest_contents = read_file(manifest_path)
    toolchain_contents = read_file(toolchain_path)
    lock_contents = read_file(lock_path)
    old_tag, new_manifest = fork_dependencies(manifest_contents, tag)
    old_channel = channel(toolchain_contents)
    packages, lock_tags = fork_packages(lock_contents, old_tag, tag)
    if not remote.has_tag(tag):
        raise BumpError(Failure.TAG_MISSING)
    new_channel = channel(remote.toolchain(tag))
    _, new_toolchain = set_channel(toolchain_contents, new_channel)

    with sigterm_cancellation():
        try:
            if new_manifest != manifest_contents:
                manifest_path.write_text(new_manifest)
            if new_toolchain != toolchain_contents:
                toolchain_path.write_text(new_toolchain)
            if old_tag != tag or lock_tags != {tag}:
                try:
                    update(root, packages)
                    _, updated_tags = fork_packages(read_file(lock_path), tag, tag)
                    if updated_tags != {tag}:
                        raise BumpError(Failure.UPDATE_FAILED)
                except Exception as error:
                    raise BumpError(Failure.UPDATE_FAILED) from error
        except BaseException as error:
            try:
                restore_from_head(root)
            except BaseException as restore_error:
                raise BumpError(Failure.RESTORE_FAILED) from restore_error
            if isinstance(error, (BumpError, KeyboardInterrupt, SystemExit, BumpCancelled)):
                raise
            raise BumpError(Failure.FILE_WRITE_FAILED) from error
    return BumpResult(old_tag, tag, old_channel, new_channel)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag")
    with sigterm_cancellation():
        try:
            args = parser.parse_args()
        except SystemExit:
            raise
        except (KeyboardInterrupt, BumpCancelled):
            parser.exit(130, "gpui:bump: cancelled\n")
        except BaseException:
            parser.exit(1, "gpui:bump: failed\n")
        try:
            result = bump(ROOT, args.tag, ForkRemote())
            print(f"Fork tag: {result.old_tag} -> {result.new_tag}")
            print(f"Rust toolchain: {result.old_channel} -> {result.new_channel}")
            print("Next: mise run validate && mise run test:macos")
        except BumpError as error:
            parser.exit(1, f"gpui:bump: {error}\n")
        except (KeyboardInterrupt, SystemExit, BumpCancelled):
            parser.exit(130, "gpui:bump: cancelled\n")
        except BaseException:
            parser.exit(1, "gpui:bump: failed\n")


if __name__ == "__main__":
    try:
        main()
    except SystemExit:
        raise
    except (KeyboardInterrupt, BumpCancelled):
        sys.stderr.write("gpui:bump: cancelled\n")
        raise SystemExit(130) from None
    except BaseException:
        sys.stderr.write("gpui:bump: failed\n")
        raise SystemExit(1) from None
