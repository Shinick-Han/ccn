# Publishes @shinick/ccn and its platform packages.
#
#   1. npm login                     (once per machine)
#   2. node npm/prepare.js           (downloads the release binaries)
#   3. pwsh -File npm/publish.ps1
#
# The platform packages must exist before the main package is installed by
# users, so publish them first.

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$version = (Get-Content (Join-Path $root 'package.json') -Raw | ConvertFrom-Json).version

$platforms = @(
    'platforms/linux-x64',
    'platforms/darwin-x64',
    'platforms/darwin-arm64',
    'platforms/win32-x64'
)

foreach ($platform in $platforms) {
    $dir = Join-Path $root $platform
    $manifest = Get-Content (Join-Path $dir 'package.json') -Raw | ConvertFrom-Json
    $vendor = Join-Path $dir 'vendor'
    if (-not (Test-Path $vendor)) {
        throw "$platform has no vendor/ - run 'node npm/prepare.js' first"
    }
    if ($manifest.version -ne $version) {
        throw "$platform is $($manifest.version) but the main package is $version"
    }
    Write-Host "publishing $($manifest.name)@$($manifest.version)"
    Push-Location $dir
    try {
        npm publish --access public
        if ($LASTEXITCODE -ne 0) { throw "npm publish failed in $dir" }
    } finally {
        Pop-Location
    }
}

Push-Location $root
try {
    Write-Host "publishing @shinick/ccn@$version"
    npm publish --access public
    if ($LASTEXITCODE -ne 0) { throw "npm publish failed for the main package" }
} finally {
    Pop-Location
}

Write-Host "done: npx @shinick/ccn@$version --version"
