use crate::logger;
use crate::metadata::AudioMetadata;
use crate::utils;
use anyhow::{anyhow, Context, Result};
use csv::ReaderBuilder;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
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

    let (artist_title_index, title_index, artist_lookup) =
        build_library_index(&options.library_dir, options.verbose)?;

    let mut matched_paths = Vec::with_capacity(entries.len());
    let mut missing = Vec::new();

    for entry in &entries {
        let mut matched: Option<(PathBuf, &'static str)> = None;

        'outer: for artist_key in &entry.artist_keys {
            for title_key in &entry.title_keys {
                let lookup_key = (artist_key.clone(), title_key.clone());
                if let Some(paths) = artist_title_index.get(&lookup_key) {
                    matched = Some((paths[0].clone(), "artist-title"));
                    break 'outer;
                }
            }
        }

        if matched.is_none() {
            for title_key in &entry.title_keys {
                if let Some(paths) = title_index.get(title_key) {
                    if paths.len() == 1 {
                        let candidate = &paths[0];
                        if let Some(artists) = artist_lookup.get(candidate) {
                            if artists
                                .iter()
                                .any(|artist| entry.artist_keys.contains(artist))
                            {
                                matched = Some((candidate.clone(), "title-only"));
                                break;
                            }
                        }
                    }
                }
            }
        }

        if let Some((path, strategy)) = matched {
            logger::debug(
                &format!(
                    "Matched '{}' - '{}' to {} ({})",
                    entry.artist_raw,
                    entry.title_raw,
                    path.display(),
                    strategy
                ),
                options.verbose,
            );
            matched_paths.push(path);
        } else {
            missing.push(format!("{} - {}", entry.artist_raw, entry.title_raw));
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

fn build_library_index(
    library_dir: &Path,
    verbose: bool,
) -> Result<(
    HashMap<(String, String), Vec<PathBuf>>,
    HashMap<String, Vec<PathBuf>>,
    HashMap<PathBuf, Vec<String>>,
)> {
    logger::info("Indexing local audio library...");
    let mut map: HashMap<(String, String), Vec<PathBuf>> = HashMap::new();
    let mut title_map: HashMap<String, Vec<PathBuf>> = HashMap::new();
    let mut artist_lookup: HashMap<PathBuf, Vec<String>> = HashMap::new();

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

                artist_lookup.insert(path.clone(), artist_variants.clone());

                let title_variants = expand_title_variants(&metadata.get_title(), &artist_variants);
                if title_variants.is_empty() {
                    logger::debug(
                        &format!("Skipping {} due to empty title metadata", path.display()),
                        verbose,
                    );
                    continue;
                }

                for title in &title_variants {
                    let entry = title_map.entry(title.clone()).or_insert_with(Vec::new);
                    push_unique(entry, &path);
                }

                for artist in &artist_variants {
                    for title in &title_variants {
                        logger::debug(
                            &format!(
                                "Indexed track '{}' - '{}' ({})",
                                artist,
                                title,
                                path.display()
                            ),
                            verbose,
                        );
                        let entry = map
                            .entry((artist.clone(), title.clone()))
                            .or_insert_with(Vec::new);
                        push_unique(entry, &path);
                    }
                }
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

    logger::success(&format!("Indexed {} unique artist/title pairs", map.len()));
    Ok((map, title_map, artist_lookup))
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
    let normalized = utils::normalize_for_comparison(raw_title);
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

    push_variant(normalized.clone());

    for artist in artist_keys {
        let prefix = format!("{} ", artist);
        if normalized.starts_with(&prefix) {
            let stripped = normalized[prefix.len()..].trim().to_string();
            if !stripped.is_empty() {
                push_variant(stripped);
            }
        }
    }

    variants
}

fn push_unique(vec: &mut Vec<PathBuf>, path: &Path) {
    if !vec.iter().any(|existing| existing == path) {
        vec.push(path.to_path_buf());
    }
}
