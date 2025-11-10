use crate::logger;
use crate::metadata::AudioMetadata;
use crate::utils;
use anyhow::{anyhow, Context, Result};
use csv::ReaderBuilder;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use strsim::jaro_winkler;
use walkdir::WalkDir;

#[derive(Debug, Clone)]
pub struct PlaylistImportOptions {
    pub playlist_csv: PathBuf,
    pub library_dir: PathBuf,
    pub output_path: Option<PathBuf>,
    pub dry_run: bool,
    pub verbose: bool,
}

#[derive(Debug)]
struct PlaylistEntry {
    artist_raw: String,
    title_raw: String,
    artist_keys: Vec<String>,
    title_keys: Vec<String>,
}

#[derive(Debug, Clone)]
struct MatchCandidate {
    path: PathBuf,
    score: f64,
    match_type: String,
}

/// Build an .m3u playlist by matching Exportify CSV rows to local audio files.
pub fn run(options: PlaylistImportOptions) -> Result<()> {
    logger::stage("Generating playlist from CSV export");
    logger::info(&format!("Playlist CSV: {}", options.playlist_csv.display()));
    logger::info(&format!(
        "Library directory: {}",
        options.library_dir.display()
    ));

    let entries = load_playlist_entries(&options.playlist_csv)?;
    if entries.is_empty() {
        logger::warning("Playlist CSV contained no tracks; nothing to do");
        return Ok(());
    }

    let library_tracks = build_library_index(&options.library_dir, options.verbose)?;

    // Build HashMap for fast exact match lookups
    logger::info("Building exact match index...");
    let exact_match_index = build_exact_match_index(&library_tracks);
    logger::success(&format!(
        "Built exact match index with {} entries",
        exact_match_index.len()
    ));

    let mut matched_paths = Vec::with_capacity(entries.len());
    let mut missing = Vec::new();

    for entry in &entries {
        let candidates = find_matches(&entry, &library_tracks, &exact_match_index, options.verbose);

        if candidates.is_empty() {
            missing.push(format!("{} - {}", entry.artist_raw, entry.title_raw));
        } else if candidates.len() == 1 {
            let candidate = &candidates[0];
            logger::debug(
                &format!(
                    "Matched '{}' - '{}' to {} (score: {:.2}, type: {})",
                    entry.artist_raw,
                    entry.title_raw,
                    candidate.path.display(),
                    candidate.score,
                    candidate.match_type
                ),
                options.verbose,
            );
            matched_paths.push(candidate.path.clone());
        } else {
            // Multiple matches found - let user choose
            logger::plain("");
            logger::warning(&format!(
                "Multiple matches found for: {} - {}",
                entry.artist_raw, entry.title_raw
            ));

            for (i, candidate) in candidates.iter().enumerate() {
                logger::plain(&format!(
                    "  {}. {} (score: {:.2}, {})",
                    i + 1,
                    candidate.path.display(),
                    candidate.score,
                    candidate.match_type
                ));
            }

            logger::plain("  0. Skip this track");
            logger::plain("");

            let choice = prompt_user_choice(candidates.len())?;

            if choice > 0 {
                let selected = &candidates[choice - 1];
                logger::success(&format!("Selected: {}", selected.path.display()));
                matched_paths.push(selected.path.clone());
            } else {
                logger::info("Skipped");
                missing.push(format!("{} - {}", entry.artist_raw, entry.title_raw));
            }
        }
    }

    logger::plain("");
    logger::info(&format!("Requested tracks : {}", entries.len()));
    logger::success(&format!("Matched tracks   : {}", matched_paths.len()));
    if !missing.is_empty() {
        logger::warning(&format!("Missing tracks   : {}", missing.len()));
        for track in &missing {
            logger::plain(&format!("  - {}", track));
        }
    }

    let output_path = options
        .output_path
        .clone()
        .unwrap_or_else(|| default_output_path(&options.playlist_csv));

    if options.dry_run {
        logger::warning("Dry-run enabled; skipping .m3u generation");
        logger::info(&format!("Planned output path: {}", output_path.display()));
        return Ok(());
    }

    if matched_paths.is_empty() {
        logger::warning("No matching tracks were found; not writing .m3u file");
        return Ok(());
    }

    write_m3u(&matched_paths, &output_path)?;
    logger::success(&format!("Playlist written to {}", output_path.display()));
    Ok(())
}

