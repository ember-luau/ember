# G2: captures every scrap of CLI text lpm can print without touching the
# network or the filesystem, so a build-configuration change can be shown not to
# have moved any of it.
#
# The subcommand tree is walked, not hand-listed: `--help` is parsed for its own
# Commands: block and each entry recursed into. lpm has three levels (main.rs,
# then cache/index/patch/self/studio/tool, then tool's own nested group), and a
# hand-written list is exactly the thing that goes stale.
#
# Usage:
#   .\scripts\golden-cli.ps1 -Out .golden\before
#   ... change something, rebuild ...
#   .\scripts\golden-cli.ps1 -Out .golden\after -Baseline .golden\before
# Exits non-zero if -Baseline is given and anything differs.

param(
    [string]$Exe = (Join-Path $PSScriptRoot "..\target\release\lpm.exe"),
    [string]$Out = (Join-Path $PSScriptRoot "..\.golden\current"),
    [string]$Baseline = ""
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path $Exe)) {
    Write-Error "No binary at $Exe (cargo build --release first, or pass -Exe)"
    exit 1
}
$Exe = (Resolve-Path $Exe).Path

Remove-Item $Out -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $Out | Out-Null

# Every capture runs here: an empty directory with no lpm.toml, so filesystem
# state cannot leak into the text. The path is fixed and scrubbed from output.
# Colour is part of the output being gated (clap's `color` feature is one of the
# things a size-minded person is tempted to trim), but auto-detection keys off
# whether stderr is a terminal -- which differs between a local run and CI.
# Force it, so the captures compare across environments.
$env:CLICOLOR_FORCE = "1"
if (Test-Path Env:\NO_COLOR) { Remove-Item Env:\NO_COLOR }

$sandbox = Join-Path $env:TEMP "lpm-golden-sandbox"
Remove-Item $sandbox -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $sandbox | Out-Null

function Capture([string]$name, [string[]]$arguments) {
    $stdout = Join-Path $sandbox "_stdout.txt"
    $stderr = Join-Path $sandbox "_stderr.txt"
    $process = Start-Process -FilePath $Exe -ArgumentList $arguments `
        -WorkingDirectory $sandbox -NoNewWindow -Wait -PassThru `
        -RedirectStandardOutput $stdout -RedirectStandardError $stderr
    $text = @(
        "$ lpm $($arguments -join ' ')",
        "exit: $($process.ExitCode)",
        "--- stdout ---",
        (Get-Content $stdout -Raw),
        "--- stderr ---",
        (Get-Content $stderr -Raw)
    ) -join "`n"
    # scrub anything machine-specific so two checkouts compare equal
    $text = $text.Replace($sandbox, "<SANDBOX>").Replace($Exe, "<EXE>")
    $text = $text -replace [regex]::Escape($env:USERPROFILE), "<HOME>"
    [IO.File]::WriteAllText((Join-Path $Out "$name.txt"), $text)
    return $text
}

# `Commands:` block of a help page, minus clap's own `help` entry.
function Subcommands([string]$helpText) {
    $names = @()
    $inBlock = $false
    # colour is forced on above, so the captured text is full of escape
    # sequences; they stay in the golden file but must not end up in a filename
    $plain = $helpText -replace "\x1b\[[0-9;]*[A-Za-z]", ""
    foreach ($line in $plain -split "`n") {
        $line = $line.TrimEnd()
        if ($line -match '^Commands:') { $inBlock = $true; continue }
        if (-not $inBlock) { continue }
        # the block ends at the first blank line following it
        if ($line.Trim() -eq "") { break }
        if ($line -match '^\s{2,}(\S+)') {
            $name = $Matches[1]
            if ($name -ne "help") { $names += $name }
        }
    }
    return $names
}

function Walk([string[]]$path) {
    $name = if ($path.Count -eq 0) { "help" } else { "help_" + ($path -join "_") }
    $text = Capture $name ($path + "--help")
    foreach ($child in Subcommands $text) {
        Walk ($path + $child)
    }
}

Write-Output "Capturing help tree..."
Walk @()
Capture "version" @("--version") | Out-Null

# Error paths, chosen to stay off the network and write nothing. Interactive
# commands (`init`) are deliberately absent: they would block on a prompt.
Write-Output "Capturing error paths..."
$errorCases = @{
    "err_unknown_command"   = @("instal")                      # clap suggestions
    "err_unknown_flag"      = @("install", "--not-a-real-flag")
    "err_no_manifest"       = @("install")
    "err_locked_no_lock"    = @("install", "--locked")
    "err_patch_no_spec"     = @("patch")
    "err_patch_bad_spec"    = @("patch", "notascope")
    "err_patch_bad_version" = @("patch", "scope/name@^1")
    "err_patch_no_copy"     = @("patch", "commit", "scope/name")
    "err_patch_remove_none" = @("patch", "remove", "scope/name")
    "err_add_no_manifest"   = @("add", "scope/name")
    "err_publish_none"      = @("publish")
    "err_run_missing"       = @("run", "no-such-script")
    "err_tool_bad_spec"     = @("tool", "install", "not-a-repo")
    "err_execute_bad_spec"  = @("execute", "@@@")
}
foreach ($case in $errorCases.GetEnumerator() | Sort-Object Name) {
    Capture $case.Key $case.Value | Out-Null
}

Remove-Item $sandbox -Recurse -Force -ErrorAction SilentlyContinue
$captured = (Get-ChildItem $Out -Filter *.txt).Count
Write-Output "Captured $captured files to $Out"

if ($Baseline -eq "") { exit 0 }

# --- comparison ---
$before = Get-ChildItem $Baseline -Filter *.txt | Sort-Object Name
$after = Get-ChildItem $Out -Filter *.txt | Sort-Object Name
$differences = @()

$beforeNames = $before | ForEach-Object { $_.Name }
$afterNames = $after | ForEach-Object { $_.Name }
foreach ($missing in ($beforeNames | Where-Object { $afterNames -notcontains $_ })) {
    $differences += "MISSING in new capture: $missing"
}
foreach ($added in ($afterNames | Where-Object { $beforeNames -notcontains $_ })) {
    $differences += "ADDED in new capture: $added"
}
foreach ($file in $before | Where-Object { $afterNames -contains $_.Name }) {
    $old = Get-Content $file.FullName -Raw
    $new = Get-Content (Join-Path $Out $file.Name) -Raw
    if ($old -ne $new) { $differences += "CHANGED: $($file.Name)" }
}

if ($differences.Count -eq 0) {
    Write-Output "G2 PASS: $($after.Count) captures identical to $Baseline"
    exit 0
}
Write-Output "G2 FAIL:"
$differences | ForEach-Object { Write-Output "  $_" }
exit 1
