# Support Linux through GPUI with client-drawn Window Controls

SpaceTerm supports Wayland and X11 through GPUI while keeping product policy and presentation
portable. The macOS design remains the visual baseline. Identical POSIX behavior belongs in shared
`unix_*` adapters; host compositions select the narrow adapters and facts where the systems differ.
macOS retains its existing native behavior.

Linux always draws client-side decorations, on Wayland and X11, even where the compositor offers
server-side ones. Like the macOS traffic lights, the Window Controls sit inside SpaceTerm's chrome
but keep the desktop's own look: Adwaita on GNOME and unknown desktops, Breeze on KDE, with the
desktop's symbolic icons, frame radius, outline, and shadow. SpaceTerm's theme does not restyle
them. They follow the desktop button layout live, on either side; the sidebar toggle follows
left-side controls and otherwise sits at the far left. The Settings Window uses the normal
window kind on Linux and remains non-resizable. Background transparency and blur follow host
capabilities. Without either, the window stays opaque and Settings shows both choices disabled at
their defaults, which then also govern floating surfaces, while the stored choices are retained.

The Linux Keymap assigns application Shortcuts to Ctrl+Shift, optionally with Alt, and to the chords
outside it that Linux terminals also leave to the application, as ADR 0010 lists: Alt+digit for Tabs,
Ctrl+Alt+digit for Workspaces, Ctrl+Page Up and Ctrl+Page Down for Tab navigation, Ctrl+`=`, Ctrl+`-`,
and Ctrl+0 for font size, and Shift with the paging keys for Scrollback. Unshifted xterm Control
forms remain available to the terminal;
Super belongs to the desktop. Shortcut labels use text. Linux has no application Command Palette;
About opens SpaceTerm's own About window in the same client-drawn chrome. Ctrl+click activates Terminal Hyperlinks and is not a secondary click.
PRIMARY Selection and middle-click paste follow Linux conventions. Right-clicking empty titlebar
space opens the desktop window menu. macOS retains its existing pointer gestures.

Host font facts preserve `.SystemUIFont` and Menlo on macOS. Linux Application Chrome uses the
bundled Inter 4.1 release under the SIL Open Font License, privately named SpaceTerm UI to avoid
collisions with installed versions, with recorded checksums and notices.
Terminal text starts with the bundled SpaceTerm Default family and falls back to fontconfig
monospace on Linux.

Linux accessibility uses GPUI's AccessKit integration with AT-SPI. SpaceTerm pins the core,
consumer, and AT-SPI translation crates to its AccessKit fork so terminal geometry, text
navigation, and bounded text-event patches are maintained and tested with their dependency owner.
File Preview uses GNOME Sushi when its D-Bus service is activatable; the command is otherwise
absent. Hide, Hide Others, Show All, Bring All to Front, the character palette, Services, Secure
Input, System Permissions, and microphone controls have no Linux capability. Linux quit
confirmations use SpaceTerm's Alert, closing the last window quits, and a session-bus application
identity makes subsequent launches activate the existing instance.

Linux supports source builds through mise only. This change adds no packaging, distribution, or
updates: the Linux update adapter stays inert and update controls are absent.
