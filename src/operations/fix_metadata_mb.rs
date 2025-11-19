use crate::{config::Config, fingerprint, logger, metadata::AudioMetadata, musicbrainz, utils};
use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use walkdir::WalkDir;

pub struct FixMetadataOptions {
    pub input_dirs: Vec<PathBuf>,
    pub dry_run: bool,
    pub verbose: bool,

    // What fields to fix
    pub fix_artist: bool,
    pub fix_album: bool,
    pub fix_album_artist: bool,
    pub fix_title: bool,
    pub fix_date: bool,
    pub fix_genre: bool,
    pub fix_all: bool, // Fix everything

    // MusicBrainz options
    pub use_musicbrainz: bool,
    pub confidence_threshold: f32,
    pub interactive: bool,      // Always prompt even for high confidence
    pub auto_apply: bool,        // Auto-apply high confidence matches
    pub skip_fingerprinting: bool,
    pub overwrite: bool,         // Replace existing metadata (default: false, additive-only)

    // Prevent Various Artists issues
    pub avoid_various_artists: bool,
}

#[derive(Debug, Clone)]
struct FileInfo {
    path: PathBuf,
    metadata: AudioMetadata,
    fingerprint: Option<String>,
    parent_dir: PathBuf,
}

#[derive(Debug, Clone)]
struct MatchResult {
    file: FileInfo,
    acoustid_results: Vec<musicbrainz::AcoustIdResult>,
    selected_metadata: Option<musicbrainz::MusicBrainzMetadata>,
}

/// Represents which metadata fields should be updated for a file
#[derive(Debug, Clone, Default)]
pub struct FieldsToUpdate {
    pub update_artist: bool,
    pub update_album: bool,
    pub update_album_artist: bool,
    pub update_title: bool,
    pub update_date: bool,
    pub update_genre: bool,
}

impl FieldsToUpdate {
    /// Determine which fields should be updated based on options and current metadata
    fn from_metadata(
        current: &AudioMetadata,
        options: &FixMetadataOptions,
    ) -> Self {
        let mut fields = Self::default();

        // Only update fields that:
        // 1. The user requested (via --artist, --album, etc.)
        // 2. Are missing OR overwrite is enabled

        if options.fix_artist {
            fields.update_artist = options.overwrite || current.artist.is_none();
        }
        if options.fix_album {
            fields.update_album = options.overwrite || current.album.is_none();
        }
        if options.fix_album_artist {
            fields.update_album_artist = options.overwrite || current.album_artist.is_none();
        }
        if options.fix_title {
            // Consider "_unknown title" as missing
            fields.update_title = options.overwrite
                || current.title.as_deref() == Some("_unknown title")
                || current.title.is_none();
        }
        if options.fix_date {
            fields.update_date = options.overwrite || current.date.is_none();
        }
        if options.fix_genre {
            fields.update_genre = options.overwrite || current.genre.is_none();
        }

        fields
    }

    /// Check if any fields need updating
    fn has_updates(&self) -> bool {
        self.update_artist || self.update_album || self.update_album_artist
            || self.update_title || self.update_date || self.update_genre
    }
}

