//! Slash command modules — trait-based command implementations.
//!
//! Each module implements [`SlashCommand`](crate::cli::command::SlashCommand) for
//! a group of related commands and exports a `register_all()` function.

pub mod advanced;
pub mod all;
pub mod analysis;
pub mod coding;
pub mod context;
pub mod cron;
pub mod diff_cmd;
pub mod evolution;
pub mod git;
pub mod hooks;
pub mod info;
pub mod observability;
pub mod pipeline;
pub mod pipelines;
pub mod plugins;
pub mod research;
pub mod session;
pub mod skills;
pub mod tasks_ext;
pub mod workspace;
