"""Write the Homebrew cask of a published release's disk image."""

import re
from pathlib import Path

from spaceterm_tasks import TaskError, release_feed, release_plan


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

  uninstall on_upgrade: :signal,
            signal:     ["TERM", "com.peblon.spaceterm"]

  zap launchctl: [
        "com.peblon.spaceterm.core",
        "com.peblon.spaceterm.core.recovery",
      ],
      delete:    "~/.local/bin/spaceterm",
      trash:     [
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


def write(tag, tap, *, request=release_plan.api):
    """Point the tap's cask at the published disk image, by the digest GitHub records.

    After a rerun, this run's own disk image may differ from the one people download.
    """
    version = release_plan.greenfield_tag(tag)
    published = request(f"repos/{release_plan.REPOSITORY}/releases/tags/{tag}")
    if published["draft"] or published.get("immutable") is not True:
        raise TaskError("the Homebrew cask needs the immutable published release")
    name = release_feed.dmg_name(version)
    assets = release_plan.pages(
        f"repos/{release_plan.REPOSITORY}/releases/{published['id']}/assets", request=request
    )
    digests = [asset.get("digest") or "" for asset in assets if asset["name"] == name]
    if len(digests) != 1 or not digests[0].startswith("sha256:"):
        raise TaskError("the published release must hold one macOS disk image with its digest")
    destination = Path(tap) / "Casks/spaceterm.rb"
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(cask(version, digests[0].removeprefix("sha256:")))
