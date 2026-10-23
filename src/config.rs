//! Per-repository configuration from `<root>/.ccn.toml`.
//!
//! The file is deliberately tiny: a flat list of `key = "value"` lines. It is
//! parsed by hand rather than with a TOML crate so the scanner keeps its
//! zero-configuration build and a broken file can never stop a scan. Every
//! problem (missing file, unreadable file, malformed line, empty value, unknown
//! keymap) collapses to [`Config::default`].

use std::path::{Path, PathBuf};

/// Values read from `<root>/.ccn.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    /// Editor command or path for the dashboard's `e` key.
    pub editor: Option<String>,
    /// Revision compared by `--baseline` when the flag is not given.
    pub baseline: Option<String>,
    /// Keymap shown by the dashboard: `simple` or `advanced`.
    pub keymap: Option<String>,
}

/// Location of the configuration file for `root`: `<root>/.ccn.toml`.
///
/// The path is not canonicalised: `load` may only ever read this one file
/// inside the scan root.
pub fn path(root: &Path) -> PathBuf {
    root.join(".ccn.toml")
}

/// Read the configuration for `root`.
///
/// A missing, unreadable, or malformed file yields the default configuration;
/// this function never panics and never prints.
pub fn load(root: &Path) -> Config {
    match std::fs::read_to_string(path(root)) {
        Ok(text) => parse(&text),
        Err(_) => Config::default(),
    }
}

/// Parse the supported TOML subset.
///
/// Supported: `key = "value"`, `#` comments (whole-line or trailing), blank
/// lines, and table headers (which are ignored). Only `editor`, `baseline`, and
/// `keymap` are recognised; unknown keys are ignored. Any malformed line
/// rejects the whole file, returning the default configuration, so a partially
/// understood file can never apply half of its settings.
fn parse(text: &str) -> Config {
    let mut config = Config::default();

    for raw in text.lines() {
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }

        // Table headers carry no meaning here: keys are flat. Accept a
        // well-formed header and keep reading, so a `[ccn]` section is fine.
        if line.starts_with('[') {
            if line.ends_with(']') && line.len() >= 2 {
                continue;
            }
            return Config::default();
        }

        let Some((key, raw_value)) = line.split_once('=') else {
            return Config::default();
        };
        let key = key.trim();
        let Some(value) = parse_string(raw_value.trim()) else {
            return Config::default();
        };

        match key {
            "editor" => config.editor = Some(value),
            "baseline" => config.baseline = Some(value),
            "keymap" => {
                if value != "simple" && value != "advanced" {
                    return Config::default();
                }
                config.keymap = Some(value);
            }
            // Unknown keys are ignored on purpose: a newer ccn may add keys,
            // and an older ccn must still read the rest of the file.
            _ => {}
        }
    }

    config
}

/// Drop a trailing `#` comment that is not inside a double-quoted string.
fn strip_comment(line: &str) -> &str {
    let mut in_string = false;
    for (offset, ch) in line.char_indices() {
        match ch {
            '"' => in_string = !in_string,
            '#' if !in_string => return &line[..offset],
            _ => {}
        }
    }
    line
}

/// Parse a non-empty double-quoted string. There are no escape sequences: the
/// inner text is taken literally (so Windows paths keep their backslashes),
/// and an inner quote or an empty value is rejected.
fn parse_string(raw: &str) -> Option<String> {
    let inner = raw.strip_prefix('"')?.strip_suffix('"')?;
    if inner.is_empty() || inner.contains('"') {
        return None;
    }
    Some(inner.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// A throwaway directory that removes itself even when an assertion fails.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir = std::env::temp_dir()
                .join(format!("ccn-config-{}-{name}-{nanos}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn with_file(name: &str, text: &str) -> Self {
            let scratch = Self::new(name);
            std::fs::write(path(&scratch.0), text).unwrap();
            scratch
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn parses_every_supported_key() {
        let scratch = Scratch::with_file(
            "valid",
            r#"
[ccn]
editor = "/usr/local/bin/nvim"
baseline = "origin/main"
keymap = "advanced"
"#,
        );
        let config = load(&scratch.0);
        assert_eq!(config.editor.as_deref(), Some("/usr/local/bin/nvim"));
        assert_eq!(config.baseline.as_deref(), Some("origin/main"));
        assert_eq!(config.keymap.as_deref(), Some("advanced"));
    }

    #[test]
    fn malformed_files_fall_back_to_defaults() {
        for text in [
            "editor = nvim\n",
            "editor =\n",
            "editor = \"\"\n",
            "editor \"nvim\"\n",
            "editor = \"nvim\n",
            "editor = \"a\" trailing\n",
            "[ccn\n",
            "keymap = \"fancy\"\n",
        ] {
            let scratch = Scratch::with_file("malformed", text);
            assert_eq!(
                load(&scratch.0),
                Config::default(),
                "expected defaults for {text:?}"
            );
        }
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let scratch = Scratch::with_file(
            "comments",
            r#"
# the editor for `e`
editor = "code --wait"   # trailing comment, outside the string
editor = "nvim#kept"     # `#` inside quotes is data

   # indented comment
"#,
        );
        let config = load(&scratch.0);
        assert_eq!(config.editor.as_deref(), Some("nvim#kept"));
        assert_eq!(config.baseline, None);
    }

    #[test]
    fn unknown_keys_and_tables_are_ignored() {
        let scratch = Scratch::with_file(
            "unknown",
            r#"
[future]
mystery = "value"
[ccn]
baseline = "HEAD~1"
another = "ignored"
"#,
        );
        let config = load(&scratch.0);
        assert_eq!(
            config,
            Config {
                editor: None,
                baseline: Some("HEAD~1".to_string()),
                keymap: None,
            }
        );
    }

    #[test]
    fn absent_file_is_the_default_config() {
        let scratch = Scratch::new("absent");
        assert_eq!(load(&scratch.0), Config::default());
        assert!(!path(&scratch.0).exists());
    }

    #[test]
    fn path_is_repo_relative_and_handles_a_trailing_separator() {
        assert_eq!(path(Path::new("repo")), Path::new("repo/.ccn.toml"));
        assert_eq!(path(Path::new("repo/")), Path::new("repo/.ccn.toml"));
        assert_eq!(path(Path::new(".")), Path::new("./.ccn.toml"));

        let nested = path(Path::new("C:/work/repo"));
        assert_eq!(nested.parent(), Some(Path::new("C:/work/repo")));
        assert!(nested.ends_with(".ccn.toml"));
    }

    #[test]
    fn keymap_accepts_only_simple_or_advanced() {
        for (value, expected) in [("simple", Some("simple")), ("advanced", Some("advanced"))] {
            let scratch = Scratch::with_file("keymap", &format!("keymap = \"{value}\"\n"));
            assert_eq!(load(&scratch.0).keymap.as_deref(), expected);
        }
    }
}
