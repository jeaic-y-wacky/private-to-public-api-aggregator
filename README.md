# API Aggregator for Static Sites

A Rust-based API aggregator that provides endpoints for Spotify recently played tracks, Letterboxd watched movies, and URL webhook functionality. This service can be used to add dynamic content to static websites.

## Setup

1. Clone the repository
2. Create configuration:
   - Copy `config.toml` to `config.local.toml` for local customization (optional)
   - Or set environment variables as described below
3. Set required environment variables:
   ```
   API_KEY=your_api_key_here
   SPOTIFY_CLIENT_ID=your_spotify_client_id
   SPOTIFY_CLIENT_SECRET=your_spotify_client_secret
   SPOTIFY_REFRESH_TOKEN=your_spotify_refresh_token
   ```
4. Generate an API key with the provided script:
   ```
   python generate_api_key.py
   ```
5. Build and run the application:
   ```
   cargo build --release
   cargo run --release
   ```

## Configuration

The application uses a flexible configuration system that supports both TOML files and environment variables:

### TOML Configuration

Create a `config.local.toml` file to customize settings (this file is ignored by git):

```toml
[server]
host = "0.0.0.0"
port = "4653"
log_level = "info"

[spotify]
excluded_genres = ["comedy", "podcast"]
tracks_limit = 6

[letterboxd]
default_feed_url = "https://letterboxd.com/username/rss"
movies_limit = 5

[cache]
spotify_duration_secs = 900      # 15 minutes
letterboxd_duration_secs = 3600  # 1 hour

[cors]
allowed_origin = "https://yourdomain.com"
```

### Environment Variables (Override TOML)

All TOML settings can be overridden with environment variables:

- `HOST` - Server host
- `PORT` - Server port
- `RUST_LOG` - Log level
- `API_KEY` - API authentication key (required)
- `SPOTIFY_CLIENT_ID` - Spotify API client ID (required)
- `SPOTIFY_CLIENT_SECRET` - Spotify API client secret (required)
- `SPOTIFY_REFRESH_TOKEN` - Spotify API refresh token (required)
- `SPOTIFY_EXCLUDED_GENRES` - Comma-separated list of genres to exclude
- `ALLOWED_ORIGIN` - CORS allowed origin

### Migration from Environment-Only Setup

The application is fully backwards compatible. Existing `.env` files and environment variables will continue to work exactly as before. The TOML configuration system provides additional flexibility for settings that were previously hardcoded.

To migrate to the new configuration system:
1. Keep your existing `.env` file for sensitive credentials
2. Create a `config.local.toml` file for application settings
3. Move non-sensitive configuration from environment variables to TOML as desired

## API Endpoints

All endpoints except `/aggregated` require authentication with the API key in the Authorization header:
```
Authorization: Bearer your_api_key_here
```

### URL Webhook Endpoint

#### POST /url-webhook
Records a URL provided in the request body.

**Request:**
- Method: POST
- Body: Raw text containing the URL

**Response:**
- 200 OK: Successfully recorded the URL
- 401 Unauthorized: Invalid or missing API key

#### GET /url-webhook
Returns the 5 most recently recorded URLs.

**Request:**
- Method: GET

**Response:**
- 200 OK: JSON containing the URLs array
- 401 Unauthorized: Invalid or missing API key

Response Format:
```json
{
  "urls": ["url1", "url2", "url3", "url4", "url5"]
}
```

### Letterboxd Endpoint

#### GET /letterboxd
Returns the 5 most recently watched movies from a Letterboxd RSS feed.

**Request:**
- Method: GET
- Query Parameters:
  - `feed_url` (optional): URL of the Letterboxd RSS feed (default: configurable in config.toml)
  - `no_cache` (optional): Set to "true" to bypass cache

**Response:**
- 200 OK: JSON containing the movies array
- 401 Unauthorized: Invalid or missing API key
- 500 Internal Server Error: Unable to fetch or parse the feed

Response Format:
```json
{
  "movies": [
    {
      "title": "Movie Title with Rating",
      "link": "https://letterboxd.com/user/film/movie-slug/",
      "description": "Review text",
      "pub_date": "Wed, 01 Jan 2023 12:00:00 +0000",
      "film_title": "Movie Title",
      "rating": "3.5",
      "rewatch": "true"
    },
    ...
  ]
}
```

### Spotify Endpoint

#### GET /spotify
Returns the most recently played tracks from Spotify.

**Request:**
- Method: GET
- Query Parameters:
  - `limit` (optional): Number of tracks to return (default: 5)
  - `no_cache` (optional): Set to "true" to bypass cache

**Response:**
- 200 OK: JSON containing the tracks array
- 401 Unauthorized: Invalid or missing API key
- 500 Internal Server Error: Unable to fetch tracks from Spotify

Response Format:
```json
{
  "tracks": [
    {
      "track_name": "Track Name",
      "artist": "Artist Name",
      "album_name": "Album Name",
      "played_at": "2023-01-01T12:00:00Z",
      "spotify_url": "https://open.spotify.com/track/id",
      "album_image_url": "https://i.scdn.co/image/id",
      "genres": ["indie rock", "alternative"]
    },
    ...
  ]
}
```

The Spotify endpoint now includes genre information for each track and automatically filters out tracks with excluded genres. By default, "comedy" is excluded. You can customize excluded genres using the `SPOTIFY_EXCLUDED_GENRES` environment variable.

### Aggregated Endpoint

#### GET /aggregated
Returns data from all three sources (URLs, Letterboxd movies, and Spotify tracks) in a single response. This endpoint does not require authentication.

**Request:**
- Method: GET
- No authentication required
- Query Parameters:
  - `feed_url` (optional): URL of the Letterboxd RSS feed (default: configurable in config.toml)
  - `limit` (optional): Number of Spotify tracks to return (default: configurable in config.toml)
  - `no_cache` (optional): Set to "true" to bypass cache

**Response:**
- 200 OK: JSON containing all aggregated data

Response Format:
```json
{
  "urls": ["url1", "url2", "url3", "url4", "url5"],
  "movies": [
    {
      "title": "Movie Title with Rating",
      "link": "https://letterboxd.com/user/film/movie-slug/",
      "description": "Review text",
      "pub_date": "Wed, 01 Jan 2023 12:00:00 +0000",
      "film_title": "Movie Title",
      "rating": "3.5",
      "rewatch": "true"
    },
    ...
  ],
  "tracks": [
    {
      "track_name": "Track Name",
      "artist": "Artist Name",
      "album_name": "Album Name",
      "played_at": "2023-01-01T12:00:00Z",
      "spotify_url": "https://open.spotify.com/track/id",
      "album_image_url": "https://i.scdn.co/image/id",
      "genres": ["indie rock", "alternative"]
    },
    ...
  ]
}
```

## Caching

Both the Letterboxd and Spotify endpoints implement caching to improve performance and reduce external API calls:

- Letterboxd data cache duration: configurable (default: 1 hour)
- Spotify data cache duration: configurable (default: 15 minutes)

Cache durations can be customized in `config.toml` or `config.local.toml`:

```toml
[cache]
spotify_duration_secs = 900      # 15 minutes
letterboxd_duration_secs = 3600  # 1 hour
```

Use the `no_cache=true` query parameter to bypass the cache when needed.

## Error Handling

All endpoints return appropriate HTTP status codes and error messages in JSON format when issues occur. 