use serde::{Deserialize, Serialize};
use tide::{log, Request, Response, StatusCode};
use tide::prelude::*;
use std::collections::HashMap;
use std::time::{Instant, Duration, SystemTime};
use std::sync::{LazyLock, Mutex};
use crate::auth;
use surf;
use base64::Engine as _;
use futures::stream::StreamExt;

static CLIENT_ID: LazyLock<String> = LazyLock::new(|| {
    std::env::var("SPOTIFY_CLIENT_ID").expect("SPOTIFY_CLIENT_ID must be set.")
});

static CLIENT_SECRET: LazyLock<String> = LazyLock::new(|| {
    std::env::var("SPOTIFY_CLIENT_SECRET").expect("SPOTIFY_CLIENT_SECRET must be set.")
});

static REFRESH_TOKEN: LazyLock<String> = LazyLock::new(|| {
    std::env::var("SPOTIFY_REFRESH_TOKEN").expect("SPOTIFY_REFRESH_TOKEN must be set.")
});

/// Genres to filter out of the recently-played list.
///
/// Note the ordering here: entries are trimmed *before* the empty check. Doing it
/// the other way round means a value like "comedy, " yields an empty-string entry,
/// and an empty needle matches every genre - which silently excludes every track.
static EXCLUDED_GENRES: LazyLock<Vec<String>> = LazyLock::new(|| {
    std::env::var("SPOTIFY_EXCLUDED_GENRES")
        .unwrap_or_else(|_| "comedy".to_string())
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
});

const CACHE_DURATION_SECS: u64 = 900; // 15 minutes
const NUMBER_OF_TRACKS_TO_SHOW: usize = 6;
/// How many recently-played items to ask Spotify for before genre filtering.
const RECENTLY_PLAYED_FETCH_LIMIT: usize = 25;
/// Spotify access tokens last an hour; refresh a minute early to avoid races.
const TOKEN_EXPIRY_SAFETY_MARGIN_SECS: u64 = 60;
/// Artist lookups are one request per artist since the batch endpoint was removed,
/// so keep the fan-out modest to stay clear of the rate limiter.
const ARTIST_FETCH_CONCURRENCY: usize = 4;
/// If artist lookups come back 401/403/404 the endpoint is not available to this
/// app at all. Stop asking for a while rather than burning quota every refresh.
const ARTIST_LOOKUP_COOLDOWN_SECS: u64 = 6 * 3600;

// Cache structure to store access token and its expiry
#[derive(Debug, Clone)]
struct TokenCacheEntry {
    access_token: String,
    expires_at: SystemTime,
}

// Cache structure to store recently played tracks and timestamp
#[derive(Debug, Clone)]
struct TracksCacheEntry {
    tracks: Vec<SpotifyTrack>,
    timestamp: SystemTime,
}

// Global cache for access token
static TOKEN_CACHE: LazyLock<Mutex<Option<TokenCacheEntry>>> = LazyLock::new(|| {
    Mutex::new(None)
});

// Global cache for recently played tracks
static TRACKS_CACHE: LazyLock<Mutex<Option<TracksCacheEntry>>> = LazyLock::new(|| {
    Mutex::new(None)
});

// Artist genres effectively never change, so cache them for the life of the process.
static ARTIST_GENRE_CACHE: LazyLock<Mutex<HashMap<String, Vec<String>>>> = LazyLock::new(|| {
    Mutex::new(HashMap::new())
});

