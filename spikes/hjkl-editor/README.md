hjkl-editor spike
=================

Comparison spike, not a product feature. Question: can the `hjkl` Vim engine
be embedded in our ratatui UI as an alternative to embedding Neovim in a PTY?
Short answer: yes, it embeds cleanly, but it is a Vim *engine*, not an editor -
and it costs us an MSRV bump plus a second ratatui. See RECOMMENDATION.

Nothing outside spikes/hjkl-editor/ was modified. Not part of the main
workspace (own `[workspace]`, own Cargo.lock). Nothing committed.

    spikes/hjkl-editor/
      Cargo.toml      exact pins
      src/main.rs     600 lines: the whole host
      smoke.ps1       headless functional test (10 cases)
      README.md       this file


RUN
---

    cargo build --manifest-path spikes/hjkl-editor/Cargo.toml
    cargo build --manifest-path spikes/hjkl-editor/Cargo.toml --no-default-features

    # real TUI (needs a terminal)
    spikes/hjkl-editor/target/debug/hjkl-spike.exe path/to/file.cpp

    # scripted: feed keys, print state, verify :w wrote the file
    spikes/hjkl-editor/target/debug/hjkl-spike.exe --headless f.cpp \
        --keys "Goline<Esc>:w<CR>" --no-syntax

    # render one frame through ratatui's TestBackend and dump it as ASCII,
    # so the layout can be checked without a TTY
    spikes/hjkl-editor/target/debug/hjkl-spike.exe f.cpp --render-once

    # functional test (10 cases: insert, o, dd, dot-repeat, undo, yy/p,
    # Vd, x, :%s, CRLF round-trip)
    pwsh spikes/hjkl-editor/smoke.ps1
    pwsh spikes/hjkl-editor/smoke.ps1 -Exe spikes/hjkl-editor/target/release/hjkl-spike.exe

Key script syntax: literal chars, plus `<Esc> <CR> <BS> <Tab> <C-x>`.
`--no-syntax` skips the tree-sitter attach attempt.


EXACT DEPENDENCIES (pinned, all `=0.43.4`)
-------------------------------------------

Direct:

    hjkl-buffer      =0.43.4   rope buffer + `View` + `Viewport`
    hjkl-buffer-tui  =0.43.4   `BufferView` ratatui Widget (paints the text)
    hjkl-engine      =0.43.4   `Editor<View, Host>`, motions, options
    hjkl-engine-tui  =0.43.4   crossterm KeyEvent -> engine Input, style conv
    hjkl-vim         =0.43.4   the FSM: `vim_editor()` + `dispatch_input()`
    hjkl-ex          =0.43.4   ex-command registry, `try_dispatch()`
    hjkl-bonsai      =0.43.4   optional, feature `syntax` (tree-sitter)
    ratatui          =0.30.2   (resolved; `^0.30`)
    crossterm        =0.29.0   (resolved; `^0.29`)

Transitive hjkl crates that also land in the lock (11 total):
hjkl-fs, hjkl-keymap, hjkl-lang, hjkl-theme, hjkl-vim-types, hjkl-xdg.
tree-sitter 0.27.1, ropey 1.6.1, regex 1.13.1. 236 packages in Cargo.lock
with default features.

Note the crate list differs from the task brief: `hjkl-editor-tui` is only
form/prompt/spinner widgets - it is NOT the renderer. The buffer renderer is
`hjkl-buffer-tui`. The engine docs point at `hjkl_engine::ex`, but that module
does not exist; ex lives in the separate `hjkl-ex` crate. Both are stale doc
links in 0.43.4.

Build features: `default = ["syntax"]`; `--no-default-features` drops
hjkl-bonsai + tree-sitter entirely (used for the size measurement below).


WHY IT IS EMBEDDABLE (the win)
------------------------------

The engine really is app-independent. No `hjkl-app`, no `hjkl-layout`, no
`hjkl-statusline`, no full-binary dependency. Construction is:

    let mut ed = hjkl_vim::vim_editor(
        hjkl_buffer::View::new(),
        hjkl_engine::DefaultHost::new(),
        hjkl_engine::Options::default(),
    );
    ed.set_content(&text);

Input is one function (plus one adapter for crossterm):

    let input = hjkl_engine_tui::crossterm_to_input(key_event);
    hjkl_vim::dispatch_input(&mut ed, input);

