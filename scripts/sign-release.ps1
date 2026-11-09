# Signs the binaries of a published GitHub release so installed copies can update to it.
#
#   .\scripts\sign-release.ps1 v1.2.3
#
# CI builds the binaries and creates the release when a v* tag is pushed. This script
# then signs an update-<platform>.json for each with the private key in
# %USERPROFILE%\.deskbell\update-signing.key (never in CI), and uploads the manifests to
# the same release. Until it has run, installed copies don't see the release.
#
# It doesn't sign whatever happens to be attached to the release: it signs the binaries
# that CI built from the commit your local tag points to, and refuses if the release's
# files differ from them (e.g. replaced by someone else with write access to the repo).
param(
    [Parameter(Mandatory = $true)][string]$Tag
)
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot -Parent)

if ($Tag -notmatch '^v(\d+\.\d+\.\d+)$') { throw "Tag must look like v1.2.3, got '$Tag'" }
$version = $Matches[1]
$cargoVersion = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
if ($cargoVersion -ne $version) { Write-Warning "Cargo.toml is at $cargoVersion, the tag is $Tag" }

# The commit you tagged, from your own checkout.
$commit = git rev-parse --verify --quiet "$Tag^{commit}"
if ($LASTEXITCODE -ne 0 -or -not $commit) { throw "Tag $Tag doesn't exist in this checkout" }

# The successful CI run for that tag, which must have built exactly that commit.
$runs = gh run list --workflow ci.yml --branch $Tag --event push --limit 20 --json databaseId,headSha,conclusion | ConvertFrom-Json
if ($LASTEXITCODE -ne 0) { throw "Could not list the CI runs of $Tag" }
$run = $runs | Where-Object { $_.conclusion -eq 'success' } | Select-Object -First 1
if (-not $run) { throw "No successful CI run for $Tag yet" }
if ($run.headSha -ne $commit) {
    throw "The CI run for $Tag built $($run.headSha), but your tag points to $commit"
}

$dir = Join-Path 'dist' $Tag
if (Test-Path $dir) { Remove-Item -Recurse -Force $dir }
$built = Join-Path $dir 'built'
$released = Join-Path $dir 'released'
New-Item -ItemType Directory -Force $built, $released | Out-Null

gh run download $run.databaseId --dir $built
if ($LASTEXITCODE -ne 0) { throw "Could not download the CI artifacts of run $($run.databaseId)" }
gh release download $Tag --dir $released --pattern 'deskbell-*'
if ($LASTEXITCODE -ne 0) { throw "Could not download the release assets of $Tag" }

$manifests = @()
foreach ($asset in 'deskbell-windows-x64.exe', 'deskbell-linux-x64', 'deskbell-macos-arm64', 'deskbell-macos-x64') {
    # Each CI artifact is a folder named after the file it holds.
    $binary = Join-Path (Join-Path $built $asset) $asset
    $public = Join-Path $released $asset
    if (-not (Test-Path $binary)) { Write-Warning "$asset is missing from the CI run, skipped"; continue }
    if (-not (Test-Path $public)) { throw "$asset is missing from the release" }
    if ((Get-FileHash $binary).Hash -ne (Get-FileHash $public).Hash) {
        throw "The release's $asset is not the one CI built from $commit"
    }
    $platform = $asset -replace '^deskbell-', '' -replace '\.exe$', ''
    $out = Join-Path $dir "update-$platform.json"
    cargo run --quiet --example release_signing -- sign $binary $version $out
    if ($LASTEXITCODE -ne 0) { throw "Signing $asset failed" }
    $manifests += $out
}
if ($manifests.Count -eq 0) { throw "Nothing to sign" }

gh release upload $Tag @manifests --clobber
if ($LASTEXITCODE -ne 0) { throw "Could not upload the manifests" }
Write-Host "Signed and uploaded $($manifests.Count) manifest(s) to $Tag (built from $commit)"
