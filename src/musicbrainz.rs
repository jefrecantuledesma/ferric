use crate::metadata::AudioMetadata;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;

/// Result from AcoustID fingerprint lookup
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcoustIdResult {
    /// MusicBrainz recording ID
    pub recording_id: String,
    /// Confidence score (0.0 - 1.0)
    pub score: f32,
    /// Recording title (if available)
    pub title: Option<String>,
    /// Artists (if available)
    pub artists: Vec<String>,
}

/// Complete metadata from MusicBrainz
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MusicBrainzMetadata {
    /// MusicBrainz recording ID
    pub recording_id: String,
    /// MusicBrainz release ID (album)
    pub release_id: Option<String>,
    /// Track title
    pub title: String,
    /// Artist name(s)
    pub artist: String,
    /// Album name
    pub album: Option<String>,
    /// Album artist (for compilations, etc.)
    pub album_artist: Option<String>,
    /// Track number on release
    pub track_number: Option<u32>,
    /// Release date (YYYY or YYYY-MM-DD)
    pub date: Option<String>,
    /// Genre/tags
    pub genres: Vec<String>,
}

/// Look up a recording using an AcoustID fingerprint
///
/// This function submits a fingerprint to the AcoustID API and retrieves
/// matching MusicBrainz recording IDs with confidence scores.
///
/// # Arguments
/// * `fingerprint` - The audio fingerprint (comma-separated u32 values)
/// * `duration_secs` - Duration of the audio file in seconds
/// * `api_key` - AcoustID API key
///
/// # Returns
/// Vector of results sorted by confidence score (highest first)
pub async fn lookup_by_fingerprint(
    fingerprint: &str,
    duration_secs: f64,
    api_key: &str,
) -> Result<Vec<AcoustIdResult>> {
    let client = reqwest::Client::new();

    // AcoustID API endpoint
    let url = "https://api.acoustid.org/v2/lookup";

    // Make request
    // Convert duration to integer seconds (API expects integer)
    let duration_int = duration_secs.round() as i32;

    let response = client
        .post(url)
        .form(&[
            ("client", api_key),
            ("duration", &duration_int.to_string()),
            ("fingerprint", fingerprint),
            ("meta", "recordings releasegroups compress"),
        ])
        .send()
        .await
        .context("Failed to send request to AcoustID API")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_else(|_| "Unable to read response body".to_string());
        anyhow::bail!("AcoustID API returned error {}: {}", status, body);
    }

    let json: serde_json::Value = response
        .json()
        .await
        .context("Failed to parse AcoustID response")?;

    // Parse results
    let mut results = Vec::new();

    if let Some(results_array) = json["results"].as_array() {
        for result in results_array {
            let score = result["score"].as_f64().unwrap_or(0.0) as f32;

            if let Some(recordings) = result["recordings"].as_array() {
                for recording in recordings {
                    let recording_id = recording["id"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();

                    if recording_id.is_empty() {
                        continue;
                    }

                    let title = recording["title"].as_str().map(String::from);

                    let artists = recording["artists"]
                        .as_array()
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|a| a["name"].as_str())
                                .map(String::from)
                                .collect()
                        })
                        .unwrap_or_default();

                    results.push(AcoustIdResult {
                        recording_id,
                        score,
                        title,
                        artists,
                    });
                }
            }
        }
    }

    // Sort by score descending
    results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

    Ok(results)
}

