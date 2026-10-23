# OSS terminal IDE candidates (2026-10-23)

Research for improving ccn's editor-first UI without rebuilding every IDE
surface. ccn is MIT, Rust, Windows/Linux/macOS, offline-first, with an embedded
Neovim and complexity/review data.

## Hard gates

1. Terminal-native, low-latency, keyboard + mouse.
2. Windows is a first-class target.
3. MIT/Apache/BSD code can be reused in ccn. GPL/AGPL code cannot be copied
   into the MIT product; MPL requires file-level compliance.
4. LSP navigation, build/test tasks, SCM, and a useful right-side inspector must
   be reachable without an Electron app.
5. Existing ccn CCN/review/P4 work should not be thrown away without evidence.

## Shortlist

| Project | Stack / license | Relevant strengths | Main gaps / risk |
|---|---|---|---|
| [TTT](https://github.com/eugenioenko/ttt) | Go, **MIT**, active, ~760 stars | TUI that targets VS Code/Sublime users; LSP completion/nav/format; Git stage/commit/blame and PR review; integrated terminal; multi-root; command palette; mouse; **Lua plugins with left/right drawers, sidebar, bottom panel, custom editor tabs, diagnostics/decorations, terminal control and allow-listed exec** | Not Rust; own editor, not Neovim; plugin API is the integration boundary |
| [Fresh](https://github.com/sinelaw/fresh) | Rust, **GPL-3.0**, ~9.3k stars | Closest full IDE feature set: LSP (completion, refs, rename, code actions), Git review/log/blame, live grep, split panes, integrated terminal, SSH, huge files, TypeScript plugins | **GPL-3.0 blocks copying into MIT ccn**; own editor; large Rust codebase |
| [Tuim](https://github.com/Tuim-editor/Tuim) | Zig, **MIT**, v0.3, ~8 stars | Native TUI shell + **embedded Neovim**; right/bottom panel, explorer, Git stage/commit, terminal, LSP through bundled Neovim, Zen mode, AI panel | Zig 0.16; Linux-first (Windows/WSL unverified); very early, tiny community; fork means Zig ownership |
| [Herdr](https://github.com/herdrdev/herdr) | Rust, **Apache-2.0**, ~45k stars | Terminal workspace, split panes, detach/restore, SSH machines, plugins, agent status; very active | Not an editor/LSP shell; it hosts terminals (TTT can run as a Herdr plugin) |
| [Helix](https://github.com/helix-editor/helix) | Rust, **MPL-2.0**, ~46k stars | Strong modal editor, tree-sitter, LSP, terminal UI | Not an IDE shell; no right inspector/CCN; fork is huge and MPL file obligations |
| [Hunk](https://github.com/modem-dev/hunk) | TypeScript, **MIT**, ~9.6k stars | Review-first terminal diff UI, multi-file stream, split/stack, annotations | Diff reviewer only; not an editor |
| [Lazydiff](https://github.com/Ataraxy-Labs/lazydiff) | Rust, **MIT**, ~165 stars | Review-focused TUI, PR queue, semantic diff, review state | Alpha; Git/GitHub only |
| [Lazygit](https://github.com/jesseduffield/lazygit) | Go, **MIT**, ~83k stars | Best-in-class Git staging/commit/rebase UI | Git only; not an editor |
| [Yazi](https://github.com/sxyazi/yazi) | Rust, **MIT**, ~43k stars | Fast terminal file manager, previews, plugins | File manager only |
| [Orbiton](https://github.com/xyproto/orbiton) | Go, **BSD-3**, ~714 stars | Config-free editor/IDE, LSP, jump-to-error, gdb/dlv | Simpler shell; no rich right pane |
| [Cleecode](https://github.com/msavox/cleecode) | Rust, **NOASSERTION**, ~88 stars | Micro-style editor, file tree, terminals, menus | License unresolved; reject for reuse |
| [OpenCode](https://github.com/opencode-ai/opencode) | Go, **MIT**, ~13.8k stars | Strong TUI, LSP diagnostics, side-by-side diff, provider routing | Agent-first; not a code editor |

## What solves the empty right pane

- **TTT**: a plugin can register a sidebar panel and a drawer anchored to the
  **right** side of the editor, with labels, lists, trees, input, buttons,
  diagnostics/decorations and terminal control. This is the fastest known route
  to a CCN inspector/review surface without writing another TUI shell.
- **Tuim**: has native right/bottom panels next to an embedded Neovim; the
  closest architecture to ccn, but early and Zig-specific.
- **Fresh**: split panes and docks, but GPL; use only as a feature/UX reference
  unless the product relicenses.
- **Herdr**: excellent pane/session host, not a code inspector; only relevant if
  ccn becomes a pane/plugin inside a larger workspace.

## Decision

### 1. Do not fork Fresh into ccn

Fresh has the most complete IDE behavior, but it is **GPL-3.0**. Copying or
deriving it into the MIT ccn binary would change ccn's distribution terms.
Keeping it as a separate optional process is possible, but does not give ccn
ownership of the UI.

### 2. TTT is the best shell candidate

TTT already contains most IDE work ccn is missing:

- LSP completion/navigation/formatting and diagnostics;
- Git staging, commits, blame, diff and PR review;
- integrated terminal, multi-root, command palette, mouse;
- a plugin system with **right-side drawers**, sidebar/bottom panels, editor
  tabs, commands and keybindings;
- MIT license, Windows builds, active releases.

The mismatch is language and editor: TTT is Go with its own editor, while ccn
is Rust + Neovim. Forking TTT means owning a Go UI; using it as a plugin host
keeps ccn's Rust engine.

### 3. Tuim is the best architecture reference, not the first fork

Tuim is the only candidate that is both MIT and built around **embedded
Neovim + native right/bottom panels**. If “real Neovim” is non-negotiable and
we want the ccn UI shell to be Zig, Tuim is the closest base. But v0.3, Zig
0.16, Linux-first packaging and an 8-star community make it a high-risk fork.

### 4. Recommended next step: TTT plugin spike

Build a **thin CCN plugin for TTT** before forking anything:

1. A right-side drawer or sidebar tab showing the selected file's CCN summary
   and top functions.
2. A command that runs `ccn` through the plugin's allow-listed `system.exec`
   and renders JSON/function detail.
3. A review command that opens the changed files and CCN deltas; use TTT's
   existing Git/PR surface rather than rebuilding it.
4. If the plugin proves useful, decide whether to (a) ship it as the supported
   integration, (b) fork TTT and make the CCN panel native, or (c) keep ccn's
   own shell and borrow TTT's drawer/panel interaction patterns.

This avoids a multi-month Go/Zig rewrite before validating that TTT's shell is
the right product surface.

## Spike result: TTT plugin works

A first-party spike now lives at [`integrations/ttt-ccn`](../../integrations/ttt-ccn/README.md).
It adds a right drawer showing the active file's CCN maximum, function count,
`>10` count and a clickable function list, with a status item and refresh on
open/save.

Native Windows smoke test (TTT v1.9.0, C fixture) rendered:

```text
CCN · main.c
max CCN 4 · 3 functions · 0 over 10
process  L3  4
simple   L12 1
main     L14 1
```

This validates the composition path: TTT provides the IDE shell and drawer,
while ccn remains the Rust analysis engine. It does not yet cover full-repo
review or P4.

## Not selected

- **Helix**: excellent editor, but no IDE shell/right inspector; fork cost too
  high and MPL file obligations complicate direct reuse.
- **Herdr**: strong workspace/session layer, but not an editor. Best used
  *around* TTT or ccn, not as the editor itself.
- **Hunk / Lazydiff / Lazygit / Yazi**: compose candidates for review, Git and
  files; they do not replace the editor shell. ccn already has a review view;
  Hunk/Lazydiff are useful UX references or optional external viewers.
- **Orbiton / Cleecode / OpenCode**: too narrow, license unresolved, or
  agent-first.

## Sources checked

- TTT architecture, README, plugin-authoring guide, plugin API and permission
  model in a shallow clone (`eugenioenko/ttt`, MIT).
- Fresh official site, README, LSP docs and repository metadata
  (`sinelaw/fresh`, GPL-3.0).
- Tuim README, architecture, right/bottom panel source and repository metadata
  (`Tuim-editor/Tuim`, MIT).
- Herdr README and repository metadata (`herdrdev/herdr`, Apache-2.0).
- Helix, Hunk, Lazydiff, Lazygit, Yazi, Orbiton, Cleecode and OpenCode
  repository metadata / public docs.
- `tui-design-library` local references: Herdr, OpenCode, HTML-TERMINAL,
  TUI-PLAYBOOK.

Stars and release dates are signals only; they were checked on 2026-10-23 and
are not a substitute for a build/UX trial.
