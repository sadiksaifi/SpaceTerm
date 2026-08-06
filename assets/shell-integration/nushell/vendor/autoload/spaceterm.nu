export module spaceterm {
  export def --env install [] {
    if (($env.SPACETERM_SHELL_INTEGRATION_VERSION? | default "") != "1") { return }
    let prompt_hook = {|| print -n $"\u{1b}]7;file://localhost($env.PWD)\u{7}\u{1b}]133;A\u{7}" }
    let command_hook = {|| print -n "\u{1b}]133;B\u{7}\u{1b}]133;C\u{7}" }
    $env.config.hooks.pre_prompt = (($env.config.hooks.pre_prompt? | default []) | append $prompt_hook)
    $env.config.hooks.pre_execution = (($env.config.hooks.pre_execution? | default []) | append $command_hook)
  }
}

if "SPACETERM_SHELL_INTEGRATION_XDG_DIR" in $env {
  $env.XDG_DATA_DIRS = ($env.XDG_DATA_DIRS | str replace $"($env.SPACETERM_SHELL_INTEGRATION_XDG_DIR):" "")
  hide-env SPACETERM_SHELL_INTEGRATION_XDG_DIR
}
