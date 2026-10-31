# SpaceTerm brand assets

The SpaceTerm mark is a small solar system: a warm sun, two orbits, and a planet on each. A terminal multiplexer gives you room to spread your work across workspaces, tabs and panes. The orbits are that room, and the mark stays readable from the 18 pt menu bar up to a 1024 px App Store tile.

Each folder holds the files for one platform. Nothing here is wired into packaging yet. Copy files from here when a platform needs them.

## Apple

`apple/SpaceTerm.icon` is the source of every Apple icon. It is an Icon Composer document with three glass layers, back to front: Orbits, Sun, Planets. Open it in Icon Composer to adjust glass, shadows, or the Dark and Tinted fills. Xcode compiles it straight into an app for macOS, iOS, iPadOS and watchOS, so the PNGs in the platform folders are for older targets, store listings and previews.

- `macos/`: `SpaceTerm.icns` (compiled by `actool`), `AppIcon.iconset`, and Icon Composer renders of all six appearances in `appearances/`.
- `macos/menu-bar/`: the one-color template glyph for the status item, as SVG, PDF and PNG at 1x and 2x. The `Template` suffix tells AppKit to tint it.
- `ios/` and `ipados/`: 1024 px icons for Default, Dark and Tinted asset catalog slots, plus the six appearance renders.
- `watchos/`: the circular 1024 px icon and the appearance renders.
- `visionos/layers-1024/`: Back, Middle and Front layers. visionOS crops them to a circle and adds its own depth between them.
- `tvos/`: Back, Middle and Front layers for the home screen icon (400x240 at 1x and 2x) and the App Store icon (1280x768).

## Other platforms

- `android/res/`: an adaptive launcher icon with a gradient background, foreground PNGs for each density, and a monochrome vector for themed icons. `play-store-512.png` is the store listing icon.
- `linux/hicolor/`: PNGs from 16 to 512 px, a scalable SVG and a symbolic SVG, named for the app ID `io.github.sadiksaifi.spaceterm`.
- `windows/`: `SpaceTerm.ico` with sizes from 16 to 256 px, and MSIX tile logos.
- `web/`: an SVG favicon that follows the system light or dark setting, `favicon.ico`, the Apple touch icon, and 192, 512 and maskable PWA icons.
- `logo/`: the glyph in `currentColor`, the full-color mark for light and dark backgrounds, and the icon as SVG.

## Geometry

The mark sits on a 100-unit grid. The sun has radius 14. The orbits have radii 27.5 and 43.5 with a 5.4 stroke. The planets have radii 6.2 and 7.4 and keep 3.6 units of clearance from their orbit. Below 32 px the icons drop the glass highlights and shadows so the shapes stay crisp.
