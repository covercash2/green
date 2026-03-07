# Dev watch script — rebuilds and restarts the server on file changes.
#
# Watches all .rs, .toml, .nu, .css, and .html files. When a change is
# detected, the current server job is killed and a new one is spawned.
# Server output is appended to logs.ndjson.
#
# Usage: nu scripts/dev.nu

use green.nu *

watch . --debounce 5sec {|op, path, new_path|
  print $"File modified: ($path)"

  let files = glob "**/*.{rs,toml,nu,css,html}"
  if ($files | any {|file| $file == $path }) {
    print "Restarting server..."

    # Kill the previous server job if one is running
    if ($env | get --optional current_job) != null {
      print $"killing job: ($env.current_job)"
      job kill $env.current_job
    }

    # Spawn a new server job and track it for the next restart
    let new_job = (job spawn { green run | save --append logs.ndjson })
    $env.current_job = $new_job

    print $"new job started: ($new_job)"
  }
}
