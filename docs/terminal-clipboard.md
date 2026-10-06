# Terminal clipboard integration

OSC 52 connects terminal programs to the clipboard of the machine running SpaceTerm, including
when the programs run over SSH. Configure each program and intervening multiplexer to forward it.
Clipboard access is controlled in Settings > Privacy; enable reading there only when needed.

## Neovim over SSH

Select the OSC 52 provider before clipboard providers initialize:

```lua
if vim.env.SSH_TTY or vim.env.SSH_CONNECTION then
  vim.g.clipboard = 'osc52'
end
vim.opt.clipboard = 'unnamedplus'
```

This chooses the local terminal's clipboard instead of a tool such as `pbcopy` on the remote
machine. See [Neovim's clipboard provider documentation](https://neovim.io/doc/user/provider/#clipboard-osc52).

## tmux

Allow programs inside tmux to write through OSC 52 in `~/.tmux.conf`:

```tmux
set -s set-clipboard on
```

For clipboard reads, a tmux version exposing `get-clipboard` can forward requests with
`set -s get-clipboard request`. Configure each layer when nesting multiplexers.
See the [tmux clipboard documentation](https://github.com/tmux/tmux/wiki/Clipboard)
and [tmux option reference](https://github.com/tmux/tmux/blob/master/tmux.1).
