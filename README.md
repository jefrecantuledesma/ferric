# Ferric
This tool:
- Organizes your music library into Artist/Album folder structures based on metadata,
- Converts audio files to modern formats (OPUS, AAC, MP3, Vorbis) with intelligent quality awareness,
- Automatically upgrades files to higher quality versions using a sophisticated codec-aware scoring system,
- Finds and removes duplicate tracks across your library based on metadata (not just filenames),
- Fixes missing or incorrect metadata using MusicBrainz and audio fingerprinting,
- Merges multiple music libraries while keeping only the highest quality version of each track,
- Generates .m3u playlists from Spotify Exportify CSV files with fuzzy matching,
- Caches metadata in a SQLite database for blazingly fast repeated operations, and
- Has a 🦀Rust🦀 back-end with parallel processing powered by Rayon.

## Dependencies
- Cargo (Rust toolchain)
- ffmpeg and ffprobe (for audio conversion and metadata extraction)
- fpcalc (optional but recommended, for MusicBrainz fingerprinting)

## Installation
Simply clone this repository, build the release binary with `cargo build --release`, and you're ready to go! The optimized binary will be at `target/release/ferric`.

You can optionally install it to your system with `cargo install --path .` to make it available globally.

## Usage
Ferric is a command-line tool with multiple subcommands. The basic pattern is `ferric <COMMAND> [OPTIONS]`.

To see all available commands, run `ferric --help`.

To see options for a specific command, run `ferric <COMMAND> --help`.

### Common Commands
- `ferric sort -i ~/Downloads/Music -o ~/Music/Library` - Organize files by metadata into Artist/Album folders
- `ferric convert -i ~/Music/FLAC -o ~/Music/OPUS --format opus` - Convert your library to OPUS format
- `ferric dedupe -i ~/Music/Library` - Find and remove duplicate tracks
- `ferric fix-metadata -i ~/Music/Library --all` - Fix missing metadata using MusicBrainz
- `ferric playlist-import --playlist liked.csv --library ~/Music --playlist-folder ~/Playlists` - Generate playlists from Spotify exports
- `ferric unified -i ~/Downloads -o ~/Music/Library` - Run the complete organization pipeline

**Important:** Use the `--dry-run` flag on any command to preview what would happen without making actual changes. This is **highly recommended** before running destructive operations!

## Configuration
Ferric can be configured through a TOML file. Generate an example configuration with:
```bash
ferric gen-config -o ~/.ferric/ferric.toml
```

By default, ferric looks for configuration files in these locations (in order):
1. `~/.ferric/ferric.toml`
2. `./ferric.toml` (current directory)

You can also specify a custom config file with the `--config` flag.

There are five main sections in the `ferric.toml` file:
1. `[general]`
2. `[convert]`
3. `[quality]`
4. `[naming]`
5. `[musicbrainz]`

### [general]
The `[general]` section has three configurable variables:
1. `threads` (integer)
2. `verbose` (boolean)
3. `cache_path` (string)

The `threads` variable sets how many parallel threads to use for operations. If you set this to `0`, ferric will automatically detect and use all available CPU cores. This is the recommended setting for maximum performance. If you want to limit resource usage, you can set it to a specific number like `4` or `8`.

The `verbose` variable controls whether detailed output is shown during operations. Set this to `true` if you want to see everything ferric is doing, or `false` for quieter output. You can always override this with the `-v` or `--verbose` command-line flag.

The `cache_path` variable specifies where the metadata cache database is stored. This database dramatically speeds up repeated operations by storing extracted metadata, audio fingerprints, and MusicBrainz IDs. The default location is `~/.ferric/metadata_cache.db`.

An example of what this would look like in the configuration file would be:
```toml
[general]
threads = 0
verbose = false
cache_path = "~/.ferric/metadata_cache.db"
```

### [convert]
The `[convert]` section has seven variables to configure:
1. `opus_bitrate` (integer)
2. `opus_compression` (integer)
3. `aac_bitrate` (integer)
4. `mp3_bitrate` (integer)
5. `delete_original` (boolean)
6. `always_convert` (boolean)
7. `convert_down` (boolean)

