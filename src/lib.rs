//! CCN — repository code-health scanner.
//!
//! M0 scope: LOC collection and the JSON contract. CCN parsing and the report
//! land in M1/M2; see `docs/PLAN.md`.

pub mod cache;
pub mod ccn;
pub mod cli;
pub mod generated;
pub mod git;
pub mod history;
pub mod loc;
pub mod metrics;
pub mod model;
pub mod report;
pub mod terminal;
pub mod tui;
