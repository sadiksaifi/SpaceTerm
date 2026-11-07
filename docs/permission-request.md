# Permission Requests

A program in a local Pane can ask SpaceTerm to set up macOS permissions with `OSC 7701 ; permissions=<list> ST`:

```sh
printf '\033]7701;permissions=screen-recording,accessibility\033\\'
```

- `<list>` takes `screen-recording`, `accessibility` or both, separated by a comma.
- ST (`ESC \`) or BEL ends the sequence.
- The request grants nothing. The person decides whether to start setup.
- SpaceTerm Core gets the permission, not the desktop app.
- Remote Panes ignore requests.
- Inside tmux, set `allow-passthrough on` and wrap the sequence in tmux's passthrough form, doubling each inner ESC. See the [tmux option reference](https://github.com/tmux/tmux/blob/master/tmux.1).

For microphone and camera, use Settings > Privacy or `spaceterm permissions request PERMISSION`.
