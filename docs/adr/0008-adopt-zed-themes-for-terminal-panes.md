# Adopt Zed themes for terminal panes

Terminal Themes are the only user-selectable colors in SpaceTerm, and Zed's theme format is their
only interchange format. Zed's extension registry already publishes hundreds of theme extensions,
each written by people who chose terminal colors with care. Reading that format gives SpaceTerm
the same library without asking authors to publish twice. A second, SpaceTerm-specific package
format would split that library and would need its own exports, versions, and documentation. It
was removed.

Application Chrome stays built in. A Zed theme also colors Zed's own interface, but SpaceTerm reads
only its terminal roles and the few editor roles a terminal needs. Chrome colors come from the
Chrome Theme Compiler alone (see ADR 0006), so no installed theme can reduce Chrome contrast or
break the material hierarchy.

Translation happens once, at install. SpaceTerm stores each theme as a complete, opaque protocol
palette in the Settings Document instead of the Zed source. Resolution then never parses Zed JSON,
and a later change to translation affects only later installs. Translation is lenient in the way
Zed's loader is, because a strict reader would refuse themes that work in Zed.

Installed identities derive from the source. A registry theme is identified by its extension, name,
and appearance. Reinstalling or updating therefore replaces a theme in place, and every selection
that names it survives. Installing an extension also removes the themes its earlier versions
installed, so the catalog holds what the extension currently ships.

A selection always names an installed theme. When a selected theme leaves the catalog, through
removal, an update that no longer ships it, or a Settings file naming a theme SpaceTerm does not
have, its slot returns to the built-in theme for its appearance. Terminal panes then never draw
colors the Settings window cannot name, and reinstalling a removed theme does not select it again.

The registry is reached through the `RegistryTransport` Seam and only on an explicit request. The
production transport is HTTPS-only and verifies certificates against the Operating System's trust
store. Archives are read in memory within fixed bounds, and nothing in them reaches the
filesystem. SpaceTerm makes no automatic registry requests, including update checks.
