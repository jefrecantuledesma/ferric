use crate::logger;
use crate::metadata::AudioMetadata;
use crate::utils;
use anyhow::{Context, Result};
use base64::{engine::general_purpose, Engine as _};
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use walkdir::WalkDir;

pub struct FixMetadataOptions {
    pub input_dir: PathBuf,
    pub check_artist: bool,
    pub check_album: bool,
    pub check_cover: bool,
    pub dry_run: bool,
    pub verbose: bool,
}

#[derive(Debug, Clone)]
struct FileMetadataInfo {
    path: PathBuf,
    metadata: AudioMetadata,
    has_cover: bool,
    parent_dir: PathBuf,
}

/// Check if a file has an embedded album cover
fn has_album_cover(path: &Path) -> Result<bool> {
    let output = Command::new("ffprobe")
        .args(&[
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_streams",
        ])
        .arg(path)
        .output()
        .context("Failed to run ffprobe")?;

    if !output.status.success() {
        return Ok(false);
    }

    let json_str = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&json_str)?;

    // Check if any stream has attached_pic disposition
    if let Some(streams) = json.get("streams").and_then(|s| s.as_array()) {
        for stream in streams {
            if let Some(disposition) = stream.get("disposition") {
                if let Some(attached_pic) = disposition.get("attached_pic") {
                    if attached_pic.as_i64() == Some(1) {
                        return Ok(true);
                    }
                }
            }
        }
    }

    Ok(false)
}

/// Prompt user for text input
fn prompt_for_text(prompt: &str) -> Result<String> {
    print!("{}", prompt);
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;

    Ok(input.trim().to_string())
}

/// Prompt user for file path
fn prompt_for_file(prompt: &str) -> Result<PathBuf> {
    loop {
        print!("{}", prompt);
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let path_str = input.trim();

        if path_str.is_empty() {
            logger::warning("Empty path provided, skipping...");
            return Err(anyhow::anyhow!("User skipped"));
        }

        let path = PathBuf::from(path_str);

        if !path.exists() {
            logger::error(&format!("File not found: {}", path.display()));
            logger::info("Please enter a valid file path or press Ctrl+C to cancel");
            continue;
        }

        // Check if it's an image file
        if let Some(ext) = path.extension() {
            let ext_lower = ext.to_string_lossy().to_lowercase();
            if ext_lower == "jpg" || ext_lower == "jpeg" || ext_lower == "png" {
                return Ok(path);
            } else {
                logger::error(&format!("Invalid image format: .{}", ext_lower));
                logger::info("Please provide a .jpg, .jpeg, or .png file");
                continue;
            }
        } else {
            logger::error("File has no extension");
            continue;
        }
    }
}

/// Create METADATA_BLOCK_PICTURE tag for OPUS/OGG files
/// Reference: https://wiki.xiph.org/VorbisComment#Cover_art
fn create_metadata_block_picture(cover_path: &Path) -> Result<String> {
    // Read the image file
    let image_data = fs::read(cover_path)
        .context("Failed to read cover image")?;

    // Determine MIME type from extension
    let mime_type = match utils::get_extension(cover_path).as_deref() {
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("png") => "image/png",
        _ => return Err(anyhow::anyhow!("Unsupported image format")),
    };

    // Get image dimensions using ffprobe
    let probe_output = Command::new("ffprobe")
        .args(&[
            "-v", "quiet",
            "-print_format", "json",
            "-show_streams",
        ])
        .arg(cover_path)
        .output()
        .context("Failed to run ffprobe on cover image")?;

    let probe_json: serde_json::Value = serde_json::from_slice(&probe_output.stdout)?;
    let width = probe_json["streams"][0]["width"].as_u64().unwrap_or(0) as u32;
    let height = probe_json["streams"][0]["height"].as_u64().unwrap_or(0) as u32;

    // Build METADATA_BLOCK_PICTURE structure
    let mut picture_data = Vec::new();

    // Picture type (3 = front cover)
    picture_data.extend_from_slice(&3u32.to_be_bytes());

    // MIME type length and data
    picture_data.extend_from_slice(&(mime_type.len() as u32).to_be_bytes());
    picture_data.extend_from_slice(mime_type.as_bytes());

    // Description (empty)
    picture_data.extend_from_slice(&0u32.to_be_bytes());

    // Width, height, color depth, colors used
    picture_data.extend_from_slice(&width.to_be_bytes());
    picture_data.extend_from_slice(&height.to_be_bytes());
    picture_data.extend_from_slice(&24u32.to_be_bytes()); // 24-bit color depth
    picture_data.extend_from_slice(&0u32.to_be_bytes());  // 0 for non-indexed

    // Image data length and data
    picture_data.extend_from_slice(&(image_data.len() as u32).to_be_bytes());
    picture_data.extend_from_slice(&image_data);

    // Base64 encode
    Ok(general_purpose::STANDARD.encode(&picture_data))
}

