use std::path::{Path, PathBuf};

/// Sanitize a string for use as a filename/folder name
/// - Replaces forward slashes with en-dash
/// - Replaces backslashes with hyphen
/// - Removes control characters
/// - Trims whitespace
/// - Collapses multiple spaces
/// - Returns "_Unknown" if empty
pub fn sanitize(s: &str) -> String {
    let mut result = s.to_string();

    // Replace slashes
    result = result.replace('/', "–").replace('\\', "-");

    // Remove control characters (0x00-0x1F)
    result = result
        .chars()
        .filter(|c| *c as u32 >= 0x20)
        .collect();

    // Trim whitespace
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

    // Return default if empty
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
pub fn normalize_for_comparison(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Generate unique path by appending (n) if file exists
pub fn unique_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let filename = path.file_name().unwrap().to_string_lossy();

    // Split into name and extension
    let (base, ext) = if let Some(dot_pos) = filename.rfind('.') {
        let (name, extension) = filename.split_at(dot_pos);
        (name.to_string(), extension.to_string())
    } else {
        (filename.to_string(), String::new())
    };

    // Try incrementing numbers
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
            "flac" | "opus" | "ogg" | "mp3" | "m4a" | "aac" | "wav" | "aiff" | "aif" | "wma" | "alac"
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

/// Normalize name: fix curly apostrophes, lowercase, normalize whitespace
pub fn normalize_name(name: &str) -> String {
    let mut result = name.to_string();

    // Replace Unicode right single quotation mark (') with ASCII apostrophe (')
    result = result.replace('\u{2019}', "'");

    // Convert to lowercase
    result = result.to_lowercase();

    // Normalize whitespace: replace multiple spaces with single space (O(n) single pass)
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

    // Trim leading and trailing whitespace
    result.trim().to_string()
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
    }

    #[test]
    fn test_normalize_for_comparison() {
        assert_eq!(
            normalize_for_comparison("The Beatles - Let It Be"),
            "the beatles let it be"
        );
        assert_eq!(
            normalize_for_comparison("Can't Stop!!!"),
            "can t stop"
        );
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
        assert_eq!(normalize_name("Can't Stop"), "can't stop");
        assert_eq!(normalize_name("Can't Stop"), "can't stop"); // curly apostrophe
        assert_eq!(normalize_name("LOUD  NOISES"), "loud noises");
    }
}
