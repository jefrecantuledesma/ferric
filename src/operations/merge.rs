use crate::config::Config;
use crate::logger;
use crate::metadata::AudioMetadata;
use crate::operations::OperationStats;
use crate::quality;
use crate::utils;
use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
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

/// Merge one library into another, preserving directory structure and upgrading based on quality
/// Files are matched by their relative path (not metadata), and only upgraded if higher quality
pub fn run(options: MergeOptions) -> Result<OperationStats> {
    logger::stage("Starting library merge");
    logger::info(&format!("Source library: {}", options.input_dir.display()));
    logger::info(&format!("Target library: {}", options.output_dir.display()));
    logger::info("Preserving source directory structure");
    logger::info("Will only replace files at the same path with higher quality versions");

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
        relative_path: PathBuf,
    }

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
                    return None;
                }
            };

            let quality = quality::calculate_quality_score(&metadata, &options.config);

            // Preserve source directory structure - use relative path from input
            let relative_path = match file.strip_prefix(&options.input_dir) {
                Ok(rel) => rel.to_path_buf(),
                Err(e) => {
                    logger::error(&format!(
                        "Failed to get relative path for {}: {}",
                        file.display(),
                        e
                    ));
                    return None;
                }
            };

            let dest_path = options.output_dir.join(&relative_path);
            let dest_dir = dest_path.parent().unwrap_or(&options.output_dir).to_path_buf();

            Some(FileInfo {
                path: file.clone(),
                metadata,
                quality,
                dest_dir,
                dest_path,
                relative_path,
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

    // Canonicalize paths to properly exclude input directory from indexing
    let input_canonical = options.input_dir.canonicalize().unwrap_or_else(|_| options.input_dir.clone());

    if options.output_dir.exists() {
        let existing_files: Vec<PathBuf> = WalkDir::new(&options.output_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .map(|e| e.path().to_path_buf())
            .filter(|p| {
                // Exclude files from the input directory to avoid matching files against themselves
                if let Ok(canonical) = p.canonicalize() {
                    !canonical.starts_with(&input_canonical)
                } else {
                    true
                }
            })
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

            // Use relative path as the key to match files by location, not metadata
            if let Ok(relative_path) = file.strip_prefix(&options.output_dir) {
                if let Ok(metadata) = AudioMetadata::from_file(file) {
                    let quality_score = quality::calculate_quality_score(&metadata, &options.config);

                    // Key is the relative path as a string
                    let key = relative_path.to_string_lossy().to_string();

                    let mut index = existing_files_index.lock().unwrap();
                    index.insert(key, (file.clone(), quality_score));
                }
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

    // Track source directories for cleanup when using --move
    let moved_from_dirs: Arc<Mutex<HashSet<PathBuf>>> = Arc::new(Mutex::new(HashSet::new()));

    for file_info in &file_infos {
        pb2.inc(1);
        stats.processed += 1;

        let new_quality = file_info.quality;
        let dest_dir = &file_info.dest_dir;
        let dest_path = &file_info.dest_path;

        // O(1) lookup by relative path - matches files by location, not metadata
        let lookup_key = file_info.relative_path.to_string_lossy().to_string();

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
                        // Track source directory for cleanup if we moved the file
                        if options.do_move && action_type != "upgrade" {
                            if let Some(parent) = file_info.path.parent() {
                                moved_from_dirs.lock().unwrap().insert(parent.to_path_buf());
                            }
                        }

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

    // Phase 3: Clean up empty directories if we moved files
    if options.do_move && !options.dry_run {
        let dirs_to_check = moved_from_dirs.lock().unwrap().clone();
        if !dirs_to_check.is_empty() {
            logger::info(&format!(
                "Cleaning up empty directories ({} directories to check)...",
                dirs_to_check.len()
            ));
            let removed = cleanup_empty_directories(&options.input_dir, dirs_to_check, options.verbose);
            if removed > 0 {
                logger::success(&format!("Removed {} empty directories", removed));
            }
        }
    }

    logger::success(&format!(
        "Merge complete: {} files added, {} files upgraded",
        added_count, replaced_count
    ));
    stats.print_summary("Library Merge");
    Ok(stats)
}

/// Remove empty directories recursively, working from deepest to shallowest
/// Returns the number of directories removed
fn cleanup_empty_directories(
    root_dir: &PathBuf,
    directories: HashSet<PathBuf>,
    verbose: bool,
) -> usize {
    let mut removed_count = 0;

    // Sort directories by depth (deepest first) to ensure we clean from bottom up
    let mut sorted_dirs: Vec<PathBuf> = directories.into_iter().collect();
    sorted_dirs.sort_by(|a, b| {
        let depth_a = a.components().count();
        let depth_b = b.components().count();
        depth_b.cmp(&depth_a) // Reverse order (deepest first)
    });

    // Clean up each directory using the shared utility function
    // The utility handles recursion up to the root automatically
    for dir in sorted_dirs {
        removed_count += utils::cleanup_empty_directory(&dir, root_dir, verbose);
    }

    removed_count
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_cleanup_empty_directories() {
        // Create temporary test directory
        let temp_root = TempDir::new().unwrap();
        let root_path = temp_root.path().to_path_buf();

        // Create nested directory structure
        let dir1 = root_path.join("artist1/album1");
        let dir2 = root_path.join("artist1/album2");
        let dir3 = root_path.join("artist2/album3");
        fs::create_dir_all(&dir1).unwrap();
        fs::create_dir_all(&dir2).unwrap();
        fs::create_dir_all(&dir3).unwrap();

        // Track directories for cleanup
        let mut dirs_to_check = HashSet::new();
        dirs_to_check.insert(dir1.clone());
        dirs_to_check.insert(dir2.clone());
        dirs_to_check.insert(dir3.clone());

        // Run cleanup
        let removed = cleanup_empty_directories(&root_path, dirs_to_check, false);

        // All empty directories should be removed, including parents
        assert_eq!(removed, 5); // album1, album2, album3, artist1, artist2
        assert!(!dir1.exists());
        assert!(!dir2.exists());
        assert!(!dir3.exists());
        assert!(!root_path.join("artist1").exists());
        assert!(!root_path.join("artist2").exists());
    }

    #[test]
    fn test_cleanup_removes_non_audio_files() {
        // Create temporary test directory
        let temp_root = TempDir::new().unwrap();
        let root_path = temp_root.path().to_path_buf();

        // Create directory with non-audio file
        let dir1 = root_path.join("artist1/album1");
        fs::create_dir_all(&dir1).unwrap();
        let cover_file = dir1.join("cover.png");
        fs::write(&cover_file, "fake album art").unwrap();

        // Track directory for cleanup
        let mut dirs_to_check = HashSet::new();
        dirs_to_check.insert(dir1.clone());

        // Run cleanup
        let removed = cleanup_empty_directories(&root_path, dirs_to_check, false);

        // Directory and non-audio file should be removed
        assert!(removed >= 1);
        assert!(!cover_file.exists());
        assert!(!dir1.exists());
    }

    #[test]
    fn test_cleanup_preserves_directories_with_audio() {
        // Create temporary test directory
        let temp_root = TempDir::new().unwrap();
        let root_path = temp_root.path().to_path_buf();

        // Create directory with audio file
        let dir1 = root_path.join("artist1/album1");
        fs::create_dir_all(&dir1).unwrap();
        let audio_file = dir1.join("track.flac");
        fs::write(&audio_file, "fake audio data").unwrap();

        // Track directory for cleanup
        let mut dirs_to_check = HashSet::new();
        dirs_to_check.insert(dir1.clone());

        // Run cleanup
        let removed = cleanup_empty_directories(&root_path, dirs_to_check, false);

        // Directory with audio file should NOT be removed
        assert_eq!(removed, 0);
        assert!(dir1.exists());
        assert!(audio_file.exists());
    }

    #[test]
    fn test_cleanup_stops_at_root() {
        // Create temporary test directory
        let temp_root = TempDir::new().unwrap();
        let root_path = temp_root.path().to_path_buf();

        // Create empty directory
        let dir1 = root_path.join("empty_dir");
        fs::create_dir_all(&dir1).unwrap();

        // Track directory for cleanup
        let mut dirs_to_check = HashSet::new();
        dirs_to_check.insert(dir1.clone());

        // Run cleanup
        cleanup_empty_directories(&root_path, dirs_to_check, false);

        // Root should still exist (not removed)
        assert!(root_path.exists());
        // Empty subdirectory should be removed
        assert!(!dir1.exists());
    }
}
