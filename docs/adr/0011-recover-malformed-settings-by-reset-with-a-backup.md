# Recover Malformed Settings by reset with a backup

Settings Recovery resets malformed content while retaining its exact bytes in one backup for
inspection or manual restoration. Storage failures and competing writers remain separate because
resetting them could destroy content SpaceTerm never saw. Keeping only the latest backup bounds
what recovery leaves in the configuration directory.
