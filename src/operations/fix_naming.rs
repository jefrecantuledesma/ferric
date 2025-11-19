use crate::config::Config;
use crate::logger;
use crate::metadata::AudioMetadata;
use crate::operations::OperationStats;
use crate::quality;
use crate::utils;
use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub struct FixNamingOptions {
    pub input_dir: PathBuf,
    pub dry_run: bool,
    pub verbose: bool,
    pub config: Config,
}

/// Fix naming issues: apostrophes, case, whitespace
pub fn run(options: FixNamingOptions) -> Result<OperationStats> {
    logger::stage("Starting name normalization");
    logger::info(&format!("Input directory: {}", options.input_dir.display()));
    logger::info(
        "Fixes: curly apostrophes -> straight, uppercase -> lowercase, whitespace normalization",
    );

    if options.dry_run {
        logger::warning("DRY RUN MODE - No changes will be made");
    }

    let mut stats = OperationStats::new();

    // First pass: rename files
    logger::info("Step 1/2: Fixing file names...");
    fix_files(&options.input_dir, &mut stats, &options)?;

    // Second pass: rename directories (depth-first)
    logger::info("Step 2/2: Fixing directory names...");
    fix_directories(&options.input_dir, &mut stats, &options)?;

    stats.print_summary("Name Normalization");
    Ok(stats)
}

fn fix_files(root: &Path, stats: &mut OperationStats, options: &FixNamingOptions) -> Result<()> {
    let files: Vec<PathBuf> = WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .collect();

    let pb = ProgressBar::new(files.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] [{bar:40}] {pos}/{len} ({eta}) | Fixing file names...")
            .unwrap()
            .progress_chars("█▓▒░"),
    );

    for file in files {
        pb.inc(1);
        stats.processed += 1;

        if let Some(filename) = file.file_name() {
            let filename_str = filename.to_string_lossy().to_string();
            let normalized = utils::normalize_name(&filename_str);

            if filename_str != normalized {
                let parent = file.parent().unwrap_or_else(|| Path::new("."));
                let new_path = parent.join(&normalized);

                if new_path.exists() && new_path != file {
                    logger::warning(&format!(
                        "Conflict: {} already exists, skipping",
                        new_path.display()
                    ));
                    stats.add_skipped(
                        file.clone(),
                        "naming conflict - target already exists".to_string(),
                    );
                    continue;
                }

                if options.dry_run {
                    logger::debug(
                        &format!("Would fix: {} -> {}", filename_str, normalized),
                        options.verbose,
                    );
                    stats.succeeded += 1;
                } else {
                    match fs::rename(&file, &new_path) {
                        Ok(_) => {
                            logger::debug(
                                &format!("Fixed: {} -> {}", filename_str, normalized),
                                options.verbose,
                            );
                            stats.succeeded += 1;
                        }
                        Err(e) => {
                            logger::error(&format!("Failed to rename {}: {}", file.display(), e));
                            stats.errors += 1;
                        }
                    }
                }
            }
        }
    }

    pb.finish_and_clear();
    Ok(())
}

