# Local npm publisher for @shinick/ccn.
#
# One-time setup (no OTP per package after this):
#   npm token create --name ccn-automation --packages-all `
#     --packages-and-scopes-permission read-write --bypass-2fa
#   npm config set //registry.npmjs.org/:_authToken <token>
#
# Then:
#   pwsh -File npm/publish.ps1              # publish missing packages
#   pwsh -File npm/publish.ps1 -Check       # registry-only verification
#   pwsh -File npm/publish.ps1 -Otp 123456  # fallback for a token that needs OTP
#
# The Node script is shared with CI and is idempotent: published packages are
# skipped, so a failed run can be resumed safely.

param(
    [string]$Otp = "",
    [switch]$Check
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $MyInvocation.MyCommand.Path

if ($Check) {
    node (Join-Path $root 'verify-release.js')
    exit $LASTEXITCODE
}

$args = @((Join-Path $root 'publish.js'))
if (-not [string]::IsNullOrWhiteSpace($Otp)) {
    $args += '--otp'
    $args += $Otp
}

node @args
exit $LASTEXITCODE
