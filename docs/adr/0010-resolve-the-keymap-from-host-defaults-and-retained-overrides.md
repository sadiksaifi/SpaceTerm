# Resolve the Keymap from host defaults and retained overrides

Settings retains only overrides so host default changes reach Commands a person has not changed.
An override unavailable on the current host remains retained but inactive, preserving a document
carried between platforms. Modifiers and symbols remain literal because resolving them while
parsing would bind the document to one keyboard layout; layout changes refresh the Keymap instead.