fn load_playlist_entries(path: &Path) -> Result<Vec<PlaylistEntry>> {
    let mut reader = ReaderBuilder::new()
        .trim(csv::Trim::All)
        .flexible(true)
        .from_path(path)
        .with_context(|| format!("Failed to open playlist CSV at {}", path.display()))?;

    let headers = reader
        .headers()
        .with_context(|| format!("CSV missing headers: {}", path.display()))?
        .clone();

    let track_idx = find_header_index(&headers, "track name")
        .ok_or_else(|| anyhow!("CSV missing 'Track Name' column"))?;
    let artist_idx = find_header_index(&headers, "artist name(s)")
        .ok_or_else(|| anyhow!("CSV missing 'Artist Name(s)' column"))?;

    let mut entries = Vec::new();
    for result in reader.records() {
        let record = result?;
        let title = record
            .get(track_idx)
            .map(|s| strip_bom(s).trim().to_string())
            .unwrap_or_default();
        let artist = record
            .get(artist_idx)
            .map(|s| strip_bom(s).trim().to_string())
            .unwrap_or_default();

        if title.is_empty() || artist.is_empty() {
            continue;
        }

        let mut artist_keys = expand_artist_keys(&artist);
        if artist_keys.is_empty() {
            let fallback = utils::normalize_for_comparison(&artist);
            if !fallback.is_empty() {
                artist_keys.push(fallback);
            }
        }

        let title_keys = expand_title_variants(&title, &artist_keys);

        if artist_keys.is_empty() || title_keys.is_empty() {
            continue;
        }

        entries.push(PlaylistEntry {
            artist_raw: artist,
            title_raw: title,
            artist_keys,
            title_keys,
        });
    }

    Ok(entries)
}

fn find_header_index(headers: &csv::StringRecord, target: &str) -> Option<usize> {
    headers.iter().position(|name| {
        let normalized = strip_bom(name).trim().to_lowercase();
        normalized == target
    })
}

fn strip_bom(value: &str) -> &str {
    value.strip_prefix('\u{feff}').unwrap_or(value)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LibraryTrack {
    path: PathBuf,
    artist_variants: Vec<String>,
    title_variants: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct LibraryCache {
    version: u32,
    library_path: PathBuf,
    track_count: usize,
    last_modified: std::time::SystemTime,
    tracks: Vec<LibraryTrack>,
}

const CACHE_VERSION: u32 = 1;

fn get_cache_path(library_dir: &Path) -> Result<PathBuf> {
    // Use a hash of the library path to create a unique cache filename
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    library_dir.canonicalize()?.hash(&mut hasher);
    let hash = hasher.finish();

    let cache_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".ferric")
        .join("cache");

    fs::create_dir_all(&cache_dir)?;
    Ok(cache_dir.join(format!("library_{:x}.json", hash)))
}

fn get_library_mtime(library_dir: &Path) -> Result<std::time::SystemTime> {
    // Get the most recent modification time in the library
    let mut latest = std::time::SystemTime::UNIX_EPOCH;

    for entry in WalkDir::new(library_dir)
        .max_depth(5) // Only check first few levels for performance
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if let Ok(metadata) = entry.metadata() {
            if let Ok(modified) = metadata.modified() {
                if modified > latest {
                    latest = modified;
                }
            }
        }
    }

    Ok(latest)
}

fn load_cached_index(library_dir: &Path) -> Option<Vec<LibraryTrack>> {
    let cache_path = get_cache_path(library_dir).ok()?;

    if !cache_path.exists() {
        return None;
    }

    let cache_data = fs::read_to_string(&cache_path).ok()?;
    let cache: LibraryCache = serde_json::from_str(&cache_data).ok()?;

    // Verify cache version
    if cache.version != CACHE_VERSION {
        logger::info("Cache version mismatch, rebuilding index...");
        return None;
    }

    // Check if library has been modified
    let current_mtime = get_library_mtime(library_dir).ok()?;
    if current_mtime > cache.last_modified {
        logger::info("Library modified since cache, rebuilding index...");
        return None;
    }

    logger::success(&format!("Loaded {} tracks from cache", cache.tracks.len()));
    Some(cache.tracks)
}

fn save_cache(library_dir: &Path, tracks: &[LibraryTrack]) -> Result<()> {
    let cache_path = get_cache_path(library_dir)?;
    let last_modified = get_library_mtime(library_dir)?;

    let cache = LibraryCache {
        version: CACHE_VERSION,
        library_path: library_dir.to_path_buf(),
        track_count: tracks.len(),
        last_modified,
        tracks: tracks.to_vec(),
    };

    let cache_json = serde_json::to_string(&cache)?;
    fs::write(&cache_path, cache_json)?;

    logger::info(&format!("Cached index to {}", cache_path.display()));
    Ok(())
}

