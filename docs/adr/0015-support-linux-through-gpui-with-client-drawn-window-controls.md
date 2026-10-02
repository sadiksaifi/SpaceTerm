# Support Linux through GPUI with client-drawn Window Controls

SpaceTerm supports Wayland and X11 through GPUI while keeping product policy and presentation
portable. The macOS design remains the visual baseline. Identical POSIX behavior belongs in shared
`unix_*` adapters; host compositions select the narrow adapters and facts where the systems differ.
macOS retains its existing native behavior.

Linux uses client-drawn Window Controls and a rounded window frame. Controls follow the desktop
button layout and available window capabilities, but always occupy the top right. The sidebar
toggle sits at the far left because Linux has no traffic lights. The Settings Window uses the normal
window kind on Linux and remains non-resizable. Background transparency and blur follow host
capabilities, with an opaque fallback when unavailable.

The Linux Keymap assigns application Shortcuts to Ctrl+Shift, optionally with Alt. Plain Ctrl and
Alt reach the terminal; Super belongs to the desktop. Shortcut labels use text. Application commands
normally reached through the macOS menu are available through the Linux Command Palette.
Ctrl+click activates Terminal Hyperlinks on Linux; Command+click remains the macOS gesture.

Host font facts preserve `.SystemUIFont` and Menlo on macOS. Linux Application Chrome uses the
bundled Inter 4.1 release under the SIL Open Font License, with recorded checksums and notices.
Terminal text starts with the bundled SpaceTerm Default family and falls back to fontconfig
monospace on Linux.

Linux accessibility uses GPUI's AccessKit integration with AT-SPI. File Preview uses GNOME Sushi
when its D-Bus service is activatable; the command is otherwise absent. Services, Secure Input, and
microphone permission controls have no Linux capability. Linux quit confirmations use SpaceTerm's
Modal presentation, and a session-bus application identity makes subsequent launches activate the
existing instance.

Linux supports source builds through mise only. This change adds no packaging, distribution, or
updates: the Linux update adapter stays inert and update controls are absent.
