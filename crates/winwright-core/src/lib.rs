//! Winwright core runtime. Transport-agnostic: MCP, the local API, and the CLI all call in here.

mod actions;
pub mod config;
pub mod diff;
pub mod engine;
mod find;
pub mod lease;
pub mod locator;
pub mod refs;
mod services;
pub mod session;
pub mod snapshot;
mod wait;

#[cfg(test)]
mod engine_tests;

pub use engine::{Engine, InspectRequest};
