# Dev watch script — rebuilds and restarts the server on file changes.
#
# Watches for changes to .rs, .toml, .nu, .css, .html, .md, .json, and .js files. When a
# change is detected, the server is restarted. If a .nu script changes, the
# entire dev environment is reloaded via exec so green.nu is re-imported.
# Server stdout (JSON tracing) is appended to logs.ndjson unmodified.
# Server stderr (cargo build output) is timestamped to the terminal.
#
# Usage: nu scripts/dev.nu

use green.nu *

green start

watch . --debounce 5sec {|op, path, new_path|
  let files = glob "**/*.{rs,toml,nu,css,html,md,json,js}"
    | each { path expand }
    | where { |f| $f != ($watch_file | path expand) }
  if ($path in $files) {
    log $"file modified: ($path)"

    # Kill the current server using the job ID from the watch file
    if ($watch_file | path exists) {
      let state = open $watch_file
      log $"killing job: ($state.job_id)"
      job kill $state.job_id
    }

    # Hot reload: .nu changes require re-importing green.nu, so replace
    # this process entirely with a fresh dev.nu instead of just restarting
    # the server
    if (($path | path parse | get extension) == "nu") {
      log "script changed — reloading dev environment"
      exec nu scripts/dev.nu
    }

    log "restarting server"
    green start
  }
}
