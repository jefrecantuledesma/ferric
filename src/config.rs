use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub general: GeneralConfig,

    #[serde(default)]
    pub convert: ConvertConfig,

    #[serde(default)]
    pub quality: QualityConfig,

    #[serde(default)]
    pub naming: NamingConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralConfig {
    /// Default number of parallel threads (0 = auto-detect)
    #[serde(default = "default_threads")]
    pub threads: usize,

    /// Enable verbose output by default
    #[serde(default)]
    pub verbose: bool,

    /// Default log file directory
    #[serde(default)]
    pub log_dir: Option<PathBuf>,

    /// Metadata cache database path
    #[serde(default = "default_cache_path")]
    pub cache_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConvertConfig {
    /// Output format (opus, aac, mp3, vorbis)
    #[serde(default = "default_output_format")]
    pub output_format: String,

    /// Target OPUS bitrate in kbps
    #[serde(default = "default_opus_bitrate")]
    pub opus_bitrate: u32,

    /// OPUS compression level (0-10, higher = better compression but slower)
    #[serde(default = "default_opus_compression")]
    pub opus_compression: u8,

    /// Target AAC bitrate in kbps
    #[serde(default = "default_aac_bitrate")]
    pub aac_bitrate: u32,

    /// Target MP3 bitrate in kbps
    #[serde(default = "default_mp3_bitrate")]
    pub mp3_bitrate: u32,

    /// Target Vorbis quality (-1 to 10, higher = better quality)
    #[serde(default = "default_vorbis_quality")]
    pub vorbis_quality: i8,

    /// Delete original files after successful conversion
    #[serde(default)]
    pub delete_original: bool,

    /// Always convert regardless of quality (ignore quality comparison)
    #[serde(default)]
    pub always_convert: bool,

    /// Convert higher quality down (e.g., FLAC to lossy to save space)
    #[serde(default)]
    pub convert_down: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityConfig {
    /// Codec multipliers for quality comparison
    #[serde(default = "default_codec_multipliers")]
    pub codec_multipliers: CodecMultipliers,

    /// Lossless format score bonus
    #[serde(default = "default_lossless_bonus")]
    pub lossless_bonus: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodecMultipliers {
    #[serde(default = "default_opus_mult")]
    pub opus: f64,

    #[serde(default = "default_aac_mult")]
    pub aac: f64,

    #[serde(default = "default_vorbis_mult")]
    pub vorbis: f64,

    #[serde(default = "default_mp3_mult")]
    pub mp3: f64,

    #[serde(default = "default_wma_mult")]
    pub wma: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamingConfig {
    /// Maximum length for folder/file name components
    #[serde(default = "default_max_name_len")]
    pub max_name_length: usize,

    /// Convert everything to lowercase
    #[serde(default = "default_lowercase")]
    pub lowercase: bool,

    /// Prefer track artist over album artist for foldering
    #[serde(default)]
    pub prefer_artist: bool,

    /// Include "Various Artists" as valid album artist
    #[serde(default)]
    pub include_va: bool,
}

// Default value functions
fn default_threads() -> usize {
    0 // 0 means auto-detect
}

fn default_cache_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".ferric")
        .join("metadata_cache.db")
}

fn default_output_format() -> String {
    "opus".to_string()
}

fn default_opus_bitrate() -> u32 {
    192
}

fn default_opus_compression() -> u8 {
    10
}

fn default_aac_bitrate() -> u32 {
    256
}

fn default_mp3_bitrate() -> u32 {
    320
}

fn default_vorbis_quality() -> i8 {
    6
}

fn default_lossless_bonus() -> u32 {
    10000
}

fn default_opus_mult() -> f64 {
    1.8
}

fn default_aac_mult() -> f64 {
    1.3
}

fn default_vorbis_mult() -> f64 {
    1.2
}

fn default_mp3_mult() -> f64 {
    1.0
}

fn default_wma_mult() -> f64 {
    0.9
}

fn default_max_name_len() -> usize {
    128
}

fn default_lowercase() -> bool {
    true
}

fn default_codec_multipliers() -> CodecMultipliers {
    CodecMultipliers {
        opus: default_opus_mult(),
        aac: default_aac_mult(),
        vorbis: default_vorbis_mult(),
        mp3: default_mp3_mult(),
        wma: default_wma_mult(),
    }
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            threads: default_threads(),
            verbose: false,
            log_dir: None,
            cache_path: default_cache_path(),
        }
    }
}

impl Default for ConvertConfig {
    fn default() -> Self {
        Self {
            output_format: default_output_format(),
            opus_bitrate: default_opus_bitrate(),
            opus_compression: default_opus_compression(),
            aac_bitrate: default_aac_bitrate(),
            mp3_bitrate: default_mp3_bitrate(),
            vorbis_quality: default_vorbis_quality(),
            delete_original: false,
            always_convert: false,
            convert_down: false,
        }
    }
}

impl Default for QualityConfig {
    fn default() -> Self {
        Self {
            codec_multipliers: default_codec_multipliers(),
            lossless_bonus: default_lossless_bonus(),
        }
    }
}

impl Default for NamingConfig {
    fn default() -> Self {
        Self {
            max_name_length: default_max_name_len(),
            lowercase: default_lowercase(),
            prefer_artist: false,
            include_va: false,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            general: GeneralConfig::default(),
            convert: ConvertConfig::default(),
            quality: QualityConfig::default(),
            naming: NamingConfig::default(),
        }
    }
}

impl Config {
    /// Load configuration from TOML file
    pub fn from_file(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {}", path.display()))?;

        let config: Config = toml::from_str(&contents)
            .with_context(|| format!("Failed to parse config file: {}", path.display()))?;

        Ok(config)
    }

    /// Try to load config from default locations, or return default config
    pub fn load_or_default() -> Self {
        // Try ~/.ferric/ferric.toml
        if let Ok(home) = std::env::var("HOME") {
            let path = PathBuf::from(home).join(".ferric").join("ferric.toml");
            if path.exists() {
                if let Ok(config) = Self::from_file(&path) {
                    return config;
                }
            }
        }

        // Try ./ferric.toml in current directory
        let path = PathBuf::from("ferric.toml");
        if path.exists() {
            if let Ok(config) = Self::from_file(&path) {
                return config;
            }
        }

        // Return default
        Self::default()
    }

    /// Generate example TOML config file
    pub fn generate_example() -> String {
        toml::to_string_pretty(&Self::default()).unwrap_or_default()
    }
}
