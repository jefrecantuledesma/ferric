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

pub struct DedupeOptions {
    pub input_dir: PathBuf,
    pub dry_run: bool,
    pub verbose: bool,
    pub auto_remove: bool,
    pub config: Config,
}

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
struct TrackSignature {
    artist: String,
    album: String,
    title: String,
}

/// Find and remove duplicate audio files based on metadata
pub fn run(options: DedupeOptions) -> Result<OperationStats> {
    logger::stage("Starting metadata-based deduplication");
    logger::info(&format!(
        "Scanning directory: {}",
        options.input_dir.display()
    ));
    logger::info("Comparing: artist, album, and title metadata");

    if options.dry_run {
        logger::warning("DRY RUN MODE - No files will be deleted");
    }

    let stats = OperationStats::new();

    // Collect all audio files
    let files: Vec<PathBuf> = WalkDir::new(&options.input_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .filter(|p| utils::is_audio_file(p))
        .collect();

    logger::info(&format!("Found {} audio files to analyze", files.len()));

    // Build signature map (parallelized for performance)
    let signature_map: Arc<Mutex<HashMap<TrackSignature, Vec<(PathBuf, AudioMetadata, u32)>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let stats_mutex = Arc::new(Mutex::new(stats));

    // Create progress bar for metadata extraction
    let pb = ProgressBar::new(files.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] [{bar:40}] {pos}/{len} ({eta}) | Analyzing metadata...")
            .unwrap()
            .progress_chars("█▓▒░"),
    );

    // Process files in parallel
    files.par_iter().for_each(|file| {
        pb.inc(1);
        {
            let mut stats = stats_mutex.lock().unwrap();
            stats.processed += 1;
        }

        match AudioMetadata::from_file(file) {
            Ok(metadata) => {
                let signature = TrackSignature {
                    artist: utils::normalize_for_comparison(&metadata.get_organizing_artist(false)),
                    album: utils::normalize_for_comparison(&metadata.get_album()),
                    title: utils::normalize_for_comparison(&metadata.get_title()),
                };

                let quality_score = quality::calculate_quality_score(&metadata, &options.config);

                let mut map = signature_map.lock().unwrap();
                map.entry(signature).or_insert_with(Vec::new).push((
                    file.clone(),
                    metadata,
                    quality_score,
                ));
            }
            Err(e) => {
                logger::error(&format!(
                    "Failed to read metadata from {}: {}",
                    file.display(),
                    e
                ));
                let mut stats = stats_mutex.lock().unwrap();
                stats.errors += 1;
            }
        }
    });

    pb.finish_and_clear();

    // Extract stats and map from Arc<Mutex<>>
    let mut stats = Arc::try_unwrap(stats_mutex).unwrap().into_inner().unwrap();
    let signature_map = Arc::try_unwrap(signature_map)
        .unwrap()
        .into_inner()
        .unwrap();

    // Find duplicates
    let mut duplicate_groups = 0;
    let mut files_to_remove = Vec::new();

    for (signature, files) in signature_map.iter() {
        if files.len() > 1 {
            duplicate_groups += 1;

            logger::warning(&format!(
                "\nFound {} duplicate(s) of: {} - {} - {}",
                files.len(),
                signature.artist,
                signature.album,
                signature.title
            ));

            // Sort by quality (highest first)
            let mut sorted_files = files.clone();
            sorted_files.sort_by(|a, b| b.2.cmp(&a.2));

            // Keep the highest quality, mark others for removal
            for (idx, (path, metadata, quality)) in sorted_files.iter().enumerate() {
                if idx == 0 {
                    logger::success(&format!(
                        "  [KEEP] {} (quality: {}, codec: {})",
                        path.display(),
                        quality,
                        metadata.codec
                    ));
                } else {
                    logger::info(&format!(
                        "  [REMOVE] {} (quality: {}, codec: {})",
                        path.display(),
                        quality,
                        metadata.codec
                    ));
                    files_to_remove.push(path.clone());
                }
            }
        }
    }

    logger::info(&format!("\nFound {} duplicate groups", duplicate_groups));
    logger::info(&format!(
        "Files marked for removal: {}",
        files_to_remove.len()
    ));

    if !files_to_remove.is_empty() && !options.auto_remove && !options.dry_run {
        logger::warning("\nProceed with deletion? [y/N]: ");
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            logger::info("Deletion cancelled by user");
            return Ok(stats);
        }
    }

    // Remove files
    for file in files_to_remove {
        if options.dry_run {
            logger::debug(
                &format!("Would delete: {}", file.display()),
                options.verbose,
            );
            stats.succeeded += 1;
        } else {
            match fs::remove_file(&file) {
                Ok(_) => {
                    logger::debug(&format!("Deleted: {}", file.display()), options.verbose);
                    stats.succeeded += 1;
                }
                Err(e) => {
                    logger::error(&format!("Failed to delete {}: {}", file.display(), e));
                    stats.errors += 1;
                }
            }
        }
    }

    stats.print_summary("Deduplication");
    Ok(stats)
}
