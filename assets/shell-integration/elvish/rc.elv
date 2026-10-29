{
use spaceterm-integration
use os
use runtime
use str

# Elvish resolves the user's rc path independently of -rc, including XDG_CONFIG_HOME.
# Load it as a file so its relative imports keep their original source directory.
if (and (not-eq $runtime:rc-path $nil) (os:exists $runtime:rc-path)) {
  # Module specs must be relative; enough parents reach the filesystem root.
  var root = (str:repeat '../' (count [(str:split / $runtime:effective-rc-path)]))
  var user-ns = (use-mod $root(str:trim-suffix $runtime:rc-path .elv))
  for name [(keys $user-ns)] {
    edit:add-var $name $user-ns[$name]
  }
}
}
