# G3: proves an install produces the same tree and says the same things, before
# and after a build-configuration change.
#
# Reproducible by construction: the fixture's lockfile is committed and the
# install runs --locked, so a package published upstream between two runs cannot
# move the result -- which it otherwise would, since the fixture depends on
# caret ranges and the index cache has a 5-minute TTL.
#
# What is compared:
#   * every output file's path and SHA-256
#   * stdout, normalised (see below) -- the success lines, which are most of
#     what a user actually reads
#   * stderr, sorted -- install warnings come off worker threads and their
#     ORDER is genuinely nondeterministic while their content is not
#
# Honest limits, so a PASS is not read as more than it is:
#   * --locked is what makes this reproducible, and it also means the RESOLVER
#     never runs. Version selection, transitive discovery, conflict handling and
#     lockfile writing are not covered here; they are covered by unit tests.
#     ember.lock is copied IN as an input, so its hash in tree.txt proves nothing.
#   * NOT hermetic with respect to the host. Every `embr install` merges the
#     machine's ~/.ember/tools.toml into its job list (install.rs, tool_jobs +
#     global_tools) and embr's home has no override -- it is always
#     dirs::home_dir()/.ember. On a machine with global tools pinned, this run
#     downloads them and prints extra lines. Those lines are filtered below.
#     A run on a machine with no global tools is hermetic by accident, not by
#     construction.
#   * the lockfile entries carry no content hash, so "same URL" is not "same
#     bytes"; a re-published archive shows up here as a spurious FAIL.
#
# Usage:
#   .\scripts\install-fixture.ps1 -Out .golden\install-before
#   .\scripts\install-fixture.ps1 -Out .golden\install-after -Baseline .golden\install-before
#   .\scripts\install-fixture.ps1 -Relock          # regenerate the committed lockfile
#
# Needs network (archives download unless the cache is warm).

param(
    [string]$Exe = (Join-Path $PSScriptRoot "..\target\release\embr.exe"),
    [string]$Out = (Join-Path $PSScriptRoot "..\.golden\install-current"),
    [string]$Baseline = "",
    [switch]$Relock,
    # DESTRUCTIVE: `embr cache clean` wipes the real ~/.ember archive and index
    # caches for every project on this machine, not a sandboxed copy. embr has no
    # home-directory override, so there is no way to do this in isolation.
    [switch]$ColdCache
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path $Exe)) {
    Write-Error "No binary at $Exe (cargo build --release first, or pass -Exe)"
    exit 1
}
$Exe = (Resolve-Path $Exe).Path
$fixture = Join-Path $PSScriptRoot "fixtures\install"
$manifest = Join-Path $fixture "ember.toml.fixture"
$lock = Join-Path $fixture "ember.lock.fixture"
$utf8 = New-Object Text.UTF8Encoding $false

if ($Baseline -ne "") {
    if ([IO.Path]::GetFullPath($Out) -eq [IO.Path]::GetFullPath($Baseline)) {
        Write-Error "-Out and -Baseline are the same directory; that would erase the baseline"
        exit 1
    }
}

$work = Join-Path $env:TEMP "embr-fixture-install"
Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $work | Out-Null
Copy-Item $manifest (Join-Path $work "ember.toml")

if ($Relock) {
    Push-Location $work
    & $Exe install
    $code = $LASTEXITCODE
    Pop-Location
    if ($code -ne 0) { Write-Error "install failed ($code); lockfile not regenerated"; exit 1 }
    Copy-Item (Join-Path $work "ember.lock") $lock -Force
    Write-Output "Wrote $lock"
    exit 0
}

if (-not (Test-Path $lock)) {
    Write-Error "No committed lockfile at $lock -- run with -Relock once, and commit it"
    exit 1
}
Copy-Item $lock (Join-Path $work "ember.lock")

if ($ColdCache) {
    Write-Warning "-ColdCache wipes the REAL ~/.ember archive and index caches (all projects on this machine)"
    & $Exe cache clean *> $null
    # a swallowed failure here would leave the cache warm and still report a
    # cold-cache PASS, which is the sort of quiet lie this harness exists to avoid
    if ($LASTEXITCODE -ne 0) {
        Write-Error "cache clean failed ($LASTEXITCODE); refusing to report a cold-cache result"
        exit 1
    }
}

