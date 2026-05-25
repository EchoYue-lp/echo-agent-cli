//! Slash command modules — trait-based command implementations.
//!
//! Each module implements [`SlashCommand`](crate::cli::command::SlashCommand) for
//! a group of related commands and exports a `register_all()` function.

pub mod advanced;
pub mod all;
pub mod coding;
pub mod context;
pub mod eval;
pub mod info;
pub mod session;
pub mod skills;