/// Main entry point for MusicBrainz-powered metadata fixing
pub async fn run(options: FixMetadataOptions, config: &Config) -> Result<()> {
    logger::stage("MusicBrainz Metadata Fix");

    // Validate configuration
    if options.use_musicbrainz {
        match musicbrainz::get_acoustid_api_key(config) {
            Ok(key) => {
                logger::info(&format!(
                    "Using AcoustID API key: {}***",
                    &key[..key.len().min(8)]
                ));
            }
            Err(e) => {
                logger::error(&format!("AcoustID API key error: {}", e));
                logger::info("Set ACOUSTID_API_KEY environment variable or add to config");
                return Err(e);
            }
        }
    }

    // Step 1: Scan for audio files
    logger::info(&format!("Scanning {} directories...", options.input_dirs.len()));
    let files = scan_audio_files(&options)?;

    if files.is_empty() {
        logger::warning("No audio files found");
        return Ok(());
    }

    logger::success(&format!("Found {} audio files", files.len()));

    // Step 2: Filter files that need fixing
    let files_to_fix = filter_files_needing_fix(&files, &options)?;

    if files_to_fix.is_empty() {
        logger::success("All files already have complete metadata!");
        return Ok(());
    }

    logger::info(&format!("{} files need metadata fixes", files_to_fix.len()));

    // Step 3: Generate fingerprints if using MusicBrainz
    let mut file_infos: Vec<FileInfo> = files_to_fix
        .into_iter()
        .map(|(path, metadata)| FileInfo {
            parent_dir: path.parent().unwrap_or(Path::new(".")).to_path_buf(),
            path,
            metadata,
            fingerprint: None,
        })
        .collect();

    if options.use_musicbrainz && !options.skip_fingerprinting {
        logger::stage("Generating Audio Fingerprints");
        file_infos = generate_fingerprints_for_files(file_infos, &options)?;
    }

    // Step 4: Look up via MusicBrainz if enabled
    let matches = if options.use_musicbrainz {
        lookup_musicbrainz_batch(file_infos, config, &options).await?
    } else {
        vec![]
    };

    // Step 5: Interactive review and application
    if options.use_musicbrainz {
        apply_musicbrainz_matches(matches, &options).await?;
    } else {
        // Fall back to manual entry mode (legacy)
        logger::warning("Manual metadata entry not implemented in MusicBrainz mode");
        logger::info("Use --use-musicbrainz flag to enable automatic lookup");
    }

    logger::success("Metadata fixing complete!");
    Ok(())
}

