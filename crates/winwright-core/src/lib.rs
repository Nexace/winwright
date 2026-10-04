//! Winwright core runtime. Transport-agnostic: MCP, the local API, and the CLI all call in here.

mod actions;
pub mod audit;
pub mod config;
pub mod diff;
pub mod engine;
mod find;
pub mod lease;
pub mod locator;
mod memory;
pub mod refs;
mod services;
pub mod session;
pub mod snapshot;
mod wait;

#[cfg(test)]
mod engine_tests;

pub use engine::{Engine, InspectRequest};

/// Per-test scratch folder under the workspace `target` directory.
#[cfg(test)]
pub(crate) fn scratch_dir(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/test-scratch")
        .join(format!("{name}-{}", std::process::id()))
}
