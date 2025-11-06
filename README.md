# Ferric - High-Performance Audio Library Organization Tool

A blazingly fast, parallel audio library organizer written in Rust. Ferric helps you manage, organize, convert, and deduplicate your music library with intelligent quality-aware operations.

## Features

- **Intelligent Quality Comparison**: Automatically upgrades files to higher quality versions
- **Smart Codec Scoring**: Understands that OPUS 192kbps > MP3 320kbps in perceptual quality
- **Parallel Processing**: Uses Rayon for concurrent operations on thousands of files
- **Metadata-Based Deduplication**: Finds duplicates by comparing artist/album/title, not just filenames
- **OPUS Conversion**: Convert entire libraries to high-quality OPUS format
- **Tag-Based Organization**: Automatically sorts files into Artist/Album folder structures
- **Name Normalization**: Fixes curly apostrophes, case issues, and whitespace problems
- **Comprehensive Logging**: Detailed logs with colored output and progress bars
- **Dry-Run Mode**: Preview all changes before applying them

## Architecture

```
ferric/
├── src/
│   ├── main.rs                 # CLI entry point with clap
│   ├── config.rs               # TOML configuration system
│   ├── logger.rs               # Colored logging with file output
│   ├── metadata.rs             # Audio metadata extraction (ffprobe + symphonia fallback)
│   ├── quality.rs              # Intelligent quality scoring algorithm
│   ├── utils.rs                # Shared utilities
│   └── operations/
│       ├── convert.rs          # OPUS conversion
│       ├── tag_sort.rs         # Basic metadata-based sorting
│       ├── sort.rs             # Quality-aware sorting with upgrades
│       ├── fix_naming.rs       # Name normalization
│       ├── lowercase.rs        # Lowercase conversion
│       ├── dedupe.rs           # Metadata-based deduplication
│       └── unified.rs          # Combined pipeline operation
└── tests/                      # Unit and integration tests
```

## Installation

### Prerequisites

- Rust 1.70+ (edition 2021)
- ffmpeg and ffprobe (for metadata extraction and conversion)

### Build from Source

```bash
cd ferric
cargo build --release
```

The optimized binary will be at `target/release/ferric`.

## Usage

### Available Commands

```bash
ferric <COMMAND> [OPTIONS]
```

#### Commands

1. **convert** - Convert audio files to OPUS format
2. **tag-sort** - Sort files by metadata into Artist/Album structure
3. **sort** - Sort with intelligent quality comparison (only upgrades)
4. **fix-naming** - Normalize names (apostrophes, case, whitespace)
5. **lowercase** - Convert all names to lowercase
6. **dedupe** - Find and remove duplicate files based on metadata
7. **unified** - Run complete pipeline: convert → sort → normalize
8. **gen-config** - Generate example configuration file

### Global Options

- `--dry-run` - Preview changes without modifying files
- `-v, --verbose` - Show detailed output
- `--config <FILE>` - Use custom config file
- `--log-file <FILE>` - Custom log file location

## Examples

### Basic Operations

#### Sort existing library by metadata
```bash
ferric tag-sort -i ~/Downloads/Music -o ~/Music/Library
```

#### Preview what would change (dry-run)
```bash
ferric tag-sort --dry-run -i ~/Downloads/Music -o ~/Music/Library
```

#### Convert to OPUS and delete originals
```bash
ferric convert -i ~/Music/FLAC -o ~/Music/OPUS --delete-original
```

#### Fix naming issues in place
```bash
ferric fix-naming -i ~/Music/Library
```

#### Find and remove duplicates
```bash
ferric dedupe -d ~/Music/Library
```

### Advanced: Quality-Aware Sorting

The `sort` command only replaces files with higher quality versions:

```bash
ferric sort -i ~/Downloads -o ~/Music/Library
```

This will:
- Analyze metadata to find matching tracks (same artist/album/title)
- Calculate quality scores based on codec efficiency and bitrate
- Only upgrade files that are better quality
- Skip files that are same or lower quality

