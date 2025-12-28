use std::path::{Path, PathBuf};
use std::fs;

/// Sanitize a string for use as a filename/folder name
/// - Replaces forward slashes with en-dash
/// - Replaces backslashes with hyphen
/// - Removes control characters
/// - Trims whitespace
/// - Collapses multiple spaces
/// - Returns "_Unknown" if empty
pub fn sanitize(s: &str) -> String {
    let mut result = s.to_string();

    result = result.replace('/', "–").replace('\\', "-");

    // Remove control characters (0x00-0x1F)
    result = result.chars().filter(|c| *c as u32 >= 0x20).collect();

    result = result.trim().to_string();

    // Collapse multiple spaces (single pass - O(n) instead of O(n²))
    let mut prev_was_space = false;
    result = result
        .chars()
        .filter(|&c| {
            let is_space = c == ' ';
            let keep = !(is_space && prev_was_space);
            prev_was_space = is_space;
            keep
        })
        .collect();

    // Some filesystems don't allow directories ending with periods
    result = result.trim_end_matches('.').trim_end().to_string();

    if result.is_empty() {
        "_unknown".to_string()
    } else {
        result
    }
}

/// Clamp a filename component to a maximum length (default 128 chars)
pub fn clamp_component(s: &str, max_len: usize) -> String {
    if s.len() > max_len {
        s[..max_len].to_string()
    } else {
        s.to_string()
    }
}

/// Normalize text for comparison (lowercase, alphanumeric only, single spaces)
/// Enhanced to handle apostrophes, accents, and common special characters
pub fn normalize_for_comparison(s: &str) -> String {
    let mut result = s.to_lowercase();

    // Replace curly apostrophes with straight ones first
    result = result.replace('\u{2019}', "'").replace('\u{2018}', "'");

    // Common replacements to improve matching
    result = result.replace("&", "and");

    // Remove common apostrophe patterns (but keep the letter before)
    result = result.replace("'s ", " "); // possessive
    result = result.replace("'t ", "t "); // can't → cant
    result = result.replace("'re ", "re "); // you're → youre
    result = result.replace("'ve ", "ve "); // I've → ive
    result = result.replace("'ll ", "ll "); // I'll → ill
    result = result.replace("'d ", "d "); // I'd → id
    result = result.replace("'m ", "m "); // I'm → im

    // Handle remaining apostrophes at word boundaries
    result = result
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c.is_whitespace() {
                c
            } else {
                ' '
            }
        })
        .collect::<String>();

    // Collapse whitespace and trim
    result.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Generate unique path by appending (n) if file exists
pub fn unique_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let filename = path.file_name().unwrap().to_string_lossy();

    let (base, ext) = if let Some(dot_pos) = filename.rfind('.') {
        let (name, extension) = filename.split_at(dot_pos);
        (name.to_string(), extension.to_string())
    } else {
        (filename.to_string(), String::new())
    };

    for n in 1..10000 {
        let candidate = if ext.is_empty() {
            parent.join(format!("{} ({})", base, n))
        } else {
            parent.join(format!("{} ({}){}", base, n, ext))
        };

        if !candidate.exists() {
            return candidate;
        }
    }

    // Fallback: append timestamp
    let timestamp = chrono::Local::now().format("%Y%m%d%H%M%S");
    if ext.is_empty() {
        parent.join(format!("{}_{}", base, timestamp))
    } else {
        parent.join(format!("{}_{}{}", base, timestamp, ext))
    }
}

/// Check if a file is an audio file by extension
pub fn is_audio_file(path: &Path) -> bool {
    if let Some(ext) = path.extension() {
        let ext_lower = ext.to_string_lossy().to_lowercase();
        matches!(
            ext_lower.as_str(),
            "flac"
                | "opus"
                | "ogg"
                | "mp3"
                | "m4a"
                | "aac"
                | "wav"
                | "aiff"
                | "aif"
                | "wma"
                | "alac"
        )
    } else {
        false
    }
}

/// Get file extension in lowercase
pub fn get_extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|s| s.to_lowercase())
}

/// Normalize name: fix curly apostrophes, optionally lowercase, normalize whitespace
pub fn normalize_name(name: &str, lowercase: bool) -> String {
    let mut result = name.to_string();

    // Replace Unicode right single quotation mark (') with ASCII apostrophe (')
    result = result.replace('\u{2019}', "'");

    if lowercase {
        result = result.to_lowercase();
    }

    // Single-pass whitespace collapse (O(n) instead of O(n²))
    let mut prev_was_space = false;
    result = result
        .chars()
        .filter(|&c| {
            let is_space = c == ' ';
            let keep = !(is_space && prev_was_space);
            prev_was_space = is_space;
            keep
        })
        .collect();

    result.trim().to_string()
}

