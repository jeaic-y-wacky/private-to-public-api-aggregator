use tide::{log, prelude::*};
use dotenv::dotenv;
use femme::LevelFilter;
use std::env;
use tide::security::{CorsMiddleware, Origin};
use http_types::headers::HeaderValue;

mod url_handlers;
mod auth;
mod letterboxd;
mod spotify;
mod cache;
mod aggregator;

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
    
    // Check for critical environment variables
    let api_key = env::var("API_KEY").unwrap_or_else(|_| {
        log::warn!("API_KEY not set in environment");
        "missing".to_string()
    });
    log::info!("API_KEY is {}", if api_key != "missing" { "set" } else { "missing" });
    
        
    let allowed_origin = env::var("ALLOWED_ORIGIN").unwrap_or_else(|_| "https://jeaic.com".to_string());
    log::info!("ALLOWED_ORIGIN is {}", allowed_origin);

    // Set log level based on environment (default to Info for production)
    let log_level = match env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string()).as_str() {
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
        .allow_origin(Origin::Exact(allowed_origin))
        .allow_methods("GET, POST, OPTIONS".parse::<HeaderValue>().unwrap())
        .allow_credentials(false);
    app.with(cors);
    
    // Get host and port from environment variables or use defaults
    let host = env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
    let port = env::var("PORT").unwrap_or_else(|_| "4653".to_string());
    log::info!("Using HOST={} and PORT={}", host, port);
    
    app.at("/").get(|_| async { Ok("API Endpoint Aggregator") });
    app.at("/url-webhook").post(url_handlers::log_url);
    app.at("/url-webhook").get(url_handlers::get_urls);
    app.at("/letterboxd").get(letterboxd::get_letterboxd_movies);
    app.at("/spotify").get(spotify::get_spotify_tracks);
    app.at("/spotify/diagnose").get(spotify::diagnose);
    app.at("/aggregated").get(aggregator::get_aggregated_data);
    
    log::info!("Server running on http://{}:{}", host, port);
    app.listen(format!("{}:{}", host, port)).await?;
    Ok(())
}