### Unified Pipeline

Run the complete organization pipeline:

```bash
ferric unified -i ~/Downloads/Music -o ~/Music/Library --destructive
```

This performs:
1. Convert all files to OPUS 192kbps
2. Sort by metadata with quality comparison
3. Normalize all naming

Use `--dry-run` to preview first!

## Quality Scoring Algorithm

Ferric uses an intelligent quality scoring system that considers codec efficiency:

### Lossless Formats
- Score: `10000 + (bitrate / 1000)`
- Always preferred over lossy formats

### Lossy Formats
- Score: `codec_multiplier × bitrate_kbps`

#### Codec Multipliers (configurable)
- OPUS: 1.8× (best modern codec)
- AAC: 1.3× (good modern codec)
- Vorbis/OGG: 1.2×
- MP3: 1.0× (baseline)
- WMA: 0.9× (suboptimal)

### Examples
- FLAC 1411kbps → **11411** (lossless always wins)
- OPUS 192kbps → **346** (192 × 1.8)
- MP3 320kbps → **320** (320 × 1.0)
- AAC 256kbps → **333** (256 × 1.3)

This means OPUS at 192kbps will beat MP3 at 320kbps in perceived quality!

## Configuration

Generate an example config:

```bash
ferric gen-config -o ~/.ferric/ferric.toml
```

Edit the config to customize:

```toml
[general]
threads = 0  # 0 = auto-detect CPU cores
verbose = false

[convert]
opus_bitrate = 192  # kbps
opus_compression = 10  # 0-10, higher = better
delete_original = false

[quality]
lossless_bonus = 10000

[quality.codec_multipliers]
opus = 1.8
aac = 1.3
vorbis = 1.2
mp3 = 1.0
wma = 0.9

[naming]
max_name_length = 128
lowercase = true
prefer_artist = false  # use album_artist instead
include_va = false  # exclude "Various Artists"
```

## Logging

Logs are automatically created at:
- Default: `~/.ferric/logs/ferric_YYYYMMDD_HHMMSS.log`
- Custom: `--log-file <path>`

Logs contain:
- Detailed operation information
- Error messages with context
- Timestamp for each operation
- Summary statistics

Console output includes:
- Color-coded messages (green=success, yellow=warning, red=error, cyan=info, magenta=stage)
- Progress bars with ETA
- Real-time status updates

## Performance

Ferric is designed for speed:

- **Parallel Processing**: Uses Rayon for concurrent file operations
- **Efficient Metadata**: Uses ffprobe with JSON parsing (fast!)
- **Smart Traversal**: WalkDir for optimized filesystem scanning
- **Compiled Binary**: Native code, no runtime overhead

Tested on 107 files: ~0.5 seconds for metadata-based sorting!

## Testing

The project has been tested with:
- 107 diverse MP3 files
- Multiple genres and metadata variations
- Various naming conventions
- Different quality levels

### Test Coverage
- Unit tests for core utilities
- Integration tests with real audio files
- Dry-run mode for safe testing

## Dependencies

### Core
- `clap` - CLI framework
- `anyhow` + `thiserror` - Error handling
- `rayon` - Parallelization
- `walkdir` - Filesystem traversal

### Audio
- `symphonia` - Pure Rust audio metadata (fallback)
- ffprobe - Primary metadata extraction

### UI/Logging
- `indicatif` - Progress bars
- `colored` - Terminal colors
- `chrono` - Timestamps

### Configuration
- `serde` + `toml` - Config file parsing

## Future Enhancements

Potential additions:
- Watch mode for automatic organization
- Database/index for faster duplicate detection
- Playlist management
- Embedded cover art handling
- M3U/M3U8 playlist generation
- Undo/rollback functionality
- Web UI for remote management

## License

MIT License

## Contributing

Contributions welcome! Areas for improvement:
- Additional audio format support
- More sophisticated duplicate detection
- Enhanced metadata correction
- Performance optimizations

## Author

Built with Rust and powered by Claude Code.
