# Dev startup script — starts the server and tails errors.log.
# Use `green restart` (or I can run it via the Bash tool) to restart after changes.
#
# Usage: nu scripts/dev.nu

use green.nu *

green start
tail -f errors.log