fn build_library_index(
    library_dir: &Path,
    verbose: bool,
) -> Result<Vec<LibraryTrack>> {
    // Try to load from cache first
    if let Some(tracks) = load_cached_index(library_dir) {
        return Ok(tracks);
    }

    logger::info("Indexing local audio library (parallel)...");

    // Collect all audio file paths first
    let audio_files: Vec<PathBuf> = WalkDir::new(library_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|path| utils::is_audio_file(path))
        .collect();

    logger::info(&format!("Found {} audio files, reading metadata...", audio_files.len()));

    // Process files in parallel using rayon
    let tracks: Vec<LibraryTrack> = audio_files
        .par_iter()
        .filter_map(|path| {
            match AudioMetadata::from_file(path) {
                Ok(metadata) => {
                    let track_artist = metadata.get_organizing_artist(true);
                    let album_artist = metadata.get_organizing_artist(false);

                    let mut artist_variants = expand_artist_keys(&track_artist);
                    if album_artist != track_artist {
                        for variant in expand_artist_keys(&album_artist) {
                            if !artist_variants.contains(&variant) {
                                artist_variants.push(variant);
                            }
                        }
                    }

                    if artist_variants.is_empty() {
                        if verbose {
                            eprintln!("Skipping {} due to empty artist metadata", path.display());
                        }
                        return None;
                    }

                    let title_variants = expand_title_variants(&metadata.get_title(), &artist_variants);
                    if title_variants.is_empty() {
                        if verbose {
                            eprintln!("Skipping {} due to empty title metadata", path.display());
                        }
                        return None;
                    }

                    if verbose {
                        eprintln!(
                            "Indexed '{}' - '{}' ({})",
                            artist_variants.first().unwrap_or(&"?".to_string()),
                            title_variants.first().unwrap_or(&"?".to_string()),
                            path.display()
                        );
                    }

                    Some(LibraryTrack {
                        path: path.clone(),
                        artist_variants,
                        title_variants,
                    })
                }
                Err(err) => {
                    eprintln!("Failed to read metadata from {}: {}", path.display(), err);
                    None
                }
            }
        })
        .collect();

    logger::success(&format!("Indexed {} tracks", tracks.len()));

    // Save to cache for next time
    if let Err(e) = save_cache(library_dir, &tracks) {
        logger::warning(&format!("Failed to save cache: {}", e));
    }

    Ok(tracks)
}

/// URL-encode a string for use in M3U playlists
fn url_encode(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '~' || c == '/' {
                c.to_string()
            } else {
                // URL encode the character
                c.encode_utf8(&mut [0; 4])
                    .bytes()
                    .map(|b| format!("%{:02X}", b))
                    .collect::<String>()
            }
        })
        .collect()
}

fn write_m3u(paths: &[PathBuf], output: &Path) -> Result<()> {
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create parent directory {}", parent.display()))?;
    }

    let mut file = File::create(output)
        .with_context(|| format!("Failed to create .m3u file at {}", output.display()))?;
    writeln!(file, "#EXTM3U")?;

    for path in paths {
        // Try to get duration and metadata for #EXTINF line
        let mut duration = -1;
        let mut title = String::new();

        if let Ok(metadata) = AudioMetadata::from_file(path) {
            duration = metadata.duration_secs.unwrap_or(0.0).round() as i32;

            // Build title as "Artist - Title"
            let artist = metadata.artist.as_deref().unwrap_or("Unknown Artist");
            let track_title = metadata.title.as_deref().unwrap_or("Unknown Title");
            title = format!("{} - {}", artist, track_title);
        }

        // Write #EXTINF line if we have metadata
        if !title.is_empty() {
            writeln!(file, "#EXTINF:{},{}", duration, url_encode(&title))?;
        }

        // Write the file path - use just the filename (relative path)
        if let Some(filename) = path.file_name() {
            let filename_str = filename.to_string_lossy();
            writeln!(file, "{}", url_encode(&filename_str))?;
        } else {
            // Fallback to absolute path if no filename (shouldn't happen)
            let absolute = fs::canonicalize(path).unwrap_or_else(|_| path.clone());
            writeln!(file, "{}", url_encode(&absolute.to_string_lossy()))?;
        }
    }

    Ok(())
}

fn default_output_path(csv_path: &Path) -> PathBuf {
    let mut path = csv_path.to_path_buf();
    path.set_extension("m3u");
    path
}

