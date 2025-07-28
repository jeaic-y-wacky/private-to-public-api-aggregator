use rss::{Channel, Item};
use serde::{Deserialize, Serialize};
use tide::{log, Request, Response, StatusCode};
use tide::prelude::*;
use std::collections::HashMap;
use std::time::{Instant, Duration, SystemTime};
use std::sync::{LazyLock, Mutex};
use crate::auth;
use crate::config::CONFIG;
use surf;
use url::Url;
use chrono::DateTime;

const LETTERBOXD_NAMESPACE: &str = "letterboxd";

// Cache structure to store results and timestamp
#[derive(Debug, Clone)]
struct CacheEntry {
    movies: Vec<LetterboxdMovie>,
    timestamp: SystemTime,
}

// Global cache for each feed URL
static FEED_CACHE: LazyLock<Mutex<HashMap<String, CacheEntry>>> = LazyLock::new(|| {
    Mutex::new(HashMap::new())
});

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LetterboxdMovie {
    pub title: String,
    pub link: String,
    pub description: String,
    pub pub_date: Option<String>,
    pub film_title: Option<String>,
    pub rating: Option<String>,
    pub rewatch: Option<String>,
}

pub async fn fetch_letterboxd_feed(feed_url: &str) -> Result<Vec<LetterboxdMovie>, String> {
    let start_time = Instant::now();
    
    // Check cache first
    {
        let cache_lock = FEED_CACHE.lock().unwrap();
        if let Some(cache_entry) = cache_lock.get(feed_url) {
            if let Ok(elapsed) = cache_entry.timestamp.elapsed() {
                if elapsed < Duration::from_secs(CONFIG.cache.letterboxd_duration_secs) {
                    log::info!("Cache hit for feed {}", feed_url);
                    return Ok(cache_entry.movies.clone());
                } else {
                    log::info!("Cache expired for feed {}", feed_url);
                }
            }
        } else {
            log::info!("Cache miss for feed {}", feed_url);
        }
    }
    
    let mut current_url = feed_url.to_string();
    let mut response = match surf::get(&current_url).await {
        Ok(resp) => resp,
        Err(e) => return Err(format!("Failed to fetch RSS feed: {}", e)),
    };
    
    // Follow redirects up to a maximum of 10 times, handling relative URLs
    let mut redirect_count = 0;
    while response.status().is_redirection() && redirect_count < 10 {
        if let Some(loc) = response.header("Location") {
            if let Some(value) = loc.iter().next() {
                let loc_str = value.as_str();
                let fixed_loc_str = if loc_str.starts_with("//") {
                    format!("https:{}", loc_str)
                } else {
                    loc_str.to_string()
                };
                let new_url = match Url::parse(&fixed_loc_str) {
                    Ok(url) => url.into_string(),
                    Err(_) => {
                        let base_url = Url::parse(&current_url).map_err(|e| format!("Invalid base URL {}: {}", current_url, e))?;
                        base_url.join(&fixed_loc_str).map_err(|e| format!("Failed to join base URL with relative redirect: {}", e))?.into_string()
                    }
                };
                
                current_url = new_url.clone();
                response = match surf::get(&new_url).await {
                    Ok(resp) => resp,
                    Err(e) => return Err(format!("Failed to follow redirect to {}: {}", new_url, e)),
                };
                redirect_count += 1;
            } else {
                break;
            }
        } else {
            break;
        }
    }
    
    let fetch_time = start_time.elapsed();
    log::info!("Network fetch took: {:?}", fetch_time);
    
    let parse_start = Instant::now();
    
    let content = match response.body_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return Err(format!("Failed to read response body: {}", e)),
    };
    
    // Parse the RSS feed
    let channel = match Channel::read_from(&content[..]) {
        Ok(channel) => channel,
        Err(e) => return Err(format!("Failed to parse RSS feed: {}", e)),
    };
    
    let parse_time = parse_start.elapsed();
    log::info!("RSS parsing took: {:?}", parse_time);
    
    let process_start = Instant::now();
    
    // Process the feed items
    let movies = process_letterboxd_items(channel.items());
    
    let process_time = process_start.elapsed();
    log::info!("Movie processing took: {:?}", process_time);
    
    let total_time = start_time.elapsed();
    log::info!("Total fetch_letterboxd_feed took: {:?}", total_time);
    
    // Update cache with the new results
    {
        let mut cache_lock = FEED_CACHE.lock().unwrap();
        cache_lock.insert(feed_url.to_string(), CacheEntry {
            movies: movies.clone(),
            timestamp: SystemTime::now(),
        });
        log::info!("Cache updated for feed {}", feed_url);
    }
    
    Ok(movies)
}

