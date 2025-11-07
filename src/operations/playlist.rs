use crate::logger;
use crate::metadata::AudioMetadata;
use crate::utils;
use anyhow::{anyhow, Context, Result};
use csv::ReaderBuilder;
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
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

    let mut matched_paths = Vec::with_capacity(entries.len());
    let mut missing = Vec::new();

    for entry in &entries {
        let candidates = find_matches(&entry, &library_tracks, options.verbose);

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

#[derive(Debug, Clone)]
struct LibraryTrack {
    path: PathBuf,
    artist_variants: Vec<String>,
    title_variants: Vec<String>,
}

fn build_library_index(
    library_dir: &Path,
    verbose: bool,
) -> Result<Vec<LibraryTrack>> {
    logger::info("Indexing local audio library...");
    let mut tracks = Vec::new();

    for entry in WalkDir::new(library_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let path = entry.into_path();
        if !utils::is_audio_file(&path) {
            continue;
        }

        match AudioMetadata::from_file(&path) {
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
                    logger::debug(
                        &format!("Skipping {} due to empty artist metadata", path.display()),
                        verbose,
                    );
                    continue;
                }

                let title_variants = expand_title_variants(&metadata.get_title(), &artist_variants);
                if title_variants.is_empty() {
                    logger::debug(
                        &format!("Skipping {} due to empty title metadata", path.display()),
                        verbose,
                    );
                    continue;
                }

                logger::debug(
                    &format!(
                        "Indexed '{}' - '{}' ({})",
                        artist_variants.first().unwrap_or(&"?".to_string()),
                        title_variants.first().unwrap_or(&"?".to_string()),
                        path.display()
                    ),
                    verbose,
                );

                tracks.push(LibraryTrack {
                    path,
                    artist_variants,
                    title_variants,
                });
            }
            Err(err) => {
                logger::error(&format!(
                    "Failed to read metadata from {}: {}",
                    path.display(),
                    err
                ));
            }
        }
    }

    logger::success(&format!("Indexed {} tracks", tracks.len()));
    Ok(tracks)
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
        let absolute = fs::canonicalize(path).unwrap_or_else(|_| path.clone());
        writeln!(file, "{}", absolute.to_string_lossy())?;
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
    let mut normalized = utils::normalize_for_comparison(raw_title);
    if normalized.is_empty() {
        return Vec::new();
    }

    let mut seen = HashSet::new();
    let mut variants = Vec::new();
    let mut push_variant = |value: String| {
        if !value.is_empty() && seen.insert(value.clone()) {
            variants.push(value);
        }
    };

    // Original normalized version
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

    // Strip common parentheticals and bracketed content
    // These patterns help remove: (Remastered), [Explicit], (feat. Artist), etc.
    let mut working = normalized.clone();
    working = remove_parentheticals(&working);
    working = remove_brackets(&working);

    let cleaned = working.trim().to_string();
    if !cleaned.is_empty() && cleaned != normalized {
        push_variant(cleaned);
    }

    // Also try stripping everything after " - "
    if let Some(dash_pos) = normalized.find(" - ") {
        let before_dash = normalized[..dash_pos].trim().to_string();
        if !before_dash.is_empty() {
            push_variant(before_dash);
        }
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

fn find_matches(
    entry: &PlaylistEntry,
    library: &[LibraryTrack],
    _verbose: bool,
) -> Vec<MatchCandidate> {
    let mut candidates: Vec<MatchCandidate> = Vec::new();
    const EXACT_MATCH_THRESHOLD: f64 = 1.0;
    const FUZZY_MATCH_THRESHOLD: f64 = 0.85;
    const MIN_SCORE_THRESHOLD: f64 = 0.75;

    for track in library {
        let mut best_score = 0.0;
        let mut match_type = String::new();

        // Try exact artist + title match first
        for entry_artist in &entry.artist_keys {
            for entry_title in &entry.title_keys {
                for track_artist in &track.artist_variants {
                    for track_title in &track.title_variants {
                        // Exact match
                        if entry_artist == track_artist && entry_title == track_title {
                            best_score = EXACT_MATCH_THRESHOLD;
                            match_type = "exact".to_string();
                            break;
                        }

                        // Fuzzy match on both artist and title
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
