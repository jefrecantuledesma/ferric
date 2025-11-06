use crate::config::Config;
use crate::logger;
use crate::metadata::AudioMetadata;
use crate::operations::OperationStats;
use crate::utils;
use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use std::fs;
use std::path::PathBuf;
use walkdir::WalkDir;

pub struct TagSortOptions {
    pub input_dir: PathBuf,
    pub output_dir: PathBuf,
    pub do_move: bool,
    pub dry_run: bool,
    pub verbose: bool,
    pub config: Config,
}

/// Sort files by metadata tags into Artist/Album structure
pub fn run(options: TagSortOptions) -> Result<OperationStats> {
    logger::stage("Starting tag-based sort");
    logger::info(&format!("Input directory: {}", options.input_dir.display()));
    logger::info(&format!("Output directory: {}", options.output_dir.display()));
    logger::info(&format!("Mode: {}", if options.do_move { "MOVE" } else { "COPY" }));

    if options.dry_run {
        logger::warning("DRY RUN MODE - No files will be modified");
    }

    let mut stats = OperationStats::new();

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

        // Determine artist and album
        let artist = metadata.get_organizing_artist(options.config.naming.prefer_artist);
        let album = metadata.get_album();

        // Sanitize and clamp
        let artist_safe = utils::clamp_component(
            &utils::sanitize(&artist),
            options.config.naming.max_name_length,
        );
        let album_safe = utils::clamp_component(
            &utils::sanitize(&album),
            options.config.naming.max_name_length,
        );

        // Build destination path
        let dest_dir = options.output_dir.join(&artist_safe).join(&album_safe);
        let filename = file.file_name().unwrap();
        let dest_path = dest_dir.join(filename);

        // Handle conflicts
        let final_dest = if dest_path.exists() {
            utils::unique_path(&dest_path)
        } else {
            dest_path
        };

        if options.dry_run {
            logger::debug(
                &format!("Would {} to: {}", if options.do_move { "move" } else { "copy" }, final_dest.display()),
                options.verbose,
            );
            stats.succeeded += 1;
        } else {
            // Create destination directory
            if let Err(e) = fs::create_dir_all(&dest_dir) {
                logger::error(&format!("Failed to create directory {}: {}", dest_dir.display(), e));
                stats.errors += 1;
                continue;
            }

            // Copy or move
            let result = if options.do_move {
                fs::rename(file, &final_dest)
            } else {
                fs::copy(file, &final_dest).map(|_| ())
            };

            match result {
                Ok(_) => {
                    logger::debug(
                        &format!("{} to: {}", if options.do_move { "Moved" } else { "Copied" }, final_dest.display()),
                        options.verbose,
                    );
                    stats.succeeded += 1;
                }
                Err(e) => {
                    logger::error(&format!("Failed to {} {}: {}", if options.do_move { "move" } else { "copy" }, file.display(), e));
                    stats.errors += 1;
                }
            }
        }
    }

    pb.finish_and_clear();
    stats.print_summary("Tag-Based Sort");
    Ok(stats)
}
