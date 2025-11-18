// Core library exports for ferric
pub mod cache;
pub mod config;
pub mod logger;
pub mod metadata;
pub mod operations;
pub mod quality;
pub mod utils;

// Re-export commonly used types
pub use anyhow::{Context, Result};
pub use colored::Colorize;
