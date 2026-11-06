"""Write the Homebrew cask of a published release's disk image."""

import re
from pathlib import Path

from spaceterm_tasks import TaskError, release_feed, release_plan, update_signing
from spaceterm_tasks.packaging import DIST


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


def write(tag, tap, directory=DIST / "release"):
    """Point the tap's cask at the disk image that publication just verified on GitHub."""
    version = release_plan.greenfield_tag(tag)
    image = Path(directory) / release_feed.dmg_name(version)
    if not image.is_file():
        raise TaskError("the Homebrew cask needs the assembled release disk image")
    destination = Path(tap) / "Casks/spaceterm.rb"
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(cask(version, update_signing.sha256(image)))
