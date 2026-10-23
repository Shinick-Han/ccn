//! Zero-configuration `ccn lsp` attachment for the embedded Neovim.
//!
//! The workbench launches Neovim in a PTY. Instead of asking users to paste
//! the `ccn setup nvim` snippet into their config, [`lua_command`] builds one
//! `-c` argument that starts `ccn lsp` with the absolute path of the running
//! executable. The command is deliberately tolerant: any failure is reported
//! through `vim.notify` and never breaks the editor.

use std::path::{Path, PathBuf};

/// Filetypes the `ccn lsp` server understands.
///
/// This mirrors [`crate::ccn::types::Lang`] (and the snippet printed by
/// `ccn setup nvim`): the React variants map to the JavaScript/TypeScript
/// grammars, and C++ headers use the C++ grammar.
pub const FILETYPES: &[&str] = &[
    "c",
    "cpp",
    "csharp",
    "go",
    "java",
    "javascript",
    "javascriptreact",
    "python",
    "rust",
    "typescript",
    "typescriptreact",
];

/// Whether the LSP server can analyse buffers of this filetype.
pub fn supports_filetype(ft: &str) -> bool {
    FILETYPES.contains(&ft)
}

/// Build the single-line Lua Ex command passed to Neovim with `-c`.
///
/// The command starts `ccn lsp` for the current buffer when its filetype is
/// supported and installs a `FileType` autocmd for future buffers. Reusing the
/// `ccn` client name lets `vim.lsp.start` attach an existing client instead of
/// spawning a duplicate.
pub fn lua_command(exe: &Path) -> String {
    let exe = lua_literal(&exe.display().to_string());
    let filetypes = FILETYPES
        .iter()
        .map(|ft| format!("\"{ft}\""))
        .collect::<Vec<_>>()
        .join(",");

    format!(
        "lua local F={{{filetypes}}}; local function s(b) local ok,e=pcall(vim.lsp.start,{{name=\"ccn\",cmd={{{exe},\"lsp\"}},root_dir=vim.fn.getcwd()}},{{bufnr=b}}); if not ok then vim.notify(\"ccn lsp: \"..tostring(e),vim.log.levels.WARN) end end; local g=vim.api.nvim_create_augroup(\"ccn_lsp\",{{clear=true}}); vim.api.nvim_create_autocmd(\"FileType\",{{group=g,pattern=F,callback=function(a) if vim.tbl_contains(F,vim.bo[a.buf].filetype) then s(a.buf) end end}}); if vim.tbl_contains(F,vim.bo.filetype) then s(vim.api.nvim_get_current_buf()) end"
    )
}

/// Render a path as a Lua string literal.
///
/// A long bracket keeps Windows backslashes readable. It is only safe when the
/// path contains no `]]` and does not end with `]` (which would merge with the
/// closing delimiter), so those cases fall back to an escaped quoted string.
fn lua_literal(value: &str) -> String {
    if !value.contains("]]") && !value.ends_with(']') {
        return format!("[[{value}]]");
    }

    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\0' => out.push_str("\\0"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// Should auto-attachment be skipped for `target`?
///
/// `lsp = "off"` in `<root>/.ccn.toml` is honoured. The current directory is
/// checked first (the usual project root), then the target's parent directory.
/// A missing, unreadable, or malformed file means auto.
pub fn lsp_disabled(target: &Path) -> bool {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd);
    }
    if let Some(parent) = target.parent() {
        roots.push(parent.to_path_buf());
    }
    roots
        .iter()
        .any(|root| crate::config::load(root).lsp.as_deref() == Some("off"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_temp_dir(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("ccn-embedded-lsp-{name}-{nanos}"));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn windows_path_uses_a_long_bracket() {
        let command = lua_command(Path::new(r"C:\Tools\ccn\ccn.exe"));
        assert!(command.contains(r#"cmd={[[C:\Tools\ccn\ccn.exe]],"lsp"}"#));
        assert!(!command.contains("\\\\"));
    }

    #[test]
    fn long_bracket_is_avoided_when_the_path_contains_the_delimiter() {
        let command = lua_command(Path::new(r"C:\odd]]tool\ccn.exe"));
        assert!(command.contains(r#""C:\\odd]]tool\\ccn.exe","lsp""#));
        assert!(!command.contains("[[C:"));
    }

    #[test]
    fn quotes_and_trailing_brackets_are_escaped_or_delimited_safely() {
        let quoted = lua_command(Path::new("C:\\odd\"tool\\ccn.exe"));
        // A long bracket is fine for quotes; the literal must still round-trip.
        assert!(quoted.contains("odd\"tool"));

        let trailing = lua_command(Path::new("C:\\odd]"));
        assert!(trailing.contains(r#""C:\\odd]""#));
        assert!(!trailing.contains("[[C:"));

        let both = lua_command(Path::new("C:\\a]]b\"c\\ccn.exe"));
        assert!(both.contains(r#""C:\\a]]b\"c\\ccn.exe""#));
    }

    #[test]
    fn command_stays_on_one_line() {
        let command = lua_command(Path::new(r"C:\Program Files\ccn\ccn.exe"));
        assert!(!command.contains('\n'));
        assert!(!command.contains('\r'));
        assert!(command.starts_with("lua "));
        assert!(command.contains("vim.lsp.start"));
        assert!(command.contains("vim.notify"));
        assert!(command.contains(r#"name="ccn""#));
        assert!(command.contains(r#"root_dir=vim.fn.getcwd()"#));
        assert!(command.contains("nvim_create_augroup"));
        assert!(command.contains(r#"\"FileType\""#) || command.contains(r#""FileType""#));
    }

    #[test]
    fn filetypes_match_the_lsp_languages() {
        for ft in [
            "c",
            "cpp",
            "csharp",
            "go",
            "java",
            "javascript",
            "javascriptreact",
            "python",
            "rust",
            "typescript",
            "typescriptreact",
        ] {
            assert!(supports_filetype(ft), "{ft} should be supported");
        }
        assert!(!supports_filetype("lua"));
        assert!(!supports_filetype("toml"));
        assert!(!supports_filetype(""));
    }

    #[test]
    fn config_can_turn_auto_attachment_off() {
        let dir = unique_temp_dir("off");
        let target = dir.join("main.rs");
        std::fs::write(dir.join(".ccn.toml"), "lsp = \"off\" # disable\n").expect("write config");
        assert!(lsp_disabled(&target));

        std::fs::write(dir.join(".ccn.toml"), "lsp = \"auto\"\n").expect("write config");
        assert!(!lsp_disabled(&target));

        std::fs::write(dir.join(".ccn.toml"), "editor = \"nvim\"\n").expect("write config");
        assert!(!lsp_disabled(&target));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
