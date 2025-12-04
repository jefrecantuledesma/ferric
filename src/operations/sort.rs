use crate::config::Config;
use crate::logger;
use crate::metadata::AudioMetadata;
use crate::operations::OperationStats;
use crate::quality;
use crate::utils;
use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use walkdir::WalkDir;

pub struct SortOptions {
    pub input_dir: PathBuf,
    pub output_dir: PathBuf,
    pub do_move: bool,
    pub fix_naming: bool,
    pub dry_run: bool,
    pub verbose: bool,
    pub force: bool,
    pub destructive: bool,
    pub config: Config,
}

struct FileInfo {
    path: PathBuf,
    metadata: AudioMetadata,
    quality: u32,
    dest_path: PathBuf,
}

/// Recursively remove empty parent directories up to (but not including) the root directory
/// Also removes directories that only contain non-audio files (like leftover cover art)
fn cleanup_empty_dirs(file_path: &PathBuf, root_dir: &PathBuf) {
    if let Some(parent) = file_path.parent() {
        let parent_path = parent.to_path_buf();

        // Don't remove the root directory itself
        if parent_path == *root_dir {
            return;
        }

        // Check directory contents
        if let Ok(entries) = fs::read_dir(&parent_path) {
            let remaining_files: Vec<PathBuf> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .collect();

            // Check if directory is empty or only contains non-audio files
            let only_non_audio = remaining_files.iter()
                .all(|p| p.is_dir() || !utils::is_audio_file(p));

            if remaining_files.is_empty() || only_non_audio {
                // Remove any leftover non-audio files first
                for file in remaining_files.iter().filter(|p| p.is_file()) {
                    if let Err(e) = fs::remove_file(file) {
                        logger::debug(
                            &format!("Failed to remove leftover file {}: {}", file.display(), e),
                            false,
                        );
                    } else {
                        logger::debug(
                            &format!("Removed leftover file: {}", file.display()),
                            false,
                        );
                    }
                }

                // Now remove the directory
                if let Err(e) = fs::remove_dir(&parent_path) {
                    logger::debug(
                        &format!("Failed to remove directory {}: {}", parent_path.display(), e),
                        false,
                    );
                } else {
                    logger::debug(
                        &format!("Removed directory: {}", parent_path.display()),
                        false,
                    );
                    // Recursively check parent directories
                    cleanup_empty_dirs(&parent_path, root_dir);
                }
            }
        }
    }
}

/// Check if a file is already organized in the correct Artist/Album structure
/// Returns true if the file's current path matches the expected path based on metadata
fn is_already_organized(file_path: &PathBuf, metadata: &AudioMetadata, options: &SortOptions) -> bool {
    // Get the expected artist and album from metadata
    let artist = metadata.get_organizing_artist(options.config.naming.prefer_artist);
    let album = metadata.get_album();

    // Apply normalization if fix_naming is enabled
    let (artist_expected, album_expected) = if options.fix_naming {
        (utils::normalize_name(&artist), utils::normalize_name(&album))
    } else {
        (artist.clone(), album.clone())
    };

    // Sanitize and clamp the names
    let artist_expected = utils::clamp_component(
        &utils::sanitize(&artist_expected),
        options.config.naming.max_name_length,
    );
    let album_expected = utils::clamp_component(
        &utils::sanitize(&album_expected),
        options.config.naming.max_name_length,
    );

    // Get the current file's parent directories
    let parent_album = match file_path.parent() {
        Some(p) => p,
        None => return false,
    };

    let parent_artist = match parent_album.parent() {
        Some(p) => p,
        None => return false,
    };

    // Extract the folder names
    let current_album = match parent_album.file_name() {
        Some(name) => name.to_string_lossy().to_string(),
        None => return false,
    };

    let current_artist = match parent_artist.file_name() {
        Some(name) => name.to_string_lossy().to_string(),
        None => return false,
    };

    // Compare case-insensitively
    current_artist.to_lowercase() == artist_expected.to_lowercase()
        && current_album.to_lowercase() == album_expected.to_lowercase()
}

