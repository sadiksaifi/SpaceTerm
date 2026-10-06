# Build from source

Install [mise](https://mise.jdx.dev/), then prepare a clone:

```sh
git clone https://github.com/sadiksaifi/SpaceTerm.git
cd SpaceTerm
mise trust
mise run setup
```

[`.mise.toml`](../.mise.toml) owns the tools, host checks, packages, and tasks; `mise tasks` lists the tasks.
On Linux distributions other than Debian and Ubuntu, install equivalents of its packages.
