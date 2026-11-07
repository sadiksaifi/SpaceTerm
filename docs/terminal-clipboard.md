# Terminal clipboard integration

OSC 52 connects terminal programs to the clipboard of the computer running the SpaceTerm desktop.
It works in Panes on remote Machines and over `ssh` inside a local Pane.
Configure each program and intervening multiplexer to send it.
Settings > Privacy controls clipboard access. Writes are on by default. Turn on reads only when needed.

## Neovim

Select the OSC 52 provider before clipboard providers initialize:

```lua
if vim.env.SPACETERM or vim.env.SSH_TTY then
  vim.g.clipboard = 'osc52'
end
vim.opt.clipboard = 'unnamedplus'
```

SpaceTerm sets `SPACETERM` in every Pane it starts, local or remote. `ssh` does not forward it, so
`SSH_TTY` covers `ssh` from a local Pane. Either way, Neovim uses the clipboard of the computer running
the desktop instead of a clipboard tool on the Machine. This also applies in local Panes, so pasting
from Neovim needs clipboard reads turned on. See [Neovim's clipboard provider documentation](https://neovim.io/doc/user/provider/#clipboard-osc52).

## tmux

Allow programs inside tmux to write through OSC 52 in `~/.tmux.conf`:

```tmux
set -s set-clipboard on
```

For clipboard reads, a tmux version exposing `get-clipboard` can forward requests with
`set -s get-clipboard request`. Configure each layer when nesting multiplexers.
See the [tmux clipboard documentation](https://github.com/tmux/tmux/wiki/Clipboard)
and [tmux option reference](https://github.com/tmux/tmux/blob/master/tmux.1).
