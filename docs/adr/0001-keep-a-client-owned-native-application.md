# Keep a client-owned native application

SpaceTerm takes layout inspiration from tmux while the native application owns Workspace and
Terminal Session lifecycles rather than a separate server. New structural boundaries require a
durable replaceability or locality Seam, keeping ownership concentrated in deep Modules.
