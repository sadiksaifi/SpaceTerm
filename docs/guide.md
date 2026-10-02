# SpaceTerm guide

## Terminal clipboard

Programs in the Pane with Terminal Input Focus can copy plain text through OSC 52, including over
SSH and in fullscreen terminal interfaces. Settings > Privacy > Clipboard controls copying and
reading separately. Copying is enabled by default. Reading is disabled by default; enabling it
lets the focused terminal program read your system clipboard. Requests are limited to 1 MiB of text.

Cmd+C copies SpaceTerm's Selection. With no Selection, applications that enable the enhanced
keyboard protocol receive Cmd+C. Cmd+V follows SpaceTerm's paste handling, including bracketed
paste. When an application captures the mouse, Shift-drag creates a SpaceTerm Selection.

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

## Computer-use permissions

Computer-use tools that take screenshots or control other apps inherit macOS Screen & System Audio
Recording and Device Control and Data Access from SpaceTerm. SpaceTerm never asks for them on its
own. To grant them, choose Set Up next to a permission in Settings > Privacy. SpaceTerm opens the
privacy list in System Settings and docks a guide at the bottom of its window. Drag SpaceTerm from
the guide into the list. The guide reports the grant, and tools started afterward receive it.

A tool can ask SpaceTerm to offer this setup by writing a Permission Request to the terminal:

```sh
printf '\033]7701;permissions=screen-recording,accessibility\033\\'
```

The list names `screen-recording`, `accessibility`, or both. SpaceTerm ignores unknown names, shows
a notice in the Pane only for permissions it lacks, and starts the setup only when you choose
Set Up. Not Now silences later requests for those permissions in that Pane. Any output a Local Pane
shows can carry a request, including output from `ssh` or a file you print, so the notice never
claims which program asked. Remote Panes ignore Permission Requests. Inside tmux, wrap the request in tmux passthrough and enable
`set -g allow-passthrough on`.

## Built with

Rust powers the application, GPUI provides the native GPU-rendered interface, and `libghostty-vt`
provides terminal emulation. Remote Workspaces use the system OpenSSH client.

## Build from source

You need macOS, Xcode 26 or newer, and [`mise`](https://mise.jdx.dev/).
Mise manages the official Zig compiler and the remaining development tools. Xcode supplies
the Metal compiler, macOS SDK, and icon packaging tools.

```sh
git clone https://github.com/sadiksaifi/SpaceTerm.git
cd SpaceTerm
mise trust

# Install pinned tools, initialize submodules, and verify the macOS development environment
mise run setup:macos

# Run from source
mise run development

# Build, verify, and install to /Applications
mise run preflight:install:macos
```

Run `mise tasks` to see the complete command list. Rust is pinned in `rust-toolchain.toml`, and
development tools and tasks are pinned in `.mise.toml`. Platform-specific tasks carry an explicit
platform segment such as `:macos`.
