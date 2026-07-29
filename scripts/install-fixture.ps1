# G3: proves an install produces the same tree, byte for byte, before and after
# a build-configuration change.
#
# Hermetic by construction. The fixture's lockfile is committed and the install
# runs --locked, so a package published upstream between the before and after
# runs cannot move the result -- which it otherwise would, since the fixture
# depends on caret ranges and the index cache has a 5-minute TTL.
#
# What is compared: every output file's path and SHA-256, the lockfile, and
# stderr as a SORTED multiset. Sorted because install warnings are emitted from
# worker threads (see bar_warn in src/commands/install.rs) and their order is
# genuinely nondeterministic; their content is not.
#
# Usage:
#   .\scripts\install-fixture.ps1 -Out .golden\install-before
#   .\scripts\install-fixture.ps1 -Out .golden\install-after -Baseline .golden\install-before
#   .\scripts\install-fixture.ps1 -Relock          # regenerate the committed lockfile
#
# Needs network (archives download unless the cache is warm).

param(
    [string]$Exe = (Join-Path $PSScriptRoot "..\target\release\lpm.exe"),
    [string]$Out = (Join-Path $PSScriptRoot "..\.golden\install-current"),
    [string]$Baseline = "",
    [switch]$Relock,
    [switch]$ColdCache
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path $Exe)) {
    Write-Error "No binary at $Exe (cargo build --release first, or pass -Exe)"
    exit 1
}
$Exe = (Resolve-Path $Exe).Path
$fixture = Join-Path $PSScriptRoot "fixtures\install"
$manifest = Join-Path $fixture "lpm.toml.fixture"
$lock = Join-Path $fixture "lpm.lock.fixture"

$work = Join-Path $env:TEMP "lpm-fixture-install"
Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $work | Out-Null
Copy-Item $manifest (Join-Path $work "lpm.toml")

if ($Relock) {
    Push-Location $work
    & $Exe install
    $code = $LASTEXITCODE
    Pop-Location
    if ($code -ne 0) { Write-Error "install failed ($code); lockfile not regenerated"; exit 1 }
    Copy-Item (Join-Path $work "lpm.lock") $lock -Force
    Write-Output "Wrote $lock"
    exit 0
}

if (-not (Test-Path $lock)) {
    Write-Error "No committed lockfile at $lock -- run with -Relock once, and commit it"
    exit 1
}
Copy-Item $lock (Join-Path $work "lpm.lock")

# The archive cache is a deliberate variable: warm is the common path, cold
# proves the download+extract path produces the same bytes.
if ($ColdCache) {
    & $Exe cache clean *> $null
}

Remove-Item $Out -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $Out | Out-Null

$stdoutFile = Join-Path $work "_stdout.txt"
$stderrFile = Join-Path $work "_stderr.txt"
$process = Start-Process -FilePath $Exe -ArgumentList @("install", "--locked") `
    -WorkingDirectory $work -NoNewWindow -Wait -PassThru `
    -RedirectStandardOutput $stdoutFile -RedirectStandardError $stderrFile

if ($process.ExitCode -ne 0) {
    Write-Output (Get-Content $stderrFile -Raw)
    Write-Error "fixture install failed with exit $($process.ExitCode)"
    exit 1
}

# path + hash of every produced file, sorted, with the temp root stripped
$entries = @()
foreach ($file in Get-ChildItem $work -Recurse -File | Sort-Object FullName) {
    if ($file.Name -like "_std*.txt") { continue }
    $relative = $file.FullName.Substring($work.Length).TrimStart("\").Replace("\", "/")
    $hash = (Get-FileHash $file.FullName -Algorithm SHA256).Hash
    $entries += "$hash  $relative"
}
[IO.File]::WriteAllLines((Join-Path $Out "tree.txt"), $entries)

# Warnings sorted: content is the contract, arrival order is not.
#
# Two things are scrubbed first. Home-relative paths, because they name the
# machine. And the global-tool lines: the fixture declares no [tools], so
# anything about a tool or a PATH shadow comes from the host's own
# ~/.lpm/tools.toml leaking into every install on that box. That is host state,
# not a property of the build under test, and leaving it in makes the gate fail
# on a different machine for a reason that has nothing to do with the change.
$warnings = @(
    Get-Content $stderrFile -ErrorAction SilentlyContinue |
    ForEach-Object { $_ -replace [regex]::Escape($env:USERPROFILE), "<HOME>" } |
    Where-Object { $_ -notmatch "on PATH before lpm's shims" } |
    Sort-Object
)
[IO.File]::WriteAllLines((Join-Path $Out "stderr-sorted.txt"), $warnings)

Write-Output "Captured $($entries.Count) files and $($warnings.Count) stderr lines to $Out"
Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue

if ($Baseline -eq "") { exit 0 }

$differences = @()
foreach ($name in @("tree.txt", "stderr-sorted.txt")) {
    $old = Join-Path $Baseline $name
    $new = Join-Path $Out $name
    if (-not (Test-Path $old)) { $differences += "no baseline $name"; continue }
    # @() so an empty capture (no warnings at all) is an empty array, not $null,
    # which Compare-Object refuses
    $diff = Compare-Object @(Get-Content $old) @(Get-Content $new)
    if ($diff) {
        $differences += "CHANGED: $name"
        $diff | Select-Object -First 10 | ForEach-Object {
            $differences += "    $($_.SideIndicator) $($_.InputObject)"
        }
    }
}

if ($differences.Count -eq 0) {
    Write-Output "G3 PASS: install tree and warnings identical to $Baseline"
    exit 0
}
Write-Output "G3 FAIL:"
$differences | ForEach-Object { Write-Output "  $_" }
exit 1
