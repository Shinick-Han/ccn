# Publishes @shinick/ccn and its platform packages.
#
#   1. npm login                     (once per machine / when the session expires)
#   2. node npm/prepare.js           (downloads the release binaries)
#   3. pwsh -File npm/publish.ps1    (npm prompts for an OTP per package)
#      pwsh -File npm/publish.ps1 -Otp 123456   (reuse one OTP while it is valid)
#
# The platform packages must exist before the main package is installed by
# users, so publish them first. The script is idempotent: a package already at
# the target version is skipped, so a run that dies on an expired OTP can be
# resumed with a fresh code.

param(
    [string]$Otp = ""
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$version = (Get-Content (Join-Path $root 'package.json') -Raw | ConvertFrom-Json).version

$user = npm whoami 2>$null
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($user)) {
    throw "npm is not logged in. Run 'npm login' and complete the browser/OTP step, then 'npm whoami'."
}
Write-Host "npm user: $user (target $version)"

$otpArgs = @()
if (-not [string]::IsNullOrWhiteSpace($Otp)) {
    $otpArgs = @("--otp=$Otp")
}

$published = @()
$toVerify = @()
$platforms = @(
    'platforms/linux-x64',
    'platforms/darwin-x64',
    'platforms/darwin-arm64',
    'platforms/win32-x64'
)
$targets = @()
foreach ($platform in $platforms) { $targets += (Join-Path $root $platform) }
$targets += $root

foreach ($dir in $targets) {
    $manifest = Get-Content (Join-Path $dir 'package.json') -Raw | ConvertFrom-Json
    $name = $manifest.name
    $existing = npm view "$name@$version" version 2>$null
    if ($LASTEXITCODE -eq 0 -and $existing -eq $version) {
        Write-Host "already published: $name@$version (skipping)"
        $published += $name
        $toVerify += $name
        continue
    }
    if ($manifest.version -ne $version) {
        throw "$name is $($manifest.version) but the target is $version"
    }
    $vendor = Join-Path $dir 'vendor'
    if ($dir -ne $root -and -not (Test-Path $vendor)) {
        throw "$name has no vendor/ - run 'node npm/prepare.js' first"
    }

    Write-Host "publishing $name@$version"
    Push-Location $dir
    try {
        npm publish --access public @otpArgs
        if ($LASTEXITCODE -ne 0) { throw "npm publish failed for $name" }
    } finally {
        Pop-Location
    }

    Write-Host "accepted: $name@$version (registry processing; verified at the end)"
    $published += $name
    $toVerify += $name
}

# npm returns 202 "being processed" for large packages; metadata can lag by
# 1-3 minutes. Poll the exact version rather than the `latest` tag, which may
# still point at the previous release while the upload is queued.
$deadline = (Get-Date).AddMinutes(3)
$pending = @($toVerify)
while ($pending.Count -gt 0 -and (Get-Date) -lt $deadline) {
    $still = @()
    foreach ($name in $pending) {
        $seen = npm view "$name@$version" version 2>$null
        if ($LASTEXITCODE -ne 0 -or $seen -ne $version) { $still += $name }
    }
    $pending = $still
    if ($pending.Count -gt 0) { Start-Sleep -Seconds 10 }
}

if ($pending.Count -gt 0) {
    throw ("npm accepted the upload but the registry still lacks " +
        "$version for: $($pending -join ', '). Wait a minute and rerun; " +
        "already-published packages are skipped.")
}

Write-Host ""
Write-Host "published and verified: $($published -join ', ')"
Write-Host "verify: npx @shinick/ccn@$version --version"
