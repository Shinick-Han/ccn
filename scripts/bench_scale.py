#!/usr/bin/env python3
"""Deterministic large-repository scale benchmark for ccn.

Generates synthetic source trees of a requested size and measures the scanner's
own scaling on the host it runs on. There is no network access and no third
party package: only the Python standard library is used.

Two jobs, one script:

    # 1. generate a tree (deterministic for a given --seed)
    python scripts/bench_scale.py --files 20000 --out %TEMP%\\ccn-bench\\n20000

    # 2. measure it (cold/warm/verify/no-cache/no-ccn/CI/report-json/HTML)
    python scripts/bench_scale.py --driver --files 20000 \\
        --out %TEMP%\\ccn-bench\\n20000 --workdir %TEMP%\\ccn-bench\\work

    # everything, including the 20k one-directory and pathological trees
    python scripts/bench_scale.py --all --workdir %TEMP%\\ccn-bench

`--driver` is the measurement entry point: it drives `target/release/ccn.exe`
(or `--binary`), isolates every cache in `--workdir`, and writes a JSON result
plus a printed table. `--engine powershell` uses `Start-Process -PassThru` /
`PeakWorkingSet64` instead of the in-process Win32 handle; the two engines are
independent implementations of the same measurement, so they can be compared.

The generator's shape (see docs/benchmarks/2026-10-21-scale.md):

* 50 % C, 20 % C++, 10 % Rust, 10 % Python, 10 % JavaScript;
* 20-120 physical lines and 1-5 functions per file;
* every 50th file has a function with 15 `if` branches;
* about 100 files per directory (wide and shallow), or one flat directory with
  `--one-dir`;
* `--pathological` adds a file above the 8 MiB CCN guard, a file with ~200
  nested `if`s and a symlink (a copy when links need privileges, or with
  `--no-links`).
"""

from __future__ import annotations

import argparse
import json
import os
import random
import shutil
import subprocess
import sys
import time
from pathlib import Path

SIZES = (1_000, 5_000, 20_000)
FILES_PER_DIR = 100
DEFAULT_SEED = 20261021
# `src/loc.rs` guards the AST pass with an 8 MiB size cap. The pathological
# file must sit above that guard, so it is 9 MiB rather than 5 MB.
HUGE_MIB = 9

# ---------------------------------------------------------------- generation


def _c_function(name: str, branches: int, rng: random.Random) -> list[str]:
    """A C function whose CCN is `1 + branches` (plus loop bodies)."""
    lines = [f"static int {name}(int a, int b)", "{"]
    lines.append("    int r = a + b;")
    for i in range(branches):
        cond = rng.choice(
            [
                f"r > {i}",
                f"r < {i + 3}",
                f"a == {i}",
                f"(r & {1 << (i % 6)}) != 0",
            ]
        )
        lines.append(f"    if ({cond}) {{ r += {i + 1}; }}")
    lines.append("    r += a;")
    lines.append("    return r;")
    lines.append("}")
    return lines


def _cpp_function(name: str, branches: int, rng: random.Random) -> list[str]:
    lines = [f"int {name}(int a, int b)", "{"]
    lines.append("    int r = a + b;")
    for i in range(branches):
        cond = rng.choice([f"r > {i}", f"r < {i + 3}", f"a == {i}"])
        lines.append(f"    if ({cond}) {{ r += {i + 1}; }}")
    lines.append("    return r;")
    lines.append("}")
    return lines


def _rust_function(name: str, branches: int, rng: random.Random) -> list[str]:
    lines = [f"pub fn {name}(a: i32, b: i32) -> i32 {{", "    let mut r = a + b;"]
    for i in range(branches):
        cond = rng.choice([f"r > {i}", f"r < {i + 3}", f"a == {i}"])
        lines.append(f"    if {cond} {{ r += {i + 1}; }}")
    lines.append("    r")
    lines.append("}")
    return lines


def _python_function(name: str, branches: int, rng: random.Random) -> list[str]:
    lines = [f"def {name}(a, b):", "    r = a + b"]
    for i in range(branches):
        cond = rng.choice([f"r > {i}", f"r < {i + 3}", f"a == {i}"])
        lines.append(f"    if {cond}:")
        lines.append(f"        r += {i + 1}")
    lines.append("    return r")
    return lines


def _js_function(name: str, branches: int, rng: random.Random) -> list[str]:
    lines = [f"function {name}(a, b) {{", "  let r = a + b;"]
    for i in range(branches):
        cond = rng.choice([f"r > {i}", f"r < {i + 3}", f"a == {i}"])
        lines.append(f"  if ({cond}) {{ r += {i + 1}; }}")
    lines.append("  return r;")
    lines.append("}")
    return lines


LANG_SPEC = {
    "c": (".c", "/* bench file {stem} */", "//", _c_function, "static int"),
    "cpp": (".cpp", "// bench file {stem}", "//", _cpp_function, "namespace bench_{stem} {{"),
    "rust": (".rs", "// bench file {stem}", "//", _rust_function, ""),
    "python": (".py", "# bench file {stem}", "#", _python_function, ""),
    "javascript": (".js", "// bench file {stem}", "//", _js_function, ""),
}