fn fix_directories(
    root: &Path,
    stats: &mut OperationStats,
    options: &FixNamingOptions,
) -> Result<()> {
    let mut dirs: Vec<PathBuf> = WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_dir())
        .map(|e| e.path().to_path_buf())
        .collect();

    // Sort by depth (deepest first)
    dirs.sort_by(|a, b| b.components().count().cmp(&a.components().count()));

    let pb = ProgressBar::new(dirs.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template(
                "[{elapsed_precise}] [{bar:40}] {pos}/{len} ({eta}) | Fixing directory names...",
            )
            .unwrap()
            .progress_chars("█▓▒░"),
    );

    for dir in dirs {
        pb.inc(1);

        if dir == root {
            continue;
        }

        stats.processed += 1;

        if let Some(dirname) = dir.file_name() {
            let dirname_str = dirname.to_string_lossy().to_string();
            let normalized = utils::normalize_name(&dirname_str);

            if dirname_str != normalized {
                let parent = dir.parent().unwrap_or_else(|| Path::new("."));
                let new_path = parent.join(&normalized);

                if new_path.exists() && new_path != dir {
                    // Directory already exists with normalized name - merge contents
                    logger::info(&format!(
                        "Merging directory contents: {} -> {}",
                        dirname_str,
                        normalized
                    ));

                    if let Err(e) = merge_directory_contents(&dir, &new_path, options, stats) {
                        logger::error(&format!(
                            "Failed to merge directory {}: {}",
                            dir.display(),
                            e
                        ));
                        stats.errors += 1;
                    } else {
                        stats.succeeded += 1;
                    }
                    continue;
                }

                if options.dry_run {
                    logger::debug(
                        &format!("Would fix dir: {} -> {}", dirname_str, normalized),
                        options.verbose,
                    );
                    stats.succeeded += 1;
                } else {
                    match fs::rename(&dir, &new_path) {
                        Ok(_) => {
                            logger::debug(
                                &format!("Fixed dir: {} -> {}", dirname_str, normalized),
                                options.verbose,
                            );
                            stats.succeeded += 1;
                        }
                        Err(e) => {
                            logger::error(&format!("Failed to rename {}: {}", dir.display(), e));
                            stats.errors += 1;
                        }
                    }
                }
            }
        }
    }

    pb.finish_and_clear();
    Ok(())
}

/// Check if two audio files represent the same song based on metadata
fn is_same_song(file1: &Path, file2: &Path) -> bool {
    let Ok(m1) = AudioMetadata::from_file(file1) else {
        return false;
    };
    let Ok(m2) = AudioMetadata::from_file(file2) else {
        return false;
    };

    let title1 = utils::normalize_for_comparison(&m1.get_title());
    let title2 = utils::normalize_for_comparison(&m2.get_title());
    let artist1 = utils::normalize_for_comparison(&m1.get_organizing_artist(false));
    let artist2 = utils::normalize_for_comparison(&m2.get_organizing_artist(false));
    let album1 = utils::normalize_for_comparison(&m1.get_album());
    let album2 = utils::normalize_for_comparison(&m2.get_album());

    title1 == title2 && artist1 == artist2 && album1 == album2
}

/// Find a unique filename by appending (1), (2), etc.
fn find_unique_filename(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap();
    let filename = path
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_default();

    let mut counter = 1;
    loop {
        let new_name = if extension.is_empty() {
            format!("{} ({})", filename, counter)
        } else {
            format!("{} ({}).{}", filename, counter, extension)
        };
        let new_path = parent.join(&new_name);
        if !new_path.exists() {
            return new_path;
        }
        counter += 1;
    }
}

