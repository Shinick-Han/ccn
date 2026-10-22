# Headless smoke test for the hjkl spike. No TTY, no assertions on rendering.
# Usage: pwsh spikes/hjkl-editor/smoke.ps1 [-Exe path\to\hjkl-spike.exe]
param([string]$Exe = "$PSScriptRoot\target\debug\hjkl-spike.exe")
$ErrorActionPreference = 'Stop'
if (-not (Test-Path $Exe)) { throw "build first: cargo build --manifest-path $PSScriptRoot/Cargo.toml" }
$tmp = Join-Path $env:TEMP "hjkl-spike-smoke"
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
$fail = 0

function Run([string]$name, [string]$initial, [string]$keys, [string]$expect) {
    $f = Join-Path $tmp "$name.txt"
    [IO.File]::WriteAllText($f, $initial)
    $out = & $Exe --headless $f --keys $keys --no-syntax 2>&1 | Select-Object -First 1
    $got = [IO.File]::ReadAllText($f).Replace("`r`n", "`n")
    if ($got -eq $expect) {
        "PASS  $name   ($out)"
    } else {
        "FAIL  $name`n  expected: $($expect -replace "`n", '\n')`n  got     : $($got -replace "`n", '\n')`n  $out"
        $script:fail++
    }
}

# i / a / o / Esc / :w
Run "insert"      "one`ntwo`n"        "A!<Esc>:w<CR>"        "one!`ntwo`n"
Run "open-line"   "one`n"             "oline2<Esc>:w<CR>"    "one`nline2`n"

# dd, dot-repeat (delete this line twice), undo
Run "dd"          "a`nb`nc`n"         "jdd:w<CR>"            "a`nc`n"
Run "dot-repeat"  "a`nb`nc`nd`n"      "ddj.:w<CR>"           "b`nd`n"
Run "undo"        "a`nb`n"            "dd u:w<CR>"           "a`nb`n"

# yy/p, visual line delete, x
Run "yank-put"    "a`nb`n"            "yyp:w<CR>"            "a`na`nb`n"
Run "visual-d"    "a`nb`nc`n"         "jVjd:w<CR>"           "a`n"
Run "x"           "abc`n"             "lx:w<CR>"             "ac`n"

# ex: substitute, :%s
Run "subst"       "foo bar foo`n"     ":%s/foo/baz/g<CR>:w<CR>" "baz bar baz`n"

# CRLF round-trip must stay byte-identical (host normalises, engine is EOL-agnostic)
$crlf = Join-Path $tmp "crlf.txt"
[IO.File]::WriteAllText($crlf, "a`r`nb`r`n")
$before = (Get-FileHash $crlf).Hash
& $Exe --headless $crlf --keys ":w<CR>" --no-syntax | Out-Null
if ((Get-FileHash $crlf).Hash -eq $before) { "PASS  crlf-round-trip" } else { "FAIL  crlf-round-trip"; $fail++ }

if ($fail -gt 0) { "`n$fail case(s) failed"; exit 1 }
"`nall cases passed"