Remove-Item $Out -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $Out | Out-Null

$stdoutFile = Join-Path $work "_stdout.txt"
$stderrFile = Join-Path $work "_stderr.txt"
$process = Start-Process -FilePath $Exe -ArgumentList @("install", "--locked") `
    -WorkingDirectory $work -NoNewWindow -Wait -PassThru `
    -RedirectStandardOutput $stdoutFile -RedirectStandardError $stderrFile

if ($process.ExitCode -ne 0) {
    Write-Output ([IO.File]::ReadAllText($stderrFile, $utf8))
    Write-Error "fixture install failed with exit $($process.ExitCode)"
    exit 1
}

# Path + hash of every produced file, sorted, with the temp root stripped.
# -Force so hidden entries count: "the same tree" should not mean "the same
# tree minus anything carrying the hidden attribute".
$entries = @()
foreach ($file in Get-ChildItem $work -Recurse -File -Force | Sort-Object FullName) {
    if ($file.Name -like "_std*.txt") { continue }
    $relative = $file.FullName.Substring($work.Length).TrimStart("\").Replace("\", "/")
    $hash = (Get-FileHash $file.FullName -Algorithm SHA256).Hash
    $entries += "$hash  $relative"
}
[IO.File]::WriteAllLines((Join-Path $Out "tree.txt"), $entries, $utf8)

# stdout: the ✓ lines, the counts, the summary. Two normalisations, both
# necessary and neither of them hiding content:
#   * the elapsed-time line is different on every run by definition
#   * per-package ✓ lines are printed as jobs complete, so their order is a
#     function of thread scheduling; sorting compares the multiset
$stdout = @(
    [IO.File]::ReadAllText($stdoutFile, $utf8) -split "`r?`n" |
    ForEach-Object { $_ -replace [regex]::Escape($env:USERPROFILE), "<HOME>" } |
    # not anchored: ui::print_elapsed wraps the line in a dim-grey escape
    # sequence, so `^Done in` never matches the actual bytes
    Where-Object { $_ -notmatch 'Done in ' } |
    Where-Object { $_.Trim() -ne "" } |
    Sort-Object
)
[IO.File]::WriteAllLines((Join-Path $Out "stdout-sorted.txt"), $stdout, $utf8)

# stderr, same treatment. The global-tool PATH warning is dropped: the fixture
# declares no [tools], so it comes from the host's own ~/.ember/tools.toml.
$warnings = @(
    [IO.File]::ReadAllText($stderrFile, $utf8) -split "`r?`n" |
    ForEach-Object { $_ -replace [regex]::Escape($env:USERPROFILE), "<HOME>" } |
    Where-Object { $_ -notmatch "on PATH before embr's shims" } |
    Where-Object { $_.Trim() -ne "" } |
    Sort-Object
)
[IO.File]::WriteAllLines((Join-Path $Out "stderr-sorted.txt"), $warnings, $utf8)

Write-Output "Captured $($entries.Count) files, $($stdout.Count) stdout lines, $($warnings.Count) stderr lines to $Out"
Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue

if ($Baseline -eq "") { exit 0 }

$differences = @()
foreach ($name in @("tree.txt", "stdout-sorted.txt", "stderr-sorted.txt")) {
    $old = Join-Path $Baseline $name
    $new = Join-Path $Out $name
    if (-not (Test-Path $old)) { $differences += "no baseline $name"; continue }
    # -CaseSensitive: the default comparison would accept packages/Promise.luau
    # turning into packages/promise.luau
    # ReadAllLines with an explicit UTF-8 decoder: Get-Content in PS 5.1 uses the
    # ANSI codepage, and these files are full of ✓ and → written as UTF-8
    $diff = Compare-Object `
        @([IO.File]::ReadAllLines($old, $utf8)) `
        @([IO.File]::ReadAllLines($new, $utf8)) -CaseSensitive
    if ($diff) {
        $differences += "CHANGED: $name"
        $diff | Select-Object -First 10 | ForEach-Object {
            $differences += "    $($_.SideIndicator) $($_.InputObject)"
        }
    }
}

if ($differences.Count -eq 0) {
    Write-Output "G3 PASS: tree, stdout and warnings identical to $Baseline"
    exit 0
}
Write-Output "G3 FAIL:"
$differences | ForEach-Object { Write-Output "  $_" }
exit 1