/// Recursively remove empty directories and directories containing only non-audio files
///
/// This function cleans up a directory after files have been moved out of it.
/// It will:
/// 1. Remove any leftover non-audio files (like .png, .jpg, .txt)
/// 2. Remove the directory if it's empty or only had non-audio files
/// 3. Recursively check parent directories up to (but not including) the root
///
/// # Arguments
/// * `dir_path` - The directory to check and potentially remove
/// * `root_dir` - The root directory to stop at (will not be removed)
/// * `verbose` - Whether to log debug messages
///
/// # Returns
/// The number of directories removed
pub fn cleanup_empty_directory(dir_path: &Path, root_dir: &Path, verbose: bool) -> usize {
    let mut removed_count = 0;

    // Don't remove the root directory itself
    if dir_path == root_dir || !dir_path.exists() {
        return 0;
    }

    // Check directory contents
    match fs::read_dir(dir_path) {
        Ok(entries) => {
            let remaining_files: Vec<PathBuf> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .collect();

            // Check if directory is empty or only contains non-audio files
            let only_non_audio = remaining_files.iter()
                .all(|p| p.is_dir() || !is_audio_file(p));

            if remaining_files.is_empty() || only_non_audio {
                // Remove any leftover non-audio files first
                for file in remaining_files.iter().filter(|p| p.is_file()) {
                    if let Err(e) = fs::remove_file(file) {
                        if verbose {
                            crate::logger::debug(
                                &format!("Failed to remove leftover file {}: {}", file.display(), e),
                                verbose,
                            );
                        }
                    } else if verbose {
                        crate::logger::debug(
                            &format!("Removed leftover file: {}", file.display()),
                            verbose,
                        );
                    }
                }

                // Now remove the directory
                match fs::remove_dir(dir_path) {
                    Ok(_) => {
                        if verbose {
                            crate::logger::debug(
                                &format!("Removed empty directory: {}", dir_path.display()),
                                verbose,
                            );
                        }
                        removed_count += 1;

                        // Recursively check parent directory
                        if let Some(parent) = dir_path.parent() {
                            removed_count += cleanup_empty_directory(parent, root_dir, verbose);
                        }
                    }
                    Err(e) => {
                        if verbose {
                            crate::logger::debug(
                                &format!("Could not remove directory {}: {}", dir_path.display(), e),
                                verbose,
                            );
                        }
                    }
                }
            }
        }
        Err(e) => {
            if verbose {
                crate::logger::debug(
                    &format!("Could not read directory {}: {}", dir_path.display(), e),
                    verbose,
                );
            }
        }
    }

    removed_count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize() {
        assert_eq!(sanitize("Artist / Album"), "Artist – Album");
        assert_eq!(sanitize("Test\\Path"), "Test-Path");
        assert_eq!(sanitize("  Multiple   Spaces  "), "Multiple Spaces");
        assert_eq!(sanitize(""), "_unknown");
        assert_eq!(sanitize("Cyclo."), "Cyclo"); // Trailing period
        assert_eq!(sanitize("Name..."), "Name"); // Multiple trailing periods
        assert_eq!(sanitize("Artist. "), "Artist"); // Period and space
    }

    #[test]
    fn test_normalize_for_comparison() {
        assert_eq!(
            normalize_for_comparison("The Beatles - Let It Be"),
            "the beatles let it be"
        );
        assert_eq!(normalize_for_comparison("Can't Stop!!!"), "cant stop");
    }

    #[test]
    fn test_is_audio_file() {
        assert!(is_audio_file(Path::new("song.mp3")));
        assert!(is_audio_file(Path::new("song.FLAC")));
        assert!(is_audio_file(Path::new("song.opus")));
        assert!(!is_audio_file(Path::new("document.txt")));
        assert!(!is_audio_file(Path::new("image.jpg")));
    }

    #[test]
    fn test_normalize_name() {
        assert_eq!(normalize_name("Can't Stop", true), "can't stop");
        assert_eq!(normalize_name("Can't Stop", true), "can't stop"); // curly apostrophe
        assert_eq!(normalize_name("LOUD  NOISES", true), "loud noises");
        assert_eq!(normalize_name("Can't Stop", false), "Can't Stop");
        assert_eq!(normalize_name("LOUD  NOISES", false), "LOUD NOISES");
    }

    #[test]
    fn test_cleanup_empty_directory() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let root = temp.path();

        // Create nested empty directories
        let dir1 = root.join("level1/level2/level3");
        fs::create_dir_all(&dir1).unwrap();

        // Cleanup should remove all nested directories
        let removed = cleanup_empty_directory(&dir1, root, false);
        assert_eq!(removed, 3); // level3, level2, level1
        assert!(!root.join("level1").exists());
    }

    #[test]
    fn test_cleanup_removes_non_audio_files() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let root = temp.path();

        let dir1 = root.join("test_dir");
        fs::create_dir_all(&dir1).unwrap();

        // Add non-audio file
        let cover = dir1.join("cover.png");
        fs::write(&cover, "fake image").unwrap();

        // Cleanup should remove the non-audio file and directory
        let removed = cleanup_empty_directory(&dir1, root, false);
        assert_eq!(removed, 1);
        assert!(!cover.exists());
        assert!(!dir1.exists());
    }

    #[test]
    fn test_cleanup_preserves_audio_files() {
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let root = temp.path();

        let dir1 = root.join("test_dir");
        fs::create_dir_all(&dir1).unwrap();

        // Add audio file
        let audio = dir1.join("track.flac");
        fs::write(&audio, "fake audio").unwrap();

        // Cleanup should NOT remove directory with audio
        let removed = cleanup_empty_directory(&dir1, root, false);
        assert_eq!(removed, 0);
        assert!(dir1.exists());
        assert!(audio.exists());
    }
}
