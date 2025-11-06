use crate::config::Config;
use crate::metadata::AudioMetadata;
use crate::utils;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AudioFormat {
    Lossless,
    Lossy,
    Unknown,
}

/// Calculate quality score for an audio file
/// Higher score = better quality
///
/// Algorithm:
/// - Lossless formats get: 10000 + (bitrate / 1000)
/// - Lossy formats get: codec_multiplier × bitrate_kbps
///
/// Codec multipliers (based on psychoacoustic efficiency):
/// - OPUS: 1.8× (best modern codec)
/// - AAC: 1.3× (good modern codec)
/// - Vorbis/OGG: 1.2×
/// - MP3: 1.0× (baseline)
/// - WMA: 0.9× (suboptimal)
pub fn calculate_quality_score(metadata: &AudioMetadata, config: &Config) -> u32 {
    let format = get_audio_format(&metadata.codec);
    let bitrate_kbps = metadata.get_bitrate_kbps().unwrap_or(0);

    match format {
        AudioFormat::Lossless => {
            // Lossless always wins - base score + bitrate bonus
            config.quality.lossless_bonus + bitrate_kbps
        }
        AudioFormat::Lossy => {
            // Apply codec-specific multiplier to bitrate
            let multiplier = get_codec_multiplier(&metadata.codec, config);
            (bitrate_kbps as f64 * multiplier) as u32
        }
        AudioFormat::Unknown => {
            // Conservative estimate for unknown formats
            bitrate_kbps
        }
    }
}

/// Calculate quality score from file path (using extension)
pub fn calculate_quality_score_from_path(path: &Path, config: &Config) -> u32 {
    if let Some(ext) = utils::get_extension(path) {
        let format = get_audio_format_from_ext(&ext);

        match format {
            AudioFormat::Lossless => {
                // Assume typical lossless bitrate if we can't read metadata
                config.quality.lossless_bonus + 1411
            }
            AudioFormat::Lossy => {
                // Use codec multiplier with estimated bitrate
                let multiplier = get_codec_multiplier_from_ext(&ext, config);
                let estimated_bitrate = estimate_bitrate_from_ext(&ext);
                (estimated_bitrate as f64 * multiplier) as u32
            }
            AudioFormat::Unknown => 100, // Low score for unknown
        }
    } else {
        100 // No extension = unknown
    }
}

/// Determine if audio format is lossless or lossy
pub fn get_audio_format(codec: &str) -> AudioFormat {
    let codec_lower = codec.to_lowercase();

    // Lossless codecs
    if codec_lower.contains("flac")
        || codec_lower.contains("alac")
        || codec_lower.contains("ape")
        || codec_lower.contains("wav")
        || codec_lower.contains("aiff")
        || codec_lower.contains("pcm")
        || codec_lower.contains("tta")
        || codec_lower.contains("wv")
    // WavPack
    {
        return AudioFormat::Lossless;
    }

    // Lossy codecs
    if codec_lower.contains("opus")
        || codec_lower.contains("mp3")
        || codec_lower.contains("aac")
        || codec_lower.contains("vorbis")
        || codec_lower.contains("ogg")
        || codec_lower.contains("wma")
    {
        return AudioFormat::Lossy;
    }

    AudioFormat::Unknown
}

/// Get audio format from file extension
pub fn get_audio_format_from_ext(ext: &str) -> AudioFormat {
    match ext.to_lowercase().as_str() {
        "flac" | "wav" | "aiff" | "aif" | "alac" | "ape" | "wv" | "tta" => AudioFormat::Lossless,
        "opus" | "mp3" | "m4a" | "aac" | "ogg" | "wma" => AudioFormat::Lossy,
        _ => AudioFormat::Unknown,
    }
}

/// Get codec multiplier based on codec name and config
fn get_codec_multiplier(codec: &str, config: &Config) -> f64 {
    let codec_lower = codec.to_lowercase();

    if codec_lower.contains("opus") {
        config.quality.codec_multipliers.opus
    } else if codec_lower.contains("aac") || codec_lower.contains("m4a") {
        config.quality.codec_multipliers.aac
    } else if codec_lower.contains("vorbis") || codec_lower.contains("ogg") {
        config.quality.codec_multipliers.vorbis
    } else if codec_lower.contains("mp3") {
        config.quality.codec_multipliers.mp3
    } else if codec_lower.contains("wma") {
        config.quality.codec_multipliers.wma
    } else {
        1.0 // Default multiplier
    }
}

