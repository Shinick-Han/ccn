# `ccn brief`

`ccn brief` is a bounded, deterministic context document for humans and coding
agents. It is built from the normal ccn scan, so it uses the same parser,
generated/vendored exclusions and per-file cache.

It does not call an LLM, use the network, or write to the workspace.

## Examples

```console
$ ccn brief .
$ ccn brief . --top 12 --min-ccn 15
$ ccn brief . --baseline HEAD~1
$ ccn brief . --json --no-source
```

`--baseline` uses the same comparison as `--fail-on-new`; rows include
`base_ccn`, `change: added|worsened` and `reasons`.

## Selection

1. Scan `PATH`.
2. Flatten `files[].ccn.detail[]`, skipping generated/vendored files.
3. Keep functions with `ccn >= --min-ccn` (default 10).
4. Sort by CCN descending, then path and line.
5. Keep the first `--top` rows (default 8).
6. Attach at most `--max-lines` source lines per function (default 120), unless
   `--no-source` is set.

With `--baseline`, the candidate set is `baseline.changes` instead of the
workspace function list.

## Markdown shape

````text
# CCN brief

Root: `C:/work/project`
Mode: workspace hotspots
Threshold: CCN >= 10
Functions: 124 measured · 8 returned

## 1. `src/parser.c:parse_expr` — CCN 21
- line 143–188, 46 nloc, nesting 6, 3 params
- reasons: ccn +19, nesting +5        # only with --baseline

```c
int parse_expr(...) {
  ...
}
```
````

## JSON shape

```json
{
  "root": "C:/work/project",
  "mode": "workspace",
  "baseline_complete": true,
  "threshold": 10,
  "total_functions": 124,
  "returned": 8,
  "functions": [
    {
      "path": "src/parser.c",
      "name": "parse_expr",
      "identity": "parse_expr(Token*,int)",
      "line": 143,
      "end_line": 188,
      "ccn": 21,
      "nloc": 46,
      "params": 3,
      "max_nesting": 6,
      "reasons": [],
      "source": "int parse_expr(...) { ... }"
    }
  ]
}
```

`source`, `params`, `base_ccn` and `kind` are omitted when not applicable.
`truncated` is present only when the source was capped. `baseline_complete` is
false when the comparison could not cover every expected file.

## What it is not

This is pre-processing, not an automatic refactoring plan. It does not include
callers, tests, ownership or diff hunks. Those are the next candidates if the
brief proves useful in real agent runs.
