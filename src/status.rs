//! Status-bar rendering for the scanned workspace.
//!
//! [`crate::vcs::identity`] collects the backend's identity as data; this module
//! turns it into the single line the dashboard/workbench shows. Rendering stays
//! separate from probing so the formatting rules are testable without a live
//! repository, Perforce client or terminal.

use crate::vcs::Identity;

/// Format a workspace identity for the status bar.
///
/// ```text
/// git main
/// p4 ws-main (CL 12345)
/// p4 ws-main (default CL)
/// no VCS
/// ```
///
/// This delegates to [`Identity::label`] so every caller (status line, future
/// JSON field, log line) shares exactly one formatting rule.
pub fn identity_line(id: &Identity) -> String {
    id.label()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(backend: &'static str, branch: Option<&str>, changelist: Option<&str>) -> String {
        identity_line(&Identity {
            backend,
            branch: branch.map(str::to_string),
            changelist: changelist.map(str::to_string),
        })
    }

    #[test]
    fn renders_git_branch_identity() {
        assert_eq!(line("git", Some("main"), None), "git main");
        assert_eq!(
            line("git", Some("feature/status-line"), None),
            "git feature/status-line"
        );
    }

    #[test]
    fn renders_p4_client_and_changelist_identity() {
        assert_eq!(
            line("p4", Some("ws-main"), Some("CL 12345")),
            "p4 ws-main (CL 12345)"
        );
        assert_eq!(
            line("p4", Some("ws-main"), Some("default CL")),
            "p4 ws-main (default CL)"
        );
    }

    #[test]
    fn a_partial_identity_degrades_to_the_known_part() {
        assert_eq!(line("git", None, None), "git");
        assert_eq!(line("p4", Some("ws-main"), None), "p4 ws-main");
        assert_eq!(line("p4", None, Some("CL 12345")), "p4 (CL 12345)");
        assert_eq!(line("p4", None, None), "p4");
    }

    #[test]
    fn empty_components_are_not_rendered_as_separators() {
        assert_eq!(line("git", Some(""), None), "git");
        assert_eq!(line("p4", Some("  "), Some("")), "p4");
    }

    #[test]
    fn no_backend_renders_as_no_vcs() {
        assert_eq!(Identity::default().label(), "no VCS");
        assert_eq!(line("", None, None), "no VCS");
        assert_eq!(line("svn", Some("trunk"), None), "no VCS");
    }
}
