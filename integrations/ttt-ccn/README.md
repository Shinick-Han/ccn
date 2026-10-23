# CCN plugin for TTT (spike)

A thin integration between [TTT](https://github.com/eugenioenko/ttt) (MIT) and
the ccn binary. TTT owns the editor, LSP, Git, terminal and layout; ccn owns
the complexity analysis.

This is an integration spike, not part of the ccn executable.

## What it does

- Adds **CCN: Open inspector** and **CCN: Refresh** to the TTT command palette.
- `Ctrl+K C` opens the same inspector.
- Opens a **right-side drawer** with:
  - the current file's max CCN, function count and `>10` count;
  - functions sorted by CCN, with `L<line>` and a CCN badge;
  - clicking a function opens the file at that line.
- Refreshes after `file.open` and `file.save`.
- Adds a compact `CCN <max> · <n> hot` status item.

## Install

TTT looks for workspace plugins at `<workspace>/plugins/<name>` and user
plugins at `~/.config/ttt/plugins/<name>` (on Windows this is
`C:\Users\<user>\.config\ttt\plugins\<name>`).

Workspace-local:

```powershell
New-Item -ItemType Directory .\plugins -Force
Copy-Item -Recurse integrations\ttt-ccn .\plugins\ccn
```

User-wide (Windows):

```powershell
Copy-Item -Recurse integrations\ttt-ccn "$HOME\.config\ttt\plugins\ccn"
```

Unix:

```sh
mkdir -p ~/.config/ttt/plugins
cp -R integrations/ttt-ccn ~/.config/ttt/plugins/ccn
```

Then:

1. Put `ccn` on `PATH` (`ccn --version` and `ccn doctor` should work).
2. Start TTT from the workspace (`ttt .` or `ttt . src/main.c`).
3. Approve the plugin's requested permissions on first load.
4. Run **CCN: Open inspector** (`Ctrl+P`, then `CCN: Open inspector`) or
   `Ctrl+K C`.

The plugin is intentionally sandboxed; it requests only drawer, commands,
keybindings, editor reads, file events, and allow-listed `ccn` execution.
It does not write files or use the network.

## Spike limitations

- It runs `ccn <active-file> --json` and displays file-level detail. It does
  not yet show baseline review, P4 changelists, or the full repository view.
- TTT's own editor and LSP remain authoritative; ccn does not replace them.
- Function identity/detail comes from ccn's normal scanner, so unsupported
  grammars show the same heuristic limits as the ccn dashboard.
- The drawer opens automatically for the first supported source file in a
  session, then refreshes on open/save. Use **CCN: Open inspector** to reopen
  it after closing.

## Verified

Native Windows smoke test against TTT v1.9.0 with a C fixture:

```text
right drawer: CCN · main.c
max CCN 4 · 3 functions · 0 over 10
process  L3  4
simple   L12 1
main     L14 1
```

The plugin also correctly reports `ccn: path does not exist` for TTT's
unsaved `untitled` buffer instead of crashing.
