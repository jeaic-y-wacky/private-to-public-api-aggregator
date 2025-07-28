use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::Path;
use std::sync::LazyLock;
use tide::log;

/// Application configuration structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub api: ApiConfig,
    pub spotify: SpotifyConfig,
    pub letterboxd: LetterboxdConfig,
    pub cache: CacheConfig,
    pub cors: CorsConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: String,
    pub log_level: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiConfig {
    pub key: Option<String>, // Will be loaded from environment
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpotifyConfig {
    pub client_id: Option<String>, // Will be loaded from environment
    pub client_secret: Option<String>, // Will be loaded from environment
    pub refresh_token: Option<String>, // Will be loaded from environment
    pub excluded_genres: Vec<String>,
    pub tracks_limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LetterboxdConfig {
    pub default_feed_url: String,
    pub movies_limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheConfig {
    pub spotify_duration_secs: u64,
    pub letterboxd_duration_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorsConfig {
    pub allowed_origin: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            server: ServerConfig {
                host: "0.0.0.0".to_string(),
                port: "4653".to_string(),
                log_level: "info".to_string(),
            },
            api: ApiConfig {
                key: None, // Will be loaded from environment
            },
            spotify: SpotifyConfig {
                client_id: None, // Will be loaded from environment
                client_secret: None, // Will be loaded from environment
                refresh_token: None, // Will be loaded from environment
                excluded_genres: vec!["comedy".to_string()],
                tracks_limit: 6,
            },
            letterboxd: LetterboxdConfig {
                default_feed_url: "https://letterboxd.com/atropos_Dad/rss".to_string(),
                movies_limit: 5,
            },
            cache: CacheConfig {
                spotify_duration_secs: 900, // 15 minutes
                letterboxd_duration_secs: 3600, // 1 hour
            },
            cors: CorsConfig {
                allowed_origin: "https://jeaic.com".to_string(),
            },
        }
    }
}

impl Config {
    /// Load configuration from TOML file and environment variables
    pub fn load() -> Self {
        let mut config = Self::load_from_toml().unwrap_or_else(|e| {
            log::warn!("Failed to load config.toml: {}. Using defaults.", e);
            Config::default()
        });

        // Override with environment variables
        config.load_from_env();
        
        config
    }

    /// Load configuration from TOML file
    fn load_from_toml() -> Result<Self, Box<dyn std::error::Error>> {
        // Try config.local.toml first (for local customization), then config.toml
        let config_paths = ["config.local.toml", "config.toml"];
        
        for config_path in &config_paths {
            if Path::new(config_path).exists() {
                log::info!("Loading configuration from {}", config_path);
                let content = fs::read_to_string(config_path)?;
                let config: Config = toml::from_str(&content)?;
                log::info!("Successfully loaded {}", config_path);
                return Ok(config);
            }
        }
        
        log::info!("No configuration files found, using defaults");
        Ok(Config::default())
    }

    /// Override configuration with environment variables
    fn load_from_env(&mut self) {
        // Server configuration
        if let Ok(host) = env::var("HOST") {
            self.server.host = host;
        }
        if let Ok(port) = env::var("PORT") {
            self.server.port = port;
        }
        if let Ok(log_level) = env::var("RUST_LOG") {
            self.server.log_level = log_level;
        }

        // API configuration
        if let Ok(api_key) = env::var("API_KEY") {
            self.api.key = Some(api_key);
        }

        // Spotify configuration
        if let Ok(client_id) = env::var("SPOTIFY_CLIENT_ID") {
            self.spotify.client_id = Some(client_id);
        }
        if let Ok(client_secret) = env::var("SPOTIFY_CLIENT_SECRET") {
            self.spotify.client_secret = Some(client_secret);
        }
        if let Ok(refresh_token) = env::var("SPOTIFY_REFRESH_TOKEN") {
            self.spotify.refresh_token = Some(refresh_token);
        }
        if let Ok(excluded_genres) = env::var("SPOTIFY_EXCLUDED_GENRES") {
            self.spotify.excluded_genres = excluded_genres
                .split(',')
                .filter(|s| !s.is_empty())
                .map(|s| s.trim().to_string())
                .collect();
        }

        // CORS configuration
        if let Ok(allowed_origin) = env::var("ALLOWED_ORIGIN") {
            self.cors.allowed_origin = allowed_origin;
        }
    }

    /// Validate that required configuration is present
    pub fn validate(&self) -> Result<(), String> {
        if self.api.key.is_none() {
            return Err("API_KEY must be set either in config.toml or environment".to_string());
        }

        if self.spotify.client_id.is_none() {
            return Err("SPOTIFY_CLIENT_ID must be set either in config.toml or environment".to_string());
        }

        if self.spotify.client_secret.is_none() {
            return Err("SPOTIFY_CLIENT_SECRET must be set either in config.toml or environment".to_string());
        }

        if self.spotify.refresh_token.is_none() {
            return Err("SPOTIFY_REFRESH_TOKEN must be set either in config.toml or environment".to_string());
        }

        Ok(())
    }
}

/// Global configuration instance
pub static CONFIG: LazyLock<Config> = LazyLock::new(|| {
    let config = Config::load();
    
    // Validate configuration
    if let Err(e) = config.validate() {
        log::error!("Configuration validation failed: {}", e);
        std::process::exit(1);
    }
    
    log::info!("Configuration loaded successfully");
    config
});