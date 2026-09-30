//! Owned contracts shared by Winwright transports, core, and platform adapters.
//!
//! Nothing in this crate references COM, HWND wrappers, or UI-toolkit types:
//! platform adapters convert native data into these DTOs at their boundary.

pub mod backend;
pub mod config;
pub mod element;
pub mod error;
pub mod geometry;
pub mod ids;
pub mod locator;
pub mod security;
pub mod snapshot;
pub mod window;

pub use error::{ErrorCode, ErrorPayload, WinwrightError, WinwrightResult};
