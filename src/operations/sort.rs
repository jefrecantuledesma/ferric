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
    pub config: Config,
}

struct FileInfo {
    path: PathBuf,
    metadata: AudioMetadata,
    quality: u32,
    dest_path: PathBuf,
}

/// Sort files into Artist/Album folder structure based on metadata
pub fn run(options: SortOptions) -> Result<OperationStats> {
    logger::stage("Sorting files by metadata into Artist/Album structure");
    logger::info(&format!("Input directory: {}", options.input_dir.display()));
    logger::info(&format!("Output directory: {}", options.output_dir.display()));

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
                "Found {} files with same metadata, keeping highest quality: {}",
                files.len(),
                dest_path.display()
            ));

            files.iter().max_by_key(|f| f.quality).unwrap()
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
