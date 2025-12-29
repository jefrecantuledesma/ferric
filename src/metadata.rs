use crate::{cache, logger};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::path::Path;
use std::process::Command;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AudioMetadata {
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub title: Option<String>,
    pub track_number: Option<u32>,
    pub date: Option<String>,
    pub genre: Option<String>,
    pub codec: String,
    pub bitrate: Option<u32>, // in bits per second
    pub sample_rate: Option<u32>,
    pub channels: Option<u8>,
    pub duration_secs: Option<f64>,

    // Audio fingerprinting and MusicBrainz integration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub musicbrainz_recording_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub musicbrainz_release_id: Option<String>,
}

impl AudioMetadata {
    /// Extract metadata from an audio file using ffprobe (more reliable for MP3s)
    pub fn from_file(path: &Path) -> Result<Self> {
        if let Some(cache) = cache::get_global_cache() {
            match cache.get(path) {
                Ok(Some(metadata)) => return Ok(metadata),
                Ok(None) => {}
                Err(err) => logger::warning(&format!(
                    "Metadata cache lookup failed for {}: {}",
                    path.display(),
                    err
                )),
            }
        }

        // Use ffprobe for better MP3/ID3 tag support
        let metadata =
            Self::from_file_ffprobe(path).or_else(|_| Self::from_file_symphonia(path))?;

        if let Some(cache) = cache::get_global_cache() {
            if let Err(err) = cache.insert(path, &metadata) {
                logger::warning(&format!(
                    "Failed to store metadata for {} in cache: {}",
                    path.display(),
                    err
                ));
            }
        }

        Ok(metadata)
    }