def _pick_lang(index: int) -> str:
    # A deterministic 50/20/10/10/10 split that does not depend on `random`
    # ordering: file 0 is C, file 1 is C, ... file 99 is JavaScript.
    position = index % 100
    if position < 50:
        return "c"
    if position < 70:
        return "cpp"
    if position < 80:
        return "rust"
    if position < 90:
        return "python"
    return "javascript"


def render_file(index: int, seed: int) -> tuple[str, str]:
    rng = random.Random(seed * 1_000_003 + index)
    lang = _pick_lang(index)
    ext, header, comment, make_fn, prefix = LANG_SPEC[lang]
    stem = f"f{index:06d}"
    target_lines = rng.randint(20, 120)
    count = rng.randint(1, 5)
    branch_file = index % 50 == 49  # every 50th file carries the 15-branch fn

    blocks: list[list[str]] = []
    for f_index in range(count):
        branches = 15 if (branch_file and f_index == count - 1) else rng.randint(0, 4)
        blocks.append(make_fn(f"{stem}_{f_index:03d}", branches, rng))

    out: list[str] = [header.format(stem=stem)]
    if lang == "cpp":
        out.append(prefix.format(stem=stem))
    if lang == "python":
        out.extend(["", ""])

    # Pad with comments until the file reaches its target length; keep at least
    # the natural length when the functions are already long.
    natural = sum(len(b) for b in blocks) + len(out) + (count - 1) * 2
    pad = max(0, target_lines - natural)
    for i, block in enumerate(blocks):
        out.extend(block)
        out.append("")
        if i < count - 1:
            take = pad // max(1, count)
            for p in range(take):
                out.append(f"{comment} padding {i:03d}-{p:03d}")
            pad -= take
    if pad > 0:
        for p in range(pad):
            out.append(f"{comment} trailing {p:03d}")
    if lang == "cpp":
        out.append("} // namespace")
    out.append("")
    return ext, "\n".join(out)


def generate_tree(
    out: Path,
    files: int,
    seed: int = DEFAULT_SEED,
    one_dir: bool = False,
    pathological: bool = False,
    no_links: bool = False,
) -> dict:
    """Write a deterministic tree and return a small description of it."""
    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True, exist_ok=True)

    written = 0
    for index in range(files):
        ext, body = render_file(index, seed)
        if one_dir:
            path = out / f"file_{index:06d}{ext}"
        else:
            directory = out / f"d{index // FILES_PER_DIR:04d}"
            directory.mkdir(exist_ok=True)
            path = directory / f"file_{index:06d}{ext}"
        path.write_text(body, encoding="utf-8", newline="\n")
        written += 1

    info: dict = {
        "files": written,
        "one_dir": one_dir,
        "pathological": False,
        "extras": {},
    }

    if not pathological:
        return info

    # 1. A file above the CCN AST guard, still counted for LOC.
    huge = out / "huge_above_guard.c"
    chunk = "/* " + "x" * 1000 + " */\n" + "int huge_dummy(int a) { return a; }\n"
    with huge.open("w", encoding="utf-8", newline="\n") as fh:
        written_bytes = 0
        while written_bytes < HUGE_MIB * 1024 * 1024:
            fh.write(chunk)
            written_bytes += len(chunk)
    info["extras"]["huge"] = {
        "path": "huge_above_guard.c",
        "bytes": huge.stat().st_size,
    }

    # 2. Deep nesting: 200 nested `if`s in one function.
    depth = 200
    deep = ["static int deeply_nested(int a)", "{"]
    for level in range(depth):
        deep.append("    " * (level + 1) + f"if (a > {level}) {{")
    deep.append("    " * (depth + 1) + "a += 1;")
    for level in range(depth, 0, -1):
        deep.append("    " * level + "}")
    deep.append("    return a;")
    deep.append("}")
    deep_path = out / "deeply_nested.c"
    deep_path.write_text("\n".join(deep) + "\n", encoding="utf-8", newline="\n")
    info["extras"]["deep"] = {"path": "deeply_nested.c", "depth": depth}

    # 3. A symlink to an existing file (falls back to a copy).
    target = sorted(p for p in out.rglob("*.c") if p.name != "huge_above_guard.c")[0]
    link = out / "linked_source.c"
    mode = "copy"
    if not no_links:
        try:
            os.symlink(os.path.relpath(target, out), link)
            mode = "symlink"
        except (OSError, NotImplementedError) as exc:  # privileges / filesystem
            info["extras"]["link_error"] = str(exc)
    if mode == "copy":
        shutil.copyfile(target, link)
    info["extras"]["link"] = {
        "path": "linked_source.c",
        "target": str(target.relative_to(out)),
        "mode": mode,
    }
    info["pathological"] = True
    return info


# --------------------------------------------------------------- measurement

