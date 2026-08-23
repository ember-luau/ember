# Benchmarks embr against pesde and wally on identical projects.
#
# Local scenarios (no-op, warm-cache, locked) are deterministic and carry
# hard targets; network scenarios interleave the tools (embr, pesde, wally,
# embr, ...) inside one session so registry drift hits everyone equally.
# Each binary's --version is printed with the results, so a tool silently
# updating between a baseline and an after-run is visible in the output.
#
# Usage: .\scripts\bench.ps1 [-EmbrPath ...] [-PesdePath ...] [-WallyPath ...]

param(
    [string]$EmbrPath = (Join-Path $PSScriptRoot "..\target\release\embr.exe"),
    [string]$PesdePath = "$env:USERPROFILE\.rokit\tool-storage\pesde-pkg\pesde\0.7.3+registry.0.2.3\pesde.exe",
    [string]$WallyPath = "$env:USERPROFILE\.rokit\tool-storage\upliftgames\wally\0.3.2\wally.exe",
    [int]$Runs = 7
)

foreach ($binary in $EmbrPath, $PesdePath, $WallyPath) {
    if (-not (Test-Path $binary)) {
        Write-Error "Benchmark binary not found: $binary (pass -EmbrPath/-PesdePath/-WallyPath)"
        exit 1
    }
}

$originalLocation = Get-Location
$bench = Join-Path $env:TEMP "embr-bench"
Remove-Item $bench -Recurse -Force -ErrorAction SilentlyContinue
foreach ($d in "embr", "pesde", "wally") { New-Item -ItemType Directory -Force "$bench\$d" | Out-Null }

# the same five wally packages for everyone
$deps = @(
    @("Promise", "evaera/promise", "^4.0.0"),
    @("Trove", "sleitnick/trove", "^1.8.0"),
    @("Signal", "sleitnick/signal", "^2.0.3"),
    @("T", "osyrisrblx/t", "^3.1.1"),
    @("Comm", "sleitnick/comm", "^1.0.1")
)

