# Resolve the Keymap from host defaults and retained overrides

Settings retain only Keymap overrides so host default changes reach Commands a person has not changed. Modifiers and symbols stay literal in the Settings Document because resolving them while parsing would bind it to one keyboard layout.

The application command palette uses a standard application action and a fixed Shortcut, Command-Shift-P on macOS and Control-Shift-P on Linux. Its Add Server entry dispatches the same action as the application menu and Settings. These actions have no retained Command identifier. A conflicting older override remains retained and inactive under the existing System Reserved rule.
