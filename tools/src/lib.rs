//! Developer tooling for Vibescript, usable from any Rust program.
//!
//! Each tool exposes a library API free of terminal and transport concerns, so
//! editors, hosts and services can embed it directly; the `vibes` CLI is a thin
//! front end over this crate.

pub mod analyze;
pub mod format;
pub mod repl;