fn expand_artist_keys(raw: &str) -> Vec<String> {
    let normalized = utils::normalize_for_comparison(raw);
    if normalized.is_empty() {
        return Vec::new();
    }

    let mut seen = HashSet::new();
    let mut variants = Vec::new();
    let mut push_variant = |value: &str| {
        if !value.is_empty() && seen.insert(value.to_string()) {
            variants.push(value.to_string());
        }
    };

    push_variant(&normalized);

    let mut working = normalized.clone();
    for pattern in &[" feat ", " featuring ", " ft ", " with ", " and "] {
        working = working.replace(pattern, "|");
    }
    for sep in [",", ";", "/", "&", "+"] {
        working = working.replace(sep, "|");
    }

    for token in working.split('|') {
        let trimmed = token.trim();
        if !trimmed.is_empty() {
            push_variant(trimmed);
        }
    }

    variants
}

fn expand_title_variants(raw_title: &str, artist_keys: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut variants = Vec::new();
    let mut push_variant = |value: String| {
        if !value.is_empty() && seen.insert(value.clone()) {
            variants.push(value);
        }
    };

    // CRITICAL: Strip suffixes from RAW title BEFORE normalization
    // This preserves dashes so we can detect patterns like " - Remaster"
    let mut working_raw = raw_title.to_string();

    // Common patterns to strip (case-insensitive)
    let patterns_to_strip = [
        " - Remaster",
        " - Remastered",
        " - 2007 Remaster",
        " - 2009 Remaster",
        " - 2011 Remaster",
        " - 2013 Remaster",
        " - 2014 Remaster",
        " - 2015 Remaster",
        " - 2018 Remaster",
        " - 2024 Remaster",
        " - Remix",
        " - Radio Edit",
        " - Album Version",
        " - Single Version",
        " - Extended Version",
        " - Live",
        " - Acoustic",
        " - Demo",
        " - Edit",
        " - Mix",
        " - Version",
    ];

    let lower = working_raw.to_lowercase();
    for pattern in &patterns_to_strip {
        if let Some(pos) = lower.rfind(&pattern.to_lowercase()) {
            working_raw = working_raw[..pos].to_string();
            break; // Only strip one suffix
        }
    }

    // Also try to strip anything after the last " - " if it contains certain keywords
    if let Some(last_dash) = working_raw.rfind(" - ") {
        let after_dash = working_raw[last_dash + 3..].to_lowercase();
        let suspicious_keywords = [
            "remaster", "remix", "edit", "version", "live", "acoustic",
            "radio", "album", "single", "vocal", "instrumental", "feat",
            "ft", "featuring", "with", "explicit", "clean", "demo"
        ];

        if suspicious_keywords.iter().any(|kw| after_dash.contains(kw)) {
            working_raw = working_raw[..last_dash].to_string();
        }
    }

    // NOW normalize the cleaned raw title
    let mut normalized = utils::normalize_for_comparison(&working_raw);
    if normalized.is_empty() {
        return Vec::new();
    }

    // Add the main normalized variant
    push_variant(normalized.clone());

    // Strip artist prefix if present
    for artist in artist_keys {
        let prefix = format!("{} ", artist);
        if normalized.starts_with(&prefix) {
            let stripped = normalized[prefix.len()..].trim().to_string();
            if !stripped.is_empty() {
                push_variant(stripped.clone());
                normalized = stripped; // Use this for further processing
                break;
            }
        }
    }

    // Strip parentheticals and brackets from normalized version
    let mut cleaned = remove_parentheticals(&normalized);
    cleaned = remove_brackets(&cleaned);
    let cleaned = cleaned.trim().to_string();
    if !cleaned.is_empty() && cleaned != normalized {
        push_variant(cleaned);
    }

    variants
}

