# Inner server runner. Launched detached by `green start` via setsid --fork.
# Inherits environment (including GREEN_DB_URL) from the parent process.
# Do not run directly — use `green start` or `green restart` instead.
#
# All paths are derived from $env.CURRENT_FILE (this script's absolute location)
# so the script is correct regardless of the working directory it was launched from.

use green.nu *

def main [config_path: path] {
    # Derive the project root from this script's location: scripts/server.nu → project root.
    # Using an absolute base ensures paths are stable even if the inherited CWD is wrong.
    let project_root = ($env.CURRENT_FILE | path dirname | path dirname)
    let abs_log_file = ($project_root | path join "logs" "logs.ndjson")
    let abs_error_log_file = ($project_root | path join "logs" "errors.log")

    # Change to the project root so that `cargo run` finds Cargo.toml and any
    # relative paths in the binary's own config resolve correctly.
    cd $project_root

    # Load dev credentials as a safety net in case the parent did not export them.
    # Under normal operation `green start` already loads them before forking,
    # so the child inherits GREEN_DB_URL via the environment — this is a fallback.
    load-env (load-secrets)

    # Run `cargo run` with full output redirection.
    # stdout  → structured JSON tracing (logs.ndjson)
    # stderr  → build output, panics, and server stderr (errors.log)
    run-external "cargo" "run" "--" "--config-path" $config_path o>> $abs_log_file e>> $abs_error_log_file
}
