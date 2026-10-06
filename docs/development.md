# Build from source

Install [mise](https://mise.jdx.dev/), then clone and trust the repository:

```sh
git clone https://github.com/sadiksaifi/SpaceTerm.git
cd SpaceTerm
mise trust
mise run setup
mise run development
```

[`.mise.toml`](../.mise.toml) defines host checks, tools, packages, and commands.
`mise run setup` installs Linux packages on Debian and Ubuntu only; on other distributions, install their equivalents.
Use `mise tasks` to discover tasks and `mise doctor project` to diagnose the host.
