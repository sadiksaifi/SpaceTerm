# Recover Malformed Settings by reset with a backup

A `settings.json` that SpaceTerm cannot read as a valid Settings Document leaves the person with default Settings and no way to edit them until the file is fixed by hand. Settings Recovery gives them one action inside the app: Reset Settings. SpaceTerm offers it in an alert in the first Workspace window at launch and in the Settings window banner.

Only Malformed Settings are recoverable: a file that does not parse, fails validation, or exceeds the size limit. A storage failure is different. When the file is unsafe, unavailable, or changed by another writer, the file may be fine, and replacing it could destroy content SpaceTerm never saw. Those states keep editing paused until an explicit reload.

The reset renames the unreadable file to `settings.json.bak` beside it and replaces any older backup. It never parses or presents the old bytes, so the backup is an exact copy and no terminal or secret content reaches the interface or Local Diagnostics. The rename happens inside the verified configuration directory with the same identity checks as every other private write.

The default document is then written with the expectation that no file exists. If another writer created `settings.json` between the rename and the write, the reset reports a conflict and keeps the backup rather than replacing a file this session did not see.

One backup is enough. Recovery is a rare repair, the backup is for inspection or manual restoration, and keeping only the latest one bounds what SpaceTerm leaves in the configuration directory.
