use crate::logger;
use crate::metadata::AudioMetadata;
use crate::utils;
use anyhow::{anyhow, Context, Result};
use csv::ReaderBuilder;
use std::collections::HashMap;
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
    artist: String,
    title: String,
    normalized_artist: String,
    normalized_title: String,
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

    let index = build_library_index(&options.library_dir, options.verbose)?;

    let mut matched_paths = Vec::with_capacity(entries.len());
    let mut missing = Vec::new();

    for entry in &entries {
        let key = (
            entry.normalized_artist.clone(),
            entry.normalized_title.clone(),
        );
        if let Some(paths) = index.get(&key) {
            let path = &paths[0];
            logger::debug(
                &format!(
                    "Matched '{}' - '{}' to {}",
                    entry.artist,
                    entry.title,
                    path.display()
                ),
                options.verbose,
            );
            matched_paths.push(path.clone());
        } else {
            missing.push(format!("{} - {}", entry.artist, entry.title));
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

        let normalized_artist = utils::normalize_for_comparison(&artist);
        let normalized_title = utils::normalize_for_comparison(&title);

        entries.push(PlaylistEntry {
            artist,
            title,
            normalized_artist,
            normalized_title,
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
) -> Result<HashMap<(String, String), Vec<PathBuf>>> {
    logger::info("Indexing local audio library...");
    let mut map: HashMap<(String, String), Vec<PathBuf>> = HashMap::new();

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
                let artist =
                    utils::normalize_for_comparison(&metadata.get_organizing_artist(false));
                let title = utils::normalize_for_comparison(&metadata.get_title());
                if artist.is_empty() || title.is_empty() {
                    logger::debug(
                        &format!("Skipping {} due to empty metadata", path.display()),
                        verbose,
                    );
                    continue;
                }

                map.entry((artist, title))
                    .or_insert_with(Vec::new)
                    .push(path);
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

    logger::success(&format!("Indexed {} unique tracks", map.len()));
    Ok(map)
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