/// Merge contents of source directory into target directory, handling file conflicts intelligently
fn merge_directory_contents(
    source_dir: &Path,
    target_dir: &Path,
    options: &FixNamingOptions,
    _stats: &mut OperationStats,
) -> Result<()> {
    // Collect all files in source directory
    let files: Vec<PathBuf> = WalkDir::new(source_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .collect();

    for file in files {
        let relative_path = file.strip_prefix(source_dir)?;
        let target_path = target_dir.join(relative_path);

        // Normalize the filename
        let filename = file.file_name().unwrap().to_string_lossy().to_string();
        let normalized_filename = utils::normalize_name(&filename);
        let final_target_path = target_path
            .parent()
            .unwrap()
            .join(&normalized_filename);

        // Create parent directory if needed
        if let Some(parent) = final_target_path.parent() {
            if !options.dry_run {
                fs::create_dir_all(parent)?;
            }
        }

        if final_target_path.exists() && final_target_path != file {
            // File already exists at target, need to handle conflict
            if utils::is_audio_file(&file) && utils::is_audio_file(&final_target_path) {
                // Both are audio files, check if same song
                if is_same_song(&file, &final_target_path) {
                    // Same song, keep higher quality version
                    if let (Ok(m1), Ok(m2)) = (
                        AudioMetadata::from_file(&file),
                        AudioMetadata::from_file(&final_target_path),
                    ) {
                        let q1 = quality::calculate_quality_score(&m1, &options.config);
                        let q2 = quality::calculate_quality_score(&m2, &options.config);

                        if q1 > q2 {
                            if options.dry_run {
                                logger::debug(
                                    &format!(
                                        "Would replace with higher quality: {} (q={}) > {} (q={})",
                                        file.display(),
                                        q1,
                                        final_target_path.display(),
                                        q2
                                    ),
                                    options.verbose,
                                );
                            } else {
                                fs::remove_file(&final_target_path)?;
                                fs::rename(&file, &final_target_path)?;
                                logger::debug(
                                    &format!(
                                        "Replaced with higher quality: {} -> {}",
                                        file.display(),
                                        final_target_path.display()
                                    ),
                                    options.verbose,
                                );
                            }
                        } else {
                            // Keep existing, remove source
                            if options.dry_run {
                                logger::debug(
                                    &format!(
                                        "Would keep existing higher quality: {} (q={}) >= {} (q={})",
                                        final_target_path.display(),
                                        q2,
                                        file.display(),
                                        q1
                                    ),
                                    options.verbose,
                                );
                            } else {
                                fs::remove_file(&file)?;
                                logger::debug(
                                    &format!(
                                        "Kept existing higher quality, removed: {}",
                                        file.display()
                                    ),
                                    options.verbose,
                                );
                            }
                        }
                    }
                } else {
                    // Different songs, find unique name
                    let unique_path = find_unique_filename(&final_target_path);
                    if options.dry_run {
                        logger::debug(
                            &format!(
                                "Would rename (different song): {} -> {}",
                                file.display(),
                                unique_path.display()
                            ),
                            options.verbose,
                        );
                    } else {
                        fs::rename(&file, &unique_path)?;
                        logger::debug(
                            &format!(
                                "Renamed (different song): {} -> {}",
                                file.display(),
                                unique_path.display()
                            ),
                            options.verbose,
                        );
                    }
                }
            } else {
                // Not audio files, or only one is, find unique name
                let unique_path = find_unique_filename(&final_target_path);
                if options.dry_run {
                    logger::debug(
                        &format!(
                            "Would rename (conflict): {} -> {}",
                            file.display(),
                            unique_path.display()
                        ),
                        options.verbose,
                    );
                } else {
                    fs::rename(&file, &unique_path)?;
                    logger::debug(
                        &format!(
                            "Renamed (conflict): {} -> {}",
                            file.display(),
                            unique_path.display()
                        ),
                        options.verbose,
                    );
                }
            }
        } else {
            // No conflict, just move
            if options.dry_run {
                logger::debug(
                    &format!(
                        "Would move: {} -> {}",
                        file.display(),
                        final_target_path.display()
                    ),
                    options.verbose,
                );
            } else {
                fs::rename(&file, &final_target_path)?;
                logger::debug(
                    &format!(
                        "Moved: {} -> {}",
                        file.display(),
                        final_target_path.display()
                    ),
                    options.verbose,
                );
            }
        }
    }

    // Remove empty source directory and its subdirectories
    if !options.dry_run {
        // Remove empty subdirectories first (depth-first)
        let mut subdirs: Vec<PathBuf> = WalkDir::new(source_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_dir())
            .map(|e| e.path().to_path_buf())
            .collect();
        subdirs.sort_by(|a, b| b.components().count().cmp(&a.components().count()));

        for subdir in subdirs {
            if subdir.read_dir()?.next().is_none() {
                let _ = fs::remove_dir(&subdir);
            }
        }
    }

    Ok(())
}