The `opus_bitrate` variable sets the target bitrate in kbps when converting to OPUS format. The recommended value is `192`, which provides excellent quality that's perceptually better than MP3 at 320kbps (thanks to OPUS being a more efficient codec). You could go lower to `128` for smaller files or higher to `256` for audiophile-grade quality.

The `opus_compression` variable controls the compression level for OPUS encoding, ranging from `0` (fastest, largest files) to `10` (slowest, smallest files). Higher compression means better quality at the same bitrate, but takes longer to encode. The recommended value is `10` unless you're in a hurry.

The `aac_bitrate` and `mp3_bitrate` variables work the same way as `opus_bitrate`, setting target bitrates for AAC and MP3 conversions respectively.

The `delete_original` variable determines whether source files are deleted after successful conversion. **Be very careful with this setting!** Set it to `true` only if you're confident in your backup situation. You can always override this per-command with the `--delete-original` flag.

The `always_convert` variable controls whether ferric will convert files even if they're already in the target format. By default, ferric skips files that are already OPUS if you're converting to OPUS, for example. Set this to `true` to force re-encoding everything.

The `convert_down` variable allows ferric to convert higher quality files (like lossless FLAC) down to lossy formats. By default, ferric refuses to downgrade quality. Set this to `true` if you want to convert lossless files to save space.

An example of what this would look like in the configuration file would be:
```toml
[convert]
opus_bitrate = 192
opus_compression = 10
aac_bitrate = 256
mp3_bitrate = 320
delete_original = false
always_convert = false
convert_down = false
```

### [quality]
The `[quality]` section contains ferric's intelligent quality scoring system. This is where the magic happens! Ferric doesn't just look at bitrate - it understands that modern codecs like OPUS are more efficient than older ones like MP3.

There are two parts to this section:
1. `lossless_bonus` (integer)
2. `[quality.codec_multipliers]` (table of codec names to multipliers)

The `lossless_bonus` variable is added to the quality score of any lossless format (FLAC, WAV, ALAC, APE). This ensures that lossless files are **always** preferred over lossy files, regardless of bitrate. The default value is `10000`, which is high enough that even a low-bitrate lossless file will beat any lossy file.

The `[quality.codec_multipliers]` subsection defines multipliers for each lossy codec. These multipliers are based on perceptual quality research and real-world listening tests. The formula is simple:
```
Quality Score = codec_multiplier × bitrate_kbps
```

For lossless formats:
```
Quality Score = 10000 + (bitrate / 1000)
```

Here's what this means in practice:
- OPUS 192kbps → 346 points (192 × 1.8)
- MP3 320kbps → 320 points (320 × 1.0)
- AAC 256kbps → 333 points (256 × 1.3)
- FLAC 1411kbps → 11411 points (always wins)

So ferric knows that OPUS 192kbps sounds better than MP3 320kbps, and will upgrade accordingly! The default multipliers are based on modern codec efficiency research, but you can tweak them if you disagree.

An example of what this would look like in the configuration file would be:
```toml
[quality]
lossless_bonus = 10000

[quality.codec_multipliers]
opus = 1.8
aac = 1.3
vorbis = 1.2
mp3 = 1.0
wma = 0.9
```

### [naming]
The `[naming]` section controls how ferric normalizes file and folder names. There are four variables:
1. `max_name_length` (integer)
2. `lowercase` (boolean)
3. `prefer_artist` (boolean)
4. `include_va` (boolean)

The `max_name_length` variable sets the maximum length for file and folder names. This is useful for filesystem compatibility and keeping things tidy. The default is `128` characters, which should work on all modern filesystems. Names longer than this will be truncated.

The `lowercase` variable controls whether all file and folder names are converted to lowercase. Set this to `true` if you prefer everything lowercase (like "pink floyd/dark side of the moon") or `false` if you want to preserve the original case from metadata (like "Pink Floyd/Dark Side of the Moon").

The `prefer_artist` variable determines which artist field to use for folder organization. By default (`false`), ferric uses the `album_artist` tag, which is more accurate for compilations and collaborative albums. If you set this to `true`, it will use the `artist` tag instead, which could result in the same album appearing under multiple artist folders if tracks have different artists.