Ex commands are a registry the host calls and interprets:

    let reg = hjkl_ex::default_registry::<DefaultHost>();
    match hjkl_ex::try_dispatch(&reg, &mut ed, "w") { ExEffect::Save => ... }

Rendering is a real `ratatui::widgets::Widget` (`hjkl_buffer_tui::BufferView`)
that paints text, syntax spans, selection, cursor cell, cursorline and the
gutter in one pass. Geometry goes the other way - the host publishes the
window size into the engine's `Host` (that is the *whole* scroll integration):

    let vp = ed.host_mut().viewport_mut();
    vp.width = area.width; vp.height = area.height; vp.text_width = w;

So: engine in, styled cells out. The full-app crates are genuinely optional.


WHAT THE HOST STILL HAS TO WRITE (the cost)
-------------------------------------------

hjkl-engine is a Vim *FSM*, not an editor. Out of the box there is no
command line, no file IO, no status line, no sidebar, no syntax plumbing.
All of that is host code here:

1. The `:` command line. `:` is never seen by the engine; the host collects
   chars, then calls `try_dispatch`. ~30 lines.
2. Save/quit policy. `ExEffect::Save | SaveAs | Quit{save,force}` -> host
   writes the file. ~30 lines. `:q` alone correctly does not write (verified).
3. `BufferView` construction: 34 named public fields, no `new()`, no
   `Default`, no builder. ~40 lines of struct literal, and this is the
   surface most exposed to 0.43.x churn.
4. Syntax plumbing: spans -> `EditorRatatuiExt::install_ratatui_syntax_spans`
   plus a per-row char-offset conversion (engine wants char offsets, not
   bytes). ~95 lines, and it still needs a grammar (see below).
5. Mode/status/cursor display: `VimEditorExt::vim_mode()` + `Editor::cursor()`.
   ~10 lines.
6. EOL handling. See the two file-format bugs below. ~10 lines.

Measured LOC, src/main.rs: 600 total / 566 non-blank / 21 comment-only.
Rough split:

    core embedding (open, handle, run_ex, save, run_ui)   ~150
    BufferView literal + layout + status bar (draw)       ~120
    syntax attach + span mapping + style conversion        ~95
    sidebar placeholder                                    ~38
    headless key-script harness + render-once              ~130
    CLI/arg parsing, struct decls, small helpers           ~70

~150 lines is the honest answer for "an embeddable modal editor with :w/:q".


FILE-FORMAT BUGS FOUND (real, reproducible)
-------------------------------------------

1. `content()` always appends a trailing "\n", while `set_content()` keeps a
   trailing empty row. Without compensation, opening any file that already
   ends in "\n" and running a bare `:w` grows the file by one byte, every
   save. This is undocumented. Fixed here by stripping exactly one trailing
   newline on load.

2. There is no `fileformat`/`ff` option anywhere in the engine or ex
   registry, and CRLF is not preserved: `a\r\nb\r\n` + bare `:w` comes back
   as `a\r\nb\n\n`, and editing a line can drop its `\r` onto a new line.
   The host must normalise CRLF -> LF on load and restore on save (done here,
   6 lines); `smoke.ps1` asserts a byte-identical CRLF round-trip.

