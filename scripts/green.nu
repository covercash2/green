# helper functions for green

const LOCAL_ADDRESS = "http://localhost:10000"

export const watch_file = "./.watch_state.toml"

# Print a timestamped message to the terminal
export def log [msg: string] {
  print $"(date now | format date '%Y-%m-%dT%H:%M:%S%z') ($msg)"
}

# Build the cargo run argument list
def "green args" [config_path: path] {
  [cargo run -- --config-path $config_path]
}

# Run the server in the foreground (interactive use)
export def "green run" [
  config_path: path = "./config.dev.toml"
] {
  let args = (green args $config_path)
  log $"running: ($args | str join ' ')"
  run-external ...$args
}

# Spawn the server as a background job.
# stdout (JSON tracing) is appended to logs.ndjson unmodified.
# stderr (cargo build output) is timestamped and printed to the terminal.
export def "green start" [
  config_path: path = "./config.dev.toml"
] {
  # Build args in outer scope so the closure can capture them.
  # run-external is used inside the job instead of green run because
  # custom commands are not in scope inside job spawn closures.
  let args = (green args $config_path)
  log $"running: ($args | str join ' ')"
  let job = (job spawn {
  run-external ...$args o>> logs.ndjson e>| each { |line|
    log $line
  } | print
  })
  log $"server started: ($job)"
  {job_id: $job, updated_at: (date now | format date '%Y-%m-%dT%H:%M:%S%z')} | to toml | save --force $watch_file
  $job
}

export def "green index" [
  address: string@addresses = $LOCAL_ADDRESS
] {
  http get $address
}

export def "green ca" [
  address: string@addresses = $LOCAL_ADDRESS
] {
  http get $"($address)/ca"
}

def addresses [] {
  [$LOCAL_ADDRESS]
}
