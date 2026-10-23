# `ccn lsp` — the editor server

`ccn lsp` starts a **stdio Language Server Protocol server**. It is the same
single binary as the scanner, but in this mode it speaks LSP on stdin/stdout:
no network listener, no TUI, no report, and no scan of the current directory.
The dispatch happens before CLI parsing, so `ccn lsp` never treats `lsp` as a
scan root.

It is meant to be launched by an editor. Running it in a terminal just waits
for input; `ccn lsp --help` also enters the server path, because editors pass no
arguments and clap must not own the command.

## Capabilities

- **Full-text document sync** — the editor sends the whole buffer, including
  unsaved changes.
- **`publishDiagnostics`** — functions above CCN 10 in the open document.
- **Hover** — complexity information for the symbol under the cursor.
- **`documentSymbol`** — the function outline for the document.

Unknown or unsupported languages return nothing; there is no fallback parser.
Diagnostics are **advisory**: they describe complexity, not correctness.

There are **no code actions, no formatting, and no completion**. Keep your
language's own server (rust-analyzer, pyright, clangd, ...) enabled for those.

## Neovim

Requires Neovim 0.10 or newer (0.11+ recommended). Neovim's built-in LSP
client is enough; no plugin is required.

### 0.10+ — `vim.lsp.start`

```lua
vim.lsp.start({
  name = "ccn",
  cmd = { "ccn", "lsp" },
  root_dir = vim.fn.getcwd(),
})
```

Call it once per buffer, for example from a `FileType` autocmd, or from your
config for the languages you scan.

### 0.11+ — `vim.lsp.config` / `vim.lsp.enable`

```lua
vim.lsp.config("ccn", {
  cmd = { "ccn", "lsp" },
  root_dir = vim.fn.getcwd(),
})
vim.lsp.enable("ccn")
```

`root_dir` only scopes the client; the server does not need the directory.

### Explicit binary path

If `ccn` is not on `PATH`, point `cmd` at the binary:

```lua
vim.lsp.start({
  name = "ccn",
  cmd = { "/opt/ccn/bin/ccn", "lsp" },
  root_dir = vim.fn.getcwd(),
})
```

On Windows use the full path to `ccn.exe`. If you launch `ccn` through the npm
shim (`npm/bin/ccn.js`), it resolves the real executable from
`CCN_BINARY_PATH`; export it in the environment that starts Neovim when the
platform package is not installed next to the shim. The `cmd` entry above still
works either way — `{ "ccn", "lsp" }` plus `CCN_BINARY_PATH` is the simplest
setup for an npm install.

## Independence from the workbench

`ccn lsp` is unrelated to `ccn workbench`. The workbench's embedded Neovim is a
real Neovim process, so it can start the same client with the same config; the
server needs no workbench feature and no workbench state.

## Troubleshooting

- **Nothing happens in the terminal** — expected: the server waits on stdin.
  Start it from the editor instead.
- **No diagnostics** — confirm the client attached with `:LspInfo` (0.10) or
  `:checkhealth vim.lsp` (0.11+), and check the client log
  (`:lua vim.lsp.set_log_level("debug")`, then
  `:lua print(vim.lsp.get_log_path())`).
- **`ccn` not found** — use an absolute `cmd` path as shown above.
- **The buffer is not a supported language** — the server intentionally returns
  nothing; pair it with that language's own server.