/// Scan directories for audio files and extract metadata
fn scan_audio_files(options: &FixMetadataOptions) -> Result<Vec<(PathBuf, AudioMetadata)>> {
    let mut all_files = Vec::new();

    for dir in &options.input_dirs {
        if !dir.exists() {
            logger::warning(&format!("Directory does not exist: {}", dir.display()));
            continue;
        }

        let files: Vec<PathBuf> = WalkDir::new(dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .map(|e| e.path().to_path_buf())
            .filter(|p| utils::is_audio_file(p))
            .collect();

        all_files.extend(files);
    }

    // Extract metadata in parallel
    logger::info("Extracting metadata...");
    let pb = ProgressBar::new(all_files.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("[{bar:40}] {pos}/{len} | {msg}")
            .unwrap()
            .progress_chars("█▓▒░"),
    );

    let results: Vec<_> = all_files
        .par_iter()
        .map(|path| {
            pb.inc(1);
            let metadata = AudioMetadata::from_file(path).ok()?;
            Some((path.clone(), metadata))
        })
        .collect();

    pb.finish_and_clear();

    Ok(results.into_iter().flatten().collect())
}

/// Filter files that actually need metadata fixes based on options
fn filter_files_needing_fix(
    files: &[(PathBuf, AudioMetadata)],
    options: &FixMetadataOptions,
) -> Result<Vec<(PathBuf, AudioMetadata)>> {
    let filtered: Vec<_> = files
        .iter()
        .filter(|(_, meta)| {
            if options.fix_all {
                return true;
            }

            let mut needs_fix = false;

            if options.fix_artist && meta.artist.is_none() {
                needs_fix = true;
            }
            if options.fix_album && meta.album.is_none() {
                needs_fix = true;
            }
            if options.fix_album_artist && meta.album_artist.is_none() {
                needs_fix = true;
            }
            if options.fix_title && meta.title.as_deref() == Some("_unknown title") {
                needs_fix = true;
            }
            if options.fix_date && meta.date.is_none() {
                needs_fix = true;
            }
            if options.fix_genre && meta.genre.is_none() {
                needs_fix = true;
            }

            needs_fix
        })
        .cloned()
        .collect();

    Ok(filtered)
}

/// Generate fingerprints for files in parallel
fn generate_fingerprints_for_files(
    mut files: Vec<FileInfo>,
    options: &FixMetadataOptions,
) -> Result<Vec<FileInfo>> {
    let pb = ProgressBar::new(files.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("[{bar:40}] {pos}/{len} | Fingerprinting...")
            .unwrap()
            .progress_chars("█▓▒░"),
    );

    let files_with_fps: Vec<FileInfo> = files
        .par_iter_mut()
        .map(|file_info| {
            pb.inc(1);

            // Check if already has fingerprint in cache
            if file_info.metadata.fingerprint.is_some() {
                file_info.fingerprint = file_info.metadata.fingerprint.clone();
                logger::debug(
                    &format!("Using cached fingerprint for {}", file_info.path.display()),
                    options.verbose,
                );
                return file_info.clone();
            }

            // Generate new fingerprint
            match fingerprint::generate_fingerprint(&file_info.path) {
                Ok(fp) => {
                    file_info.fingerprint = Some(fp.clone());

                    // Store fingerprint in cache for future use
                    file_info.metadata.fingerprint = Some(fp);
                    if let Some(cache) = crate::cache::get_global_cache() {
                        let _ = cache.insert(&file_info.path, &file_info.metadata);
                    }

                    file_info.clone()
                }
                Err(e) => {
                    logger::warning(&format!(
                        "Failed to fingerprint {}: {}",
                        file_info.path.display(),
                        e
                    ));
                    file_info.clone()
                }
            }
        })
        .collect();

    pb.finish_and_clear();
    Ok(files_with_fps)
}

/// Look up files via MusicBrainz in batch
async fn lookup_musicbrainz_batch(
    files: Vec<FileInfo>,
    config: &Config,
    _options: &FixMetadataOptions,
) -> Result<Vec<MatchResult>> {
    logger::stage("Looking up via AcoustID + MusicBrainz");

    let api_key = musicbrainz::get_acoustid_api_key(config)?;
    let rate_limiter = musicbrainz::RateLimiter::new(1.0); // 1 request/sec

    let results = Arc::new(Mutex::new(Vec::new()));
    let total = files.len();
    let processed = Arc::new(Mutex::new(0));

    let pb = ProgressBar::new(total as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("[{bar:40}] {pos}/{len} | Querying MusicBrainz...")
            .unwrap()
            .progress_chars("█▓▒░"),
    );

    for file in files {
        let api_key = api_key.clone();
        let results = Arc::clone(&results);
        let processed = Arc::clone(&processed);

        // Wait for rate limit
        rate_limiter.wait().await;

        let acoustid_results = if let Some(ref fp) = file.fingerprint {
            let duration = file.metadata.duration_secs.unwrap_or(0.0);
            match musicbrainz::lookup_by_fingerprint(fp, duration, &api_key).await {
                Ok(r) => r,
                Err(e) => {
                    logger::warning(&format!(
                        "AcoustID lookup failed for {}: {}",
                        file.path.display(),
                        e
                    ));
                    vec![]
                }
            }
        } else {
            vec![]
        };

        results.lock().unwrap().push(MatchResult {
            file,
            acoustid_results,
            selected_metadata: None,
        });

        *processed.lock().unwrap() += 1;
        pb.set_position(*processed.lock().unwrap() as u64);
    }

    pb.finish_and_clear();

    let final_results = results.lock().unwrap().clone();
    let matched = final_results
        .iter()
        .filter(|r| !r.acoustid_results.is_empty())
        .count();

    logger::info(&format!(
        "Found matches for {} / {} files ({:.1}%)",
        matched,
        total,
        (matched as f32 / total as f32) * 100.0
    ));

    Ok(final_results)
}

/// Apply MusicBrainz matches interactively
async fn apply_musicbrainz_matches(
    mut matches: Vec<MatchResult>,
    options: &FixMetadataOptions,
) -> Result<()> {
    logger::stage("Applying Metadata");

    let user_agent = format!("Ferric/{}", env!("CARGO_PKG_VERSION"));
    let rate_limiter = musicbrainz::RateLimiter::new(1.0);

    for match_result in &mut matches {
        if match_result.acoustid_results.is_empty() {
            logger::warning(&format!(
                "No matches found for: {}",
                match_result.file.path.display()
            ));
            println!("  Current: {} - {}",
                match_result.file.metadata.artist.as_deref().unwrap_or("Unknown"),
                match_result.file.metadata.title.as_deref().unwrap_or("Unknown")
            );
            println!();
            continue;
        }

        // Get top match
        let top_match = &match_result.acoustid_results[0];

        // Check confidence
        if !options.interactive && top_match.score < options.confidence_threshold {
            logger::warning(&format!(
                "Low confidence ({:.1}%) for {}, skipping",
                top_match.score * 100.0,
                match_result.file.path.display()
            ));
            continue;
        }

        // Fetch full metadata from MusicBrainz
        rate_limiter.wait().await;
        let mb_metadata =
            match musicbrainz::fetch_recording_metadata(&top_match.recording_id, &user_agent)
                .await
            {
                Ok(m) => m,
                Err(e) => {
                    logger::error(&format!("Failed to fetch metadata: {}", e));
                    continue;
                }
            };

        // Check for "Various Artists" and skip if user wants to avoid
        if options.avoid_various_artists {
            let is_va = mb_metadata
                .album_artist
                .as_ref()
                .or(mb_metadata.album.as_ref())
                .map(|s| {
                    let lower = s.to_lowercase();
                    lower.contains("various") || lower.contains("compilation")
                })
                .unwrap_or(false);

            if is_va {
                logger::warning(&format!(
                    "Skipping '{}' - detected as Various Artists compilation",
                    match_result.file.path.display()
                ));
                println!("  Use --no-avoid-various-artists to include these\n");
                continue;
            }
        }

        // Calculate which fields will be updated
        let fields_to_update = FieldsToUpdate::from_metadata(&match_result.file.metadata, options);

        // Skip if no fields need updating
        if !fields_to_update.has_updates() {
            logger::info(&format!(
                "Skipping {} - all requested fields already present",
                match_result.file.path.display()
            ));
            continue;
        }

        // Display match info
        println!("\n{}", "=".repeat(80));
        println!("File: {}", match_result.file.path.display());
        println!("{}", "-".repeat(80));

        println!("\nCurrent metadata:");
        println!("  Artist: {}", match_result.file.metadata.artist.as_deref().unwrap_or("(none)"));
        println!("  Album:  {}", match_result.file.metadata.album.as_deref().unwrap_or("(none)"));
        println!("  Title:  {}", match_result.file.metadata.title.as_deref().unwrap_or("(none)"));
        if let Some(ref date) = match_result.file.metadata.date {
            println!("  Date:   {}", date);
        }
        if let Some(ref genre) = match_result.file.metadata.genre {
            println!("  Genre:  {}", genre);
        }

        println!("\nMusicBrainz match ({:.1}% confidence):", top_match.score * 100.0);

        // Show what will be added/updated
        println!("\nFields to {}:", if options.overwrite { "update" } else { "add" });
        if fields_to_update.update_artist {
            println!("  Artist: {} -> {}",
                match_result.file.metadata.artist.as_deref().unwrap_or("(none)"),
                mb_metadata.artist);
        }
        if fields_to_update.update_album {
            if let Some(ref album) = mb_metadata.album {
                println!("  Album:  {} -> {}",
                    match_result.file.metadata.album.as_deref().unwrap_or("(none)"),
                    album);
            }
        }
        if fields_to_update.update_title {
            println!("  Title:  {} -> {}",
                match_result.file.metadata.title.as_deref().unwrap_or("(none)"),
                mb_metadata.title);
        }
        if fields_to_update.update_date {
            if let Some(ref date) = mb_metadata.date {
                println!("  Date:   {} -> {}",
                    match_result.file.metadata.date.as_deref().unwrap_or("(none)"),
                    date);
            }
        }
        if fields_to_update.update_genre && !mb_metadata.genres.is_empty() {
            println!("  Genre:  {} -> {}",
                match_result.file.metadata.genre.as_deref().unwrap_or("(none)"),
                mb_metadata.genres[0]);
        }

        // Ask user if they want to apply
        let should_apply = if options.interactive || top_match.score < 0.9 {
            print!("\nApply this metadata? [Y/n/s(kip all)]: ");
            io::stdout().flush()?;

            let mut input = String::new();
            io::stdin().read_line(&mut input)?;
            let choice = input.trim().to_lowercase();

            if choice == "s" {
                logger::info("Skipping remaining files");
                break;
            }

            choice.is_empty() || choice == "y" || choice == "yes"
        } else if options.auto_apply {
            println!("\n✓ Auto-applying (high confidence)");
            true
        } else {
            false
        };

        if should_apply {
            match musicbrainz::apply_metadata_to_file(
                &match_result.file.path,
                &match_result.file.metadata,
                &mb_metadata,
                &fields_to_update,
                options.dry_run,
            ) {
                Ok(_) => {
                    if options.dry_run {
                        logger::info("  [DRY RUN] Would apply metadata");
                    } else {
                        logger::success("  ✓ Metadata applied");
                    }
                }
                Err(e) => {
                    logger::error(&format!("  Failed to apply: {}", e));
                }
            }
        } else {
            logger::info("  Skipped");
        }
    }

    Ok(())
}
