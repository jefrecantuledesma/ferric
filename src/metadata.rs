use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;
use std::fs::File;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

#[derive(Debug, Clone, Default)]
pub struct AudioMetadata {
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub title: Option<String>,
    pub track_number: Option<u32>,
    pub date: Option<String>,
    pub genre: Option<String>,
    pub codec: String,
    pub bitrate: Option<u32>,  // in bits per second
    pub sample_rate: Option<u32>,
    pub channels: Option<u8>,
    pub duration_secs: Option<f64>,
}

impl AudioMetadata {
    /// Extract metadata from an audio file using ffprobe (more reliable for MP3s)
    pub fn from_file(path: &Path) -> Result<Self> {
        // Use ffprobe for better MP3/ID3 tag support
        Self::from_file_ffprobe(path).or_else(|_| Self::from_file_symphonia(path))
    }

    /// Extract metadata using ffprobe (fallback method, more reliable)
    fn from_file_ffprobe(path: &Path) -> Result<Self> {
        let output = Command::new("ffprobe")
            .args(&[
                "-v", "quiet",
                "-print_format", "json",
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
        let json: serde_json::Value = serde_json::from_str(&json_str)
            .context("Failed to parse ffprobe JSON output")?;

        let mut metadata = AudioMetadata::default();

        // Extract stream information first (OPUS files store tags here)
        if let Some(streams) = json.get("streams").and_then(|s| s.as_array()) {
            for stream in streams {
                if stream.get("codec_type").and_then(|v| v.as_str()) == Some("audio") {
                    metadata.codec = stream.get("codec_name")
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
                        metadata.artist = tags.get("artist")
                            .or_else(|| tags.get("ARTIST"))
                            .and_then(|v| v.as_str())
                            .map(String::from);
                        metadata.album = tags.get("album")
                            .or_else(|| tags.get("ALBUM"))
                            .and_then(|v| v.as_str())
                            .map(String::from);
                        metadata.album_artist = tags.get("album_artist")
                            .or_else(|| tags.get("ALBUM_ARTIST"))
                            .or_else(|| tags.get("albumartist"))
                            .and_then(|v| v.as_str())
                            .map(String::from);
                        metadata.title = tags.get("title")
                            .or_else(|| tags.get("TITLE"))
                            .and_then(|v| v.as_str())
                            .map(String::from);
                        metadata.date = tags.get("date")
                            .or_else(|| tags.get("DATE"))
                            .or_else(|| tags.get("year"))
                            .or_else(|| tags.get("YEAR"))
                            .and_then(|v| v.as_str())
                            .map(String::from);
                        metadata.genre = tags.get("genre")
                            .or_else(|| tags.get("GENRE"))
                            .and_then(|v| v.as_str())
                            .map(String::from);

                        if let Some(track_str) = tags.get("track")
                            .or_else(|| tags.get("TRACK"))
                            .and_then(|v| v.as_str()) {
                            if let Some(num_str) = track_str.split('/').next() {
                                metadata.track_number = num_str.parse().ok();
                            }
                        }
                    }

                    break;
                }
            }
        }

        // Extract format/container metadata (fallback for MP3/MP4 files)
        if let Some(format) = json.get("format") {
            if let Some(tags) = format.get("tags").and_then(|t| t.as_object()) {
                // Only set if not already set from stream tags
                if metadata.artist.is_none() {
                    metadata.artist = tags.get("artist").and_then(|v| v.as_str()).map(String::from);
                }
                if metadata.album.is_none() {
                    metadata.album = tags.get("album").and_then(|v| v.as_str()).map(String::from);
                }
                if metadata.album_artist.is_none() {
                    metadata.album_artist = tags.get("album_artist")
                        .or_else(|| tags.get("ALBUM_ARTIST"))
                        .or_else(|| tags.get("albumartist"))
                        .and_then(|v| v.as_str())
                        .map(String::from);
                }
                if metadata.title.is_none() {
                    metadata.title = tags.get("title").and_then(|v| v.as_str()).map(String::from);
                }
                if metadata.date.is_none() {
                    metadata.date = tags.get("date")
                        .or_else(|| tags.get("year"))
                        .and_then(|v| v.as_str())
                        .map(String::from);
                }
                if metadata.genre.is_none() {
                    metadata.genre = tags.get("genre").and_then(|v| v.as_str()).map(String::from);
                }

                if metadata.track_number.is_none() {
                    if let Some(track_str) = tags.get("track").and_then(|v| v.as_str()) {
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
            .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
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
                    metadata.duration_secs = Some(
                        n_frames as f64 * time_base.numer as f64 / time_base.denom as f64,
                    );
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
}