$emberDeps = ($deps | ForEach-Object { "$($_[0]) = { name = `"$($_[1])`", version = `"$($_[2])`", index = `"wally`" }" }) -join "`n"
[IO.File]::WriteAllText("$bench\embr\ember.toml", @"
[package]
name = "bench/bench"
version = "0.1.0"

[target]
environment = "roblox"

[indices]
wally = "https://github.com/UpliftGames/wally-index"

[dependencies]
$emberDeps
"@)

$wallyDeps = ($deps | ForEach-Object { "$($_[0]) = `"$($_[1])@$($_[2])`"" }) -join "`n"
[IO.File]::WriteAllText("$bench\wally\wally.toml", @"
[package]
name = "bench/bench"
version = "0.1.0"
registry = "https://github.com/UpliftGames/wally-index"
realm = "shared"

[dependencies]
$wallyDeps
"@)

$pesdeDeps = ($deps | ForEach-Object { "$($_[0]) = { wally = `"$($_[1])`", version = `"$($_[2])`", index = `"wally`" }" }) -join "`n"
[IO.File]::WriteAllText("$bench\pesde\pesde.toml", @"
name = "bench/bench"
version = "0.1.0"

[target]
environment = "roblox"
build_files = ["src"]

[indices]
default = "https://github.com/pesde-pkg/index"

[wally_indices]
wally = "https://github.com/UpliftGames/wally-index"

[dependencies]
$pesdeDeps
"@)

$tools = @(
    @{ name = "embr";   exe = $EmbrPath;   dir = "$bench\embr";   out = "packages";        lock = "ember.lock" }
    @{ name = "pesde"; exe = $PesdePath; dir = "$bench\pesde"; out = "roblox_packages"; lock = "pesde.lock" }
    @{ name = "wally"; exe = $WallyPath; dir = "$bench\wally"; out = "Packages";        lock = "wally.lock" }
)

Write-Output "=== binaries ==="
foreach ($t in $tools) {
    $version = (& $t.exe --version 2>&1 | Select-Object -First 1)
    $size = (Get-Item $t.exe).Length
    Write-Output ("{0,-6} {1,8:N0} KiB   {2}" -f $t.name, ($size / 1024), $version)
}

function Median($times) {
    $sorted = $times | Sort-Object
    $mid = [int][math]::Floor($sorted.Count / 2)
    if ($sorted.Count % 2 -eq 0) { ($sorted[$mid - 1] + $sorted[$mid]) / 2 } else { $sorted[$mid] }
}

function Time($tool, [scriptblock]$prepare, [string[]]$arguments) {
    Set-Location $tool.dir
    & $prepare
    $sw = [Diagnostics.Stopwatch]::StartNew()
    & $tool.exe @arguments *> $null
    $sw.Stop()
    $sw.Elapsed.TotalMilliseconds
}

function Report($label, $samples) {
    foreach ($t in "embr", "pesde", "wally") {
        if ($samples[$t].Count -gt 0) {
            Write-Output ("{0,-34} {1,-6} median {2,7:N0} ms   min {3,7:N0} ms   max {4,7:N0} ms" -f `
                    $label, $t, (Median $samples[$t]), ($samples[$t] | Sort-Object)[0], ($samples[$t] | Sort-Object)[-1])
        }
    }
}

# warm-up: one install each, so registries, DNS, and index caches are hot
foreach ($t in $tools) { Set-Location $t.dir; & $t.exe install *> $null }

Write-Output ""
Write-Output "=== startup (--version), $($Runs + 5) runs ==="
$samples = @{ embr = @(); pesde = @(); wally = @() }
for ($i = 0; $i -lt $Runs + 5; $i++) {
    foreach ($t in $tools) { $samples[$t.name] += Time $t {} @("--version") }
}
Report "startup" $samples

Write-Output ""
Write-Output "=== install, nothing to do, $Runs runs ==="
$samples = @{ embr = @(); pesde = @(); wally = @() }
for ($i = 0; $i -lt $Runs; $i++) {
    foreach ($t in $tools) { $samples[$t.name] += Time $t {} @("install") }
}
Report "no-op" $samples

Write-Output ""
Write-Output "=== install, output wiped (embr: warm archive cache), $Runs runs ==="
$samples = @{ embr = @(); pesde = @(); wally = @() }
for ($i = 0; $i -lt $Runs; $i++) {
    foreach ($t in $tools) {
        $tool = $t
        $prepare = { Remove-Item (Join-Path $tool.dir $tool.out), (Join-Path $tool.dir $tool.lock) -Recurse -Force -ErrorAction SilentlyContinue }.GetNewClosure()
        $samples[$t.name] += Time $t $prepare @("install")
    }
}
Report "scratch (interleaved)" $samples

Write-Output ""
Write-Output "=== embr only: --locked, output wiped, $Runs runs ==="
$samples = @{ embr = @(); pesde = @(); wally = @() }
$embr = $tools[0]
Set-Location $embr.dir; & $embr.exe install *> $null
$tool = $embr
$prepare = { Remove-Item (Join-Path $tool.dir $tool.out) -Recurse -Force -ErrorAction SilentlyContinue }.GetNewClosure()
for ($i = 0; $i -lt $Runs; $i++) {
    $samples["embr"] += Time $embr $prepare @("install", "--locked")
}
Report "locked, warm cache" $samples

Write-Output ""
Write-Output "=== embr only: --refresh (cold: no fast path, forced pull, cache bypass), 3 runs ==="
$samples = @{ embr = @(); pesde = @(); wally = @() }
for ($i = 0; $i -lt 3; $i++) {
    $samples["embr"] += Time $embr $prepare @("install", "--refresh")
}
Report "cold refresh" $samples

Write-Output ""
Write-Output @"
CAVEAT: these are product-behavior comparisons, not identical-work ones.
In the no-op row embr takes its fast path (no resolve, no git, no HTTP)
while the others do whatever their no-change install does; in the scratch
rows embr skips its index refresh inside the TTL and serves archives from
its cache, as it would for a real user. The 'cold refresh' row is embr with
every shortcut disabled.
"@

Set-Location $originalLocation
Remove-Item $bench -Recurse -Force -ErrorAction SilentlyContinue
