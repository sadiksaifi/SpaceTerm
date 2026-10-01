#!/usr/bin/env python3
"""Render the sadiksaifi/tap Homebrew cask for a published release's disk image."""

import argparse
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
NOTARIZATION_NOTICE = "By using SpaceTerm, you acknowledge that it's not notarized."
CASK = """\
cask "spaceterm" do
  version "{version}"
  sha256 "{sha256}"

  url "https://github.com/sadiksaifi/SpaceTerm/releases/download/v#{{version}}/SpaceTerm-#{{version}}-darwin-arm64.dmg"
  name "SpaceTerm"
  desc "Native desktop terminal multiplexer"
  homepage "https://github.com/sadiksaifi/SpaceTerm"

  auto_updates true
  depends_on arch: :arm64
  depends_on macos: :tahoe

  app "SpaceTerm.app"

  # SpaceTerm is ad hoc signed and not notarized; see ADR 0009 in the SpaceTerm repository.
  postflight_steps do
    run "/usr/bin/xattr", args: ["-dr", "com.apple.quarantine", "{{{{appdir}}}}/SpaceTerm.app"]
  end

  zap trash: [
    "~/.cache/spaceterm",
    "~/.config/spaceterm",
    "~/.local/share/spaceterm",
    "~/.local/state/spaceterm",
    "~/Library/Caches/io.github.sadiksaifi.spaceterm",
    "~/Library/HTTPStorages/io.github.sadiksaifi.spaceterm",
    "~/Library/HTTPStorages/io.github.sadiksaifi.spaceterm.binarycookies",
    "~/Library/Preferences/io.github.sadiksaifi.spaceterm.plist",
  ]

  caveats "{notice}"
end
"""


def release_version(tag):
    match = re.fullmatch(r"v(\d+\.\d+\.\d+)", tag)
    if not match:
        raise ValueError("the release tag must be a stable v<version> tag")
    return match.group(1)


def supersedes(tag, cask):
    """Whether the tag's release is at least as new as the version an existing cask installs."""
    match = re.search(r'^  version "(\d+\.\d+\.\d+)"$', cask, re.MULTILINE)
    if not match:
        raise ValueError("the existing cask has no stable version")
    def parse(version):
        return tuple(int(part) for part in version.split("."))
    return parse(release_version(tag)) >= parse(match.group(1))


def render(tag, checksums):
    version = release_version(tag)
    archive = f"SpaceTerm-{version}-darwin-arm64.dmg"
    digests = [line.split("  ", 1)[0] for line in checksums.splitlines() if line.endswith(f"  {archive}")]
    if len(digests) != 1 or not re.fullmatch(r"[0-9a-f]{64}", digests[0]):
        raise ValueError(f"the release checksums must list {archive} once")
    return CASK.format(version=version, sha256=digests[0], notice=NOTARIZATION_NOTICE)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag")
    parser.add_argument("output", type=Path, help="cask file to write")
    args = parser.parse_args()
    try:
        # Re-running an older release's job must not roll the tap back.
        if args.output.exists() and not supersedes(args.tag, args.output.read_text()):
            print(f"The cask already installs a release newer than SpaceTerm {args.tag}.")
            return
        cask = render(args.tag, (ROOT / "dist/release/SHA256SUMS").read_text())
    except (OSError, ValueError) as error:
        parser.exit(1, f"error: {error}\n")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(cask)
    print(f"Rendered the SpaceTerm {args.tag} Homebrew cask.")


if __name__ == "__main__":
    main()
