# Predict typed characters at the shell's line editor

A SpaceTerm Client draws a printable key typed at the shell's line editor before the SpaceTerm Server's echo arrives, once the round trip to that server is 30 ms or more. It turns prediction off again below 20 ms. Under 30 ms the echo arrives within about two frames at 60 Hz, so a prediction gains little and risks a visible correction. mosh and VS Code turn prediction on at 30 ms, and wezterm at 100 ms.

The server reads each Terminal Session's Line Mode from the PTY master. `tcgetattr` returns the ICANON, ECHO and ISIG flags, and `tcgetpgrp` tells whether the shell's process group owns the foreground. One read costs about 3 µs on macOS and under 1 µs on Linux. The server reads after it writes input and after focus changes. It also reads before it captures a frame when output arrived since its last read. Input, focus changes and output each schedule one more read 200 ms after the latest of them, because programs may print their prompt before they change the mode. A hidden Pane captures no frames, so the server reads its output at most once every 100 ms, the pace of a hidden Pane's metadata reports. The server pushes a Line Mode Pane event only when the mode changes. The event carries an epoch that increases by 1 at each change, and it goes before the frame captured with it. It carries flags only, never terminal contents. Clients derive hidden input from it, because hidden input is ICANON on with ECHO off.

The PTY spike and the real PTY tests measured these modes:

| Program | ICANON | ECHO | ISIG | Foreground |
|---|---|---|---|---|
| zsh's ZLE or bash's readline at the prompt | off | off | on | shell |
| bash's `read -sn`, zsh's `read -sk` | off | off | on | shell |
| python3 REPL, less | off | off | on | job |
| `read -s`, sudo, ssh-keygen passphrase | on | off | on | any |
| vim, `stty raw`, ssh session | off | off | off | any |
| dash at the prompt | on | on | on | shell |

The mode alone cannot tell the shell's line editor from python3 or less. The foreground can, while the shell runs each job in its own process group. With job control off (`set +m`), other programs share the shell's process group. Nothing the server reads tells the line editor from a shell builtin that reads silently in its mode, or from such a program.

The client predicts a key only when all of these hold:

- the key types one printable ASCII character with no Alt, Control or platform modifier;
- the latest Line Mode shows the line editor: ICANON off, ECHO off, ISIG on, and the shell in the foreground;
- the screen is the primary screen with a visible cursor, and the predicted cell is not in the last column;
- a frame reflects every input that suspended prediction.

Enter, a control key and any input other than a printable key suspend prediction until a frame reflects that input. Each suspension and each Line Mode change starts a new epoch. The predictions of an epoch stay hidden until the server's echo confirms one of them. A frame confirms the epoch when it shows a prediction in a cell that held something else when it was predicted, with the cursor right after that cell, and with the row to its left as the prediction expected it.

In the cases Line Mode cannot tell apart, safety rests on this echo confirmation and on the echo deadline below. A program that reads silently from the start never echoes, so its keys stay in a hidden epoch. A program can echo one key and then read silently, as a function that runs `read -sk1`, prints the key and runs `read -sk4` does. The server may never see the mode change between the reads, so the first echo confirms the epoch. A key typed after it shows until its echo is overdue. The client then drops every prediction, and later keys stay hidden until a frame reflects them and an echo confirms the line editor again. Keys typed within one round trip estimate and 100 ms of each other can therefore show for that long. No rule built on Line Mode and echoes can tell such a read from the line editor sooner.

The unconfirmed epoch also covers the window before a password prompt. sudo's prompt appears about 55 ms after Enter, and the client learns of the mode change one round trip later. A key typed in that window belongs to an unconfirmed epoch, which shows nothing. The Line Mode event precedes the frame captured with it. A program that prints its prompt before it changes the mode can still show the prompt in a frame whose Line Mode is the shell's, so in that window safety comes from the unconfirmed epoch alone. A prompt in the line editor's mode that starts with the typed character, at the cell where it was predicted, leaves the cursor after the prompt and does not confirm the epoch. A prompt confirms the epoch only if it prints exactly the typed character at the predicted cell, stops right after it, and leaves the row to its left unchanged.

Each frame reports the highest Input serial written to the PTY. A frame that covers a prediction confirms it when the predicted cell shows the predicted character. When the cell still holds what it held before, the echo may follow in a later frame. Each prediction's echo is due one round trip estimate and 100 ms after its key was sent. The server sends no frame for a key that changes nothing on screen, so the client drops every prediction at that deadline whether or not a frame arrived, and wakes for it only while predictions wait. A discarded Input message and anything else a frame shows is a misprediction, and it drops every prediction, as in mosh. The client measures the round trip from the start of sending an input that takes a new Input serial to the first frame that reflects it. It uses the smallest of the last 8 samples: each sample is at least one network round trip, and a key that changes nothing on screen yields a long one.

Predictions are an overlay. The client underlines them on its copy of the presented screen while the view follows the screen, and moves the cursor after them. The server's screen, the Scrollback Cache, Selection, copying, Terminal Find and assistive technology never see them. A lost connection drops them, and the client presents the last screen once more without them, so a frozen screen shows only what the server confirmed.

These limits follow from the rule:

- dash and other shells without a line editor read in cooked mode, so they get no prediction.
- Inside a nested ssh session the local PTY shows ssh in raw mode and the remote mode is invisible, so prediction stays off.
- Only ASCII is predicted, so wide and combining characters wait for the echo.

[ADR 0050](0050-push-terminal-presentation-over-one-multiplexed-connection.md) owns the other Pane events, and [ADR 0045](0045-freeze-terminal-screens-while-reconnecting.md) the frozen screen.
