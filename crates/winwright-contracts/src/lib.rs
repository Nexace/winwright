//! Owned contracts shared by Winwright transports, core, and platform adapters.
//!
//! Nothing in this crate references COM, HWND wrappers, or UI-toolkit types:
//! platform adapters convert native data into these DTOs at their boundary.

pub mod action;
pub mod backend;
pub mod capture;
pub mod config;
pub mod element;
pub mod error;
pub mod geometry;
pub mod ids;
pub mod input;
pub mod locator;
pub mod overlay;
pub mod security;
pub mod snapshot;
pub mod system;
pub mod wait;
pub mod window;

pub use error::{ErrorCode, ErrorPayload, WinwrightError, WinwrightResult};