IS_WINDOWS = os.name == "nt"
if IS_WINDOWS:
    import ctypes
    from ctypes import wintypes

    _kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    _psapi = ctypes.WinDLL("psapi", use_last_error=True)

    class _ProcessMemoryCounters(ctypes.Structure):
        _fields_ = [
            ("cb", wintypes.DWORD),
            ("PageFaultCount", wintypes.DWORD),
            ("PeakWorkingSetSize", ctypes.c_size_t),
            ("WorkingSetSize", ctypes.c_size_t),
            ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
            ("QuotaPagedPoolUsage", ctypes.c_size_t),
            ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
            ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
            ("PagefileUsage", ctypes.c_size_t),
            ("PeakPagefileUsage", ctypes.c_size_t),
        ]

    _kernel32.OpenProcess.restype = wintypes.HANDLE
    _kernel32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    _kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
    _psapi.GetProcessMemoryInfo.argtypes = [
        wintypes.HANDLE,
        ctypes.POINTER(_ProcessMemoryCounters),
        wintypes.DWORD,
    ]

    PROCESS_QUERY_LIMITED_INFORMATION = 0x1000


def _peak_working_set(handle) -> int | None:
    if not IS_WINDOWS or not handle:
        return None
    counters = _ProcessMemoryCounters()
    counters.cb = ctypes.sizeof(counters)
    if not _psapi.GetProcessMemoryInfo(
        handle, ctypes.byref(counters), counters.cb
    ):
        return None
    return int(counters.PeakWorkingSetSize)


def run_once(
    binary: Path,
    tree: Path,
    extra_args: list[str],
    cache_dir: Path | None,
    cwd: Path,
    timeout: float = 1800.0,
    timing: bool = False,
    stdout_to: Path | None = None,
) -> dict:
    """Run ccn once and return wall time, exit code and peak working set."""
    env = os.environ.copy()
    if cache_dir is None:
        env.pop("CCN_CACHE_DIR", None)
    else:
        cache_dir.mkdir(parents=True, exist_ok=True)
        env["CCN_CACHE_DIR"] = str(cache_dir)
    if timing:
        env["CCN_TIMING"] = "1"
    else:
        env.pop("CCN_TIMING", None)

    cmd = [str(binary), str(tree), *extra_args]
    out_fh = None
    if stdout_to is not None:
        stdout_to.parent.mkdir(parents=True, exist_ok=True)
        out_fh = stdout_to.open("wb")
        stdout = out_fh
    else:
        stdout = subprocess.DEVNULL

    started = time.perf_counter()
    proc = subprocess.Popen(
        cmd,
        stdout=stdout,
        stderr=subprocess.PIPE,
        env=env,
        cwd=str(cwd),
    )
    handle = None
    if IS_WINDOWS:
        try:
            handle = _kernel32.OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION, False, proc.pid
            )
        except OSError:
            handle = None
    try:
        _, stderr = proc.communicate(timeout=timeout)
    finally:
        if out_fh is not None:
            out_fh.close()
    elapsed = time.perf_counter() - started
    peak = _peak_working_set(handle)
    if handle:
        _kernel32.CloseHandle(handle)

    return {
        "wall_s": elapsed,
        "exit": proc.returncode,
        "peak_ws": peak,
        "stderr": stderr.decode("utf-8", "replace") if stderr else "",
        "cmd": " ".join(cmd),
    }


