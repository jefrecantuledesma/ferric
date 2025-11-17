# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Ferric is a high-performance audio library organization tool written in Rust that intelligently manages, converts, deduplicates, and organizes music collections. It uses parallel processing (Rayon) and includes a sophisticated quality scoring algorithm that understands codec efficiency (e.g., OPUS 192kbps > MP3 320kbps).

## Build & Run Commands

### Building
```bash
# Development build
cargo build

# Optimized release build (recommended)
cargo build --release

# Build with native CPU optimizations for maximum performance
RUSTFLAGS="-C target-cpu=native" cargo build --release
```

The optimized binary will be at `target/release/ferric`.

### Running Tests
```bash
# Run all tests
cargo test

# Run specific test
cargo test test_name

# Run tests with output
cargo test -- --nocapture

# Run tests in a specific module
cargo test quality::tests
```

### Running the Tool
```bash
# Generate example config
cargo run -- gen-config -o ferric.toml

# Sort audio files (dry-run first!)
cargo run --release -- tag-sort --dry-run -i ~/test_audio -o ~/output

# Convert to OPUS
cargo run --release -- convert -i ~/music -o ~/music_opus --delete-original

# Run complete pipeline
cargo run --release -- unified -i ~/downloads -o ~/library --format opus
```

## Architecture Overview

### Core Components

**Metadata Extraction** (`src/metadata.rs`)
- Primary: ffprobe with JSON output for best compatibility
- Fallback: Symphonia (pure Rust) for when ffprobe unavailable
- Different formats store tags differently (OPUS/OGG in stream tags, MP3/MP4 in format tags)
- Both extraction methods tried sequentially with `.or_else()` pattern

**Quality Scoring Algorithm** (`src/quality.rs`)
- Lossless formats: `lossless_bonus (10000) + bitrate_kbps`
- Lossy formats: `codec_multiplier × bitrate_kbps`
- Default codec multipliers: OPUS=1.8, AAC=1.3, Vorbis=1.2, MP3=1.0, WMA=0.9
- This makes OPUS 192kbps (score: 346) beat MP3 320kbps (score: 320)
- Quality comparison only upgrades, never downgrades unless explicitly requested

**Configuration System** (`src/config.rs`)
- TOML-based configuration with serde
- Load order: CLI arg → `~/.ferric/ferric.toml` → `./ferric.toml` → defaults
- All settings have sensible defaults via `#[serde(default)]` attributes
- `Config::load_or_default()` automatically tries standard locations

**Logging** (`src/logger.rs`)
- Colored terminal output with `colored` crate
- File logs at `~/.ferric/logs/ferric_TIMESTAMP.log`
- Functions: `info()`, `success()`, `warning()`, `error()`, `stage()`
- Progress bars with `indicatif` integrated with Rayon for parallel operations

### Operations Module (`src/operations/`)

Each operation follows a consistent pattern:
1. Options struct with configuration
2. `run()` function returning `Result<OperationStats>` or `Result<()>`
3. Parallel processing with Rayon where applicable
4. Dry-run support throughout

**Key operations:**
- `convert.rs` - Audio format conversion using ffmpeg
- `tag_sort.rs` - Basic metadata-based sorting
- `sort.rs` - Quality-aware sorting with upgrade logic
- `dedupe.rs` - Metadata-based duplicate detection
- `fix_naming.rs` - Normalize apostrophes, case, whitespace
- `fix_metadata.rs` - Interactive metadata repair (artist, album, cover art embedding)
- `unified.rs` - Combined pipeline (sort → convert → normalize)
- `playlist.rs` - Import playlists from Exportify CSV to M3U

### Parallel Processing Pattern

Ferric uses Rayon throughout for performance:

```rust
files.par_iter().for_each(|file| {
    // Work on each file in parallel
});
```

Progress tracking with parallel operations:
```rust
let pb = ProgressBar::new(total);
files.par_iter().for_each(|file| {
    // ... do work ...
    pb.inc(1);
});
pb.finish();
```

## Key Implementation Details

### Audio Format Detection
- Uses both file extension and codec string
- Extension detection for quick checks: `utils::is_audio_file()`
- Codec detection for accuracy: `quality::get_audio_format()`

