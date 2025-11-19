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

pub struct MergeLibrariesOptions {
    pub input_dirs: Vec<PathBuf>,
    pub output_dir: PathBuf,
    pub dry_run: bool,
    pub verbose: bool,
    pub config: Config,
}

#[derive(Debug, Clone)]
struct FileInfo {
    path: PathBuf,
    metadata: AudioMetadata,
    quality: u32,
    dest_dir: PathBuf,
    dest_path: PathBuf,
    song_id: String, // Normalized identifier for deduplication
}

/// Merge multiple music libraries into one using symlinks, keeping highest quality versions
pub fn run(options: MergeLibrariesOptions) -> Result<OperationStats> {
    logger::stage("Starting library merge with symlinks");
    logger::info(&format!(
        "Merging {} input libraries:",
        options.input_dirs.len()
    ));
    for (i, dir) in options.input_dirs.iter().enumerate() {
        logger::info(&format!("  Library {}: {}", i + 1, dir.display()));
    }
    logger::info(&format!(
        "Output directory: {}",
        options.output_dir.display()
    ));
    logger::info("Creating symlinks to highest quality versions");

    if options.dry_run {
        logger::warning("DRY RUN MODE - No files will be modified");
    }

    let stats = OperationStats::new();
    let symlink_created = 0;
    let symlink_upgraded = 0;

    // Phase 1: Collect all audio files from all input libraries (PARALLEL!)
    logger::info("Phase 1/4: Scanning input libraries in parallel...");

    let scan_pb = ProgressBar::new(options.input_dirs.len() as u64);
    scan_pb.set_style(
        ProgressStyle::default_bar()
            .template(
                "[{elapsed_precise}] [{bar:40}] {pos}/{len} libraries | Scanning in parallel...",
            )
            .unwrap()
            .progress_chars("█▓▒░"),
    );

    // Scan all libraries in parallel for maximum speed!
    let all_files: Vec<PathBuf> = options
        .input_dirs
        .par_iter()
        .flat_map(|input_dir| {
            scan_pb.inc(1);

            if !input_dir.exists() {
                logger::warning(&format!(
                    "Input directory does not exist: {}",
                    input_dir.display()
                ));
                return Vec::new();
            }

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

            // Determine destination based on metadata
            let artist = metadata.get_organizing_artist(options.config.naming.prefer_artist);
            let album = metadata.get_album();
            let title = metadata.get_title();

            let artist_safe = utils::clamp_component(
                &utils::sanitize(&artist),
                options.config.naming.max_name_length,
            );
            let album_safe = utils::clamp_component(
                &utils::sanitize(&album),
                options.config.naming.max_name_length,
            );

            let dest_dir = options.output_dir.join(&artist_safe).join(&album_safe);
            let filename = file.file_name().unwrap();
            let dest_path = dest_dir.join(filename);

            // Create a normalized song ID for deduplication
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
                dest_dir,
                dest_path,
                song_id,
            })
        })
        .collect();

    pb.finish_and_clear();
    logger::success(&format!(
        "Metadata extracted from {} files",
        file_infos.len()
    ));

    // Phase 3: Group by song ID and find best quality for each song
    logger::info("Phase 3/4: Finding highest quality versions...");
    let mut song_groups: HashMap<String, Vec<FileInfo>> = HashMap::new();

    for file_info in file_infos {
        song_groups
            .entry(file_info.song_id.clone())
            .or_insert_with(Vec::new)
            .push(file_info);
    }

    logger::info(&format!(
        "Found {} unique songs across all libraries",
        song_groups.len()
    ));

    // Find best version of each song
    let mut best_versions: Vec<FileInfo> = Vec::new();
    let mut duplicate_count = 0;

    for (_song_id, mut versions) in song_groups {
        if versions.len() > 1 {
            duplicate_count += versions.len() - 1;
            logger::debug(
                &format!(
                    "Song has {} versions: {}",
                    versions.len(),
                    versions[0].metadata.get_title()
                ),
                options.verbose,
            );
        }

        // Sort by quality (descending) and take the best
        versions.sort_by(|a, b| b.quality.cmp(&a.quality));
        let best = versions.into_iter().next().unwrap();

        logger::debug(
            &format!(
                "Best version (quality {}): {}",
                best.quality,
                best.path.display()
            ),
            options.verbose,
        );

        best_versions.push(best);
    }

    logger::success(&format!(
        "Identified {} duplicates, keeping best quality versions",
        duplicate_count
    ));

    // Phase 4: Create/update symlinks (PARALLEL!)
    logger::info("Phase 4/4: Creating symlinks in parallel...");
    let pb2 = ProgressBar::new(best_versions.len() as u64);
    pb2.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] [{bar:40}] {pos}/{len} ({eta}) | Creating symlinks...")
            .unwrap()
            .progress_chars("█▓▒░"),
    );

    // Thread-safe counters for parallel execution
    let stats_mutex = Arc::new(Mutex::new(stats));
    let symlink_created_mutex = Arc::new(Mutex::new(symlink_created));
    let symlink_upgraded_mutex = Arc::new(Mutex::new(symlink_upgraded));

    // Process all symlinks in parallel for maximum throughput!
    best_versions.par_iter().for_each(|file_info| {
        pb2.inc(1);
        {
            let mut stats = stats_mutex.lock().unwrap();
            stats.processed += 1;
        }

        // Compute relative path from destination directory to source file
        // This makes symlinks work inside Docker containers!
        let source_absolute = match fs::canonicalize(&file_info.path) {
            Ok(p) => p,
            Err(e) => {
                logger::error(&format!(
                    "Failed to resolve absolute path for {}: {}",
                    file_info.path.display(),
                    e
                ));
                let mut stats = stats_mutex.lock().unwrap();
                stats.errors += 1;
                return;
            }
        };

        let dest_path = &file_info.dest_path;
        let dest_dir = &file_info.dest_dir;

        // Create relative path from dest_dir to source file
        let source_path = match diff_paths(&source_absolute, dest_dir) {
            Some(relative) => relative,
            None => {
                logger::error(&format!(
                    "Failed to compute relative path from {} to {}",
                    dest_dir.display(),
                    source_absolute.display()
                ));
                let mut stats = stats_mutex.lock().unwrap();
                stats.errors += 1;
                return;
            }
        };

        let new_quality = file_info.quality;

        // Check if destination exists
        let dest_exists = dest_path.exists() || dest_path.symlink_metadata().is_ok();

        if dest_exists {
            // Check if it's a symlink
            let metadata = match fs::symlink_metadata(dest_path) {
                Ok(m) => m,
                Err(e) => {
                    logger::error(&format!(
                        "Failed to read metadata for {}: {}",
                        dest_path.display(),
                        e
                    ));
                    let mut stats = stats_mutex.lock().unwrap();
                    stats.errors += 1;
                    return;
                }
            };

            if metadata.is_symlink() {
                // It's a symlink - check what it points to
                let existing_target = match fs::read_link(dest_path) {
                    Ok(t) => t,
                    Err(e) => {
                        logger::error(&format!(
                            "Failed to read symlink {}: {}",
                            dest_path.display(),
                            e
                        ));
                        let mut stats = stats_mutex.lock().unwrap();
                        stats.errors += 1;
                        return;
                    }
                };

                // Resolve existing target to absolute path for comparison
                // (it might be relative or absolute)
                let existing_target_absolute = if existing_target.is_absolute() {
                    existing_target.clone()
                } else {
                    dest_dir.join(&existing_target)
                };

                // Canonicalize for proper comparison
                let existing_target_canonical = match fs::canonicalize(&existing_target_absolute) {
                    Ok(p) => p,
                    Err(_) => existing_target_absolute, // Use as-is if canonicalize fails (broken symlink)
                };

                // If it already points to the same file, skip
                if existing_target_canonical == source_absolute {
                    logger::debug(
                        &format!(
                            "Symlink already points to correct file: {}",
                            dest_path.display()
                        ),
                        options.verbose,
                    );
                    let mut stats = stats_mutex.lock().unwrap();
                    stats.add_skipped(source_absolute.clone(), "symlink already correct".to_string());
                    return;
                }

                // Get quality of existing target
                let existing_quality =
                    if let Ok(existing_meta) = AudioMetadata::from_file(&existing_target) {
                        quality::calculate_quality_score(&existing_meta, &options.config)
                    } else {
                        // If we can't read the existing file, assume we should replace it
                        logger::warning(&format!(
                            "Cannot read existing symlink target {}, will replace",
                            existing_target.display()
                        ));
                        0
                    };

                if new_quality > existing_quality {
                    // Upgrade!
                    logger::debug(
                        &format!(
                            "Upgrading symlink (quality {} > {}): {}",
                            new_quality,
                            existing_quality,
                            dest_path.display()
                        ),
                        options.verbose,
                    );

                    if !options.dry_run {
                        // Remove old symlink
                        if let Err(e) = fs::remove_file(dest_path) {
                            logger::error(&format!(
                                "Failed to remove old symlink {}: {}",
                                dest_path.display(),
                                e
                            ));
                            let mut stats = stats_mutex.lock().unwrap();
                            stats.errors += 1;
                            return;
                        }

                        // Create new symlink
                        if let Err(e) = unix_fs::symlink(&source_path, dest_path) {
                            logger::error(&format!(
                                "Failed to create symlink {} -> {}: {}",
                                dest_path.display(),
                                source_path.display(),
                                e
                            ));
                            let mut stats = stats_mutex.lock().unwrap();
                            stats.errors += 1;
                            return;
                        }
                    }

                    {
                        let mut upgraded = symlink_upgraded_mutex.lock().unwrap();
                        *upgraded += 1;
                        let mut stats = stats_mutex.lock().unwrap();
                        stats.succeeded += 1;
                    }
                } else if new_quality == existing_quality {
                    logger::debug(
                        &format!(
                            "Skipping (same quality {}): {}",
                            new_quality,
                            dest_path.display()
                        ),
                        options.verbose,
                    );
                    let mut stats = stats_mutex.lock().unwrap();
                    stats.add_skipped(
                        source_absolute.clone(),
                        format!("same quality ({})", new_quality),
                    );
                } else {
                    logger::debug(
                        &format!(
                            "Skipping (lower quality {} < {}): {}",
                            new_quality,
                            existing_quality,
                            dest_path.display()
                        ),
                        options.verbose,
                    );
                    let mut stats = stats_mutex.lock().unwrap();
                    stats.add_skipped(
                        source_absolute.clone(),
                        format!("lower quality ({} < {})", new_quality, existing_quality),
                    );
                }
            } else {
                // Destination exists but is NOT a symlink - this is a problem
                logger::error(&format!(
                    "Destination exists but is not a symlink (skipping): {}",
                    dest_path.display()
                ));
                let mut stats = stats_mutex.lock().unwrap();
                stats.add_skipped(
                    source_absolute.clone(),
                    "destination is not a symlink".to_string(),
                );
            }
        } else {
            // Destination doesn't exist - create new symlink
            logger::debug(
                &format!(
                    "Creating symlink: {} -> {}",
                    dest_path.display(),
                    source_path.display()
                ),
                options.verbose,
            );

            if !options.dry_run {
                // Create destination directory if needed
                if let Err(e) = fs::create_dir_all(dest_dir) {
                    logger::error(&format!(
                        "Failed to create directory {}: {}",
                        dest_dir.display(),
                        e
                    ));
                    let mut stats = stats_mutex.lock().unwrap();
                    stats.errors += 1;
                    return;
                }

                // Create symlink
                if let Err(e) = unix_fs::symlink(&source_path, dest_path) {
                    logger::error(&format!(
                        "Failed to create symlink {} -> {}: {}",
                        dest_path.display(),
                        source_path.display(),
                        e
                    ));
                    let mut stats = stats_mutex.lock().unwrap();
                    stats.errors += 1;
                    return;
                }
            }

            {
                let mut created = symlink_created_mutex.lock().unwrap();
                *created += 1;
                let mut stats = stats_mutex.lock().unwrap();
                stats.succeeded += 1;
            }
        }
    });

    pb2.finish_and_clear();

    // Extract final values from mutexes
    let stats = Arc::try_unwrap(stats_mutex).unwrap().into_inner().unwrap();
    let symlink_created = Arc::try_unwrap(symlink_created_mutex)
        .unwrap()
        .into_inner()
        .unwrap();
    let symlink_upgraded = Arc::try_unwrap(symlink_upgraded_mutex)
        .unwrap()
        .into_inner()
        .unwrap();

    logger::success(&format!(
        "Merge complete: {} new symlinks created, {} symlinks upgraded",
        symlink_created, symlink_upgraded
    ));
    stats.print_summary("Library Merge (Symlinks)");

    Ok(stats)
}
