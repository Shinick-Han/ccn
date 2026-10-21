#!/usr/bin/env python3
"""Explain why ccn did or did not produce per-function numbers.

Usage:
    python3 tools/diagnose.py [PATH] [--ccn /path/to/ccn]

Runs `ccn <PATH> --json` and groups every file by language and CCN tier, so a
"no parsed functions" report can be traced to one of three causes:

  * the language has no grammar (heuristic tier),
  * the file could not be read (ccn is null),
  * the file parsed but contained no functions.

Python 3.8+; no third-party packages.
"""

import argparse
import collections
import json
import shutil
import subprocess
import sys

# Languages with a tier-1 tree-sitter grammar. Kept in sync with src/ccn/types.rs.
EXACT_LANGUAGES = {
    "JavaScript", "TypeScript", "TSX", "Python", "Rust", "Go",
    "Java", "C", "C++", "C#",
}

HINTS = {
    "Kotlin": "no grammar yet (tier 3)",
    "Swift": "no grammar yet (tier 3)",
    "Ruby": "no grammar yet (tier 3)",
    "PHP": "no grammar yet (tier 3)",
    "Scala": "no grammar yet (tier 3)",
    "Lua": "no grammar yet (tier 3)",
    "Perl": "no grammar yet (tier 3)",
    "R": "no grammar yet (tier 3)",
    "Dart": "no grammar yet (tier 3)",
    "Elixir": "no grammar yet (tier 3)",
    "Objective-C": "no grammar yet (tier 3)",
    "Vue": "no grammar yet (tier 3)",
    "Svelte": "no grammar yet (tier 3)",
}


def find_ccn(explicit):
    if explicit:
        return explicit
    for name in ("ccn", "ccn.exe"):
        found = shutil.which(name)
        if found:
            return found
    sys.exit("ccn not found on PATH; pass --ccn /path/to/ccn")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("path", nargs="?", default=".")
    parser.add_argument("--ccn", default=None)
    args = parser.parse_args()

    binary = find_ccn(args.ccn)
    proc = subprocess.run(
        [binary, args.path, "--json"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if proc.returncode not in (0, 1):
        sys.stderr.write(proc.stderr.decode("utf-8", "replace"))
        sys.exit(proc.returncode)
    scan = json.loads(proc.stdout.decode("utf-8"))

    files = scan["files"]
    print("ccn:", binary)
    print("root:", scan["root"])
    print("files:", scan["files_analyzed"], "functions:", scan["functions_total"])
    print()

    per_lang = collections.defaultdict(lambda: collections.Counter())
    for f in files:
        ccn = f.get("ccn")
        if ccn is None:
            bucket = "none"
        elif ccn.get("tier") in ("exact", "best-effort"):
            bucket = "exact"
        else:
            bucket = "heuristic"
        per_lang[f["lang"]][bucket] += 1
        per_lang[f["lang"]]["files"] += 1
        per_lang[f["lang"]]["loc"] += f["loc"]

    print("%-16s %6s %7s %7s %7s  %s" % ("language", "files", "exact", "heur", "none", "note"))
    for lang in sorted(per_lang, key=lambda k: -per_lang[k]["loc"]):
        row = per_lang[lang]
        print("%-16s %6d %7d %7d %7d  %s" % (
            lang, row["files"], row["exact"], row["heuristic"], row["none"],
            HINTS.get(lang, ""),
        ))

    unreadable = [f for f in files if f.get("ccn") is None]
    if unreadable:
        print()
        print("files with no CCN summary (unreadable, oversize, or skipped):")
        for f in unreadable[:20]:
            print("  %s  (%s, %d lines)" % (f["path"], f["lang"], f["loc"]))

    heuristic = [f for f in files if (f.get("ccn") or {}).get("tier") == "heuristic"]
    unsupported = [f for f in heuristic if f["lang"] not in EXACT_LANGUAGES]
    if unsupported:
        langs = sorted({f["lang"] for f in unsupported})
        print()
        print("heuristic because the language has no grammar:", ", ".join(langs))
        print("  (these files have a file-level number only; they never enter the")
        print("   function histogram or the health score)")

    failed = [f for f in heuristic if f["lang"] in EXACT_LANGUAGES]
    if failed:
        print()
        print("heuristic despite a supported language (parse failure):")
        for f in failed[:10]:
            print("  %s  (%s)" % (f["path"], f["lang"]))
        print("  a parse failure usually means the file is truncated, uses an")
        print("  unsupported dialect, or is not valid UTF-8.")


if __name__ == "__main__":
    main()
