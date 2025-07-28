use tide::{log, prelude::*};
use dotenv::dotenv;
use femme::LevelFilter;
use tide::security::{CorsMiddleware, Origin};
use http_types::headers::HeaderValue;
use crate::config::CONFIG;

mod url_handlers;
mod auth;
mod letterboxd;
mod spotify;
mod cache;
mod aggregator;
mod config;

#[async_std::main]
async fn main() -> tide::Result<()> {
    // Load .env file and report result
    match dotenv() {
        Ok(_) => log::info!("Successfully loaded .env file"),
        Err(e) => log::warn!("Failed to load .env file: {}", e),
    };
    
    // Print current working directory for debugging
    match std::env::current_dir() {
        Ok(dir) => log::info!("Current working directory: {}", dir.display()),
        Err(e) => log::warn!("Failed to get current directory: {}", e),
    }
    
    // Initialize configuration (this will validate and log configuration status)
    let config = &*CONFIG;
    log::info!("Configuration loaded - API_KEY: {}, HOST: {}, PORT: {}", 
        if config.api.key.is_some() { "set" } else { "missing" },
        config.server.host,
        config.server.port
    );

    // Set log level from configuration
    let log_level = match config.server.log_level.as_str() {
        "debug" => LevelFilter::Debug,
        "trace" => LevelFilter::Trace,
        "warn" => LevelFilter::Warn,
        "error" => LevelFilter::Error,
        _ => LevelFilter::Info,
    };
    tide::log::with_level(log_level);
    
    let mut app = tide::new();
    let cors = CorsMiddleware::new()
        // .allow_origin(Origin::Any)
        .allow_origin(Origin::Exact(config.cors.allowed_origin.clone()))
        .allow_methods("GET, POST, OPTIONS".parse::<HeaderValue>().unwrap())
        .allow_credentials(false);
    app.with(cors);
    
    log::info!("Using HOST={} and PORT={}", config.server.host, config.server.port);
    
    app.at("/").get(|_| async { Ok("API Endpoint Aggregator") });
    app.at("/url-webhook").post(url_handlers::log_url);
    app.at("/url-webhook").get(url_handlers::get_urls);
    app.at("/letterboxd").get(letterboxd::get_letterboxd_movies);
    app.at("/spotify").get(spotify::get_spotify_tracks);
    app.at("/aggregated").get(aggregator::get_aggregated_data);
    
    log::info!("Server running on http://{}:{}", config.server.host, config.server.port);
    app.listen(format!("{}:{}", config.server.host, config.server.port)).await?;
    Ok(())
}