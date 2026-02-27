use serde::{Deserialize, Serialize};
use tide::{log, Request, Response, StatusCode};
use tide::prelude::*;
use std::collections::HashMap;
use std::time::{Instant, Duration, SystemTime};
use std::sync::{LazyLock, Mutex};
use crate::auth;
use surf;
use base64;

static CLIENT_ID: LazyLock<String> = LazyLock::new(|| {
    std::env::var("SPOTIFY_CLIENT_ID").expect("SPOTIFY_CLIENT_ID must be set.")
});

static CLIENT_SECRET: LazyLock<String> = LazyLock::new(|| {
    std::env::var("SPOTIFY_CLIENT_SECRET").expect("SPOTIFY_CLIENT_SECRET must be set.")
});

static REFRESH_TOKEN: LazyLock<String> = LazyLock::new(|| {
    std::env::var("SPOTIFY_REFRESH_TOKEN").expect("SPOTIFY_REFRESH_TOKEN must be set.")
});

static EXCLUDED_GENRES: LazyLock<Vec<String>> = LazyLock::new(|| {
    std::env::var("SPOTIFY_EXCLUDED_GENRES")
        .unwrap_or_else(|_| "comedy".to_string())
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|s| s.trim().to_lowercase())
        .collect()
});

const CACHE_DURATION_SECS: u64 = 900; // 15 minutes
const NUMBER_OF_TRACKS_TO_SHOW: usize = 6;

// Cache structure to store access token and timestamp
#[derive(Debug, Clone)]
struct TokenCacheEntry {
    access_token: String,
    timestamp: SystemTime,
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
    token_type: String,
    #[allow(dead_code)]
    expires_in: u32,
    #[allow(dead_code)]
    scope: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RecentlyPlayedResponse {
    items: Vec<PlayHistoryObject>,
}

#[derive(Debug, Deserialize)]
struct PlayHistoryObject {
    track: TrackObject,
    played_at: String,
}

#[derive(Debug, Deserialize)]
struct TrackObject {
    name: String,
    album: AlbumObject,
    artists: Vec<ArtistObject>,
    external_urls: ExternalUrls,
    id: String,
}

#[derive(Debug, Deserialize)]
struct AlbumObject {
    name: String,
    images: Vec<ImageObject>,
}

#[derive(Debug, Deserialize)]
struct ArtistObject {
    name: String,
    id: String,
}

#[derive(Debug, Deserialize)]
struct ImageObject {
    url: String,
    #[allow(dead_code)]
    height: u32,
    #[allow(dead_code)]
    width: u32,
}

#[derive(Debug, Deserialize)]
struct ExternalUrls {
    spotify: String,
}

#[derive(Debug, Deserialize)]
struct FullArtistObject {
    id: String,
    name: String,
    genres: Vec<String>,
}

async fn fetch_single_artist(artist_id: String, access_token: String) -> Option<(String, Vec<String>)> {
    let mut response = match surf::get(format!("https://api.spotify.com/v1/artists/{}", artist_id))
        .header("Authorization", format!("Bearer {}", access_token))
        .await
    {
        Ok(resp) => resp,
        Err(e) => {
            log::error!("Failed to fetch artist {}: {}", artist_id, e);
            return None;
        }
    };

    if response.status().is_success() {
        match response.body_json::<FullArtistObject>().await {
            Ok(artist) => Some((artist.id, artist.genres)),
            Err(e) => {
                log::error!("Failed to parse artist {} response: {}", artist_id, e);
                None
            }
        }
    } else {
        let error_text = response.body_string().await.unwrap_or_else(|_| "Unknown error".to_string());
        log::error!("Failed to get artist {}: {} - {}", artist_id, response.status(), error_text);
        None
    }
}

async fn get_artists_with_genres(artist_ids: Vec<String>, access_token: &str) -> Result<HashMap<String, Vec<String>>, String> {
    if artist_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let start_time = Instant::now();

    // Fetch each artist individually (batch endpoint removed in March 2026)
    // Run all requests concurrently for performance
    let futures: Vec<_> = artist_ids.iter()
        .map(|id| fetch_single_artist(id.clone(), access_token.to_string()))
        .collect();

    let results = futures::future::join_all(futures).await;

    let all_genres: HashMap<String, Vec<String>> = results.into_iter()
        .flatten()
        .collect();

    let total_time = start_time.elapsed();
    log::info!("Fetched genres for {} artists in {:?}", artist_ids.len(), total_time);

    Ok(all_genres)
}

async fn get_access_token() -> Result<String, String> {
    let start_time = Instant::now();
    
    // Check cache first
    {
        let cache_lock = TOKEN_CACHE.lock().unwrap();
        if let Some(cache_entry) = &*cache_lock {
            if let Ok(elapsed) = cache_entry.timestamp.elapsed() {
                if elapsed < Duration::from_secs(CACHE_DURATION_SECS) {
                    log::info!("Access token cache hit");
                    return Ok(cache_entry.access_token.clone());
                } else {
                    log::info!("Access token cache expired");
                }
            }
        } else {
            log::info!("Access token cache miss");
        }
    }
    
    // Create basic auth header
    let basic = base64::encode(format!("{}:{}", *CLIENT_ID, *CLIENT_SECRET));
    
    // Prepare request body
    let mut body = surf::Body::from_form(&[
        ("grant_type", "refresh_token"),
        ("refresh_token", REFRESH_TOKEN.as_str()),
    ]).map_err(|e| format!("Failed to create request body: {}", e))?;
    
    // Make request to Spotify API
    let mut response = surf::post("https://accounts.spotify.com/api/token")
        .header("Authorization", format!("Basic {}", basic))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .await
        .map_err(|e| format!("Failed to make request to Spotify API: {}", e))?;
    
    // Handle response
    if response.status().is_success() {
        let token_response: TokenResponse = response.body_json()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))?;
        
