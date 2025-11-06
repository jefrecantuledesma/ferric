use crate::config::Config;
use crate::logger;
use crate::metadata::AudioMetadata;
use crate::operations::OperationStats;
use crate::quality;
use crate::utils;
use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use walkdir::WalkDir;

pub struct ConvertOptions {
    pub input_dir: PathBuf,
    pub output_dir: PathBuf,
    pub delete_original: bool,
    pub always_convert: bool,
    pub convert_down: bool,
    pub dry_run: bool,
    pub verbose: bool,
    pub config: Config,
}

/// Convert audio files to OPUS format
pub fn run(options: ConvertOptions) -> Result<OperationStats> {
    logger::stage("Starting OPUS conversion");
    logger::info(&format!("Input directory: {}", options.input_dir.display()));
    logger::info(&format!("Output directory: {}", options.output_dir.display()));
    logger::info(&format!(
        "Target: OPUS {}kbps VBR",
        options.config.convert.opus_bitrate
    ));

    if options.dry_run {
        logger::warning("DRY RUN MODE - No conversions will be performed");
    }

    // Check for ffmpeg
    if !options.dry_run {
        check_ffmpeg()?;
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

    logger::info(&format!("Found {} audio files", files.len()));

    let pb = ProgressBar::new(files.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] [{bar:40}] {pos}/{len} ({eta}) {msg}")
            .unwrap()
            .progress_chars("=>-"),
    );

    // Wrap stats in Arc<Mutex<>> for thread-safe parallel access
    let stats = Arc::new(Mutex::new(stats));

    // Process files in parallel using rayon
    files.par_iter().for_each(|file| {
        pb.inc(1);
        {
            let mut stats = stats.lock().unwrap();
            stats.processed += 1;
        }

        // Skip if already OPUS
        if let Some(ext) = utils::get_extension(file) {
            if ext == "opus" {
                logger::debug(&format!("Skipping (already OPUS): {}", file.display()), options.verbose);
                let mut stats = stats.lock().unwrap();
                stats.add_skipped(file.clone(), "already OPUS".to_string());
                return;
            }
        }

        // Calculate output path
        let relative_path = file.strip_prefix(&options.input_dir).unwrap_or(file);
        let output_file = options.output_dir.join(relative_path).with_extension("opus");

        // Check if we should convert based on quality comparison
        if output_file.exists() && !options.always_convert {
            // Output file exists, check quality
            match AudioMetadata::from_file(file) {
                Ok(input_meta) => {
                    match AudioMetadata::from_file(&output_file) {
                        Ok(output_meta) => {
                            let input_quality = quality::calculate_quality_score(&input_meta, &options.config);
                            let output_quality = quality::calculate_quality_score(&output_meta, &options.config);

                            // Calculate target OPUS quality
                            let target_opus_quality = (options.config.convert.opus_bitrate as f64
                                * options.config.quality.codec_multipliers.opus) as u32;

                            if input_quality > output_quality {
                                // Input is better quality, should convert (upgrade)
                                logger::debug(
                                    &format!("Will upgrade: {} (quality {} > {})", file.display(), input_quality, output_quality),
                                    options.verbose,
                                );
                            } else if input_quality < target_opus_quality && !options.convert_down {
                                // Input would be downgrade and convert_down not enabled
                                logger::debug(
                                    &format!("Skipping (would be downgrade, quality {} < target {}): {}",
                                        input_quality, target_opus_quality, file.display()),
                                    options.verbose,
                                );
                                let mut stats = stats.lock().unwrap();
                                stats.add_skipped(
                                    file.clone(),
                                    format!("would be downgrade (quality {} < target {})", input_quality, target_opus_quality)
                                );
                                return;
                            } else if input_quality == output_quality && !options.convert_down {
                                // Same quality
                                logger::debug(&format!("Skipping (same quality {}): {}", output_quality, file.display()), options.verbose);
                                let mut stats = stats.lock().unwrap();
                                stats.add_skipped(file.clone(), format!("same quality ({})", output_quality));
                                return;
                            }
                            // If convert_down is enabled, we fall through and convert
                        }
                        Err(_) => {
                            // Can't read output file metadata, convert anyway
                            logger::debug(&format!("Cannot read output metadata, will convert: {}", file.display()), options.verbose);
                        }
                    }
                }
                Err(_) => {
                    // Can't read input file metadata, convert anyway
                    logger::debug(&format!("Cannot read input metadata, will convert: {}", file.display()), options.verbose);
                }
            }
        }

        pb.set_message(format!("Converting: {}", file.file_name().unwrap().to_string_lossy()));

        if options.dry_run {
            logger::debug(&format!("Would convert: {} -> {}", file.display(), output_file.display()), options.verbose);
            let mut stats = stats.lock().unwrap();
            stats.succeeded += 1;
        } else {
            // Create output directory
            if let Some(parent) = output_file.parent() {
                let _ = std::fs::create_dir_all(parent);
            }

            // Convert using ffmpeg
            match convert_file(file, &output_file, &options.config) {
                Ok(_) => {
                    logger::debug(&format!("Converted: {}", output_file.display()), options.verbose);
                    let mut stats = stats.lock().unwrap();
                    stats.succeeded += 1;

                    // Delete original if requested
                    if options.delete_original {
                        if let Err(e) = std::fs::remove_file(file) {
                            logger::warning(&format!("Failed to delete original {}: {}", file.display(), e));
                        } else {
                            logger::debug(&format!("Deleted original: {}", file.display()), options.verbose);
                        }
                    }
                }
                Err(e) => {
                    logger::error(&format!("Conversion failed for {}: {}", file.display(), e));
                    let mut stats = stats.lock().unwrap();
                    stats.errors += 1;
                }
            }
        }
    });

    pb.finish_and_clear();

    // Extract stats from Arc<Mutex<>>
    let stats = Arc::try_unwrap(stats).unwrap().into_inner().unwrap();
    stats.print_summary("OPUS Conversion");
    Ok(stats)
}

fn check_ffmpeg() -> Result<()> {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .context("ffmpeg not found - please install ffmpeg to use conversion feature")?;
    Ok(())
}

fn convert_file(input: &Path, output: &Path, config: &Config) -> Result<()> {
    let status = Command::new("ffmpeg")
        .arg("-i")
        .arg(input)
        .arg("-c:a")
        .arg("libopus")
        .arg("-b:a")
        .arg(format!("{}k", config.convert.opus_bitrate))
        .arg("-vbr")
        .arg("on")
        .arg("-compression_level")
        .arg(config.convert.opus_compression.to_string())
        .arg("-map_metadata")
        .arg("0")
        .arg("-y") // Overwrite output
        .arg(output)
        .output()
        .context("Failed to execute ffmpeg")?;

    if !status.status.success() {
        anyhow::bail!("ffmpeg failed with status: {}", status.status);
    }

    Ok(())
}