### Quality-Aware Operations
When implementing features that compare files:
1. Extract metadata with `AudioMetadata::from_file()`
2. Calculate scores with `quality::calculate_quality_score()`
3. Compare with `quality::compare_quality()` or score comparison
4. Only proceed if new file is better quality

### Dry-Run Pattern
All operations support `--dry-run`. Implement as:
```rust
if !options.dry_run {
    fs::copy(source, dest)?;
} else {
    logger::info(&format!("Would copy: {} -> {}", source, dest));
}
```

### Interactive Operations Pattern
The `fix_metadata` operation demonstrates interactive user prompting with intelligent grouping:

**Parallelization:**
- Uses Rayon to scan files in parallel with progress bars
- Embeds covers in parallel across all files in an album
- Updates metadata in parallel within folder groups

**Grouping Strategy:**
- **Album covers**: Groups by album metadata (artist + album name)
  - One prompt per album, applies to all tracks
  - Parallel embedding for speed
- **Artist/Album text metadata**: Groups by folder (parent directory)
  - One prompt per folder, applies to all files in that folder
  - Assumes files in same folder = same album
  - Only updates files actually missing that field

**Interactive Features:**
- Uses `std::io::stdin()` for text input (artist, album names)
- Validates file paths for images (jpg, png) before accepting
- Press Enter to skip any prompt
- Shows file counts and example filenames for context
- Real-time progress bars during parallel operations

**Implementation Details:**
- Embeds album art using ffmpeg with `-disposition:v:0 attached_pic`
- Updates text metadata using `ffmpeg -metadata key=value`
- Also sets `album_artist` when updating artist
- For OPUS/OGG files, adds cover as video stream (format limitation)
- Album covers detected by checking `attached_pic` disposition in streams
- All operations use `Arc<Mutex<T>>` for thread-safe counters

### File Organization Structure
Standard output structure for metadata-based sorting:
```
output_dir/
├── Artist Name/
│   └── Album Name/
│       ├── 01 - Track Title.opus
│       └── 02 - Another Track.opus
```

Naming controlled by `config.naming`:
- `prefer_artist`: Use track artist vs album_artist
- `include_va`: Whether to include "Various Artists" folders
- `lowercase`: Convert all paths to lowercase
- `max_name_length`: Truncate long names

## Dependencies of Note

**Audio Processing:**
- `symphonia` - Pure Rust audio metadata extraction (fallback)
- ffprobe/ffmpeg - Primary metadata extraction and conversion (external)

**Parallelization:**
- `rayon` - Data parallelism for file operations
- `indicatif` - Progress bars with rayon integration

**CLI:**
- `clap` v4 with derive macros for argument parsing
- Subcommand pattern with global options (dry-run, verbose, config)

## Common Development Patterns

### Adding a New Operation
1. Create new file in `src/operations/`
2. Define `*Options` struct with config
3. Implement `pub fn run(options: *Options) -> Result<T>`
4. Add to `src/operations/mod.rs`
5. Add variant to `Commands` enum in `src/main.rs`
6. Wire up in `main()` match statement

### Modifying Quality Algorithm
Quality scoring is centralized in `src/quality.rs`. Key functions:
- `calculate_quality_score()` - Score from metadata
- `get_codec_multiplier()` - Codec efficiency values
- All multipliers configurable via `config.quality.codec_multipliers`

### Error Handling
Uses `anyhow` for application errors and `thiserror` for library errors:
- `anyhow::Result<T>` for operation returns
- `.context()` to add error context
- `anyhow::bail!()` for early error returns

## Configuration System

Config precedence: CLI args > specified config file > `~/.ferric/ferric.toml` > `./ferric.toml` > defaults

Generate example config:
```bash
ferric gen-config -o ~/.ferric/ferric.toml
```

Key config sections in `ferric.toml`:
- `[general]` - Threads, verbosity, log directory
- `[convert]` - Format-specific bitrates and conversion behavior
- `[quality]` - Codec multipliers and lossless bonus
- `[naming]` - File/folder naming conventions

## External Dependencies

**Required:**
- ffmpeg and ffprobe must be in PATH for conversion and metadata extraction
- Checked at runtime with `which` crate

**Optional:**
- Symphonia fallback works without ffprobe but less reliable for some formats