def run_once_powershell(
    binary: Path,
    tree: Path,
    extra_args: list[str],
    cache_dir: Path | None,
    cwd: Path,
    workdir: Path,
) -> dict:
    """Same measurement through `Start-Process -PassThru` (independent check)."""
    plan = {
        "binary": str(binary),
        "tree": str(tree),
        "args": extra_args,
        "cache": str(cache_dir) if cache_dir else None,
        "cwd": str(cwd),
    }
    plan_path = workdir / "ps-plan.json"
    plan_path.write_text(json.dumps(plan), encoding="utf-8")
    script = workdir / "ps-run.ps1"
    script.write_text(
        "$ErrorActionPreference = 'Stop'\n"
        f"$plan = Get-Content -LiteralPath '{plan_path}' -Raw | ConvertFrom-Json\n"
        "if ($plan.cache) { $env:CCN_CACHE_DIR = $plan.cache } "
        "else { Remove-Item Env:CCN_CACHE_DIR -ErrorAction SilentlyContinue }\n"
        "Set-Location -LiteralPath $plan.cwd\n"
        "$argvList = @($plan.tree) + @($plan.args)\n"
        "$errPath = Join-Path $plan.cwd 'ps-stderr.txt'\n"
        "$sw = [Diagnostics.Stopwatch]::StartNew()\n"
        "$p = Start-Process -FilePath $plan.binary -ArgumentList $argvList "
        "-NoNewWindow -PassThru -RedirectStandardOutput 'NUL' "
        "-RedirectStandardError $errPath\n"
        "$peak = 0\n"
        "while (-not $p.HasExited) {\n"
        "  try { if ($p.PeakWorkingSet64 -gt $peak) { "
        "$peak = $p.PeakWorkingSet64 } } catch { }\n"
        "  Start-Sleep -Milliseconds 2\n"
        "}\n"
        "$p.WaitForExit()\n"
        "$sw.Stop()\n"
        "if ($peak -le 0) { $peak = $null }\n"
        "[pscustomobject]@{ wall_s = $sw.Elapsed.TotalSeconds; "
        "exit = $p.ExitCode; peak_ws = $peak } | "
        "ConvertTo-Json -Compress\n",
        encoding="utf-8",
    )
    proc = subprocess.run(
        [
            "powershell",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            str(script),
        ],
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        raise RuntimeError(f"powershell driver failed: {proc.stderr.strip()}")
    payload = json.loads(proc.stdout.strip().splitlines()[-1])
    payload["stderr"] = ""
    payload["cmd"] = " ".join([str(binary), str(tree), *extra_args])
    return payload


# ------------------------------------------------------------------ helpers


def fresh_cache(workdir: Path, name: str) -> Path:
    path = workdir / "caches" / name
    if path.exists():
        shutil.rmtree(path)
    path.mkdir(parents=True, exist_ok=True)
    return path


def cache_files(cache_dir: Path) -> list[Path]:
    root = cache_dir / "ccn"
    if not root.exists():
        return []
    return sorted(root.glob("*.json"))


def cache_entry_count(cache_dir: Path) -> int:
    total = 0
    for path in cache_files(cache_dir):
        try:
            total += len(json.loads(path.read_text(encoding="utf-8"))["entries"])
        except (OSError, ValueError, KeyError):
            continue
    return total


def cache_bytes(cache_dir: Path) -> int:
    return sum(p.stat().st_size for p in cache_files(cache_dir))


def median(values: list[float]) -> float:
    ordered = sorted(values)
    if not ordered:
        return 0.0
    mid = len(ordered) // 2
    if len(ordered) % 2:
        return ordered[mid]
    return (ordered[mid - 1] + ordered[mid]) / 2.0


_DURATION_UNITS = {
    "ns": 1e-9,
    "µs": 1e-6,
    "us": 1e-6,
    "ms": 1e-3,
    "s": 1.0,
}


def parse_timing(stderr: str) -> dict:
    """Parse the one `CCN_TIMING` line into seconds per phase."""
    for line in stderr.splitlines():
        if "cache load" not in line:
            continue
        # ccn: cache load 12.3ms (20000 entries) · walk 20345 in 98.1ms · resolve in 1.2s
        result: dict = {}
        tokens = line.split()
        for i, token in enumerate(tokens):
            try:
                if token == "load" and i + 1 < len(tokens):
                    result["cache_load_s"] = _duration(tokens[i + 1])
                elif token == "in" and i + 1 < len(tokens) and i >= 2 and tokens[i - 2] == "walk":
                    result["walk_s"] = _duration(tokens[i + 1])
                elif token == "in" and i + 1 < len(tokens) and "entries)" in tokens[i - 1]:
                    result["walk_s"] = _duration(tokens[i + 1])
                elif token == "in" and i + 1 < len(tokens) and tokens[i - 1] == "resolve":
                    result["resolve_s"] = _duration(tokens[i + 1])
            except (ValueError, IndexError):
                continue
        result["raw"] = line.strip()
        return result
    return {}


def _duration(text: str) -> float:
    for unit, scale in _DURATION_UNITS.items():
        if text.endswith(unit):
            return float(text[: -len(unit)]) * scale
    return float(text)


def machine_info() -> dict:
    info = {
        "platform": sys.platform,
        "python": sys.version.split()[0],
        "cpu_count": os.cpu_count(),
    }
    if IS_WINDOWS:
        try:
            proc = subprocess.run(
                [
                    "powershell",
                    "-NoProfile",
                    "-Command",
                    "Get-CimInstance Win32_Processor | Select-Object Name,"
                    "NumberOfCores,NumberOfLogicalProcessors | ConvertTo-Json -Compress",
                ],
                capture_output=True,
                text=True,
                check=True,
            )
            info["cpu"] = json.loads(proc.stdout.strip() or "null")
        except (OSError, subprocess.SubprocessError, ValueError):
            pass
        try:
            mem = subprocess.run(
                [
                    "powershell",
                    "-NoProfile",
                    "-Command",
                    "(Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory",
                ],
                capture_output=True,
                text=True,
                check=True,
            )
            info["total_memory_bytes"] = int(mem.stdout.strip())
        except (OSError, subprocess.SubprocessError, ValueError):
            pass
        try:
            disk = subprocess.run(
                [
                    "powershell",
                    "-NoProfile",
                    "-Command",
                    "Get-PSDrive C | Select-Object Used,Free | ConvertTo-Json -Compress",
                ],
                capture_output=True,
                text=True,
                check=True,
            )
            info["disk_c"] = json.loads(disk.stdout.strip() or "null")
        except (OSError, subprocess.SubprocessError, ValueError):
            pass
    return info


def build_identity(repo: Path, binary: Path) -> dict:
    identity: dict = {"binary": str(binary)}
    for key, cmd in (
        ("rustc", ["rustc", "--version"]),
        ("cargo", ["cargo", "--version"]),
        ("commit", ["git", "-C", str(repo), "rev-parse", "--short", "HEAD"]),
        ("dirty", ["git", "-C", str(repo), "status", "--porcelain"]),
    ):
        try:
            proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
            identity[key] = proc.stdout.strip()
        except OSError:
            identity[key] = None
    try:
        identity["ccn_version"] = subprocess.run(
            [str(binary), "--version"], capture_output=True, text=True, check=False
        ).stdout.strip()
    except OSError:
        identity["ccn_version"] = None
    return identity


# --------------------------------------------------------------- benchmark


def measure_tree(
    name: str,
    tree: Path,
    files: int,
    args: argparse.Namespace,
    workdir: Path,
    engine: str,
) -> dict:
    """Run the full configuration matrix over one tree."""
    binary = Path(args.binary).resolve()
    base = workdir / "runs" / name
    base.mkdir(parents=True, exist_ok=True)
    html_dir = base / "html"
    html_dir.mkdir(exist_ok=True)

    warm_runs = max(2, args.runs)
    cold_runs = max(1, args.cold_runs)

    plan: list[dict] = []

    def add(label: str, extra: list[str], cache: Path | None, repeats: int,
            capture: Path | None = None, timing: bool = False,
            html: Path | None = None) -> None:
        for run in range(repeats):
            plan.append(
                {
                    "label": label,
                    "args": extra,
                    "cache": cache,
                    "capture": capture,
                    "timing": timing,
                    "html": html,
                    "run": run,
                }
            )

    json_out = base / "cold.json"
    # First touch of a freshly generated tree: the OS file cache is cold too.
    # Its time is kept as a separate data point, never as the cold scan, so the
    # cold number measures ccn's cache rather than NTFS metadata misses.
    add("prewarm", ["--json", "--no-cache"], None, 1)
    # Every cold repeat gets its own cache directory; reusing one would turn
    # repeats 2..N into warm scans.
    cold_caches = [fresh_cache(base, f"cold-{run}") for run in range(cold_runs)]
    for run, cold_cache in enumerate(cold_caches):
        plan.append(
            {
                "label": "cold",
                "args": ["--json"],
                "cache": cold_cache,
                "capture": json_out,
                "timing": False,
                "html": None,
                "run": run,
            }
        )
    # A cold cache now exists; the warm family scans against it.
    cache_shared = cold_caches[0]
    add("cold_timing", ["--json"], fresh_cache(base, "timing-cold"), 1, timing=True)
    add("warm", ["--json"], cache_shared, warm_runs)
    add("warm_timing", ["--json"], cache_shared, 1, timing=True)
    add("warm_verify", ["--json", "--verify-cache"], cache_shared, warm_runs)
    add("no_cache", ["--json", "--no-cache"], None, max(2, args.runs))
    add("no_ccn", ["--json", "--no-ccn"], None, max(2, args.runs))
    add("ci", ["--ci", "--no-report"], cache_shared, max(2, args.runs))
    add("report_json", ["--report-json"], cache_shared, max(2, args.runs))
    html_path = html_dir / f"{name}.html"
    add("html", ["--ci", "--report", str(html_path)], cache_shared,
        max(2, args.runs), html=html_path)

    results: list[dict] = []
    for item in plan:
        extra = list(item["args"])
        capture = item["capture"]
        if engine == "powershell":
            record = run_once_powershell(
                binary,
                tree,
                extra,
                item["cache"],
                base,
                base,
            )
        else:
            record = run_once(
                binary,
                tree,
                extra,
                item["cache"],
                base,
                timing=item["timing"],
                stdout_to=capture if (capture and item["run"] == 0) else None,
            )
        record["label"] = item["label"]
        record["run"] = item["run"]
        record["timing"] = parse_timing(record.get("stderr", ""))
        record["cache_dir"] = str(item["cache"]) if item["cache"] else None
        record["cache_entries"] = (
            cache_entry_count(item["cache"]) if item["cache"] else None
        )
        if item["html"] is not None:
            record["html_bytes"] = (
                item["html"].stat().st_size if item["html"].exists() else 0
            )
        results.append(record)
        if record["exit"] != 0:
            print(
                f"  ! {name}/{item['label']} run {item['run']} exited "
                f"{record['exit']}: {record.get('stderr', '')[:400]}",
                file=sys.stderr,
            )

    # Cache write cost: a fully warm run is not rewritten (`next == old`), so
    # force exactly one entry to change and measure the delta against a warm run
    # taken immediately before it. Alternating A/B pairs keep slow drift (other
    # processes, page-cache pressure) out of the difference.
    rewrites = []
    warm_pairs = []
    for run in range(3):
        victim = sorted(
            p for p in tree.rglob("*") if p.is_file() and p.suffix == ".c"
        )[run % 3]
        baseline = run_once(binary, tree, ["--json"], cache_shared, base)
        warm_pairs.append(baseline["wall_s"])
        time.sleep(0.02)
        victim.write_bytes(victim.read_bytes())
        record = run_once(binary, tree, ["--json"], cache_shared, base)
        record.update(
            label="cache_rewrite",
            run=run,
            timing={},
            cache_dir=str(cache_shared),
            cache_entries=cache_entry_count(cache_shared),
        )
        results.append(record)
        rewrites.append(record["wall_s"])

    save_estimate = (
        median(rewrites) - median(warm_pairs) if rewrites and warm_pairs else None
    )

    # Warm cache stability: every warm/verify run must report the same count.
    entry_counts = sorted(
        {
            r["cache_entries"]
            for r in results
            if r["label"] in ("cold", "warm", "warm_verify")
            and r["cache_entries"] is not None
        }
    )

    scan = {}
    if json_out.exists():
        try:
            document = json.loads(json_out.read_text(encoding="utf-8"))
            scan = {
                "files_analyzed": document.get("files_analyzed"),
                "loc_total": document.get("loc_total"),
                "files_without_ccn": sum(
                    1 for f in document.get("files", []) if f.get("ccn") is None
                ),
            }
        except ValueError:
            scan = {"error": "cold JSON was not parseable"}

    summary = {
        "tree": name,
        "dir": str(tree),
        "files": files,
        "cache_entries": entry_counts,
        "cache_bytes": cache_bytes(cache_shared),
        "cache_save_estimate_s": save_estimate,
        "scan": scan,
        "runs": results,
    }

    # Aggregate per label.
    table = []
    for label in dict.fromkeys(r["label"] for r in results):
        rows = [r for r in results if r["label"] == label]
        walls = [r["wall_s"] for r in rows]
        peaks = [r["peak_ws"] for r in rows if r["peak_ws"]]
        table.append(
            {
                "label": label,
                "n": len(rows),
                "median_s": median(walls),
                "min_s": min(walls),
                "max_s": max(walls),
                "peak_ws": max(peaks) if peaks else None,
                "exit": sorted({r["exit"] for r in rows}),
                "html_bytes": max(
                    (r.get("html_bytes") or 0 for r in rows), default=0
                ),
                "timing": next(
                    (r["timing"] for r in rows if r["timing"]), {}
                ),
            }
        )
    summary["table"] = table
    return summary


def correctness_checks(args: argparse.Namespace, workdir: Path) -> dict:
    """Functional, not just timing: does the cache stay correct at scale?"""
    binary = Path(args.binary).resolve()
    tree = workdir / "checks" / "tree"
    info = generate_tree(tree, args.check_files, seed=args.seed + 7)
    base = workdir / "checks"
    cache = fresh_cache(base, "correctness")
    out = base / "json"

    checks: dict = {"tree_files": info["files"]}

    def scan(tag: str, extra: list[str] | None = None) -> dict:
        target = out / f"{tag}.json"
        record = run_once(
            binary, tree, ["--json", *(extra or [])], cache, base, stdout_to=target
        )
        document = None
        parse_error = None
        try:
            document = json.loads(target.read_text(encoding="utf-8"))
        except (OSError, ValueError) as exc:
            parse_error = str(exc)
        return {
            "exit": record["exit"],
            "json_parseable": document is not None,
            "json_error": parse_error,
            "document": document,
            "entries": cache_entry_count(cache),
            "bytes": target.stat().st_size if target.exists() else 0,
        }

    cold = scan("cold")
    warm1 = scan("warm1")
    warm2 = scan("warm2")
    checks["cold"] = {
        "exit": cold["exit"],
        "json_parseable": cold["json_parseable"],
        "files_analyzed": (cold["document"] or {}).get("files_analyzed"),
        "loc_total": (cold["document"] or {}).get("loc_total"),
        "cache_entries": cold["entries"],
        "json_bytes": cold["bytes"],
    }
    checks["warm"] = [
        {
            "exit": run["exit"],
            "json_parseable": run["json_parseable"],
            "files_analyzed": (run["document"] or {}).get("files_analyzed"),
            "cache_entries": run["entries"],
        }
        for run in (warm1, warm2)
    ]
    checks["files_analyzed_stable"] = (
        cold["document"] is not None
        and warm1["document"] is not None
        and warm2["document"] is not None
        and cold["document"]["files_analyzed"]
        == warm1["document"]["files_analyzed"]
        == warm2["document"]["files_analyzed"]
    )
    checks["entry_count_matches_files"] = (
        cold["entries"] == (cold["document"] or {}).get("files_analyzed")
    )
    checks["entry_count_stable"] = cold["entries"] == warm1["entries"] == warm2["entries"]

    # --- delete 100 files ---------------------------------------------------
    victims = sorted(p for p in tree.rglob("*") if p.is_file())[:100]
    for victim in victims:
        victim.unlink()
    after_delete = scan("after_delete")
    checks["delete"] = {
        "deleted": len(victims),
        "entries_before": warm2["entries"],
        "entries_after": after_delete["entries"],
        "files_analyzed_after": (after_delete["document"] or {}).get(
            "files_analyzed"
        ),
        "dropped_exactly": warm2["entries"] - after_delete["entries"] == len(victims),
    }

    # --- touch one file ----------------------------------------------------
    before_entries = _read_entries(cache)
    touched = sorted(
        p
        for p in tree.rglob("*")
        if p.is_file() and p.suffix == ".c" and p not in victims
    )[0]
    original = touched.read_bytes()
    time.sleep(0.02)  # make the mtime differ even on a coarse filesystem clock
    touched.write_bytes(original)
    after_touch = scan("after_touch")
    after_entries = _read_entries(cache)
    changed = [
        key
        for key in set(before_entries) | set(after_entries)
        if before_entries.get(key) != after_entries.get(key)
    ]
    checks["touch"] = {
        "path": str(touched.relative_to(tree)).replace("\\", "/"),
        "entries_before": len(before_entries),
        "entries_after": len(after_entries),
        "changed_keys": changed,
        "exactly_one_changed": len(changed) == 1
        and changed[0].replace("\\", "/").endswith(touched.name),
        "content_hash_unchanged": (
            changed
            and before_entries.get(changed[0], {}).get("content_hash")
            == after_entries.get(changed[0], {}).get("content_hash")
        ),
        "cache_entries_after": after_touch["entries"],
    }
    return checks


def _read_entries(cache_dir: Path) -> dict:
    entries: dict = {}
    for path in cache_files(cache_dir):
        try:
            entries.update(json.loads(path.read_text(encoding="utf-8"))["entries"])
        except (OSError, ValueError, KeyError):
            continue
    return entries


def pathological_checks(args: argparse.Namespace, workdir: Path) -> dict:
    binary = Path(args.binary).resolve()
    tree = workdir / "pathological" / "tree"
    info = generate_tree(
        tree,
        args.check_files,
        seed=args.seed + 11,
        pathological=True,
        no_links=args.no_links,
    )
    base = workdir / "pathological"
    cache = fresh_cache(base, "cache")
    target = base / "out.json"
    record = run_once(binary, tree, ["--json"], cache, base, stdout_to=target)
    document = None
    try:
        document = json.loads(target.read_text(encoding="utf-8"))
    except (OSError, ValueError) as exc:
        document = None
        error = str(exc)
    else:
        error = None

    by_suffix = {}
    files = (document or {}).get("files", [])
    for entry in files:
        by_suffix.setdefault(entry.get("path", "").split("/")[-1], entry)

    huge_name = info["extras"]["huge"]["path"]
    huge = by_suffix.get(huge_name)
    deep = by_suffix.get(info["extras"]["deep"]["path"])
    link = by_suffix.get(info["extras"]["link"]["path"])
    return {
        "exit": record["exit"],
        "json_parseable": document is not None,
        "json_error": error,
        "files_analyzed": (document or {}).get("files_analyzed"),
        "huge": {
            "expected_bytes": info["extras"]["huge"]["bytes"],
            "present": huge is not None,
            "ccn_is_null": bool(huge) and huge.get("ccn") is None,
            "loc_total": (huge or {}).get("loc_total"),
            "loc": (huge or {}).get("loc"),
        },
        "deep": {
            "present": deep is not None,
            "ccn_is_null": bool(deep) and deep.get("ccn") is None,
            "tier": ((deep or {}).get("ccn") or {}).get("tier"),
            "max_ccn": ((deep or {}).get("ccn") or {}).get("max"),
            "functions": ((deep or {}).get("ccn") or {}).get("functions"),
        },
        "link": {
            "mode": info["extras"]["link"]["mode"],
            "link_error": info["extras"].get("link_error"),
            "present": link is not None,
            "loc_total": (link or {}).get("loc_total"),
        },
        "peak_ws": record["peak_ws"],
        "wall_s": record["wall_s"],
    }


def print_summary(summaries: list[dict]) -> None:
    header = (
        f"{'tree':<14}{'config':<14}{'runs':>5}{'median s':>10}"
        f"{'min s':>9}{'max s':>9}{'files/s':>10}{'peak MB':>9}"
    )
    print(header)
    print("-" * len(header))
    for summary in summaries:
        for row in summary["table"]:
            files_per_s = (
                summary["files"] / row["median_s"] if row["median_s"] else 0.0
            )
            peak = (row["peak_ws"] or 0) / (1024 * 1024)
            print(
                f"{summary['tree']:<14}{row['label']:<14}{row['n']:>5}"
                f"{row['median_s']:>10.3f}{row['min_s']:>9.3f}"
                f"{row['max_s']:>9.3f}{files_per_s:>10.0f}{peak:>9.1f}"
            )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Generate and measure synthetic large ccn repositories.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("--files", type=int, default=1_000,
                        help="number of generated source files (default 1000)")
    parser.add_argument("--out", type=Path, default=None,
                        help="tree directory (default: <workdir>/n<files>)")
    parser.add_argument("--one-dir", action="store_true",
                        help="put every file in one directory")
    parser.add_argument("--pathological", action="store_true",
                        help="add an above-guard file, deep nesting and a link")
    parser.add_argument("--no-links", action="store_true",
                        help="use a copy instead of a symlink")
    parser.add_argument("--seed", type=int, default=DEFAULT_SEED)
    parser.add_argument("--make-only", action="store_true",
                        help="generate the tree and stop")
    parser.add_argument("--driver", action="store_true",
                        help="measure an existing tree (this is the benchmark)")
    parser.add_argument("--all", action="store_true",
                        help="generate and measure 1k/5k/20k, one-dir and pathological")
    parser.add_argument("--workdir", type=Path,
                        default=Path(os.environ.get("TEMP", ".")) / "ccn-bench",
                        help="scratch directory for caches and outputs")
    parser.add_argument("--binary", default="target/release/ccn.exe",
                        help="ccn executable to measure")
    parser.add_argument("--runs", type=int, default=3,
                        help="repeat count for warm/config runs (default 3)")
    parser.add_argument("--cold-runs", type=int, default=3,
                        help="repeat count for the cold run (default 3; each repeat has a fresh cache)")
    parser.add_argument("--sizes", default="1000,5000,20000",
                        help="comma separated sizes for --all")
    parser.add_argument("--check-files", type=int, default=1_000,
                        help="size of the correctness/pathological trees")
    parser.add_argument("--engine", choices=("python", "powershell"),
                        default="python",
                        help="measurement backend (default: in-process handle)")
    parser.add_argument("--result", type=Path, default=None,
                        help="write the raw JSON result here")
    parser.add_argument("--skip-correctness", action="store_true")
    parser.add_argument("--keep", action="store_true",
                        help="keep generated trees (they live under --workdir)")
    args = parser.parse_args(argv)

    args.binary = str(Path(args.binary).resolve())
    if not Path(args.binary).is_file():
        parser.error(f"binary not found: {args.binary} (run cargo build --release)")

    repo = Path(__file__).resolve().parent.parent
    workdir = args.workdir.resolve()
    workdir.mkdir(parents=True, exist_ok=True)

    report: dict = {
        "generated_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "machine": machine_info(),
        "build": build_identity(repo, Path(args.binary)),
        "config": {
            "seed": args.seed,
            "runs": args.runs,
            "engine": args.engine,
            "huge_mib": HUGE_MIB,
        },
        "trees": [],
    }

    # --- plain generation mode -------------------------------------------
    if not args.driver and not args.all:
        out = args.out or workdir / "trees" / f"n{args.files}"
        info = generate_tree(
            out,
            args.files,
            seed=args.seed,
            one_dir=args.one_dir,
            pathological=args.pathological,
            no_links=args.no_links,
        )
        print(json.dumps({"out": str(out), **info}, indent=2))
        return 0

    # --- --all: build every tree first -----------------------------------
    trees: list[tuple[str, Path, int]] = []
    if args.all:
        sizes = [int(s) for s in args.sizes.split(",") if s.strip()]
        for size in sizes:
            tree = workdir / "trees" / f"n{size}"
            print(f"generating {tree} ({size} files)...", file=sys.stderr)
            generate_tree(tree, size, seed=args.seed)
            trees.append((f"n{size}", tree, size))
        flat_size = max(sizes)
        flat = workdir / "trees" / f"n{flat_size}-flat"
        print(f"generating {flat} ({flat_size} files, one dir)...", file=sys.stderr)
        generate_tree(flat, flat_size, seed=args.seed, one_dir=True)
        trees.append((f"n{flat_size}-flat", flat, flat_size))
        patho = workdir / "trees" / "pathological"
        print(f"generating {patho}...", file=sys.stderr)
        info = generate_tree(
            patho,
            args.check_files,
            seed=args.seed,
            pathological=True,
            no_links=args.no_links,
        )
        trees.append(("pathological", patho, info["files"] + 3))
    else:
        tree = args.out or workdir / "trees" / f"n{args.files}"
        if not tree.exists():
            generate_tree(
                tree,
                args.files,
                seed=args.seed,
                one_dir=args.one_dir,
                pathological=args.pathological,
                no_links=args.no_links,
            )
        trees.append((args.out.name if args.out else f"n{args.files}", tree, args.files))

    if args.make_only:
        report["trees"] = [{"name": n, "dir": str(p), "files": f} for n, p, f in trees]
        if args.result:
            args.result.parent.mkdir(parents=True, exist_ok=True)
            args.result.write_text(json.dumps(report, indent=2), encoding="utf-8")
        return 0

    summaries = []
    for name, tree, size in trees:
        print(f"measuring {name} ({size} files)...", file=sys.stderr)
        summaries.append(measure_tree(name, tree, size, args, workdir, args.engine))
    report["trees"] = summaries

    if not args.skip_correctness:
        print("correctness checks...", file=sys.stderr)
        report["correctness"] = correctness_checks(args, workdir)
        print("pathological checks...", file=sys.stderr)
        report["pathological"] = pathological_checks(args, workdir)

    print_summary(summaries)
    if args.result:
        args.result.parent.mkdir(parents=True, exist_ok=True)
        args.result.write_text(json.dumps(report, indent=2), encoding="utf-8")
        print(f"\nraw results: {args.result}")

    if not args.keep:
        print(
            f"\nscratch trees and caches remain in {workdir} "
            "(remove them when the results are recorded)",
            file=sys.stderr,
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
