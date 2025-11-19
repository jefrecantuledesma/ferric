use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use ferric::operations::*;
use ferric::{cache, config::Config};
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

    /// Metadata cache database path (default: ~/.ferric/metadata_cache.db)
    #[arg(long, global = true)]
    database: Option<PathBuf>,

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

    /// Sort files by metadata into Artist/Album folder structure
    Sort {
        /// Input directory to scan
        #[arg(short, long)]
        input: PathBuf,

        /// Output library directory (defaults to input directory for in-place sorting)
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Move files instead of copying
        #[arg(long)]
        r#move: bool,

        /// Fix naming (normalize apostrophes, whitespace, etc.) while sorting
        #[arg(long)]
        fix_naming: bool,
    },

    /// Merge an organized library into another, upgrading with better quality
    Merge {
        /// Source library directory to merge from
        #[arg(short, long)]
        input: PathBuf,

        /// Target library directory to merge into
        #[arg(short, long)]
        output: PathBuf,

        /// Move files instead of copying
        #[arg(long)]
        r#move: bool,
    },

    /// Merge multiple libraries using symlinks, keeping highest quality versions
    MergeLibraries {
        /// Input library directories to merge
        #[arg(short, long, num_args = 2..)]
        input: Vec<PathBuf>,

        /// Output directory for merged library
        #[arg(short, long)]
        output: PathBuf,
    },

    /// Deduplicate files across multiple libraries by replacing lower quality with symlinks
    DedupeLibraries {
        /// Input library directories to scan
        #[arg(short, long, num_args = 2..)]
        input: Vec<PathBuf>,
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

        /// Playlist folder where .m3u files will be stored (required for relative path calculation)
        #[arg(short = 'f', long)]
        playlist_folder: PathBuf,

        /// Automatically select the best match instead of prompting for conflicts
        #[arg(long)]
        auto_select: bool,
    },

    /// Remove entries from the metadata cache that point to missing or changed files
    DatabaseClean,

    /// Initialize/warm up the metadata cache by scanning directories
    DatabaseInit {
        /// Input directories to scan (can specify multiple)
        #[arg(short, long, required = true)]
        input: Vec<PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Load configuration
    let mut config = if let Some(config_path) = cli.config {
        Config::from_file(&config_path)?
    } else {
        Config::load_or_default()
    };

    if let Some(database_path) = cli.database {
        config.general.cache_path = database_path;
    }

    // Initialize logging
    let log_path = ferric::logger::init_logger(cli.log_file)?;
    ferric::logger::info(&format!("Log file: {}", log_path.display()));

    // Initialize metadata cache database
    cache::init_global_cache(&config.general.cache_path)?;
    ferric::logger::info(&format!(
        "Metadata cache: {}",
        config.general.cache_path.display()
    ));

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

        Commands::Sort {
            input,
            output,
            r#move,
            fix_naming,
        } => {
            let output_dir = output.unwrap_or_else(|| input.clone());
            let opts = sort::SortOptions {
                input_dir: input,
                output_dir,
                do_move: r#move,
                fix_naming,
                dry_run: cli.dry_run,
                verbose: cli.verbose,
                config,
            };
            sort::run(opts).map(|_| ())
        }

        Commands::Merge {
            input,
            output,
            r#move,
        } => {
            let opts = merge::MergeOptions {
                input_dir: input,
                output_dir: output,
                do_move: r#move,
                dry_run: cli.dry_run,
                verbose: cli.verbose,
                config,
            };
            merge::run(opts).map(|_| ())
        }

        Commands::MergeLibraries { input, output } => {
            let opts = merge_libraries::MergeLibrariesOptions {
                input_dirs: input,
                output_dir: output,
                dry_run: cli.dry_run,
                verbose: cli.verbose,
                config,
            };
            merge_libraries::run(opts).map(|_| ())
        }

        Commands::DedupeLibraries { input } => {
            let opts = dedupe_libraries::DedupeLibrariesOptions {
                input_dirs: input,
                dry_run: cli.dry_run,
                verbose: cli.verbose,
                config,
            };
            dedupe_libraries::run(opts).map(|_| ())
        }

        Commands::FixNaming { input } => {
            let opts = fix_naming::FixNamingOptions {
                input_dir: input,
                dry_run: cli.dry_run,
                verbose: cli.verbose,
                config: config.clone(),
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
            playlist_folder,
            auto_select,
        } => {
            let opts = playlist::PlaylistImportOptions {
                playlist_csv: playlist,
                library_dir: library,
                playlist_folder,
                auto_select,
                dry_run: cli.dry_run,
                verbose: cli.verbose,
            };
            playlist::run(opts)
        }

        Commands::DatabaseClean => {
            let cache = cache::get_global_cache()
                .ok_or_else(|| anyhow!("Metadata cache is not initialized"))?;
            let stats = cache.clean_stale_entries()?;
            stats.print();
            Ok(())
        }

        Commands::DatabaseInit { input } => {
            let cache = cache::get_global_cache()
                .ok_or_else(|| anyhow!("Metadata cache is not initialized"))?;
            cache.initialize_from_directories(&input, cli.verbose)?;
            Ok(())
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