fn remove_parentheticals(s: &str) -> String {
    let mut result = String::new();
    let mut depth: i32 = 0;

    for c in s.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ => {
                if depth == 0 {
                    result.push(c);
                }
            }
        }
    }

    result.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn remove_brackets(s: &str) -> String {
    let mut result = String::new();
    let mut depth: i32 = 0;

    for c in s.chars() {
        match c {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            _ => {
                if depth == 0 {
                    result.push(c);
                }
            }
        }
    }

    result.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn build_exact_match_index(library: &[LibraryTrack]) -> HashMap<(String, String), Vec<usize>> {
    let mut index: HashMap<(String, String), Vec<usize>> = HashMap::new();

    for (track_idx, track) in library.iter().enumerate() {
        for artist in &track.artist_variants {
            for title in &track.title_variants {
                let key = (artist.clone(), title.clone());
                index.entry(key).or_insert_with(Vec::new).push(track_idx);
            }
        }
    }

    index
}

fn find_matches(
    entry: &PlaylistEntry,
    library: &[LibraryTrack],
    exact_match_index: &HashMap<(String, String), Vec<usize>>,
    _verbose: bool,
) -> Vec<MatchCandidate> {
    let mut candidates: Vec<MatchCandidate> = Vec::new();
    const EXACT_MATCH_THRESHOLD: f64 = 1.0;
    const FUZZY_MATCH_THRESHOLD: f64 = 0.85;
    const MIN_SCORE_THRESHOLD: f64 = 0.75;

    let mut exact_matched_indices = HashSet::new();

    // FAST PATH: Try exact matches first using the HashMap index
    for entry_artist in &entry.artist_keys {
        for entry_title in &entry.title_keys {
            let key = (entry_artist.clone(), entry_title.clone());
            if let Some(track_indices) = exact_match_index.get(&key) {
                for &track_idx in track_indices {
                    if exact_matched_indices.insert(track_idx) {
                        candidates.push(MatchCandidate {
                            path: library[track_idx].path.clone(),
                            score: EXACT_MATCH_THRESHOLD,
                            match_type: "exact".to_string(),
                        });
                    }
                }
            }
        }
    }

    // If we found exact matches, return them immediately
    if !candidates.is_empty() {
        return candidates;
    }

    // SLOW PATH: Fall back to fuzzy matching only if no exact matches found
    for (track_idx, track) in library.iter().enumerate() {
        let mut best_score = 0.0;
        let mut match_type = String::new();

        // Fuzzy match on both artist and title
        for entry_artist in &entry.artist_keys {
            for entry_title in &entry.title_keys {
                for track_artist in &track.artist_variants {
                    for track_title in &track.title_variants {
                        let artist_sim = jaro_winkler(entry_artist, track_artist);
                        let title_sim = jaro_winkler(entry_title, track_title);

                        // Combined score (weighted average: title is more important)
                        let combined_score = (title_sim * 0.7) + (artist_sim * 0.3);

                        if combined_score > best_score && combined_score >= FUZZY_MATCH_THRESHOLD {
                            best_score = combined_score;
                            match_type = format!("fuzzy (A:{:.2}, T:{:.2})", artist_sim, title_sim);
                        }
                    }
                }
            }
        }

        // If no good artist+title match, try title-only with artist verification
        if best_score < FUZZY_MATCH_THRESHOLD {
            for entry_title in &entry.title_keys {
                for track_title in &track.title_variants {
                    let title_sim = jaro_winkler(entry_title, track_title);

                    if title_sim >= FUZZY_MATCH_THRESHOLD {
                        // Verify artist has some similarity
                        let mut artist_match = false;
                        for entry_artist in &entry.artist_keys {
                            for track_artist in &track.artist_variants {
                                if jaro_winkler(entry_artist, track_artist) > 0.7 {
                                    artist_match = true;
                                    break;
                                }
                            }
                            if artist_match {
                                break;
                            }
                        }

                        if artist_match && title_sim > best_score {
                            best_score = title_sim * 0.9; // Slightly lower confidence
                            match_type = format!("title-fuzzy ({:.2})", title_sim);
                        }
                    }
                }
            }
        }

        if best_score >= MIN_SCORE_THRESHOLD {
            candidates.push(MatchCandidate {
                path: track.path.clone(),
                score: best_score,
                match_type,
            });
        }
    }

    // Sort by score descending
    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // If we have a perfect match, return only that
    if !candidates.is_empty() && candidates[0].score >= EXACT_MATCH_THRESHOLD {
        return vec![candidates[0].clone()];
    }

    // Return top candidates (limit to avoid overwhelming user)
    const MAX_CANDIDATES: usize = 5;
    candidates.truncate(MAX_CANDIDATES);

    // Filter to only show candidates with similar scores (within 0.05 of best)
    if let Some(best) = candidates.first() {
        let cutoff = best.score - 0.05;
        candidates.retain(|c| c.score >= cutoff);
    }

    candidates
}

fn prompt_user_choice(max_choice: usize) -> Result<usize> {
    loop {
        print!("Enter choice (0-{}): ", max_choice);
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;

        match input.trim().parse::<usize>() {
            Ok(choice) if choice <= max_choice => return Ok(choice),
            _ => {
                println!("Invalid choice. Please enter a number between 0 and {}.", max_choice);
            }
        }
    }
}
