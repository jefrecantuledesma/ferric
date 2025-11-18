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

pub struct MergeOptions {
    pub input_dir: PathBuf,
    pub output_dir: PathBuf,
    pub do_move: bool,
    pub dry_run: bool,
    pub verbose: bool,
    pub config: Config,
}

/// Merge an organized library into another, upgrading files with better quality
pub fn run(options: MergeOptions) -> Result<OperationStats> {
    logger::stage("Starting library merge");
    logger::info(&format!("Source library: {}", options.input_dir.display()));
    logger::info(&format!("Target library: {}", options.output_dir.display()));
    logger::info("Will only replace files with higher quality versions");

    if options.dry_run {
        logger::warning("DRY RUN MODE - No files will be modified");
    }

    let mut stats = OperationStats::new();
    let mut replaced_count = 0;
    let mut added_count = 0;

    // Collect all audio files from source
    let files: Vec<PathBuf> = WalkDir::new(&options.input_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .filter(|p| utils::is_audio_file(p))
        .collect();

    logger::info(&format!("Found {} audio files to merge", files.len()));

    // Phase 1: Parallel metadata extraction
    logger::info("Phase 1/2: Reading metadata in parallel...");
    let pb = ProgressBar::new(files.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] [{bar:40}] {pos}/{len} ({eta}) | Extracting metadata...")
            .unwrap()
            .progress_chars("█▓▒░"),
    );

    struct FileInfo {
        path: PathBuf,
        metadata: AudioMetadata,
        quality: u32,
        dest_dir: PathBuf,
        dest_path: PathBuf,
        title_normalized: String,
    }

    let file_infos: Vec<FileInfo> = files
        .par_iter()
        .filter_map(|file| {
            pb.inc(1);

            // Extract metadata
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
            let title_normalized = utils::normalize_for_comparison(&title);

            Some(FileInfo {
                path: file.clone(),
                metadata,
                quality,
                dest_dir,
                dest_path,
                title_normalized,
            })
        })
        .collect();

    pb.finish_and_clear();

    logger::info(&format!(
        "Metadata extracted from {} files",
        file_infos.len()
    ));

    // Phase 1.5: Build index of existing files in output directory (MASSIVE performance optimization!)
    logger::info("Building index of existing files...");
    let existing_files_index: Arc<Mutex<HashMap<String, (PathBuf, u32)>>> =
        Arc::new(Mutex::new(HashMap::new()));

    if options.output_dir.exists() {
        let existing_files: Vec<PathBuf> = WalkDir::new(&options.output_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .map(|e| e.path().to_path_buf())
            .filter(|p| utils::is_audio_file(p))
            .collect();

        logger::info(&format!(
            "Indexing {} existing files...",
            existing_files.len()
        ));

        let index_pb = ProgressBar::new(existing_files.len() as u64);
        index_pb.set_style(
            ProgressStyle::default_bar()
                .template("[{elapsed_precise}] [{bar:40}] {pos}/{len} ({eta}) | Building index...")
                .unwrap()
                .progress_chars("█▓▒░"),
        );

        existing_files.par_iter().for_each(|file| {
            index_pb.inc(1);
            if let Ok(metadata) = AudioMetadata::from_file(file) {
                let artist = metadata.get_organizing_artist(options.config.naming.prefer_artist);
                let album = metadata.get_album();
                let title = metadata.get_title();
                let title_normalized = utils::normalize_for_comparison(&title);

                let artist_safe = utils::clamp_component(
                    &utils::sanitize(&artist),
                    options.config.naming.max_name_length,
                );
                let album_safe = utils::clamp_component(
                    &utils::sanitize(&album),
                    options.config.naming.max_name_length,
                );

                // Create a unique key: "artist/album/title"
                let key = format!("{}/{}/{}", artist_safe, album_safe, title_normalized);
                let quality_score = quality::calculate_quality_score(&metadata, &options.config);

                let mut index = existing_files_index.lock().unwrap();
                index.insert(key, (file.clone(), quality_score));
            }
        });

        index_pb.finish_and_clear();
        let index = existing_files_index.lock().unwrap();
        logger::success(&format!("Indexed {} existing files", index.len()));
    }

    // Phase 2: Merging files with instant O(1) lookups!
    logger::info("Phase 2/2: Merging files...");
    let pb2 = ProgressBar::new(file_infos.len() as u64);
    pb2.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] [{bar:40}] {pos}/{len} ({eta}) | Merging libraries...")
            .unwrap()
            .progress_chars("█▓▒░"),
    );

    for file_info in &file_infos {
        pb2.inc(1);
        stats.processed += 1;

        let new_quality = file_info.quality;
        let dest_dir = &file_info.dest_dir;
        let dest_path = &file_info.dest_path;
        let title_normalized = &file_info.title_normalized;

        // O(1) lookup in the index instead of O(n) directory scan!
        let artist = file_info
            .metadata
            .get_organizing_artist(options.config.naming.prefer_artist);
        let album = file_info.metadata.get_album();
        let artist_safe = utils::clamp_component(
            &utils::sanitize(&artist),
            options.config.naming.max_name_length,
        );
        let album_safe = utils::clamp_component(
            &utils::sanitize(&album),
            options.config.naming.max_name_length,
        );
        let lookup_key = format!("{}/{}/{}", artist_safe, album_safe, title_normalized);

        let existing_match: Option<(PathBuf, u32)> = {
            let index = existing_files_index.lock().unwrap();
            index.get(&lookup_key).cloned()
        };

        // Decide what to do based on quality comparison
        let action = if let Some((existing_path, existing_quality)) = existing_match {
            if new_quality > existing_quality {
                Some(("upgrade", existing_path, new_quality, existing_quality))
            } else if new_quality == existing_quality {
                logger::debug(
                    &format!(
                        "Skipping (same quality {}): {}",
                        new_quality,
                        file_info.path.display()
                    ),
                    options.verbose,
                );
                stats.add_skipped(
                    file_info.path.clone(),
                    format!("same quality ({})", new_quality),
                );
                continue;
            } else {
                logger::debug(
                    &format!(
                        "Skipping (lower quality {} < {}): {}",
                        new_quality,
                        existing_quality,
                        file_info.path.display()
                    ),
                    options.verbose,
                );
                stats.add_skipped(
                    file_info.path.clone(),
                    format!("lower quality ({} < {})", new_quality, existing_quality),
                );
                continue;
            }
        } else {
            Some(("add", dest_path.clone(), new_quality, 0))
        };

        if let Some((action_type, target_path, new_q, old_q)) = action {
            if options.dry_run {
                if action_type == "upgrade" {
                    logger::debug(
                        &format!(
                            "Would upgrade (quality {} > {}): {}",
                            new_q,
                            old_q,
                            target_path.display()
                        ),
                        options.verbose,
                    );
                } else {
                    logger::debug(
                        &format!("Would add: {}", target_path.display()),
                        options.verbose,
                    );
                }
                stats.succeeded += 1;
            } else {
                // Create destination directory if needed (won't fail if exists)
                if let Err(e) = fs::create_dir_all(&dest_dir) {
                    logger::error(&format!(
                        "Failed to create directory {}: {}",
                        dest_dir.display(),
                        e
                    ));
                    stats.errors += 1;
                    continue;
                }

                // Perform action
                let result = if action_type == "upgrade" {
                    // Always copy when upgrading (even if do_move is true)
                    fs::copy(&file_info.path, &target_path).map(|_| ())
                } else if options.do_move {
                    fs::rename(&file_info.path, &target_path)
                } else {
                    fs::copy(&file_info.path, &target_path).map(|_| ())
                };

                match result {
                    Ok(_) => {
                        if action_type == "upgrade" {
                            logger::debug(
                                &format!(
                                    "Upgraded (quality {} > {}): {}",
                                    new_q,
                                    old_q,
                                    target_path.display()
                                ),
                                options.verbose,
                            );
                            replaced_count += 1;
                        } else {
                            logger::debug(
                                &format!("Added: {}", target_path.display()),
                                options.verbose,
                            );
                            added_count += 1;
                        }
                        stats.succeeded += 1;
                    }
                    Err(e) => {
                        logger::error(&format!(
                            "Failed to process {}: {}",
                            file_info.path.display(),
                            e
                        ));
                        stats.errors += 1;
                    }
                }
            }
        }
    }

    pb2.finish_and_clear();

    logger::success(&format!(
        "Merge complete: {} files added, {} files upgraded",
        added_count, replaced_count
    ));
    stats.print_summary("Library Merge");
    Ok(stats)
}
