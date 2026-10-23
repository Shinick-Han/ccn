//! CCN — repository code-health scanner.
//!
//! M0 scope: LOC collection and the JSON contract. CCN parsing and the report
//! land in M1/M2; see `docs/PLAN.md`.

pub mod baseline;
pub mod breadcrumb;
pub mod cache;
pub mod ccn;
pub mod cli;
pub mod diagnostics;
pub mod diff;
pub mod fuzzy;
pub mod generated;
pub mod git;
pub mod history;
pub mod loc;
pub mod lsp;
pub mod metrics;
pub mod model;
pub mod p4;
pub mod palette;
pub mod report;
pub mod review;
pub mod sarif;
pub mod session;
pub mod status;
pub mod symbols;
pub mod terminal;
pub mod theme;
pub mod tree;
pub mod tui;
pub mod vcs;

#[cfg(feature = "workbench")]
pub mod workbench;
