# Dev startup script — starts the server and tails logs/errors.log.
# Use `green restart` (or I can run it via the Bash tool) to restart after changes.
#
# Usage: nu scripts/dev.nu

use green.nu *

green start

(tail -f logs/errors.log
  | lines
  | each {|line| log $"[logs/errors.log] {line}"}
)