fn process_letterboxd_items(items: &[Item]) -> Vec<LetterboxdMovie> {
    let start_time = Instant::now();
    
    // Group movies by film title to handle duplicates
    let mut movie_map: HashMap<String, LetterboxdMovie> = HashMap::new();
    
    for item in items {
        log::debug!("Processing item: {}", item.title().unwrap_or_default());
        // Extract Letterboxd-specific fields from extensions
        let film_title = extract_extension_value(item, LETTERBOXD_NAMESPACE, "filmTitle");
        
        // Skip if no film title
        if let Some(film_title) = &film_title {
            log::debug!("Film title: {}", film_title);
            let rating = extract_extension_value(item, LETTERBOXD_NAMESPACE, "memberRating");
            let rewatch = extract_extension_value(item, LETTERBOXD_NAMESPACE, "rewatch");
            
            let movie = LetterboxdMovie {
                title: item.title().unwrap_or_default().to_string(),
                link: item.link().unwrap_or_default().to_string(),
                description: item.description().unwrap_or_default().to_string(),
                pub_date: item.pub_date().map(|s| s.to_string()),
                film_title: Some(film_title.clone()),
                rating,
                rewatch,
            };

            // If we already have an entry for this movie, update with any new info
            if let Some(existing_movie) = movie_map.get_mut(film_title) {
                // Keep the rating if it exists
                if existing_movie.rating.is_none() && movie.rating.is_some() {
                    existing_movie.rating = movie.rating;
                }
                
                // Update title to include rating if original didn't have it
                if !existing_movie.title.contains('★') && movie.title.contains('★') {
                    existing_movie.title = movie.title;
                }
                
                // Keep the most recent review
                if let (Some(existing_date), Some(new_date)) = (&existing_movie.pub_date, &movie.pub_date) {
                    if new_date > existing_date {
                        existing_movie.description = movie.description;
                        existing_movie.pub_date = Some(new_date.clone());
                    }
                }
            } else {
                // Add new movie to the map
                movie_map.insert(film_title.clone(), movie);
            }
        } else {
            log::debug!("No film title found");
        }
    }
    
    let processing_time = start_time.elapsed();
    log::debug!("Movie map processing took: {:?}", processing_time);
    
    let sorting_start = Instant::now();
    
    // Convert hashmap to vector
    let mut movies: Vec<LetterboxdMovie> = movie_map.values().cloned().collect();
    
    // Sort by publication date (most recent first)
    movies.sort_by(|a, b| {
        match (&a.pub_date, &b.pub_date) {
            (Some(a_date), Some(b_date)) => {
                // Parse the RFC2822 dates
                let a_parsed = DateTime::parse_from_rfc2822(a_date);
                let b_parsed = DateTime::parse_from_rfc2822(b_date);
                
                match (a_parsed, b_parsed) {
                    (Ok(a_dt), Ok(b_dt)) => b_dt.cmp(&a_dt), // Most recent first
                    _ => b_date.cmp(a_date),  // Fallback to string comparison if parse fails
                }
            },
            (None, Some(_)) => std::cmp::Ordering::Less,
            (Some(_), None) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
    });
    
    let sorting_time = sorting_start.elapsed();
    log::debug!("Sorting movies took: {:?}", sorting_time);
    
    // Limit to the number of movies to show
    movies.truncate(CONFIG.letterboxd.movies_limit);
    
    let total_time = start_time.elapsed();
    log::debug!("Total process_letterboxd_items took: {:?}", total_time);
    
    movies
}

fn extract_extension_value(item: &Item, namespace: &str, key: &str) -> Option<String> {
    item.extensions().get(namespace)
        .and_then(|ext| ext.get(key))
        .and_then(|values| values.first())
        .and_then(|value| value.value().map(|s| s.to_string()))
}

pub async fn get_letterboxd_movies(req: Request<()>) -> tide::Result<Response> {
    let start_time = Instant::now();
    
    // Check for API key in the request headers
    if !auth::validate_api_key(&req) {
        return Ok(Response::new(StatusCode::Unauthorized));
    }
    
    // Get the feed URL from query parameters, or use default
    let feed_url = req.url().query_pairs()
        .find(|(k, _)| k == "feed_url")
        .map(|(_, v)| v.to_string())
        .unwrap_or_else(|| CONFIG.letterboxd.default_feed_url.clone());
    
    // Get optional no_cache parameter
    let no_cache = req.url().query_pairs()
        .find(|(k, _)| k == "no_cache")
        .map(|(_, v)| v == "true")
        .unwrap_or(false);
        
    let setup_time = start_time.elapsed();
    log::debug!("API endpoint setup took: {:?}", setup_time);
    
    // Clear cache if requested
    if no_cache {
        let mut cache_lock = FEED_CACHE.lock().unwrap();
        cache_lock.remove(&feed_url);
        log::info!("Cache cleared for feed {} due to no_cache parameter", feed_url);
    }
    
    // Fetch and process the feed
    match fetch_letterboxd_feed(&feed_url).await {
        Ok(movies) => {
            let fetch_time = start_time.elapsed();
            log::info!("Feed fetch completed in: {:?}", fetch_time);
            
            let mut res = Response::new(StatusCode::Ok);
            res.set_content_type("application/json");
            res.set_body(json!({ "movies": movies }));
            
            let total_time = start_time.elapsed();
            log::info!("Total API request handled in: {:?}", total_time);
            
            Ok(res)
        },
        Err(e) => {
            let error_time = start_time.elapsed();
            log::error!("Error fetching Letterboxd RSS feed after {:?}: {}", error_time, e);
            
            let mut res = Response::new(StatusCode::InternalServerError);
            res.set_content_type("application/json");
            res.set_body(json!({ "error": "Could not load watched movies." }));
            
            Ok(res)
        }
    }
} 