# Dev server management commands for the green project.
#
# The server is launched via `setsid --fork`, creating a new OS process session
# that outlives the calling nushell session. This means `green start` and
# `green restart` work identically whether called from a Zellij pane or the
# Claude Code Bash tool.
#
# Typical workflow:
#   nu scripts/dev.nu          — start server and tail errors (from Zellij)
#   green restart              — rebuild and restart after making changes
#   green stop                 — shut the server down
#
# Log files (both truncated on each start):
#   logs.ndjson   — structured JSON tracing output from the green binary (stdout)
#   errors.log    — cargo build output, panics, and server stderr

const LOCAL_ADDRESS = "http://localhost:10000"

# Records server startup metadata between commands.
export const watch_file = "./.watch_state.toml"

# Structured JSON tracing output from the running server (stdout).
export const log_file = "./logs.ndjson"

# Cargo build output and server stderr — panics, errors, warnings.
export const error_log_file = "./errors.log"

# Print a timestamped status line to the terminal.
export def log [message: string] {
    print $"(date now | format date '%Y-%m-%dT%H:%M:%S%z') ($message)"
}

# Load dev credentials from secrets.toml if the file exists.
# Returns a record suitable for `load-env`; returns {} if the file is absent.
# To set up: copy secrets.toml.example → secrets.toml and fill in credentials.
export def "load-secrets" [] {
    let secrets_path = "secrets.toml"
    if ($secrets_path | path exists) {
        open $secrets_path
    } else {
        {}
    }
}

# Verify that `setsid` is available on PATH and error with a helpful message if not.
# `setsid` is provided by util-linux, which is standard on NixOS.
def "require-setsid" [] {
    if (which setsid | is-empty) {
        error make {
            msg: "`setsid` not found on PATH — it is required to detach the server process"
            help: "On NixOS, add pkgs.util-linux to environment.systemPackages"
        }
    }
}

# Return the absolute path to the inner server runner script.
def "server-script-path" [] {
    $env.PWD | path join "scripts" "server.nu"
}

# Run the server in the foreground. Useful for interactive debugging where you
# want stdout/stderr to appear directly in the terminal rather than log files.
export def "green run" [
    --config-path: path = "./config.dev.toml"  # config file to use
] {
    let abs_config = ($config_path | path expand)
    load-env (load-secrets)
    log $"running foreground server with config: ($abs_config)"
    run-external "cargo" "run" "--" "--config-path" $abs_config
}

# Build and start the server as a detached background process.
#
# Uses `setsid --fork` to place the server in a new OS process session, making it
# independent of the calling nushell session. Unlike `job spawn`, the process
# survives when the calling session exits.
#
# Credentials are inherited from the current environment, so `load-env (load-secrets)`
# is called here before forking — the child inherits the result.
#
# Both log files are truncated on each start so `tail -f` always shows the current run.
export def "green start" [
    --config-path: path = "./config.dev.toml"  # config file to use
] {
    require-setsid

    let abs_config = ($config_path | path expand)

    if not ($abs_config | path exists) {
        error make { msg: $"config file not found: ($abs_config)" }
    }

    # Stop any running instance first so we don't have two servers fighting over the port.
    if ($watch_file | path exists) {
        log "stopping existing instance before starting"
        green stop --config-path $config_path
    }

    # Truncate both log files so `tail -f` always reflects the current run.
    "" | save --force $log_file
    "" | save --force $error_log_file

    # Set credentials in the current env before forking so the child inherits them.
    load-env (load-secrets)

    let script = (server-script-path)
    log $"starting server with config: ($abs_config)"

    # --fork: setsid forks a child into a new session and exits immediately.
    # The child (server.nu → cargo run → green binary) is then adopted by PID 1
    # and runs independently of this nushell session.
    ^setsid --fork nu --no-config-file $script $abs_config

    { config_path: $abs_config, started_at: (date now | format date '%Y-%m-%dT%H:%M:%S%z') }
        | to toml
        | save --force $watch_file

    log "server started — watching logs.ndjson and errors.log"
}

# Stop the running server by sending SIGTERM to all matching processes.
#
# Matches against the absolute config file path in the full command line,
# which identifies both `cargo run` and the compiled `green` binary.
# Because it targets OS-level processes by argument pattern rather than
# nushell job IDs, this works correctly across different nushell sessions.
export def "green stop" [
    --config-path: path = "./config.dev.toml"  # config file used when starting the server
] {
    let abs_config = ($config_path | path expand)

    # pkill --full matches against each process's complete argument list.
    # The absolute config path is unique enough to identify our server processes
    # without risking false positives.
    let result = (do { ^pkill --signal SIGTERM --full $abs_config } | complete)

    if $result.exit_code == 0 {
        log "server stopped"
    } else {
        log "no running server found matching this config path"
    }

    if ($watch_file | path exists) {
        rm $watch_file
    }
}

# Stop the running server and start a fresh one.
# Equivalent to `green stop` followed by `green start`.
export def "green restart" [
    --config-path: path = "./config.dev.toml"  # config file to use
] {
    green stop --config-path $config_path
    # Brief pause to let the OS release the listen port before restarting.
    sleep 500ms
    green start --config-path $config_path
}

# Fetch the index page from the running dev server.
export def "green index" [
    address: string@"complete-addresses" = $LOCAL_ADDRESS
] {
    http get $address
}

# Fetch the CA certificate from the running dev server.
export def "green ca" [
    address: string@"complete-addresses" = $LOCAL_ADDRESS
] {
    http get $"($address)/api/ca"
}

def "complete-addresses" [] {
    [$LOCAL_ADDRESS]
}
