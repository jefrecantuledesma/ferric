use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

/// Generate an audio fingerprint using Chromaprint (via ffmpeg)
///
/// This function uses ffmpeg to decode the audio and pipe it to chromaprint's fpcalc tool,
/// which generates an AcoustID fingerprint. The fingerprint is a compact representation of
/// the audio that can be used for:
/// - Identifying tracks via AcoustID/MusicBrainz
/// - Detecting duplicate files with different encodings
/// - Finding the same recording in different quality/format
///
/// # Performance Notes
/// - Fingerprinting is CPU-intensive but fast (typically < 1 second per track)
/// - Only analyzes the first 120 seconds of audio for efficiency
/// - Can be parallelized across multiple files using Rayon
///
/// # Returns
/// A base64-encoded fingerprint string on success
pub fn generate_fingerprint<P: AsRef<Path>>(audio_path: P) -> Result<String> {
    let path = audio_path.as_ref();

    // Verify file exists
    if !path.exists() {
        anyhow::bail!("Audio file does not exist: {}", path.display());
    }

    // Try to use fpcalc directly first (most efficient)
    match generate_fingerprint_fpcalc(path) {
        Ok(fp) => return Ok(fp),
        Err(e) => {
            crate::logger::warning(&format!(
                "fpcalc not available, trying ffmpeg method: {}",
                e
            ));
        }
    }

    // Fallback: use ffmpeg to decode and chromaprint library
    generate_fingerprint_ffmpeg(path)
}

/// Generate fingerprint using fpcalc command-line tool (fastest method)
fn generate_fingerprint_fpcalc<P: AsRef<Path>>(audio_path: P) -> Result<String> {
    let path = audio_path.as_ref();

    // Run fpcalc to generate fingerprint
    // -length 120 = only analyze first 120 seconds for speed
    // Default format (compressed) is compatible with AcoustID API
    let output = Command::new("fpcalc")
        .args(&["-length", "120"])
        .arg(path)
        .output()
        .context("Failed to execute fpcalc - is chromaprint installed?")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("fpcalc failed: {}", stderr);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Parse output: fpcalc outputs "FINGERPRINT=..." on one line
    for line in stdout.lines() {
        if let Some(fp) = line.strip_prefix("FINGERPRINT=") {
            return Ok(fp.to_string());
        }
    }

    anyhow::bail!("fpcalc did not output a fingerprint")
}

/// Generate fingerprint using ffmpeg + rusty-chromaprint library
///
/// This method decodes audio with ffmpeg and feeds it to rusty-chromaprint (pure Rust implementation).
/// Fallback method when fpcalc is not available.
fn generate_fingerprint_ffmpeg<P: AsRef<Path>>(audio_path: P) -> Result<String> {
    let path = audio_path.as_ref();

    // Use ffmpeg to decode audio to raw PCM that chromaprint can process
    // -t 120 = only decode first 120 seconds for speed
    // -f s16le = signed 16-bit little-endian PCM
    // -ac 1 = mono (rusty-chromaprint typically expects mono)
    // -ar 16000 = 16kHz sample rate (standard for chromaprint)
    let output = Command::new("ffmpeg")
        .args(&[
            "-v",
            "quiet",
            "-t",
            "120", // Only process first 120 seconds for speed
            "-i",
        ])
        .arg(path)
        .args(&[
            "-f",
            "s16le",
            "-ac",
            "1", // Mono
            "-ar",
            "16000", // 16kHz
            "pipe:1", // Output to stdout
        ])
        .output()
        .context("Failed to decode audio with ffmpeg")?;

    if !output.status.success() {
        anyhow::bail!("ffmpeg failed to decode audio file");
    }

    let pcm_data = output.stdout;

    // Convert raw bytes to i16 samples
    let samples: Vec<i16> = pcm_data
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();

    // Use rusty-chromaprint to generate fingerprint
    let config = rusty_chromaprint::Configuration::preset_test2();
    let mut fingerprinter = rusty_chromaprint::Fingerprinter::new(&config);

    // Start fingerprinting: 16kHz sample rate, mono (1 channel)
    fingerprinter
        .start(16000, 1)
        .context("Failed to start rusty-chromaprint fingerprinter")?;

    // Feed the audio samples
    fingerprinter.consume(&samples);

    // Finalize the fingerprint
    fingerprinter.finish();

    // Get the fingerprint (returns &[u32])
    let fingerprint = fingerprinter.fingerprint();

    // Encode fingerprint as comma-separated string for storage
    // This format can be used with AcoustID API
    let fp_str = fingerprint
        .iter()
        .map(|x| x.to_string())
        .collect::<Vec<_>>()
        .join(",");

    Ok(fp_str)
}

/// Check if fingerprinting tools are available on the system
pub fn check_fingerprint_availability() -> FingerprintAvailability {
    let fpcalc_available = Command::new("fpcalc")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    let ffmpeg_available = Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    FingerprintAvailability {
        fpcalc_available,
        ffmpeg_available,
    }
}

#[derive(Debug, Clone)]
pub struct FingerprintAvailability {
    pub fpcalc_available: bool,
    pub ffmpeg_available: bool,
}

impl FingerprintAvailability {
    pub fn is_available(&self) -> bool {
        self.fpcalc_available || self.ffmpeg_available
    }

    pub fn print_status(&self) {
        if self.fpcalc_available {
            crate::logger::success("fpcalc (chromaprint) is available - fingerprinting enabled");
        } else {
            crate::logger::warning("fpcalc not found - install chromaprint for better performance");
        }

        if self.ffmpeg_available {
            crate::logger::success("ffmpeg is available - can decode audio for fingerprinting");
        } else {
            crate::logger::error("ffmpeg not found - fingerprinting will not work!");
        }

        if !self.is_available() {
            crate::logger::error(
                "Audio fingerprinting is disabled. Install fpcalc or ffmpeg + libchromaprint.",
            );
        }
    }
}

/// Generate fingerprints for multiple files in parallel
///
/// This is the recommended way to fingerprint a large collection, as it uses
/// Rayon for parallelization and shows a progress bar.
pub fn generate_fingerprints_parallel(
    files: &[std::path::PathBuf],
    verbose: bool,
) -> Vec<(String, Option<String>)> {
    use indicatif::{ProgressBar, ProgressStyle};
    use rayon::prelude::*;

    let pb = ProgressBar::new(files.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] [{bar:40}] {pos}/{len} ({eta}) | Generating fingerprints...")
            .unwrap()
            .progress_chars("█▓▒░"),
    );

    let results: Vec<(String, Option<String>)> = files
        .par_iter()
        .map(|path| {
            let path_str = path.to_string_lossy().to_string();

            let fingerprint = match generate_fingerprint(path) {
                Ok(fp) => {
                    crate::logger::debug(
                        &format!("Generated fingerprint for: {}", path.display()),
                        verbose,
                    );
                    Some(fp)
                }
                Err(e) => {
                    crate::logger::warning(&format!(
                        "Failed to fingerprint {}: {}",
                        path.display(),
                        e
                    ));
                    None
                }
            };

            pb.inc(1);
            (path_str, fingerprint)
        })
        .collect();

    pb.finish_and_clear();
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fingerprint_availability() {
        let availability = check_fingerprint_availability();
        println!(
            "fpcalc: {}, ffmpeg: {}",
            availability.fpcalc_available, availability.ffmpeg_available
        );
        // This test just checks if the detection works, doesn't require tools to be installed
    }
}
