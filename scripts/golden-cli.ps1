# G2: captures every scrap of CLI text embr can print without touching the
# network, so a build-configuration change can be shown not to have moved any
# of it.
#
# The subcommand tree is walked, not hand-listed: `--help` is parsed for its own
# Commands: block and each entry recursed into. embr has three levels (main.rs,
# then cache/index/patch/self/studio/tool, then tool's own nested group), and a
# hand-written list is exactly the thing that goes stale.
#
# Captured twice, once with colour forced on and once with NO_COLOR: those are
# different rendering paths, and the plain one is what CI and every piping user
# actually sees.
#
# WHAT THIS DOES NOT COVER, so nobody mistakes a pass for more than it is:
# interactive prompts (inquire/crossterm) are absent by design -- they would
# block on stdin; the shim and embx dispatch paths in main.rs key off the
# executable's own filename and are never entered here; `self install`,
# `self code`, `studio open`, and `publish` appear only as help text, which
# means both of the crate's `unsafe` blocks are outside this gate; and the
# indicatif progress bar hides itself when stderr is redirected, so its
# rendering is never exercised.
#
# Usage:
#   .\scripts\golden-cli.ps1 -Out .golden\before
#   ... change something, rebuild ...
#   .\scripts\golden-cli.ps1 -Out .golden\after -Baseline .golden\before
# Exits non-zero if -Baseline is given and anything differs.

param(
    [string]$Exe = (Join-Path $PSScriptRoot "..\target\release\embr.exe"),
    [string]$Out = (Join-Path $PSScriptRoot "..\.golden\current"),
    [string]$Baseline = "",
    [int]$TimeoutSeconds = 60
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path $Exe)) {
    Write-Error "No binary at $Exe (cargo build --release first, or pass -Exe)"
    exit 1
}
$Exe = (Resolve-Path $Exe).Path

# A capture that overwrites its own baseline would delete the evidence and then
# compare the new run against itself -- a guaranteed, meaningless PASS.
if ($Baseline -ne "") {
    $outFull = [IO.Path]::GetFullPath($Out)
    $baseFull = [IO.Path]::GetFullPath($Baseline)
    if ($outFull -eq $baseFull) {
        Write-Error "-Out and -Baseline are the same directory ($outFull); that would erase the baseline"
        exit 1
    }
}

Remove-Item $Out -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $Out | Out-Null

