use crate::config::Config;
use crate::logger;
use crate::metadata::AudioMetadata;
use crate::operations::OperationStats;
use crate::quality;
use crate::utils;
use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use pathdiff::diff_paths;
use rayon::prelude::*;
use std::collections::HashMap;
use std::fs;
use std::os::unix::fs as unix_fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use walkdir::WalkDir;

pub struct DedupeLibrariesOptions {
    pub input_dirs: Vec<PathBuf>,
    pub dry_run: bool,
    pub verbose: bool,
    pub config: Config,
}

#[derive(Debug, Clone)]
struct FileInfo {
    path: PathBuf,
    metadata: AudioMetadata,
    quality: u32,
    song_id: String, // Normalized identifier for deduplication
}

/// Deduplicate files across multiple libraries by replacing lower quality versions with symlinks
pub fn run(options: DedupeLibrariesOptions) -> Result<OperationStats> {
    logger::stage("Starting cross-library deduplication");
    logger::info(&format!(
        "Scanning {} libraries for duplicates:",
        options.input_dirs.len()
    ));
    for (i, dir) in options.input_dirs.iter().enumerate() {
        logger::info(&format!("  Library {}: {}", i + 1, dir.display()));
    }
    logger::info("Will replace lower quality duplicates with symlinks to best version");

    if options.dry_run {
        logger::warning("DRY RUN MODE - No files will be modified");
    }

    // Validate input directories
    if options.input_dirs.len() < 2 {
        anyhow::bail!(
            "Need at least 2 libraries to deduplicate. Use 'dedupe' command for single library."
        );
    }

    for dir in &options.input_dirs {
        if !dir.exists() {
            anyhow::bail!("Directory does not exist: {}", dir.display());
        }
    }

    let stats = OperationStats::new();
    let replaced_count = 0;

    // Phase 1: Collect all audio files from all input libraries (PARALLEL!)
    logger::info("Phase 1/4: Scanning libraries in parallel...");

    let scan_pb = ProgressBar::new(options.input_dirs.len() as u64);
    scan_pb.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] [{bar:40}] {pos}/{len} libraries | Scanning...")
            .unwrap()
            .progress_chars("█▓▒░"),
    );

    // Scan all libraries in parallel for maximum speed!
    let all_files: Vec<PathBuf> = options
        .input_dirs
        .par_iter()
        .flat_map(|input_dir| {
            scan_pb.inc(1);

            let files: Vec<PathBuf> = WalkDir::new(input_dir)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().is_file())
                .map(|e| e.path().to_path_buf())
                .filter(|p| utils::is_audio_file(p))
                .collect();

            logger::info(&format!(
                "  Found {} audio files in {}",
                files.len(),
                input_dir.display()
            ));

            files
        })
        .collect();

    scan_pb.finish_and_clear();
    logger::success(&format!("Total files found: {}", all_files.len()));

    // Phase 2: Extract metadata in parallel
    logger::info("Phase 2/4: Extracting metadata in parallel...");
    let pb = ProgressBar::new(all_files.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] [{bar:40}] {pos}/{len} ({eta}) | Extracting metadata...")
            .unwrap()
            .progress_chars("█▓▒░"),
    );

    let file_infos: Vec<FileInfo> = all_files
        .par_iter()
        .filter_map(|file| {
            pb.inc(1);

            let metadata = match AudioMetadata::from_file(file) {
                Ok(m) => m,
                Err(e) => {
                    logger::error(&format!(
                        "Failed to read metadata from {}: {}",
                        file.display(),
                        e
                    ));
                    return None;
                }
            };

            let quality = quality::calculate_quality_score(&metadata, &options.config);

            // Create a normalized song ID for deduplication
            let artist = metadata.get_organizing_artist(options.config.naming.prefer_artist);
            let album = metadata.get_album();
            let title = metadata.get_title();

            let song_id = format!(
                "{}__{}__{}",
                utils::normalize_for_comparison(&artist),
                utils::normalize_for_comparison(&album),
                utils::normalize_for_comparison(&title)
            );

            Some(FileInfo {
                path: file.clone(),
                metadata,
                quality,
                song_id,
            })
        })
        .collect();

    pb.finish_and_clear();
    logger::success(&format!(
        "Metadata extracted from {} files",
        file_infos.len()
    ));

    // Phase 3: Group by song ID and find duplicates
    logger::info("Phase 3/4: Finding duplicate groups...");
    let mut song_groups: HashMap<String, Vec<FileInfo>> = HashMap::new();

    for file_info in file_infos {
        song_groups
            .entry(file_info.song_id.clone())
            .or_insert_with(Vec::new)
            .push(file_info);
    }

    // Filter to only groups with duplicates (2+ files)
    let duplicate_groups: Vec<(String, Vec<FileInfo>)> = song_groups
        .into_iter()
        .filter(|(_, files)| files.len() > 1)
        .collect();

    let duplicate_groups_found = duplicate_groups.len();

    logger::success(&format!(
        "Found {} songs with duplicates across libraries",
        duplicate_groups_found
    ));

    if duplicate_groups_found == 0 {
        logger::info("No duplicates found - all libraries contain unique songs!");
        return Ok(stats);
    }

    // Calculate total duplicates to replace
    let total_duplicates: usize = duplicate_groups
        .iter()
        .map(|(_, files)| files.len() - 1) // -1 because we keep the best version
        .sum();

    logger::info(&format!(
        "Will replace {} lower-quality files with symlinks",
        total_duplicates
    ));

    // Phase 4: Replace with symlinks (PARALLEL!)
    logger::info("Phase 4/4: Replacing duplicates with symlinks in parallel...");
    let pb2 = ProgressBar::new(duplicate_groups.len() as u64);
    pb2.set_style(
        ProgressStyle::default_bar()
            .template(
                "[{elapsed_precise}] [{bar:40}] {pos}/{len} ({eta}) | Replacing duplicates...",
            )
            .unwrap()
            .progress_chars("█▓▒░"),
    );

    // Thread-safe counters for parallel execution
    let stats_mutex = Arc::new(Mutex::new(stats));
    let replaced_count_mutex = Arc::new(Mutex::new(replaced_count));

    // Process all duplicate groups in parallel for maximum throughput!
    duplicate_groups.par_iter().for_each(|(_song_id, files)| {
        pb2.inc(1);

        // Sort by quality (descending) to find the best version
        let mut sorted_files = files.clone();
        sorted_files.sort_by(|a, b| b.quality.cmp(&a.quality));

        // Best version is the first one (highest quality)
        let best_file = &sorted_files[0];
        let best_path_absolute = match fs::canonicalize(&best_file.path) {
            Ok(p) => p,
            Err(e) => {
                logger::error(&format!(
                    "Failed to resolve absolute path for {}: {}",
                    best_file.path.display(),
                    e
                ));
                let mut stats = stats_mutex.lock().unwrap();
                stats.errors += 1;
                return;
            }
        };

        logger::debug(
            &format!(
                "Best version (quality {}): {}",
                best_file.quality,
                best_file.metadata.get_title()
            ),
            options.verbose,
        );

        // Replace all other versions with symlinks
        for file_info in sorted_files.iter().skip(1) {
            let current_path = &file_info.path;
            let current_dir = current_path.parent().unwrap_or_else(|| std::path::Path::new("."));

            logger::debug(
                &format!(
                    "  Replacing {} (quality {}) with symlink to best version",
                    current_path.display(),
                    file_info.quality
                ),
                options.verbose,
            );

            {
                let mut stats = stats_mutex.lock().unwrap();
                stats.processed += 1;
            }

            if !options.dry_run {
                // Check if current file is already a symlink pointing to the best file
                if let Ok(metadata) = fs::symlink_metadata(current_path) {
                    if metadata.is_symlink() {
                        if let Ok(existing_target) = fs::read_link(current_path) {
                            // Resolve existing target to absolute path for comparison
                            // (it might be relative or absolute)
                            let existing_target_absolute = if existing_target.is_absolute() {
                                existing_target.clone()
                            } else {
                                current_dir.join(&existing_target)
                            };

                            // Canonicalize for proper comparison
                            let existing_target_canonical = match fs::canonicalize(&existing_target_absolute) {
                                Ok(p) => p,
                                Err(_) => existing_target_absolute, // Use as-is if canonicalize fails (broken symlink)
                            };

                            if existing_target_canonical == best_path_absolute {
                                logger::debug(
                                    &format!(
                                        "Already a symlink to best version: {}",
                                        current_path.display()
                                    ),
                                    options.verbose,
                                );
                                let mut stats = stats_mutex.lock().unwrap();
                                stats.add_skipped(
                                    current_path.clone(),
                                    "already symlinked to best version".to_string(),
                                );
                                continue;
                            }
                        }
                    }
                }

                // Compute relative path from current directory to best file
                // This makes symlinks work inside Docker containers!
                let best_path_relative = match diff_paths(&best_path_absolute, current_dir) {
                    Some(relative) => relative,
                    None => {
                        logger::error(&format!(
                            "Failed to compute relative path from {} to {}",
                            current_dir.display(),
                            best_path_absolute.display()
                        ));
                        let mut stats = stats_mutex.lock().unwrap();
                        stats.errors += 1;
                        continue;
                    }
                };

                if let Err(e) = fs::remove_file(current_path) {
                    logger::error(&format!(
                        "Failed to remove {}: {}",
                        current_path.display(),
                        e
                    ));
                    let mut stats = stats_mutex.lock().unwrap();
                    stats.errors += 1;
                    continue;
                }

                if let Err(e) = unix_fs::symlink(&best_path_relative, current_path) {
                    logger::error(&format!(
                        "Failed to create symlink {} -> {}: {}",
                        current_path.display(),
                        best_path_relative.display(),
                        e
                    ));
                    let mut stats = stats_mutex.lock().unwrap();
                    stats.errors += 1;
                    continue;
                }
            }

            {
                let mut replaced = replaced_count_mutex.lock().unwrap();
                *replaced += 1;
                let mut stats = stats_mutex.lock().unwrap();
                stats.succeeded += 1;
            }
        }
    });

    pb2.finish_and_clear();

    // Extract final values from mutexes
    let stats = Arc::try_unwrap(stats_mutex).unwrap().into_inner().unwrap();
    let replaced_count = Arc::try_unwrap(replaced_count_mutex)
        .unwrap()
        .into_inner()
        .unwrap();

    logger::success(&format!(
        "Deduplication complete: {} duplicate groups found, {} files replaced with symlinks",
        duplicate_groups_found, replaced_count
    ));
    stats.print_summary("Cross-Library Deduplication");

    Ok(stats)
}
