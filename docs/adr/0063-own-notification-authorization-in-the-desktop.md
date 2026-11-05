# Own notification authorization in the desktop

The desktop posts every SpaceTerm notification. It posts a terminal attention notification when a bell or a finished command reaches a Pane without Terminal Input Focus while SpaceTerm is in the background, and it posts Update Reminders. Programs in a Terminal Session post nothing through SpaceTerm. They reach a notification only by ringing the bell or finishing a command, and its body carries only a count. Notification authorization therefore belongs to the desktop's own bundle, not to the Permission Holder of [ADR 0061](0061-own-system-permissions-in-the-server-package.md). SpaceTerm Core and the SpaceTerm CLI neither read, request nor post notifications.

## macOS

The desktop reads and requests authorization through `UNUserNotificationCenter` under its own bundle identifier. Delivery asks for provisional authorization the first time it needs one, so notifications start in Notification Center without a prompt. [The notification adapter](../../src/platform/macos_notification.rs) owns both calls.

Settings > Privacy > Notifications presents the desktop's authorization with one action per state:

- **Not requested.** Request Access… asks for alerts, which shows the system prompt.
- **Delivered quietly, denied, or with alerts and Notification Center both off.** Open System Settings opens the desktop's own entry in Notifications. Quiet delivery is provisional authorization: macOS shows those notifications only in Notification Center.
- **A read that failed.** Check Again reads the authorization again.

Settings reads the authorization again whenever its window becomes active, because the person changes it in System Settings.

## Linux

Linux has no per-application notification authorization. The desktop posts through `org.freedesktop.Notifications` on the session bus. The row reports whether a notification service answers `GetServerInformation`, and offers Check Again when none does.

## Client Setting

"Terminal attention notifications" is a Client Setting in the Settings Document. Turning it off stops terminal attention notifications and removes one already shown. Bells and dock attention keep their own behavior. Update Reminders do not follow it, because an Overdue Update relies on them.