/// Get codec multiplier from file extension
fn get_codec_multiplier_from_ext(ext: &str, config: &Config) -> f64 {
    match ext.to_lowercase().as_str() {
        "opus" => config.quality.codec_multipliers.opus,
        "aac" | "m4a" => config.quality.codec_multipliers.aac,
        "ogg" => config.quality.codec_multipliers.vorbis,
        "mp3" => config.quality.codec_multipliers.mp3,
        "wma" => config.quality.codec_multipliers.wma,
        _ => 1.0,
    }
}

/// Estimate typical bitrate for a given extension (used when metadata unavailable)
fn estimate_bitrate_from_ext(ext: &str) -> u32 {
    match ext.to_lowercase().as_str() {
        "opus" => 160,
        "aac" | "m4a" => 256,
        "ogg" => 192,
        "mp3" => 320,
        "wma" => 192,
        _ => 128,
    }
}

/// Compare two files and return which is higher quality
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualityComparison {
    FirstBetter,
    SecondBetter,
    Equal,
}

pub fn compare_quality(
    meta1: &AudioMetadata,
    meta2: &AudioMetadata,
    config: &Config,
) -> QualityComparison {
    let score1 = calculate_quality_score(meta1, config);
    let score2 = calculate_quality_score(meta2, config);

    if score1 > score2 {
        QualityComparison::FirstBetter
    } else if score2 > score1 {
        QualityComparison::SecondBetter
    } else {
        QualityComparison::Equal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_format_detection() {
        assert_eq!(get_audio_format("flac"), AudioFormat::Lossless);
        assert_eq!(get_audio_format("FLAC"), AudioFormat::Lossless);
        assert_eq!(get_audio_format("opus"), AudioFormat::Lossy);
        assert_eq!(get_audio_format("mp3"), AudioFormat::Lossy);
        assert_eq!(get_audio_format("unknown_codec"), AudioFormat::Unknown);
    }

    #[test]
    fn test_format_from_extension() {
        assert_eq!(get_audio_format_from_ext("flac"), AudioFormat::Lossless);
        assert_eq!(get_audio_format_from_ext("mp3"), AudioFormat::Lossy);
        assert_eq!(get_audio_format_from_ext("xyz"), AudioFormat::Unknown);
    }

    #[test]
    fn test_quality_score() {
        let config = Config::default();

        // Lossless should always score higher
        let mut lossless = AudioMetadata {
            codec: "flac".to_string(),
            bitrate: Some(1411000), // 1411 kbps
            ..Default::default()
        };
        let lossless_score = calculate_quality_score(&lossless, &config);
        assert!(lossless_score > 10000); // Should have lossless bonus

        // High-bitrate lossy
        let mut lossy = AudioMetadata {
            codec: "opus".to_string(),
            bitrate: Some(320000), // 320 kbps
            ..Default::default()
        };
        let lossy_score = calculate_quality_score(&lossy, &config);

        // OPUS 320kbps = 320 * 1.8 = 576
        assert_eq!(lossy_score, 576);

        // Lossless should still win
        assert!(lossless_score > lossy_score);
    }

    #[test]
    fn test_opus_vs_mp3() {
        let config = Config::default();

        let mut opus = AudioMetadata {
            codec: "opus".to_string(),
            bitrate: Some(192000), // 192 kbps
            ..Default::default()
        };

        let mut mp3 = AudioMetadata {
            codec: "mp3".to_string(),
            bitrate: Some(320000), // 320 kbps
            ..Default::default()
        };

        let opus_score = calculate_quality_score(&opus, &config);
        let mp3_score = calculate_quality_score(&mp3, &config);

        // OPUS 192kbps = 192 * 1.8 = 345.6 = 345
        // MP3 320kbps = 320 * 1.0 = 320
        // OPUS should win despite lower bitrate!
        assert!(opus_score > mp3_score);
    }
}
