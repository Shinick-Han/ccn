# Calibration harness: scan a list of repositories and emit one CSV row per repo.
#
# Usage:
#   pwsh -File tools/calibrate.ps1
#   pwsh -File tools/calibrate.ps1 -Repo C:\path\a,C:\path\b
#   pwsh -File tools/calibrate.ps1 -Out docs/calibration.csv
#
# Reads `ccn --report-json`, i.e. exactly the numbers the HTML/terminal report
# shows. Never re-implements the health formula here, so the harness cannot
# drift from the product.

param(
    [string[]]$Repo = @(
        'C:\work\project-c',
        'C:\work\project-g',
        'C:\work\project-j',
        'C:\work\project-h',
        'C:\work\project-d',
        'C:\work\project-e',
        'C:\work\agy',
        'C:\work\CCN',
        'C:\work\project-i',
        'C:\work\project-f',
        'C:\work\retriever'
    ),
    [string]$Out = 'docs/calibration.csv',
    [string]$Bin = "$PSScriptRoot\..\target\release\ccn.exe"
)

$ErrorActionPreference = 'Stop'
if (-not (Test-Path $Bin)) { throw "ccn binary not found at $Bin - run 'cargo build --release' first" }

$rows = foreach ($path in $Repo) {
    if (-not (Test-Path $path)) {
        Write-Warning "skip (missing): $path"
        continue
    }

    $sw = [Diagnostics.Stopwatch]::StartNew()
    $raw = & $Bin $path --report-json 2>$null
    $sw.Stop()
    if (-not $raw) { Write-Warning "skip (no output): $path"; continue }

    $r = $raw | ConvertFrom-Json
    $h = $r.health

    [pscustomobject]@{
        repo            = Split-Path $path -Leaf
        files           = $r.files_analyzed
        loc             = $r.loc_total
        functions       = $r.stats.functions_total
        health          = if ($h) { [Math]::Round($h.score, 2) } else { $null }
        grade           = if ($h) { $h.grade } else { '' }
        debt            = if ($h) { [Math]::Round($h.debt, 4) } else { $null }
        pct_over_10     = if ($h) { [Math]::Round($h.pct_over_10, 3) } else { 0 }
        over_20         = if ($h) { $h.over_20 } else { 0 }
        over_50         = if ($h) { $h.over_50 } else { 0 }
        ccn_median      = $r.stats.ccn_median
        ccn_p90         = $r.stats.ccn_p90
        ccn_p99         = $r.stats.ccn_p99
        ccn_max         = $r.stats.ccn_max
        loc_p90         = $r.stats.loc_p90
        loc_max         = $r.stats.loc_max
        share_top10pct  = [Math]::Round($r.stats.loc_share_top10pct, 1)
        files_exact     = $r.stats.files_exact
        files_heuristic = $r.stats.files_heuristic
        scan_ms         = $sw.ElapsedMilliseconds
    }
}

$rows | Export-Csv -Path $Out -NoTypeInformation -Encoding utf8

$rows | Format-Table repo, files, functions, health, grade, pct_over_10, ccn_p90, ccn_max, files_exact, files_heuristic -AutoSize
Write-Host "`nwrote $Out ($($rows.Count) repos)"