/// Sort files into Artist/Album folder structure based on metadata
pub fn run(options: SortOptions) -> Result<OperationStats> {
    logger::stage("Sorting files by metadata into Artist/Album structure");
    logger::info(&format!("Input directory: {}", options.input_dir.display()));
    logger::info(&format!("Output directory: {}", options.output_dir.display()));

    if options.force {
        logger::info("Force mode enabled - will re-sort all files including already-organized ones");
    } else {
        logger::info("Skipping files that are already organized (use --force to override)");
    }

    if options.destructive {
        logger::warning("DESTRUCTIVE MODE - Will delete lower quality duplicate files");
    }

    if options.fix_naming {
        logger::info("Will normalize file and folder names");
    }

    if options.dry_run {
        logger::warning("DRY RUN MODE - No files will be modified");
    }

    let stats_mutex = Arc::new(Mutex::new(OperationStats::new()));
    let duplicate_count = Arc::new(Mutex::new(0_usize));

    let files: Vec<PathBuf> = WalkDir::new(&options.input_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .filter(|p| utils::is_audio_file(p))
        .collect();

    logger::info(&format!("Found {} audio files", files.len()));

    logger::info("Extracting metadata and organizing files...");
    let pb = ProgressBar::new(files.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] [{bar:40}] {pos}/{len} ({eta}) | Organizing...")
            .unwrap()
            .progress_chars("█▓▒░"),
    );

    // Build all file info in parallel
    let file_infos: Vec<FileInfo> = files
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
                    let mut stats = stats_mutex.lock().unwrap();
                    stats.errors += 1;
                    return None;
                }
            };

            // Skip files that are already organized unless force is enabled
            if !options.force && is_already_organized(file, &metadata, &options) {
                logger::debug(
                    &format!("File already organized, skipping: {}", file.display()),
                    options.verbose,
                );
                let mut stats = stats_mutex.lock().unwrap();
                stats.add_skipped(file.clone(), "already organized".to_string());
                return None;
            }

            let quality = quality::calculate_quality_score(&metadata, &options.config);

            let artist = metadata.get_organizing_artist(options.config.naming.prefer_artist);
            let album = metadata.get_album();
            let title = metadata.get_title();

            // Determine destination path
            let (artist_final, album_final, title_final) = if options.fix_naming {
                (
                    utils::normalize_name(&artist),
                    utils::normalize_name(&album),
                    utils::normalize_name(&title),
                )
            } else {
                (artist.clone(), album.clone(), title.clone())
            };

            let artist_safe = utils::clamp_component(
                &utils::sanitize(&artist_final),
                options.config.naming.max_name_length,
            );
            let album_safe = utils::clamp_component(
                &utils::sanitize(&album_final),
                options.config.naming.max_name_length,
            );

            let filename = if options.fix_naming {
                let title_safe = utils::sanitize(&title_final);
                let ext = utils::get_extension(file).unwrap_or_else(|| "mp3".to_string());

                if let Some(track_num) = metadata.track_number {
                    format!("{:02} - {}.{}", track_num, title_safe, ext)
                } else {
                    format!("{}.{}", title_safe, ext)
                }
            } else {
                file.file_name().unwrap().to_string_lossy().to_string()
            };

            let dest_dir = options.output_dir.join(&artist_safe).join(&album_safe);
            let dest_path = dest_dir.join(&filename);

            Some(FileInfo {
                path: file.clone(),
                metadata,
                quality,
                dest_path,
            })
        })
        .collect();

    pb.finish_and_clear();

    // Group files by destination to detect duplicates
    let mut dest_map: HashMap<PathBuf, Vec<FileInfo>> = HashMap::new();
    for file_info in file_infos {
        dest_map
            .entry(file_info.dest_path.clone())
            .or_insert_with(Vec::new)
            .push(file_info);
    }

    logger::info("Processing files...");
    let pb2 = ProgressBar::new(dest_map.len() as u64);
    pb2.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] [{bar:40}] {pos}/{len} ({eta}) | Processing...")
            .unwrap()
            .progress_chars("█▓▒░"),
    );

    dest_map.par_iter().for_each(|(dest_path, files)| {
        pb2.inc(1);

        let file_to_use = if files.len() > 1 {
            // Multiple files want same destination - keep highest quality
            let mut dup_count = duplicate_count.lock().unwrap();
            *dup_count += files.len() - 1;

            logger::warning(&format!(
                "Found {} files with same metadata, keeping highest quality (or oldest if equal): {}",
                files.len(),
                dest_path.display()
            ));

            // Choose best file: highest quality, then oldest modification time if quality is equal
            let best_file = files.iter().max_by(|a, b| {
                match a.quality.cmp(&b.quality) {
                    std::cmp::Ordering::Equal => {
                        // If quality is equal, prefer older file (lower mtime)
                        let a_mtime = fs::metadata(&a.path)
                            .and_then(|m| m.modified())
                            .ok();
                        let b_mtime = fs::metadata(&b.path)
                            .and_then(|m| m.modified())
                            .ok();
                        b_mtime.cmp(&a_mtime) // Reversed: b compared to a = prefer older
                    }
                    other => other,
                }
            }).unwrap();

            // Delete lower quality duplicates if destructive mode is enabled
            if options.destructive && !options.dry_run {
                for file in files.iter() {
                    if file.path != best_file.path {
                        if let Err(e) = fs::remove_file(&file.path) {
                            logger::error(&format!(
                                "Failed to delete duplicate file {}: {}",
                                file.path.display(),
                                e
                            ));
                        } else {
                            logger::debug(
                                &format!("Deleted duplicate file: {}", file.path.display()),
                                options.verbose,
                            );
                            // Clean up any empty directories left behind
                            cleanup_empty_dirs(&file.path, &options.input_dir);
                        }
                    }
                }
            }

            best_file
        } else {
            &files[0]
        };

        let mut stats = stats_mutex.lock().unwrap();
        stats.processed += 1;

        // Check if destination already exists
        if dest_path.exists() && dest_path.canonicalize().ok() == file_to_use.path.canonicalize().ok() {
            // Source and destination are the same file, skip
            logger::debug(
                &format!("File already in correct location: {}", dest_path.display()),
                options.verbose,
            );
            stats.add_skipped(file_to_use.path.clone(), "already in correct location".to_string());
            return;
        }

        if dest_path.exists() {
            // Destination exists but is a different file - compare quality
            if let Ok(existing_meta) = AudioMetadata::from_file(dest_path) {
                let existing_quality = quality::calculate_quality_score(&existing_meta, &options.config);

                if file_to_use.quality > existing_quality {
                    logger::debug(
                        &format!(
                            "Replacing lower quality file (quality {} > {}): {}",
                            file_to_use.quality,
                            existing_quality,
                            dest_path.display()
                        ),
                        options.verbose,
                    );
                } else if file_to_use.quality < existing_quality {
                    logger::debug(
                        &format!(
                            "Skipping lower quality file (quality {} < {}): {}",
                            file_to_use.quality,
                            existing_quality,
                            file_to_use.path.display()
                        ),
                        options.verbose,
                    );
                    stats.add_skipped(
                        file_to_use.path.clone(),
                        format!("lower quality ({} < {})", file_to_use.quality, existing_quality),
                    );

                    // Delete lower quality file if destructive mode is enabled
                    if options.destructive && !options.dry_run {
                        if let Err(e) = fs::remove_file(&file_to_use.path) {
                            logger::error(&format!(
                                "Failed to delete lower quality file {}: {}",
                                file_to_use.path.display(),
                                e
                            ));
                        } else {
                            logger::debug(
                                &format!("Deleted lower quality file: {}", file_to_use.path.display()),
                                options.verbose,
                            );
                            // Clean up any empty directories left behind
                            cleanup_empty_dirs(&file_to_use.path, &options.input_dir);
                        }
                    }

                    return;
                }
            }
        }

        if options.dry_run {
            logger::debug(
                &format!(
                    "Would {} {} -> {}",
                    if options.do_move { "move" } else { "copy" },
                    file_to_use.path.display(),
                    dest_path.display()
                ),
                options.verbose,
            );
            stats.succeeded += 1;
        } else {
            // Create destination directory
            if let Some(parent) = dest_path.parent() {
                if let Err(e) = fs::create_dir_all(parent) {
                    logger::error(&format!(
                        "Failed to create directory {}: {}",
                        parent.display(),
                        e
                    ));
                    stats.errors += 1;
                    return;
                }
            }

            // Perform copy or move
            let result = if options.do_move {
                fs::rename(&file_to_use.path, dest_path)
            } else {
                fs::copy(&file_to_use.path, dest_path).map(|_| ())
            };

            match result {
                Ok(_) => {
                    logger::debug(
                        &format!(
                            "{} {} -> {}",
                            if options.do_move { "Moved" } else { "Copied" },
                            file_to_use.path.display(),
                            dest_path.display()
                        ),
                        options.verbose,
                    );
                    stats.succeeded += 1;

                    // If we moved the file, clean up any empty directories left behind
                    if options.do_move {
                        cleanup_empty_dirs(&file_to_use.path, &options.input_dir);
                    }
                }
                Err(e) => {
                    logger::error(&format!(
                        "Failed to {} {}: {}",
                        if options.do_move { "move" } else { "copy" },
                        file_to_use.path.display(),
                        e
                    ));
                    stats.errors += 1;
                }
            }
        }
    });

    pb2.finish_and_clear();

    let dup_count = *duplicate_count.lock().unwrap();
    if dup_count > 0 {
        logger::warning(&format!(
            "Found {} duplicate files (same metadata), kept highest quality versions",
            dup_count
        ));
    }

    let stats = Arc::try_unwrap(stats_mutex)
        .unwrap()
        .into_inner()
        .unwrap();

    stats.print_summary("Sort");
    Ok(stats)
}
