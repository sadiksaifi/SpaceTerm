# Permission Requests

A program in a Pane can offer macOS Permission Setup by writing this sequence:

```text
OSC 7701 ; permissions=<list> ST
```

`<list>` is a comma-separated set of `screen-recording` and `accessibility`.
ST (`ESC \`) or BEL ends the sequence.

```sh
printf '\033]7701;permissions=screen-recording,accessibility\033\\'
```

A request grants no permission; the person decides whether to start setup.
Remote Panes ignore requests.

Inside tmux, enable `set -g allow-passthrough on` and wrap the sequence in tmux's passthrough form,
doubling each inner ESC. See the [tmux option reference](https://github.com/tmux/tmux/blob/master/tmux.1).