Files with no trailing newline gain one (vim's `eol` behaviour). Acceptable,
but it means saves are not byte-idempotent in that case.


TREE-SITTER (hjkl-bonsai): wired, but not self-contained
--------------------------------------------------------

The API is usable - `GrammarRegistry::embedded()` (manifest only),
`GrammarLoader`, `Grammar::load_from_path`, `Highlighter::highlight()` ->
`HighlightSpan { byte_range, capture }`, `DotFallbackTheme::dark()` for
capture -> colour. But:

* No grammar is bundled. Not one. tree-sitter-cpp/rust/etc. are not
  dependencies of hjkl-bonsai.
* Grammars are `.so`/`.dll` files `dlopen`ed (libloading) from
  `$XDG_DATA_HOME/bonsai/grammars` - on this Windows box that resolves to
  `C:\Users\user\.local/share/bonsai/grammars` (note the mixed separators,
  a small hjkl-xdg bug).
* `Grammar::load()` on a cache miss **clones the grammar repo and compiles
  it with the system C compiler, then dlopens it** ("downloads and builds
  remote code" - their wording). `lookup_only()` is the no-network variant.

This spike therefore uses `lookup_only()` and never downloads. Result at
runtime, reported honestly in the status line:

    syntax: off (grammar 'cpp' not installed in C:\Users\user\.local/share/bonsai/grammars)

Consequence: the highlight code path compiles and is wired end to end, but it
was never exercised at runtime here, because installing a grammar means
network + a C toolchain and running fetched code. I did not want to do that
to the user's machine for a spike, and did not verify it. Treat syntax as
"API exists, packaging story does not".

This is a duplicate of work CCN already does: the main crate statically links
tree-sitter-c/cpp/rust/go/java/js/python/ts/c-sharp already.


EDITOR FEATURES OBSERVED WORKING (headless, assert-based)
---------------------------------------------------------

    i/a/A/I, o/O, Esc, typed text          insert mode, correct content
    h j k l, w b e, 0 $ ^, G gg, counts    motions
    x, dd, yy, p, P                        operators
    .  (dot-repeat)                        repeats the last change
    u                                      undo
    V + j + d (visual line)                selection delete
    :w  :w <path>  :q  :q!                 save / quit semantics
    :%s/foo/baz/g                          ex substitute (returns count)
    /<pattern>                             in-engine search prompt code path

Not exercised here: visual-block, text objects (`ciw`, `di(`), marks,
registers beyond unnamed, macros, folds, LSP, completions, pickers, splits,
tabs. The engine has code for most of these (`hjkl-engine` motions.rs 2.6k
lines, text_object.rs 1.9k, hjkl-vim pending/visual ~4k) but the host must
drive each subsystem.

Mode reporting works: `VimMode::{Normal,Insert,Visual,VisualLine,VisualBlock}`
via `VimEditorExt::vim_mode()`. Insert shows while typing, Normal after Esc.


COMPILE TIME AND SIZE (Windows, rustc 1.99.1, warm registry cache)
------------------------------------------------------------------

    debug,   cold target dir, default features        13.4 s    10.18 MB exe
    release, cold target dir, default features        14.8 s     3.83 MiB
    release, cold target dir, --no-default-features   14.6 s     3.01 MiB

So hjkl-bonsai + tree-sitter costs ~+0.82 MiB and ~+0.2 s. The 3 MiB floor is
ratatui + crossterm + ropey + regex + the hjkl stack, not the editor engine
itself. `profile.release` uses opt-level="s", strip=true.

For scale: that is a self-contained binary with no runtime files.

Not claimed: any Linux or macOS build. No interactive TUI session was driven
here (no TTY in this environment); rendering was verified via ratatui
`TestBackend` (`--render-once`) and behaviour via key scripts. That verifies
the widget construction and the FSM, not terminal raw-mode handling.


VERSUS EMBEDDING NEOVIM IN A PTY
--------------------------------

The Neovim column is not re-measured by this spike; it reflects the approach
already present in the main crate (`portable-pty 0.9`, `vt100 0.15`,
`tui-term 0.2`, `nvim-rs 0.9.2`) and how that path behaves in general. Note
that the sibling spike in this repo (docs/nvim-rpc-spike.md) is the *RPC*
variant - `nvim --listen` + msgpack-RPC + extmarks - and it already works
against unsaved buffers.

Legend: ++ strong, + ok, - weak, -- blocker.

Dimension            hjkl embedded (measured)          Neovim in a PTY
-------------------  --------------------------------  ------------------------------
vimrc/plugin compat  -- none. No vimrc parser, no       ++ complete. Real vimrc,
                        plugin system, no Lua, no          lazy.nvim/packer, Lua,
                        LSP/completion/fuzzy-finder.       treesitter, LSP, every
                        Config is a Rust struct.           plugin in the ecosystem.
editor completeness  -  real FSM (modes, motions,       ++ is the editor. Splits,
                        text objects, dot-repeat,          tabs, folds, macros,
                        :s, :w, :q). No splits, tabs,     marks, quickfix, LSP,
                        quickfix, LSP, terminal, file     terminal, diffs - all
                        tree, macros beyond basics.        already there.
single-binary dist   +   yes, ~3.8 MiB, zero runtime    -- needs nvim + its runtime
                        files.                             tree on the user's box
                                                           (or ~40-80 MB vendored).
MSRV                 --  hjkl crates declare rust-version ++ subprocess: no Rust
                        1.95. CCN is 1.90 -> embedding      MSRV impact at all.
                        raises the project MSRV to 1.95
                        for everyone. Confirmed in each
                        hjkl-*.crate manifest.
dependency clash     --  hjkl needs ratatui 0.30 +      +   none - it is a process,
                        crossterm 0.29; CCN pins           not a library. No shared
                        0.29/0.28. Two copies of both      types, no version drift.
                        in the tree if added to main.
integration effort   ~   ~150 LOC core (done, works),   -   PTY spawn + resize +
                        plus ~100 for cmdline/status/      vt100 screen emulation +
                        save/syntax; plus per-feature      input translation +
                        host work for every editor          nvim-rs RPC; known but
                        luxury features.                    fiddly (rendering, keys,
                                                           latency, process death).
maintenance risk     -   pre-1.0, frequent breaking      +   Neovim API is stable
                        releases; their own docs say       and massively used;
                        "public surface may change in      nvim-rs exists and is
                        patch bumps". 2k downloads for     maintained. Failure mode
                        the app crate. Small, single-      (crash) is isolated from
                        maintainer ecosystem.              our process.
failure isolation    -   a panic in the engine takes     +   nvim crashing does not
                        our process down.                  take CCN down.
debuggability        +   one process, one debugger,      -   two processes; you debug
                        Rust types everywhere.             a terminal emulator.
offline/privacy      +   no network, no external deps.   +   offline, but a second
                                                           binary to trust.

Two strong pieces of evidence for the Neovim side, both from the hjkl source:

* hjkl's own app ships `--embed` (JSON-RPC) *and* `--nvim-api`: a 5.6k-line
  msgpack-rpc server explicitly built for `nvim-rs` (issue #26). Upstream is
  spending real effort becoming Neovim-API-compatible rather than competing
  on plugin compatibility.
* CCN already has `nvim-rs` + PTY + vt100 wired as dependencies, so the
  Neovim path's marginal cost is integration, not new infrastructure.


RECOMMENDATION
--------------

Use the Neovim PTY route for anything that should feel like an editor.

Reasons, in priority order:
1. Editor completeness is not close. hjkl gives vim *keys*; users expect
   splits, LSP, completion, file tree, fuzzy find, git signs, and their
   vimrc. Reimplementing those on hjkl-engine is the project.
2. MSRV: embedding hjkl forces CCN from 1.90 to 1.95. That is a real cost to
   every downstream user, for an editor.
3. ratatui 0.30/crossterm 0.29 collide with CCN's 0.29/0.28 - two copies of
   both, and no type sharing.
4. hjkl-bonsai's grammars are runtime-dlopen artifacts fetched and compiled
   from the network, duplicating tree-sitter crates CCN already links.

Use hjkl only if the requirement is explicitly: "modal vim keys, in-process,
no subprocess, no plugins, and we control the crate versions". In that case
it is a good engine - embeddable as promised, ~150 LOC of glue, 3-4 MiB,
predictable. Pin exactly (`=0.43.4`), vendor the crates, and budget for
`BufferView`'s 34-field literal churning between releases.

Use neither only if "vim keys" is really just "hjkl/0/$/dd/yy/p" - then a
200-line modal layer over our own buffer is cheaper than either option.

Do not add hjkl to the main CCN crate on the strength of this spike.


ONE GENUINE ADVANTAGE FOR CCN'S ACTUAL USE CASE
-----------------------------------------------

CCN's job is to measure the buffer the user is editing. In-process, that is
trivial with hjkl: `editor.content()` is the live buffer, no IPC at all. The
Neovim path needs RPC (`buf_get_lines`) or PTY + terminal emulation to get the
same bytes, and the sibling probe shows that cost is real but already paid.

So if CCN ever needs an embedded editor whose buffer it reads directly, hjkl
is the shorter path to the *data*. It is not the shorter path to a usable
editor. Those are different requirements and should not be conflated.

A possible middle ground, if this ever becomes a product question: keep
Neovim for editing, and use hjkl-engine only as a vim-key layer for CCN's own
small inspection widgets (a diff viewer, a blame pane), where the missing
plugins do not matter.


CAVEATS
-------

* Windows only; not built or run on Linux/macOS.
* No interactive TTY session was driven. Verified: 10 headless behaviour
  cases (release + debug), a TestBackend render dump, and `:w`/`:q` file
  semantics against real files.
* Syntax highlighting: code path present and compiling, runtime unverified -
  no grammar installed, and installing one means downloading and compiling
  third-party code. Deliberately skipped.
* The Neovim column is analytical (from main Cargo.toml + ecosystem
  knowledge), not measured by this spike.