// Set when Spotify tells us the artist endpoint is not available to this app.
static ARTIST_LOOKUP_DISABLED_UNTIL: LazyLock<Mutex<Option<SystemTime>>> = LazyLock::new(|| {
    Mutex::new(None)
});

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpotifyTrack {
    pub track_name: String,
    pub artist: String,
    pub album_name: String,
    pub played_at: String,
    pub spotify_url: String,
    pub album_image_url: Option<String>,
    pub genres: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[allow(dead_code)]
    token_type: Option<String>,
    expires_in: Option<u64>,
    #[allow(dead_code)]
    scope: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RecentlyPlayedResponse {
    /// Deserialized item-by-item rather than straight into `Vec<PlayHistoryObject>`:
    /// with a typed vec, a single unexpected entry fails the whole response and the
    /// endpoint returns nothing at all.
    #[serde(default)]
    items: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct PlayHistoryObject {
    #[serde(alias = "item")]
    track: TrackObject,
    played_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TrackObject {
    name: Option<String>,
    album: Option<AlbumObject>,
    #[serde(default)]
    artists: Vec<ArtistObject>,
    external_urls: Option<ExternalUrls>,
    id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AlbumObject {
    name: Option<String>,
    #[serde(default)]
    images: Vec<ImageObject>,
}

#[derive(Debug, Deserialize)]
struct ArtistObject {
    name: Option<String>,
    id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ImageObject {
    url: String,
    #[allow(dead_code)]
    height: Option<u32>,
    #[allow(dead_code)]
    width: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct ExternalUrls {
    spotify: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FullArtistObject {
    id: String,
    #[allow(dead_code)]
    name: Option<String>,
    #[serde(default)]
    genres: Vec<String>,
}

/// Outcome of a single artist lookup.
enum ArtistFetch {
    Found(String, Vec<String>),
    /// Spotify refused the lookup outright (removed endpoint / insufficient access).
    Unavailable(u16),
    /// Transient problem - worth retrying on a later refresh.
    Failed,
}

/// Truncate a body for logging so a huge error page cannot flood the log.
fn snippet(body: &str, max_chars: usize) -> String {
    let trimmed = body.trim();
    if trimmed.chars().count() <= max_chars {
        trimmed.to_string()
    } else {
        let head: String = trimmed.chars().take(max_chars).collect();
        format!("{}…", head)
    }
}

/// True when a token-endpoint failure means the refresh token itself is dead
/// (revoked, or the user withdrew the app's access) rather than something
/// transient. Spotify signals this with OAuth's `invalid_grant`.
fn is_revoked_grant(body: &str) -> bool {
    body.contains("invalid_grant")
}

fn artist_lookup_disabled() -> bool {
    let lock = ARTIST_LOOKUP_DISABLED_UNTIL.lock().unwrap();
    match *lock {
        Some(until) => SystemTime::now() < until,
        None => false,
    }
}

fn disable_artist_lookup(status: u16) {
    let mut lock = ARTIST_LOOKUP_DISABLED_UNTIL.lock().unwrap();
    *lock = Some(SystemTime::now() + Duration::from_secs(ARTIST_LOOKUP_COOLDOWN_SECS));
    log::warn!(
        "Spotify artist lookup returned {}; genre data is unavailable to this app. \
         Pausing artist lookups for {}s and serving tracks unfiltered.",
        status,
        ARTIST_LOOKUP_COOLDOWN_SECS
    );
}

async fn fetch_single_artist(artist_id: String, access_token: String) -> ArtistFetch {
    let mut response = match surf::get(format!("https://api.spotify.com/v1/artists/{}", artist_id))
        .header("Authorization", format!("Bearer {}", access_token))
        .await
    {
        Ok(resp) => resp,
        Err(e) => {
            log::error!("Failed to fetch artist {}: {}", artist_id, e);
            return ArtistFetch::Failed;
        }
    };

    let status = u16::from(response.status());

    if response.status().is_success() {
        return match response.body_json::<FullArtistObject>().await {
            Ok(artist) => ArtistFetch::Found(artist.id, artist.genres),
            Err(e) => {
                log::error!("Failed to parse artist {} response: {}", artist_id, e);
                ArtistFetch::Failed
            }
        };
    }

    let error_text = response.body_string().await.unwrap_or_else(|_| "Unknown error".to_string());
    log::error!("Failed to get artist {}: {} - {}", artist_id, status, snippet(&error_text, 300));

    // 401/403/404/410 mean "this app cannot have this data", not "try again in a second".
    if matches!(status, 401 | 403 | 404 | 410) {
        ArtistFetch::Unavailable(status)
    } else {
        ArtistFetch::Failed
    }
}

/// Look up genres for the given artists. Best effort: a failure here yields fewer
/// genres, never an error, because genres are only used to *filter* the track list
/// and a lookup outage must not empty the whole "now playing" section.
async fn get_artists_with_genres(artist_ids: Vec<String>, access_token: &str) -> HashMap<String, Vec<String>> {
    let mut result: HashMap<String, Vec<String>> = HashMap::new();
    if artist_ids.is_empty() {
        return result;
    }

    // Serve what we already know before going near the network.
    let mut to_fetch: Vec<String> = Vec::new();
    {
        let cache = ARTIST_GENRE_CACHE.lock().unwrap();
        for id in artist_ids {
            match cache.get(&id) {
                Some(genres) => { result.insert(id, genres.clone()); }
                None => to_fetch.push(id),
            }
        }
    }

    if to_fetch.is_empty() {
        log::info!("Artist genre cache served all {} artists", result.len());
        return result;
    }

    if artist_lookup_disabled() {
        log::info!(
            "Skipping {} artist genre lookups: artist endpoint is in cooldown",
            to_fetch.len()
        );
        return result;
    }

    let start_time = Instant::now();
    let requested = to_fetch.len();

    // One request per artist: the batch "Get Several Artists" endpoint
    // (GET /v1/artists?ids=) was removed in the February 2026 Web API changes.
    // Bounded concurrency keeps the burst away from the rate limiter.
    let outcomes: Vec<ArtistFetch> = futures::stream::iter(to_fetch)
        .map(|id| fetch_single_artist(id, access_token.to_string()))
        .buffer_unordered(ARTIST_FETCH_CONCURRENCY)
        .collect()
        .await;

    let mut unavailable_status: Option<u16> = None;
    let mut fetched = 0usize;
    {
        let mut cache = ARTIST_GENRE_CACHE.lock().unwrap();
        for outcome in outcomes {
            match outcome {
                ArtistFetch::Found(id, genres) => {
                    fetched += 1;
                    cache.insert(id.clone(), genres.clone());
                    result.insert(id, genres);
                }
                ArtistFetch::Unavailable(status) => unavailable_status = Some(status),
                ArtistFetch::Failed => {}
            }
        }
    }

    if let Some(status) = unavailable_status {
        disable_artist_lookup(status);
    }

    log::info!(
        "Fetched genres for {}/{} artists in {:?}",
        fetched,
        requested,
        start_time.elapsed()
    );

    result
}

/// Exchange the refresh token for a fresh access token. Returns the token and its
/// lifetime; errors carry the HTTP status and body so the cause is visible in logs.
async fn request_new_token() -> Result<(String, u64), String> {
    let basic = base64::engine::general_purpose::STANDARD
        .encode(format!("{}:{}", *CLIENT_ID, *CLIENT_SECRET));

    let body = surf::Body::from_form(&[
        ("grant_type", "refresh_token"),
        ("refresh_token", REFRESH_TOKEN.as_str()),
    ]).map_err(|e| format!("Failed to create request body: {}", e))?;

    let mut response = surf::post("https://accounts.spotify.com/api/token")
        .header("Authorization", format!("Basic {}", basic))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .await
        .map_err(|e| format!("Failed to reach accounts.spotify.com: {}", e))?;

    let status = u16::from(response.status());

    if !response.status().is_success() {
        let error_text = response.body_string()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());

        // `invalid_grant` means the refresh token is dead - revoked, or the app's
        // access was withdrawn. No amount of retrying fixes it, and it needs a
        // person to re-run the authorization flow, so say so plainly rather than
        // leaving a bare status code in the log.
        if is_revoked_grant(&error_text) {
            return Err(format!(
                "Refresh token is no longer valid (HTTP {} - {}). This needs a human: \
                 re-run `python3 spotify_reauth.py --write-env` as the account owner \
                 and restart the service.",
                status,
                snippet(&error_text, 300)
            ));
        }

        return Err(format!(
            "Token refresh failed: HTTP {} - {}",
            status,
            snippet(&error_text, 500)
        ));
    }

    let token_response: TokenResponse = response.body_json()
        .await
        .map_err(|e| format!("Token refresh returned unparseable JSON: {}", e))?;

    let expires_in = token_response.expires_in.unwrap_or(3600);
    Ok((token_response.access_token, expires_in))
}

async fn get_access_token() -> Result<String, String> {
    let start_time = Instant::now();

    // Check cache first
    {
        let cache_lock = TOKEN_CACHE.lock().unwrap();
        if let Some(cache_entry) = &*cache_lock {
            if SystemTime::now() < cache_entry.expires_at {
                log::info!("Access token cache hit");
                return Ok(cache_entry.access_token.clone());
            }
            log::info!("Access token cache expired");
        } else {
            log::info!("Access token cache miss");
        }
    }

    let (access_token, expires_in) = request_new_token().await?;

    {
        let lifetime = expires_in.saturating_sub(TOKEN_EXPIRY_SAFETY_MARGIN_SECS).max(30);
        let mut cache_lock = TOKEN_CACHE.lock().unwrap();
        *cache_lock = Some(TokenCacheEntry {
            access_token: access_token.clone(),
            expires_at: SystemTime::now() + Duration::from_secs(lifetime),
        });
        log::info!("Access token cache updated (valid for {}s)", lifetime);
    }

    log::info!("Total get_access_token took: {:?}", start_time.elapsed());
    Ok(access_token)
}

fn invalidate_token_cache() {
    let mut cache_lock = TOKEN_CACHE.lock().unwrap();
    *cache_lock = None;
}

/// Clear every Spotify cache. Used by `no_cache=true`.
pub fn clear_caches() {
    invalidate_token_cache();
    let mut tracks_cache = TRACKS_CACHE.lock().unwrap();
    *tracks_cache = None;
    let mut disabled = ARTIST_LOOKUP_DISABLED_UNTIL.lock().unwrap();
    *disabled = None;
    log::info!("Spotify caches cleared");
}

/// Fetch the raw recently-played payload, retrying once on a 401 with a fresh token.
/// Returns (http status, body text).
async fn fetch_recently_played_raw(limit: usize) -> Result<(u16, String), String> {
    let url = format!(
        "https://api.spotify.com/v1/me/player/recently-played?limit={}",
        limit
    );

    let mut access_token = get_access_token().await?;

    for attempt in 0..2 {
        let mut response = surf::get(&url)
            .header("Authorization", format!("Bearer {}", access_token))
            .await
            .map_err(|e| format!("Failed to reach api.spotify.com: {}", e))?;

        let status = u16::from(response.status());

        // A cached token can be revoked server-side; take exactly one more run at it.
        if status == 401 && attempt == 0 {
            log::warn!("Recently-played returned 401; refreshing access token and retrying once");
            invalidate_token_cache();
            access_token = get_access_token().await?;
            continue;
        }

        let body = response.body_string()
            .await
            .map_err(|e| format!("Failed to read recently-played body: {}", e))?;

        return Ok((status, body));
    }

    unreachable!("retry loop always returns on the second attempt")
}

/// Parse the play-history items one at a time, skipping (and logging) any entry
/// Spotify sends that we cannot make sense of. Returns the parsed items plus the
/// number skipped and the first parse error, for diagnostics.
fn parse_play_history(items: &[serde_json::Value]) -> (Vec<PlayHistoryObject>, usize, Option<String>) {
    let mut parsed = Vec::with_capacity(items.len());
    let mut skipped = 0usize;
    let mut first_error: Option<String> = None;

    for raw in items {
        match serde_json::from_value::<PlayHistoryObject>(raw.clone()) {
            Ok(item) => parsed.push(item),
            Err(e) => {
                skipped += 1;
                let message = e.to_string();
                log::warn!(
                    "Skipping unparseable recently-played item: {} - raw: {}",
                    message,
                    snippet(&raw.to_string(), 400)
                );
                if first_error.is_none() {
                    first_error = Some(message);
                }
            }
        }
    }

    (parsed, skipped, first_error)
}

fn to_spotify_track(item: &PlayHistoryObject, artist_genres: &HashMap<String, Vec<String>>) -> Option<SpotifyTrack> {
    // Without a name there is nothing worth showing.
    let track_name = item.track.name.clone()?;

    let artist_ids: Vec<String> = item.track.artists.iter()
        .filter_map(|a| a.id.clone())
        .collect();
    let genres = aggregate_genres_for_track(&artist_ids, artist_genres);

    let spotify_url = item.track.external_urls.as_ref()
        .and_then(|urls| urls.spotify.clone())
        .or_else(|| item.track.id.as_ref().map(|id| format!("https://open.spotify.com/track/{}", id)))
        .unwrap_or_default();

    Some(SpotifyTrack {
        track_name,
        artist: item.track.artists.first()
            .and_then(|artist| artist.name.clone())
            .unwrap_or_default(),
        album_name: item.track.album.as_ref()
            .and_then(|album| album.name.clone())
            .unwrap_or_default(),
        played_at: item.played_at.clone().unwrap_or_default(),
        spotify_url,
        album_image_url: item.track.album.as_ref()
            .and_then(|album| album.images.first().map(|image| image.url.clone())),
        genres,
    })
}

pub async fn get_recently_played(limit: usize) -> Result<Vec<SpotifyTrack>, String> {
    let start_time = Instant::now();

    // Check cache first
    {
        let cache_lock = TRACKS_CACHE.lock().unwrap();
        if let Some(cache_entry) = &*cache_lock {
            if let Ok(elapsed) = cache_entry.timestamp.elapsed() {
                if elapsed < Duration::from_secs(CACHE_DURATION_SECS) {
                    log::info!("Recently played tracks cache hit");
                    return Ok(cache_entry.tracks.iter().take(limit).cloned().collect());
                }
                log::info!("Recently played tracks cache expired");
            }
        } else {
            log::info!("Recently played tracks cache miss");
        }
    }

    // Fetch more tracks than needed to leave room for genre filtering.
    let (status, body) = fetch_recently_played_raw(RECENTLY_PLAYED_FETCH_LIMIT).await?;

    if !(200..300).contains(&status) {
        return Err(format!(
            "Failed to get recently played tracks: HTTP {} - {}",
            status,
            snippet(&body, 500)
        ));
    }

    let recently_played: RecentlyPlayedResponse = serde_json::from_str(&body)
        .map_err(|e| format!(
            "Failed to parse recently-played response: {} - body: {}",
            e,
            snippet(&body, 500)
        ))?;

    let (items, skipped, _) = parse_play_history(&recently_played.items);
    if skipped > 0 {
        log::warn!(
            "Skipped {}/{} recently-played items that failed to parse",
            skipped,
            recently_played.items.len()
        );
    }

    // Unique artist IDs across all the parsed items.
    let artist_ids: Vec<String> = items.iter()
        .flat_map(|item| item.track.artists.iter().filter_map(|artist| artist.id.clone()))
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    let artist_genres = get_artists_with_genres(artist_ids, &get_access_token().await?).await;

    let all_tracks: Vec<SpotifyTrack> = items.iter()
        .filter_map(|item| to_spotify_track(item, &artist_genres))
        .collect();

    let before_filter = all_tracks.len();
    let tracks: Vec<SpotifyTrack> = all_tracks.into_iter()
        .filter(|track| !should_exclude_track(&track.genres, &EXCLUDED_GENRES))
        .collect();

    log::info!(
        "Recently played: {} items returned, {} unparseable, {} tracks built, {} after genre filtering (excluded genres: {:?})",
        recently_played.items.len(),
        skipped,
        before_filter,
        tracks.len(),
        *EXCLUDED_GENRES
    );

    if before_filter > 0 && tracks.is_empty() {
        log::warn!("Genre filtering removed every track - check SPOTIFY_EXCLUDED_GENRES");
    }

    // Update cache with all filtered tracks
    {
        let mut cache_lock = TRACKS_CACHE.lock().unwrap();
        *cache_lock = Some(TracksCacheEntry {
            tracks: tracks.clone(),
            timestamp: SystemTime::now(),
        });
        log::info!("Recently played tracks cache updated");
    }

    let limited_tracks: Vec<SpotifyTrack> = tracks.into_iter().take(limit).collect();

    log::info!(
        "Total get_recently_played took: {:?}, returning {} tracks",
        start_time.elapsed(),
        limited_tracks.len()
    );

    Ok(limited_tracks)
}

fn aggregate_genres_for_track(
    track_artist_ids: &[String],
    artist_genres: &HashMap<String, Vec<String>>,
) -> Vec<String> {
    let mut track_genres: Vec<String> = Vec::new();
    for artist_id in track_artist_ids {
        if let Some(genres) = artist_genres.get(artist_id) {
            track_genres.extend(genres.clone());
        }
    }
    track_genres.sort();
    track_genres.dedup();
    track_genres
}

fn should_exclude_track(track_genres: &[String], excluded_genres: &[String]) -> bool {
    if excluded_genres.is_empty() {
        return false;
    }
    track_genres.iter().any(|genre| {
        let genre_lower = genre.trim().to_lowercase();
        if genre_lower.is_empty() {
            return false;
        }
        excluded_genres.iter().any(|excluded| {
            !excluded.is_empty() && (genre_lower.contains(excluded) || excluded.contains(&genre_lower))
        })
    })
}

pub async fn get_spotify_tracks(req: Request<()>) -> tide::Result<Response> {
    let start_time = Instant::now();

    // Check for API key in the request headers
    if !auth::validate_api_key(&req) {
        return Ok(Response::new(StatusCode::Unauthorized));
    }

    // Get the limit from query parameters, or use default
    let limit = req.url().query_pairs()
        .find(|(k, _)| k == "limit")
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(NUMBER_OF_TRACKS_TO_SHOW);

    // Get optional no_cache parameter
    let no_cache = req.url().query_pairs()
        .find(|(k, _)| k == "no_cache")
        .map(|(_, v)| v == "true")
        .unwrap_or(false);

    log::debug!("API endpoint setup took: {:?}", start_time.elapsed());

    if no_cache {
        clear_caches();
    }

    match get_recently_played(limit).await {
        Ok(tracks) => {
            log::info!("Tracks fetch completed in: {:?}", start_time.elapsed());

            let mut res = Response::new(StatusCode::Ok);
            res.set_content_type("application/json");
            res.insert_header("Cache-Control", "no-store");
            res.set_body(json!({ "tracks": tracks }));

            log::info!("Total API request handled in: {:?}", start_time.elapsed());
            Ok(res)
        },
        Err(e) => {
            log::error!("Error fetching Spotify recently played tracks after {:?}: {}", start_time.elapsed(), e);

            let mut res = Response::new(StatusCode::InternalServerError);
            res.set_content_type("application/json");
            res.insert_header("Cache-Control", "no-store");
            res.set_body(json!({
                "error": "Could not load recently played tracks.",
                "detail": e,
            }));

            Ok(res)
        }
    }
}

/// Authenticated, cache-bypassing health check that reports exactly where the
/// Spotify pipeline breaks. Reaches every stage even when an earlier one fails,
/// so one request tells you whether it is credentials, the endpoint, or filtering.
pub async fn diagnose(req: Request<()>) -> tide::Result<Response> {
    if !auth::validate_api_key(&req) {
        return Ok(Response::new(StatusCode::Unauthorized));
    }

    let mut report = json!({
        "config": {
            "client_id_set": std::env::var("SPOTIFY_CLIENT_ID").map(|v| !v.is_empty()).unwrap_or(false),
            "client_secret_set": std::env::var("SPOTIFY_CLIENT_SECRET").map(|v| !v.is_empty()).unwrap_or(false),
            "refresh_token_set": std::env::var("SPOTIFY_REFRESH_TOKEN").map(|v| !v.is_empty()).unwrap_or(false),
            "excluded_genres": &*EXCLUDED_GENRES,
        },
    });

    // Step 1: token refresh, always live.
    let token = match request_new_token().await {
        Ok((token, expires_in)) => {
            report["token_refresh"] = json!({ "ok": true, "expires_in": expires_in });
            Some(token)
        }
        Err(e) => {
            report["token_refresh"] = json!({ "ok": false, "error": e });
            None
        }
    };

    let token = match token {
        Some(t) => t,
        None => {
            let revoked = report["token_refresh"]["error"]
                .as_str()
                .map(|e| is_revoked_grant(e) || e.contains("no longer valid"))
                .unwrap_or(false);

            report["conclusion"] = json!(if revoked {
                "The refresh token has been revoked. The client ID and secret are \
                 probably still fine - only the user grant is gone. Re-run \
                 `python3 spotify_reauth.py --write-env` as the account owner, then \
                 restart the service. To confirm the app credentials separately, try a \
                 client_credentials grant: if that returns 200, the secret is valid and \
                 only the refresh token needs replacing."
            } else {
                "Token refresh failed. Check SPOTIFY_CLIENT_ID / SPOTIFY_CLIENT_SECRET / \
                 SPOTIFY_REFRESH_TOKEN, and that the app still exists and the owner has \
                 an active Spotify Premium subscription (required for Development Mode \
                 apps since February 2026)."
            });
            let mut res = Response::new(StatusCode::Ok);
            res.set_content_type("application/json");
            res.insert_header("Cache-Control", "no-store");
            res.set_body(report);
            return Ok(res);
        }
    };

    // Step 2: recently-played, live.
    let url = format!(
        "https://api.spotify.com/v1/me/player/recently-played?limit={}",
        RECENTLY_PLAYED_FETCH_LIMIT
    );
    let raw = match surf::get(&url).header("Authorization", format!("Bearer {}", token)).await {
        Ok(mut response) => {
            let status = u16::from(response.status());
            let body = response.body_string().await.unwrap_or_default();
            Some((status, body))
        }
        Err(e) => {
            report["recently_played"] = json!({ "ok": false, "error": e.to_string() });
            None
        }
    };

    let mut first_artist_id: Option<String> = None;

    if let Some((status, body)) = raw {
        if !(200..300).contains(&status) {
            report["recently_played"] = json!({
                "ok": false,
                "status": status,
                "body": snippet(&body, 800),
            });
            report["conclusion"] = json!(
                "The recently-played endpoint rejected the request. A 403 usually means \
                 this app no longer has access to the endpoint (February 2026 Development \
                 Mode restrictions) or the account is not on the app's allow-list."
            );
        } else {
            match serde_json::from_str::<RecentlyPlayedResponse>(&body) {
                Ok(parsed) => {
                    let (items, skipped, first_error) = parse_play_history(&parsed.items);
                    first_artist_id = items.iter()
                        .flat_map(|i| i.track.artists.iter())
                        .filter_map(|a| a.id.clone())
                        .next();

                    let empty_genres: HashMap<String, Vec<String>> = HashMap::new();
                    let built: Vec<SpotifyTrack> = items.iter()
                        .filter_map(|item| to_spotify_track(item, &empty_genres))
                        .collect();

                    report["recently_played"] = json!({
                        "ok": true,
                        "status": status,
                        "items_returned": parsed.items.len(),
                        "items_parsed": items.len(),
                        "items_skipped": skipped,
                        "first_parse_error": first_error,
                        "tracks_built": built.len(),
                    });

                    if parsed.items.is_empty() {
                        report["conclusion"] = json!(
                            "Spotify returned an empty listening history. Nothing has been \
                             played recently, or the refresh token belongs to a different account."
                        );
                    }
                }
                Err(e) => {
                    report["recently_played"] = json!({
                        "ok": false,
                        "status": status,
                        "error": format!("unparseable response: {}", e),
                        "body": snippet(&body, 800),
                    });
                    report["conclusion"] = json!(
                        "Spotify answered 200 but the response shape is not what we expect. \
                         Compare the body above against the structs in src/spotify.rs."
                    );
                }
            }
        }
    }

    // Step 3: one artist lookup, to see whether genre data is reachable at all.
    match first_artist_id {
        Some(artist_id) => {
            match surf::get(format!("https://api.spotify.com/v1/artists/{}", artist_id))
                .header("Authorization", format!("Bearer {}", token))
                .await
            {
                Ok(mut response) => {
                    let status = u16::from(response.status());
                    let body = response.body_string().await.unwrap_or_default();
                    let genres = serde_json::from_str::<FullArtistObject>(&body)
                        .ok()
                        .map(|a| a.genres);
                    report["artist_lookup"] = json!({
                        "ok": (200..300).contains(&status),
                        "artist_id": artist_id,
                        "status": status,
                        "genres": genres,
                        "body": if (200..300).contains(&status) { None } else { Some(snippet(&body, 500)) },
                    });
                }
                Err(e) => {
                    report["artist_lookup"] = json!({ "ok": false, "error": e.to_string() });
                }
            }
        }
        None => {
            report["artist_lookup"] = json!({ "skipped": "no artist id available from recently-played" });
        }
    }

    if report.get("conclusion").is_none() {
        report["conclusion"] = json!(
            "Every stage succeeded. If /aggregated still returns an empty tracks array, \
             the genre filter is removing everything - compare excluded_genres above \
             against the genres reported by artist_lookup."
        );
    }

    let mut res = Response::new(StatusCode::Ok);
    res.set_content_type("application/json");
    res.insert_header("Cache-Control", "no-store");
    res.set_body(report);
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_artist_object_deserializes_from_individual_endpoint_response() {
        // This is the shape returned by GET /artists/{id} (individual endpoint)
        // as opposed to the old batch GET /artists?ids= which wrapped in { "artists": [...] }
        let json = r#"{
            "id": "06HL4z0CvFAxyc27GXpf02",
            "name": "Taylor Swift",
            "genres": ["pop", "singer-songwriter pop"],
            "external_urls": { "spotify": "https://open.spotify.com/artist/06HL4z0CvFAxyc27GXpf02" },
            "followers": { "total": 100000000 },
            "href": "https://api.spotify.com/v1/artists/06HL4z0CvFAxyc27GXpf02",
            "images": [],
            "popularity": 100,
            "type": "artist",
            "uri": "spotify:artist:06HL4z0CvFAxyc27GXpf02"
        }"#;

        let artist: FullArtistObject = serde_json::from_str(json).unwrap();
        assert_eq!(artist.id, "06HL4z0CvFAxyc27GXpf02");
        assert_eq!(artist.genres, vec!["pop", "singer-songwriter pop"]);
    }

    #[test]
    fn full_artist_object_deserializes_with_empty_genres() {
        let json = r#"{
            "id": "abc123",
            "name": "Unknown Artist",
            "genres": []
        }"#;

        let artist: FullArtistObject = serde_json::from_str(json).unwrap();
        assert_eq!(artist.id, "abc123");
        assert!(artist.genres.is_empty());
    }

    #[test]
    fn full_artist_object_deserializes_without_genres_field() {
        // February 2026 stripped fields from artist payloads; genres may simply be absent.
        let artist: FullArtistObject = serde_json::from_str(r#"{"id": "abc123"}"#).unwrap();
        assert!(artist.genres.is_empty());
    }

    #[test]
    fn aggregate_genres_deduplicates_and_sorts() {
        let mut artist_genres = HashMap::new();
        artist_genres.insert("a1".to_string(), vec!["rock".to_string(), "indie".to_string()]);
        artist_genres.insert("a2".to_string(), vec!["rock".to_string(), "alternative".to_string()]);

        let result = aggregate_genres_for_track(
            &["a1".to_string(), "a2".to_string()],
            &artist_genres,
        );

        assert_eq!(result, vec!["alternative", "indie", "rock"]);
    }

    #[test]
    fn aggregate_genres_handles_missing_artist() {
        let mut artist_genres = HashMap::new();
        artist_genres.insert("a1".to_string(), vec!["pop".to_string()]);

        let result = aggregate_genres_for_track(
            &["a1".to_string(), "missing_id".to_string()],
            &artist_genres,
        );

        assert_eq!(result, vec!["pop"]);
    }

    #[test]
    fn aggregate_genres_empty_input() {
        let artist_genres = HashMap::new();
        let result = aggregate_genres_for_track(&[], &artist_genres);
        assert!(result.is_empty());
    }

    #[test]
    fn should_exclude_matches_substring() {
        let genres = vec!["stand-up comedy".to_string(), "rock".to_string()];
        let excluded = vec!["comedy".to_string()];
        assert!(should_exclude_track(&genres, &excluded));
    }

    #[test]
    fn should_exclude_no_match() {
        let genres = vec!["indie rock".to_string(), "alternative".to_string()];
        let excluded = vec!["comedy".to_string()];
        assert!(!should_exclude_track(&genres, &excluded));
    }

    #[test]
    fn should_exclude_empty_excluded_list() {
        let genres = vec!["comedy".to_string()];
        let excluded: Vec<String> = vec![];
        assert!(!should_exclude_track(&genres, &excluded));
    }

    #[test]
    fn should_exclude_reverse_substring_match() {
        // "comedy" excluded genre contains "com" track genre
        let genres = vec!["com".to_string()];
        let excluded = vec!["comedy".to_string()];
        assert!(should_exclude_track(&genres, &excluded));
    }

    #[test]
    fn should_exclude_case_insensitive() {
        let genres = vec!["Stand-Up Comedy".to_string()];
        let excluded = vec!["comedy".to_string()];
        assert!(should_exclude_track(&genres, &excluded));
    }

    #[test]
    fn empty_excluded_entry_never_matches_everything() {
        // An empty needle is a substring of every genre. Left unguarded this
        // silently excludes the entire track list.
        let genres = vec!["indie rock".to_string()];
        let excluded = vec!["".to_string()];
        assert!(!should_exclude_track(&genres, &excluded));
    }

    #[test]
    fn empty_track_genre_is_not_excluded() {
        let genres = vec!["".to_string()];
        let excluded = vec!["comedy".to_string()];
        assert!(!should_exclude_track(&genres, &excluded));
    }

    #[async_std::test]
    async fn get_artists_with_genres_empty_returns_empty() {
        let result = get_artists_with_genres(vec![], "fake_token").await;
        assert!(result.is_empty());
    }

    #[test]
    fn play_history_parses_minimal_item() {
        let raw: serde_json::Value = serde_json::from_str(r#"{
            "track": {
                "name": "Song",
                "album": { "name": "Album", "images": [{ "url": "https://i.scdn.co/image/x" }] },
                "artists": [{ "name": "Artist", "id": "a1" }],
                "external_urls": { "spotify": "https://open.spotify.com/track/t1" },
                "id": "t1"
            },
            "played_at": "2026-02-27T10:00:00Z"
        }"#).unwrap();

        let (items, skipped, err) = parse_play_history(&[raw]);
        assert_eq!(skipped, 0);
        assert!(err.is_none());
        assert_eq!(items.len(), 1);

        let track = to_spotify_track(&items[0], &HashMap::new()).unwrap();
        assert_eq!(track.track_name, "Song");
        assert_eq!(track.artist, "Artist");
        assert_eq!(track.spotify_url, "https://open.spotify.com/track/t1");
        assert_eq!(track.album_image_url.as_deref(), Some("https://i.scdn.co/image/x"));
    }

    #[test]
    fn play_history_tolerates_null_image_dimensions() {
        // Spotify sends null height/width for some images; a u32 field rejects
        // the whole response when that happens.
        let raw: serde_json::Value = serde_json::from_str(r#"{
            "track": {
                "name": "Song",
                "album": { "name": "Album", "images": [{ "url": "https://i.scdn.co/image/x", "height": null, "width": null }] },
                "artists": [{ "name": "Artist", "id": "a1" }],
                "external_urls": { "spotify": "https://open.spotify.com/track/t1" },
                "id": "t1"
            },
            "played_at": "2026-02-27T10:00:00Z"
        }"#).unwrap();

        let (items, skipped, _) = parse_play_history(&[raw]);
        assert_eq!(skipped, 0);
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn play_history_tolerates_stripped_fields() {
        // Development Mode strips metadata fields from payloads. Everything except
        // the track name is optional so a thinned-out payload still renders.
        let raw: serde_json::Value = serde_json::from_str(r#"{
            "track": { "name": "Song", "id": "t1" },
            "played_at": "2026-02-27T10:00:00Z"
        }"#).unwrap();

        let (items, skipped, _) = parse_play_history(&[raw]);
        assert_eq!(skipped, 0);

        let track = to_spotify_track(&items[0], &HashMap::new()).unwrap();
        assert_eq!(track.track_name, "Song");
        // Falls back to a constructed URL when external_urls is absent.
        assert_eq!(track.spotify_url, "https://open.spotify.com/track/t1");
        assert_eq!(track.artist, "");
        assert!(track.album_image_url.is_none());
    }

    #[test]
    fn play_history_accepts_item_alias_for_track() {
        let raw: serde_json::Value = serde_json::from_str(r#"{
            "item": { "name": "Song", "id": "t1" },
            "played_at": "2026-02-27T10:00:00Z"
        }"#).unwrap();

        let (items, skipped, _) = parse_play_history(&[raw]);
        assert_eq!(skipped, 0);
        assert_eq!(items[0].track.name.as_deref(), Some("Song"));
    }

    #[test]
    fn play_history_skips_only_the_bad_item() {
        // One malformed entry used to fail the entire response and empty the page.
        let good: serde_json::Value = serde_json::from_str(r#"{
            "track": { "name": "Good", "id": "t1" },
            "played_at": "2026-02-27T10:00:00Z"
        }"#).unwrap();
        let bad: serde_json::Value = serde_json::from_str(r#"{ "track": "not-an-object" }"#).unwrap();

        let (items, skipped, err) = parse_play_history(&[bad, good]);
        assert_eq!(skipped, 1);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].track.name.as_deref(), Some("Good"));
        assert!(err.is_some());
    }

    #[test]
    fn recently_played_response_tolerates_missing_items() {
        let parsed: RecentlyPlayedResponse = serde_json::from_str(r#"{"next": null}"#).unwrap();
        assert!(parsed.items.is_empty());
    }

    #[test]
    fn revoked_grant_is_recognised() {
        // The exact body Spotify returns for a revoked refresh token.
        assert!(is_revoked_grant(
            r#"{"error":"invalid_grant","error_description":"Refresh token revoked"}"#
        ));
        assert!(is_revoked_grant(
            r#"{"error":"invalid_grant","error_description":"Invalid refresh token"}"#
        ));
    }

    #[test]
    fn other_token_failures_are_not_treated_as_revoked() {
        // A bad client secret is invalid_client, and needs a different fix.
        assert!(!is_revoked_grant(
            r#"{"error":"invalid_client","error_description":"Invalid client secret"}"#
        ));
        assert!(!is_revoked_grant(r#"{"error":"server_error"}"#));
        assert!(!is_revoked_grant(""));
    }

    #[test]
    fn spotify_track_serializes_to_json() {
        let track = SpotifyTrack {
            track_name: "Test Song".to_string(),
            artist: "Test Artist".to_string(),
            album_name: "Test Album".to_string(),
            played_at: "2026-02-27T10:00:00Z".to_string(),
            spotify_url: "https://open.spotify.com/track/abc".to_string(),
            album_image_url: Some("https://i.scdn.co/image/abc".to_string()),
            genres: vec!["pop".to_string(), "rock".to_string()],
        };

        let json = serde_json::to_value(&track).unwrap();
        assert_eq!(json["track_name"], "Test Song");
        assert_eq!(json["genres"][0], "pop");
        assert_eq!(json["genres"][1], "rock");
    }

    #[test]
    fn spotify_track_serializes_with_null_image() {
        let track = SpotifyTrack {
            track_name: "No Image".to_string(),
            artist: "Artist".to_string(),
            album_name: "Album".to_string(),
            played_at: "2026-02-27T10:00:00Z".to_string(),
            spotify_url: "https://open.spotify.com/track/xyz".to_string(),
            album_image_url: None,
            genres: vec![],
        };

        let json = serde_json::to_value(&track).unwrap();
        assert!(json["album_image_url"].is_null());
        assert!(json["genres"].as_array().unwrap().is_empty());
    }
}
