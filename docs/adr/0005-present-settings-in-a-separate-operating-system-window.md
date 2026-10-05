# Present Settings in a separate Operating-System Window

Settings has a modeless Operating-System Window because its application-scoped document must
remain reachable without a Workspace. The separate window also lets a person watch a live terminal
while adjusting its appearance. A Workspace-owned panel would tie Settings to one Workspace's
lifetime and cover the output being configured; a modal would prevent that interleaving.
