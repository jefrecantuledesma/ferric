use anyhow::Result;
use clap::{Parser, Subcommand};
use ferric::config::Config;
use ferric::operations::*;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "ferric")]
#[command(author, version, about, long_about = None)]
#[command(about = "High-performance audio library organization tool")]
struct Cli {
    /// Path to config file (default: ~/.ferric/ferric.toml or ./ferric.toml)
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Dry run - show what would be done without making changes
    #[arg(long, global = true)]
    dry_run: bool,

    /// Verbose output
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Custom log file path (default: ~/.ferric/logs/ferric_TIMESTAMP.log)
    #[arg(long, global = true)]
    log_file: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Convert audio files to specified format
    Convert {
        /// Input directory to scan
        #[arg(short, long)]
        input: PathBuf,

        /// Output directory for converted files
        #[arg(short, long)]
        output: PathBuf,

        /// Output format (opus, aac, mp3, vorbis)
        #[arg(short, long)]
        format: Option<String>,

        /// Delete original files after successful conversion
        #[arg(long)]
        delete_original: bool,
    },

    /// Sort files by metadata tags into Artist/Album structure
    TagSort {
        /// Input directory to scan
        #[arg(short, long)]
        input: PathBuf,

        /// Output library directory
        #[arg(short, long)]
        output: PathBuf,

        /// Move files instead of copying
        #[arg(long)]
        r#move: bool,
    },

    /// Sort files with intelligent quality comparison (only upgrades)
    Sort {
        /// Input directory to scan
        #[arg(short, long)]
        input: PathBuf,

        /// Output library directory
        #[arg(short, long)]
        output: PathBuf,

        /// Move files instead of copying
        #[arg(long)]
        r#move: bool,
    },

    /// Fix naming issues (apostrophes, case, whitespace)
    FixNaming {
        /// Directory to process
        #[arg(short, long)]
        input: PathBuf,
    },

    /// Fix missing metadata (artist, album, cover art)
    FixMetadata {
        /// Directory to process
        #[arg(short, long)]
        input: PathBuf,

        /// Check for missing artist
        #[arg(long)]
        artist: bool,

        /// Check for missing album
        #[arg(long)]
        album: bool,

        /// Check for missing album cover
        #[arg(long)]
        cover: bool,
    },

    /// Find and remove duplicate files based on metadata
    Dedupe {
        /// Input directory to scan
        #[arg(short, long)]
        input: PathBuf,

        /// Automatically remove duplicates without confirmation
        #[arg(long)]
        auto_remove: bool,
    },

    /// Run unified pipeline: sort -> optional convert -> fix naming
    Unified {
        /// Input directory to scan
        #[arg(short, long)]
        input: PathBuf,

        /// Output library directory
        #[arg(short, long)]
        output: PathBuf,

        /// Convert to specified format after sorting (opus, aac, mp3, vorbis)
        #[arg(short, long)]
        format: Option<String>,

        /// Delete original files after conversion (requires --format)
        #[arg(long)]
        destructive: bool,

        /// Always convert regardless of quality (requires --format)
        #[arg(long)]
        always_convert: bool,

        /// Convert higher quality down (e.g., FLAC to lossy to save space, requires --format)
        #[arg(long)]
        convert_down: bool,
    },

    /// Generate example config file
    GenConfig {
        /// Output path for config file
        #[arg(short, long, default_value = "ferric.toml")]
        output: PathBuf,
    },

    /// Build an .m3u playlist from an Exportify CSV and local library
    PlaylistImport {
        /// Path to Exportify CSV file
        #[arg(short, long)]
        playlist: PathBuf,

        /// Library root to search for audio files
        #[arg(short, long)]
        library: PathBuf,

        /// Optional output path for generated .m3u (defaults to playlist path with .m3u)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Load configuration
    let config = if let Some(config_path) = cli.config {
        Config::from_file(&config_path)?
    } else {
        Config::load_or_default()
    };

    // Initialize logging
    let log_path = ferric::logger::init_logger(cli.log_file)?;
    ferric::logger::info(&format!("Log file: {}", log_path.display()));

    // Execute command
    let result = match cli.command {
        Commands::Convert {
            input,
            output,
            format,
            delete_original,
        } => {
            let opts = convert::ConvertOptions {
                input_dir: input,
                output_dir: output,
                output_format: format,
                delete_original,
                always_convert: config.convert.always_convert,
                convert_down: config.convert.convert_down,
                dry_run: cli.dry_run,
                verbose: cli.verbose,
                config,
            };
            convert::run(opts).map(|_| ())
        }

        Commands::TagSort {
            input,
            output,
            r#move,
        } => {
            let opts = tag_sort::TagSortOptions {
                input_dir: input,
                output_dir: output,
                do_move: r#move,
                dry_run: cli.dry_run,
                verbose: cli.verbose,
                config,
            };
            tag_sort::run(opts).map(|_| ())
        }

        Commands::Sort {
            input,
            output,
            r#move,
        } => {
            let opts = sort::SortOptions {
                input_dir: input,
                output_dir: output,
                do_move: r#move,
                dry_run: cli.dry_run,
                verbose: cli.verbose,
                config,
            };
            sort::run(opts).map(|_| ())
        }

        Commands::FixNaming { input } => {
            let opts = fix_naming::FixNamingOptions {
                input_dir: input,
                dry_run: cli.dry_run,
                verbose: cli.verbose,
            };
            fix_naming::run(opts).map(|_| ())
        }

        Commands::FixMetadata {
            input,
            artist,
            album,
            cover,
        } => {
            let opts = fix_metadata::FixMetadataOptions {
                input_dir: input,
                check_artist: artist,
                check_album: album,
                check_cover: cover,
                dry_run: cli.dry_run,
                verbose: cli.verbose,
            };
            fix_metadata::run(opts)
        }

        Commands::Dedupe { input, auto_remove } => {
            let opts = dedupe::DedupeOptions {
                input_dir: input,
                dry_run: cli.dry_run,
                verbose: cli.verbose,
                auto_remove,
                config,
            };
            dedupe::run(opts).map(|_| ())
        }

        Commands::Unified {
            input,
            output,
            format,
            destructive,
            always_convert,
            convert_down,
        } => {
            let opts = unified::UnifiedOptions {
                input_dir: input,
                output_dir: output,
                output_format: format,
                destructive,
                always_convert,
                convert_down,
                dry_run: cli.dry_run,
                verbose: cli.verbose,
                config,
            };
            unified::run(opts)
        }

        Commands::GenConfig { output } => {
            let example = Config::generate_example();
            std::fs::write(&output, example)?;
            ferric::logger::success(&format!(
                "Generated example config at: {}",
                output.display()
            ));
            Ok(())
        }

        Commands::PlaylistImport {
            playlist,
            library,
            output,
        } => {
            let opts = playlist::PlaylistImportOptions {
                playlist_csv: playlist,
                library_dir: library,
                output_path: output,
                dry_run: cli.dry_run,
                verbose: cli.verbose,
            };
            playlist::run(opts)
        }
    };

    match result {
        Ok(_) => {
            ferric::logger::success("\nOperation completed successfully!");
            Ok(())
        }
        Err(e) => {
            ferric::logger::error(&format!("\nOperation failed: {}", e));
            Err(e)
        }
    }
}
