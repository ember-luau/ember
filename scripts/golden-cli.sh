#!/usr/bin/env bash
# G2: captures every scrap of CLI text lpm can print without touching the
# network, so a build-configuration change can be shown not to have moved any
# of it.
#
# The subcommand tree is walked, not hand-listed: `--help` is parsed for its own
# Commands: block and each entry recursed into. lpm has three levels (main.rs,
# then cache/index/patch/self/studio/tool, then tool's own nested group), and a
# hand-written list is exactly the thing that goes stale.
#
# Captured twice, once with colour forced on and once with NO_COLOR: those are
# different rendering paths, and the plain one is what CI and every piping user
# actually sees.
#
# WHAT THIS DOES NOT COVER, so nobody mistakes a pass for more than it is:
# interactive prompts (inquire/crossterm) are absent by design -- they would
# block on stdin; the shim and lpx dispatch paths in main.rs key off the
# executable's own filename and are never entered here; `self install`,
# `self code`, `studio open`, and `publish` appear only as help text, which
# means both of the crate's `unsafe` blocks are outside this gate; the
# indicatif progress bar hides itself when stderr is redirected, so its
# rendering is never exercised; and the hidden [scripts] shortcuts (build,
# test, start, serve, fmt) are not in any Commands: block, so the walk cannot
# reach their help pages.
#
# Output is redirected here, so the root help renders plain: the logo layout
# in main::print_root_help is for a terminal, deliberately.
#
# Usage:
#   scripts/golden-cli.sh --out .golden/before
#   ... change something, rebuild ...
#   scripts/golden-cli.sh --out .golden/after --baseline .golden/before
# Exits non-zero if --baseline is given and anything differs.

set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
exe="$here/../target/release/lpm"
out="$here/../.golden/current"
baseline=""
timeout_seconds=60

while [ $# -gt 0 ]; do
    case "$1" in
        --exe) exe=$2; shift 2 ;;
        --out) out=$2; shift 2 ;;
        --baseline) baseline=$2; shift 2 ;;
        --timeout) timeout_seconds=$2; shift 2 ;;
        -h | --help) sed -n '2,32p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

if [ ! -x "$exe" ]; then
    echo "No binary at $exe (cargo build --release first, or pass --exe)" >&2
    exit 1
fi
exe=$(cd "$(dirname "$exe")" && pwd)/$(basename "$exe")

mkdir -p "$out"
out=$(cd "$out" && pwd)

# A capture that overwrites its own baseline would delete the evidence and then
# compare the new run against itself -- a guaranteed, meaningless PASS.
if [ -n "$baseline" ]; then
    [ -d "$baseline" ] && baseline=$(cd "$baseline" && pwd)
    if [ "$out" = "$baseline" ]; then
        echo "--out and --baseline are the same directory ($out); that would erase the baseline" >&2
        exit 1
    fi
fi

rm -rf "$out"
mkdir -p "$out"

# Three sandboxes, because the error text depends on what is on disk and a
# single empty directory collapses most failures into "no manifest found".
sandbox_root="${TMPDIR:-/tmp}/lpm-golden-sandbox"
rm -rf "$sandbox_root"
mkdir -p "$sandbox_root/empty" "$sandbox_root/project" "$sandbox_root/broken"
# a valid manifest with no dependencies: reaches the errors that only fire once
# a manifest has been found and parsed
cat > "$sandbox_root/project/lpm.toml" <<'TOML'
[package]
name = "golden/fixture"
version = "0.1.0"

[target]
environment = "roblox"
TOML
# and one that does not parse, for the manifest-error path
printf '[package\nname = ' > "$sandbox_root/broken/lpm.toml"

# $TMPDIR is a symlink on macOS, and lpm canonicalizes some paths and not
# others, so both spellings of the sandbox get scrubbed.
sandbox_real=$(cd "$sandbox_root" && pwd -P)

