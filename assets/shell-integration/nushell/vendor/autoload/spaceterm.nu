export module spaceterm {
  export def --env install [] {
    if (($env.SPACETERM_SHELL_INTEGRATION_VERSION? | default "") != "1") { return }
    # This runs after the user's configuration set PATH, so the server's own CLI goes first.
    if "SPACETERM_CLI" in $env {
      let dir = ($env.SPACETERM_CLI | path dirname)
      $env.PATH = ($env.PATH | split row (char esep) | where {|entry| $entry != $dir } | prepend $dir)
    }
    # The session's Prompt Owner marks this shell's own prompt markers; only hex is accepted.
    let owner = ($env.SPACETERM_PROMPT_OWNER? | default "")
    let owner = if ($owner =~ '^[0-9a-f]+$') { $";spaceterm=($owner)" } else { "" }
    # A record is never exported, so commands do not inherit the pending finish.
    let prompt_hook = {||
      let finished = ($env._SPACETERM_COMMAND? | default { started: false }).started
      $env._SPACETERM_COMMAND = { started: false }
      let finish = if $finished { $"\u{1b}]133;D;($env.LAST_EXIT_CODE)($owner)\u{7}" } else { "" }
      print -n $"($finish)\u{1b}]7;file://localhost($env.PWD | url encode --all | str replace --all '%2F' '/')\u{7}\u{1b}]133;A;redraw=1($owner)\u{7}"
    }
    let command_hook = {||
      $env._SPACETERM_COMMAND = { started: true }
      print -n $"\u{1b}]133;B\u{7}\u{1b}]133;C($owner)\u{7}"
    }
    # Nushell's own prompt markers carry no Prompt Owner and would count every command twice.
    $env.config.shell_integration.osc133 = false
    $env.config.hooks.pre_prompt = (($env.config.hooks.pre_prompt? | default []) | append $prompt_hook)
    $env.config.hooks.pre_execution = (($env.config.hooks.pre_execution? | default []) | append $command_hook)
  }
}

if "SPACETERM_SHELL_INTEGRATION_XDG_DIR" in $env {
  $env.XDG_DATA_DIRS = ($env.XDG_DATA_DIRS | str replace $"($env.SPACETERM_SHELL_INTEGRATION_XDG_DIR):" "")
  hide-env SPACETERM_SHELL_INTEGRATION_XDG_DIR
}
