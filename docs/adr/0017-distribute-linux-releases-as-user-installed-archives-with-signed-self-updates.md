# Distribute Linux releases as user-installed archives with signed self-updates

Linux releases ship as one x86_64 archive that the shared installer unpacks into the user's own directories, so SpaceTerm can replace itself without root and without a distribution package for each desktop. The archive is built against glibc 2.35, which excludes musl and older distributions. Updates verify a signed per-platform feed and archive with the macOS update key, keeping one trust root across platforms.
