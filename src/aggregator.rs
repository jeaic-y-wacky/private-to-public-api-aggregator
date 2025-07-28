use tide::{log, Request, Response, StatusCode};
use tide::prelude::*;
use std::time::Instant;
use crate::url_handlers::LAST_READ_URLS;
use crate::letterboxd;
use crate::spotify;
use crate::config::CONFIG;

/// Aggregated data response structure
#[derive(Debug, serde::Serialize)]
struct AggregatedData {
    urls: Vec<String>,
    movies: Vec<letterboxd::LetterboxdMovie>,
    tracks: Vec<spotify::SpotifyTrack>,
}

/// Endpoint that aggregates data from URLs, Letterboxd, and Spotify
/// This endpoint does not require authentication
pub async fn get_aggregated_data(req: Request<()>) -> tide::Result<Response> {
    let start_time = Instant::now();
    log::info!("Processing aggregated data request");

    // Get optional parameters from query
    let letterboxd_feed = req.url().query_pairs()
        .find(|(k, _)| k == "feed_url")
        .map(|(_, v)| v.to_string())
        .unwrap_or_else(|| CONFIG.letterboxd.default_feed_url.clone());
        
    let spotify_limit = req.url().query_pairs()
        .find(|(k, _)| k == "limit")
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(CONFIG.spotify.tracks_limit);
        
    let no_cache = req.url().query_pairs()
        .find(|(k, _)| k == "no_cache")
        .map(|(_, v)| v == "true")
        .unwrap_or(false);

    // Log cache preferences
    if no_cache {
        log::info!("Request to bypass cache, but this is not fully implemented in the aggregated endpoint");
    }

    // Fetch URLs from the static queue
    let urls = {
        let urls_lock = LAST_READ_URLS.lock().unwrap();
        urls_lock.iter().cloned().collect::<Vec<String>>()
    };
    log::info!("Retrieved {} URLs", urls.len());

    // Fetch Letterboxd movies
    let movies = match letterboxd::fetch_letterboxd_feed(&letterboxd_feed).await {
        Ok(movies) => {
            log::info!("Retrieved {} Letterboxd movies", movies.len());
            movies
        },
        Err(e) => {
            log::error!("Error fetching Letterboxd data: {}", e);
            vec![]
        }
    };

    // Fetch Spotify tracks
    let tracks = match spotify::get_recently_played(spotify_limit).await {
        Ok(tracks) => {
            log::info!("Retrieved {} Spotify tracks", tracks.len());
            tracks
        },
        Err(e) => {
            log::error!("Error fetching Spotify data: {}", e);
            vec![]
        }
    };

    // Combine all data into response
    let aggregated_data = AggregatedData {
        urls,
        movies,
        tracks,
    };

    let mut res = Response::new(StatusCode::Ok);
    res.set_content_type("application/json");
    res.set_body(json!(aggregated_data));

    let elapsed = start_time.elapsed();
    log::info!("Aggregated data request processed in {:?}", elapsed);

    Ok(res)
} 