use crate::logger;
use crate::operations::OperationStats;
use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub struct LowercaseOptions {
    pub input_dir: PathBuf,
    pub dry_run: bool,
    pub verbose: bool,
}

/// Recursively lowercase all files and folders
pub fn run(options: LowercaseOptions) -> Result<OperationStats> {
    logger::stage("Starting lowercase conversion");
    logger::info(&format!("Input directory: {}", options.input_dir.display()));

    if options.dry_run {
        logger::warning("DRY RUN MODE - No changes will be made");
    }

    let mut stats = OperationStats::new();

    // First pass: rename files
    logger::info("Step 1/2: Lowercasing files...");
    rename_files(&options.input_dir, &mut stats, &options)?;

    // Second pass: rename directories (depth-first)
    logger::info("Step 2/2: Lowercasing directories...");
    rename_directories(&options.input_dir, &mut stats, &options)?;

    stats.print_summary("Lowercase Conversion");
    Ok(stats)
}

fn rename_files(root: &Path, stats: &mut OperationStats, options: &LowercaseOptions) -> Result<()> {
    // Collect all files first
    let files: Vec<PathBuf> = WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .collect();

    let pb = ProgressBar::new(files.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] [{bar:40}] {pos}/{len} ({eta})")
            .unwrap()
            .progress_chars("=>-"),
    );

    for file in files {
        pb.inc(1);
        stats.processed += 1;

        if let Some(filename) = file.file_name() {
            let filename_str = filename.to_string_lossy();
            let lowercase_name = filename_str.to_lowercase();

            if filename_str != lowercase_name {
                let parent = file.parent().unwrap_or_else(|| Path::new("."));
                let new_path = parent.join(&lowercase_name);

                if new_path.exists() && new_path != file {
                    logger::warning(&format!(
                        "Conflict: {} already exists",
                        new_path.display()
                    ));
                    stats.add_skipped(file.clone(), "naming conflict - lowercase target already exists".to_string());
                    continue;
                }

                if options.dry_run {
                    logger::debug(
                        &format!("Would rename: {} -> {}", filename_str, lowercase_name),
                        options.verbose,
                    );
                    stats.succeeded += 1;
                } else {
                    match fs::rename(&file, &new_path) {
                        Ok(_) => {
                            logger::debug(
                                &format!("Renamed: {} -> {}", filename_str, lowercase_name),
                                options.verbose,
                            );
                            stats.succeeded += 1;
                        }
                        Err(e) => {
                            logger::error(&format!("Failed to rename {}: {}", file.display(), e));
                            stats.errors += 1;
                        }
                    }
                }
            }
        }
    }

    pb.finish_and_clear();
    Ok(())
}

fn rename_directories(root: &Path, stats: &mut OperationStats, options: &LowercaseOptions) -> Result<()> {
    // Collect directories in reverse order (deepest first)
    let mut dirs: Vec<PathBuf> = WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_dir())
        .map(|e| e.path().to_path_buf())
        .collect();

    // Sort by depth (deepest first)
    dirs.sort_by(|a, b| b.components().count().cmp(&a.components().count()));

    let pb = ProgressBar::new(dirs.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] [{bar:40}] {pos}/{len} ({eta})")
            .unwrap()
            .progress_chars("=>-"),
    );

    for dir in dirs {
        pb.inc(1);

        // Skip the root directory itself
        if dir == root {
            continue;
        }

        stats.processed += 1;

        if let Some(dirname) = dir.file_name() {
            let dirname_str = dirname.to_string_lossy();
            let lowercase_name = dirname_str.to_lowercase();

            if dirname_str != lowercase_name {
                let parent = dir.parent().unwrap_or_else(|| Path::new("."));
                let new_path = parent.join(&lowercase_name);

                if new_path.exists() && new_path != dir {
                    logger::warning(&format!(
                        "Conflict: {} already exists",
                        new_path.display()
                    ));
                    stats.add_skipped(dir.clone(), "naming conflict - lowercase directory already exists".to_string());
                    continue;
                }

                if options.dry_run {
                    logger::debug(
                        &format!("Would rename dir: {} -> {}", dirname_str, lowercase_name),
                        options.verbose,
                    );
                    stats.succeeded += 1;
                } else {
                    match fs::rename(&dir, &new_path) {
                        Ok(_) => {
                            logger::debug(
                                &format!("Renamed dir: {} -> {}", dirname_str, lowercase_name),
                                options.verbose,
                            );
                            stats.succeeded += 1;
                        }
                        Err(e) => {
                            logger::error(&format!("Failed to rename {}: {}", dir.display(), e));
                            stats.errors += 1;
                        }
                    }
                }
            }
        }
    }

    pb.finish_and_clear();
    Ok(())
}
