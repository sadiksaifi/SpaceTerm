{
use str

if (not-eq $E:SPACETERM_SHELL_INTEGRATION_VERSION 1) { return }

if (has-env SPACETERM_SHELL_INTEGRATION_XDG_DIR) {
  set-env XDG_DATA_DIRS (str:replace $E:SPACETERM_SHELL_INTEGRATION_XDG_DIR":" "" $E:XDG_DATA_DIRS)
  unset-env SPACETERM_SHELL_INTEGRATION_XDG_DIR
}

fn spaceterm-encode {|value|
  for byte [(str:to-utf8-bytes $value)] {
    if (== (num $byte) 47) {
      printf /
    } else {
      printf '%%%02X' (num $byte)
    }
  }
}

fn spaceterm-prompt {
  printf "\e]7;file://localhost%s\a\e]133;A\a" (spaceterm-encode $pwd)
}
fn spaceterm-command {|_| printf "\e]133;B\a\e]133;C\a" }
fn spaceterm-finished {|info|
  var status = 0
  if (not-eq $nil $info[error]) { set status = 1 }
  printf "\e]133;D;"$status"\a"
}

set edit:before-readline = (conj $edit:before-readline $spaceterm-prompt~)
set edit:after-readline = (conj $edit:after-readline $spaceterm-command~)
set edit:after-command = (conj $edit:after-command $spaceterm-finished~)
}
