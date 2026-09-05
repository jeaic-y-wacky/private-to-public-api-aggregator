use tide::{log, Request, Response, StatusCode};
use tide::prelude::*;
use std::time::Instant;
use crate::url_handlers::LAST_READ_URLS;
use crate::letterboxd;
use crate::spotify;

/// Aggregated data response structure
#[derive(Debug, serde::Serialize)]
struct AggregatedData {
    urls: Vec<String>,
    movies: Vec<letterboxd::LetterboxdMovie>,
    tracks: Vec<spotify::SpotifyTrack>,
    /// Populated only when a source failed. Without this, a broken upstream is
    /// indistinguishable from "nothing to show" on the consuming page.
    #[serde(skip_serializing_if = "Option::is_none")]
    errors: Option<AggregationErrors>,
}

#[derive(Debug, Default, serde::Serialize)]
struct AggregationErrors {
    #[serde(skip_serializing_if = "Option::is_none")]
    letterboxd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    spotify: Option<String>,
}

impl AggregationErrors {
    fn is_empty(&self) -> bool {
        self.letterboxd.is_none() && self.spotify.is_none()
    }
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
        .unwrap_or_else(|| "https://letterboxd.com/atropos_Dad/rss".to_string());

    let spotify_limit = req.url().query_pairs()
        .find(|(k, _)| k == "limit")
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(6);

    let no_cache = req.url().query_pairs()
        .find(|(k, _)| k == "no_cache")
        .map(|(_, v)| v == "true")
        .unwrap_or(false);

    if no_cache {
        log::info!("Bypassing caches for this aggregated request");
        spotify::clear_caches();
    }

    let mut errors = AggregationErrors::default();

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
            errors.letterboxd = Some(e.to_string());
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
            errors.spotify = Some(e);
            vec![]
        }
    };

    // Combine all data into response
    let aggregated_data = AggregatedData {
        urls,
        movies,
        tracks,
        errors: if errors.is_empty() { None } else { Some(errors) },
    };

    let mut res = Response::new(StatusCode::Ok);
    res.set_content_type("application/json");
    // This is a live feed: never let a browser or CDN hand back a stale copy.
    res.insert_header("Cache-Control", "no-store, max-age=0");
    res.set_body(json!(aggregated_data));

    log::info!("Aggregated data request processed in {:?}", start_time.elapsed());

    Ok(res)
}