The `include_va` variable controls whether "Various Artists" albums are included in the organization. Set this to `false` if you want to exclude "Various Artists" from your folder structure (they'll still be organized, just without the "Various Artists" folder).

An example of what this would look like in the configuration file would be:
```toml
[naming]
max_name_length = 128
lowercase = true
prefer_artist = false
include_va = false
```

### [musicbrainz]
The `[musicbrainz]` section configures the MusicBrainz integration for automatic metadata correction. This is one of ferric's most powerful features! There are three variables:
1. `acoustid_api_key` (string)
2. `confidence_threshold` (float)
3. `user_agent` (string)

The `acoustid_api_key` variable is your API key for the AcoustID service, which is used for audio fingerprinting. You need to get a free API key from https://acoustid.org/api-key if you want to use the metadata fixing features. You can also set this via the `ACOUSTID_API_KEY` environment variable instead of putting it in the config file.

The `confidence_threshold` variable sets the minimum confidence score (from 0.0 to 1.0) required for ferric to automatically apply metadata from MusicBrainz without prompting you. The default is `0.7`, which means matches with 70% confidence or higher are considered reliable. Lower this if you want ferric to be more aggressive with auto-applying metadata, or raise it if you want to review more matches manually.

The `user_agent` variable sets the User-Agent header for MusicBrainz API requests. MusicBrainz requires a descriptive user agent that identifies your application. The default is fine for most users, but you can customize it if you want.

**It is worth noting** that you'll need to obtain your own AcoustID API key to use the metadata fixing features. It's free and takes about 30 seconds to register.

An example of what this would look like in the configuration file would be:
```toml
[musicbrainz]
acoustid_api_key = "your_api_key_here"
confidence_threshold = 0.7
user_agent = "ferric/0.1.0 (https://github.com/yourusername/ferric)"
```

### Example Complete Configuration File
```toml
[general]
threads = 0
verbose = false
cache_path = "~/.ferric/metadata_cache.db"

[convert]
opus_bitrate = 192
opus_compression = 10
aac_bitrate = 256
mp3_bitrate = 320
delete_original = false
always_convert = false
convert_down = false

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
prefer_artist = false
include_va = false

[musicbrainz]
acoustid_api_key = "your_api_key_here"
confidence_threshold = 0.7
user_agent = "ferric/0.1.0 (https://github.com/yourusername/ferric)"
```

## Metadata Cache
Ferric uses a SQLite database to cache metadata, which makes repeated operations **much** faster. The cache stores:
- Audio metadata (artist, album, title, codec, bitrate, etc.)
- Audio fingerprints (for MusicBrainz lookups)
- MusicBrainz IDs (recording and release IDs)

The cache is automatically updated when files change (based on modification time and file size). You can manage the cache with these commands:

- `ferric database-init -i ~/Music/Library` - Scan your library and warm up the cache
- `ferric database-init -i ~/Music/Library --without-fingerprints` - Scan without generating fingerprints (faster)
- `ferric database-clean` - Remove stale entries for missing or changed files

The cache is stored at `~/.ferric/metadata_cache.db` by default.

## Quality Scoring Examples
Here are some real-world examples of how ferric's quality scoring works:

| Format | Bitrate | Codec Multiplier | Quality Score | Notes |
|--------|---------|------------------|---------------|-------|
| FLAC | 1411 kbps | N/A | **11411** | Lossless always wins |
| OPUS | 192 kbps | 1.8× | **346** | Best lossy option |
| AAC | 256 kbps | 1.3× | **333** | Good modern codec |
| MP3 | 320 kbps | 1.0× | **320** | Baseline |
| MP3 | 256 kbps | 1.0× | **256** | Lower quality |
| WMA | 320 kbps | 0.9× | **288** | Suboptimal codec |

So when ferric compares files, OPUS 192 beats MP3 320! This is based on perceptual quality research showing that modern codecs are significantly more efficient.

## Common Workflows

### Organizing a New Music Download
```bash
# Preview what would happen
ferric sort --dry-run -i ~/Downloads/NewAlbum -o ~/Music/Library

# Actually do it (moves files)
ferric sort --move -i ~/Downloads/NewAlbum -o ~/Music/Library
```

### Converting Your FLAC Library to OPUS
```bash
# Preview first (always!)
ferric convert --dry-run -i ~/Music/FLAC -o ~/Music/OPUS --format opus

# Convert to OPUS at 192kbps
ferric convert -i ~/Music/FLAC -o ~/Music/OPUS --format opus

# If you're confident, delete originals after conversion
ferric convert -i ~/Music/FLAC -o ~/Music/OPUS --format opus --delete-original
```

### Fixing Metadata with MusicBrainz
```bash
# Fix all metadata fields (artist, album, title, date, genre)
ferric fix-metadata -i ~/Music/Library --all

# Only fix missing artist and album tags
ferric fix-metadata -i ~/Music/Library --artist --album

# Auto-apply high confidence matches without prompting
ferric fix-metadata -i ~/Music/Library --all --auto-apply

# Always prompt, even for high confidence matches
ferric fix-metadata -i ~/Music/Library --all --interactive
```

### Creating Spotify Playlists Locally
```bash
# Export your Spotify playlist using Exportify (https://watsonbox.github.io/exportify/)
# Then convert it to .m3u format
ferric playlist-import \
  --playlist ~/Downloads/MyPlaylist.csv \
  --library ~/Music/Library \
  --playlist-folder ~/Playlists

# Auto-select best matches without prompting
ferric playlist-import \
  --playlist ~/Downloads/MyPlaylist.csv \
  --library ~/Music/Library \
  --playlist-folder ~/Playlists \
  --auto-select
```

### Merging Multiple Music Libraries
```bash
# Merge two or more libraries using symlinks (space-efficient)
ferric merge-libraries \
  -i ~/Music/Library1 \
  -i ~/Music/Library2 \
  -i ~/Music/Library3 \
  -o ~/Music/MergedLibrary

# Deduplicate across libraries (replaces lower quality with symlinks)
ferric dedupe-libraries \
  -i ~/Music/Library1 \
  -i ~/Music/Library2
```

### Complete Organization Pipeline
```bash
# Sort, convert to OPUS, and normalize naming - all in one command
ferric unified -i ~/Downloads/Music -o ~/Music/Library --format opus

# Same but with dry-run to preview
ferric unified --dry-run -i ~/Downloads/Music -o ~/Music/Library --format opus

# Destructive mode: delete lower quality duplicates
ferric unified -i ~/Downloads/Music -o ~/Music/Library --destructive
```

## Shell Completion
Generate shell completion scripts for bash, zsh, fish, or PowerShell:
```bash
# Bash
ferric completions bash > /etc/bash_completion.d/ferric

# Zsh
ferric completions zsh > /usr/local/share/zsh/site-functions/_ferric

# Fish
ferric completions fish > ~/.config/fish/completions/ferric.fish

# PowerShell
ferric completions powershell > ferric.ps1
```

## Logging
All operations are logged to timestamped files in `~/.ferric/logs/`. Logs include:
- Detailed operation information
- Error messages with context
- File paths and metadata
- Summary statistics

Console output includes color-coded messages:
- 🟢 Green = Success
- 🟡 Yellow = Warning
- 🔴 Red = Error
- 🔵 Cyan = Info
- 🟣 Magenta = Stage markers

## To-Do
- Have a real developer review this code and tell me what I'm doing wrong
- Add a web interface so I don't have to explain the command line to my friends
- Implement undo/rollback functionality (because we all make mistakes)
- Add automatic album cover downloading from MusicBrainz
- ~~Implement MusicBrainz integration~~ ✓
- ~~Add metadata caching for faster operations~~ ✓
- ~~Create playlist import from Spotify Exportify~~ ✓
- Add watch mode for automatic library organization
- Figure out why symphonia sometimes crashes on weird files
- Add support for embedded lyrics
- Make the playlist matching even smarter (it's already pretty smart though)

## License
MIT License - do whatever you want with this code, just don't blame me if it eats your music library. (But seriously, use `--dry-run` first!)
