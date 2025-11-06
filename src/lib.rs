// Core library exports for ferric
pub mod config;
pub mod logger;
pub mod metadata;
pub mod quality;
pub mod utils;
pub mod operations;

// Re-export commonly used types
pub use anyhow::{Context, Result};
pub use colored::Colorize;