        let access_token = token_response.access_token;
        
        // Update cache
        {
            let mut cache_lock = TOKEN_CACHE.lock().unwrap();
            *cache_lock = Some(TokenCacheEntry {
                access_token: access_token.clone(),
                timestamp: SystemTime::now(),
            });
            log::info!("Access token cache updated");
        }
        
        let total_time = start_time.elapsed();
        log::info!("Total get_access_token took: {:?}", total_time);
        
        Ok(access_token)
    } else {
        let error_text = response.body_string()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        Err(format!("Failed to get access token: {} - {}", response.status(), error_text))
    }
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
                    // Return limited results from cache
                    let limited_tracks = cache_entry.tracks.iter().take(limit).cloned().collect();
                    return Ok(limited_tracks);
                } else {
                    log::info!("Recently played tracks cache expired");
                }
            }
        } else {
            log::info!("Recently played tracks cache miss");
        }
    }
    
    // Get access token
    let access_token = get_access_token().await?;
    
    // Fetch more tracks than needed to account for filtering
    // Spotify API max is 50, so we'll use that to maximize our chances of getting enough tracks after filtering
    let fetch_limit = 25;
    
    // Make request to Spotify API
    let mut response = surf::get(format!("https://api.spotify.com/v1/me/player/recently-played?limit={}", fetch_limit))
        .header("Authorization", format!("Bearer {}", access_token))
        .await
        .map_err(|e| format!("Failed to make request to Spotify API: {}", e))?;
    
    // Handle response
    if response.status().is_success() {
        let recently_played: RecentlyPlayedResponse = response.body_json()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))?;
        
        // Get unique artist IDs
        let artist_ids: Vec<String> = recently_played.items.iter()
            .flat_map(|item| item.track.artists.iter().map(|artist| artist.id.clone()))
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        
        // Fetch artist genres
        let artist_genres = get_artists_with_genres(artist_ids, &access_token).await?;
        
        // Transform response to simplified format with genres
        let mut tracks: Vec<SpotifyTrack> = Vec::new();
        
        for item in recently_played.items.iter() {
            let artist_ids: Vec<String> = item.track.artists.iter().map(|a| a.id.clone()).collect();
            let track_genres = aggregate_genres_for_track(&artist_ids, &artist_genres);

            if !should_exclude_track(&track_genres, &EXCLUDED_GENRES) {
                tracks.push(SpotifyTrack {
                    track_name: item.track.name.clone(),
                    artist: item.track.artists.first().map(|artist| artist.name.clone()).unwrap_or_default(),
                    album_name: item.track.album.name.clone(),
                    played_at: item.played_at.clone(),
                    spotify_url: item.track.external_urls.spotify.clone(),
                    album_image_url: item.track.album.images.first().map(|image| image.url.clone()),
                    genres: track_genres,
                });
            }
        }
        
        log::info!("Filtered tracks: {} tracks after genre filtering (excluded genres: {:?})", tracks.len(), *EXCLUDED_GENRES);
        
        // Update cache with all filtered tracks
        {
            let mut cache_lock = TRACKS_CACHE.lock().unwrap();
            *cache_lock = Some(TracksCacheEntry {
                tracks: tracks.clone(),
                timestamp: SystemTime::now(),
            });
            log::info!("Recently played tracks cache updated");
        }
        
        // Limit the results to the requested number
        let limited_tracks: Vec<SpotifyTrack> = tracks.into_iter().take(limit).collect();
        
        let total_time = start_time.elapsed();
        log::info!("Total get_recently_played took: {:?}, returning {} tracks", total_time, limited_tracks.len());
        
        Ok(limited_tracks)
    } else {
        let error_text = response.body_string()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        Err(format!("Failed to get recently played tracks: {} - {}", response.status(), error_text))
    }
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
        let genre_lower = genre.to_lowercase();
        excluded_genres.iter().any(|excluded| {
            genre_lower.contains(excluded) || excluded.contains(&genre_lower)
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
        
    let setup_time = start_time.elapsed();
    log::debug!("API endpoint setup took: {:?}", setup_time);
    
    // Clear cache if requested
    if no_cache {
        let mut tracks_cache_lock = TRACKS_CACHE.lock().unwrap();
        *tracks_cache_lock = None;
        
        let mut token_cache_lock = TOKEN_CACHE.lock().unwrap();
        *token_cache_lock = None;
        
        log::info!("Cache cleared due to no_cache parameter");
    }
    
    // Fetch and process recently played tracks
    match get_recently_played(limit).await {
        Ok(tracks) => {
            let fetch_time = start_time.elapsed();
            log::info!("Tracks fetch completed in: {:?}", fetch_time);
            
            let mut res = Response::new(StatusCode::Ok);
            res.set_content_type("application/json");
            res.set_body(json!({ "tracks": tracks }));
            
            let total_time = start_time.elapsed();
            log::info!("Total API request handled in: {:?}", total_time);
            
            Ok(res)
        },
        Err(e) => {
            let error_time = start_time.elapsed();
            log::error!("Error fetching Spotify recently played tracks after {:?}: {}", error_time, e);
            
            let mut res = Response::new(StatusCode::InternalServerError);
            res.set_content_type("application/json");
            res.set_body(json!({ "error": "Could not load recently played tracks." }));
            
            Ok(res)
        }
    }
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
        assert_eq!(artist.name, "Taylor Swift");
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

    #[async_std::test]
    async fn get_artists_with_genres_empty_returns_empty() {
        let result = get_artists_with_genres(vec![], "fake_token").await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
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