/// Fetch complete metadata for a recording from MusicBrainz
///
/// # Arguments
/// * `recording_id` - MusicBrainz recording ID
/// * `user_agent` - User agent string (required by MusicBrainz)
///
/// # Rate Limiting
/// Caller is responsible for rate limiting (≤1 request/second)
pub async fn fetch_recording_metadata(
    recording_id: &str,
    _user_agent: &str,
) -> Result<MusicBrainzMetadata> {
    use musicbrainz_rs::entity::recording::Recording;
    use musicbrainz_rs::prelude::*;

    // Note: User agent configuration handled globally by musicbrainz_rs

    // Fetch recording with includes
    let recording = Recording::fetch()
        .id(recording_id)
        .with_artists()
        .with_releases()
        .with_tags()
        .execute()
        .await
        .context("Failed to fetch recording from MusicBrainz")?;

    // Extract artist names from artist credit
    let artist = recording
        .artist_credit
        .as_ref()
        .map(|ac| {
            ac.iter()
                .map(|a| a.name.clone())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_else(|| "Unknown Artist".to_string());

    // Find best release (prefer official releases, then by date)
    let best_release = recording
        .releases
        .as_ref()
        .and_then(|releases| {
            releases
                .iter()
                .filter(|r| {
                    // ReleaseStatus is an enum, compare directly
                    matches!(r.status, Some(musicbrainz_rs::entity::release::ReleaseStatus::Official))
                })
                .max_by(|a, b| {
                    // DateString is a newtype wrapper, compare the inner String
                    match (&a.date, &b.date) {
                        (Some(a_date), Some(b_date)) => a_date.0.cmp(&b_date.0),
                        (Some(_), None) => std::cmp::Ordering::Greater,
                        (None, Some(_)) => std::cmp::Ordering::Less,
                        (None, None) => std::cmp::Ordering::Equal,
                    }
                })
                .or_else(|| releases.first())
        });

    let album = best_release.map(|r| r.title.clone());
    let release_id = best_release.map(|r| r.id.clone());
    let date = best_release.and_then(|r| r.date.as_ref().map(|d| d.0.clone()));

    let album_artist = best_release.and_then(|r| {
        r.artist_credit.as_ref().map(|ac| {
            ac.iter()
                .map(|a| a.name.clone())
                .collect::<Vec<_>>()
                .join(", ")
        })
    });

    // Extract genres from tags
    let genres = recording
        .tags
        .unwrap_or_default()
        .iter()
        .map(|t| t.name.clone())
        .collect();

    Ok(MusicBrainzMetadata {
        recording_id: recording.id,
        release_id,
        title: recording.title,
        artist,
        album,
        album_artist,
        track_number: None, // TODO: Extract from release/medium
        date,
        genres,
    })
}

/// Search MusicBrainz by existing metadata (fuzzy matching)
///
/// Useful for files that already have some metadata but might be incomplete or incorrect.
pub async fn search_by_metadata(
    metadata: &AudioMetadata,
    _user_agent: &str,
) -> Result<Vec<MusicBrainzMetadata>> {
    use musicbrainz_rs::entity::recording::Recording;
    use musicbrainz_rs::prelude::*;

    // Note: User agent configuration handled globally by musicbrainz_rs

    // Build search query
    let mut query_parts = Vec::new();

    if let Some(ref artist) = metadata.artist {
        query_parts.push(format!("artist:\"{}\"", artist));
    }
    if let Some(ref title) = metadata.title {
        query_parts.push(format!("recording:\"{}\"", title));
    }
    if let Some(ref album) = metadata.album {
        query_parts.push(format!("release:\"{}\"", album));
    }

    if query_parts.is_empty() {
        anyhow::bail!("No metadata available to search with");
    }

    let query = query_parts.join(" AND ");

    // Search recordings
    let search_result = Recording::search(query)
        .execute()
        .await
        .context("Failed to search MusicBrainz")?;

    // Convert results to our format
    let mut results = Vec::new();
    for recording in search_result.entities.into_iter().take(10) {
        let artist = recording
            .artist_credit
            .as_ref()
            .map(|ac| {
                ac.iter()
                    .map(|a| a.name.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_else(|| "Unknown Artist".to_string());

        let best_release = recording.releases.as_ref().and_then(|r| r.first());

        results.push(MusicBrainzMetadata {
            recording_id: recording.id.clone(),
            release_id: best_release.map(|r| r.id.clone()),
            title: recording.title.clone(),
            artist,
            album: best_release.map(|r| r.title.clone()),
            album_artist: None,
            track_number: None,
            date: best_release.and_then(|r| r.date.as_ref().map(|d| d.0.clone())),
            genres: recording
                .tags
                .unwrap_or_default()
                .iter()
                .map(|t| t.name.clone())
                .collect(),
        });
    }

    Ok(results)
}

/// Apply MusicBrainz metadata to an audio file
///
/// Updates the file's tags using ffmpeg and stores MusicBrainz IDs.
/// Only updates fields specified in `fields_to_update` for additive-only mode.
///
/// # Arguments
/// * `file_path` - Path to the audio file
/// * `current_metadata` - Current file metadata (to preserve existing values)
/// * `mb_metadata` - MusicBrainz metadata to apply
/// * `fields_to_update` - Which fields should be updated
/// * `dry_run` - If true, only show what would be done
pub fn apply_metadata_to_file(
    file_path: &Path,
    current_metadata: &AudioMetadata,
    mb_metadata: &MusicBrainzMetadata,
    fields_to_update: &crate::operations::fix_metadata_mb::FieldsToUpdate,
    dry_run: bool,
) -> Result<()> {
    if dry_run {
        crate::logger::info(&format!(
            "Would update {} with:",
            file_path.display()
        ));
        if fields_to_update.update_title {
            crate::logger::info(&format!("  Title: {}", mb_metadata.title));
        }
        if fields_to_update.update_artist {
            crate::logger::info(&format!("  Artist: {}", mb_metadata.artist));
        }
        if fields_to_update.update_album {
            if let Some(ref album) = mb_metadata.album {
                crate::logger::info(&format!("  Album: {}", album));
            }
        }
        if fields_to_update.update_date {
            if let Some(ref date) = mb_metadata.date {
                crate::logger::info(&format!("  Date: {}", date));
            }
        }
        crate::logger::info(&format!(
            "  MusicBrainz Recording ID: {}",
            mb_metadata.recording_id
        ));
        return Ok(());
    }

    // Create temporary file
    let temp_path = file_path.with_extension("tmp");

    // Build ffmpeg command with metadata
    let mut cmd = Command::new("ffmpeg");
    cmd.args(&["-i"])
        .arg(file_path)
        .args(&["-c", "copy"]); // Copy codec, don't re-encode

    // Only add metadata for fields that should be updated
    if fields_to_update.update_title {
        cmd.args(&["-metadata", &format!("title={}", mb_metadata.title)]);
    }
    if fields_to_update.update_artist {
        cmd.args(&["-metadata", &format!("artist={}", mb_metadata.artist)]);
    }
    if fields_to_update.update_album {
        if let Some(ref album) = mb_metadata.album {
            cmd.args(&["-metadata", &format!("album={}", album)]);
        }
    }
    if fields_to_update.update_album_artist {
        if let Some(ref album_artist) = mb_metadata.album_artist {
            cmd.args(&["-metadata", &format!("album_artist={}", album_artist)]);
        }
    }
    if fields_to_update.update_date {
        if let Some(ref date) = mb_metadata.date {
            cmd.args(&["-metadata", &format!("date={}", date)]);
        }
    }
    if let Some(track_num) = mb_metadata.track_number {
        cmd.args(&["-metadata", &format!("track={}", track_num)]);
    }
    if fields_to_update.update_genre && !mb_metadata.genres.is_empty() {
        cmd.args(&["-metadata", &format!("genre={}", mb_metadata.genres[0])]);
    }

    // Add MusicBrainz IDs
    cmd.args(&[
        "-metadata",
        &format!("MUSICBRAINZ_TRACKID={}", mb_metadata.recording_id),
    ]);
    if let Some(ref release_id) = mb_metadata.release_id {
        cmd.args(&["-metadata", &format!("MUSICBRAINZ_ALBUMID={}", release_id)]);
    }

    cmd.args(&["-y"]) // Overwrite output file
        .arg(&temp_path);

    let output = cmd
        .output()
        .context("Failed to execute ffmpeg for metadata update")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("ffmpeg failed to update metadata: {}", stderr);
    }

    // Replace original with updated file
    std::fs::rename(&temp_path, file_path)
        .context("Failed to replace original file with updated version")?;

    // Update cache
    let mut metadata = AudioMetadata::from_file(file_path)?;
    metadata.musicbrainz_recording_id = Some(mb_metadata.recording_id.clone());
    metadata.musicbrainz_release_id = mb_metadata.release_id.clone();

    if let Some(cache) = crate::cache::get_global_cache() {
        let _ = cache.insert(file_path, &metadata);
    }

    crate::logger::success(&format!("Updated metadata for {}", file_path.display()));
    Ok(())
}

/// Rate limiter for API requests
///
/// Ensures we don't exceed MusicBrainz/AcoustID rate limits
pub struct RateLimiter {
    last_request: std::sync::Mutex<std::time::Instant>,
    min_interval: std::time::Duration,
}

impl RateLimiter {
    pub fn new(requests_per_second: f32) -> Self {
        let min_interval = std::time::Duration::from_secs_f32(1.0 / requests_per_second);
        Self {
            last_request: std::sync::Mutex::new(
                std::time::Instant::now() - min_interval, // Allow first request immediately
            ),
            min_interval,
        }
    }

    /// Wait until it's safe to make another request (async)
    pub async fn wait(&self) {
        let elapsed = {
            let last = self.last_request.lock().unwrap();
            last.elapsed()
        };

        if elapsed < self.min_interval {
            let sleep_duration = self.min_interval - elapsed;
            tokio::time::sleep(sleep_duration).await;
        }

        *self.last_request.lock().unwrap() = std::time::Instant::now();
    }
}

/// Helper to get AcoustID API key from config or environment
pub fn get_acoustid_api_key(config: &crate::config::Config) -> Result<String> {
    config
        .musicbrainz
        .acoustid_api_key
        .clone()
        .or_else(|| std::env::var("ACOUSTID_API_KEY").ok())
        .context("AcoustID API key not found. Set ACOUSTID_API_KEY environment variable or add to config file. Get one from https://acoustid.org/api-key")
}
