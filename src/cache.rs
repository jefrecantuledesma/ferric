use crate::metadata::AudioMetadata;
use anyhow::{bail, Context, Result};
use lazy_static::lazy_static;
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

lazy_static! {
    static ref GLOBAL_CACHE: Mutex<Option<MetadataCache>> = Mutex::new(None);
}

/// Thread-safe metadata cache using SQLite
#[derive(Clone)]
pub struct MetadataCache {
    connection: Arc<Mutex<Connection>>,
}

/// Initialize the process-wide metadata cache (safe to call multiple times)
pub fn init_global_cache<P: AsRef<Path>>(db_path: P) -> Result<()> {
    let cache = MetadataCache::new(db_path)?;
    let mut global = GLOBAL_CACHE.lock().unwrap();
    *global = Some(cache);
    Ok(())
}

/// Get the initialized metadata cache (if any)
pub fn get_global_cache() -> Option<MetadataCache> {
    GLOBAL_CACHE
        .lock()
        .ok()
        .and_then(|guard| guard.as_ref().cloned())
}

impl MetadataCache {
    /// Create or open a metadata cache database
    pub fn new<P: AsRef<Path>>(db_path: P) -> Result<Self> {
        let path = db_path.as_ref();

        // Create parent directory if it doesn't exist
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).context("Failed to create cache directory")?;
        }

        let conn = Connection::open(path).context("Failed to open cache database")?;

        // Enable WAL mode for better concurrent access
        conn.execute_batch("PRAGMA journal_mode=WAL;")
            .context("Failed to enable WAL mode")?;

        // Create schema if not exists
        conn.execute(
            "CREATE TABLE IF NOT EXISTS metadata_cache (
                path TEXT PRIMARY KEY,
                mtime INTEGER NOT NULL,
                size INTEGER NOT NULL,
                metadata_json TEXT NOT NULL,
                cached_at INTEGER NOT NULL
            )",
            [],
        )
        .context("Failed to create metadata_cache table")?;

        Ok(Self {
            connection: Arc::new(Mutex::new(conn)),
        })
    }

    /// Get cached metadata if file hasn't changed
    pub fn get(&self, path: &Path) -> Result<Option<AudioMetadata>> {
        // Get file metadata to check if it's changed
        let file_meta = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(_) => return Ok(None), // File doesn't exist
        };

        let mtime = file_meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let size = file_meta.len() as i64;

        // Use canonical path for lookup (same as insert)
        let canonical_path = path.canonicalize()
            .unwrap_or_else(|_| path.to_path_buf());
        let path_str = canonical_path.to_string_lossy().to_string();

        let conn = self.connection.lock().unwrap();

        let mut stmt = conn.prepare(
            "SELECT metadata_json
             FROM metadata_cache
             WHERE path = ?1 AND mtime = ?2 AND size = ?3",
        )?;

        let result: Result<String, rusqlite::Error> =
            stmt.query_row(params![path_str.as_str(), mtime, size], |row| row.get(0));

        match result {
            Ok(json) => match serde_json::from_str(&json) {
                Ok(metadata) => Ok(Some(metadata)),
                Err(err) => {
                    crate::logger::warning(&format!(
                        "Failed to parse cached metadata for {}: {}. Entry will be cleared.",
                        path_str, err
                    ));
                    let _ = conn.execute(
                        "DELETE FROM metadata_cache WHERE path = ?1",
                        params![path_str.as_str()],
                    );
                    Ok(None)
                }
            },
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Cache metadata for a file
    pub fn insert(&self, path: &Path, metadata: &AudioMetadata) -> Result<()> {
        // Get file metadata for mtime/size
        let file_meta =
            std::fs::metadata(path).context("Failed to get file metadata for caching")?;

        let mtime = file_meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let size = file_meta.len() as i64;

        // Always store canonical (absolute) paths to avoid issues with relative paths
        // when database-clean is run from a different directory
        let canonical_path = path.canonicalize()
            .unwrap_or_else(|_| path.to_path_buf());
        let path_str = canonical_path.to_string_lossy().to_string();
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let metadata_json =
            serde_json::to_string(metadata).context("Failed to serialize metadata for cache")?;

        let conn = self.connection.lock().unwrap();

        conn.execute(
            "INSERT OR REPLACE INTO metadata_cache
             (path, mtime, size, metadata_json, cached_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![path_str.as_str(), mtime, size, metadata_json, now,],
        )?;

        Ok(())
    }

    /// Clear all cached metadata
    pub fn clear(&self) -> Result<()> {
        let conn = self.connection.lock().unwrap();
        conn.execute("DELETE FROM metadata_cache", [])?;
        Ok(())
    }

    /// Remove entries for files that no longer exist or changed on disk
    pub fn clean_stale_entries(&self) -> Result<CacheCleanupStats> {
        let entries: Vec<(String, i64, i64)> = {
            let conn = self.connection.lock().unwrap();
            let mut stmt = conn
                .prepare("SELECT path, mtime, size FROM metadata_cache")
                .context("Failed to query cache entries")?;
            let rows = stmt
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
                .collect::<std::result::Result<Vec<_>, _>>()
                .context("Failed to iterate cache rows")?;
            rows
        };

        let mut removed_missing = 0;
        let mut removed_changed = 0;
        let mut to_delete: Vec<String> = Vec::new();

        for (path, cached_mtime, cached_size) in entries.iter() {
            let path_buf = PathBuf::from(path);
            match std::fs::metadata(&path_buf) {
                Ok(meta) => {
                    let current_mtime = meta
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                    let current_size = meta.len() as i64;

                    if current_mtime != *cached_mtime || current_size != *cached_size {
                        removed_changed += 1;
                        crate::logger::info(&format!(
                            "Removing stale cache entry (changed): {}",
                            path
                        ));
                        to_delete.push(path.clone());
                    }
                }
                Err(_) => {
                    removed_missing += 1;
                    crate::logger::info(&format!(
                        "Removing cache entry for missing file: {}",
                        path
                    ));
                    to_delete.push(path.clone());
                }
            }
        }

        if !to_delete.is_empty() {
            let conn = self.connection.lock().unwrap();
            let tx = conn
                .unchecked_transaction()
                .context("Failed to start cache cleanup transaction")?;
            for path in &to_delete {
                tx.execute("DELETE FROM metadata_cache WHERE path = ?1", params![path])?;
            }
            tx.commit()
                .context("Failed to commit cache cleanup transaction")?;
        }

        Ok(CacheCleanupStats {
            total_entries: entries.len(),
            removed_missing,
            removed_changed,
        })
    }

    /// Get cache statistics
    pub fn stats(&self) -> Result<CacheStats> {
        let conn = self.connection.lock().unwrap();

        let total_entries: i64 =
            conn.query_row("SELECT COUNT(*) FROM metadata_cache", [], |row| row.get(0))?;

        let db_size = if let Ok(path) = self.get_path() {
            std::fs::metadata(&path).ok().map(|m| m.len()).unwrap_or(0)
        } else {
            0
        };

        Ok(CacheStats {
            total_entries: total_entries as usize,
            db_size_bytes: db_size,
        })
    }

    /// Get the database file path (for info/debugging)
    fn get_path(&self) -> Result<PathBuf> {
        let conn = self.connection.lock().unwrap();
        let mut stmt = conn.prepare("PRAGMA database_list")?;
        let mut rows = stmt
            .query([])
            .context("Failed to query database information for cache")?;

        while let Some(row) = rows.next()? {
            let name: String = row.get(1)?;
            if name == "main" {
                let path: String = row.get(2)?;
                return Ok(PathBuf::from(path));
            }
        }

        bail!("Unable to determine cache database path")
    }

    /// Initialize/warm up cache by scanning directories for audio files
    pub fn initialize_from_directories(
        &self,
        directories: &[PathBuf],
        verbose: bool,
    ) -> Result<()> {
        use crate::logger;
        use crate::metadata::AudioMetadata;
        use crate::utils;
        use indicatif::{ProgressBar, ProgressStyle};
        use rayon::prelude::*;
        use walkdir::WalkDir;

        logger::stage("Initializing metadata cache");
        logger::info(&format!("Scanning {} directories", directories.len()));

        // Collect all audio files from all directories
        let mut all_files = Vec::new();
        for dir in directories {
            if !dir.exists() {
                logger::warning(&format!("Directory does not exist, skipping: {}", dir.display()));
                continue;
            }

            logger::info(&format!("Scanning: {}", dir.display()));
            let files: Vec<PathBuf> = WalkDir::new(dir)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().is_file())
                .map(|e| e.path().to_path_buf())
                .filter(|p| utils::is_audio_file(p))
                .collect();

            logger::info(&format!("  Found {} audio files", files.len()));
            all_files.extend(files);
        }

        let total_files = all_files.len();
        logger::info(&format!("\nTotal audio files found: {}", total_files));
        logger::info("Extracting metadata and populating cache...");

        let pb = ProgressBar::new(total_files as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("[{elapsed_precise}] [{bar:40}] {pos}/{len} ({eta}) | Caching metadata...")
                .unwrap()
                .progress_chars("█▓▒░"),
        );

        // Process files in parallel
        let cache_hits = std::sync::atomic::AtomicUsize::new(0);
        let cache_misses = std::sync::atomic::AtomicUsize::new(0);
        let errors = std::sync::atomic::AtomicUsize::new(0);

        all_files.par_iter().for_each(|file| {
            pb.inc(1);

            // Check if already cached
            if let Ok(Some(_)) = self.get(file) {
                cache_hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                logger::debug(&format!("Already cached: {}", file.display()), verbose);
                return;
            }

            // Extract and cache metadata
            match AudioMetadata::from_file(file) {
                Ok(metadata) => {
                    if let Err(e) = self.insert(file, &metadata) {
                        logger::error(&format!(
                            "Failed to cache metadata for {}: {}",
                            file.display(),
                            e
                        ));
                        errors.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    } else {
                        cache_misses.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        logger::debug(&format!("Cached: {}", file.display()), verbose);
                    }
                }
                Err(e) => {
                    logger::error(&format!(
                        "Failed to extract metadata from {}: {}",
                        file.display(),
                        e
                    ));
                    errors.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
        });

        pb.finish_and_clear();

        let hits = cache_hits.load(std::sync::atomic::Ordering::Relaxed);
        let misses = cache_misses.load(std::sync::atomic::Ordering::Relaxed);
        let errs = errors.load(std::sync::atomic::Ordering::Relaxed);

        logger::success(&format!("\nCache initialization complete!"));
        logger::info(&format!("  Total files: {}", total_files));
        logger::info(&format!("  Already cached: {}", hits));
        logger::info(&format!("  Newly cached: {}", misses));
        if errs > 0 {
            logger::warning(&format!("  Errors: {}", errs));
        }

        // Show final cache stats
        if let Ok(stats) = self.stats() {
            stats.print();
        }

        Ok(())
    }
}

#[derive(Debug)]
pub struct CacheStats {
    pub total_entries: usize,
    pub db_size_bytes: u64,
}

impl CacheStats {
    pub fn print(&self) {
        crate::logger::info(&format!(
            "Cache contains {} entries ({:.2} MB)",
            self.total_entries,
            self.db_size_bytes as f64 / 1024.0 / 1024.0
        ));
    }
}

#[derive(Debug)]
pub struct CacheCleanupStats {
    pub total_entries: usize,
    pub removed_missing: usize,
    pub removed_changed: usize,
}

impl CacheCleanupStats {
    pub fn print(&self) {
        crate::logger::success(&format!(
            "Cache cleanup complete. Checked {} entries: {} removed (missing), {} removed (changed).",
            self.total_entries, self.removed_missing, self.removed_changed
        ));
    }
}