/// Embed album cover into audio file using ffmpeg
fn embed_cover(audio_path: &Path, cover_path: &Path, dry_run: bool) -> Result<()> {
    if dry_run {
        return Ok(());
    }

    // Create temporary output file with proper extension
    let mut temp_path = audio_path.to_path_buf();
    let original_filename = temp_path.file_name().unwrap().to_string_lossy().to_string();
    temp_path.set_file_name(format!("{}.tmp", original_filename));

    // Get file extension to determine codec copy parameters
    let ext = utils::get_extension(audio_path).unwrap_or_default();

    // For OPUS/OGG files, we need to use METADATA_BLOCK_PICTURE
    let result = if ext == "opus" || ext == "ogg" {
        // OPUS/OGG: Use METADATA_BLOCK_PICTURE tag
        let metadata_tag = create_metadata_block_picture(cover_path)?;

        Command::new("ffmpeg")
            .args(&[
                "-i",
                audio_path.to_str().unwrap(),
                "-c:a",
                "copy",
                "-metadata",
                &format!("METADATA_BLOCK_PICTURE={}", metadata_tag),
                "-f",
                &ext, // Explicitly specify output format
                "-y",
                temp_path.to_str().unwrap(),
            ])
            .output()
    } else {
        // For MP3, M4A, FLAC, etc.
        Command::new("ffmpeg")
            .args(&[
                "-i",
                audio_path.to_str().unwrap(),
                "-i",
                cover_path.to_str().unwrap(),
                "-map",
                "0",
                "-map",
                "1",
                "-c",
                "copy",
                "-disposition:v:0",
                "attached_pic",
                "-y",
                temp_path.to_str().unwrap(),
            ])
            .output()
    };

    let output = result.context("Failed to run ffmpeg for cover embedding")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("ffmpeg failed: {}", stderr));
    }

    // Replace original with temp file
    fs::rename(&temp_path, audio_path)
        .context("Failed to replace original file with updated version")?;

    Ok(())
}

/// Update text metadata using ffmpeg
fn update_metadata(audio_path: &Path, artist: Option<&str>, album: Option<&str>, dry_run: bool) -> Result<()> {
    if dry_run {
        return Ok(());
    }

    // Create temporary output file with proper extension
    let mut temp_path = audio_path.to_path_buf();
    let original_filename = temp_path.file_name().unwrap().to_string_lossy().to_string();
    temp_path.set_file_name(format!("{}.tmp", original_filename));

    let mut args = vec![
        "-i".to_string(),
        audio_path.to_str().unwrap().to_string(),
        "-c".to_string(),
        "copy".to_string(),
    ];

    // Add metadata arguments
    if let Some(a) = artist {
        args.push("-metadata".to_string());
        args.push(format!("artist={}", a));
        args.push("-metadata".to_string());
        args.push(format!("ARTIST={}", a));
        args.push("-metadata".to_string());
        args.push(format!("album_artist={}", a));
        args.push("-metadata".to_string());
        args.push(format!("ALBUM_ARTIST={}", a));
    }

    if let Some(a) = album {
        args.push("-metadata".to_string());
        args.push(format!("album={}", a));
        args.push("-metadata".to_string());
        args.push(format!("ALBUM={}", a));
    }

    args.push("-y".to_string());
    args.push(temp_path.to_str().unwrap().to_string());

    let output = Command::new("ffmpeg")
        .args(&args)
        .output()
        .context("Failed to run ffmpeg for metadata update")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("ffmpeg failed: {}", stderr));
    }

    // Replace original with temp file
    fs::rename(&temp_path, audio_path)
        .context("Failed to replace original file with updated version")?;

    Ok(())
}

