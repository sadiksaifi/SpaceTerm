# SpaceTerm guide

## Terminal clipboard

Programs in the Pane with Terminal Input Focus can copy plain text through OSC 52, including over
SSH and in fullscreen terminal interfaces. Settings > Privacy > Clipboard controls copying and
reading separately. Copying is enabled by default. Reading is disabled by default; enabling it
lets the focused terminal program read your system clipboard. Requests are limited to 1 MiB of text.

Cmd+C on macOS or Ctrl+Shift+C on Linux copies SpaceTerm's Selection. On macOS, with no
Selection, applications that enable the enhanced keyboard protocol receive Cmd+C. Cmd+V on macOS
or Ctrl+Shift+V on Linux follows SpaceTerm's paste handling, including bracketed paste. When an application captures the mouse, Shift-drag creates a SpaceTerm Selection.

For Neovim over SSH, select its OSC 52 provider before clipboard providers initialize:

```lua
if vim.env.SSH_TTY or vim.env.SSH_CONNECTION then
  vim.g.clipboard = 'osc52'
end
vim.opt.clipboard = 'unnamedplus'
```

On a remote Mac, Neovim otherwise prefers `pbcopy` and `pbpaste`, which access that Mac's
clipboard. The SSH configuration above routes `y` and `yy` to the local clipboard through
SpaceTerm. Restart Neovim after changing its provider.

Pasting with `p` through Neovim's OSC 52 provider also requires enabling
clipboard reading in SpaceTerm. Denied or unavailable reads return empty text.

For tmux, allow application clipboard writes in `~/.tmux.conf`:

```tmux
set -s set-clipboard on
```

SpaceTerm advertises the `Ms` terminal capability. In tmux 3.7c and newer, forwarding application
clipboard reads also requires `set -s get-clipboard request`. Enable `set -g mouse on` if you want
tmux to route mouse input to fullscreen applications. Reload the configuration or restart tmux
after changing these settings. Nested multiplexers must forward OSC 52 at every layer.

[Neovim clipboard provider](https://neovim.io/doc/user/provider/#clipboard-osc52) and
[tmux clipboard configuration](https://github.com/tmux/tmux/wiki/Clipboard) describe program setup.
Codex CLI, Claude Code, pi, and Herdr can use their own Selection and copy commands when they emit
OSC 52. Their paste behavior also depends on the program and multiplexer configuration.

## Built with

Rust powers the application, GPUI provides the native GPU-rendered interface, and `libghostty-vt`
provides terminal emulation. Remote Workspaces use the system OpenSSH client.

## Build from source

Install [`mise`](https://mise.jdx.dev/) to manage the pinned development tools, including Zig.
Clone the repository and trust its tasks:

```sh
git clone https://github.com/sadiksaifi/SpaceTerm.git
cd SpaceTerm
mise trust
```

### macOS

Xcode 26 or newer supplies the Metal compiler, macOS SDK, and icon packaging tools.

```sh
# Install pinned tools, initialize submodules, and verify the macOS development environment
mise run setup:macos

# Run from source
mise run development

# Build, verify, and install to /Applications
mise run preflight:install:macos
```

### Linux

Wayland and X11 are supported. On Debian or Ubuntu, install:

```sh
sudo apt install build-essential pkg-config libxkbcommon-dev libxkbcommon-x11-dev \
  libwayland-dev libxcb1-dev libx11-xcb-dev libfontconfig-dev libfreetype-dev \
  libvulkan1 mesa-vulkan-drivers ncurses-bin openssh-client dbus desktop-file-utils

mise run setup:linux
mise run doctor:linux
mise run development
```

`setup:linux` installs pinned tools, initializes submodules, and runs the environment checks.
`doctor:linux` repeats those checks. Development uses Wayland when available;
`mise run development:x11:linux` selects X11. Each launch registers the Development desktop entry
for desktop activation and notifications. GNOME Sushi (`gnome-sushi`) is optional for File Preview.
Ctrl+Shift+P opens application commands, including About, Help, and Export Terminal Diagnostics.
Linux currently supports source builds only, with no packaging, distribution, or updates.

Run `mise tasks` to see the complete command list. Rust is pinned in `rust-toolchain.toml`, and
development tools and tasks are pinned in `.mise.toml`. Platform-specific tasks carry an explicit
platform segment such as `:macos` or `:linux`.
