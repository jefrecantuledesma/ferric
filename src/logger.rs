use anyhow::Result;
use chrono::Local;
use colored::Colorize;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

lazy_static::lazy_static! {
    static ref LOG_FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);
}

/// Initialize the logging system
pub fn init_logger(custom_log_path: Option<PathBuf>) -> Result<PathBuf> {
    let log_path = if let Some(path) = custom_log_path {
        path
    } else {
        get_default_log_path()?
    };

    // Create parent directory if it doesn't exist
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)?;
    }

    // Open or create log file
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;

    *LOG_FILE.lock().unwrap() = Some(file);

    // Write session header
    log_to_file(&format!(
        "\n{}\nFerric Session: {}\n{}\n",
        "=".repeat(60),
        Local::now().format("%Y-%m-%d %H:%M:%S"),
        "=".repeat(60)
    ));

    Ok(log_path)
}

/// Get default log file path: ~/.ferric/logs/ferric_YYYYMMDD_HHMMSS.log
fn get_default_log_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let log_dir = Path::new(&home).join(".ferric").join("logs");
    fs::create_dir_all(&log_dir)?;

    let timestamp = Local::now().format("%Y%m%d_%H%M%S");
    Ok(log_dir.join(format!("ferric_{}.log", timestamp)))
}

/// Log to file only (detailed)
pub fn log_to_file(message: &str) {
    if let Ok(mut file_opt) = LOG_FILE.lock() {
        if let Some(file) = file_opt.as_mut() {
            let timestamp = Local::now().format("%H:%M:%S");
            let _ = writeln!(file, "[{}] {}", timestamp, message);
        }
    }
}

/// Print success message (green) to console and log
pub fn success(message: &str) {
    println!("{}", message.green());
    log_to_file(&format!("SUCCESS: {}", message));
}

/// Print info message (blue) to console and log
pub fn info(message: &str) {
    println!("{}", message.cyan());
    log_to_file(&format!("INFO: {}", message));
}

/// Print warning message (yellow) to console and log
pub fn warning(message: &str) {
    println!("{}", message.yellow());
    log_to_file(&format!("WARNING: {}", message));
}

/// Print error message (red) to console and log
pub fn error(message: &str) {
    eprintln!("{}", message.red());
    log_to_file(&format!("ERROR: {}", message));
}

/// Print step/stage message (magenta bold) to console and log
pub fn stage(message: &str) {
    println!("{}", message.bright_magenta().bold());
    log_to_file(&format!("STAGE: {}", message));
}

/// Print plain message to console and log
pub fn plain(message: &str) {
    println!("{}", message);
    log_to_file(message);
}

/// Print verbose/debug message (only to log file, not console unless --verbose)
pub fn debug(message: &str, verbose: bool) {
    if verbose {
        println!("{}", message.dimmed());
    }
    log_to_file(&format!("DEBUG: {}", message));
}