/// Run the fix-metadata operation
pub fn run(options: FixMetadataOptions) -> Result<()> {
    logger::stage("Starting metadata fix operation");
    logger::info(&format!("Input directory: {}", options.input_dir.display()));

    if !options.check_artist && !options.check_album && !options.check_cover {
        return Err(anyhow::anyhow!(
            "No metadata checks specified. Use --artist, --album, or --cover"
        ));
    }

    let mut checks = vec![];
    if options.check_artist {
        checks.push("artist");
    }
    if options.check_album {
        checks.push("album");
    }
    if options.check_cover {
        checks.push("cover");
    }
    logger::info(&format!("Checking for missing: {}", checks.join(", ")));

    if options.dry_run {
        logger::warning("DRY RUN MODE - No files will be modified");
    }

    // Collect all audio files
    logger::info("Scanning for audio files...");
    let files: Vec<PathBuf> = WalkDir::new(&options.input_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .filter(|p| utils::is_audio_file(p))
        .collect();

    logger::info(&format!("Found {} audio files", files.len()));

    // Parallel metadata extraction with progress bar
    logger::info("Analyzing metadata...");
    let pb = ProgressBar::new(files.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] [{bar:40}] {pos}/{len} ({eta}) | Analyzing metadata...")
            .unwrap()
            .progress_chars("█▓▒░"),
    );

    let file_info_list: Arc<Mutex<Vec<FileMetadataInfo>>> = Arc::new(Mutex::new(Vec::new()));

    files.par_iter().for_each(|file_path| {
        let metadata = match AudioMetadata::from_file(file_path) {
            Ok(m) => m,
            Err(_) => {
                pb.inc(1);
                return;
            }
        };

        let has_cover = if options.check_cover {
            has_album_cover(file_path).unwrap_or(false)
        } else {
            true
        };

        let parent_dir = file_path.parent().unwrap_or(Path::new("")).to_path_buf();

        let has_issues = (options.check_artist && metadata.artist.is_none())
            || (options.check_album && metadata.album.is_none())
            || (options.check_cover && !has_cover);

        if has_issues {
            let info = FileMetadataInfo {
                path: file_path.clone(),
                metadata,
                has_cover,
                parent_dir,
            };
            file_info_list.lock().unwrap().push(info);
        }

        pb.inc(1);
    });

    pb.finish_with_message("Analysis complete");

    let file_info_list = Arc::try_unwrap(file_info_list).unwrap().into_inner().unwrap();

    if file_info_list.is_empty() {
        logger::success("No missing metadata found!");
        return Ok(());
    }

    logger::warning(&format!(
        "Found {} files with missing metadata",
        file_info_list.len()
    ));

    // ===== PROCESS ALBUM COVERS (grouped by album metadata) =====
    if options.check_cover {
        logger::stage("\n==================== ALBUM COVERS ====================");

        // Group by album metadata (artist + album)
        let mut albums: HashMap<(String, String), Vec<FileMetadataInfo>> = HashMap::new();
        for info in &file_info_list {
            if !info.has_cover {
                let artist = info.metadata.get_organizing_artist(false);
                let album = info.metadata.get_album();
                albums.entry((artist, album)).or_insert_with(Vec::new).push(info.clone());
            }
        }

        logger::info(&format!("Found {} albums missing covers", albums.len()));

        for ((artist, album), album_files) in albums.iter() {
            logger::info("\n----------------------------------------");
            logger::warning(&format!("Album: {} - {}", artist, album));
            logger::info(&format!("  Files: {} tracks", album_files.len()));
            logger::info("  Example files:");
            for file in album_files.iter().take(3) {
                logger::info(&format!("    - {}", file.path.file_name().unwrap().to_string_lossy()));
            }

            // Prompt for cover
            logger::info("\nEnter path to album cover (jpg/png), or press Enter to skip:");
            match prompt_for_file("Cover path: ") {
                Ok(cover_path) => {
                    logger::info(&format!("Embedding cover into {} files...", album_files.len()));

                    if options.dry_run {
                        logger::info(&format!("Would embed {} into {} files", cover_path.display(), album_files.len()));
                    } else {
                        let pb = ProgressBar::new(album_files.len() as u64);
                        pb.set_style(
                            ProgressStyle::default_bar()
                                .template("[{elapsed_precise}] [{bar:40}] {pos}/{len} ({eta}) | Embedding covers...")
                                .unwrap()
                                .progress_chars("█▓▒░"),
                        );

                        let success_count = Arc::new(Mutex::new(0));
                        let error_count = Arc::new(Mutex::new(0));

                        album_files.par_iter().for_each(|info| {
                            match embed_cover(&info.path, &cover_path, options.dry_run) {
                                Ok(_) => {
                                    *success_count.lock().unwrap() += 1;
                                }
                                Err(e) => {
                                    *error_count.lock().unwrap() += 1;
                                    if options.verbose {
                                        logger::error(&format!("  ✗ {}: {}", info.path.display(), e));
                                    }
                                }
                            }
                            pb.inc(1);
                        });

                        pb.finish_with_message("Embedding complete");

                        let success = *success_count.lock().unwrap();
                        let errors = *error_count.lock().unwrap();

                        logger::success(&format!("✓ Embedded cover in {}/{} files", success, album_files.len()));
                        if errors > 0 {
                            logger::warning(&format!("✗ Failed: {} files", errors));
                        }
                    }
                }
                Err(_) => {
                    logger::info("Skipped album");
                }
            }
        }
    }

    // ===== PROCESS ARTIST/ALBUM METADATA (grouped by folder) =====
    if options.check_artist || options.check_album {
        logger::stage("\n==================== ARTIST/ALBUM METADATA ====================");

        // Group files by folder (parent directory)
        let mut folders: HashMap<PathBuf, Vec<FileMetadataInfo>> = HashMap::new();
        for info in &file_info_list {
            let missing_artist = options.check_artist && info.metadata.artist.is_none();
            let missing_album = options.check_album && info.metadata.album.is_none();

            if missing_artist || missing_album {
                folders.entry(info.parent_dir.clone()).or_insert_with(Vec::new).push(info.clone());
            }
        }

        logger::info(&format!("Found {} folders with missing artist/album metadata", folders.len()));

        for (folder, folder_files) in folders.iter() {
            logger::info("\n----------------------------------------");
            logger::warning(&format!("Folder: {}", folder.display()));
            logger::info(&format!("  Files: {} tracks", folder_files.len()));

            // Show what's missing
            let missing_artist_count = folder_files.iter().filter(|f| f.metadata.artist.is_none()).count();
            let missing_album_count = folder_files.iter().filter(|f| f.metadata.album.is_none()).count();

            if missing_artist_count > 0 {
                logger::warning(&format!("  Missing artist: {} files", missing_artist_count));
            }
            if missing_album_count > 0 {
                logger::warning(&format!("  Missing album: {} files", missing_album_count));
            }

            logger::info("  Example files:");
            for file in folder_files.iter().take(3) {
                logger::info(&format!("    - {}", file.path.file_name().unwrap().to_string_lossy()));
            }

            let mut new_artist = None;
            let mut new_album = None;

            if missing_artist_count > 0 && options.check_artist {
                logger::info("");
                match prompt_for_text("Enter artist name for this folder (or press Enter to skip): ") {
                    Ok(input) if !input.is_empty() => {
                        new_artist = Some(input);
                    }
                    _ => {
                        logger::info("Skipped artist");
                    }
                }
            }

            if missing_album_count > 0 && options.check_album {
                logger::info("");
                match prompt_for_text("Enter album name for this folder (or press Enter to skip): ") {
                    Ok(input) if !input.is_empty() => {
                        new_album = Some(input);
                    }
                    _ => {
                        logger::info("Skipped album");
                    }
                }
            }

            // Update metadata if anything was provided
            if new_artist.is_some() || new_album.is_some() {
                logger::info(&format!("Updating metadata for {} files...", folder_files.len()));

                if options.dry_run {
                    logger::info("Would update metadata:");
                    if let Some(ref a) = new_artist {
                        logger::info(&format!("  Artist: {}", a));
                    }
                    if let Some(ref a) = new_album {
                        logger::info(&format!("  Album: {}", a));
                    }
                } else {
                    let pb = ProgressBar::new(folder_files.len() as u64);
                    pb.set_style(
                        ProgressStyle::default_bar()
                            .template("[{elapsed_precise}] [{bar:40}] {pos}/{len} ({eta}) | Updating metadata...")
                            .unwrap()
                            .progress_chars("█▓▒░"),
                    );

                    let success_count = Arc::new(Mutex::new(0));
                    let error_count = Arc::new(Mutex::new(0));

                    let artist_ref = new_artist.as_deref();
                    let album_ref = new_album.as_deref();

                    // Filter files that need updates
                    let files_to_update: Vec<_> = folder_files.iter()
                        .filter(|f| {
                            (artist_ref.is_some() && f.metadata.artist.is_none()) ||
                            (album_ref.is_some() && f.metadata.album.is_none())
                        })
                        .collect();

                    files_to_update.par_iter().for_each(|info| {
                        match update_metadata(&info.path, artist_ref, album_ref, options.dry_run) {
                            Ok(_) => {
                                *success_count.lock().unwrap() += 1;
                            }
                            Err(e) => {
                                *error_count.lock().unwrap() += 1;
                                if options.verbose {
                                    logger::error(&format!("  ✗ {}: {}", info.path.display(), e));
                                }
                            }
                        }
                        pb.inc(1);
                    });

                    pb.finish_with_message("Update complete");

                    let success = *success_count.lock().unwrap();
                    let errors = *error_count.lock().unwrap();

                    logger::success(&format!("✓ Updated {} files", success));
                    if errors > 0 {
                        logger::warning(&format!("✗ Failed: {} files", errors));
                    }
                }
            }
        }
    }

    logger::success("\n==========================================");
    logger::success("Metadata fix operation completed!");
    logger::success("==========================================");
    Ok(())
}