# capture <mode> <name> <sandbox> <args...>
capture() {
    local mode=$1 name=$2 sandbox=$3
    shift 3
    local work="$sandbox_root/$sandbox"
    local stdout="$sandbox_root/_stdout.txt" stderr="$sandbox_root/_stderr.txt"

    local code=0
    # never hang the harness on a command that turns out to want stdin
    (cd "$work" && timeout "${timeout_seconds}s" "$exe" "$@" </dev/null) \
        >"$stdout" 2>"$stderr" || code=$?
    if [ "$code" = 124 ]; then
        echo "lpm $* did not exit within ${timeout_seconds}s" >&2
        exit 1
    fi

    local text
    text="\$ lpm $*   [$mode, sandbox: $sandbox]
exit: $code
--- stdout ---
$(cat "$stdout")
--- stderr ---
$(cat "$stderr")"

    # scrub anything machine-specific so two checkouts compare equal. literal
    # replacement, not sed: a path is full of characters a regex would read
    text=${text//"$sandbox_real"/<SANDBOX>}
    text=${text//"$sandbox_root"/<SANDBOX>}
    text=${text//"$exe"/<EXE>}
    text=${text//"$HOME"/<HOME>}

    mkdir -p "$out/$mode"
    printf '%s\n' "$text" > "$out/$mode/$name.txt"
    printf '%s\n' "$text"
}

# `Commands:` block of a help page, minus clap's own `help` entry. colour may
# be forced on, so the captured text is full of escape sequences; they stay in
# the golden file but must not end up in a filename.
subcommands() {
    sed -e 's/\x1b\[[0-9;]*[A-Za-z]//g' \
        | awk '
            /^Commands:/ { inblock = 1; next }
            !inblock { next }
            /^[[:space:]]*$/ { exit }
            /^[[:space:]][[:space:]]+[^[:space:]]/ { print $1 }
        ' \
        | grep -v '^help$' || true
}

# walk <mode> <path...>
walk() {
    local mode=$1
    shift
    local name="help"
    if [ $# -gt 0 ]; then
        name="help_$(IFS=_; echo "$*")"
    fi

    local text children child
    text=$(capture "$mode" "$name" empty "$@" --help)
    children=$(printf '%s\n' "$text" | subcommands)
    # a name is one word by construction, so word splitting is the iteration
    for child in $children; do
        walk "$mode" "$@" "$child"
    done
    # `for` over nothing, and `read` running out, both report failure, and this
    # function is called under `set -e`
    return 0
}

# Error paths, each pinned to the sandbox that reaches the code being tested.
# Six of these used to run in an empty directory and all print the same "no
# lpm.toml found" line; `tool install` used to be tested against a subcommand
# that does not exist (it is `tool add`), so it only ever exercised clap.
error_cases=(
    "err_unknown_command|empty|instal"
    "err_unknown_flag|empty|install --not-a-real-flag"
    "err_no_manifest|empty|install"
    "err_malformed_manifest|broken|install"
    "err_locked_no_lock|project|install --locked"
    "err_patch_no_spec|project|patch"
    "err_patch_bad_spec|project|patch notascope"
    "err_patch_bad_version|project|patch scope/name@^1"
    "err_patch_no_copy|project|patch commit scope/name"
    "err_patch_remove_none|project|patch remove scope/name"
    "err_run_missing|project|run no-such-script"
    "err_tool_bad_spec|project|tool add not-a-repo"
    "err_tool_unknown_sub|empty|tool install x"
    "err_execute_bad_spec|empty|execute @@@"
    "err_publish_none|empty|publish"
)

# Colour is part of the output being gated (clap's `color` feature is one of the
# things a size-minded person is tempted to trim), and auto-detection keys off
# whether stderr is a terminal -- which differs between a local run and CI. So
# both renderings are forced explicitly rather than left to detection. Set per
# command rather than exported, so nothing leaks into whatever the caller runs
# next: an install-fixture run in the same shell would render differently.
for mode in color plain; do
    if [ "$mode" = color ]; then
        export CLICOLOR_FORCE=1
        unset NO_COLOR
    else
        unset CLICOLOR_FORCE
        export NO_COLOR=1
    fi

    echo "Capturing $mode ..."
    walk "$mode" > /dev/null
    capture "$mode" version empty --version > /dev/null
    for case in "${error_cases[@]}"; do
        IFS='|' read -r name box args <<< "$case"
        # shellcheck disable=SC2086 # the arguments are deliberately split
        capture "$mode" "$name" "$box" $args > /dev/null
    done
done
unset CLICOLOR_FORCE NO_COLOR

rm -rf "$sandbox_root"
captured=$(find "$out" -name '*.txt' -type f | wc -l)
echo "Captured $captured files to $out"

[ -z "$baseline" ] && exit 0

# --- comparison ---
differences=()
before=$(cd "$baseline" && find . -name '*.txt' -type f | LC_ALL=C sort)
after=$(cd "$out" && find . -name '*.txt' -type f | LC_ALL=C sort)

for name in $before; do
    if ! printf '%s\n' "$after" | grep -qxF "$name"; then
        differences+=("MISSING in new capture: ${name#./}")
    fi
done

for name in $after; do
    if ! printf '%s\n' "$before" | grep -qxF "$name"; then
        differences+=("ADDED in new capture: ${name#./}")
    fi
done

for name in $before; do
    printf '%s\n' "$after" | grep -qxF "$name" || continue
    # cmp, byte for byte: this gate's whole job is identity, and a comparison
    # that folded case would let `Installed` -> `installed` sail through
    cmp -s "$baseline/$name" "$out/$name" || differences+=("CHANGED: ${name#./}")
done

if [ ${#differences[@]} -eq 0 ]; then
    echo "G2 PASS: $captured captures identical to $baseline"
    exit 0
fi
echo "G2 FAIL:"
printf '  %s\n' "${differences[@]}"
exit 1