# Three sandboxes, because the error text depends on what is on disk and a
# single empty directory collapses most failures into "no manifest found".
$sandboxRoot = Join-Path $env:TEMP "embr-golden-sandbox"
Remove-Item $sandboxRoot -Recurse -Force -ErrorAction SilentlyContinue
$sandboxes = @{}
foreach ($kind in "empty", "project", "broken") {
    $path = Join-Path $sandboxRoot $kind
    New-Item -ItemType Directory -Force $path | Out-Null
    $sandboxes[$kind] = $path
}
# a valid manifest with no dependencies: reaches the errors that only fire once
# a manifest has been found and parsed
[IO.File]::WriteAllText((Join-Path $sandboxes["project"] "ember.toml"), @"
[package]
name = "golden/fixture"
version = "0.1.0"

[target]
environment = "roblox"
"@)
# and one that does not parse, for the manifest-error path
[IO.File]::WriteAllText((Join-Path $sandboxes["broken"] "ember.toml"), "[package`nname = ")

$utf8 = New-Object Text.UTF8Encoding $false

function Capture([string]$mode, [string]$name, [string[]]$arguments, [string]$sandbox) {
    $work = $sandboxes[$sandbox]
    $stdout = Join-Path $sandboxRoot "_stdout.txt"
    $stderr = Join-Path $sandboxRoot "_stderr.txt"
    $process = Start-Process -FilePath $Exe -ArgumentList $arguments `
        -WorkingDirectory $work -NoNewWindow -PassThru `
        -RedirectStandardOutput $stdout -RedirectStandardError $stderr
    # never hang the harness on a command that turns out to want stdin
    if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
        $process.Kill()
        throw "embr $($arguments -join ' ') did not exit within $TimeoutSeconds s"
    }
    # read as UTF-8 explicitly: Get-Content in PS 5.1 decodes with the ANSI
    # codepage, which mangles embr's ✓/✗/→ and makes captures codepage-dependent
    $text = @(
        "$ embr $($arguments -join ' ')   [$mode, sandbox: $sandbox]",
        "exit: $($process.ExitCode)",
        "--- stdout ---",
        [IO.File]::ReadAllText($stdout, $utf8),
        "--- stderr ---",
        [IO.File]::ReadAllText($stderr, $utf8)
    ) -join "`n"
    # scrub anything machine-specific so two checkouts compare equal. all three
    # are case-insensitive regex replaces: Windows paths vary in case, and the
    # 8.3 short form of $env:TEMP is scrubbed alongside the long form because
    # embr canonicalizes some paths and not others.
    foreach ($pair in @(
            @($sandboxRoot, "<SANDBOX>"),
            @([IO.Path]::GetFullPath($sandboxRoot), "<SANDBOX>"),
            @($Exe, "<EXE>"),
            @($env:USERPROFILE, "<HOME>"))) {
        if ($pair[0]) { $text = $text -replace [regex]::Escape($pair[0]), $pair[1] }
    }
    $target = Join-Path $Out $mode
    New-Item -ItemType Directory -Force $target | Out-Null
    [IO.File]::WriteAllText((Join-Path $target "$name.txt"), $text, $utf8)
    return $text
}

# `Commands:` block of a help page, minus clap's own `help` entry.
function Subcommands([string]$helpText) {
    $names = @()
    $inBlock = $false
    # colour may be forced on, so the captured text is full of escape
    # sequences; they stay in the golden file but must not end up in a filename
    $plain = $helpText -replace "\x1b\[[0-9;]*[A-Za-z]", ""
    foreach ($line in $plain -split "`n") {
        $line = $line.TrimEnd()
        if ($line -match '^Commands:') { $inBlock = $true; continue }
        if (-not $inBlock) { continue }
        if ($line.Trim() -eq "") { break }
        if ($line -match '^\s{2,}(\S+)') {
            $name = $Matches[1]
            if ($name -ne "help") { $names += $name }
        }
    }
    return $names
}

function Walk([string]$mode, [string[]]$path) {
    $name = if ($path.Count -eq 0) { "help" } else { "help_" + ($path -join "_") }
    $text = Capture $mode $name ($path + "--help") "empty"
    foreach ($child in Subcommands $text) {
        Walk $mode ($path + $child)
    }
}

# Error paths, each pinned to the sandbox that reaches the code being tested.
# Six of these used to run in an empty directory and all print the same "no
# ember.toml found" line; `tool install` used to be tested against a subcommand
# that does not exist (it is `tool add`), so it only ever exercised clap.
$errorCases = @(
    @{ name = "err_unknown_command"; args = @("instal"); box = "empty" }
    @{ name = "err_unknown_flag"; args = @("install", "--not-a-real-flag"); box = "empty" }
    @{ name = "err_no_manifest"; args = @("install"); box = "empty" }
    @{ name = "err_malformed_manifest"; args = @("install"); box = "broken" }
    @{ name = "err_locked_no_lock"; args = @("install", "--locked"); box = "project" }
    @{ name = "err_patch_no_spec"; args = @("patch"); box = "project" }
    @{ name = "err_patch_bad_spec"; args = @("patch", "notascope"); box = "project" }
    @{ name = "err_patch_bad_version"; args = @("patch", "scope/name@^1"); box = "project" }
    @{ name = "err_patch_no_copy"; args = @("patch", "commit", "scope/name"); box = "project" }
    @{ name = "err_patch_remove_none"; args = @("patch", "remove", "scope/name"); box = "project" }
    @{ name = "err_run_missing"; args = @("run", "no-such-script"); box = "project" }
    @{ name = "err_tool_bad_spec"; args = @("tool", "add", "not-a-repo"); box = "project" }
    @{ name = "err_tool_unknown_sub"; args = @("tool", "install", "x"); box = "empty" }
    @{ name = "err_execute_bad_spec"; args = @("execute", "@@@"); box = "empty" }
    @{ name = "err_publish_none"; args = @("publish"); box = "empty" }
)

