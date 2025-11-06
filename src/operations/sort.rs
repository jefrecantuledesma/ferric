use crate::config::Config;
use crate::logger;
use crate::metadata::AudioMetadata;
use crate::operations::OperationStats;
use crate::quality;
use crate::utils;
use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use std::fs;
use std::path::PathBuf;
use walkdir::WalkDir;

pub struct SortOptions {
    pub input_dir: PathBuf,
    pub output_dir: PathBuf,
    pub do_move: bool,
    pub dry_run: bool,
    pub verbose: bool,
    pub config: Config,
}

/// Sort files with intelligent quality comparison and upgrading
pub fn run(options: SortOptions) -> Result<OperationStats> {
    logger::stage("Starting intelligent quality-aware sort");
    logger::info(&format!("Input directory: {}", options.input_dir.display()));
    logger::info(&format!("Output directory: {}", options.output_dir.display()));
    logger::info("Will only replace files with higher quality versions");

    if options.dry_run {
        logger::warning("DRY RUN MODE - No files will be modified");
    }

    let mut stats = OperationStats::new();
    let mut replaced_count = 0;

    // Collect all audio files
    let files: Vec<PathBuf> = WalkDir::new(&options.input_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .filter(|p| utils::is_audio_file(p))
        .collect();

    logger::info(&format!("Found {} audio files", files.len()));

    let pb = ProgressBar::new(files.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] [{bar:40}] {pos}/{len} ({eta}) {msg}")
            .unwrap()
            .progress_chars("=>-"),
    );

    for file in &files {
        pb.inc(1);
        stats.processed += 1;

        pb.set_message(format!("Processing: {}", file.file_name().unwrap().to_string_lossy()));

        // Extract metadata
        let metadata = match AudioMetadata::from_file(file) {
            Ok(m) => m,
            Err(e) => {
                logger::error(&format!("Failed to read metadata from {}: {}", file.display(), e));
                stats.errors += 1;
                continue;
            }
        };

        let new_quality = quality::calculate_quality_score(&metadata, &options.config);

        // Determine destination
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

        // Check for existing file with same title
        let title_normalized = utils::normalize_for_comparison(&title);
        let mut existing_match: Option<(PathBuf, u32)> = None;

        if dest_dir.exists() {
            for entry in WalkDir::new(&dest_dir)
                .max_depth(1)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().is_file() && utils::is_audio_file(e.path()))
            {
                if let Ok(existing_meta) = AudioMetadata::from_file(entry.path()) {
                    let existing_title = existing_meta.get_title();
                    let existing_normalized = utils::normalize_for_comparison(&existing_title);

                    if title_normalized == existing_normalized {
                        let existing_quality = quality::calculate_quality_score(&existing_meta, &options.config);
                        existing_match = Some((entry.path().to_path_buf(), existing_quality));
                        break;
                    }
                }
            }
        }

        let action = if let Some((existing_path, existing_quality)) = existing_match {
            if new_quality > existing_quality {
                Some(("replace", existing_path, new_quality, existing_quality))
            } else if new_quality == existing_quality {
                logger::debug(
                    &format!("Skipping (same quality {}): {}", new_quality, file.display()),
                    options.verbose,
                );
                stats.add_skipped(file.clone(), format!("same quality ({})", new_quality));
                continue;
            } else {
                logger::debug(
                    &format!("Skipping (lower quality {} < {}): {}", new_quality, existing_quality, file.display()),
                    options.verbose,
                );
                stats.add_skipped(file.clone(), format!("lower quality ({} < {})", new_quality, existing_quality));
                continue;
            }
        } else {
            Some(("copy", dest_path, new_quality, 0))
        };

        if let Some((action_type, target_path, new_q, old_q)) = action {
            if options.dry_run {
                if action_type == "replace" {
                    logger::debug(
                        &format!("Would replace (quality {} > {}): {}", new_q, old_q, target_path.display()),
                        options.verbose,
                    );
                } else {
                    logger::debug(
                        &format!("Would copy to: {}", target_path.display()),
                        options.verbose,
                    );
                }
                stats.succeeded += 1;
            } else {
                // Create destination directory
                if let Err(e) = fs::create_dir_all(&dest_dir) {
                    logger::error(&format!("Failed to create directory {}: {}", dest_dir.display(), e));
                    stats.errors += 1;
                    continue;
                }

                // Perform action
                let result = if action_type == "replace" {
                    fs::copy(file, &target_path).map(|_| ())
                } else if options.do_move {
                    fs::rename(file, &target_path)
                } else {
                    fs::copy(file, &target_path).map(|_| ())
                };

                match result {
                    Ok(_) => {
                        if action_type == "replace" {
                            logger::debug(
                                &format!("Replaced (quality {} > {}): {}", new_q, old_q, target_path.display()),
                                options.verbose,
                            );
                            replaced_count += 1;
                        } else {
                            logger::debug(
                                &format!("Copied to: {}", target_path.display()),
                                options.verbose,
                            );
                        }
                        stats.succeeded += 1;
                    }
                    Err(e) => {
                        logger::error(&format!("Failed to process {}: {}", file.display(), e));
                        stats.errors += 1;
                    }
                }
            }
        }
    }

    pb.finish_and_clear();

    logger::success(&format!("Upgraded {} files with better quality", replaced_count));
    stats.print_summary("Quality-Aware Sort");
    Ok(stats)
}
