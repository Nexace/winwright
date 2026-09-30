//! Winwright core runtime. Transport-agnostic: MCP, the local API, and the CLI all call in here.

pub mod config;
pub mod lease;
pub mod refs;
pub mod session;
