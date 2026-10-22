# Own status controls in the desktop client

The desktop SpaceTerm Client owns its windows and native status item in one process. The menu bar/status area is part of that client, with no separately installed status application or helper registration. The installed Preflight and Release SpaceTerm Server remains independently supervised and headless under [ADR 0030](0030-start-servers-at-boot-independently-of-clients.md).

Command-Q and equivalent application Quit requests present a native three-choice alert: "Keep in Menu Bar" (default), "Quit SpaceTerm", and "Cancel". Linux uses its host's status-area terminology. Keep closes every client window and removes Dock presence while keeping the same client running in the menu bar or status area. Quit exits the client and removes its status item. Cancel leaves the application unchanged. The alert explains these consequences.

Native menus expose "Close All Windows" and "Quit SpaceTerm". Close All Windows performs the Keep choice directly. Completing an explicit Quit choice exits without presenting the chooser again. Opening SpaceTerm restores the existing client's windowed presentation. For installed Preflight and Release, both dismissal choices leave the independent server and its Terminal Sessions running; Stop Server remains a distinct confirmed operation.

An installed client crash may remove the status item but leaves server-owned work running. The supported service-control fallback remains usable without the client or a status-area host. A later desktop installation reuses an existing headless installation under [ADR 0033](0033-reuse-one-server-across-headless-and-desktop-installations.md). The future general CLI in [issue #596](https://github.com/sadiksaifi/SpaceTerm/issues/596) records intent only and does not expand this control surface.

For source Development, the development command owns the complete run under [ADR 0030](0030-start-servers-at-boot-independently-of-clients.md). Closing windows keeps that run available in the status area. Complete client exit or ending the command ends its foreground Server and Terminal Sessions.
