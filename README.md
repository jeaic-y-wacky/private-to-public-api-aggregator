# API Aggregator for Static Sites

A Rust-based API aggregator that provides endpoints for Spotify recently played tracks, Letterboxd watched movies, and URL webhook functionality. This service can be used to add dynamic content to static websites.

## Setup

1. Clone the repository
2. Create a `.env` file with the following variables:
   ```
   API_KEY=your_api_key_here
   SPOTIFY_CLIENT_ID=your_spotify_client_id
   SPOTIFY_CLIENT_SECRET=your_spotify_client_secret
   SPOTIFY_REFRESH_TOKEN=your_spotify_refresh_token
   ```
3. Generate an API key with the provided script:
   ```
   python generate_api_key.py
   ```
4. Build and run the application:
   ```
   cargo build --release
   cargo run --release
   ```

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
  - `feed_url` (optional): URL of the Letterboxd RSS feed (default: https://letterboxd.com/atropos_Dad/rss)
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

Genre lookups are best-effort. If Spotify will not serve artist data to this app,
tracks are returned **unfiltered** rather than the endpoint returning nothing — an
outage in the genre lookup must never empty the whole section.

#### GET /spotify/diagnose

Authenticated health check that walks the whole Spotify pipeline live, bypassing
every cache, and reports where it breaks. Use this first whenever the tracks list
comes back empty.

**Request:**
- Method: GET
- Requires the `Authorization: Bearer <API_KEY>` header

**Response:** 200 OK with a report covering each stage:

```json
{
  "config": {
    "client_id_set": true,
    "client_secret_set": true,
    "refresh_token_set": true,
    "excluded_genres": ["comedy"]
  },
  "token_refresh": { "ok": true, "expires_in": 3600 },
  "recently_played": {
    "ok": true,
    "status": 200,
    "items_returned": 25,
    "items_parsed": 25,
    "items_skipped": 0,
    "first_parse_error": null,
    "tracks_built": 25
  },
  "artist_lookup": { "ok": true, "artist_id": "…", "status": 200, "genres": ["…"] },
  "conclusion": "Every stage succeeded. …"
}
```

Every stage runs even when an earlier one fails, and failures carry the HTTP
status plus a truncated response body, so one request distinguishes a credential
problem from a removed endpoint from an over-eager genre filter.

### Aggregated Endpoint

#### GET /aggregated
Returns data from all three sources (URLs, Letterboxd movies, and Spotify tracks) in a single response. This endpoint does not require authentication.

**Request:**
- Method: GET
- No authentication required
- Query Parameters:
  - `feed_url` (optional): URL of the Letterboxd RSS feed (default: https://letterboxd.com/atropos_Dad/rss)
  - `limit` (optional): Number of Spotify tracks to return (default: 5)
  - `no_cache` (optional): Set to "true" to bypass cache

**Response:**
- 200 OK: JSON containing all aggregated data

If a source fails, its array comes back empty **and** an `errors` object is added
naming the source and the reason. The field is omitted entirely when everything
succeeded, so existing consumers are unaffected:

```json
{
  "urls": [],
  "movies": [],
  "tracks": [],
  "errors": { "spotify": "Failed to get recently played tracks: HTTP 403 - …" }
}
```

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

- Letterboxd data is cached for 1 hour
- Spotify data is cached for 15 minutes

Use the `no_cache=true` query parameter to bypass the cache when needed. This
works on `/aggregated` as well as on the individual endpoints.

Artist genres are cached separately for the life of the process, since they
effectively never change and each one now costs its own request.

All API responses are sent with `Cache-Control: no-store` so a browser or CDN
cannot serve a stale copy of a live feed.

## Error Handling

All endpoints return appropriate HTTP status codes and error messages in JSON format when issues occur.

## Notes on the Spotify Web API (2026)

Spotify tightened Web API access in two waves, and both affect this service:

- **27 November 2024** — audio features/analysis, recommendations, related
  artists, featured playlists and genre seeds were deprecated.
- **February 2026** — Development Mode apps were restricted to a smaller set of
  endpoints. New Client IDs got the new rules on **11 February 2026**; existing
  apps were migrated on **9 March 2026**. The batch "Get Several Artists"
  endpoint (`GET /v1/artists?ids=`) was removed, along with the other batch
  lookups, browse endpoints and artist top tracks. Fields including
  `popularity`, `followers` and `available_markets` were dropped from responses.
  Development Mode now also requires the **app owner to hold an active Spotify
  Premium subscription**, allows a maximum of **five** allow-listed users, and
  extended quota mode is reserved for registered businesses with 250k+ MAU.

Two consequences shape the code in `src/spotify.rs`:

1. **Genres are best-effort.** They come from one `GET /v1/artists/{id}` request
   per artist, with bounded concurrency, a process-lifetime cache, and a cooldown
   if Spotify answers 401/403/404. A track with no known genres is never filtered
   out.
2. **Deserialization is lenient.** Development Mode strips fields from payloads,
   so every field except the track name is optional and play-history items are
   parsed one at a time. A single unexpected entry is logged and skipped rather
   than failing the whole response — which is what turns a small upstream change
   into an empty tracks array.

### Security note

Never commit `SPOTIFY_CLIENT_SECRET` or `SPOTIFY_REFRESH_TOKEN`. They belong in
`.env` only. A client secret that has been pushed to a public repository should
be rotated in the Spotify developer dashboard and the refresh token reissued,
since a leaked secret can be revoked without warning.
