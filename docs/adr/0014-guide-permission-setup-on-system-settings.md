# Guide Permission Setup on System Settings

Terminal programs that take screenshots or control other apps need Screen Recording and
Accessibility, and macOS grants both only through System Settings. macOS 27 renamed the
Accessibility list Device Control and Data Access, so the macOS adapter reports which name the
running system uses and every SpaceTerm label follows it; a person is never sent to a list their
System Settings does not show. SpaceTerm cannot grant them itself and must not change privacy
settings without a person. Permission Setup therefore opens the relevant privacy list and docks a
Setup Guide on System Settings' window, from which the person drags SpaceTerm into the list and
turns it on.

Permission Setup starts only from a person's action: Set Up in Settings, or Set Up on a Permission
Request notice in a Pane. Onboarding never starts it, and every permission stays off until the
person grants it.

SpaceTerm reads grants from a fresh child process of its own executable. The Screen Recording answer
in a running process keeps its launch value, so an in-process read cannot see a grant made during
the setup. At most one probe runs at a time. A Pane reads authorization when a request adds a
permission it has not seen, and again when its window becomes active while a request waits, because
the system reports no Screen Recording change.

Set Up clears the permission's entry with `tccutil` when a probe has just verified that it is not
granted. A stale entry from an earlier signature shows as present but grants nothing, and removing
it lets the person add SpaceTerm afresh. A verified grant and a failed probe never reset, and
cancelling the setup before its reset begins prevents the reset. Set Up therefore also recovers a
stale grant, so only an allowed grant offers Troubleshoot. Its alert resets only after the person
chooses Reset, then starts a Permission Setup. A reset names only the running identity's bundle
identifier. `tccutil` succeeds whether or not an entry existed, so the guide says that Set Up
removed any earlier entry rather than claiming it removed one.

The Setup Guide is a non-activating panel that follows System Settings' window by reading window
geometry and owners, which needs no permission. It shows only while System Settings is frontmost, so
it never covers another application, and it polls less often while System Settings is covered.
Locating the window and its content column is host code; placement within the column is portable.
Dragging SpaceTerm out of the guide shows a copy of the guide's row under the pointer, so the row
seems to move into the list. GPUI promotes the drag to a native file drag as it leaves the window,
and the SpaceTerm GPUI fork lets the caller supply that drag image; the host draws the row with
AppKit from the same measurements the guide renders.

A Permission Request is an OSC 7701 sequence that any terminal output can carry, including remote
programs and printed files. It can only offer the setup; the person decides. The notice never takes
keyboard focus, and its shortcuts are Command-Return and Command-Period rather than Return and
Escape. An enhanced keyboard mode can still send any key to a program, so the notice accepts no
click or shortcut until it has shown its offer for half a second. A permission added to the offer
starts that delay again, and so does a move to the other edge of the Pane, because the program moves
the cursor that picks the edge. A program therefore cannot redirect a click or keystroke meant for
itself by timing a request just before it or by moving the notice under the pointer. The filter
discards a request that any escape interrupts, where Ghostty's parser would dispatch the string it
holds, so a truncated request starts nothing and cannot hide later output.
