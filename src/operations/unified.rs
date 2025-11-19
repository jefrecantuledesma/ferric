use crate::config::Config;
use crate::logger;
use crate::operations::{convert, fix_naming, sort, OperationStats};
use anyhow::Result;
use std::path::PathBuf;

pub struct UnifiedOptions {
    pub input_dir: PathBuf,
    pub output_dir: PathBuf,
    pub output_format: Option<String>,
    pub destructive: bool,
    pub always_convert: bool,
    pub convert_down: bool,
    pub dry_run: bool,
    pub verbose: bool,
    pub config: Config,
}

/// Run the unified pipeline: sort -> optional convert -> fix naming
pub fn run(options: UnifiedOptions) -> Result<()> {
    logger::stage("============================================================");
    logger::stage("UNIFIED PIPELINE");
    logger::stage("============================================================");

    logger::info(&format!("Input directory: {}", options.input_dir.display()));
    logger::info(&format!(
        "Output directory: {}",
        options.output_dir.display()
    ));

    let should_convert = options.output_format.is_some();
    if should_convert {
        let format = options.output_format.as_ref().unwrap().to_uppercase();
        logger::info(&format!("Convert to: {}", format));
        if options.destructive {
            logger::info("Destructive mode: YES - will delete originals");
        }
    } else {
        logger::info("Convert: NO");
    }

    if options.dry_run {
        logger::warning("DRY RUN MODE - No actual changes will be made");
    }

    // Confirm with user
    if !options.dry_run {
        logger::warning("\nYou are about to run the unified pipeline:");
        logger::info("  1. Sort into Artist/Album structure (quality-aware)");
        if should_convert {
            let format = options.output_format.as_ref().unwrap().to_uppercase();
            logger::info(&format!("  2. Convert to {} (only higher quality)", format));
            if options.destructive {
                logger::warning(&format!("  3. DELETE original non-{} files", format));
            }
            logger::info(&format!(
                "  {}. Normalize all naming",
                if options.destructive { "4" } else { "3" }
            ));
        } else {
            logger::info("  2. Normalize all naming");
        }

        logger::warning("\nProceed? [y/N]: ");
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            logger::info("Operation cancelled by user");
            return Ok(());
        }
    }

    let mut total_stats = OperationStats::new();
    let total_steps = if should_convert { 3 } else { 2 };

    // Step 1: Sort by metadata
    logger::stage(&format!("\n[1/{}] Sorting by metadata...", total_steps));
    let sort_opts = sort::SortOptions {
        input_dir: options.input_dir.clone(),
        output_dir: options.output_dir.clone(),
        do_move: false,
        fix_naming: true, // Unified pipeline always fixes naming
        dry_run: options.dry_run,
        verbose: options.verbose,
        config: options.config.clone(),
    };

    match sort::run(sort_opts) {
        Ok(stats) => {
            total_stats.processed += stats.processed;
            total_stats.succeeded += stats.succeeded;
            total_stats.skipped += stats.skipped;
            total_stats.errors += stats.errors;
            total_stats.skipped_files.extend(stats.skipped_files);
        }
        Err(e) => {
            logger::error(&format!("Sorting failed: {}", e));
            return Err(e);
        }
    }

    // Step 2: Convert (optional)
    let mut current_step = 2;
    if should_convert {
        let format = options.output_format.as_ref().unwrap().to_uppercase();
        logger::stage(&format!(
            "\n[{}/{}] Converting to {}...",
            current_step, total_steps, format
        ));
        let convert_opts = convert::ConvertOptions {
            input_dir: options.output_dir.clone(),
            output_dir: options.output_dir.clone(),
            output_format: options.output_format.clone(),
            delete_original: options.destructive,
            always_convert: options.always_convert,
            convert_down: options.convert_down,
            dry_run: options.dry_run,
            verbose: options.verbose,
            config: options.config.clone(),
        };

        match convert::run(convert_opts) {
            Ok(stats) => {
                total_stats.processed += stats.processed;
                total_stats.succeeded += stats.succeeded;
                total_stats.skipped += stats.skipped;
                total_stats.errors += stats.errors;
                total_stats.skipped_files.extend(stats.skipped_files);
            }
            Err(e) => {
                logger::error(&format!("Conversion failed: {}", e));
                return Err(e);
            }
        }
        current_step += 1;
    }

    // Step 3: Fix naming
    logger::stage(&format!(
        "\n[{}/{}] Normalizing names...",
        current_step, total_steps
    ));
    let naming_opts = fix_naming::FixNamingOptions {
        input_dir: options.output_dir.clone(),
        dry_run: options.dry_run,
        verbose: options.verbose,
        config: options.config.clone(),
    };

    match fix_naming::run(naming_opts) {
        Ok(stats) => {
            total_stats.processed += stats.processed;
            total_stats.succeeded += stats.succeeded;
            total_stats.skipped += stats.skipped;
            total_stats.errors += stats.errors;
            total_stats.skipped_files.extend(stats.skipped_files);
        }
        Err(e) => {
            logger::error(&format!("Name normalization failed: {}", e));
            return Err(e);
        }
    }

    // Final summary
    logger::stage("\n============================================================");
    logger::stage("UNIFIED PIPELINE COMPLETE");
    logger::stage("============================================================");
    total_stats.print_summary("Total");

    Ok(())
}
