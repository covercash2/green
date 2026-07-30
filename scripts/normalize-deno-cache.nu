#!/usr/bin/env nu
# Normalize a DENO_DIR produced by `deno cache` so it hashes identically
# across independent fetches of the same dependencies. Used to make the
# `denoPackageCache` fixed-output derivation in flake.nix reproducible.
# Without this, three sources of non-determinism make the raw cache
# unusable as a fixed-output derivation input:
#
#  1. Cached remote source files (remote/https/**) carry a trailing
#     `// denoCacheMetadata={"headers":{...},"time":<epoch>}` comment with
#     the full HTTP response headers, including per-request/CDN fields
#     (date, age, cf-ray, set-cookie, report-to) and a fetch timestamp.
#  2. npm registry metadata (npm/**/registry.json) is served with
#     non-deterministic JSON key ordering by the registry.
#  3. Deno's own internal analysis-cache SQLite databases
#     (dep_analysis_cache_v2*, node_analysis_cache_v2*) are local
#     incremental-compilation caches, not dependency content, and are
#     never byte-identical between separate runs.

def "sort-deep" [] {
    let v = $in
    let t = ($v | describe -d | get type)
    if $t == "record" {
        $v | transpose key val | sort-by key | reduce -f {} {|row, acc| $acc | insert $row.key ($row.val | sort-deep) }
    } else if $t == "list" {
        $v | each {|it| $it | sort-deep }
    } else {
        $v
    }
}

def main [deno_dir: string] {
    # (3) Drop internal analysis caches. Not real dependencies -- deno
    # regenerates them on demand from the (now-normalized) source cache.
    for f in (glob ($deno_dir | path join "dep_analysis_cache_v2*")) { rm -f $f }
    for f in (glob ($deno_dir | path join "node_analysis_cache_v2*")) { rm -f $f }

    # (2) Canonicalize npm registry metadata key ordering.
    let npm_dir = ($deno_dir | path join "npm")
    if ($npm_dir | path exists) {
        for f in (glob ($npm_dir | path join "**" "registry.json")) {
            open --raw $f | from json | sort-deep | to json -r | save --raw --force $f
        }
    }

    # (1) Strip volatile HTTP headers from denoCacheMetadata trailers. Only
    # an allowlist of headers deno actually needs (module-kind / redirect /
    # type resolution) survives; everything else (etag, cache-control,
    # CORS, CDN tracing headers, ...) is dropped rather than chasing every
    # CDN's set of volatile field names.
    let remote_dir = ($deno_dir | path join "remote")
    if ($remote_dir | path exists) {
        let allowed = ["content-type" "x-typescript-types" "location"]
        for f in (glob ($remote_dir | path join "**" "*") --no-dir) {
            let content = (open --raw $f)
            let parts = ($content | split row "\n")
            let last_line = ($parts | last)
            if ($last_line | str starts-with "// denoCacheMetadata=") {
                # Byte-exact prefix (everything but the last line) -- this
                # can't corrupt body content the way a `head -n -1`-through-
                # command-substitution reconstruction can (which silently
                # swallows a trailing blank line), and that body is exactly
                # what deno.lock's integrity hash is checked against.
                let prefix = (($parts | drop 1 | str join "\n") + "\n")
                let json = ($last_line | str substring 21.. | from json)
                let headers = (
                    $json.headers | transpose key val | where key in $allowed
                        | reduce -f {} {|r, acc| $acc | insert $r.key $r.val }
                )
                let normalized = ($json | update headers $headers | update time 0 | to json -r)
                $"($prefix)// denoCacheMetadata=($normalized)" | save --raw --force $f
            }
        }
    }
}
