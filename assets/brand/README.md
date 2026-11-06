# SpaceTerm brand assets

The SpaceTerm mark is a small solar system: a warm sun, two orbits, and a planet on each. A terminal multiplexer gives you room to spread your work across workspaces, tabs and panes. The orbits are that room, and the mark stays readable from a 16 pt menu bar glyph up to a 1024 px App Store tile.

Everything SpaceTerm shows of itself comes from this folder: the app icon on every platform, the menu bar and tray glyphs, the disk image window and the GitHub link card. Packaging and the app copy these files. They never draw their own.

## Where things come from

Two things are written by hand. `apple/SpaceTerm.icon/icon.json` holds the colors, the glass and the six appearances, and you edit it in Icon Composer. The mark's geometry lives in [`brand.py`](../../mise-tasks/lib/spaceterm_tasks/brand.py). Everything else here is generated:

```sh
mise run brand          # regenerate on macOS with Xcode installed
mise run brand --check  # verify on any host; part of mise run lint
```

`manifest.json` records a digest of every source and every output, so the check catches a stale file or a hand edit without redrawing anything.

## Apple

`apple/SpaceTerm.icon` is the app icon for macOS, iOS, iPadOS and watchOS. Its three glass layers, back to front, are Orbits, Sun and Planets, and generation writes their artwork. Xcode's `actool` compiles the document into the app, every Release, Preflight and Development build uses it, and each macOS release draws its own Liquid Glass from the layers. In the Clear and Tinted appearances the layers turn opaque and full-bright, so the system tint has the whole mark to work with.

- `macos/appearances/`, `ios/appearances/`: Icon Composer renders of the six appearances, for previews. iPadOS uses the iOS renders.
- `watchos/AppIcon-1088.png`: the circular icon at watchOS's layout size.
- `macos/menu-bar/`: the 16 pt template glyph for the status item, and the attention variant with a dot on the outer orbit.
- `macos/dmg/background.tiff`: the disk image window at 1x and 2x.

Icon Composer does not build visionOS or tvOS icons, so generation draws their layers with the document's light colors and flat fills. Both systems add their own highlights, shadows and depth.

- `visionos/layers-1024/`: Back, Middle and Front for a solid image stack.
- `tvos/`: Back, Middle and Front for the home screen icon and the App Store icon, plus the Top Shelf images.

## Everywhere else

Outside Apple platforms the mark sits on a tile drawn the way GNOME and Flathub ask: a rounded square on Flathub's 103 of 128 grid, a flat face, a darker profile along the bottom, and no baked shadows or glows. KDE, Xfce and the rest read the same files.

- `linux/hicolor/`: PNGs from 16 to 512 px, the scalable SVG, and the symbolic SVG for small sizes and the status area, named for the app ID `com.peblon.spaceterm`.
- `windows/`: `SpaceTerm.ico` and the MSIX logos.
- `android/`: the adaptive launcher icon with its themed monochrome layer, icons for older launchers, and the Google Play icon.
- `web/`: a favicon that follows the color scheme, `favicon.ico`, the Apple touch icon, and web app icons including a maskable one.
- `logo/`: the glyph in `currentColor`, the full-color mark for light and dark backgrounds, and the tile.
- `github/social-preview.png`: the repository's link card, uploaded in the GitHub repository settings.

## Geometry

The mark sits on a 100-unit grid. The sun has radius 14. The orbits have radii 27.5 and 43.5 with a stroke of 7. The planets have radii 6.2 and 7.4, and each orbit leaves 4.5 units of clearance around its planet.