    /// Normalize a tag key for case-insensitive, whitespace-insensitive matching
    ///
    /// Converts to lowercase and removes all non-alphanumeric characters.
    /// This allows matching variations like:
    /// - "Album Artist", "album_artist", "ALBUM-ARTIST" all become "albumartist"
    /// - "GENRE", "Genre", "genre" all become "genre"
    fn normalize_tag_key(key: &str) -> String {
        key.to_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric())
            .collect()
    }

    /// Get a tag value with fuzzy matching (case-insensitive, separator-insensitive)
    ///
    /// Fast path: tries exact match first (covers 99% of cases)
    /// Slow path: normalizes keys and does fuzzy matching
    fn get_tag_fuzzy(tags: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<String> {
        // Fast path: try exact match first (most common case)
        if let Some(val) = tags.get(key).and_then(|v| v.as_str()) {
            return Some(val.to_string());
        }

        // Normalize the search key
        let normalized_key = Self::normalize_tag_key(key);

        // Slow path: fuzzy match by normalizing all tag keys
        for (k, v) in tags {
            if Self::normalize_tag_key(k) == normalized_key {
                return v.as_str().map(String::from);
            }
        }

        None
    }

    /// Extract metadata using ffprobe (fallback method, more reliable)
    fn from_file_ffprobe(path: &Path) -> Result<Self> {
        let output = Command::new("ffprobe")
            .args(&[
                "-v",
                "quiet",
                "-print_format",
                "json",
                "-show_format",
                "-show_streams",
            ])
            .arg(path)
            .output()
            .context("Failed to run ffprobe")?;

        if !output.status.success() {
            anyhow::bail!("ffprobe failed");
        }

        let json_str = String::from_utf8_lossy(&output.stdout);
        let json: serde_json::Value =
            serde_json::from_str(&json_str).context("Failed to parse ffprobe JSON output")?;

        let mut metadata = AudioMetadata::default();

        // Extract stream information first (OPUS files store tags here)
        if let Some(streams) = json.get("streams").and_then(|s| s.as_array()) {
            for stream in streams {
                if stream.get("codec_type").and_then(|v| v.as_str()) == Some("audio") {
                    metadata.codec = stream
                        .get("codec_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string();

                    if let Some(sr) = stream.get("sample_rate").and_then(|v| v.as_str()) {
                        metadata.sample_rate = sr.parse().ok();
                    }

                    if let Some(ch) = stream.get("channels").and_then(|v| v.as_u64()) {
                        metadata.channels = Some(ch as u8);
                    }

                    // Extract tags from stream (OPUS/OGG files store tags here)
                    if let Some(tags) = stream.get("tags").and_then(|t| t.as_object()) {
                        metadata.artist = Self::get_tag_fuzzy(tags, "artist");
                        metadata.album = Self::get_tag_fuzzy(tags, "album");
                        metadata.album_artist = Self::get_tag_fuzzy(tags, "albumartist");
                        metadata.title = Self::get_tag_fuzzy(tags, "title");
                        metadata.genre = Self::get_tag_fuzzy(tags, "genre");

                        // Date can be "date" or "year"
                        metadata.date = Self::get_tag_fuzzy(tags, "date")
                            .or_else(|| Self::get_tag_fuzzy(tags, "year"));

                        // Track number
                        if let Some(track_str) = Self::get_tag_fuzzy(tags, "track") {
                            if let Some(num_str) = track_str.split('/').next() {
                                metadata.track_number = num_str.parse().ok();
                            }
                        }
                    }

                    break;
                }
            }
        }

        // Extract format/container metadata (fallback for MP3/MP4 files and primary for FLAC)
        if let Some(format) = json.get("format") {
            if let Some(tags) = format.get("tags").and_then(|t| t.as_object()) {
                // Only set if not already set from stream tags
                if metadata.artist.is_none() {
                    metadata.artist = Self::get_tag_fuzzy(tags, "artist");
                }
                if metadata.album.is_none() {
                    metadata.album = Self::get_tag_fuzzy(tags, "album");
                }
                if metadata.album_artist.is_none() {
                    metadata.album_artist = Self::get_tag_fuzzy(tags, "albumartist");
                }
                if metadata.title.is_none() {
                    metadata.title = Self::get_tag_fuzzy(tags, "title");
                }
                if metadata.date.is_none() {
                    metadata.date = Self::get_tag_fuzzy(tags, "date")
                        .or_else(|| Self::get_tag_fuzzy(tags, "year"));
                }
                if metadata.genre.is_none() {
                    metadata.genre = Self::get_tag_fuzzy(tags, "genre");
                }

                if metadata.track_number.is_none() {
                    if let Some(track_str) = Self::get_tag_fuzzy(tags, "track") {
                        if let Some(num_str) = track_str.split('/').next() {
                            metadata.track_number = num_str.parse().ok();
                        }
                    }
                }
            }

            if let Some(bitrate_str) = format.get("bit_rate").and_then(|v| v.as_str()) {
                metadata.bitrate = bitrate_str.parse().ok();
            }

            if let Some(duration_str) = format.get("duration").and_then(|v| v.as_str()) {
                metadata.duration_secs = duration_str.parse().ok();
            }
        }

        // If no title, use filename
        if metadata.title.is_none() {
            if let Some(filename) = path.file_stem() {
                metadata.title = Some(filename.to_string_lossy().to_string());
            }
        }

        Ok(metadata)
    }

    /// Extract metadata using Symphonia (original method)
    fn from_file_symphonia(path: &Path) -> Result<Self> {
        let file = File::open(path)
            .with_context(|| format!("Failed to open audio file: {}", path.display()))?;

        let mss = MediaSourceStream::new(Box::new(file), Default::default());

        // Create hint from file extension
        let mut hint = Hint::new();
        if let Some(ext) = path.extension() {
            hint.with_extension(&ext.to_string_lossy());
        }

        // Probe the media source
        let mut probed = symphonia::default::get_probe()
            .format(
                &hint,
                mss,
                &FormatOptions::default(),
                &MetadataOptions::default(),
            )
            .with_context(|| format!("Failed to probe audio file: {}", path.display()))?;

        let mut metadata = AudioMetadata::default();

        // Extract codec information from the default track
        if let Some(track) = probed.format.default_track() {
            metadata.codec = track.codec_params.codec.to_string();
            // Note: bitrate is not directly available from CodecParameters in symphonia 0.5
            // It would need to be calculated or extracted from format metadata
            metadata.bitrate = None;
            metadata.sample_rate = track.codec_params.sample_rate;
            metadata.channels = track.codec_params.channels.map(|ch| ch.count() as u8);

            // Calculate duration
            if let Some(n_frames) = track.codec_params.n_frames {
                if let Some(sample_rate) = track.codec_params.sample_rate {
                    metadata.duration_secs = Some(n_frames as f64 / sample_rate as f64);
                }
            } else if let Some(time_base) = track.codec_params.time_base {
                if let Some(n_frames) = track.codec_params.n_frames {
                    metadata.duration_secs =
                        Some(n_frames as f64 * time_base.numer as f64 / time_base.denom as f64);
                }
            }
        }

        // Extract metadata tags
        if let Some(metadata_rev) = probed.format.metadata().current() {
            for tag in metadata_rev.tags() {
                let key = tag.key.to_lowercase();
                let value = tag.value.to_string();

                match key.as_str() {
                    "artist" => metadata.artist = Some(value),
                    "album" => metadata.album = Some(value),
                    "albumartist" | "album_artist" | "album artist" => {
                        metadata.album_artist = Some(value)
                    }
                    "title" => metadata.title = Some(value),
                    "tracknumber" | "track" => {
                        if let Ok(num) = value.parse::<u32>() {
                            metadata.track_number = Some(num);
                        } else if let Some(first_part) = value.split('/').next() {
                            if let Ok(num) = first_part.parse::<u32>() {
                                metadata.track_number = Some(num);
                            }
                        }
                    }
                    "date" | "year" => metadata.date = Some(value),
                    "genre" => metadata.genre = Some(value),
                    _ => {}
                }
            }
        }

        // If no title, use filename
        if metadata.title.is_none() {
            if let Some(filename) = path.file_stem() {
                metadata.title = Some(filename.to_string_lossy().to_string());
            }
        }

        Ok(metadata)
    }

    /// Get the best "artist" for organizing (album_artist preferred, then artist)
    pub fn get_organizing_artist(&self, prefer_track_artist: bool) -> String {
        if prefer_track_artist {
            self.artist
                .clone()
                .or_else(|| self.album_artist.clone())
                .unwrap_or_else(|| "_unknown artist".to_string())
        } else {
            self.album_artist
                .clone()
                .or_else(|| self.artist.clone())
                .unwrap_or_else(|| "_unknown artist".to_string())
        }
    }

    /// Get album name or default
    pub fn get_album(&self) -> String {
        self.album
            .clone()
            .unwrap_or_else(|| "_unknown album".to_string())
    }

    /// Get title or default
    pub fn get_title(&self) -> String {
        self.title
            .clone()
            .unwrap_or_else(|| "_unknown title".to_string())
    }

    /// Check if this looks like a "Various Artists" album
    pub fn looks_like_va(&self) -> bool {
        if let Some(ref aa) = self.album_artist {
            let aa_lower = aa.to_lowercase();
            aa_lower.contains("various") || aa_lower.contains("compilation")
        } else {
            false
        }
    }

    /// Get bitrate in kbps (kilobits per second)
    pub fn get_bitrate_kbps(&self) -> Option<u32> {
        self.bitrate.map(|br| br / 1000)
    }

    /// Add a fingerprint to this metadata and optionally update cache
    pub fn add_fingerprint(&mut self, fingerprint: String, path: &Path) -> Result<()> {
        self.fingerprint = Some(fingerprint);

        // Update cache if available
        if let Some(cache) = cache::get_global_cache() {
            cache
                .insert(path, self)
                .context("Failed to update cache with fingerprint")?;
        }

        Ok(())
    }

    /// Extract metadata and generate fingerprint in one operation
    ///
    /// This is more efficient than calling from_file() then generating fingerprint separately,
    /// as it only writes to the cache once.
    pub fn from_file_with_fingerprint(path: &Path) -> Result<Self> {
        // First extract basic metadata
        let mut metadata = Self::from_file(path)?;

        // Generate fingerprint
        match crate::fingerprint::generate_fingerprint(path) {
            Ok(fp) => {
                metadata.fingerprint = Some(fp);
                // Update cache with fingerprint included
                if let Some(cache) = cache::get_global_cache() {
                    let _ = cache.insert(path, &metadata);
                }
            }
            Err(e) => {
                logger::warning(&format!(
                    "Failed to generate fingerprint for {}: {}",
                    path.display(),
                    e
                ));
            }
        }

        Ok(metadata)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_organizing_artist() {
        let mut meta = AudioMetadata::default();
        meta.artist = Some("The Beatles".to_string());
        meta.album_artist = Some("Beatles, The".to_string());

        // Prefer album artist (default)
        assert_eq!(meta.get_organizing_artist(false), "Beatles, The");

        // Prefer track artist
        assert_eq!(meta.get_organizing_artist(true), "The Beatles");
    }

    #[test]
    fn test_looks_like_va() {
        let mut meta = AudioMetadata::default();
        meta.album_artist = Some("Various Artists".to_string());
        assert!(meta.looks_like_va());

        meta.album_artist = Some("Compilation".to_string());
        assert!(meta.looks_like_va());

        meta.album_artist = Some("The Beatles".to_string());
        assert!(!meta.looks_like_va());
    }

    #[test]
    fn test_uppercase_flac_tags() {
        // Test that we can parse uppercase tags from FLAC files (format.tags section)
        // This simulates the ffprobe JSON output for FLAC files
        let json_str = r#"{
            "streams": [{
                "codec_name": "flac",
                "codec_type": "audio",
                "sample_rate": "44100",
                "channels": 2
            }],
            "format": {
                "format_name": "flac",
                "bit_rate": "1056812",
                "duration": "108.840408",
                "tags": {
                    "TITLE": "Gas Pedal",
                    "ARTIST": "Babe Haven",
                    "ALBUM": "Nuisance",
                    "TRACK": "1",
                    "DATE": "2024",
                    "GENRE": "Hardcore"
                }
            }
        }"#;

        let json: serde_json::Value = serde_json::from_str(json_str).unwrap();

        // Simulate the parsing logic from from_file_ffprobe
        let tags = json.get("format").unwrap().get("tags").unwrap().as_object().unwrap();

        let artist = tags
            .get("artist")
            .or_else(|| tags.get("ARTIST"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let album = tags
            .get("album")
            .or_else(|| tags.get("ALBUM"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let title = tags
            .get("title")
            .or_else(|| tags.get("TITLE"))
            .and_then(|v| v.as_str())
            .map(String::from);

        assert_eq!(artist, Some("Babe Haven".to_string()));
        assert_eq!(album, Some("Nuisance".to_string()));
        assert_eq!(title, Some("Gas Pedal".to_string()));
    }

    #[test]
    fn test_normalize_tag_key() {
        // Test case normalization
        assert_eq!(AudioMetadata::normalize_tag_key("ARTIST"), "artist");
        assert_eq!(AudioMetadata::normalize_tag_key("Artist"), "artist");
        assert_eq!(AudioMetadata::normalize_tag_key("artist"), "artist");

        // Test whitespace removal
        assert_eq!(AudioMetadata::normalize_tag_key("Album Artist"), "albumartist");
        assert_eq!(AudioMetadata::normalize_tag_key("album artist"), "albumartist");

        // Test separator removal
        assert_eq!(AudioMetadata::normalize_tag_key("album_artist"), "albumartist");
        assert_eq!(AudioMetadata::normalize_tag_key("album-artist"), "albumartist");
        assert_eq!(AudioMetadata::normalize_tag_key("ALBUM_ARTIST"), "albumartist");

        // Test combination
        assert_eq!(AudioMetadata::normalize_tag_key("Album-Artist"), "albumartist");
        assert_eq!(AudioMetadata::normalize_tag_key("ALBUM ARTIST"), "albumartist");
    }

    #[test]
    fn test_get_tag_fuzzy() {
        use serde_json::json;

        // Test exact match (fast path)
        let tags_exact = json!({
            "artist": "The Beatles",
            "album": "Abbey Road"
        });
        let tags_map = tags_exact.as_object().unwrap();

        assert_eq!(
            AudioMetadata::get_tag_fuzzy(tags_map, "artist"),
            Some("The Beatles".to_string())
        );

        // Test uppercase tags (common in FLAC)
        let tags_upper = json!({
            "ARTIST": "Babe Haven",
            "ALBUM": "Nuisance",
            "GENRE": "Hardcore"
        });
        let tags_map = tags_upper.as_object().unwrap();

        assert_eq!(
            AudioMetadata::get_tag_fuzzy(tags_map, "artist"),
            Some("Babe Haven".to_string())
        );
        assert_eq!(
            AudioMetadata::get_tag_fuzzy(tags_map, "album"),
            Some("Nuisance".to_string())
        );
        assert_eq!(
            AudioMetadata::get_tag_fuzzy(tags_map, "genre"),
            Some("Hardcore".to_string())
        );

        // Test variations with spaces and separators
        // Each variation should be tested separately since they all normalize to the same key
        let tags_space = json!({"Album Artist": "Various Artists"});
        assert_eq!(
            AudioMetadata::get_tag_fuzzy(tags_space.as_object().unwrap(), "albumartist"),
            Some("Various Artists".to_string())
        );

        let tags_underscore = json!({"album_artist": "Compilation"});
        assert_eq!(
            AudioMetadata::get_tag_fuzzy(tags_underscore.as_object().unwrap(), "albumartist"),
            Some("Compilation".to_string())
        );

        let tags_hyphen = json!({"ALBUM-ARTIST": "VA"});
        assert_eq!(
            AudioMetadata::get_tag_fuzzy(tags_hyphen.as_object().unwrap(), "albumartist"),
            Some("VA".to_string())
        );

        // Test missing tag
        let tags_missing = json!({
            "artist": "Someone"
        });
        let tags_map = tags_missing.as_object().unwrap();

        assert_eq!(AudioMetadata::get_tag_fuzzy(tags_map, "album"), None);
    }
}