# Colour is part of the output being gated (clap's `color` feature is one of the
# things a size-minded person is tempted to trim), and auto-detection keys off
# whether stderr is a terminal -- which differs between a local run and CI. So
# both renderings are forced explicitly rather than left to detection.
$savedForce = $env:CLICOLOR_FORCE
$savedNoColor = $env:NO_COLOR
try {
    foreach ($mode in "color", "plain") {
        if ($mode -eq "color") {
            $env:CLICOLOR_FORCE = "1"
            Remove-Item Env:\NO_COLOR -ErrorAction SilentlyContinue
        }
        else {
            Remove-Item Env:\CLICOLOR_FORCE -ErrorAction SilentlyContinue
            $env:NO_COLOR = "1"
        }
        Write-Output "Capturing $mode ..."
        Walk $mode @()
        Capture $mode "version" @("--version") "empty" | Out-Null
        foreach ($case in $errorCases) {
            Capture $mode $case.name $case.args $case.box | Out-Null
        }
    }
}
finally {
    # never leak the forcing into whatever the caller runs next -- an
    # install-fixture run in the same shell would otherwise render differently
    if ($null -eq $savedForce) { Remove-Item Env:\CLICOLOR_FORCE -ErrorAction SilentlyContinue }
    else { $env:CLICOLOR_FORCE = $savedForce }
    if ($null -eq $savedNoColor) { Remove-Item Env:\NO_COLOR -ErrorAction SilentlyContinue }
    else { $env:NO_COLOR = $savedNoColor }
}

Remove-Item $sandboxRoot -Recurse -Force -ErrorAction SilentlyContinue
$captured = (Get-ChildItem $Out -Recurse -Filter *.txt).Count
Write-Output "Captured $captured files to $Out"

if ($Baseline -eq "") { exit 0 }

# --- comparison ---
function Relative([string]$root, [IO.FileInfo]$file) {
    return $file.FullName.Substring((Resolve-Path $root).Path.Length).TrimStart("\")
}
$before = @(Get-ChildItem $Baseline -Recurse -Filter *.txt | Sort-Object FullName)
$after = @(Get-ChildItem $Out -Recurse -Filter *.txt | Sort-Object FullName)
$beforeNames = @($before | ForEach-Object { Relative $Baseline $_ })
$afterNames = @($after | ForEach-Object { Relative $Out $_ })
$differences = @()

foreach ($missing in ($beforeNames | Where-Object { $afterNames -notcontains $_ })) {
    $differences += "MISSING in new capture: $missing"
}
foreach ($added in ($afterNames | Where-Object { $beforeNames -notcontains $_ })) {
    $differences += "ADDED in new capture: $added"
}
foreach ($name in ($beforeNames | Where-Object { $afterNames -contains $_ })) {
    $old = [IO.File]::ReadAllText((Join-Path $Baseline $name), $utf8)
    $new = [IO.File]::ReadAllText((Join-Path $Out $name), $utf8)
    # -cne, not -ne: PowerShell's default string comparison is CASE-INSENSITIVE,
    # so `Installed` -> `installed` would sail through a gate whose entire job
    # is byte-identity
    if ($old -cne $new) { $differences += "CHANGED: $name" }
}

if ($differences.Count -eq 0) {
    Write-Output "G2 PASS: $($after.Count) captures identical to $Baseline"
    exit 0
}
Write-Output "G2 FAIL:"
$differences | ForEach-Object { Write-Output "  $_" }
exit 1
