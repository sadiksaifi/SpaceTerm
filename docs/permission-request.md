# Permission Requests

A terminal tool can offer macOS Permission Setup by emitting this sequence:

```text
OSC 7701 ; permissions=screen-recording,accessibility ST
```

The comma-separated list accepts `screen-recording`, `accessibility`, or both. Terminate with
ST (`ESC` followed by `\`) or BEL. For example:

```sh
printf '\033]7701;permissions=screen-recording,accessibility\033\\'
```

A request offers setup for missing permissions; the person starts it. It grants no permission and
identifies no program, because any terminal output can carry it. Remote Panes ignore requests.

Inside tmux, enable `set -g allow-passthrough on` and wrap the sequence in tmux's passthrough form,
escaping each inner ESC by doubling it. See the [tmux option reference](https://github.com/tmux/tmux/blob/master/tmux.1).
