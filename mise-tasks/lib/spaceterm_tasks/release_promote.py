"""Promote immutable published bytes and prepare their Homebrew cask."""

import re
import subprocess
import tempfile
from pathlib import Path

from spaceterm_tasks import TaskError, release_feed, release_plan, update_signing


def cask(version, digest):
    release_plan.greenfield_tag(f"v{version}")
    if not re.fullmatch(r"[0-9a-f]{64}", digest):
        raise TaskError("the Homebrew disk image needs a SHA-256 digest")
    name = release_feed.dmg_name(version).replace(version, "#{version}")
    return f'''cask "spaceterm" do
  version "{version}"
  sha256 "{digest}"

  url "https://github.com/{release_plan.REPOSITORY}/releases/download/v#{{version}}/{name}"
  name "SpaceTerm"
  desc "Native desktop terminal multiplexer"
  homepage "https://spaceterm.peblon.com"

  auto_updates true
  depends_on arch: :arm64
  depends_on macos: :tahoe

  app "SpaceTerm.app"

  zap trash: [
    "~/.cache/spaceterm",
    "~/.config/spaceterm",
    "~/.local/share/spaceterm",
    "~/.local/state/spaceterm",
    "~/Library/Caches/com.peblon.spaceterm",
    "~/Library/HTTPStorages/com.peblon.spaceterm",
    "~/Library/HTTPStorages/com.peblon.spaceterm.binarycookies",
    "~/Library/Preferences/com.peblon.spaceterm.plist",
    "~/Library/Saved Application State/com.peblon.spaceterm.savedState",
  ]
end
'''


def prepare(tag, tap):
    version = release_plan.greenfield_tag(tag)
    release_plan.immutable()
    published = release_plan.api(f"repos/{release_plan.REPOSITORY}/releases/tags/{tag}")
    if published["draft"] or published["prerelease"] or not published.get("immutable"):
        raise TaskError("promotion requires an immutable published stable release")
    name = release_feed.dmg_name(version)
    assets = release_plan.pages(
        f"repos/{release_plan.REPOSITORY}/releases/{published['id']}/assets"
    )
    matches = [asset for asset in assets if asset["name"] == name]
    if len(matches) != 1:
        raise TaskError("the published release must hold exactly one macOS disk image")
    with tempfile.TemporaryDirectory(prefix="spaceterm-promote-") as temporary:
        if subprocess.run(
            [
                "gh",
                "release",
                "download",
                tag,
                "--repo",
                release_plan.REPOSITORY,
                "--pattern",
                name,
                "--dir",
                temporary,
            ]
        ).returncode:
            raise TaskError("the published disk image could not be downloaded")
        digest = update_signing.sha256(Path(temporary) / name)
        if matches[0].get("digest") != f"sha256:{digest}":
            raise TaskError("the downloaded disk image differs from the immutable release")
        if subprocess.run(
            [
                "gh",
                "attestation",
                "verify",
                str(Path(temporary) / name),
                "--repo",
                release_plan.REPOSITORY,
                "--signer-workflow",
                f"{release_plan.REPOSITORY}/.github/workflows/release.yml",
            ]
        ).returncode:
            raise TaskError("the published disk image has no verified release attestation")
    destination = Path(tap) / "Casks/spaceterm.rb"
    destination.write_text(cask(version, digest))
