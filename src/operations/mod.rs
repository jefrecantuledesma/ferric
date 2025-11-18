pub mod convert;
pub mod dedupe;
pub mod dedupe_libraries;
pub mod fix_metadata;
pub mod fix_naming;
pub mod merge;
pub mod merge_libraries;
pub mod playlist;
pub mod sort;
pub mod unified;

// Common structures for all operations
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct OperationStats {
    pub processed: usize,
    pub succeeded: usize,
    pub skipped: usize,
    pub errors: usize,
    pub skipped_files: Vec<(PathBuf, String)>, // (file_path, reason)
}

impl OperationStats {
    pub fn new() -> Self {
        Self {
            processed: 0,
            succeeded: 0,
            skipped: 0,
            errors: 0,
            skipped_files: Vec::new(),
        }
    }

    pub fn add_skipped(&mut self, file: PathBuf, reason: String) {
        self.skipped += 1;
        self.skipped_files.push((file, reason));
    }

    pub fn print_summary(&self, operation_name: &str) {
        use crate::logger;

        logger::plain(&format!("\n{} Summary:", operation_name));
        logger::plain(&format!("  Processed: {}", self.processed));
        logger::success(&format!("  Succeeded: {}", self.succeeded));
        if self.skipped > 0 {
            logger::warning(&format!("  Skipped: {}", self.skipped));
            if !self.skipped_files.is_empty() {
                logger::plain("  Skipped files:");
                for (file, reason) in &self.skipped_files {
                    logger::plain(&format!("    - {}: {}", file.display(), reason));
                }
            }
        }
        if self.errors > 0 {
            logger::error(&format!("  Errors: {}", self.errors));
        }
    }
}

impl Default for OperationStats {
    fn default() -> Self {
        Self::new()
    }
}

/// Common options passed to most operations
#[derive(Debug, Clone)]
pub struct CommonOptions {
    pub dry_run: bool,
    pub verbose: bool,
    pub input_dir: PathBuf,
    pub output_dir: Option<PathBuf>,
}
