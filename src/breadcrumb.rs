//! Breadcrumb labels for functions.
//!
//! Turns the stable engine identity (see `crate::ccn::types::FunctionCcn`)
//! into a human-readable scope trail plus a compact form for tight layouts.

use crate::ccn::types::FunctionCcn;

/// A function's location described as a scope trail, a name and optional
/// parameter list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Breadcrumb {
    /// Enclosing namespaces, classes or impls, outermost first.
    pub scope: Vec<String>,
    /// Function name (lambda bindings keep their binding name).
    pub name: String,
    /// Parameter list including its parentheses, when present.
    pub params: Option<String>,
}

impl Breadcrumb {
    /// Build a breadcrumb from a function's identity, falling back to `name`
    /// when the identity is empty (older or hand-built data).
    pub fn from_function(function: &FunctionCcn) -> Self {
        let raw = if function.identity.trim().is_empty() {
            function.name.as_str()
        } else {
            function.identity.as_str()
        };
        Self::parse(raw)
    }

    fn parse(raw: &str) -> Self {
        let raw = raw.trim();

        // `(anonymous)(int)` carries the name itself in parentheses, so the
        // first `(` is not the parameter list.
        if let Some(rest) = raw.strip_prefix("(anonymous)") {
            return Self {
                scope: Vec::new(),
                name: "(anonymous)".to_string(),
                params: params_from(rest),
            };
        }

        let name_start = raw.rfind("::").map_or(0, |index| index + 2);
        let segment = &raw[name_start..];

        let (name_part, params) = match segment.find('(') {
            Some(index) => (&segment[..index], params_from(&segment[index..])),
            None => (segment, None),
        };

        let mut name = name_part.trim();
        if let Some(binding) = name.strip_prefix("lambda:") {
            name = binding.trim();
        }

        Self {
            scope: raw[..name_start]
                .split("::")
                .filter(|part| !part.is_empty())
                .map(str::to_string)
                .collect(),
            name: name.to_string(),
            params,
        }
    }

    /// Full label: `demo > process(int)`, or just `process` without a scope.
    pub fn display(&self) -> String {
        let mut out = String::new();
        for part in &self.scope {
            out.push_str(part);
            out.push_str(" > ");
        }
        out.push_str(&self.name);
        if let Some(params) = &self.params {
            out.push_str(params);
        }
        out
    }

    /// Compact label for narrow layouts: `demo::process` (no parameters).
    pub fn compact(&self) -> String {
        let mut out = self.scope.join("::");
        if !out.is_empty() {
            out.push_str("::");
        }
        out.push_str(&self.name);
        out
    }
}

fn params_from(rest: &str) -> Option<String> {
    let rest = rest.trim();
    if rest.is_empty() {
        None
    } else {
        Some(rest.to_string())
    }
}

/// Convenience wrapper around [`Breadcrumb::from_function`].
pub fn for_function(function: &FunctionCcn) -> Breadcrumb {
    Breadcrumb::from_function(function)
}

/// Convenience wrapper around [`Breadcrumb::display`].
pub fn line(function: &FunctionCcn) -> String {
    for_function(function).display()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn function(name: &str, identity: &str) -> FunctionCcn {
        FunctionCcn {
            name: name.to_string(),
            identity: identity.to_string(),
            line: 1,
            end_line: 1,
            nloc: 1,
            ccn: 1,
            params: 0,
            max_nesting: 0,
        }
    }

    fn parsed(identity: &str) -> Breadcrumb {
        for_function(&function("ignored", identity))
    }

    #[test]
    fn namespace() {
        let crumb = parsed("demo::process(int)");
        assert_eq!(crumb.scope, vec!["demo"]);
        assert_eq!(crumb.name, "process");
        assert_eq!(crumb.params.as_deref(), Some("(int)"));
        assert_eq!(crumb.display(), "demo > process(int)");
        assert_eq!(crumb.compact(), "demo::process");
    }

    #[test]
    fn nested_class() {
        let crumb = parsed("N::X::method(int)");
        assert_eq!(crumb.scope, vec!["N", "X"]);
        assert_eq!(crumb.name, "method");
        assert_eq!(crumb.display(), "N > X > method(int)");
        assert_eq!(crumb.compact(), "N::X::method");
    }

    #[test]
    fn overload_params() {
        let crumb = parsed("N::X::method(int, const char*)");
        assert_eq!(crumb.name, "method");
        assert_eq!(crumb.params.as_deref(), Some("(int, const char*)"));
        assert_eq!(crumb.display(), "N > X > method(int, const char*)");
    }

    #[test]
    fn lambda_binding() {
        let crumb = parsed("N::run::lambda: binding(int)");
        assert_eq!(crumb.scope, vec!["N", "run"]);
        assert_eq!(crumb.name, "binding");
        assert_eq!(crumb.params.as_deref(), Some("(int)"));
        assert_eq!(crumb.display(), "N > run > binding(int)");
        assert_eq!(crumb.compact(), "N::run::binding");
    }

    #[test]
    fn anonymous() {
        let crumb = parsed("(anonymous)(int)");
        assert!(crumb.scope.is_empty());
        assert_eq!(crumb.name, "(anonymous)");
        assert_eq!(crumb.params.as_deref(), Some("(int)"));
        assert_eq!(crumb.display(), "(anonymous)(int)");
        assert_eq!(crumb.compact(), "(anonymous)");
    }

    #[test]
    fn empty_identity_falls_back_to_name() {
        let crumb = for_function(&function("demo::process(int)", ""));
        assert_eq!(crumb.scope, vec!["demo"]);
        assert_eq!(crumb.name, "process");
        assert_eq!(crumb.params.as_deref(), Some("(int)"));
        assert_eq!(crumb.display(), "demo > process(int)");
    }

    #[test]
    fn no_params() {
        let crumb = parsed("demo::process");
        assert_eq!(crumb.scope, vec!["demo"]);
        assert_eq!(crumb.name, "process");
        assert_eq!(crumb.params, None);
        assert_eq!(crumb.display(), "demo > process");
    }

    #[test]
    fn plain_name_without_scope() {
        let crumb = parsed("process");
        assert!(crumb.scope.is_empty());
        assert_eq!(crumb.display(), "process");
        assert_eq!(crumb.compact(), "process");
    }

    #[test]
    fn leading_scope_separator_is_ignored() {
        let crumb = parsed("::demo::process(int)");
        assert_eq!(crumb.scope, vec!["demo"]);
        assert_eq!(crumb.name, "process");
        assert_eq!(crumb.display(), "demo > process(int)");
    }

    #[test]
    fn unicode_name() {
        let crumb = parsed("café::naïve(int)");
        assert_eq!(crumb.scope, vec!["café"]);
        assert_eq!(crumb.name, "naïve");
        assert_eq!(crumb.display(), "café > naïve(int)");
        assert_eq!(crumb.compact(), "café::naïve");
    }

    #[test]
    fn line_helper_matches_display() {
        let function = function("ignored", "demo::process(int)");
        assert_eq!(line(&function), "demo > process(int)");
    }
}
